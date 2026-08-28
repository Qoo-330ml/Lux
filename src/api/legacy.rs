use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Component, Path as FsPath, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, RawQuery, State},
    http::{
        HeaderMap, HeaderValue, Method, Request, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE, COOKIE, SET_COOKIE},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
};
use serde::{Deserialize, Serialize, de::Deserializer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::fs;
use tower_http::{
    ServiceBuilderExt,
    request_id::MakeRequestUuid,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use uuid::Uuid;

use crate::api::lux;
use crate::{
    COMMIT, VERSION,
    application::danmaku::{DanmakuService, DanmakuServiceError},
    application::database_setup::{DatabaseSetupError, DatabaseSetupService},
    application::deletion::{MediaDeleteError, MediaDeleteService},
    application::downloads::{DownloadArtifact, DownloadError, DownloadService},
    application::home::{HomeError, HomeService},
    application::playback::decision::{PlaybackCapabilities, PlaybackSourceKind},
    application::playback::session::{
        CreateWebPlaybackSession, EMBY_DIRECT_STREAM_TTL_SECONDS, WebPlaybackEvent,
        WebPlaybackPlan, WebPlaybackSessionError, WebPlaybackSessionService,
    },
    application::playback::{ByteRange, RangeError, parse_single_range},
    application::probe::{FfprobeRunner, MediaProbeService},
    application::setup::{SetupError, SetupService},
    application::{
        access::{AccessPrincipal, MediaAccessService},
        admin_events::{AdminEventHub, AdminEventScope, UserEventHub},
        candidates::{
            MetadataCandidateError, MetadataCandidatePage, MetadataCandidateService,
            MetadataSelectionError, MetadataSelectionMode, MetadataSelectionService,
        },
        catalog::{
            CatalogError, CatalogFilter, CatalogItem, CatalogPage, CatalogService, CatalogSort,
            CatalogSource, normalize_search_like_query, normalize_search_query,
        },
        chapter_detector::{
            ChapterDetectionError, ChapterDetectionOptions, ChapterDetectionService,
            DEFAULT_CHAPTER_DETECTOR_PLUGIN_ID,
        },
        collections::{CollectionError, CollectionService},
        directory_browser::{DirectoryBrowserError, list_directories},
        emby_migration_service::{
            CreateMigrationRequest, EmbyMigrationService, EmbyMigrationServiceError,
        },
        images::{
            ImageCandidateError, ImageCandidateService, ImageError, ImageService, ImageWriteError,
            ImageWriteService, normalize_image_type, read_image_dimensions,
        },
        ip_location::{IpLocation, IpLocationService},
        libraries::{LibraryService, LibraryServiceError, LibrarySettingsPatch, LibraryView},
        library_covers::{LibraryCoverError, LibraryCoverService, MAX_LIBRARY_COVER_BYTES},
        metadata::MetadataField,
        network_diagnostics::{NetworkDiagnostics, NetworkProbeResult, test_network},
        nfo::{
            LocalNfoDetails, LocalNfoMetadataStore, MetadataWriteRequest, MetadataWriteService,
            NfoWriteError,
        },
        people::{PeopleError, PeopleService, PersonMetadataUpdate},
        plugins::{PluginPage, PluginService, PluginServiceError},
        reidentify::{MetadataReidentifyError, MetadataReidentifyService},
        scanner::{BACKGROUND_SCAN_BATCH_SIZE, ScanJob, ScanJobError, ScanJobService},
        schedule::validate_cron,
        scheduled_tasks::{ScheduledTaskError, ScheduledTaskRun, ScheduledTaskService},
        scraper::{ScraperProvider, ScraperResolver},
        settings::{read_network_proxy_url_async, write_network_proxy_url},
        strm_playback::{StrmPlaybackError, StrmPlaybackResolver},
        strm_probe::{StrmProbeError, StrmProbeService},
        strm_target::{
            StrmLocalPathError, StrmTargetKind, canonical_local_strm_target, classify_strm_target,
        },
        thumbnails::ThumbnailService,
        user_avatars::{MAX_USER_AVATAR_BYTES, UserAvatarError, UserAvatarService},
        watch::LibraryWatchService,
        webhooks::{BUILTIN_WEBHOOK_PROVIDER_ID, WebhookError, WebhookEventType, WebhookService},
    },
    auth::users::{UserRecord, UserStore, UserStoreError, UserUpdate},
    auth::{
        admin_api_key::AdminApiKeyService,
        emby::{EmbyAuthService, EmbyDeviceInfo},
        sessions::WebAuthService,
    },
    config::{Config, DatabaseBackend, DatabaseConfiguration, PostgresConnection},
    library::{LibraryKind, LibraryRecord, LibraryRootRecord, LibraryScraper, LibraryScraperRole},
    network::{
        RemoteAccessPolicy, normalize_proxy_url, proxy_url_from_env, proxy_url_has_credentials,
        redact_proxy_url,
    },
    observability::{
        logs::{LogDateRange, LogExport, LogExportError, export_logs},
        resources::ResourceMetrics,
    },
    security::LoginRateLimiter,
    storage::{
        DashboardStats, Database, ExternalSubtitleUpdate, NewPlaybackEvent, PersonListOptions,
        PersonSort, StorageError, StoredPlaybackSession, WebPlaybackEventClaim,
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
    process::Command,
};

#[path = "admin.rs"]
mod admin;
use admin::*;
#[path = "emby.rs"]
mod emby;
#[path = "emby_handlers.rs"]
mod emby_handlers;
use emby_handlers::*;
#[path = "emby_catalog.rs"]
mod emby_catalog;
#[allow(unused_imports)]
use emby_catalog::*;
#[path = "playback.rs"]
mod playback;
use playback::*;
#[path = "media.rs"]
mod media;
use media::*;
#[path = "lux_api.rs"]
mod lux_api;
#[path = "routes.rs"]
mod routes;
#[path = "users.rs"]
mod users;

#[derive(Clone, Default)]
pub struct AppState {
    database: Option<Database>,
    config_dir: Option<PathBuf>,
    database_setup: Option<DatabaseSetupService>,
    database_selection_required: bool,
    server_id: String,
    filmly_image_compat_mode: FilmlyImageCompatMode,
    setup: Option<SetupService>,
    auth: Option<WebAuthService>,
    emby_auth: Option<EmbyAuthService>,
    admin_api_key: Option<AdminApiKeyService>,
    libraries: Option<LibraryService>,
    catalog: Option<CatalogService>,
    home: Option<HomeService>,
    images: Option<ImageService>,
    image_writes: Option<ImageWriteService>,
    image_candidates: Option<ImageCandidateService>,
    library_covers: Option<LibraryCoverService>,
    access: Option<MediaAccessService>,
    metadata_candidates: Option<MetadataCandidateService>,
    metadata_selection: Option<MetadataSelectionService>,
    metadata_writes: Option<MetadataWriteService>,
    downloads: Option<DownloadService>,
    strm_playback: Option<StrmPlaybackResolver>,
    metadata_reidentify: Option<MetadataReidentifyService>,
    deletion: Option<MediaDeleteService>,
    probe: Option<MediaProbeService>,
    thumbnails: Option<ThumbnailService>,
    scan_jobs: Option<ScanJobService>,
    strm_probe: Option<StrmProbeService>,
    chapter_detection: Option<ChapterDetectionService>,
    scheduled_tasks: Option<ScheduledTaskService>,
    webhooks: Option<WebhookService>,
    danmaku: Option<DanmakuService>,
    plugins: Option<PluginService>,
    emby_migration: Option<Arc<EmbyMigrationService>>,
    scraper_resolver: Option<ScraperResolver>,
    scraper: Option<ScraperProvider>,
    collections: Option<CollectionService>,
    people: Option<PeopleService>,
    local_nfo: Option<LocalNfoMetadataStore>,
    user_avatars: Option<UserAvatarService>,
    ip_location: Option<IpLocationService>,
    admin_events: AdminEventHub,
    user_events: UserEventHub,
    web_playback: Option<WebPlaybackSessionService>,
    resources: ResourceMetrics,
    remote_access: RemoteAccessPolicy,
    login_rate_limiter: LoginRateLimiter,
}

impl AppState {
    pub fn ready(
        config: Config,
        database: Database,
        setup: SetupService,
        auth: WebAuthService,
        emby_auth: EmbyAuthService,
    ) -> Self {
        Self::ready_with_proxy(config, database, setup, auth, emby_auth, None)
    }

    pub fn ready_with_proxy(
        config: Config,
        database: Database,
        setup: SetupService,
        auth: WebAuthService,
        emby_auth: EmbyAuthService,
        network_proxy_url: Option<String>,
    ) -> Self {
        let server_id = database.server_id().to_owned();
        let filmly_image_compat_mode = filmly_image_compat_mode_from_env_value(
            std::env::var("LUX_FILMLY_IMAGE_MODE").ok().as_deref(),
        );
        let config_dir = config.config_dir.clone();
        let user_avatars = Some(UserAvatarService::new(config_dir.clone()));
        let resources = ResourceMetrics::new();
        let database_setup = Some(DatabaseSetupService::new(
            config.clone(),
            database.backend(),
        ));
        let admin_events = AdminEventHub::new();
        let user_events = UserEventHub::new();
        let web_playback = Some(WebPlaybackSessionService::new(
            database.clone(),
            config_dir.clone(),
        ));
        let access = MediaAccessService::new(database.clone());
        let libraries = LibraryService::new(database.clone());
        let catalog = CatalogService::new(database.clone(), access.clone());
        let home = HomeService::new(catalog.clone(), libraries.clone(), access.clone());
        let image_writes = ImageWriteService::new_with_proxy_and_config_dir(
            database.clone(),
            config.config_dir.clone(),
            network_proxy_url.clone(),
        )
        .ok()
        .map(|service| service.with_resource_metrics(resources.clone()));
        let library_covers = Some(
            LibraryCoverService::new(database.clone(), config.config_dir.join("library-covers"))
                .with_metadata_directory(config.config_dir.join("metadata")),
        );
        let metadata_selection = image_writes.clone().map(|images| {
            MetadataSelectionService::with_config_dir(database.clone(), images, config_dir.clone())
                .with_home(home.clone())
        });
        let plugins = PluginService::new_with_proxy(
            database.clone(),
            config_dir.clone(),
            network_proxy_url.clone(),
        );
        let emby_migration = Some(Arc::new(EmbyMigrationService::new(
            database.clone(),
            plugins.clone(),
            config_dir.clone(),
        )));
        let scraper = ScraperProvider::unconfigured();
        let scraper_resolver = ScraperResolver::new(database.clone(), plugins.clone());
        let collections = Some(
            CollectionService::with_resolver(
                database.clone(),
                scraper.clone(),
                scraper_resolver.clone(),
            )
            .with_config_dir(config.config_dir.clone()),
        );
        let metadata_reidentify = Some(
            MetadataReidentifyService::with_resolver_and_selection(
                database.clone(),
                scraper.clone(),
                scraper_resolver.clone(),
                metadata_selection.clone(),
            )
            .with_admin_events(admin_events.clone())
            .with_resource_metrics(resources.clone()),
        );
        let image_candidates = Some(ImageCandidateService::with_resolver(
            database.clone(),
            scraper.clone(),
            scraper_resolver.clone(),
        ));
        let strm_probe = StrmProbeService::new(database.clone(), plugins.clone())
            .with_resource_metrics(resources.clone());
        let chapter_detection = ChapterDetectionService::new(database.clone(), plugins.clone());
        let webhooks = match WebhookService::new(database.clone(), config_dir.clone())
            .map(|service| service.with_plugins(plugins.clone()))
        {
            Ok(service) => Some(service),
            Err(error) => {
                tracing::error!(%error, "failed to initialize webhook notifications");
                None
            }
        };
        let metadata_reidentify = metadata_reidentify.map(|service| match webhooks.clone() {
            Some(webhooks) => service.with_webhooks(webhooks),
            None => service,
        });
        let people = PeopleService::new_with_proxy(config_dir.clone(), network_proxy_url.clone())
            .with_database(database.clone());
        let local_nfo = LocalNfoMetadataStore::new(database.clone());
        let probe = Some(
            MediaProbeService::new(database.clone(), FfprobeRunner::default())
                .with_resource_metrics(resources.clone()),
        );
        let thumbnails = Some(ThumbnailService::new(database.clone()));
        let scan_jobs = {
            let service = ScanJobService::new(database.clone())
                .with_admin_events(admin_events.clone())
                .with_user_events(user_events.clone())
                .with_resource_metrics(resources.clone())
                .with_home(home.clone());
            let service = match library_covers.clone() {
                Some(covers) => service.with_library_covers(covers),
                None => service,
            };
            let service = service
                .with_strm_probe(strm_probe.clone())
                .with_people(people.clone())
                .with_nfo_store(local_nfo.clone());
            match webhooks.clone() {
                Some(webhooks) => service.with_webhooks(webhooks),
                None => service,
            }
        };
        let scheduled_tasks = ScheduledTaskService::new(
            database.clone(),
            plugins.clone(),
            strm_probe.clone(),
            Some(chapter_detection.clone()),
        )
        .with_library_services(
            scan_jobs.clone(),
            metadata_reidentify.clone(),
            probe.clone(),
            thumbnails.clone(),
        )
        .with_library_covers(library_covers.clone());
        Self {
            database: Some(database.clone()),
            config_dir: Some(config_dir.clone()),
            database_setup,
            database_selection_required: false,
            server_id,
            filmly_image_compat_mode,
            setup: Some(setup),
            auth: Some(auth),
            emby_auth: Some(emby_auth),
            admin_api_key: Some(AdminApiKeyService::new(
                config_dir.clone(),
                database.clone(),
            )),
            libraries: Some(libraries),
            catalog: Some(catalog),
            home: Some(home),
            images: Some(ImageService::new(
                database.clone(),
                access.clone(),
                config.config_dir.clone(),
            )),
            image_writes,
            image_candidates,
            library_covers: library_covers.clone(),
            access: Some(access),
            metadata_candidates: Some(MetadataCandidateService::new(database.clone())),
            metadata_selection,
            metadata_writes: Some(MetadataWriteService::new_with_config_dir(
                database.clone(),
                config_dir.clone(),
            )),
            downloads: DownloadService::new_with_proxy(database.clone(), network_proxy_url.clone())
                .ok(),
            // STRM targets can be internal NAS services. This resolver is
            // deliberately outside the global proxy configuration.
            strm_playback: StrmPlaybackResolver::new().ok(),
            metadata_reidentify,
            deletion: Some(match webhooks.clone() {
                Some(webhooks) => MediaDeleteService::new(database.clone()).with_webhooks(webhooks),
                None => MediaDeleteService::new(database.clone()),
            }),
            probe,
            thumbnails,
            scan_jobs: Some(scan_jobs),
            strm_probe: Some(strm_probe),
            chapter_detection: Some(chapter_detection),
            scheduled_tasks: Some(scheduled_tasks),
            webhooks,
            danmaku: Some(
                DanmakuService::new(database.clone())
                    .with_plugins(plugins.clone())
                    .with_resource_metrics(resources.clone()),
            ),
            plugins: Some(plugins.clone()),
            emby_migration,
            scraper_resolver: Some(scraper_resolver),
            scraper: Some(scraper),
            collections,
            people: Some(people),
            local_nfo: Some(local_nfo),
            user_avatars,
            ip_location: Some(IpLocationService::new(plugins.clone())),
            admin_events,
            user_events,
            web_playback,
            resources,
            remote_access: RemoteAccessPolicy,
            login_rate_limiter: LoginRateLimiter::default(),
        }
    }

    #[doc(hidden)]
    pub fn with_strm_playback_resolver(mut self, resolver: StrmPlaybackResolver) -> Self {
        self.strm_playback = Some(resolver);
        self
    }

    pub fn with_scraper<T>(mut self, scraper: T) -> Self
    where
        T: Into<ScraperProvider>,
    {
        let Some(database) = self.database.clone() else {
            return self;
        };
        let scraper = scraper.into();
        self.scraper = Some(scraper.clone());
        if let Some(resolver) = self.scraper_resolver.clone() {
            let mut collections = CollectionService::with_resolver(
                database.clone(),
                scraper.clone(),
                resolver.clone(),
            );
            if let Some(config_dir) = self.config_dir.clone() {
                collections = collections.with_config_dir(config_dir);
            }
            self.collections = Some(collections);
            self.metadata_reidentify = Some(
                MetadataReidentifyService::with_resolver_and_selection(
                    database.clone(),
                    scraper.clone(),
                    resolver.clone(),
                    self.metadata_selection.clone(),
                )
                .with_admin_events(self.admin_events.clone())
                .with_resource_metrics(self.resources.clone()),
            );
            self.image_candidates = Some(ImageCandidateService::with_resolver(
                database, scraper, resolver,
            ));
        } else {
            let mut collections = CollectionService::new(database.clone(), scraper.clone());
            if let Some(config_dir) = self.config_dir.clone() {
                collections = collections.with_config_dir(config_dir);
            }
            self.collections = Some(collections);
            self.metadata_reidentify = Some(
                MetadataReidentifyService::with_selection(
                    database.clone(),
                    scraper.clone(),
                    self.metadata_selection.clone(),
                )
                .with_admin_events(self.admin_events.clone())
                .with_resource_metrics(self.resources.clone()),
            );
            self.image_candidates = Some(ImageCandidateService::new(database, scraper));
        }
        self
    }

    pub async fn resume_metadata_reidentify_jobs(&self) {
        let Some(service) = self.metadata_reidentify.clone() else {
            return;
        };
        let Ok(job_ids) = service.active_job_ids().await else {
            tracing::error!("failed to discover active metadata reidentify jobs during startup");
            return;
        };
        for job_id in job_ids {
            let worker = service.clone();
            tokio::spawn(async move {
                worker.run(&job_id).await;
            });
        }
    }

    pub async fn rebuild_people_index(&self) {
        let Some(service) = self.people.clone() else {
            return;
        };
        service.schedule_person_index_rebuild();
    }

    pub async fn resume_scan_jobs(&self) {
        let Some(service) = self.scan_jobs.clone() else {
            return;
        };
        let Ok(job_ids) = service.active_job_ids().await else {
            tracing::error!("failed to discover active scan jobs during startup");
            return;
        };
        for job_id in job_ids {
            let worker = service.clone();
            let worker_probe = self.probe.clone();
            let worker_metadata = self.metadata_reidentify.clone();
            let worker_thumbnails = self.thumbnails.clone();
            tokio::spawn(async move {
                if let Err(error) = worker
                    .run_to_completion_with_metadata_and_thumbnails(
                        &job_id,
                        BACKGROUND_SCAN_BATCH_SIZE,
                        worker_probe,
                        worker_metadata,
                        worker_thumbnails,
                    )
                    .await
                {
                    tracing::error!(job_id = %job_id, %error, "resumed scan job stopped");
                }
            });
        }
    }

    pub async fn start_realtime_watchers(&self) {
        let Some(database) = self.database.clone() else {
            return;
        };
        let Some(scan_jobs) = self.scan_jobs.clone() else {
            return;
        };
        let watch_service = LibraryWatchService::with_scan_jobs_and_metadata(
            database,
            scan_jobs,
            self.metadata_reidentify.clone(),
        );
        let watch_service = match self.libraries.as_ref() {
            Some(libraries) => {
                watch_service.with_library_change_notifications(libraries.change_notifier())
            }
            None => watch_service,
        };
        watch_service.spawn();
    }

    pub async fn resume_strm_probe_jobs(&self) {
        let Some(service) = self.strm_probe.clone() else {
            return;
        };
        let Ok(job_ids) = service.active_job_ids().await else {
            tracing::error!("failed to discover active STRM probe jobs during startup");
            return;
        };
        for job_id in job_ids {
            let worker = service.clone();
            tokio::spawn(async move {
                if let Err(error) = worker.run(&job_id).await {
                    tracing::error!(job_id = %job_id, %error, "resumed STRM probe job stopped");
                }
            });
        }
    }

    pub async fn resume_chapter_detection_jobs(&self) {
        let Some(service) = self.chapter_detection.clone() else {
            return;
        };
        let Ok(job_ids) = service.active_job_ids().await else {
            tracing::error!("failed to discover active chapter detection jobs during startup");
            return;
        };
        for job_id in job_ids {
            let worker = service.clone();
            tokio::spawn(async move {
                if let Err(error) = worker.run(&job_id).await {
                    tracing::error!(job_id = %job_id, %error, "resumed chapter detection job stopped");
                }
            });
        }
    }

    pub async fn resume_library_cover_jobs(&self) {
        let Some(service) = self.library_covers.clone() else {
            return;
        };
        let Ok(job_ids) = service.active_job_ids().await else {
            tracing::error!("failed to discover active library cover jobs during startup");
            return;
        };
        for job_id in job_ids {
            let worker = service.clone();
            tokio::spawn(async move {
                if let Err(error) = worker.run_job(&job_id).await {
                    tracing::error!(job_id = %job_id, %error, "resumed library cover job stopped");
                }
            });
        }
        let reconciliation = service.clone();
        tokio::spawn(async move {
            match reconciliation.reconcile_auto_library_covers().await {
                Ok(generated) if generated > 0 => {
                    tracing::info!(generated, "reconciled automatic library covers at startup");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::error!(%error, "automatic library cover startup reconciliation failed");
                }
            }
        });
    }

    pub async fn start_scheduled_tasks(&self) {
        if let Some(plugins) = self.plugins.as_ref()
            && let Err(error) = plugins.sync_media_info_scheduled_task().await
        {
            tracing::error!(%error, "failed to synchronize STRM scheduled task");
        }
        if let Some(plugins) = self.plugins.as_ref()
            && let Err(error) = plugins.sync_chapter_detection_scheduled_tasks().await
        {
            tracing::error!(%error, "failed to synchronize chapter detection scheduled tasks");
        }
        if let Some(scheduled_tasks) = self.scheduled_tasks.as_ref() {
            scheduled_tasks.spawn();
        }
    }

    pub fn start_webhook_worker(&self) {
        if let Some(webhooks) = self.webhooks.as_ref() {
            webhooks.spawn_worker();
        }
    }

    pub async fn resume_danmaku_match_jobs(&self) {
        let Some(service) = self.danmaku.clone() else {
            return;
        };
        let Ok(job_ids) = service.active_job_ids().await else {
            tracing::error!("failed to discover active danmaku match jobs during startup");
            return;
        };
        for job_id in job_ids {
            let worker = service.clone();
            tokio::spawn(async move {
                if let Err(error) = worker.run(&job_id).await {
                    tracing::error!(job_id = %job_id, %error, "resumed danmaku match job stopped");
                }
            });
        }
    }

    pub async fn resume_emby_migration_jobs(&self) {
        let Some(service) = self.emby_migration.clone() else {
            return;
        };
        let Ok(jobs) = service.list_jobs(0, 100).await else {
            tracing::error!("failed to discover active Emby migration jobs during startup");
            return;
        };
        for job in jobs {
            if !matches!(job.status.as_str(), "PENDING" | "RUNNING") {
                continue;
            }
            service.clone().spawn(job.id);
        }
    }

    pub fn require_database_selection(mut self) -> Self {
        self.database_selection_required = true;
        self
    }
}

pub fn app() -> Router {
    routes::app_with_state(AppState::default())
}

pub fn app_with_state(state: AppState) -> Router {
    routes::app_with_state(state)
}

async fn require_web_user(headers: &HeaderMap, state: &AppState) -> Result<UserRecord, Response> {
    users::require_web_user(headers, state).await
}

async fn require_web_csrf(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    users::require_web_csrf(headers, state).await
}

const MAX_CONCURRENT_CATALOG_REQUESTS: usize = 16;
const MAX_IN_FLIGHT_CATALOG_REQUESTS: usize = 64;

fn is_catalog_aggregation_path(path: &str) -> bool {
    let route = path
        .strip_prefix("/emby/")
        .or_else(|| path.strip_prefix('/'))
        .unwrap_or(path);
    let segments = route.split('/').collect::<Vec<_>>();

    matches!(
        segments.as_slice(),
        ["api", "v1", "favorites" | "search" | "home"]
            | ["api", "v1", "libraries", _, "items"]
            | ["api", "v1", "items", _, "children"]
            | ["api", "v1", "collections", _]
            | ["Users", _, "Items"]
            | ["Users", _, "Items", "Root" | "Resume" | "Latest" | "NextUp"]
            | ["Shows", "NextUp"]
            | ["Shows", _, "Seasons" | "Episodes"]
            | ["Items"]
            | ["Items", "Counts"]
            | ["Items", "Root"]
            | ["Search", "Hints"]
            | ["Items", _, "Children"]
    )
}

async fn normalize_empty_api_service_unavailable(request: Request<Body>, next: Next) -> Response {
    let is_lux_api = request.uri().path().starts_with("/api/v1/");
    let request_headers = request.headers().clone();
    let response = next.run(request).await;
    if !is_lux_api || response.status() != StatusCode::SERVICE_UNAVAILABLE {
        return response;
    }

    let (parts, body) = response.into_parts();
    match to_bytes(body, 64 * 1024).await {
        Ok(body) if !body.is_empty() => {
            return Response::from_parts(parts, Body::from(body));
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, "failed to inspect service unavailable response body");
        }
    }

    let mut error_headers = request_headers;
    if let Some(request_id) = parts.headers.get("x-request-id") {
        error_headers.insert("x-request-id", request_id.clone());
    }
    let mut normalized = api_error(
        &error_headers,
        StatusCode::SERVICE_UNAVAILABLE,
        lux::ApiErrorCode::DatabaseUnavailable,
        "数据库暂时不可用",
    )
    .into_response();
    normalized.headers_mut().extend(parts.headers);
    normalized
}

async fn attach_peer_address(mut request: Request<Body>, next: Next) -> Response {
    request.headers_mut().remove("x-lux-peer-ip");
    if let Some(ConnectInfo(address)) = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
    {
        let value = address.ip().to_string();
        if let Ok(value) = HeaderValue::from_str(&value) {
            request.headers_mut().insert("x-lux-peer-ip", value);
        }
    }
    next.run(request).await
}

fn safe_trace_path(uri: &axum::http::Uri) -> &str {
    uri.path()
}

fn is_emby_playback_callback_path(path: &str) -> bool {
    matches!(
        path,
        "/Sessions/Playing"
            | "/Sessions/Playing/Progress"
            | "/Sessions/Playing/Stopped"
            | "/emby/Sessions/Playing"
            | "/emby/Sessions/Playing/Progress"
            | "/emby/Sessions/Playing/Stopped"
    )
}

fn emby_playback_info_item_id(path: &str) -> Option<&str> {
    let path = path.strip_suffix("/PlaybackInfo")?;
    let item_id = path
        .strip_prefix("/Items/")
        .or_else(|| path.strip_prefix("/emby/Items/"))?;
    (!item_id.is_empty() && !item_id.contains('/')).then_some(item_id)
}

fn emby_media_stream_item_id(path: &str) -> Option<&str> {
    let path = path
        .strip_prefix("/Videos/")
        .or_else(|| path.strip_prefix("/emby/Videos/"))
        .or_else(|| path.strip_prefix("/videos/"))
        .or_else(|| path.strip_prefix("/emby/videos/"))?;
    let mut segments = path.split('/');
    let item_id = segments.next()?;
    let second_segment = segments.next()?;
    let third_segment = segments.next();
    if segments.next().is_some() || item_id.is_empty() {
        return None;
    }
    match third_segment {
        None if is_emby_media_stream_segment(second_segment) => Some(item_id),
        Some(stream) if !second_segment.is_empty() && is_emby_media_stream_segment(stream) => {
            Some(item_id)
        }
        _ => None,
    }
}

fn is_emby_media_stream_segment(segment: &str) -> bool {
    segment == "stream"
        || segment
            .strip_prefix("stream.")
            .is_some_and(|container| !container.is_empty())
}

async fn trace_emby_playback_callback(request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    if !is_emby_playback_callback_path(&path) {
        return next.run(request).await;
    }
    let method = request.method().clone();
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown")
        .to_owned();
    let started = Instant::now();
    let response = next.run(request).await;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    tracing::info!(
        event = "emby_playback_callback",
        method = %method,
        path = %path,
        request_id = %request_id,
        status_code = response.status().as_u16(),
        duration_ms,
        "processed emby playback callback"
    );
    response
}

async fn trace_emby_playback_info(request: Request<Body>, next: Next) -> Response {
    let Some(item_id) = emby_playback_info_item_id(request.uri().path()).map(str::to_owned) else {
        return next.run(request).await;
    };
    let method = request.method().clone();
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown")
        .to_owned();
    let started = Instant::now();
    let response = next.run(request).await;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    tracing::info!(
        event = "emby_playback_info",
        method = %method,
        item_id_prefix = %playback_identifier_prefix(&item_id),
        request_id = %request_id,
        status_code = response.status().as_u16(),
        duration_ms,
        "processed emby PlaybackInfo request"
    );
    response
}

async fn trace_emby_media_stream_failure(request: Request<Body>, next: Next) -> Response {
    let Some(item_id) = emby_media_stream_item_id(request.uri().path()).map(str::to_owned) else {
        return next.run(request).await;
    };
    let method = request.method().clone();
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown")
        .to_owned();
    let started = Instant::now();
    let response = next.run(request).await;
    if response.status().is_client_error() || response.status().is_server_error() {
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::warn!(
            event = "emby_media_stream_failure",
            method = %method,
            item_id_prefix = %playback_identifier_prefix(&item_id),
            request_id = %request_id,
            status_code = response.status().as_u16(),
            duration_ms,
            "emby media stream request failed"
        );
    }
    response
}

fn is_emby_video_path(path: &str) -> bool {
    path.starts_with("/Videos/")
        || path.starts_with("/videos/")
        || path.starts_with("/emby/Videos/")
        || path.starts_with("/emby/videos/")
}

fn emby_path_without_prefix(path: &str) -> &str {
    path.strip_prefix("/emby")
        .unwrap_or(path)
        .strip_prefix('/')
        .unwrap_or(path)
}

fn is_emby_subtitle_path(path: &str) -> bool {
    let path = emby_path_without_prefix(path);
    let mut segments = path.split('/');
    matches!(segments.next(), Some("Videos"))
        && segments.next().is_some_and(|segment| !segment.is_empty())
        && segments.next().is_some_and(|segment| !segment.is_empty())
        && segments.next() == Some("Subtitles")
        && segments.next().is_some_and(|segment| !segment.is_empty())
        && segments.next() == Some("Stream")
        && segments.next().is_none()
}

fn is_emby_legacy_strm_path(path: &str) -> bool {
    let path = emby_path_without_prefix(path);
    let mut segments = path.split('/');
    matches!(segments.next(), Some("Videos" | "videos"))
        && segments.next().is_some_and(|segment| !segment.is_empty())
        && segments.next() == Some("original.strm")
        && segments.next().is_none()
}

fn is_registered_emby_video_path(path: &str) -> bool {
    emby_media_stream_item_id(path).is_some()
        || is_emby_subtitle_path(path)
        || is_emby_legacy_strm_path(path)
}

async fn reject_unmatched_emby_video_path(request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path();
    if is_emby_video_path(path) && !is_registered_emby_video_path(path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(request).await
}

fn web_root() -> PathBuf {
    if let Some(directory) = std::env::var_os("LUX_WEB_DIR") {
        return PathBuf::from(directory);
    }

    let dist = FsPath::new("web/dist");
    if dist.join("index.html").is_file() {
        dist.to_path_buf()
    } else {
        FsPath::new("web/src").to_path_buf()
    }
}

async fn web_logo() -> Response {
    static_response("image/svg+xml", include_str!("../../logo.svg"))
}

fn static_response(content_type: &'static str, body: &'static str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .header("Cache-Control", "no-cache")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn request_client_ip(headers: &HeaderMap, policy: &RemoteAccessPolicy) -> Option<String> {
    policy
        .reported_client_ip(
            header_str(headers, "x-lux-peer-ip"),
            header_str(headers, "x-forwarded-for"),
        )
        .map(|address| address.to_string())
}

fn login_attempt_key(headers: &HeaderMap, username: &str) -> String {
    format!(
        "{}:{}",
        header_str(headers, "x-lux-peer-ip").unwrap_or("local"),
        username.trim().to_ascii_lowercase()
    )
}

fn emby_page_params(query: &EmbyItemsQuery) -> Result<(i64, i64), StatusCode> {
    let offset = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(50);
    if offset < 0 || !(1..=100).contains(&limit) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok((offset, limit))
}

fn emby_person_page_params(query: &EmbyPersonsQuery) -> Result<(i64, i64), StatusCode> {
    let offset = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(50);
    if offset < 0 || limit < 1 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok((offset, limit))
}

fn emby_person_sort(value: Option<&str>) -> Result<PersonSort, StatusCode> {
    match value
        .unwrap_or("Name")
        .split(',')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Name")
        .to_ascii_lowercase()
        .as_str()
    {
        "name" => Ok(PersonSort::Name),
        "datecreated" => Ok(PersonSort::DateCreated),
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

fn emby_person_sort_order(value: Option<&str>) -> Result<bool, StatusCode> {
    match value
        .unwrap_or("Ascending")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "ascending" => Ok(false),
        "descending" => Ok(true),
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

fn emby_person_type_filter(person_types: Option<&str>) -> Option<&'static str> {
    let mut requested = person_types
        .unwrap_or("Actor")
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty());
    requested
        .find(|value| value.eq_ignore_ascii_case("Actor"))
        .map(|_| "Actor")
}

fn ensure_emby_user_scope(user: &UserRecord, requested_id: &str) -> Result<(), StatusCode> {
    let requested_id = requested_id
        .parse::<crate::domain::ids::UserId>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if user.is_admin || user.id == requested_id {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

fn emby_catalog_item_json_with_state(
    item: &CatalogItem,
    server_id: &str,
    user_state: Option<&crate::storage::StoredUserItemState>,
    nfo: Option<&LocalNfoDetails>,
    can_download: bool,
    fields: Option<&str>,
) -> Value {
    emby_catalog_item_json_with_state_and_aspect_ratio(
        item,
        server_id,
        user_state,
        EmbyItemJsonOptions {
            nfo,
            can_download,
            fields,
            primary_image_aspect_ratio: None,
            include_top_level_media_streams: false,
        },
    )
}

struct EmbyItemJsonOptions<'a> {
    nfo: Option<&'a LocalNfoDetails>,
    can_download: bool,
    fields: Option<&'a str>,
    primary_image_aspect_ratio: Option<f64>,
    include_top_level_media_streams: bool,
}

fn emby_catalog_item_json_with_state_and_aspect_ratio(
    item: &CatalogItem,
    server_id: &str,
    user_state: Option<&crate::storage::StoredUserItemState>,
    options: EmbyItemJsonOptions<'_>,
) -> Value {
    let EmbyItemJsonOptions {
        nfo,
        can_download,
        fields,
        primary_image_aspect_ratio,
        include_top_level_media_streams,
    } = options;
    let default_source = item
        .media_sources
        .iter()
        .find(|source| source.is_default)
        .or_else(|| item.media_sources.first());
    let runtime_ticks = item
        .runtime_ticks
        .or_else(|| default_source.and_then(|source| source.duration_ticks));
    let played_percentage = user_state.and_then(|state| {
        if state.position_ticks <= 0 {
            return None;
        }
        let runtime_ticks = runtime_ticks.filter(|value| *value > 0)?;
        Some((state.position_ticks.max(0) as f64 * 100.0 / runtime_ticks as f64).clamp(0.0, 100.0))
    });
    let mut image_tags = serde_json::Map::new();
    if let Some(tag) = item.poster_image_tag.as_ref() {
        image_tags.insert("Primary".to_owned(), json!(tag));
    } else if item.item_type == "EPISODE"
        && let Some(tag) = item.thumb_image_tag.as_ref()
    {
        // Filmly requests episode thumbnails through the standard Primary
        // image tag. A local Kodi-style `-thumb` image is the episode's
        // primary artwork when no dedicated poster exists.
        image_tags.insert("Primary".to_owned(), json!(tag));
    }
    if let Some(tag) = item.logo_image_tag.as_ref() {
        image_tags.insert("Logo".to_owned(), json!(tag));
    }
    if let Some(tag) = item.thumb_image_tag.as_ref() {
        image_tags.insert("Thumb".to_owned(), json!(tag));
    }
    if let Some(tag) = item.banner_image_tag.as_ref() {
        image_tags.insert("Banner".to_owned(), json!(tag));
    }
    if let Some(tag) = item.disc_image_tag.as_ref() {
        image_tags.insert("Disc".to_owned(), json!(tag));
    }
    if let Some(tag) = item.art_image_tag.as_ref() {
        image_tags.insert("Art".to_owned(), json!(tag));
    }
    if let Some(tag) = item.wallpaper_image_tag.as_ref() {
        image_tags.insert("Wallpaper".to_owned(), json!(tag));
    }
    let parent_id = item.parent_id.as_deref().unwrap_or(&item.library_id);
    let child_count = match item.item_type.as_str() {
        "SERIES" => item.season_count,
        "SEASON" => item.episode_count,
        _ => None,
    };
    let recursive_item_count = match item.item_type.as_str() {
        "SERIES" | "SEASON" => item.episode_count,
        _ => None,
    };
    let index_number = if item.item_type == "SEASON" {
        item.season_number
    } else {
        item.episode_number
    };
    let season_id = if item.item_type == "EPISODE" {
        item.parent_id.clone()
    } else {
        None
    };
    // Emby always exposes the series relationship on season and episode DTOs.
    // Filmly uses the season's SeriesId to construct the subsequent Episodes URL.
    let series_id = item
        .series_id
        .clone()
        .or_else(|| (item.item_type == "SEASON").then(|| item.parent_id.clone())?);
    let season_name = (item.item_type == "SEASON").then(|| item.title.clone());
    let episode_season_name = (item.item_type == "EPISODE")
        .then_some(item.season_number)
        .flatten()
        .map(|number| format!("Season {number:02}"));
    let is_folder = matches!(
        item.item_type.as_str(),
        "SERIES" | "SEASON" | "BOX_SET" | "FOLDER"
    );
    // Emby advertises sync capability on item details for playable media and
    // series containers. The list projection intentionally keeps the legacy
    // compact capability shape used by existing clients.
    let basic_sync_requested =
        fields.is_some_and(|fields| emby_fields_include(Some(fields), "BasicSyncInfo"));
    let supports_sync = (!is_folder || item.item_type == "SERIES")
        && (include_top_level_media_streams
            || (basic_sync_requested && item.item_type == "EPISODE"));
    let mut user_data = serde_json::Map::from_iter([
        (
            "PlaybackPositionTicks".to_owned(),
            json!(
                user_state
                    .map(|state| state.position_ticks)
                    .unwrap_or_default()
            ),
        ),
        (
            "PlayCount".to_owned(),
            json!(user_state.map(|state| state.play_count).unwrap_or_default()),
        ),
        (
            "IsFavorite".to_owned(),
            json!(user_state.map(|state| state.is_favorite).unwrap_or(false)),
        ),
        (
            "Played".to_owned(),
            json!(user_state.map(|state| state.is_played).unwrap_or(false)),
        ),
    ]);
    if let Some(played_percentage) = played_percentage {
        user_data.insert("PlayedPercentage".to_owned(), json!(played_percentage));
    }
    if let Some(last_played_at) = user_state.and_then(|state| state.last_played_at) {
        if let Some(last_played_date) = emby_timestamp(last_played_at) {
            user_data.insert("LastPlayedDate".to_owned(), json!(last_played_date));
        }
    }

    let mut object = serde_json::Map::from_iter([
        ("Name".to_owned(), json!(item.title)),
        ("Id".to_owned(), json!(item.id)),
        ("ServerId".to_owned(), json!(server_id)),
        ("Type".to_owned(), json!(emby_item_type(&item.item_type))),
        ("MediaType".to_owned(), json!("Video")),
        (
            "IsFolder".to_owned(),
            json!(matches!(
                item.item_type.as_str(),
                "SERIES" | "SEASON" | "BOX_SET" | "FOLDER"
            )),
        ),
        ("ParentId".to_owned(), json!(parent_id)),
        ("ImageTags".to_owned(), Value::Object(image_tags)),
        (
            "BackdropImageTags".to_owned(),
            if item.fanart_image_tags.is_empty() {
                item.fanart_image_tag
                    .as_ref()
                    .map(|tag| json!([tag]))
                    .unwrap_or_else(|| json!([]))
            } else {
                json!(item.fanart_image_tags)
            },
        ),
        ("UserData".to_owned(), Value::Object(user_data)),
    ]);

    if fields.is_none() {
        object.extend([
            ("SortName".to_owned(), json!(item.sort_title)),
            ("ForcedSortName".to_owned(), json!(item.sort_title)),
            (
                "OriginalTitle".to_owned(),
                json!(item.original_title.clone().unwrap_or_default()),
            ),
            ("SupportsSync".to_owned(), json!(supports_sync)),
            ("CanDelete".to_owned(), json!(false)),
            ("LockData".to_owned(), json!(false)),
            ("LockedFields".to_owned(), json!([])),
            ("ExternalUrls".to_owned(), json!([])),
            ("RemoteTrailers".to_owned(), json!([])),
            ("Taglines".to_owned(), json!([])),
            ("Genres".to_owned(), json!([])),
            ("GenreItems".to_owned(), json!([])),
            ("Studios".to_owned(), json!([])),
            ("TagItems".to_owned(), json!([])),
            ("LocalTrailerCount".to_owned(), json!(0)),
            ("Etag".to_owned(), json!(emby_item_etag(&item.id))),
            ("DisplayPreferencesId".to_owned(), json!(item.id)),
            ("PresentationUniqueKey".to_owned(), json!(item.id)),
            (
                "ParentBackdropImageTags".to_owned(),
                json!(item.series_fanart_image_tags),
            ),
            (
                "ProviderIds".to_owned(),
                json!(emby_provider_ids(&item.provider_ids)),
            ),
            ("CanDownload".to_owned(), json!(can_download && !is_folder)),
        ]);
        // Emby always emits the item's creation/modification timestamps and a
        // filesystem path on detail DTOs. Lux ids are UUIDv7, so the embedded
        // timestamp is the real item creation time; the path is a stable,
        // harmless label because Lux never reveals real local paths.
        if let Some(created) = emby_item_timestamp(&item.id) {
            object.insert("DateCreated".to_owned(), json!(created));
            object.insert("DateModified".to_owned(), json!(created));
        }
        object.insert(
            "Path".to_owned(),
            json!(emby_safe_path(item, default_source)),
        );
        if matches!(item.item_type.as_str(), "MOVIE" | "SERIES") {
            object.insert("OfficialRating".to_owned(), json!(""));
        }
        if item.item_type == "SERIES" {
            object.extend([
                ("AirDays".to_owned(), json!([])),
                ("DisplayOrder".to_owned(), json!("Aired")),
            ]);
            emby_insert_optional(&mut object, "Status", item.status.clone().map(Value::from));
        }
        if let Some(file_name) = emby_file_name(item, default_source) {
            object.insert("FileName".to_owned(), json!(file_name));
        }
        if !is_folder {
            emby_insert_optional(&mut object, "PartCount", default_source.map(|_| json!(1)));
            emby_insert_optional(
                &mut object,
                "Container",
                default_source
                    .and_then(|source| source.container.clone())
                    .map(Value::from),
            );
            emby_insert_optional(
                &mut object,
                "Size",
                default_source
                    .and_then(|source| source.size)
                    .map(Value::from),
            );
            emby_insert_optional(
                &mut object,
                "Bitrate",
                default_source
                    .and_then(|source| source.bitrate)
                    .map(Value::from),
            );
            if let Some(width) = emby_video_stream_dimension(default_source, "Width") {
                object.insert("Width".to_owned(), json!(width));
            }
            if let Some(height) = emby_video_stream_dimension(default_source, "Height") {
                object.insert("Height".to_owned(), json!(height));
            }
        }
        emby_insert_optional(
            &mut object,
            "CollectionType",
            (item.item_type == "BOX_SET").then(|| json!("movies")),
        );
        emby_insert_optional(
            &mut object,
            "PrimaryImageItemId",
            item.poster_image_tag
                .as_ref()
                .map(|_| json!(item.id.clone())),
        );
        emby_insert_optional(
            &mut object,
            "SeriesId",
            series_id.as_ref().map(|value| json!(value)),
        );
        emby_insert_optional(
            &mut object,
            "SeriesName",
            item.series_name.clone().map(Value::from),
        );
        emby_insert_optional(
            &mut object,
            "SeriesPrimaryImageTag",
            item.series_primary_image_tag.clone().map(Value::from),
        );
        let episode_season_name = (item.item_type == "EPISODE")
            .then_some(item.season_number)
            .flatten()
            .map(|number| format!("Season {number:02}"));
        emby_insert_optional(
            &mut object,
            "SeasonName",
            season_name.or(episode_season_name).map(Value::from),
        );
        emby_insert_optional(
            &mut object,
            "ParentLogoItemId",
            series_id.as_ref().map(|value| json!(value)),
        );
        emby_insert_optional(
            &mut object,
            "ParentBackdropItemId",
            series_id.as_ref().map(|value| json!(value)),
        );
        emby_insert_optional(
            &mut object,
            "SeasonId",
            season_id.as_ref().map(|value| json!(value)),
        );
        emby_insert_optional(&mut object, "IndexNumber", index_number.map(Value::from));
        emby_insert_optional(
            &mut object,
            "ParentIndexNumber",
            item.season_number.map(Value::from),
        );
        emby_insert_optional(&mut object, "Index", item.episode_number.map(Value::from));
        emby_insert_optional(
            &mut object,
            "ProductionYear",
            item.production_year.map(Value::from),
        );
        emby_insert_optional(
            &mut object,
            "PremiereDate",
            emby_datetime(item.premiere_date.as_deref()),
        );
        emby_insert_optional(&mut object, "CommunityRating", item.rating.map(Value::from));
        emby_insert_optional(
            &mut object,
            "Overview",
            item.overview.clone().map(Value::from),
        );
        emby_insert_optional(&mut object, "RunTimeTicks", runtime_ticks.map(Value::from));
        emby_insert_optional(&mut object, "ChildCount", child_count.map(Value::from));
        emby_insert_optional(
            &mut object,
            "RecursiveItemCount",
            recursive_item_count.map(Value::from),
        );
        if item.item_type == "EPISODE" {
            emby_insert_optional(
                &mut object,
                "ParentLogoImageTag",
                item.series_logo_image_tag.clone().map(Value::from),
            );
            emby_insert_optional(
                &mut object,
                "ParentThumbImageTag",
                item.series_thumb_image_tag.clone().map(Value::from),
            );
            emby_insert_optional(
                &mut object,
                "ParentThumbItemId",
                series_id.as_ref().map(|value| json!(value)),
            );
        }
        if let Some(aspect_ratio) = primary_image_aspect_ratio {
            object.insert("PrimaryImageAspectRatio".to_owned(), json!(aspect_ratio));
        }
    } else {
        if matches!(item.item_type.as_str(), "SEASON" | "EPISODE") {
            emby_insert_optional(
                &mut object,
                "SeriesId",
                series_id.as_ref().map(|value| json!(value)),
            );
            emby_insert_optional(
                &mut object,
                "SeriesName",
                item.series_name.clone().map(|value| json!(value)),
            );
            emby_insert_optional(
                &mut object,
                "SeriesPrimaryImageTag",
                item.series_primary_image_tag
                    .clone()
                    .map(|value| json!(value)),
            );
            emby_insert_optional(
                &mut object,
                "SeasonName",
                season_name
                    .clone()
                    .or_else(|| episode_season_name.clone())
                    .map(|value| json!(value)),
            );
            emby_insert_optional(
                &mut object,
                "ParentLogoItemId",
                series_id.clone().map(|value| json!(value)),
            );
            emby_insert_optional(
                &mut object,
                "ParentBackdropItemId",
                series_id.clone().map(|value| json!(value)),
            );
            object.insert(
                "ParentBackdropImageTags".to_owned(),
                json!(item.series_fanart_image_tags),
            );
            emby_insert_optional(
                &mut object,
                "IndexNumber",
                index_number.map(|value| json!(value)),
            );
            emby_insert_optional(
                &mut object,
                "ChildCount",
                child_count.map(|value| json!(value)),
            );
            if item.item_type == "EPISODE" {
                emby_insert_optional(
                    &mut object,
                    "SeasonId",
                    season_id.clone().map(|value| json!(value)),
                );
                emby_insert_optional(
                    &mut object,
                    "ParentIndexNumber",
                    item.season_number.map(|value| json!(value)),
                );
                emby_insert_optional(
                    &mut object,
                    "Index",
                    item.episode_number.map(|value| json!(value)),
                );
            }
        }
        if emby_fields_include(fields, "BasicSyncInfo") {
            object.insert("SupportsSync".to_owned(), json!(supports_sync));
        }
        if emby_fields_include(fields, "DateModified") {
            if let Some(modified) = emby_item_timestamp(&item.id) {
                object.insert("DateModified".to_owned(), json!(modified));
            }
        }
        if emby_fields_include(fields, "Path") {
            object.insert(
                "Path".to_owned(),
                json!(emby_safe_path(item, default_source)),
            );
        }
        if emby_fields_include(fields, "CanDownload") {
            object.insert("CanDownload".to_owned(), json!(can_download && !is_folder));
        }
        if emby_fields_include(fields, "Overview") {
            emby_insert_optional(
                &mut object,
                "Overview",
                item.overview.clone().map(Value::from),
            );
        }
        if emby_fields_include(fields, "PremiereDate")
            || (item.item_type == "EPISODE" && emby_fields_include(fields, "MediaSources"))
        {
            emby_insert_optional(
                &mut object,
                "PremiereDate",
                emby_datetime(item.premiere_date.as_deref()),
            );
        }
        if emby_fields_include(fields, "ProviderIds") {
            object.insert(
                "ProviderIds".to_owned(),
                json!(emby_provider_ids(&item.provider_ids)),
            );
        }
        if emby_fields_include(fields, "People") {
            // Catalog pages do not load the potentially large people snapshot;
            // preserve Emby's non-null collection contract for clients that
            // map this field eagerly. Full item details add the populated list.
            object.insert("People".to_owned(), json!([]));
        }
        if emby_fields_include(fields, "Genres") {
            object.insert("Genres".to_owned(), json!([]));
            object.insert("GenreItems".to_owned(), json!([]));
        } else if emby_fields_include(fields, "GenreItems") {
            object.insert("GenreItems".to_owned(), json!([]));
        }
        if emby_fields_include(fields, "ProductionYear") {
            emby_insert_optional(
                &mut object,
                "ProductionYear",
                item.production_year.map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "PremiereDate") {
            emby_insert_optional(
                &mut object,
                "PremiereDate",
                emby_datetime(item.premiere_date.as_deref()),
            );
        }
        if emby_fields_include(fields, "CommunityRating") {
            emby_insert_optional(
                &mut object,
                "CommunityRating",
                item.rating.map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "RunTimeTicks") {
            emby_insert_optional(
                &mut object,
                "RunTimeTicks",
                runtime_ticks.map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "ChildCount") {
            emby_insert_optional(
                &mut object,
                "ChildCount",
                child_count.map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "RecursiveItemCount") {
            emby_insert_optional(
                &mut object,
                "RecursiveItemCount",
                recursive_item_count.map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "Container") || emby_fields_include(fields, "MediaSources") {
            emby_insert_optional(
                &mut object,
                "Container",
                default_source
                    .and_then(|source| source.container.clone())
                    .map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "Size") {
            emby_insert_optional(
                &mut object,
                "Size",
                default_source
                    .and_then(|source| source.size)
                    .map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "Bitrate") || emby_fields_include(fields, "MediaSources") {
            emby_insert_optional(
                &mut object,
                "Bitrate",
                default_source
                    .and_then(|source| source.bitrate)
                    .map(|value| json!(value)),
            );
        }
        if item.item_type == "EPISODE" {
            emby_insert_optional(
                &mut object,
                "ParentLogoImageTag",
                item.series_logo_image_tag.clone().map(Value::from),
            );
            emby_insert_optional(
                &mut object,
                "ParentThumbImageTag",
                item.series_thumb_image_tag.clone().map(Value::from),
            );
            emby_insert_optional(
                &mut object,
                "ParentThumbItemId",
                series_id.as_ref().map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "PrimaryImageAspectRatio") {
            emby_insert_optional(
                &mut object,
                "PrimaryImageAspectRatio",
                primary_image_aspect_ratio.map(|value| json!(value)),
            );
        }
    }
    let mut value = Value::Object(object);
    let include_media_streams = !is_folder
        && (fields.is_none()
            || emby_fields_include(fields, "MediaStreams")
            || emby_fields_include(fields, "MediaSources"));
    if !is_folder
        && emby_fields_include(fields, "MediaSources")
        && let Value::Object(object) = &mut value
    {
        object.insert(
            "MediaSources".to_owned(),
            Value::Array(
                item.media_sources
                    .iter()
                    .map(|source| {
                        emby_media_source_json_with_resolver_and_chapters(
                            &item.id,
                            source,
                            include_media_streams,
                            false,
                            emby_fields_include(fields, "Chapters"),
                        )
                    })
                    .collect(),
            ),
        );
    }
    if !is_folder
        && include_top_level_media_streams
        && let Value::Object(object) = &mut value
    {
        object.insert(
            "MediaStreams".to_owned(),
            Value::Array(
                default_source
                    .map(|source| source.streams.iter().map(emby_media_stream_json).collect())
                    .unwrap_or_default(),
            ),
        );
    }
    if !is_folder
        && emby_fields_include(fields, "Chapters")
        && let Value::Object(object) = &mut value
    {
        object.insert(
            "Chapters".to_owned(),
            Value::Array(
                default_source
                    .map(|source| source.chapters.iter().map(emby_chapter_json).collect())
                    .unwrap_or_default(),
            ),
        );
    }
    apply_emby_nfo_details(&mut value, item, nfo, fields);
    value
}

fn apply_emby_nfo_details(
    value: &mut Value,
    item: &CatalogItem,
    nfo: Option<&LocalNfoDetails>,
    fields: Option<&str>,
) {
    let Some(nfo) = nfo else {
        return;
    };
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let include = |field: &str| emby_fields_include(fields, field);

    if include("CommunityRating")
        && let Some(rating) = nfo.rating
    {
        object.insert("CommunityRating".to_owned(), json!(rating));
    }
    if include("PremiereDate")
        && let Some(date) = nfo
            .premiered
            .as_deref()
            .or(nfo.release_date.as_deref())
            .or(nfo.aired.as_deref())
            .or(item.premiere_date.as_deref())
    {
        object.insert(
            "PremiereDate".to_owned(),
            emby_datetime(Some(date)).unwrap_or(Value::Null),
        );
    }
    if include("EndDate")
        && let Some(date) = nfo.last_air_date.as_deref()
    {
        object.insert(
            "EndDate".to_owned(),
            emby_datetime(Some(date)).unwrap_or(Value::Null),
        );
    }
    if include("RunTimeTicks")
        && let Some(runtime) = nfo.runtime
        && let Some(runtime_ticks) = i64::from(runtime)
            .checked_mul(60)
            .and_then(|value| value.checked_mul(10_000_000))
    {
        object.insert("RunTimeTicks".to_owned(), json!(runtime_ticks));
    }
    if include("OriginalLanguage")
        && let Some(language) = nfo.original_language.as_deref()
    {
        object.insert("OriginalLanguage".to_owned(), json!(language));
    }
    if include("Status")
        && let Some(status) = nfo.status.as_deref()
    {
        object.insert("Status".to_owned(), json!(status));
    }
    if include("OfficialRating")
        && let Some(certification) = nfo.certification.as_deref()
    {
        object.insert("OfficialRating".to_owned(), json!(certification));
    }
    if include("ProviderIds") {
        let mut provider_ids = item.provider_ids.clone();
        provider_ids.extend(nfo.provider_ids.clone());
        object.insert(
            "ProviderIds".to_owned(),
            json!(emby_provider_ids(&provider_ids)),
        );
    }
    if include("Taglines") && !nfo.tagline.as_deref().unwrap_or_default().is_empty() {
        object.insert("Taglines".to_owned(), json!([nfo.tagline]));
    }
    if include("Genres") && !nfo.genres.is_empty() {
        object.insert("Genres".to_owned(), json!(nfo.genres));
    }
    if include("GenreItems") && !nfo.genres.is_empty() {
        object.insert(
            "GenreItems".to_owned(),
            json!(
                nfo.genres
                    .iter()
                    .map(|name| {
                        json!({
                            "Name": name,
                            "Id": emby_stable_named_id("genre", name),
                        })
                    })
                    .collect::<Vec<_>>()
            ),
        );
    }
    if include("Studios") && !nfo.studios.is_empty() {
        object.insert(
            "Studios".to_owned(),
            json!(
                nfo.studios
                    .iter()
                    .map(|name| {
                        json!({
                            "Name": name,
                            "Id": emby_stable_named_id("studio", name),
                        })
                    })
                    .collect::<Vec<_>>()
            ),
        );
    }
    if include("RemoteTrailers") && !nfo.trailers.is_empty() {
        let trailers = nfo
            .trailers
            .iter()
            .enumerate()
            .map(|(index, url)| json!({ "Url": url, "Name": format!("Trailer {}", index + 1) }))
            .collect::<Vec<_>>();
        object.insert("RemoteTrailers".to_owned(), json!(trailers));
    }
    if (include("ExternalUrls") || include("HomePageUrl"))
        && let Some(website) = nfo.website.as_deref()
    {
        object.insert(
            "ExternalUrls".to_owned(),
            json!([{ "Name": "Website", "Url": website }]),
        );
    }
}

fn emby_insert_optional(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<Value>,
) {
    if let Some(value) = value {
        object.insert(key.to_owned(), value);
    }
}

/// Stable, server-local identifier that mirrors Emby's per-item Etag. Emby uses
/// a content hash; Lux derives one from the item id so it stays stable across
/// requests without leaking library paths.
fn emby_item_etag(item_id: &str) -> String {
    Sha256::digest(item_id.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Emby serializes timestamps as UTC with seven fractional digits, e.g.
/// `2026-03-29T17:51:26.0000000Z`. Lux stores unix seconds in user state.
fn emby_timestamp(unix_seconds: i64) -> Option<String> {
    let datetime = time::OffsetDateTime::from_unix_timestamp(unix_seconds).ok()?;
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.0000000Z",
        datetime.year(),
        u8::from(datetime.month()),
        datetime.day(),
        datetime.hour(),
        datetime.minute(),
        datetime.second(),
    ))
}

/// Emby exposes a display filename on every item DTO. Lux does not store source
/// file names, so derive a stable, harmless label from the title and container.
fn emby_file_name(item: &CatalogItem, default_source: Option<&CatalogSource>) -> Option<String> {
    if matches!(
        item.item_type.as_str(),
        "SERIES" | "SEASON" | "BOX_SET" | "FOLDER"
    ) {
        return Some(item.title.clone());
    }
    let container = default_source
        .and_then(|source| source.container.as_deref())
        .filter(|value| !value.is_empty())
        .unwrap_or("strm");
    Some(format!("{}.{}", item.title, container))
}

/// Emby always exposes a filesystem path on item DTOs. Lux never reveals real
/// local paths, so synthesize a stable, harmless path from the library id and
/// title; clients only display this value.
fn emby_safe_path(item: &CatalogItem, default_source: Option<&CatalogSource>) -> String {
    let title = &item.title;
    if matches!(
        item.item_type.as_str(),
        "SERIES" | "SEASON" | "BOX_SET" | "FOLDER"
    ) {
        return format!("/media/{}/{title}", item.library_id);
    }
    let container = default_source
        .and_then(|source| source.container.as_deref())
        .filter(|value| !value.is_empty())
        .unwrap_or("strm");
    format!("/media/{}/{title}.{container}", item.library_id)
}

/// Extracts the creation timestamp embedded in Lux's UUIDv7 item ids. The first
/// 48 bits of a v7 uuid are Unix milliseconds, which is exactly when Lux
/// generated the id for the media item. Non-v7 ids (imported/migrated data)
/// return None and the field is omitted instead of emitting a fabricated value.
fn emby_item_timestamp(item_id: &str) -> Option<String> {
    let compact = item_id.replace('-', "");
    if compact.len() != 32 || compact.as_bytes().get(12).is_none_or(|byte| *byte != b'7') {
        return None;
    }
    let millis = u64::from_str_radix(&compact[..12], 16).ok()?;
    emby_timestamp(i64::try_from(millis / 1000).ok()?)
}

/// Reads a video stream dimension (Width or Height) from the default source's
/// probe details, matching Emby's per-item Width/Height fields.
fn emby_video_stream_dimension(
    default_source: Option<&CatalogSource>,
    emby_key: &str,
) -> Option<i64> {
    let stream = default_source?
        .streams
        .iter()
        .find(|stream| stream.stream_type.eq_ignore_ascii_case("video"))?;
    let value = stream
        .details
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(emby_key))?
        .1;
    value.as_i64().or_else(|| {
        value
            .as_str()
            .and_then(|text| text.trim().parse::<i64>().ok())
    })
}

async fn emby_primary_image_aspect_ratio(
    state: &AppState,
    principal: AccessPrincipal,
    item_id: &str,
) -> Option<f64> {
    if let Some((width, height)) = state
        .database
        .as_ref()?
        .find_primary_image_dimensions(item_id)
        .await
        .ok()?
    {
        if width > 0 && height > 0 {
            return Some(f64::from(width) / f64::from(height));
        }
    }

    let images = state.images.as_ref()?;
    let image = images
        .resolve(principal, item_id, "POSTER", 0)
        .await
        .ok()??;
    let dimensions = read_image_dimensions(&image.path).await?;
    let width = dimensions.0;
    let height = dimensions.1;
    if width <= 0 || height <= 0 {
        return None;
    }
    if let Some(database) = state.database.as_ref() {
        let _ = database
            .set_item_image_dimensions(item_id, "POSTER", 0, width, height)
            .await;
    }
    Some(f64::from(width) / f64::from(height))
}

fn emby_datetime(value: Option<&str>) -> Option<Value> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    let value = if value.contains('T') {
        value.to_owned()
    } else {
        format!("{value}T00:00:00.0000000Z")
    };
    Some(json!(value))
}

fn emby_provider_ids(provider_ids: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    provider_ids
        .iter()
        .map(|(name, value)| {
            let name = match name.to_ascii_lowercase().as_str() {
                "tmdb" => "Tmdb",
                "tvdb" => "Tvdb",
                "imdb" => "Imdb",
                _ => name,
            };
            (name.to_owned(), value.clone())
        })
        .collect()
}

#[cfg(test)]
fn emby_media_source_json(
    item_id: &str,
    source: &crate::application::catalog::CatalogSource,
    include_media_streams: bool,
) -> Value {
    emby_media_source_json_with_resolver_and_chapters(
        item_id,
        source,
        include_media_streams,
        false,
        false,
    )
}

fn emby_media_source_json_with_resolver(
    item_id: &str,
    source: &crate::application::catalog::CatalogSource,
    include_media_streams: bool,
    strm_resolver_available: bool,
) -> Value {
    emby_media_source_json_with_resolver_and_chapters(
        item_id,
        source,
        include_media_streams,
        strm_resolver_available,
        true,
    )
}

fn emby_media_source_json_with_resolver_and_chapters(
    item_id: &str,
    source: &crate::application::catalog::CatalogSource,
    include_media_streams: bool,
    strm_resolver_available: bool,
    include_chapters: bool,
) -> Value {
    let strm_target_kind = (source.source_kind == "STRM_URL").then(|| {
        source
            .external_url
            .as_deref()
            .map(classify_strm_target)
            .map_or(StrmTargetKind::Empty, |target| target.kind)
    });
    let is_remote = matches!(strm_target_kind, Some(StrmTargetKind::Url));
    let is_local_strm_target = matches!(strm_target_kind, Some(StrmTargetKind::Path));
    let is_resolver_target = strm_resolver_available
        && matches!(
            strm_target_kind,
            Some(StrmTargetKind::Smb | StrmTargetKind::Ftp)
        );
    let direct_stream_url = if source.source_kind == "LOCAL_FILE"
        || is_local_strm_target
        || is_remote
        || is_resolver_target
    {
        Some(emby_media_source_stream_url(item_id, source))
    } else {
        None
    };
    let is_remote_playback = is_remote || is_resolver_target;
    let is_playable =
        source.source_kind == "LOCAL_FILE" || is_local_strm_target || is_remote_playback;
    let default_audio_stream_index = source
        .streams
        .iter()
        .find(|stream| stream.stream_type == "AUDIO" && stream.is_default)
        .or_else(|| {
            source
                .streams
                .iter()
                .find(|stream| stream.stream_type == "AUDIO")
        })
        .map(|stream| stream.index)
        .unwrap_or(-1);
    let mut value = json!({
        "Id": source.id,
        "ItemId": item_id,
        "Name": source.edition_name,
        "Edition": source.edition_name,
        "Quality": source.quality_label,
        "VideoType": source.quality_label,
        "Container": source.container,
        "Size": source.size,
        "Bitrate": source.bitrate,
        "RunTimeTicks": source.duration_ticks,
        "Path": source.external_url,
        "IsDefault": source.is_default,
        "Protocol": if is_remote_playback { "Http" } else { "File" },
        "Type": "Default",
        "IsRemote": is_remote_playback,
        "SupportsDirectPlay": is_playable,
        "SupportsDirectStream": is_playable,
        "SupportsProbing": !source.probe_status.eq_ignore_ascii_case("FAILED"),
        "SupportsTranscoding": false,
        "DirectStreamUrl": direct_stream_url,
        // Android clients deserialize this compatibility field as a number,
        // even while a source is waiting for media probing and has no audio
        // stream yet. Keep the wire type numeric without selecting a video
        // stream as audio.
        "DefaultAudioStreamIndex": default_audio_stream_index,
        "Formats": [],
        "HasMixedProtocols": false,
        "IsInfiniteStream": false,
        "ReadAtNativeFramerate": false,
        "RequiredHttpHeaders": {},
        "RequiresClosing": false,
        "RequiresOpening": false,
        "RequiresLooping": false,
        // Some Android clients use an independent media request stack and
        // drop the Emby auth headers sent to PlaybackInfo. This standard Emby
        // flag tells them to append the current API key to the direct URL,
        // while keeping the long-lived token out of the response URL itself.
        "AddApiKeyToDirectStreamUrl": true,
    });
    if include_chapters && let Value::Object(object) = &mut value {
        object.insert(
            "Chapters".to_owned(),
            Value::Array(source.chapters.iter().map(emby_chapter_json).collect()),
        );
    }
    if include_media_streams && let Value::Object(object) = &mut value {
        object.insert(
            "MediaStreams".to_owned(),
            Value::Array(source.streams.iter().map(emby_media_stream_json).collect()),
        );
    }
    value
}

fn emby_source_needs_strm_resolver(source: &crate::application::catalog::CatalogSource) -> bool {
    source.source_kind == "STRM_URL"
        && source.external_url.as_deref().is_some_and(|target| {
            matches!(
                classify_strm_target(target).kind,
                StrmTargetKind::Smb | StrmTargetKind::Ftp
            )
        })
}

fn emby_media_source_stream_url(
    item_id: &str,
    source: &crate::application::catalog::CatalogSource,
) -> String {
    let stream_suffix = source
        .container
        .as_deref()
        .filter(|container| {
            !(source.source_kind == "STRM_URL" && container.eq_ignore_ascii_case("strm"))
        })
        .map(|container| format!(".{container}"))
        .unwrap_or_default();
    format!(
        "/Videos/{item_id}/stream{stream_suffix}?MediaSourceId={}",
        source.id
    )
}

fn emby_signed_direct_stream_url(
    service: &WebPlaybackSessionService,
    item_id: &str,
    source: &crate::application::catalog::CatalogSource,
    user: &UserRecord,
) -> Option<String> {
    let expires_at = current_unix_timestamp().saturating_add(EMBY_DIRECT_STREAM_TTL_SECONDS);
    let user_id = user.id.to_string();
    let signature = service.sign_emby_direct_stream(
        &user_id,
        user.is_admin,
        item_id,
        &source.id,
        expires_at,
    )?;
    let mut url = emby_media_source_stream_url(item_id, source);
    url.push_str("&luxPlaybackUserId=");
    url.push_str(&percent_encode_filename(&user_id));
    url.push_str("&luxPlaybackAdmin=");
    url.push_str(if user.is_admin { "1" } else { "0" });
    url.push_str("&luxPlaybackExpires=");
    url.push_str(&expires_at.to_string());
    url.push_str("&luxPlaybackSignature=");
    url.push_str(&percent_encode_filename(&signature));
    Some(url)
}

fn emby_chapter_json(chapter: &crate::application::catalog::CatalogChapter) -> Value {
    let mut value = json!({
        "StartPositionTicks": chapter.start_position_ticks,
        "MarkerType": match chapter.marker_type.as_str() {
            "INTRO_START" => "IntroStart",
            "INTRO_END" => "IntroEnd",
            "CREDITS_START" => "CreditsStart",
            _ => "Chapter",
        },
        "ChapterIndex": chapter.chapter_index,
    });
    if let Some(name) = chapter.name.as_deref().filter(|name| !name.is_empty())
        && let Value::Object(object) = &mut value
    {
        object.insert("Name".to_owned(), json!(name));
    }
    value
}

fn is_http_strm_target(value: &str) -> bool {
    matches!(classify_strm_target(value).kind, StrmTargetKind::Url)
}

fn normalize_strm_http_location(value: &str) -> Option<HeaderValue> {
    if value.is_ascii() {
        return HeaderValue::from_str(value).ok();
    }
    if !is_http_strm_target(value) {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    HeaderValue::from_str(url.as_str()).ok()
}

fn emby_media_stream_json(stream: &crate::application::catalog::CatalogStream) -> Value {
    let language = stream
        .language
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("und");
    let display_title = stream
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(emby_stream_type(&stream.stream_type));
    let mut value = json!({
        "Index": stream.index,
        "Type": emby_stream_type(&stream.stream_type),
        "Codec": stream.codec,
        "Language": language,
        "DisplayTitle": display_title,
        "AttachmentSize": 0,
        "IsAnamorphic": false,
        "Protocol": if stream.is_external { "Http" } else { "File" },
        "SupportsExternalStream": stream.is_external,
        "IsExternal": stream.is_external,
        "IsDefault": stream.is_default,
        "IsForced": stream.is_forced,
    });
    if let Value::Object(object) = &mut value {
        for (key, detail) in &stream.details {
            let Some(detail) = normalize_emby_media_stream_detail(key, detail) else {
                continue;
            };
            object.entry(key.clone()).or_insert(detail);
        }
    }
    value
}

fn normalize_emby_media_stream_detail(key: &str, value: &Value) -> Option<Value> {
    const INTEGER_FIELDS: [&str; 9] = [
        "BitRate",
        "BitDepth",
        "RefFrames",
        "Height",
        "Width",
        "Level",
        "Channels",
        "SampleRate",
        "AttachmentSize",
    ];
    const FLOAT_FIELDS: [&str; 2] = ["AverageFrameRate", "RealFrameRate"];
    const BOOLEAN_FIELDS: [&str; 3] = ["IsInterlaced", "IsHearingImpaired", "IsTextSubtitleStream"];

    if INTEGER_FIELDS
        .iter()
        .any(|field| key.eq_ignore_ascii_case(field))
    {
        return emby_integer_value(value);
    }
    if FLOAT_FIELDS
        .iter()
        .any(|field| key.eq_ignore_ascii_case(field))
    {
        return emby_frame_rate_value(value);
    }
    if BOOLEAN_FIELDS
        .iter()
        .any(|field| key.eq_ignore_ascii_case(field))
    {
        return emby_boolean_value(value);
    }
    (!value.is_null()).then(|| value.clone())
}

fn emby_integer_value(value: &Value) -> Option<Value> {
    match value {
        Value::Number(value) if value.as_i64().is_some() => Some(Value::Number(value.clone())),
        Value::String(value) => value.trim().parse::<i64>().ok().map(Value::from),
        _ => None,
    }
}

fn emby_frame_rate_value(value: &Value) -> Option<Value> {
    let number = match value {
        Value::Number(value) => value.as_f64()?,
        Value::String(value) => {
            let value = value.trim();
            if let Some((numerator, denominator)) = value.split_once('/') {
                let numerator = numerator.trim().parse::<f64>().ok()?;
                let denominator = denominator.trim().parse::<f64>().ok()?;
                if denominator == 0.0 {
                    return None;
                }
                numerator / denominator
            } else {
                value.parse::<f64>().ok()?
            }
        }
        _ => return None,
    };
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    if number.fract() == 0.0 && number <= i64::MAX as f64 {
        return Some(Value::from(number as i64));
    }
    serde_json::Number::from_f64(number).map(Value::Number)
}

fn emby_boolean_value(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(value) => Some(Value::Bool(*value)),
        Value::Number(value) => value.as_i64().map(|value| Value::Bool(value != 0)),
        Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(Value::Bool(true)),
            "false" | "0" | "no" => Some(Value::Bool(false)),
            _ => None,
        },
        _ => None,
    }
}

fn emby_library_view_json(library: &LibraryRecord, server_id: &str, child_count: i64) -> Value {
    json!({
        "Name": library.name,
        "SortName": library.name,
        "Id": library.id,
        "ServerId": server_id,
        "Type": "CollectionFolder",
        "IsFolder": true,
        "MediaType": "Video",
        "CollectionType": emby_collection_type(library.kind),
        "ChildCount": child_count,
        "RecursiveItemCount": child_count,
        "PrimaryImageItemId": library.cover_image_tag.as_ref().map(|_| library.id.to_string()),
        "PrimaryImageTag": library.cover_image_tag,
        "ImageTags": library
            .cover_image_tag
            .as_ref()
            .map(|tag| json!({"Primary": tag}))
            .unwrap_or_else(|| json!({})),
        "BackdropImageTags": [],
        "UserData": {
            "PlaybackPositionTicks": 0,
            "PlayCount": 0,
            "IsFavorite": false,
            "Played": false,
        },
    })
}

fn emby_virtual_folder_json(
    view: &LibraryView,
    global_media_strategy: &MediaStrategySettings,
    resume_played_percent: i64,
    resume_min_ticks: i64,
) -> Value {
    let media_strategy = view
        .library
        .media_strategy_json
        .as_deref()
        .and_then(|value| serde_json::from_str::<MediaStrategySettings>(value).ok())
        .unwrap_or_else(|| global_media_strategy.clone());
    let collection_type = emby_collection_type(view.library.kind);
    json!({
        "Name": view.library.name,
        "Locations": view
            .roots
            .iter()
            .map(|root| root.display_path.to_string_lossy().to_string())
            .collect::<Vec<_>>(),
        "CollectionType": collection_type,
        "LibraryOptions": emby_virtual_folder_options_json(
            view,
            &media_strategy,
            resume_played_percent,
            resume_min_ticks,
        ),
        "Id": view.library.id,
        "Guid": view.library.id,
        "ItemId": view.library.id,
        "PrimaryImageItemId": view
            .library
            .cover_image_tag
            .as_ref()
            .map(|_| view.library.id),
        "RefreshProgress": null,
        "RefreshStatus": "Idle",
    })
}

fn emby_virtual_folder_options_json(
    view: &LibraryView,
    media_strategy: &MediaStrategySettings,
    resume_played_percent: i64,
    resume_min_ticks: i64,
) -> Value {
    let collection_type = emby_collection_type(view.library.kind);
    let type_options = match view.library.kind {
        LibraryKind::Movie => vec![emby_library_type_options_json("Movie", media_strategy)],
        LibraryKind::Series => vec![emby_library_type_options_json("Series", media_strategy)],
        LibraryKind::Mixed => vec![
            emby_library_type_options_json("Movie", media_strategy),
            emby_library_type_options_json("Series", media_strategy),
        ],
    };
    json!({
        "EnableArchiveMediaFiles": false,
        "EnablePhotos": false,
        "EnableRealtimeMonitor": true,
        "EnableChapterImageExtraction": false,
        "ExtractChapterImagesDuringLibraryScan": false,
        "DownloadImagesInAdvance": false,
        "PathInfos": view.roots.iter().map(|root| json!({
            "Path": root.display_path.to_string_lossy().to_string(),
            "NetworkPath": "",
        })).collect::<Vec<_>>(),
        "SaveLocalMetadata": true,
        "SaveLocalThumbnailSets": false,
        "ImportMissingEpisodes": false,
        "EnableAutomaticSeriesGrouping": false,
        "EnableEmbeddedTitles": false,
        "EnableAudioResume": false,
        "AutomaticRefreshIntervalDays": 0,
        "PreferredMetadataLanguage": media_strategy.metadata_language,
        "ContentType": collection_type,
        "MetadataCountryCode": media_strategy.region,
        "SeasonZeroDisplayName": "Specials",
        "MetadataSavers": ["Nfo"],
        "DisabledLocalMetadataReaders": [],
        "LocalMetadataReaderOrder": ["Nfo"],
        "DisabledSubtitleFetchers": [],
        "SubtitleFetcherOrder": [],
        "SkipSubtitlesIfEmbeddedSubtitlesPresent": true,
        "SkipSubtitlesIfAudioTrackMatches": false,
        "SubtitleDownloadLanguages": media_strategy
            .subtitles
            .languages
            .iter()
            .map(|language| emby_subtitle_language_code(language))
            .collect::<Vec<_>>(),
        "RequirePerfectSubtitleMatch": false,
        "SaveSubtitlesWithMedia": false,
        "ForcedSubtitlesOnly": media_strategy.subtitles.forced_only,
        "TypeOptions": type_options,
        "CollapseSingleItemFolders": false,
        "MinResumePct": 0,
        "MaxResumePct": resume_played_percent,
        "MinResumeDurationSeconds": resume_min_ticks
            .max(0)
            .saturating_add(9_999_999)
            / 10_000_000,
        "ThumbnailImagesIntervalSeconds": 0,
    })
}

fn emby_library_type_options_json(
    item_type: &str,
    media_strategy: &MediaStrategySettings,
) -> Value {
    let mut image_options = Vec::new();
    if media_strategy.images.poster {
        image_options.push(json!({
            "Type": "Primary",
            "Limit": 1,
            "MinWidth": media_strategy.images.min_download_width,
        }));
    }
    if media_strategy.images.artwork {
        image_options.push(json!({
            "Type": "Art",
            "Limit": 1,
            "MinWidth": media_strategy.images.min_download_width,
        }));
    }
    if media_strategy.images.banner {
        image_options.push(json!({
            "Type": "Banner",
            "Limit": 1,
            "MinWidth": media_strategy.images.min_download_width,
        }));
    }
    if media_strategy.images.logo {
        image_options.push(json!({
            "Type": "Logo",
            "Limit": 1,
            "MinWidth": media_strategy.images.min_download_width,
        }));
    }
    if media_strategy.images.thumbnail {
        image_options.push(json!({
            "Type": "Thumb",
            "Limit": 1,
            "MinWidth": media_strategy.images.min_download_width,
        }));
    }
    if media_strategy.images.disc {
        image_options.push(json!({
            "Type": "Disc",
            "Limit": 1,
            "MinWidth": media_strategy.images.min_download_width,
        }));
    }
    if media_strategy.images.max_backdrop_count > 0 {
        image_options.push(json!({
            "Type": "Backdrop",
            "Limit": media_strategy.images.max_backdrop_count,
            "MinWidth": media_strategy.images.min_download_width,
        }));
    }

    json!({
        "Type": item_type,
        "MetadataFetchers": [],
        "MetadataFetcherOrder": [],
        "ImageFetchers": [],
        "ImageFetcherOrder": [],
        "ImageOptions": image_options,
    })
}

fn emby_subtitle_language_code(language: &str) -> String {
    match language.split('-').next().unwrap_or(language) {
        "zh" => "chi".to_owned(),
        "en" => "eng".to_owned(),
        "ja" => "jpn".to_owned(),
        "ko" => "kor".to_owned(),
        "fr" => "fra".to_owned(),
        "de" => "deu".to_owned(),
        "es" => "spa".to_owned(),
        "it" => "ita".to_owned(),
        "ru" => "rus".to_owned(),
        _ => language.to_owned(),
    }
}

fn emby_collection_type(kind: LibraryKind) -> Option<&'static str> {
    match kind {
        LibraryKind::Movie => Some("movies"),
        LibraryKind::Series => Some("tvshows"),
        LibraryKind::Mixed => None,
    }
}

fn emby_item_type(item_type: &str) -> &'static str {
    match item_type {
        "MOVIE" => "Movie",
        "SERIES" => "Series",
        "SEASON" => "Season",
        "EPISODE" => "Episode",
        "BOX_SET" => "BoxSet",
        _ => "Folder",
    }
}

fn emby_stream_type(stream_type: &str) -> &'static str {
    match stream_type {
        "VIDEO" => "Video",
        "AUDIO" => "Audio",
        "SUBTITLE" => "Subtitle",
        _ => "Unknown",
    }
}

fn emby_user_json(
    user: &UserRecord,
    server_id: &str,
    server_name: &str,
    ordered_views: &[String],
) -> Value {
    json!({
        "Id": user.id.to_string(),
        "ServerId": server_id,
        "ServerName": server_name,
        "Name": user.display_name,
        "HasPassword": true,
        "HasConfiguredPassword": true,
        "HasConfiguredEasyPassword": false,
        "EnableAutoLogin": false,
        "LastLoginDate": "1970-01-01T00:00:00.0000000Z",
        "LastActivityDate": "1970-01-01T00:00:00.0000000Z",
        "Configuration": emby_user_configuration_json(ordered_views),
        "Policy": emby_user_policy_json(user),
    })
}

fn emby_user_configuration_json(ordered_views: &[String]) -> Value {
    json!({
        "AudioLanguagePreference": "",
        "PlayDefaultAudioTrack": true,
        "SubtitleLanguagePreference": "",
        "DisplayMissingEpisodes": false,
        "GroupedFolders": [],
        "SubtitleMode": "Default",
        "DisplayCollectionsView": true,
        "EnableLocalPassword": false,
        "OrderedViews": ordered_views,
        "LatestItemsExcludes": [],
        "MyMediaExcludes": [],
        "HidePlayedInLatest": false,
        "RememberAudioSelections": true,
        "RememberSubtitleSelections": true,
        "EnableNextEpisodeAutoPlay": true,
    })
}

fn emby_user_policy_json(user: &UserRecord) -> Value {
    json!({
        "IsAdministrator": user.is_admin,
        "IsHidden": false,
        "IsHiddenRemotely": false,
        "IsDisabled": user.is_disabled,
        "BlockedTags": [],
        "EnableUserPreferenceAccess": true,
        "AccessSchedules": [],
        "BlockUnratedItems": [],
        "EnableRemoteControlOfOtherUsers": user.can_manage_server,
        "EnableSharedDeviceControl": true,
        "EnableRemoteAccess": user.can_remote_access,
        "EnableLiveTvManagement": false,
        "EnableLiveTvAccess": false,
        "EnableMediaPlayback": true,
        "EnableAudioPlaybackTranscoding": false,
        "EnableVideoPlaybackTranscoding": false,
        "EnablePlaybackRemuxing": false,
        "EnableContentDeletion": false,
        "EnableContentDeletionFromFolders": [],
        "EnableContentDownloading": user.can_download,
        "EnableSubtitleDownloading": false,
        "EnableSubtitleManagement": user.can_manage_server,
        "EnableSyncTranscoding": false,
        "EnableMediaConversion": false,
        "EnabledDevices": [],
        "EnableAllDevices": true,
        "EnabledChannels": [],
        "EnableAllChannels": false,
        "EnabledFolders": [],
        "EnableAllFolders": true,
        "InvalidLoginAttemptCount": 0,
        "EnablePublicSharing": false,
        "BlockedMediaFolders": [],
        "BlockedChannels": [],
        "RemoteClientBitrateLimit": 0,
        "AuthenticationProviderId": "Lux",
        "ExcludedSubFolders": [],
        "DisablePremiumFeatures": true,
    })
}

fn emby_login_session_json(result: &crate::auth::emby::EmbyAuthResult, server_id: &str) -> Value {
    json!({
        "Id": result.session_id,
        "ServerId": server_id,
        "UserId": result.user.id.to_string(),
        "UserName": result.user.display_name,
        "Client": result.device.client,
        "DeviceId": result.device.device_id,
        "DeviceName": result.device.device,
        "DeviceType": result.device.device,
        "ApplicationVersion": result.device.version,
        "AdditionalUsers": [],
        "PlayableMediaTypes": ["Audio", "Video"],
        "SupportedCommands": [],
        "SupportsRemoteControl": false,
        "RemoteEndPoint": "",
        "UserPrimaryImageTag": serde_json::Value::Null,
        "AppIconUrl": serde_json::Value::Null,
        "PlaylistItemId": serde_json::Value::Null,
        "PlayState": {},
        "Capabilities": {},
    })
}

#[derive(Deserialize, Default)]
struct DirectoryBrowseQuery {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    page: Option<i64>,
    #[serde(rename = "pageSize", default)]
    page_size: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LuxPeopleSearchQuery {
    q: Option<String>,
    page: Option<i64>,
    page_size: Option<i64>,
}

async fn lux_search_people(
    headers: HeaderMap,
    Query(query): Query<LuxPeopleSearchQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(raw_query) = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "人物搜索关键词不能为空",
        )
        .into_response();
    };
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let library_ids = match access
        .accessible_library_ids(AccessPrincipal::new(user.id, user.is_admin))
        .await
    {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(people) = state.people.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match people
        .search_actors(&library_ids, raw_query, offset, limit)
        .await
    {
        Ok((actors, total)) => Json(json!({
            "items": actors,
            "total": total,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(PeopleError::InvalidComponent(_)) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "人物搜索关键词无效",
        )
        .into_response(),
        Err(PeopleError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn lux_get_person_items(
    headers: HeaderMap,
    Path(person_id): Path<String>,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let (offset, limit) = match lux_page_params(&query) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let library_ids = match access
        .accessible_library_ids(AccessPrincipal::new(user.id, user.is_admin))
        .await
    {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(people) = state.people.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let person = match people.find_person(&library_ids, "Actor", &person_id).await {
        Ok(Some(person)) => person,
        Ok(None) | Err(PeopleError::InvalidComponent(_)) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "人物不存在",
            )
            .into_response();
        }
        Err(PeopleError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let filter = CatalogFilter {
        item_types: vec!["MOVIE".to_owned(), "SERIES".to_owned()],
        person_id: Some(person.lookup_id),
        sort_by: CatalogSort::PremiereDate,
        descending: true,
        ..CatalogFilter::default()
    };
    match catalog
        .list_all_items_filtered(
            AccessPrincipal::new(user.id, user.is_admin),
            &filter,
            offset,
            limit,
        )
        .await
    {
        Ok(page) => {
            match lux_catalog_page_json_for_user(database, &user.id.to_string(), &page).await {
                Ok(body) => Json(body).into_response(),
                Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

async fn lux_get_person_image(
    headers: HeaderMap,
    Path(person_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    lux_get_person_image_inner(headers, None, person_id, state).await
}

async fn lux_get_person(
    headers: HeaderMap,
    Path(person_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let library_ids = match access
        .accessible_library_ids(AccessPrincipal::new(user.id, user.is_admin))
        .await
    {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(people) = state.people.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match people.find_person(&library_ids, "Actor", &person_id).await {
        Ok(Some(mut person)) => {
            person.is_favorite = match database
                .find_user_person_favorite(&user.id.to_string(), &person.id)
                .await
            {
                Ok(is_favorite) => is_favorite,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            Json(person).into_response()
        }
        Ok(None) | Err(PeopleError::InvalidComponent(_)) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "人物不存在",
        )
        .into_response(),
        Err(PeopleError::Storage(_)) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn lux_set_person_favorite(
    headers: HeaderMap,
    Path(person_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<LuxFavoriteRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let library_ids = match access
        .accessible_library_ids(AccessPrincipal::new(user.id, user.is_admin))
        .await
    {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(people) = state.people.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let person = match people.find_person(&library_ids, "Actor", &person_id).await {
        Ok(Some(person)) => person,
        Ok(None) | Err(PeopleError::InvalidComponent(_)) => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .set_user_person_favorite(&user.id.to_string(), &person.id, request.favorite)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LuxPersonUpdateRequest {
    name: String,
    #[serde(default)]
    biography: Option<String>,
    #[serde(default)]
    birthday: Option<String>,
    #[serde(default)]
    deathday: Option<String>,
    #[serde(default)]
    known_for_department: Option<String>,
    #[serde(default)]
    place_of_birth: Option<String>,
    #[serde(default)]
    provider_ids: BTreeMap<String, String>,
    #[serde(default)]
    genres: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    production_locations: Vec<String>,
    #[serde(default)]
    premiere_date: Option<String>,
    #[serde(default)]
    production_year: Option<i32>,
    #[serde(default)]
    taglines: Vec<String>,
}

async fn lux_update_person(
    headers: HeaderMap,
    Path(person_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<LuxPersonUpdateRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if !person_update_is_bounded(&request) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "人物资料内容过长或格式无效",
        )
        .into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let library_ids = match access
        .accessible_library_ids(AccessPrincipal::new(user.id, user.is_admin))
        .await
    {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(people) = state.people.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let update = PersonMetadataUpdate {
        name: request.name.trim().to_owned(),
        biography: clean_person_text(request.biography),
        birthday: clean_person_text(request.birthday),
        deathday: clean_person_text(request.deathday),
        known_for_department: clean_person_text(request.known_for_department),
        place_of_birth: clean_person_text(request.place_of_birth),
        provider_ids: clean_person_provider_ids(request.provider_ids),
        genres: clean_person_values(request.genres),
        tags: clean_person_values(request.tags),
        production_locations: clean_person_values(request.production_locations),
        premiere_date: clean_person_text(request.premiere_date),
        production_year: request.production_year,
        taglines: clean_person_values(request.taglines),
    };
    match people
        .replace_person_metadata(&library_ids, &person_id, update)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "人物不存在",
            )
            .into_response();
        }
        Err(PeopleError::InvalidComponent(_)) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "人物资料无效",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    record_audit_event(
        &state,
        &headers,
        "PERSON_METADATA_EDITED",
        Some("person"),
        Some(&person_id),
        "{}",
    )
    .await;
    match people.find_person(&library_ids, "Actor", &person_id).await {
        Ok(Some(person)) => Json(person).into_response(),
        Ok(None) | Err(PeopleError::InvalidComponent(_)) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

fn clean_person_text(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_owned();
        (!value.is_empty()).then_some(value)
    })
}

fn clean_person_values(values: Vec<String>) -> Vec<String> {
    let mut cleaned = Vec::new();
    for value in values {
        let value = value.trim().to_owned();
        if !value.is_empty() && !cleaned.contains(&value) {
            cleaned.push(value);
        }
    }
    cleaned
}

fn clean_person_provider_ids(values: BTreeMap<String, String>) -> BTreeMap<String, String> {
    values
        .into_iter()
        .filter_map(|(provider, id)| {
            let provider = provider.trim().to_owned();
            let id = id.trim().to_owned();
            (!provider.is_empty() && !id.is_empty()).then_some((provider, id))
        })
        .collect()
}

fn person_update_is_bounded(request: &LuxPersonUpdateRequest) -> bool {
    const MAX_PERSON_TEXT_BYTES: usize = 64 * 1024;
    const MAX_PERSON_LIST_ITEMS: usize = 256;
    const MAX_PERSON_LIST_VALUE_BYTES: usize = 2048;
    let text_is_bounded =
        |value: Option<&String>| value.is_none_or(|value| value.len() <= MAX_PERSON_TEXT_BYTES);
    let values_are_bounded = |values: &[String]| {
        values.len() <= MAX_PERSON_LIST_ITEMS
            && values
                .iter()
                .all(|value| value.len() <= MAX_PERSON_LIST_VALUE_BYTES)
    };
    request.name.len() <= 128
        && text_is_bounded(request.biography.as_ref())
        && text_is_bounded(request.birthday.as_ref())
        && text_is_bounded(request.deathday.as_ref())
        && text_is_bounded(request.known_for_department.as_ref())
        && text_is_bounded(request.place_of_birth.as_ref())
        && text_is_bounded(request.premiere_date.as_ref())
        && request.provider_ids.len() <= MAX_PERSON_LIST_ITEMS
        && request
            .provider_ids
            .iter()
            .all(|(provider, id)| provider.len() <= 128 && id.len() <= MAX_PERSON_LIST_VALUE_BYTES)
        && values_are_bounded(&request.genres)
        && values_are_bounded(&request.tags)
        && values_are_bounded(&request.production_locations)
        && values_are_bounded(&request.taglines)
}

async fn lux_get_person_image_for_provider(
    headers: HeaderMap,
    Path((provider, person_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    lux_get_person_image_inner(headers, Some(provider), person_id, state).await
}

async fn lux_get_person_image_inner(
    headers: HeaderMap,
    provider: Option<String>,
    person_id: String,
    state: AppState,
) -> Response {
    if let Err(response) = require_web_user(&headers, &state).await {
        return response;
    }
    let Some(people) = state.people.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let image = match people
        .profile_image_for_provider(provider.as_deref(), &person_id)
        .await
    {
        Ok(Some(image)) => image,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(PeopleError::InvalidComponent(_)) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Ok(file) = tokio::fs::File::open(&image.path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", image.content_type)
        .header("Content-Length", image.content_length)
        .header("Cache-Control", "private, max-age=3600")
        .body(Body::from_stream(tokio_util::io::ReaderStream::new(file)))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

const MAX_LUX_CHAPTERS_PER_SOURCE: usize = 1_000;

fn lux_catalog_source_json(source: &crate::application::catalog::CatalogSource) -> Value {
    let mut chapters = source
        .chapters
        .iter()
        .filter(|chapter| chapter.start_position_ticks >= 0 && chapter.chapter_index >= 0)
        .collect::<Vec<_>>();
    chapters.sort_by(|left, right| {
        left.start_position_ticks
            .cmp(&right.start_position_ticks)
            .then_with(|| {
                lux_chapter_marker_rank(&left.marker_type)
                    .cmp(&lux_chapter_marker_rank(&right.marker_type))
            })
            .then(left.chapter_index.cmp(&right.chapter_index))
    });
    json!({
        "id": source.id,
        "sourceKind": source.source_kind,
        "container": source.container,
        "size": source.size,
        "bitrate": source.bitrate,
        "durationTicks": source.duration_ticks,
        "externalUrl": source.external_url,
        "editionName": source.edition_name,
        "qualityLabel": source.quality_label,
        "isDefault": source.is_default,
        "probeStatus": source.probe_status,
        "streams": source.streams.iter().map(|stream| json!({
            "index": stream.index,
            "type": stream.stream_type,
            "codec": stream.codec,
            "language": stream.language,
            "title": stream.title,
            "isExternal": stream.is_external,
            "isDefault": stream.is_default,
            "isForced": stream.is_forced,
            "details": &stream.details,
        })).collect::<Vec<_>>(),
        "chapters": chapters
            .into_iter()
            .take(MAX_LUX_CHAPTERS_PER_SOURCE)
            .map(lux_catalog_chapter_json)
            .collect::<Vec<_>>(),
    })
}

fn lux_chapter_marker_rank(marker_type: &str) -> u8 {
    match marker_type {
        "INTRO_START" => 0,
        "INTRO_END" => 1,
        "CREDITS_START" => 2,
        _ => 99,
    }
}

fn lux_catalog_chapter_json(chapter: &crate::application::catalog::CatalogChapter) -> Value {
    json!({
        "startPositionTicks": chapter.start_position_ticks,
        "name": chapter.name,
        "markerType": chapter.marker_type,
        "chapterIndex": chapter.chapter_index,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        CatalogSort, EmbyItemDetailWorkPlan, FilmlyImageCompatMode, MediaStrategySettings,
        MetadataCandidateFailureKind, build_cookie, catalog_filter_from_emby, emby_collection_type,
        emby_item_detail_work_plan, emby_media_source_json, emby_media_source_json_with_resolver,
        emby_media_stream_item_id, emby_media_stream_json, emby_playback_info_item_id,
        emby_single_id_lookup, emby_source_needs_strm_resolver,
        filmly_image_compat_mode_from_env_value, is_catalog_aggregation_path,
        is_emby_legacy_strm_path, is_emby_media_stream_segment, is_emby_playback_callback_path,
        is_emby_subtitle_path, is_emby_video_path, is_filmly_user_agent,
        is_registered_emby_video_path, lux_catalog_source_json, metadata_candidate_failure_kind,
        normalize_strm_http_location, playback_client_label, playback_identifier_prefix,
        record_activity_event, safe_trace_path, secure_cookie_for_request, validate_media_strategy,
    };
    use crate::application::admin_events::{AdminEventHub, AdminEventScope};
    use crate::application::candidates::MetadataCandidateError;
    use crate::application::catalog::{CatalogChapter, CatalogSource, CatalogStream};
    use crate::application::scraper::ScraperError;
    use crate::application::setup::SetupService;
    use crate::config::Config;
    use crate::library::LibraryKind;
    use crate::network::RemoteAccessPolicy;
    use crate::storage::{Database, StorageError};
    use axum::http::{HeaderMap, HeaderValue, Uri};
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn strm_http_location_percent_encodes_non_ascii_url_components() {
        let raw = "https://media.example.test/path/剧集?title=第1集&token=secret";
        assert!(super::is_http_strm_target(raw));
        let location = normalize_strm_http_location(raw)
            .and_then(|value| value.to_str().ok().map(str::to_owned));

        assert_eq!(
            location.as_deref(),
            Some(
                "https://media.example.test/path/%E5%89%A7%E9%9B%86?title=%E7%AC%AC1%E9%9B%86&token=secret"
            )
        );
    }

    #[test]
    fn emby_collection_type_uses_standard_types_for_each_library_kind() {
        assert_eq!(emby_collection_type(LibraryKind::Movie), Some("movies"));
        assert_eq!(emby_collection_type(LibraryKind::Series), Some("tvshows"));
        assert_eq!(emby_collection_type(LibraryKind::Mixed), None);
    }

    #[test]
    fn media_strategy_accepts_frontend_plugin_ids_and_no_image_language_preference() {
        let settings = MediaStrategySettings {
            image_language: String::new(),
            scraper_id: Some("org.lux.tmdb".to_owned()),
            ..MediaStrategySettings::default()
        };

        assert!(validate_media_strategy(&settings));
    }

    #[test]
    fn media_strategy_rejects_unsafe_plugin_ids() {
        let settings = MediaStrategySettings {
            scraper_id: Some("../org.lux.tmdb".to_owned()),
            ..MediaStrategySettings::default()
        };

        assert!(!validate_media_strategy(&settings));
    }

    #[test]
    fn catalog_concurrency_guard_excludes_streaming_paths() {
        for path in [
            "/api/v1/home",
            "/api/v1/libraries/library-1/items",
            "/api/v1/collections/collection-1",
            "/Users/user-1/Items/Resume",
            "/emby/Shows/show-1/Episodes",
            "/Items/collection-1/Children",
        ] {
            assert!(is_catalog_aggregation_path(path), "{path}");
        }

        for path in [
            "/api/v1/items/item-1/stream",
            "/api/v1/items/item-1/download",
            "/Videos/item-1/stream.mkv",
            "/emby/Videos/item-1/source-1/stream",
            "/Items/item-1/Download",
            "/Items/item-1/PlaybackInfo",
        ] {
            assert!(!is_catalog_aggregation_path(path), "{path}");
        }
    }

    #[test]
    fn metadata_candidate_errors_have_fixed_diagnostic_categories() {
        let cases = [
            (
                MetadataCandidateError::ItemNotFound,
                MetadataCandidateFailureKind::ItemNotFound,
                "ITEM_NOT_FOUND",
            ),
            (
                MetadataCandidateError::InvalidSearch,
                MetadataCandidateFailureKind::InvalidSearch,
                "INVALID_SEARCH",
            ),
            (
                MetadataCandidateError::InvalidCandidateJson("secret detail".to_owned()),
                MetadataCandidateFailureKind::InvalidCandidateJson,
                "INVALID_CANDIDATE_JSON",
            ),
            (
                MetadataCandidateError::Scraper(ScraperError::Provider("secret detail".to_owned())),
                MetadataCandidateFailureKind::ScraperUnavailable,
                "SCRAPER_UNAVAILABLE",
            ),
            (
                MetadataCandidateError::Storage(StorageError::LastManager),
                MetadataCandidateFailureKind::StorageUnavailable,
                "STORAGE_UNAVAILABLE",
            ),
        ];

        for (error, expected_kind, expected_label) in cases {
            let kind = metadata_candidate_failure_kind(&error);
            assert_eq!(kind, expected_kind);
            assert_eq!(kind.as_str(), expected_label);
            assert!(!kind.as_str().contains("secret detail"));
        }
    }

    #[test]
    fn emby_ids_filter_preserves_item_and_media_source_candidates() {
        let query = super::EmbyItemsQuery {
            ids: Some("item-1, source-2".to_owned()),
            ..super::EmbyItemsQuery::default()
        };

        let filter = catalog_filter_from_emby(&query);

        assert_eq!(
            filter.item_ids,
            Some(vec!["item-1".to_owned(), "source-2".to_owned()])
        );
        assert_eq!(
            filter.media_source_ids,
            Some(vec!["item-1".to_owned(), "source-2".to_owned()])
        );
    }

    #[test]
    fn single_ids_lookup_is_limited_to_unfiltered_first_pages() {
        let query = super::EmbyItemsQuery {
            ids: Some("item-1".to_owned()),
            ..super::EmbyItemsQuery::default()
        };
        assert_eq!(emby_single_id_lookup(&query), Some("item-1"));

        let mut query = query;
        query.ids = Some("item-1,source-2".to_owned());
        assert_eq!(emby_single_id_lookup(&query), None);

        query.ids = Some("item-1".to_owned());
        query.parent_id = Some("library-1".to_owned());
        assert_eq!(emby_single_id_lookup(&query), None);

        query.parent_id = None;
        query.start_index = Some(1);
        assert_eq!(emby_single_id_lookup(&query), None);

        query.start_index = Some(0);
        query.limit = Some(0);
        assert_eq!(emby_single_id_lookup(&query), None);
    }

    #[test]
    fn emby_combined_date_created_sort_uses_date_created_primary_sort() {
        let query = super::EmbyItemsQuery {
            sort_by: Some("DateCreated,SortName".to_owned()),
            sort_order: Some("Descending".to_owned()),
            ..super::EmbyItemsQuery::default()
        };

        let filter = catalog_filter_from_emby(&query);

        assert_eq!(filter.sort_by, CatalogSort::DateCreated);
        assert!(filter.descending);
    }

    #[test]
    fn direct_http_cookie_is_not_marked_secure() {
        let headers = HeaderMap::new();

        assert!(!secure_cookie_for_request(&headers, &RemoteAccessPolicy));
        let cookie = build_cookie("lux_session", "token", true, None, false)
            .expect("cookie value should be valid");
        assert!(
            !cookie
                .to_str()
                .expect("cookie should be valid")
                .contains("Secure")
        );
    }

    #[test]
    fn trusted_https_forwarding_marks_cookie_secure() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        let policy = RemoteAccessPolicy;

        assert!(secure_cookie_for_request(&headers, &policy));
        let cookie = build_cookie("lux_session", "token", true, None, true)
            .expect("cookie value should be valid");
        assert!(
            cookie
                .to_str()
                .expect("cookie should be valid")
                .contains("Secure")
        );
    }

    #[test]
    fn forwarded_https_marks_cookie_secure_without_proxy_allowlist() {
        let mut headers = HeaderMap::new();
        headers.insert("x-lux-peer-ip", HeaderValue::from_static("10.0.0.2"));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));

        assert!(secure_cookie_for_request(&headers, &RemoteAccessPolicy));
    }

    #[test]
    fn trace_path_excludes_query_credentials() {
        let uri: Uri = "/System/Info?api_key=do-not-log".parse().unwrap();

        assert_eq!(safe_trace_path(&uri), "/System/Info");
    }

    #[test]
    fn playback_callback_trace_matches_root_and_emby_paths_only() {
        assert!(is_emby_playback_callback_path("/Sessions/Playing"));
        assert!(is_emby_playback_callback_path(
            "/emby/Sessions/Playing/Progress"
        ));
        assert!(!is_emby_playback_callback_path("/Sessions"));
        assert!(!is_emby_playback_callback_path(
            "/Sessions/Playing?api_key=secret"
        ));
    }

    #[test]
    fn playback_info_trace_extracts_only_single_path_segments() {
        assert_eq!(
            emby_playback_info_item_id("/Items/item-123/PlaybackInfo"),
            Some("item-123")
        );
        assert_eq!(
            emby_playback_info_item_id("/emby/Items/item-123/PlaybackInfo"),
            Some("item-123")
        );
        assert_eq!(
            emby_playback_info_item_id("/Items/item-123/PlaybackInfo?api_key=secret"),
            None
        );
        assert_eq!(
            emby_playback_info_item_id("/Items/item-123/PlaybackInfo/extra"),
            None
        );
    }

    #[test]
    fn media_stream_trace_matches_only_direct_stream_routes() {
        assert_eq!(
            emby_media_stream_item_id("/Videos/item-123/stream"),
            Some("item-123")
        );
        assert_eq!(
            emby_media_stream_item_id("/emby/Videos/item-123/source-456/stream.mkv"),
            Some("item-123")
        );
        assert!(is_emby_media_stream_segment("stream.mp4"));
        assert!(!is_emby_media_stream_segment("Subtitles"));
        assert_eq!(
            emby_media_stream_item_id("/Videos/item-123/source-456/Subtitles/0/Stream"),
            None
        );
        assert_eq!(
            emby_media_stream_item_id("/Videos/item-123/stream?api_key=secret"),
            None
        );
    }

    #[test]
    fn unmatched_emby_video_paths_are_not_registered_routes() {
        assert!(is_emby_video_path("/Videos/item-123/original.strm"));
        assert!(is_emby_video_path("/emby/videos/item-123/original.strm"));
        assert!(is_emby_legacy_strm_path("/Videos/item-123/original.strm"));
        assert!(is_registered_emby_video_path(
            "/emby/videos/item-123/original.strm"
        ));
        assert!(!is_registered_emby_video_path(
            "/Videos/item-123/unknown.strm"
        ));
        assert!(is_registered_emby_video_path(
            "/Videos/item-123/source-456/stream.mkv"
        ));
        assert!(is_registered_emby_video_path(
            "/emby/videos/item-123/stream.mkv"
        ));
        assert!(is_emby_subtitle_path(
            "/emby/Videos/item-123/source-456/Subtitles/0/Stream"
        ));
        assert!(!is_emby_subtitle_path(
            "/emby/videos/item-123/source-456/Subtitles/0/Stream"
        ));
    }

    #[test]
    fn playback_log_fields_are_bounded_and_allowlisted() {
        assert_eq!(playback_identifier_prefix("12345678-abcdef"), "12345678");
        assert_eq!(playback_client_label(Some("VidHub")), "vidhub");
        assert_eq!(playback_client_label(Some("unknown-client")), "other");
        assert_eq!(playback_client_label(None), "unknown");
    }

    #[tokio::test]
    async fn activity_events_publish_dashboard_invalidations() {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be available");
        let config = Config {
            http_addr: "127.0.0.1:8097"
                .parse()
                .expect("test address should be valid"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config)
            .await
            .expect("test database should connect");
        let setup = SetupService::new(database.clone()).expect("setup service should initialize");
        let user = setup
            .complete("admin", "Admin", "correct password")
            .await
            .expect("initial admin should be created");
        let hub = AdminEventHub::new();
        let mut receiver = hub.subscribe();

        record_activity_event(
            Some(&database),
            &hub,
            &user.id.to_string(),
            "PLAYBACK_STARTED",
            Some("item-1"),
            json!({ "client": "Lux" }),
        )
        .await;

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .expect("dashboard invalidation should be published")
                .expect("event stream should remain open"),
            AdminEventScope::Dashboard
        );
    }

    #[test]
    fn emby_media_source_includes_path_and_detailed_stream_fields() {
        let source = CatalogSource {
            id: "source-1".to_owned(),
            source_kind: "STRM_URL".to_owned(),
            container: Some("mkv".to_owned()),
            size: Some(1_234_567),
            external_url: Some("https://example.invalid/media.mkv".to_owned()),
            edition_name: None,
            quality_label: Some("1080p".to_owned()),
            bitrate: Some(800_000),
            duration_ticks: Some(90_000_000),
            is_default: true,
            probe_status: "READY".to_owned(),
            streams: vec![CatalogStream {
                index: 0,
                stream_type: "VIDEO".to_owned(),
                codec: Some("h264".to_owned()),
                language: None,
                title: Some("1080p H264".to_owned()),
                is_external: false,
                is_default: true,
                is_forced: false,
                details: BTreeMap::from([
                    ("Width".to_owned(), serde_json::json!(1920)),
                    ("Height".to_owned(), serde_json::json!(1080)),
                    ("Profile".to_owned(), serde_json::json!("High")),
                ]),
            }],
            chapters: Vec::new(),
        };

        let body = emby_media_source_json("item-1", &source, true);
        assert_eq!(body["Path"], "https://example.invalid/media.mkv");
        assert_eq!(body["Size"], 1_234_567);
        assert_eq!(body["SupportsDirectPlay"], true);
        assert_eq!(body["SupportsDirectStream"], true);
        assert_eq!(
            body["DirectStreamUrl"],
            "/Videos/item-1/stream.mkv?MediaSourceId=source-1"
        );
        assert_eq!(body["DefaultAudioStreamIndex"], -1);
        assert!(body.get("Chapters").is_none());
        assert_eq!(body["MediaStreams"][0]["Width"], 1920);
        assert_eq!(body["MediaStreams"][0]["Height"], 1080);
        assert_eq!(body["MediaStreams"][0]["Profile"], "High");
    }

    #[test]
    fn emby_media_stream_json_uses_safe_text_defaults_for_missing_probe_metadata() {
        let stream = CatalogStream {
            index: 0,
            stream_type: "VIDEO".to_owned(),
            codec: None,
            language: None,
            title: None,
            is_external: false,
            is_default: true,
            is_forced: false,
            details: BTreeMap::new(),
        };

        let body = emby_media_stream_json(&stream);

        assert_eq!(body["Language"], "und");
        assert_eq!(body["DisplayTitle"], "Video");
    }

    #[test]
    fn filmly_user_agent_detection_recognizes_known_clients() {
        assert!(is_filmly_user_agent("Filmly/2.12.3-423"));
        assert!(is_filmly_user_agent("网易爆米花/2.12.3-423"));
        assert!(!is_filmly_user_agent("VidHub/1.0"));
    }

    #[test]
    fn filmly_image_compat_mode_defaults_to_compat_and_accepts_generic_ab_value() {
        assert_eq!(
            filmly_image_compat_mode_from_env_value(None),
            FilmlyImageCompatMode::Compat
        );
        assert_eq!(
            filmly_image_compat_mode_from_env_value(Some("generic")),
            FilmlyImageCompatMode::Generic
        );
        assert_eq!(
            filmly_image_compat_mode_from_env_value(Some("compat")),
            FilmlyImageCompatMode::Compat
        );
        assert_eq!(
            filmly_image_compat_mode_from_env_value(Some("unexpected")),
            FilmlyImageCompatMode::Compat
        );
    }

    #[test]
    fn emby_media_streams_use_numeric_and_boolean_json_types() {
        let stream = CatalogStream {
            index: 0,
            stream_type: "VIDEO".to_owned(),
            codec: Some("h264".to_owned()),
            language: None,
            title: Some("1080p H264".to_owned()),
            is_external: false,
            is_default: true,
            is_forced: false,
            details: BTreeMap::from([
                ("Width".to_owned(), serde_json::json!("1920")),
                ("BitDepth".to_owned(), serde_json::json!("8")),
                ("AverageFrameRate".to_owned(), serde_json::json!("24/1")),
                ("RealFrameRate".to_owned(), serde_json::json!("24000/1001")),
                ("IsInterlaced".to_owned(), serde_json::json!("false")),
                ("Profile".to_owned(), serde_json::json!("High")),
            ]),
        };

        let body = emby_media_stream_json(&stream);

        assert_eq!(body["Width"], 1920);
        assert_eq!(body["BitDepth"], 8);
        assert_eq!(body["AverageFrameRate"], 24);
        assert!(
            (body["RealFrameRate"]
                .as_f64()
                .expect("frame rate should be numeric")
                - (24_000.0 / 1_001.0))
                .abs()
                < 0.000_001
        );
        assert_eq!(body["IsInterlaced"], false);
        assert_eq!(body["Profile"], "High");
    }

    #[test]
    fn emby_media_source_chapters_are_only_included_when_requested() {
        let source = CatalogSource {
            id: "source-1".to_owned(),
            source_kind: "LOCAL_FILE".to_owned(),
            container: Some("mkv".to_owned()),
            size: None,
            external_url: None,
            edition_name: None,
            quality_label: None,
            bitrate: None,
            duration_ticks: Some(100_000_000),
            is_default: true,
            probe_status: "READY".to_owned(),
            streams: Vec::new(),
            chapters: vec![CatalogChapter {
                start_position_ticks: 10_000_000,
                name: None,
                marker_type: "INTRO_START".to_owned(),
                chapter_index: 0,
            }],
        };

        let without_chapters = emby_media_source_json("item-1", &source, false);
        assert!(without_chapters.get("Chapters").is_none());
        let with_chapters = emby_media_source_json_with_resolver("item-1", &source, false, false);
        assert_eq!(with_chapters["Chapters"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn lux_media_source_chapters_are_sorted_bounded_and_source_local() {
        let mut chapters = (0_i64..=1_000)
            .rev()
            .map(|index| CatalogChapter {
                start_position_ticks: index,
                name: Some(format!("chapter-{index}")),
                marker_type: "CHAPTER".to_owned(),
                chapter_index: index,
            })
            .collect::<Vec<_>>();
        chapters.push(CatalogChapter {
            start_position_ticks: -1,
            name: Some("invalid".to_owned()),
            marker_type: "CHAPTER".to_owned(),
            chapter_index: 0,
        });
        let source = CatalogSource {
            id: "source-1".to_owned(),
            source_kind: "LOCAL_FILE".to_owned(),
            container: None,
            size: None,
            external_url: None,
            edition_name: None,
            quality_label: None,
            bitrate: None,
            duration_ticks: None,
            is_default: true,
            probe_status: "READY".to_owned(),
            streams: Vec::new(),
            chapters,
        };

        let body = lux_catalog_source_json(&source);
        let chapters = body["chapters"]
            .as_array()
            .expect("chapters should be an array");
        assert_eq!(chapters.len(), super::MAX_LUX_CHAPTERS_PER_SOURCE);
        assert_eq!(chapters[0]["startPositionTicks"], 0);
        assert_eq!(chapters[999]["startPositionTicks"], 999);
        assert_eq!(chapters[0]["markerType"], "CHAPTER");
        assert_eq!(chapters[0]["chapterIndex"], 0);
        assert!(chapters[0].get("sourceId").is_none());
    }

    #[test]
    fn local_path_targets_use_a_protected_lux_stream_entrypoint() {
        let source = CatalogSource {
            id: "source-1".to_owned(),
            source_kind: "STRM_URL".to_owned(),
            container: Some("mkv".to_owned()),
            size: None,
            external_url: Some("/cloud/library/movie.mp4".to_owned()),
            edition_name: None,
            quality_label: None,
            bitrate: None,
            duration_ticks: None,
            is_default: true,
            probe_status: "PENDING".to_owned(),
            streams: Vec::new(),
            chapters: Vec::new(),
        };

        let body = emby_media_source_json_with_resolver("item-1", &source, false, true);

        assert_eq!(body["Protocol"], "File");
        assert_eq!(body["IsRemote"], false);
        assert_eq!(body["SupportsDirectPlay"], true);
        assert_eq!(
            body["DirectStreamUrl"],
            "/Videos/item-1/stream.mkv?MediaSourceId=source-1"
        );
        assert_eq!(body["Path"], "/cloud/library/movie.mp4");
    }

    #[test]
    fn playback_info_only_needs_a_strm_resolver_for_smb_or_ftp_targets() {
        let mut source = CatalogSource {
            id: "source-1".to_owned(),
            source_kind: "STRM_URL".to_owned(),
            container: Some("mkv".to_owned()),
            size: None,
            external_url: Some("/cloud/library/movie.mkv".to_owned()),
            edition_name: None,
            quality_label: None,
            bitrate: None,
            duration_ticks: None,
            is_default: true,
            probe_status: "PENDING".to_owned(),
            streams: Vec::new(),
            chapters: Vec::new(),
        };

        assert!(!emby_source_needs_strm_resolver(&source));
        source.external_url = Some("https://media.example.test/movie.mkv".to_owned());
        assert!(!emby_source_needs_strm_resolver(&source));
        source.external_url = Some("smb://nas/media/movie.mkv".to_owned());
        assert!(emby_source_needs_strm_resolver(&source));
        source.external_url = Some("ftp://nas/media/movie.mkv".to_owned());
        assert!(emby_source_needs_strm_resolver(&source));
    }

    #[test]
    fn filtered_emby_item_details_skip_unrequested_enrichment() {
        assert_eq!(
            emby_item_detail_work_plan(Some("Path,MediaSources")),
            EmbyItemDetailWorkPlan {
                populate_image_tags: false,
                read_nfo: false,
                read_primary_image_aspect_ratio: false,
                read_people: false,
            }
        );
        assert_eq!(
            emby_item_detail_work_plan(Some("MediaSources")),
            EmbyItemDetailWorkPlan {
                populate_image_tags: false,
                read_nfo: false,
                read_primary_image_aspect_ratio: false,
                read_people: false,
            }
        );
        assert_eq!(
            emby_item_detail_work_plan(None),
            EmbyItemDetailWorkPlan {
                populate_image_tags: true,
                read_nfo: true,
                read_primary_image_aspect_ratio: true,
                read_people: true,
            }
        );
        assert_eq!(
            emby_item_detail_work_plan(Some("ShareLevel")),
            EmbyItemDetailWorkPlan {
                populate_image_tags: true,
                read_nfo: true,
                read_primary_image_aspect_ratio: true,
                read_people: true,
            }
        );
    }

    #[test]
    fn lux_media_source_keeps_detailed_stream_fields_for_web_clients() {
        let source = CatalogSource {
            id: "source-1".to_owned(),
            source_kind: "LOCAL_FILE".to_owned(),
            container: Some("mkv".to_owned()),
            size: Some(1_234_567),
            external_url: None,
            edition_name: None,
            quality_label: None,
            bitrate: Some(800_000),
            duration_ticks: Some(90_000_000),
            is_default: true,
            probe_status: "READY".to_owned(),
            streams: vec![CatalogStream {
                index: 0,
                stream_type: "VIDEO".to_owned(),
                codec: Some("h264".to_owned()),
                language: None,
                title: Some("1080p H264".to_owned()),
                is_external: false,
                is_default: true,
                is_forced: false,
                details: BTreeMap::from([("Width".to_owned(), serde_json::json!(1920))]),
            }],
            chapters: Vec::new(),
        };

        let body = lux_catalog_source_json(&source);
        assert_eq!(body["streams"][0]["details"]["Width"], 1920);
    }

    #[test]
    fn hls_manifest_rewrites_init_and_segment_urls() {
        let manifest = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4.0,\nsegment_000000.m4s\n";
        let rewritten =
            super::rewrite_hls_manifest(manifest, |asset| Some(format!("/signed/{asset}")))
                .expect("manifest should rewrite");
        assert!(rewritten.contains("URI=\"/signed/init.mp4\""));
        assert!(rewritten.contains("/signed/segment_000000.m4s"));
    }

    #[test]
    fn hls_manifest_rejects_unexpected_media_paths() {
        let manifest = "#EXTM3U\n#EXTINF:4.0,\n../outside.m4s\n";
        assert!(super::rewrite_hls_manifest(manifest, |_| None).is_none());
    }
}
