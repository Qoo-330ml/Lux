pub mod lux;

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
use serde::{Deserialize, Serialize};
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

use crate::{
    COMMIT, VERSION,
    application::danmaku::{DanmakuService, DanmakuServiceError, validate_provider_base_url},
    application::database_setup::{DatabaseSetupError, DatabaseSetupService},
    application::deletion::{MediaDeleteError, MediaDeleteService},
    application::downloads::{DownloadArtifact, DownloadError, DownloadService},
    application::home::{HomeError, HomeService},
    application::playback::{ByteRange, RangeError, parse_single_range},
    application::probe::{FfprobeRunner, MediaProbeService},
    application::setup::{SetupError, SetupService},
    application::{
        access::{AccessPrincipal, MediaAccessService},
        admin_events::{AdminEventHub, AdminEventScope},
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
        people::{PeopleError, PeopleService},
        plugins::{PluginPage, PluginService, PluginServiceError},
        reidentify::{MetadataReidentifyError, MetadataReidentifyService},
        scanner::{BACKGROUND_SCAN_BATCH_SIZE, ScanJob, ScanJobError, ScanJobService},
        scheduled_tasks::ScheduledTaskService,
        scraper::ScraperResolver,
        settings::{
            read_danmaku_provider_url_async, read_network_proxy_url_async,
            write_danmaku_provider_url, write_network_proxy_url,
        },
        strm_probe::{StrmProbeError, StrmProbeService},
        strm_target::{StrmTargetKind, classify_strm_target},
        thumbnails::ThumbnailService,
        tmdb::{TmdbClient, TmdbError},
        tmdb_plugin::{TmdbPluginClient, TmdbProvider},
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
    library::{LibraryKind, LibraryRecord, LibraryRootRecord},
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
        PersonSort, StorageError, StoredPlaybackSession,
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
    process::Command,
};

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
    scraper_resolver: Option<ScraperResolver>,
    tmdb: Option<TmdbProvider>,
    collections: Option<CollectionService>,
    people: Option<PeopleService>,
    local_nfo: Option<LocalNfoMetadataStore>,
    user_avatars: Option<UserAvatarService>,
    ip_location: Option<IpLocationService>,
    admin_events: AdminEventHub,
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
        let access = MediaAccessService::new(database.clone());
        let libraries = LibraryService::new(database.clone());
        let catalog = CatalogService::new(database.clone(), access.clone());
        let home = HomeService::new(catalog.clone(), libraries.clone(), access.clone());
        let image_writes = ImageWriteService::new_with_proxy_and_config_dir(
            database.clone(),
            config.config_dir.clone(),
            network_proxy_url.clone(),
        )
        .ok();
        let library_covers = Some(
            LibraryCoverService::new(database.clone(), config.config_dir.join("library-covers"))
                .with_metadata_directory(config.config_dir.join("metadata")),
        );
        let metadata_selection = image_writes.clone().map(|images| {
            MetadataSelectionService::with_config_dir(database.clone(), images, config_dir.clone())
        });
        let plugins = PluginService::new_with_proxy(
            database.clone(),
            config_dir.clone(),
            network_proxy_url.clone(),
        );
        let tmdb = TmdbProvider::Plugin(TmdbPluginClient::new(plugins.clone()));
        let scraper_resolver = ScraperResolver::new(database.clone(), plugins.clone());
        let collections = Some(
            CollectionService::with_resolver(
                database.clone(),
                tmdb.clone(),
                scraper_resolver.clone(),
            )
            .with_config_dir(config.config_dir.clone()),
        );
        let metadata_reidentify = Some(
            MetadataReidentifyService::with_resolver_and_selection(
                database.clone(),
                tmdb.clone(),
                scraper_resolver.clone(),
                metadata_selection.clone(),
            )
            .with_admin_events(admin_events.clone())
            .with_resource_metrics(resources.clone()),
        );
        let image_candidates = Some(ImageCandidateService::with_resolver(
            database.clone(),
            tmdb.clone(),
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
        let probe = Some(MediaProbeService::new(
            database.clone(),
            FfprobeRunner::default(),
        ));
        let thumbnails = Some(ThumbnailService::new(database.clone()));
        let scan_jobs = {
            let service = ScanJobService::new(database.clone())
                .with_admin_events(admin_events.clone())
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
        );
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
            metadata_writes: Some(MetadataWriteService::new(database.clone())),
            downloads: DownloadService::new_with_proxy(database.clone(), network_proxy_url.clone())
                .ok(),
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
                DanmakuService::new(
                    database.clone(),
                    config_dir.clone(),
                    network_proxy_url.clone(),
                )
                .with_resource_metrics(resources.clone()),
            ),
            plugins: Some(plugins.clone()),
            scraper_resolver: Some(scraper_resolver),
            tmdb: Some(tmdb),
            collections,
            people: Some(people),
            local_nfo: Some(local_nfo),
            user_avatars,
            ip_location: Some(IpLocationService::new(plugins.clone())),
            admin_events,
            resources,
            remote_access: RemoteAccessPolicy,
            login_rate_limiter: LoginRateLimiter::default(),
        }
    }

    pub fn with_tmdb_client(mut self, tmdb: TmdbClient) -> Self {
        let Some(database) = self.database.clone() else {
            return self;
        };
        let tmdb = TmdbProvider::from(tmdb);
        self.tmdb = Some(tmdb.clone());
        if let Some(resolver) = self.scraper_resolver.clone() {
            let mut collections =
                CollectionService::with_resolver(database.clone(), tmdb.clone(), resolver.clone());
            if let Some(config_dir) = self.config_dir.clone() {
                collections = collections.with_config_dir(config_dir);
            }
            self.collections = Some(collections);
            self.metadata_reidentify = Some(
                MetadataReidentifyService::with_resolver_and_selection(
                    database.clone(),
                    tmdb.clone(),
                    resolver.clone(),
                    self.metadata_selection.clone(),
                )
                .with_admin_events(self.admin_events.clone())
                .with_resource_metrics(self.resources.clone()),
            );
            self.image_candidates = Some(ImageCandidateService::with_resolver(
                database, tmdb, resolver,
            ));
        } else {
            let mut collections = CollectionService::new(database.clone(), tmdb.clone());
            if let Some(config_dir) = self.config_dir.clone() {
                collections = collections.with_config_dir(config_dir);
            }
            self.collections = Some(collections);
            self.metadata_reidentify = Some(
                MetadataReidentifyService::with_selection(
                    database.clone(),
                    tmdb.clone(),
                    self.metadata_selection.clone(),
                )
                .with_admin_events(self.admin_events.clone())
                .with_resource_metrics(self.resources.clone()),
            );
            self.image_candidates = Some(ImageCandidateService::new(database, tmdb));
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
        tokio::spawn(async move {
            match service.rebuild_person_credit_index().await {
                Ok(rebuilt_items) => {
                    tracing::info!(rebuilt_items, "person credit index rebuild completed");
                }
                Err(error) => {
                    tracing::error!(%error, "person credit index rebuild failed");
                }
            }
        });
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
        LibraryWatchService::with_scan_jobs_and_metadata(
            database,
            scan_jobs,
            self.metadata_reidentify.clone(),
        )
        .spawn();
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

    pub fn require_database_selection(mut self) -> Self {
        self.database_selection_required = true;
        self
    }
}

pub fn app() -> Router {
    app_with_state(AppState::default())
}

pub fn app_with_state(state: AppState) -> Router {
    let web_root = web_root();
    let resources = state.resources.clone();
    let catalog_request_slots =
        Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT_CATALOG_REQUESTS));
    let catalog_workers = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CATALOG_REQUESTS));
    Router::new()
        .route("/logo.svg", get(web_logo))
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/api/v1/version", get(version))
        .route("/api/v1/setup/status", get(setup_status))
        .route("/api/v1/setup/database", get(setup_database_status))
        .route("/api/v1/setup/database/test", post(setup_database_test))
        .route("/api/v1/setup/database/select", post(setup_database_select))
        .route("/api/v1/setup/complete", post(setup_complete))
        .route("/api/v1/auth/login", post(auth_login))
        .route("/api/v1/auth/logout", post(auth_logout))
        .route("/api/v1/auth/me", get(auth_me))
        .route(
            "/api/v1/auth/settings",
            get(auth_settings).patch(auth_update_settings),
        )
        .route(
            "/api/v1/auth/avatar",
            get(auth_avatar)
                .put(auth_update_avatar)
                .layer(DefaultBodyLimit::max(MAX_USER_AVATAR_BYTES as usize)),
        )
        .route("/api/v1/auth/sessions", get(auth_sessions))
        .route(
            "/api/v1/auth/sessions/{session_id}",
            delete(auth_revoke_session),
        )
        .route(
            "/api/v1/admin/libraries",
            get(admin_list_libraries).post(admin_create_library),
        )
        .route("/api/v1/admin/directories", get(admin_list_directories))
        .route(
            "/api/v1/admin/libraries/{library_id}",
            patch(admin_update_library).delete(admin_delete_library),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/cover",
            put(admin_update_library_cover)
                .layer(DefaultBodyLimit::max(MAX_LIBRARY_COVER_BYTES as usize)),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/cover/auto",
            post(admin_run_auto_library_cover),
        )
        .route("/api/v1/admin/plugins", get(admin_list_plugins))
        .route(
            "/api/v1/admin/notification-providers",
            get(admin_list_notification_providers),
        )
        .route(
            "/api/v1/admin/chapter-sources",
            get(admin_list_chapter_sources),
        )
        .route(
            "/api/v1/admin/plugins/installed",
            get(admin_list_installed_plugins),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/install",
            post(admin_install_plugin),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}",
            delete(admin_uninstall_plugin),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/enabled",
            patch(admin_update_plugin_enabled),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/config",
            put(admin_update_plugin_config),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/run",
            post(admin_run_plugin),
        )
        .route(
            "/api/v1/admin/plugin-store",
            get(admin_plugin_store).put(admin_update_plugin_store),
        )
        .route(
            "/api/v1/admin/users",
            get(admin_list_users).post(admin_create_user),
        )
        .route(
            "/api/v1/admin/users/{user_id}",
            patch(admin_update_user).delete(admin_disable_user),
        )
        .route(
            "/api/v1/admin/users/{user_id}/libraries",
            get(admin_list_user_library_access),
        )
        .route(
            "/api/v1/admin/metadata/pending",
            get(admin_list_pending_metadata),
        )
        .route(
            "/api/v1/admin/metadata/reidentify",
            get(admin_list_metadata_reidentify).post(admin_start_metadata_reidentify),
        )
        .route(
            "/api/v1/admin/metadata/confirm",
            post(admin_confirm_metadata),
        )
        .route(
            "/api/v1/admin/metadata/reidentify/{job_id}",
            get(admin_get_metadata_reidentify).post(admin_retry_metadata_reidentify),
        )
        .route(
            "/api/v1/admin/metadata/reidentify/{job_id}/cancel",
            post(admin_cancel_metadata_reidentify),
        )
        .route(
            "/api/v1/admin/items/{item_id}/identify/candidates",
            get(admin_list_item_candidates).post(admin_search_item_candidates),
        )
        .route(
            "/api/v1/admin/items/{item_id}/identify/candidates/{candidate_id}/select",
            post(admin_select_candidate),
        )
        .route(
            "/api/v1/admin/items/{item_id}/images",
            get(admin_list_item_images),
        )
        .route(
            "/api/v1/admin/items/{item_id}/images/{image_id}",
            delete(admin_delete_item_image),
        )
        .route(
            "/api/v1/admin/items/{item_id}/scan",
            post(admin_start_item_scan),
        )
        .route(
            "/api/v1/admin/items/{item_id}/metadata/refresh",
            post(admin_start_item_metadata_refresh),
        )
        .route(
            "/api/v1/admin/items/{item_id}/subtitles/{stream_index}",
            patch(admin_update_item_subtitle),
        )
        .route("/api/v1/admin/items/{item_id}", delete(admin_delete_item))
        .route(
            "/api/v1/admin/items/{item_id}/collection/refresh",
            post(admin_refresh_collection),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/roots",
            post(admin_add_library_root),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/roots/{root_id}",
            delete(admin_delete_library_root),
        )
        .route(
            "/api/v1/admin/users/{user_id}/libraries/{library_id}",
            patch(admin_set_library_access),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/scan",
            post(admin_start_scan),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/reidentify",
            post(admin_start_library_reidentify),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/metadata/refresh",
            post(admin_start_library_metadata_refresh),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/reconcile",
            post(admin_start_scan),
        )
        .route(
            "/api/v1/admin/jobs/{job_id}/cancel",
            post(admin_cancel_scan),
        )
        .route("/api/v1/admin/jobs/{job_id}/retry", post(admin_retry_scan))
        .route("/api/v1/admin/jobs/{job_id}", get(admin_get_job))
        .route(
            "/api/v1/admin/strm-probe-jobs",
            get(admin_list_strm_probe_jobs).post(admin_start_strm_probe),
        )
        .route(
            "/api/v1/admin/strm-probe-jobs/{job_id}",
            get(admin_get_strm_probe_job),
        )
        .route(
            "/api/v1/admin/strm-probe-jobs/{job_id}/cancel",
            post(admin_cancel_strm_probe),
        )
        .route(
            "/api/v1/admin/strm-probe-jobs/{job_id}/retry",
            post(admin_retry_strm_probe),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/chapter-detection",
            post(admin_start_chapter_detection),
        )
        .route(
            "/api/v1/admin/chapter-detection-jobs",
            get(admin_list_chapter_detection_jobs),
        )
        .route(
            "/api/v1/admin/chapter-detection-jobs/{job_id}",
            get(admin_get_chapter_detection_job),
        )
        .route(
            "/api/v1/admin/chapter-detection-jobs/{job_id}/cancel",
            post(admin_cancel_chapter_detection),
        )
        .route(
            "/api/v1/admin/chapter-detection-jobs/{job_id}/retry",
            post(admin_retry_chapter_detection),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/danmaku/match",
            post(admin_start_danmaku_match),
        )
        .route(
            "/api/v1/admin/danmaku/match-jobs",
            get(admin_list_danmaku_match_jobs),
        )
        .route(
            "/api/v1/admin/danmaku/match-jobs/{job_id}",
            get(admin_get_danmaku_match_job),
        )
        .route(
            "/api/v1/admin/danmaku/match-jobs/{job_id}/cancel",
            post(admin_cancel_danmaku_match),
        )
        .route(
            "/api/v1/admin/danmaku/match-jobs/{job_id}/retry",
            post(admin_retry_danmaku_match),
        )
        .route(
            "/api/v1/admin/jobs/{job_id}/events",
            get(admin_list_job_events),
        )
        .route("/api/v1/admin/jobs", get(admin_list_jobs))
        .route(
            "/api/v1/admin/scheduled-tasks",
            get(admin_list_scheduled_tasks).put(admin_upsert_scheduled_task),
        )
        .route(
            "/api/v1/admin/settings",
            get(admin_settings).patch(admin_update_settings),
        )
        .route(
            "/api/v1/admin/api-key",
            get(admin_get_api_key).delete(admin_revoke_api_key),
        )
        .route("/api/v1/admin/api-key/rotate", post(admin_rotate_api_key))
        .route(
            "/api/v1/admin/settings/network-proxy/test",
            post(admin_test_network_proxy),
        )
        .route(
            "/api/v1/admin/notification-destinations",
            get(admin_list_webhook_destinations).post(admin_create_webhook_destination),
        )
        .route(
            "/api/v1/admin/notification-destinations/{destination_id}",
            get(admin_get_webhook_destination)
                .patch(admin_update_webhook_destination)
                .delete(admin_delete_webhook_destination),
        )
        .route(
            "/api/v1/admin/notification-destinations/{destination_id}/test",
            post(admin_test_webhook_destination),
        )
        .route(
            "/api/v1/admin/notification-destinations/{destination_id}/rotate-secret",
            post(admin_rotate_webhook_secret),
        )
        .route(
            "/api/v1/admin/notification-deliveries",
            get(admin_list_webhook_deliveries),
        )
        .route(
            "/api/v1/admin/notification-deliveries/{delivery_id}/retry",
            post(admin_retry_webhook_delivery),
        )
        .route("/api/v1/admin/health", get(admin_health))
        .route("/api/v1/admin/dashboard", get(admin_dashboard))
        .route("/api/v1/admin/events", get(admin_events))
        .route("/api/v1/admin/audit", get(admin_list_audit))
        .route("/api/v1/admin/logs/export", get(admin_export_logs))
        .route("/api/v1/admin/logs", get(admin_list_logs))
        .route("/api/v1/libraries", get(lux_list_libraries))
        .route(
            "/api/v1/libraries/{library_id}/cover",
            get(lux_library_cover).head(lux_library_cover),
        )
        .route("/api/v1/favorites", get(lux_list_favorites))
        .route("/api/v1/search", get(lux_search))
        .route("/api/v1/home", get(lux_home))
        .route(
            "/api/v1/libraries/{library_id}/items",
            get(lux_list_library_items),
        )
        .route("/api/v1/items/{item_id}", get(lux_get_item))
        .route(
            "/api/v1/people/{person_id}/image",
            get(lux_get_person_image),
        )
        .route(
            "/api/v1/people/{provider}/{person_id}/image",
            get(lux_get_person_image_for_provider),
        )
        .route("/api/v1/items/{item_id}/children", get(lux_get_children))
        .route(
            "/api/v1/collections/{collection_id}",
            get(lux_get_collection),
        )
        .route(
            "/api/v1/items/{item_id}/images/{image_type}",
            get(lux_image).head(lux_image),
        )
        .route(
            "/api/v1/items/{item_id}/images/{image_type}/{image_index}",
            get(lux_image_at_index).head(lux_image_at_index),
        )
        .route("/api/v1/items/{item_id}/images", get(lux_list_item_images))
        .route(
            "/api/v1/items/{item_id}/images/search",
            post(lux_search_item_images),
        )
        .route(
            "/api/v1/items/{item_id}/images/select",
            post(lux_select_item_image),
        )
        .route(
            "/api/v1/items/{item_id}/subtitles/{stream_index}",
            get(lux_subtitle).head(lux_subtitle),
        )
        .route(
            "/api/v1/items/{item_id}/stream",
            get(lux_stream).head(lux_stream),
        )
        .route("/api/v1/items/{item_id}/playback", get(lux_get_playback))
        .route("/api/v1/items/{item_id}/progress", post(lux_post_progress))
        .route("/api/v1/items/{item_id}/favorite", put(lux_set_favorite))
        .route("/api/v1/items/{item_id}/played", put(lux_set_played))
        .route(
            "/api/v1/items/{item_id}/metadata",
            get(lux_get_metadata).patch(lux_update_metadata),
        )
        .route(
            "/api/v1/items/{item_id}/download",
            get(lux_download).head(lux_download),
        )
        .merge(emby_routes())
        .nest("/emby", emby_routes())
        .fallback_service(
            ServeDir::new(web_root.clone())
                .append_index_html_on_directories(true)
                .fallback(ServeFile::new(web_root.join("index.html"))),
        )
        .with_state(state)
        .layer(middleware::from_fn(
            move |request: Request<Body>, next: Next| {
                let catalog_request_slots = catalog_request_slots.clone();
                let catalog_workers = catalog_workers.clone();
                async move {
                    let request_slot = if is_catalog_aggregation_path(request.uri().path()) {
                        match catalog_request_slots.try_acquire_owned() {
                            Ok(permit) => Some(permit),
                            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                        }
                    } else {
                        None
                    };
                    let worker_permit = if request_slot.is_some() {
                        match catalog_workers.acquire_owned().await {
                            Ok(permit) => Some(permit),
                            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                        }
                    } else {
                        None
                    };
                    let response = next.run(request).await;
                    drop(worker_permit);
                    drop(request_slot);
                    response
                }
            },
        ))
        .layer(middleware::from_fn(attach_peer_address))
        .layer(middleware::from_fn(normalize_lux_api_key_query))
        .layer(middleware::from_fn(trace_emby_playback_callback))
        .layer(middleware::from_fn(trace_emby_playback_info))
        .layer(middleware::from_fn(trace_emby_media_stream_failure))
        .layer(middleware::from_fn(reject_unmatched_emby_video_path))
        .layer(middleware::from_fn(normalize_empty_api_service_unavailable))
        .layer(middleware::from_fn(
            move |request: Request<Body>, next: Next| {
                let resources = resources.clone();
                async move {
                    let is_home = request.uri().path() == "/api/v1/home";
                    let started = Instant::now();
                    let response = next.run(request).await;
                    if is_home {
                        resources.record_home_latency(started.elapsed());
                    }
                    response
                }
            },
        ))
        .layer(
            tower::ServiceBuilder::new()
                .set_x_request_id(MakeRequestUuid)
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(|request: &axum::http::Request<_>| {
                            let request_id = request
                                .headers()
                                .get("x-request-id")
                                .and_then(|value| value.to_str().ok())
                                .unwrap_or("unknown");
                            tracing::info_span!(
                                "request",
                                method = %request.method(),
                                path = %safe_trace_path(request.uri()),
                                version = ?request.version(),
                                "requestId" = %request_id,
                                "durationMs" = tracing::field::Empty,
                                "statusCode" = tracing::field::Empty,
                                "errorCode" = tracing::field::Empty,
                            )
                        })
                        .on_response(
                            |response: &Response, latency: Duration, span: &tracing::Span| {
                                let duration_ms =
                                    u64::try_from(latency.as_millis()).unwrap_or(u64::MAX);
                                span.record("durationMs", duration_ms);
                                span.record("statusCode", response.status().as_u16());
                                tracing::debug!(
                                    latency = ?latency,
                                    status = %response.status(),
                                    "finished processing request"
                                );
                            },
                        ),
                )
                .propagate_x_request_id(),
        )
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

fn emby_routes() -> Router<AppState> {
    Router::new()
        .route("/System/Info/Public", get(emby_public_system_info))
        .route("/System/Info", get(emby_system_info))
        .route("/System/Ping", get(emby_ping).post(emby_ping))
        .route("/Users/Public", get(emby_public_users))
        .route("/Users/AuthenticateByName", post(emby_authenticate))
        .route("/Library/VirtualFolders", get(emby_library_virtual_folders))
        .route("/Persons", get(emby_persons))
        .route("/Users/{user_id}", get(emby_user))
        .route("/Users/{user_id}/Views", get(emby_user_views))
        .route("/Users/{user_id}/Items/Root", get(emby_user_root))
        .route("/Users/{user_id}/Items/Resume", get(emby_user_resume))
        .route("/Users/{user_id}/Items/Latest", get(emby_user_latest))
        .route("/Users/{user_id}/Items/NextUp", get(emby_user_next_up))
        .route("/Users/{user_id}/Items", get(emby_user_items))
        .route("/Users/{user_id}/Items/{item_id}", get(emby_user_item))
        .route(
            "/Persons/{person_id}/Images/{image_type}",
            get(emby_person_image).head(emby_person_image),
        )
        .route(
            "/Persons/{person_id}/Images/{image_type}/{image_index}",
            get(emby_person_image_at_index).head(emby_person_image_at_index),
        )
        .route("/Shows/NextUp", get(emby_shows_next_up))
        .route("/Shows/{series_id}/Seasons", get(emby_show_seasons))
        .route("/Shows/{series_id}/Episodes", get(emby_show_episodes))
        .route("/Items", get(emby_items))
        .route("/Items/Counts", get(emby_items_counts))
        .route("/Items/Root", get(emby_items_root))
        .route("/Search/Hints", get(emby_search_hints))
        .route("/Items/{item_id}", get(emby_item))
        .route("/Items/{item_id}/Children", get(emby_collection_children))
        .route("/api/danmu/{item_id}", get(emby_danmaku_info))
        .route("/api/danmu/{item_id}/raw", get(emby_danmaku_raw))
        .route(
            "/Items/{item_id}/Images/{image_type}",
            get(emby_image).head(emby_image),
        )
        .route(
            "/Items/{item_id}/Images/{image_type}/{image_index}",
            get(emby_image_at_index).head(emby_image_at_index),
        )
        .route(
            "/Videos/{item_id}/{media_source_id}/Subtitles/{stream_index}/Stream",
            get(emby_subtitle_with_source).head(emby_subtitle_with_source),
        )
        .route(
            "/Videos/{item_id}/original.strm",
            get(emby_stream).head(emby_stream),
        )
        .route(
            "/Items/{item_id}/Subtitles/{stream_index}/Stream",
            get(emby_subtitle_without_source).head(emby_subtitle_without_source),
        )
        .route(
            "/Videos/{item_id}/stream",
            get(emby_stream).head(emby_stream),
        )
        .route(
            "/Videos/{item_id}/stream.{container}",
            get(emby_stream_with_container).head(emby_stream_with_container),
        )
        .route(
            "/Videos/{item_id}/{media_source_id}/stream",
            get(emby_stream_with_source).head(emby_stream_with_source),
        )
        .route(
            "/Videos/{item_id}/{media_source_id}/stream.{container}",
            get(emby_stream_with_source_and_container).head(emby_stream_with_source_and_container),
        )
        .route(
            "/videos/{item_id}/stream",
            get(emby_stream).head(emby_stream),
        )
        .route(
            "/videos/{item_id}/original.strm",
            get(emby_stream).head(emby_stream),
        )
        .route(
            "/videos/{item_id}/stream.{container}",
            get(emby_stream_with_container).head(emby_stream_with_container),
        )
        .route(
            "/videos/{item_id}/{media_source_id}/stream",
            get(emby_stream_with_source).head(emby_stream_with_source),
        )
        .route(
            "/videos/{item_id}/{media_source_id}/stream.{container}",
            get(emby_stream_with_source_and_container).head(emby_stream_with_source_and_container),
        )
        .route(
            "/Items/{item_id}/PlaybackInfo",
            get(emby_playback_info).post(emby_playback_info),
        )
        .route(
            "/Items/{item_id}/Download",
            get(emby_download).head(emby_download),
        )
        .route("/Sessions", get(emby_sessions))
        .route("/Sessions/Playing", post(emby_playing))
        .route("/Sessions/Playing/Progress", post(emby_playing_progress))
        .route("/Sessions/Playing/Stopped", post(emby_playing_stopped))
        .route(
            "/Users/{user_id}/PlayedItems/{item_id}",
            post(emby_mark_played).delete(emby_unmark_played),
        )
        .route(
            "/Users/{user_id}/FavoriteItems/{item_id}",
            post(emby_mark_favorite).delete(emby_unmark_favorite),
        )
        .route("/Sessions/Logout", post(emby_logout))
}

#[derive(Deserialize, Default)]
struct DanmakuQuery {
    #[serde(
        rename = "api_key",
        alias = "apiKey",
        alias = "ApiKey",
        alias = "X-Emby-Token",
        alias = "x-emby-token",
        alias = "X-MediaBrowser-Token",
        alias = "x-media-browser-token"
    )]
    api_key: Option<String>,
    option: Option<String>,
}

async fn emby_danmaku_info(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<DanmakuQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    match access.can_view_item(principal, &item_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::FORBIDDEN.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(service) = state.danmaku.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.read_sidecar(&item_id).await {
        Ok(Some(_)) => Json(json!({
            "hasDanmaku": true,
            "format": "xml",
            "url": format!("/api/danmu/{item_id}/raw"),
            "rawUrl": format!("/api/danmu/{item_id}/raw"),
            "option": query.option,
        }))
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn emby_danmaku_raw(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<DanmakuQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    match access.can_view_item(principal, &item_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::FORBIDDEN.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(service) = state.danmaku.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.read_sidecar(&item_id).await {
        Ok(Some(bytes)) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/xml; charset=utf-8")
            .header("Cache-Control", "private, no-cache")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn current_emby_server_name(state: &AppState) -> String {
    let Some(database) = state.database.as_ref() else {
        return DEFAULT_SERVER_NAME.to_owned();
    };
    match database.server_name().await {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        Ok(_) | Err(_) => DEFAULT_SERVER_NAME.to_owned(),
    }
}

async fn emby_public_system_info(State(state): State<AppState>) -> Json<Value> {
    let startup_wizard_completed = match state.setup.as_ref() {
        Some(setup) => setup.status().await.unwrap_or(false),
        None => false,
    };
    let server_name = current_emby_server_name(&state).await;
    Json(json!({
        "LocalAddress": "",
        "ServerName": server_name,
        "Version": VERSION,
        "Id": state.server_id,
        "StartupWizardCompleted": startup_wizard_completed
    }))
}

async fn emby_system_info(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let Some(auth) = state.emby_auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if let Err(status) = require_emby_token(&headers, &query, auth, &state).await {
        return status.into_response();
    }
    let server_name = current_emby_server_name(&state).await;
    Json(json!({
        "LocalAddress": "",
        "ServerName": server_name,
        "Version": VERSION,
        "Id": state.server_id,
        "OperatingSystem": std::env::consts::OS,
        "OperatingSystemDisplayName": std::env::consts::OS,
        "SupportsLibraryMonitor": false,
        "SupportsHttps": false,
        "HasPendingRestart": false,
        "IsShuttingDown": false,
        "HttpServerPortNumber": 8097
    }))
    .into_response()
}

async fn emby_ping(
    _headers: HeaderMap,
    Query(_query): Query<EmbyTokenQuery>,
    State(_state): State<AppState>,
) -> Response {
    StatusCode::OK.into_response()
}

async fn emby_public_users(State(state): State<AppState>) -> Json<Value> {
    let server_id = state.server_id.clone();
    let Some(auth) = state.emby_auth.as_ref() else {
        return Json(json!([]));
    };
    let server_name = current_emby_server_name(&state).await;
    let users = auth.public_users().await.unwrap_or_default();
    Json(Value::Array(
        users
            .iter()
            .map(|user| emby_user_json(user, &server_id, &server_name))
            .collect(),
    ))
}

async fn emby_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let server_name = current_emby_server_name(&state).await;
    Json(emby_user_json(&user, &state.server_id, &server_name)).into_response()
}

#[derive(Deserialize)]
struct EmbyAuthenticateRequest {
    #[serde(rename = "Username")]
    username: String,
    #[serde(rename = "Pw")]
    password: String,
}

async fn emby_authenticate(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<EmbyAuthenticateRequest>,
) -> Response {
    let Some(auth) = state.emby_auth.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let login_key = login_attempt_key(&headers, &request.username);
    if !state.login_rate_limiter.is_allowed(&login_key).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let device = emby_device_info_from_headers(&headers);
    match auth
        .authenticate(&request.username, &request.password, &device)
        .await
    {
        Ok(Some(result)) => {
            if state.remote_access.is_remote(
                header_str(&headers, "x-lux-peer-ip"),
                header_str(&headers, "x-forwarded-for"),
            ) && !result.user.can_remote_access
            {
                let _ = auth.logout(&result.token).await;
                return StatusCode::FORBIDDEN.into_response();
            }
            state.login_rate_limiter.record_success(&login_key).await;
            let user_id = result.user.id.to_string();
            record_activity_event(
                state.database.as_ref(),
                &state.admin_events,
                &user_id,
                "AUTH_LOGIN",
                None,
                json!({
                    "client": result.device.client,
                    "clientVersion": result.device.version,
                    "deviceName": result.device.device,
                    "deviceType": result.device.device,
                }),
            )
            .await;
            let server_name = current_emby_server_name(&state).await;
            Json(json!({
                "User": emby_user_json(&result.user, &state.server_id, &server_name),
                "SessionInfo": emby_login_session_json(&result, &state.server_id),
                "AccessToken": result.token,
                "ServerId": state.server_id
            }))
            .into_response()
        }
        Ok(None) => {
            state.login_rate_limiter.record_failure(&login_key).await;
            StatusCode::UNAUTHORIZED.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize, Default)]
struct EmbyTokenQuery {
    #[serde(
        rename = "api_key",
        alias = "apiKey",
        alias = "ApiKey",
        alias = "X-Emby-Token",
        alias = "x-emby-token",
        alias = "X-MediaBrowser-Token",
        alias = "x-media-browser-token"
    )]
    api_key: Option<String>,
    #[serde(rename = "tag", alias = "Tag")]
    tag: Option<String>,
    #[serde(rename = "Fields", default)]
    fields: Option<String>,
}

#[derive(Deserialize, Default)]
struct EmbyPersonsQuery {
    #[serde(flatten)]
    auth: EmbyTokenQuery,
    #[serde(rename = "UserId", alias = "userId", alias = "userid", default)]
    user_id: Option<String>,
    #[serde(rename = "ParentId", alias = "parentId", default)]
    parent_id: Option<String>,
    #[serde(rename = "PersonTypes", alias = "personTypes", default)]
    person_types: Option<String>,
    #[serde(rename = "StartIndex", alias = "startIndex", default)]
    start_index: Option<i64>,
    #[serde(rename = "Limit", alias = "limit", default)]
    limit: Option<i64>,
    #[serde(rename = "Recursive", alias = "recursive", default)]
    recursive: Option<bool>,
    #[serde(rename = "SortBy", alias = "sortBy", default)]
    sort_by: Option<String>,
    #[serde(rename = "SortOrder", alias = "sortOrder", default)]
    sort_order: Option<String>,
}

async fn require_emby_token(
    headers: &HeaderMap,
    query: &EmbyTokenQuery,
    auth: &EmbyAuthService,
    state: &AppState,
) -> Result<(), StatusCode> {
    let user = resolve_emby_user_with_auth(headers, query, auth, state).await?;
    if state.remote_access.is_remote(
        header_str(headers, "x-lux-peer-ip"),
        header_str(headers, "x-forwarded-for"),
    ) && !user.can_remote_access
    {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
}

async fn resolve_emby_user_with_auth(
    headers: &HeaderMap,
    query: &EmbyTokenQuery,
    auth: &EmbyAuthService,
    state: &AppState,
) -> Result<UserRecord, StatusCode> {
    let token = emby_token_from_headers(headers)
        .or_else(|| query.api_key.clone())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if let Some(service) = state.admin_api_key.as_ref() {
        match service.resolve(&token).await {
            Ok(Some(user)) => return Ok(user),
            Ok(None) => {}
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        }
    }
    match auth.resolve_token(&token).await {
        Ok(Some(user)) => Ok(user),
        Ok(None) => Err(StatusCode::UNAUTHORIZED),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

fn emby_token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("X-Lux-Api-Key")
        .and_then(|value| value.to_str().ok())
        .and_then(emby_token_header_value)
        .or_else(|| {
            headers
                .get("X-Emby-Token")
                .or_else(|| headers.get("X-MediaBrowser-Token"))
                .and_then(|value| value.to_str().ok())
                .and_then(emby_token_header_value)
        })
        .or_else(|| {
            headers
                .get("X-Emby-Authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(emby_authorization_token)
        })
        .or_else(|| {
            headers
                .get("X-Emby-Authentication")
                .and_then(|value| value.to_str().ok())
                .and_then(emby_authorization_token)
        })
        .or_else(|| {
            headers
                .get("Authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(emby_token_header_value)
        })
}

fn emby_device_info_from_headers(headers: &HeaderMap) -> EmbyDeviceInfo {
    let mut info = EmbyDeviceInfo::default();
    for name in [
        "X-Emby-Authorization",
        "X-Emby-Authentication",
        "Authorization",
    ] {
        let Some(candidate) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(EmbyDeviceInfo::parse)
        else {
            continue;
        };
        merge_emby_device_info(&mut info, candidate);
    }
    info
}

fn merge_emby_device_info(target: &mut EmbyDeviceInfo, fallback: EmbyDeviceInfo) {
    if target.client.is_empty() {
        target.client = fallback.client;
    }
    if target.device.is_empty() {
        target.device = fallback.device;
    }
    if target.device_id.is_empty() {
        target.device_id = fallback.device_id;
    }
    if target.version.is_empty() {
        target.version = fallback.version;
    }
    if target.user_id.is_none() {
        target.user_id = fallback.user_id;
    }
}

fn emby_token_header_value(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(token) = value.strip_prefix("Bearer ") {
        return (!token.is_empty()).then(|| token.to_owned());
    }
    emby_authorization_token(value).or_else(|| (!value.is_empty()).then(|| value.to_owned()))
}

fn emby_authorization_token(value: &str) -> Option<String> {
    let parameters = value
        .split_once(' ')
        .map_or(value, |(_, parameters)| parameters);
    parameters.split(',').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("Token") {
            return None;
        }
        let token = value.trim().trim_matches('"');
        (!token.is_empty()).then(|| token.to_owned())
    })
}

async fn require_emby_user(
    headers: &HeaderMap,
    state: &AppState,
    api_key: Option<&str>,
) -> Result<UserRecord, StatusCode> {
    let query = EmbyTokenQuery {
        api_key: api_key.map(str::to_owned),
        tag: None,
        fields: None,
    };
    require_emby_user_with_query(headers, state, &query).await
}

async fn require_emby_user_with_query(
    headers: &HeaderMap,
    state: &AppState,
    query: &EmbyTokenQuery,
) -> Result<UserRecord, StatusCode> {
    let Some(auth) = state.emby_auth.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let user = resolve_emby_user_with_auth(headers, query, auth, state).await?;
    if state.remote_access.is_remote(
        header_str(headers, "x-lux-peer-ip"),
        header_str(headers, "x-forwarded-for"),
    ) && !user.can_remote_access
    {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(user)
}

async fn emby_logout(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> StatusCode {
    let Some(auth) = state.emby_auth else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    let token = headers
        .get("X-Emby-Token")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or(query.api_key);
    let Some(token) = token else {
        return StatusCode::UNAUTHORIZED;
    };
    match auth.logout(&token).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[derive(Deserialize, Default)]
struct EmbyItemsQuery {
    #[serde(
        rename = "api_key",
        alias = "apiKey",
        alias = "ApiKey",
        alias = "X-Emby-Token",
        alias = "x-emby-token",
        alias = "X-MediaBrowser-Token",
        alias = "x-media-browser-token",
        default
    )]
    api_key: Option<String>,
    #[serde(rename = "UserId", alias = "userId", alias = "userid", default)]
    user_id: Option<String>,
    #[serde(rename = "SeriesId", alias = "seriesId", default)]
    series_id: Option<String>,
    #[serde(rename = "ParentId", default)]
    parent_id: Option<String>,
    #[serde(rename = "Ids", default)]
    ids: Option<String>,
    #[serde(rename = "IncludeItemTypes", default)]
    include_item_types: Option<String>,
    #[serde(rename = "ExcludeItemTypes", default)]
    exclude_item_types: Option<String>,
    #[serde(rename = "SeasonId", default)]
    season_id: Option<String>,
    #[serde(rename = "SearchTerm", alias = "searchTerm", default)]
    search_term: Option<String>,
    #[serde(rename = "StartIndex", default)]
    start_index: Option<i64>,
    #[serde(rename = "Limit", default)]
    limit: Option<i64>,
    #[serde(rename = "IsPlayed", default)]
    is_played: Option<bool>,
    #[serde(rename = "IsFavorite", default)]
    is_favorite: Option<bool>,
    #[serde(rename = "Years", default)]
    years: Option<String>,
    #[serde(rename = "SortBy", default)]
    sort_by: Option<String>,
    #[serde(rename = "SortOrder", default)]
    sort_order: Option<String>,
    #[serde(rename = "Fields", default)]
    fields: Option<String>,
    #[serde(rename = "GroupItems", default)]
    group_items: Option<bool>,
    #[serde(rename = "EnableTotalRecordCount", default)]
    enable_total_record_count: Option<bool>,
    #[serde(rename = "Recursive", default)]
    recursive: Option<bool>,
}

#[derive(Deserialize, Default)]
struct EmbyItemCountsQuery {
    #[serde(
        rename = "api_key",
        alias = "apiKey",
        alias = "ApiKey",
        alias = "X-Emby-Token",
        alias = "x-emby-token",
        alias = "X-MediaBrowser-Token",
        alias = "x-media-browser-token",
        default
    )]
    api_key: Option<String>,
    #[serde(rename = "UserId", alias = "userId", alias = "userid", default)]
    user_id: Option<String>,
    #[serde(rename = "IsFavorite", alias = "isFavorite", default)]
    is_favorite: Option<bool>,
}

fn emby_fields_include(fields: Option<&str>, field: &str) -> bool {
    fields.is_none_or(|fields| {
        fields
            .split(',')
            .map(str::trim)
            .any(|value| value.eq_ignore_ascii_case(field))
    })
}

/// Filmly sends `ShareLevel` as a capability hint on item detail requests. It
/// is not a field selector, so discard it before applying the Emby field
/// projection; if it is the only value, keep the normal full-detail response.
fn emby_detail_fields(fields: Option<&str>) -> Option<String> {
    let fields = fields?;
    let filtered = fields
        .split(',')
        .map(str::trim)
        .filter(|field| !field.is_empty() && !field.eq_ignore_ascii_case("ShareLevel"))
        .collect::<Vec<_>>();
    (!filtered.is_empty()).then(|| filtered.join(","))
}

fn normalize_emby_item_type(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "movie" => Some("MOVIE".to_owned()),
        "series" | "show" => Some("SERIES".to_owned()),
        "season" => Some("SEASON".to_owned()),
        "episode" => Some("EPISODE".to_owned()),
        "boxset" | "box_set" => Some("BOX_SET".to_owned()),
        "folder" => Some("FOLDER".to_owned()),
        _ => None,
    }
}

fn catalog_filter_from_values(
    item_types: Option<&str>,
    years: Option<&str>,
    is_played: Option<bool>,
    is_favorite: Option<bool>,
    sort_by: Option<&str>,
    sort_order: Option<&str>,
    metadata_pending: bool,
) -> CatalogFilter {
    let item_types = item_types
        .map(|values| {
            let raw_values = values
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            let normalized = raw_values
                .iter()
                .filter_map(|value| normalize_emby_item_type(value))
                .collect::<Vec<_>>();
            if raw_values.is_empty() || !normalized.is_empty() {
                normalized
            } else {
                vec!["__NO_MATCH__".to_owned()]
            }
        })
        .unwrap_or_default();
    let years = years
        .map(|values| {
            values
                .split(',')
                .filter_map(|value| value.trim().parse::<i64>().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    CatalogFilter {
        item_types,
        excluded_item_types: Vec::new(),
        item_ids: None,
        media_source_ids: None,
        years,
        is_played,
        is_favorite,
        metadata_pending,
        sort_by: match sort_by {
            Some(value)
                if value
                    .split(',')
                    .any(|field| field.trim().eq_ignore_ascii_case("DateCreated")) =>
            {
                CatalogSort::DateCreated
            }
            Some(value)
                if value
                    .split(',')
                    .any(|field| field.trim().eq_ignore_ascii_case("PremiereDate")) =>
            {
                CatalogSort::PremiereDate
            }
            Some(value)
                if value.split(',').any(|field| {
                    field.trim().eq_ignore_ascii_case("CommunityRating")
                        || field.trim().eq_ignore_ascii_case("Rating")
                }) =>
            {
                CatalogSort::Rating
            }
            _ => CatalogSort::Name,
        },
        descending: sort_order.is_some_and(|value| value.eq_ignore_ascii_case("Descending")),
    }
}

fn catalog_filter_from_emby(query: &EmbyItemsQuery) -> CatalogFilter {
    let mut filter = catalog_filter_from_values(
        query.include_item_types.as_deref(),
        query.years.as_deref(),
        query.is_played,
        query.is_favorite,
        query.sort_by.as_deref(),
        query.sort_order.as_deref(),
        false,
    );
    let ids = query.ids.as_deref().map(|values| {
        values
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect()
    });
    filter.item_ids = ids.clone();
    filter.media_source_ids = ids;
    filter.excluded_item_types = query
        .exclude_item_types
        .as_deref()
        .map(|values| {
            values
                .split(',')
                .filter_map(normalize_emby_item_type)
                .collect()
        })
        .unwrap_or_default();
    filter
}

fn emby_compat_media_source_id<'a>(ids: Option<&'a str>, page: &CatalogPage) -> Option<&'a str> {
    ids?.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .find(|id| {
            page.items.iter().any(|item| {
                item.id != *id && item.media_sources.iter().any(|source| source.id == *id)
            })
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EmbyClientCompatibility {
    Generic,
    VidHub,
}

fn emby_client_compatibility_from_name(client: Option<&str>) -> EmbyClientCompatibility {
    match client.map(str::trim) {
        Some(client) if client.eq_ignore_ascii_case("vidhub") => EmbyClientCompatibility::VidHub,
        _ => EmbyClientCompatibility::Generic,
    }
}

async fn emby_client_compatibility(
    headers: &HeaderMap,
    api_key: Option<&str>,
    state: &AppState,
) -> EmbyClientCompatibility {
    let header_device = emby_device_info_from_headers(headers);
    if !header_device.client.is_empty() {
        return emby_client_compatibility_from_name(Some(&header_device.client));
    }
    let Some(token) = emby_token_from_headers(headers).or_else(|| api_key.map(str::to_owned))
    else {
        return EmbyClientCompatibility::Generic;
    };
    let Some(auth) = state.emby_auth.as_ref() else {
        return EmbyClientCompatibility::Generic;
    };
    match auth.device_info(&token).await {
        Ok(Some(device)) => emby_client_compatibility_from_name(Some(&device.client)),
        Ok(None) | Err(_) => EmbyClientCompatibility::Generic,
    }
}

async fn emby_user_views(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let compatibility = emby_client_compatibility(&headers, query.api_key.as_deref(), &state).await;
    match emby_visible_library_items(&state, principal, compatibility).await {
        Ok(items) => {
            let total = items.len();
            Json(json!({
                "Items": items,
                "TotalRecordCount": total,
                "StartIndex": 0,
            }))
            .into_response()
        }
        Err(status) => status.into_response(),
    }
}

async fn emby_library_virtual_folders(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if !user.can_manage_server {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(libraries) = state.libraries.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let media_strategy = match read_media_strategy_settings(database).await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let (resume_played_percent, resume_min_ticks) = match database.resume_settings().await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let compatibility = emby_client_compatibility(&headers, query.api_key.as_deref(), &state).await;
    match libraries.list_libraries().await {
        Ok(views) => Json(
            views
                .iter()
                .filter(|view| view.library.is_enabled)
                .map(|view| {
                    emby_virtual_folder_json(
                        view,
                        &media_strategy,
                        resume_played_percent,
                        resume_min_ticks,
                        compatibility,
                    )
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn emby_persons(
    headers: HeaderMap,
    Query(query): Query<EmbyPersonsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.auth.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Some(user_id) = query.user_id.as_deref()
        && let Err(status) = ensure_emby_user_scope(&user, user_id)
    {
        return status.into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let accessible_library_ids = match access.accessible_library_ids(principal).await {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let library_ids = match query.parent_id.as_deref() {
        Some(parent_id) if accessible_library_ids.iter().any(|id| id == parent_id) => {
            vec![parent_id.to_owned()]
        }
        Some(_) => return StatusCode::NOT_FOUND.into_response(),
        None => accessible_library_ids,
    };
    let (offset, limit) = match emby_person_page_params(&query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    // Keep the historical Lux behavior for clients that omit Recursive. An
    // explicit false still requests only direct children.
    let recursive = query.recursive.unwrap_or(true);
    let sort_by = match emby_person_sort(query.sort_by.as_deref()) {
        Ok(sort_by) => sort_by,
        Err(status) => return status.into_response(),
    };
    let descending = match emby_person_sort_order(query.sort_order.as_deref()) {
        Ok(descending) => descending,
        Err(status) => return status.into_response(),
    };
    let options = PersonListOptions {
        recursive,
        sort_by,
        descending,
        offset,
        limit,
    };
    let person_type = match emby_person_type_filter(query.person_types.as_deref()) {
        Some(person_type) => person_type,
        None => {
            return Json(json!({
                "Items": [],
                "TotalRecordCount": 0,
            }))
            .into_response();
        }
    };
    let Some(people) = state.people.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = match query.parent_id.as_deref() {
        Some(parent_id) => {
            people
                .list_library_actors(parent_id, person_type, options)
                .await
        }
        None => {
            people
                .list_libraries_actors(&library_ids, person_type, options)
                .await
        }
    };
    let (actors, total) = match result {
        Ok(result) => result,
        Err(PeopleError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    Json(json!({
        "Items": actors
            .into_iter()
            .map(|actor| {
                emby_person_json_with_fields(actor, &state.server_id, query.auth.fields.as_deref())
            })
            .collect::<Vec<_>>(),
        "TotalRecordCount": total,
    }))
    .into_response()
}

async fn emby_user_root(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let compatibility = emby_client_compatibility(&headers, query.api_key.as_deref(), &state).await;
    emby_user_root_response(
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
        compatibility,
    )
    .await
}

async fn emby_items_root(
    headers: HeaderMap,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let requested_user_id = query.user_id.unwrap_or_else(|| user.id.to_string());
    if let Err(status) = ensure_emby_user_scope(&user, &requested_user_id) {
        return status.into_response();
    }
    let compatibility = emby_client_compatibility(&headers, query.api_key.as_deref(), &state).await;
    emby_user_root_response(
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
        compatibility,
    )
    .await
}

async fn emby_user_root_response(
    state: &AppState,
    principal: AccessPrincipal,
    compatibility: EmbyClientCompatibility,
) -> Response {
    let items = match emby_visible_library_items(state, principal, compatibility).await {
        Ok(items) => items,
        Err(status) => return status.into_response(),
    };
    Json(json!({
        "Name": "Media Folders",
        "SortName": "Media Folders",
        "Id": principal.user_id.to_string(),
        "ServerId": state.server_id,
        "Type": "Folder",
        "IsFolder": true,
        "MediaType": "Video",
        "ChildCount": items.len(),
        "RecursiveItemCount": items.len(),
        "ImageTags": {},
        "BackdropImageTags": [],
        "UserData": {
            "PlaybackPositionTicks": 0,
            "PlayCount": 0,
            "IsFavorite": false,
            "Played": false,
        },
    }))
    .into_response()
}

async fn emby_visible_library_items(
    state: &AppState,
    principal: AccessPrincipal,
    compatibility: EmbyClientCompatibility,
) -> Result<Vec<Value>, StatusCode> {
    let Some(access) = state.access.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let views = libraries
        .list_libraries()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let mut items = Vec::new();
    for view in views {
        let library_id = view.library.id.to_string();
        let can_view = access
            .can_view_library(principal, &library_id)
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        if !view.library.is_enabled || !can_view {
            continue;
        }
        let child_count =
            emby_library_root_count(state, principal, &library_id, view.library.kind).await?;
        items.push(emby_library_view_json(
            &view.library,
            &state.server_id,
            child_count,
            compatibility,
        ));
    }
    Ok(items)
}

async fn emby_library_root_count(
    state: &AppState,
    principal: AccessPrincipal,
    library_id: &str,
    kind: LibraryKind,
) -> Result<i64, StatusCode> {
    let Some(catalog) = state.catalog.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let item_types = match kind {
        LibraryKind::Movie => vec!["MOVIE".to_owned()],
        LibraryKind::Series => vec!["SERIES".to_owned()],
        LibraryKind::Mixed => vec!["MOVIE".to_owned(), "SERIES".to_owned()],
    };
    catalog
        .list_library_items_filtered(
            principal,
            library_id,
            &CatalogFilter {
                item_types,
                ..CatalogFilter::default()
            },
            0,
            1,
        )
        .await
        .map(|page| page.total)
        .map_err(|error| match error {
            CatalogError::Storage(_) => StatusCode::SERVICE_UNAVAILABLE,
            CatalogError::LibraryNotFound | CatalogError::AccessDenied => StatusCode::NOT_FOUND,
        })
}

async fn emby_user_resume(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let (offset, limit) = match emby_page_params(&query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let page = match catalog
        .list_continue_watching(principal, &user_id, offset, limit)
        .await
    {
        Ok(page) => page,
        Err(CatalogError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            return StatusCode::FORBIDDEN.into_response();
        }
    };
    emby_catalog_page_for_user_with_fields(
        &state,
        &user_id,
        &page,
        query.fields.as_deref(),
        user.can_download,
    )
    .await
}

async fn emby_user_latest(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(mut query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let group_items = query.group_items.unwrap_or(true);
    let parent_is_library = match query.parent_id.as_deref() {
        Some(parent_id) => emby_parent_is_library(&state, parent_id).await,
        None => false,
    };
    if group_items
        && query.include_item_types.is_none()
        && (query.parent_id.is_none() || parent_is_library)
    {
        query.include_item_types = Some("Movie,Series".to_owned());
    }
    query.sort_by = Some("DateCreated".to_owned());
    query.sort_order = Some("Descending".to_owned());
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let page = match emby_catalog_page_from_query(&state, principal, &query).await {
        Ok(page) => page,
        Err(status) => return status.into_response(),
    };
    if group_items && emby_latest_groups_children(&query) {
        let (grouped_page, group_counts) =
            match emby_group_latest_page(&state, principal, page).await {
                Ok(result) => result,
                Err(status) => return status.into_response(),
            };
        let mut items = match emby_catalog_items_for_user(
            &state,
            &user_id,
            &grouped_page,
            query.fields.as_deref(),
            user.can_download,
        )
        .await
        {
            Ok(items) => items,
            Err(status) => return status.into_response(),
        };
        for item in &mut items {
            let Some(item_id) = item.get("Id").and_then(Value::as_str) else {
                continue;
            };
            let Some(child_count) = group_counts.get(item_id) else {
                continue;
            };
            if let Value::Object(object) = item {
                object.insert("ChildCount".to_owned(), json!(child_count));
                object.insert("RecursiveItemCount".to_owned(), json!(child_count));
            }
        }
        return Json(items).into_response();
    }
    match emby_catalog_items_for_user(
        &state,
        &user_id,
        &page,
        query.fields.as_deref(),
        user.can_download,
    )
    .await
    {
        Ok(items) => Json(items).into_response(),
        Err(status) => status.into_response(),
    }
}

async fn emby_parent_is_library(state: &AppState, parent_id: &str) -> bool {
    let Ok(library_id) = parent_id.parse::<crate::domain::ids::LibraryId>() else {
        return false;
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return false;
    };
    matches!(
        libraries.get_library(library_id).await,
        Ok(library) if library.is_enabled
    )
}

fn emby_latest_groups_children(query: &EmbyItemsQuery) -> bool {
    query.include_item_types.as_deref().is_some_and(|types| {
        types.split(',').any(|item_type| {
            matches!(
                item_type.trim().to_ascii_lowercase().as_str(),
                "episode" | "season"
            )
        })
    })
}

async fn emby_group_latest_page(
    state: &AppState,
    principal: AccessPrincipal,
    page: CatalogPage,
) -> Result<(CatalogPage, HashMap<String, i64>), StatusCode> {
    enum LatestGroup {
        Series(String),
        Item(Box<CatalogItem>),
    }

    let mut groups = Vec::new();
    let mut group_counts = HashMap::new();
    let mut series_ids = Vec::new();
    for item in page.items {
        let Some(series_id) = item
            .series_id
            .as_deref()
            .filter(|_| matches!(item.item_type.as_str(), "EPISODE" | "SEASON"))
        else {
            groups.push(LatestGroup::Item(Box::new(item)));
            continue;
        };
        if !group_counts.contains_key(series_id) {
            series_ids.push(series_id.to_owned());
            groups.push(LatestGroup::Series(series_id.to_owned()));
        }
        *group_counts.entry(series_id.to_owned()).or_insert(0) += 1;
    }

    let Some(catalog) = state.catalog.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let mut series_by_id = HashMap::new();
    if !series_ids.is_empty() {
        let filter = CatalogFilter {
            item_types: vec!["SERIES".to_owned()],
            excluded_item_types: Vec::new(),
            item_ids: Some(series_ids.clone()),
            media_source_ids: None,
            years: Vec::new(),
            is_played: None,
            is_favorite: None,
            metadata_pending: false,
            sort_by: CatalogSort::Name,
            descending: false,
        };
        let series_page = catalog
            .list_all_items_filtered(principal, &filter, 0, series_ids.len() as i64)
            .await
            .map_err(emby_catalog_error_status)?;
        series_by_id.extend(
            series_page
                .items
                .into_iter()
                .map(|item| (item.id.clone(), item)),
        );
    }

    let mut items = Vec::with_capacity(groups.len());
    let mut resolved_group_counts = HashMap::new();
    for group in groups {
        match group {
            LatestGroup::Series(series_id) => {
                if let Some(item) = series_by_id.remove(&series_id) {
                    if let Some(count) = group_counts.get(&series_id) {
                        resolved_group_counts.insert(series_id, *count);
                    }
                    items.push(item);
                }
            }
            LatestGroup::Item(item) => items.push(*item),
        }
    }
    let total = i64::try_from(items.len()).unwrap_or(i64::MAX);
    Ok((
        CatalogPage {
            items,
            total,
            offset: page.offset,
            limit: page.limit,
        },
        resolved_group_counts,
    ))
}

async fn emby_user_next_up(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    emby_next_up_response(&state, &user, &user_id, &query).await
}

async fn emby_shows_next_up(
    headers: HeaderMap,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let user_id = query.user_id.clone().unwrap_or_else(|| user.id.to_string());
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    emby_next_up_response(&state, &user, &user_id, &query).await
}

async fn emby_next_up_response(
    state: &AppState,
    user: &UserRecord,
    user_id: &str,
    query: &EmbyItemsQuery,
) -> Response {
    let (offset, limit) = match emby_page_params(query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog
        .list_next_up(
            AccessPrincipal::new(user.id, user.is_admin),
            user_id,
            query.series_id.as_deref(),
            offset,
            limit,
        )
        .await
    {
        Ok(page) => {
            emby_catalog_page_for_user_with_preferred_source(
                state,
                user_id,
                &page,
                query.fields.as_deref(),
                user.can_download,
                None,
                query.enable_total_record_count != Some(false),
            )
            .await
        }
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            StatusCode::FORBIDDEN.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn emby_show_seasons(
    headers: HeaderMap,
    Path(series_id): Path<String>,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let (offset, limit) = match emby_page_params(&query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog
        .list_children(
            AccessPrincipal::new(user.id, user.is_admin),
            &series_id,
            "SEASON",
            offset,
            limit,
        )
        .await
    {
        Ok(page) => {
            emby_catalog_page_for_user_with_fields(
                &state,
                &user.id.to_string(),
                &page,
                query.fields.as_deref(),
                user.can_download,
            )
            .await
        }
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn emby_show_episodes(
    headers: HeaderMap,
    Path(series_id): Path<String>,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let (offset, limit) = match emby_page_params(&query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    // Emby clients commonly serialize an unset season selector as `SeasonId=`.
    // Treat it the same as an omitted selector instead of looking up an empty ID.
    let season_id = query.season_id.as_deref().and_then(|value| {
        let value = value.trim();
        (!value.is_empty()
            && !value.eq_ignore_ascii_case("null")
            && !value.eq_ignore_ascii_case("undefined"))
        .then_some(value)
    });
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let episodes = catalog
        .list_series_episodes(principal, &series_id, season_id, offset, limit)
        .await;
    let normalize_filmly_null_languages =
        header_str(&headers, "user-agent").is_some_and(is_filmly_user_agent);
    match episodes {
        Ok(page) => {
            emby_catalog_page_for_user_with_preferred_source_and_options(
                &state,
                &user.id.to_string(),
                &page,
                query.fields.as_deref(),
                user.can_download,
                EmbyCatalogPageOptions {
                    preferred_source_id: None,
                    include_start_index: true,
                    normalize_filmly_null_languages,
                },
            )
            .await
        }
        // VidHub can retain a stale season identifier after a library refresh.
        // Emby still serves the show's episode list in that case; retry without
        // the optional season filter so one stale selector cannot blank the page.
        Err(CatalogError::LibraryNotFound) if season_id.is_some() => match catalog
            .list_series_episodes(principal, &series_id, None, offset, limit)
            .await
        {
            Ok(page) => {
                emby_catalog_page_for_user_with_preferred_source_and_options(
                    &state,
                    &user.id.to_string(),
                    &page,
                    query.fields.as_deref(),
                    user.can_download,
                    EmbyCatalogPageOptions {
                        preferred_source_id: None,
                        include_start_index: true,
                        normalize_filmly_null_languages,
                    },
                )
                .await
            }
            Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
                StatusCode::NOT_FOUND.into_response()
            }
            Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn emby_collection_children(
    headers: HeaderMap,
    Path(collection_id): Path<String>,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let (offset, limit) = match emby_page_params(&query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog
        .list_collection_items(
            AccessPrincipal::new(user.id, user.is_admin),
            &collection_id,
            offset,
            limit,
        )
        .await
    {
        Ok(page) => {
            emby_catalog_page_for_user_with_fields(
                &state,
                &user.id.to_string(),
                &page,
                query.fields.as_deref(),
                user.can_download,
            )
            .await
        }
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn emby_catalog_page_for_user_with_fields(
    state: &AppState,
    user_id: &str,
    page: &CatalogPage,
    fields: Option<&str>,
    can_download: bool,
) -> Response {
    emby_catalog_page_for_user_with_preferred_source(
        state,
        user_id,
        page,
        fields,
        can_download,
        None,
        true,
    )
    .await
}

async fn emby_catalog_page_for_user_with_preferred_source(
    state: &AppState,
    user_id: &str,
    page: &CatalogPage,
    fields: Option<&str>,
    can_download: bool,
    preferred_source_id: Option<&str>,
    include_start_index: bool,
) -> Response {
    emby_catalog_page_for_user_with_preferred_source_and_options(
        state,
        user_id,
        page,
        fields,
        can_download,
        EmbyCatalogPageOptions {
            preferred_source_id,
            include_start_index,
            normalize_filmly_null_languages: false,
        },
    )
    .await
}

struct EmbyCatalogPageOptions<'a> {
    preferred_source_id: Option<&'a str>,
    include_start_index: bool,
    normalize_filmly_null_languages: bool,
}

async fn emby_catalog_page_for_user_with_preferred_source_and_options(
    state: &AppState,
    user_id: &str,
    page: &CatalogPage,
    fields: Option<&str>,
    can_download: bool,
    options: EmbyCatalogPageOptions<'_>,
) -> Response {
    match emby_catalog_items_for_user_with_preferred_source(
        state,
        user_id,
        page,
        fields,
        can_download,
        options.preferred_source_id,
    )
    .await
    {
        Ok(mut items) => {
            if options.normalize_filmly_null_languages {
                normalize_filmly_null_languages(&mut items);
            }
            let mut body = json!({
                "Items": items,
                "TotalRecordCount": page.total,
            });
            if options.include_start_index
                && let Value::Object(object) = &mut body
            {
                object.insert("StartIndex".to_owned(), json!(page.offset));
            }
            Json(body).into_response()
        }
        Err(status) => status.into_response(),
    }
}

async fn emby_catalog_items_for_user(
    state: &AppState,
    user_id: &str,
    page: &CatalogPage,
    fields: Option<&str>,
    can_download: bool,
) -> Result<Vec<Value>, StatusCode> {
    emby_catalog_items_for_user_with_preferred_source(
        state,
        user_id,
        page,
        fields,
        can_download,
        None,
    )
    .await
}

async fn emby_catalog_items_for_user_with_preferred_source(
    state: &AppState,
    user_id: &str,
    page: &CatalogPage,
    fields: Option<&str>,
    can_download: bool,
    preferred_source_id: Option<&str>,
) -> Result<Vec<Value>, StatusCode> {
    let Some(database) = state.database.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let item_ids = page
        .items
        .iter()
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    let user_states = match database.list_user_item_states(user_id, &item_ids).await {
        Ok(states) => states,
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let mut catalog_items = page.items.clone();
    if catalog
        .populate_image_tags(&mut catalog_items)
        .await
        .is_err()
    {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    if emby_fields_include(fields, "Chapters")
        && catalog.populate_chapters(&mut catalog_items).await.is_err()
    {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let mut items = Vec::with_capacity(catalog_items.len());
    for item in &catalog_items {
        let nfo = if emby_nfo_fields_requested(fields) {
            read_local_nfo_details(state, &item.id).await
        } else {
            None
        };
        let mut value = emby_catalog_item_json_with_state(
            item,
            &state.server_id,
            user_states.get(&item.id),
            nfo.as_ref(),
            can_download,
            fields,
        );
        if let Some(source_id) = preferred_source_id
            && let Some(Value::Array(sources)) = value.get_mut("MediaSources")
            && let Some(index) = sources
                .iter()
                .position(|source| source.get("Id").and_then(Value::as_str) == Some(source_id))
        {
            let source = sources.remove(index);
            sources.insert(0, source);
        }
        if fields.is_some_and(|fields| emby_fields_include(Some(fields), "People")) {
            let actors = match state.people.as_ref() {
                Some(people) => match people.list_item_actors(&item.id).await {
                    Ok(actors) => actors,
                    Err(error) => {
                        tracing::warn!(
                            item_id = %item.id,
                            %error,
                            "derived actor relation is unavailable for Emby list response"
                        );
                        Vec::new()
                    }
                },
                None => Vec::new(),
            };
            if let Value::Object(object) = &mut value {
                let mut people = actors
                    .into_iter()
                    .map(|actor| emby_person_json(actor, &state.server_id))
                    .collect::<Vec<_>>();
                if let Some(nfo) = nfo.as_ref() {
                    people.extend(emby_nfo_crew_json(nfo));
                }
                object.insert("People".to_owned(), Value::Array(people));
            }
        }
        items.push(value);
    }
    Ok(items)
}

async fn emby_user_items(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    emby_list_items(&headers, &state, principal, user.can_download, &query).await
}

async fn emby_items(
    headers: HeaderMap,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Some(user_id) = query.user_id.as_deref()
        && let Err(status) = ensure_emby_user_scope(&user, user_id)
    {
        return status.into_response();
    }
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    emby_list_items(&headers, &state, principal, user.can_download, &query).await
}

async fn emby_items_counts(
    headers: HeaderMap,
    Query(query): Query<EmbyItemCountsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Some(auth) = state.emby_auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    let (principal, target_user_id) = match query.user_id.as_deref() {
        Some(requested_id) => {
            if let Err(status) = ensure_emby_user_scope(&user, requested_id) {
                return status.into_response();
            }
            let target_user = match auth.user_by_id(requested_id).await {
                Ok(Some(target_user)) => target_user,
                Ok(None) => return StatusCode::NOT_FOUND.into_response(),
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
            if target_user.is_disabled {
                return StatusCode::NOT_FOUND.into_response();
            }
            let target_user_id = match requested_id.parse::<crate::domain::ids::UserId>() {
                Ok(target_user_id) => target_user_id,
                Err(_) => return StatusCode::BAD_REQUEST.into_response(),
            };
            (
                AccessPrincipal::new(target_user_id, target_user.is_admin),
                target_user.id.to_string(),
            )
        }
        None => (
            AccessPrincipal::new(user.id, user.is_admin),
            user.id.to_string(),
        ),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let counts = match catalog
        .count_item_types(principal, &target_user_id, query.is_favorite)
        .await
    {
        Ok(counts) => counts,
        Err(CatalogError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            return StatusCode::FORBIDDEN.into_response();
        }
    };

    Json(json!({
        "MovieCount": counts.movie_count,
        "SeriesCount": counts.series_count,
        "EpisodeCount": counts.episode_count,
        "GameCount": 0,
        "ArtistCount": 0,
        "ProgramCount": 0,
        "GameSystemCount": 0,
        "TrailerCount": 0,
        "SongCount": 0,
        "AlbumCount": 0,
        "MusicVideoCount": 0,
        "BoxSetCount": counts.box_set_count,
        "BookCount": 0,
        "ItemCount": counts.item_count,
    }))
    .into_response()
}

async fn emby_list_items(
    headers: &HeaderMap,
    state: &AppState,
    principal: AccessPrincipal,
    can_download: bool,
    query: &EmbyItemsQuery,
) -> Response {
    let root_id = principal.user_id.to_string();
    if emby_query_targets_user_root_views(query, &root_id) {
        let compatibility =
            emby_client_compatibility(headers, query.api_key.as_deref(), state).await;
        return match emby_visible_library_items(state, principal, compatibility).await {
            Ok(items) => Json(json!({
                "Items": items,
                "TotalRecordCount": items.len(),
                "StartIndex": 0,
            }))
            .into_response(),
            Err(status) => status.into_response(),
        };
    }
    match emby_catalog_page_from_query(state, principal, query).await {
        Ok(page) => {
            let preferred_source_id = emby_compat_media_source_id(query.ids.as_deref(), &page);
            emby_catalog_page_for_user_with_preferred_source(
                state,
                &principal.user_id.to_string(),
                &page,
                query.fields.as_deref(),
                can_download,
                preferred_source_id,
                emby_query_requests_series_children(state, principal, query).await,
            )
            .await
        }
        Err(status) => status.into_response(),
    }
}

async fn emby_query_requests_series_children(
    state: &AppState,
    principal: AccessPrincipal,
    query: &EmbyItemsQuery,
) -> bool {
    if query.include_item_types.as_deref().is_some_and(|types| {
        types.split(',').any(|item_type| {
            matches!(
                item_type.trim().to_ascii_lowercase().as_str(),
                "season" | "episode"
            )
        })
    }) {
        return true;
    }
    // Emby infers the child type from ParentId when IncludeItemTypes is omitted.
    // VidHub uses this compact form on the series detail screen.
    let Some(parent_id) = query.parent_id.as_deref() else {
        return false;
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return false;
    };
    matches!(
        catalog.find_item(principal, parent_id).await,
        Ok(Some(item)) if matches!(item.item_type.as_str(), "SERIES" | "SEASON")
    )
}

fn emby_query_targets_user_root_views(query: &EmbyItemsQuery, root_id: &str) -> bool {
    let parent_is_root = query.parent_id.as_deref() == Some(root_id);
    let requests_folder_views = query.include_item_types.as_deref().is_some_and(|types| {
        types.split(',').all(|item_type| {
            matches!(
                item_type.trim().to_ascii_lowercase().as_str(),
                "folder" | "collectionfolder"
            )
        })
    });
    let requests_filmly_home_views = query.parent_id.is_none()
        && query.include_item_types.is_none()
        && query.recursive != Some(true)
        && query.exclude_item_types.is_some();
    (parent_is_root && (query.include_item_types.is_none() || requests_folder_views))
        || (query.parent_id.is_none() && requests_folder_views)
        || requests_filmly_home_views
}

async fn emby_catalog_page_from_query(
    state: &AppState,
    principal: AccessPrincipal,
    query: &EmbyItemsQuery,
) -> Result<CatalogPage, StatusCode> {
    let (offset, limit) = match emby_page_params(query) {
        Ok(params) => params,
        Err(status) => return Err(status),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    if let Some(raw_query) = query.search_term.as_deref().map(str::trim)
        && !raw_query.is_empty()
    {
        let (Some(search_query), Some(like_query)) = (
            normalize_search_query(raw_query),
            normalize_search_like_query(raw_query),
        ) else {
            return Ok(CatalogPage {
                items: Vec::new(),
                total: 0,
                offset,
                limit,
            });
        };
        return catalog
            .search_items(principal, &search_query, &like_query, offset, limit)
            .await
            .map_err(emby_catalog_error_status);
    }
    let mut filter = catalog_filter_from_emby(query);
    let root_scope = match query.parent_id.as_deref() {
        Some(parent_id) => emby_parent_is_library(state, parent_id).await,
        None => true,
    };
    if root_scope && !query.recursive.unwrap_or(false) && query.include_item_types.is_none() {
        filter.item_types = vec!["MOVIE".to_owned(), "SERIES".to_owned()];
    }
    let page = match query.parent_id.as_deref() {
        Some(parent_id) => {
            if let Ok(library_id) = parent_id.parse::<crate::domain::ids::LibraryId>() {
                match catalog
                    .list_library_items_filtered(
                        principal,
                        &library_id.to_string(),
                        &filter,
                        offset,
                        limit,
                    )
                    .await
                {
                    Ok(page) => Ok(page),
                    Err(CatalogError::LibraryNotFound) => {
                        emby_catalog_page_for_item_parent(
                            catalog, principal, parent_id, query, offset, limit,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                }
            } else {
                emby_catalog_page_for_item_parent(
                    catalog, principal, parent_id, query, offset, limit,
                )
                .await
            }
        }
        None => {
            catalog
                .list_all_items_filtered(principal, &filter, offset, limit)
                .await
        }
    };
    match page {
        Ok(page) => Ok(page),
        Err(error) => Err(emby_catalog_error_status(error)),
    }
}

async fn emby_catalog_page_for_item_parent(
    catalog: &CatalogService,
    principal: AccessPrincipal,
    parent_id: &str,
    query: &EmbyItemsQuery,
    offset: i64,
    limit: i64,
) -> Result<CatalogPage, CatalogError> {
    let Some(parent) = catalog.find_item(principal, parent_id).await? else {
        return Err(CatalogError::LibraryNotFound);
    };
    let requested_types = catalog_filter_from_emby(query).item_types;
    let requested_type = requested_types.first().map(String::as_str);
    if parent.item_type == "SERIES"
        && requested_type == Some("EPISODE")
        && (query.recursive.unwrap_or(false) || query.group_items == Some(false))
    {
        return catalog
            .list_series_episodes(
                principal,
                parent_id,
                query.season_id.as_deref(),
                offset,
                limit,
            )
            .await;
    }
    let child_type = match (parent.item_type.as_str(), requested_type) {
        (_, Some(item_type)) => item_type,
        ("SERIES", _) => "SEASON",
        ("SEASON", _) => "EPISODE",
        _ => {
            return Ok(CatalogPage {
                items: Vec::new(),
                total: 0,
                offset,
                limit,
            });
        }
    };
    catalog
        .list_children(principal, parent_id, child_type, offset, limit)
        .await
}

fn emby_catalog_error_status(error: CatalogError) -> StatusCode {
    match error {
        CatalogError::LibraryNotFound => StatusCode::NOT_FOUND,
        CatalogError::AccessDenied => StatusCode::FORBIDDEN,
        CatalogError::Storage(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn emby_item(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let fields = emby_detail_fields(query.fields.as_deref());
    let compatibility = emby_client_compatibility(&headers, query.api_key.as_deref(), &state).await;
    emby_item_response(
        &state,
        principal,
        &item_id,
        user.can_download,
        fields.as_deref(),
        compatibility,
    )
    .await
}

async fn emby_user_item(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let fields = emby_detail_fields(query.fields.as_deref());
    let compatibility = emby_client_compatibility(&headers, query.api_key.as_deref(), &state).await;
    emby_item_response(
        &state,
        principal,
        &item_id,
        user.can_download,
        fields.as_deref(),
        compatibility,
    )
    .await
}

async fn emby_item_response(
    state: &AppState,
    principal: AccessPrincipal,
    item_id: &str,
    can_download: bool,
    fields: Option<&str>,
    compatibility: EmbyClientCompatibility,
) -> Response {
    if item_id == principal.user_id.to_string() {
        return emby_user_root_response(state, principal, compatibility).await;
    }
    if let Ok(library_id) = item_id.parse::<crate::domain::ids::LibraryId>()
        && let Some(libraries) = state.libraries.as_ref()
    {
        match libraries.get_library(library_id).await {
            Ok(library) => {
                if !library.is_enabled {
                    return StatusCode::NOT_FOUND.into_response();
                }
                let Some(access) = state.access.as_ref() else {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                };
                match access.can_view_library(principal, item_id).await {
                    Ok(true) => {}
                    Ok(false) => return StatusCode::NOT_FOUND.into_response(),
                    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                }
                let child_count =
                    match emby_library_root_count(state, principal, item_id, library.kind).await {
                        Ok(count) => count,
                        Err(status) => return status.into_response(),
                    };
                return Json(emby_library_view_json(
                    &library,
                    &state.server_id,
                    child_count,
                    compatibility,
                ))
                .into_response();
            }
            Err(LibraryServiceError::LibraryNotFound) => {}
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog.find_item(principal, item_id).await {
        Ok(Some(mut item)) => {
            let Some(database) = state.database.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            if catalog
                .populate_image_tags(std::slice::from_mut(&mut item))
                .await
                .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            let nfo = read_local_nfo_details(state, &item.id).await;
            let user_id = principal.user_id.to_string();
            let user_state = match database.find_user_item_state(&user_id, item_id).await {
                Ok(state) => state,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            let aspect_ratio = emby_primary_image_aspect_ratio(state, principal, &item.id).await;
            let mut item_json = emby_catalog_item_json_with_state_and_aspect_ratio(
                &item,
                &state.server_id,
                user_state.as_ref(),
                EmbyItemJsonOptions {
                    nfo: nfo.as_ref(),
                    can_download,
                    fields,
                    primary_image_aspect_ratio: aspect_ratio,
                    include_top_level_media_streams: true,
                },
            );
            let actors = match state.people.as_ref() {
                Some(people) => match people.list_item_actors(&item.id).await {
                    Ok(actors) => actors,
                    Err(error) => {
                        tracing::warn!(
                            item_id = %item.id,
                            %error,
                            "derived actor relation is unavailable; returning an empty cast"
                        );
                        Vec::new()
                    }
                },
                None => Vec::new(),
            };
            if let Value::Object(object) = &mut item_json {
                let mut people = actors
                    .into_iter()
                    .map(|actor| emby_person_json(actor, &state.server_id))
                    .collect::<Vec<_>>();
                if let Some(nfo) = nfo.as_ref() {
                    people.extend(emby_nfo_crew_json(nfo));
                }
                object.insert("People".to_owned(), Value::Array(people));
            }
            Json(item_json).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            unreachable!("inaccessible item is returned as not found")
        }
    }
}

async fn emby_person_image(
    headers: HeaderMap,
    method: Method,
    Path((person_id, image_type)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    emby_person_image_response(&headers, &method, &person_id, &image_type, &query, &state).await
}

async fn emby_person_image_at_index(
    headers: HeaderMap,
    method: Method,
    Path((person_id, image_type, image_index)): Path<(String, String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    if image_index.parse::<i64>().ok() != Some(0) {
        return StatusCode::NOT_FOUND.into_response();
    }
    emby_person_image_response(&headers, &method, &person_id, &image_type, &query, &state).await
}

async fn emby_person_image_response(
    headers: &HeaderMap,
    method: &Method,
    person_id: &str,
    image_type: &str,
    query: &EmbyTokenQuery,
    state: &AppState,
) -> Response {
    if normalize_image_type(image_type) != Some("POSTER") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Err(status) = require_emby_user(headers, state, query.api_key.as_deref()).await {
        return status.into_response();
    }
    let Some(people) = state.people.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let image = match people.profile_image_for_emby_name_or_id(person_id).await {
        Ok(Some(image)) => image,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(PeopleError::InvalidComponent(_)) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let etag = format!("\"{}\"", emby_person_image_tag(person_id));
    serve_image_file(
        &image.path,
        image.content_type,
        image.content_length,
        &etag,
        headers,
        method,
    )
    .await
}

fn emby_person_json(actor: crate::application::people::ActorView, server_id: &str) -> Value {
    emby_person_json_with_fields(actor, server_id, None)
}

fn emby_person_json_with_fields(
    actor: crate::application::people::ActorView,
    server_id: &str,
    fields: Option<&str>,
) -> Value {
    let image_tag = actor
        .image_url
        .as_ref()
        .map(|_| emby_person_image_tag(&actor.id));
    let include = |field| fields.is_none_or(|fields| emby_fields_include(Some(fields), field));
    let image_tags = image_tag
        .clone()
        .map(|tag| json!({"Primary": tag}))
        .unwrap_or_else(|| json!({}));
    let mut object = serde_json::Map::from_iter([
        ("Name".to_owned(), json!(actor.name)),
        ("ServerId".to_owned(), json!(server_id)),
        ("Id".to_owned(), json!(actor.id)),
        ("Type".to_owned(), json!("Person")),
        ("ImageTags".to_owned(), image_tags),
        ("BackdropImageTags".to_owned(), json!([])),
    ]);
    if include("Role")
        && let Some(role) = actor.character
    {
        object.insert("Role".to_owned(), json!(role));
    }
    if include("PrimaryImageTag")
        && let Some(image_tag) = image_tag
    {
        object.insert("PrimaryImageTag".to_owned(), json!(image_tag));
    }
    if include("Overview")
        && let Some(overview) = actor.biography
    {
        object.insert("Overview".to_owned(), json!(overview));
    }
    if include("BirthDate")
        && let Some(birthday) = actor.birthday
    {
        object.insert("BirthDate".to_owned(), json!(birthday));
    }
    if include("DeathDate")
        && let Some(deathday) = actor.deathday
    {
        object.insert("DeathDate".to_owned(), json!(deathday));
    }
    if include("KnownForDepartment")
        && let Some(known_for_department) = actor.known_for_department
    {
        object.insert("KnownForDepartment".to_owned(), json!(known_for_department));
    }
    if include("PlaceOfBirth")
        && let Some(place_of_birth) = actor.place_of_birth
    {
        object.insert("PlaceOfBirth".to_owned(), json!(place_of_birth));
    }
    if include("DateCreated")
        && let Some(date_created) = actor.date_created.and_then(emby_timestamp)
    {
        object.insert("DateCreated".to_owned(), Value::String(date_created));
    }
    Value::Object(object)
}

fn emby_stable_named_id(kind: &str, name: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"lux-emby:");
    digest.update(kind.as_bytes());
    digest.update(b":");
    digest.update(name.as_bytes());
    let suffix = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{kind}-{suffix}")
}

fn emby_nfo_crew_json(nfo: &LocalNfoDetails) -> Vec<Value> {
    let mut people = Vec::with_capacity(nfo.directors.len() + nfo.writers.len());
    for (person_type, credits) in [("Director", &nfo.directors), ("Writer", &nfo.writers)] {
        for credit in credits {
            let person = json!({
                "Name": credit.name,
                "Id": if credit.provider_id.is_empty() {
                    emby_stable_named_id(person_type, &credit.name)
                } else {
                    credit.provider_id.clone()
                },
                "Type": person_type,
            });
            people.push(person);
        }
    }
    people
}

async fn read_local_nfo_details(state: &AppState, item_id: &str) -> Option<LocalNfoDetails> {
    let store = state.local_nfo.as_ref()?;
    match store.read_item(item_id).await {
        Ok(details) => details,
        Err(error) => {
            tracing::warn!(
                item_id,
                %error,
                "derived local NFO cache is unavailable for Emby response"
            );
            None
        }
    }
}

fn emby_nfo_fields_requested(fields: Option<&str>) -> bool {
    const NFO_FIELDS: [&str; 16] = [
        "CommunityRating",
        "PremiereDate",
        "EndDate",
        "RunTimeTicks",
        "OriginalLanguage",
        "Status",
        "OfficialRating",
        "ProviderIds",
        "Taglines",
        "Genres",
        "GenreItems",
        "Studios",
        "RemoteTrailers",
        "ExternalUrls",
        "HomePageUrl",
        "People",
    ];
    fields.is_none_or(|fields| {
        NFO_FIELDS
            .iter()
            .any(|field| emby_fields_include(Some(fields), field))
    })
}

fn emby_person_image_tag(person_id: &str) -> String {
    Sha256::digest(person_id.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn emby_playback_info(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let query = emby_stream_query_from_raw(raw_query);
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let item = match catalog.find_item(principal, &item_id).await {
        Ok(Some(item)) => item,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(CatalogError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let mut sources = item.media_sources.iter().collect::<Vec<_>>();
    sources.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.id.cmp(&right.id))
    });
    if let Some(source_id) = query.media_source_id {
        let Some(index) = sources.iter().position(|source| source.id == source_id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let source = sources.remove(index);
        sources.insert(0, source);
    }
    let strm_resolver_available = match state.plugins.as_ref() {
        Some(plugins) => match plugins.has_available_strm_resolver().await {
            Ok(available) => available,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        None => false,
    };
    Json(json!({
        "PlaySessionId": Uuid::now_v7().to_string(),
        "MediaSources": sources
            .into_iter()
            .map(|source| {
                emby_media_source_json_with_resolver(
                    &item.id,
                    source,
                    true,
                    strm_resolver_available,
                )
            })
            .collect::<Vec<_>>(),
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
struct PlaybackEventRequest {
    #[serde(rename = "ItemId", alias = "itemId", alias = "mediaServerItemId")]
    item_id: String,
    #[serde(
        rename = "MediaSourceId",
        alias = "mediaSourceId",
        alias = "mediaServerMediaSourceId"
    )]
    media_source_id: Option<String>,
    #[serde(
        rename = "PlaySessionId",
        alias = "playSessionId",
        alias = "mediaServerPlaySessionId"
    )]
    play_session_id: Option<String>,
    #[serde(
        rename = "PositionTicks",
        alias = "positionTicks",
        alias = "PlaybackPositionTicks",
        alias = "playbackPositionTicks",
        default
    )]
    position_ticks: i64,
    #[serde(rename = "RunTimeTicks", alias = "runTimeTicks")]
    duration_ticks: Option<i64>,
    #[serde(rename = "IsPaused", alias = "isPaused", default)]
    is_paused: bool,
    #[serde(rename = "DeviceId", alias = "deviceId")]
    device_id: Option<String>,
    #[serde(rename = "Client", alias = "client")]
    client: Option<String>,
    #[serde(rename = "DeviceName", alias = "deviceName", alias = "Device")]
    device_name: Option<String>,
    #[serde(
        rename = "ApplicationVersion",
        alias = "applicationVersion",
        alias = "ClientVersion",
        alias = "clientVersion",
        alias = "Version"
    )]
    client_version: Option<String>,
    #[serde(rename = "DeviceType", alias = "deviceType")]
    device_type: Option<String>,
}

async fn emby_playing(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    Json(request): Json<PlaybackEventRequest>,
) -> Response {
    handle_emby_playback_event(headers, query, state, request, "PLAYING").await
}

async fn emby_playing_progress(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    Json(request): Json<PlaybackEventRequest>,
) -> Response {
    let state_name = if request.is_paused {
        "PAUSED"
    } else {
        "PLAYING"
    };
    handle_emby_playback_event(headers, query, state, request, state_name).await
}

async fn emby_playing_stopped(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    Json(request): Json<PlaybackEventRequest>,
) -> Response {
    handle_emby_playback_event(headers, query, state, request, "STOPPED").await
}

async fn handle_emby_playback_event(
    headers: HeaderMap,
    query: EmbyTokenQuery,
    state: AppState,
    request: PlaybackEventRequest,
    state_name: &'static str,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => {
            tracing::warn!(
                event = "emby_playback_callback_rejected",
                stage = "authentication",
                status_code = status.as_u16(),
                playback_state = state_name,
                "rejected emby playback callback"
            );
            return status.into_response();
        }
    };
    if request.position_ticks < 0
        || request.duration_ticks.is_some_and(|duration| duration < 0)
        || request.item_id.is_empty()
    {
        tracing::warn!(
            event = "emby_playback_callback_rejected",
            stage = "validation",
            status_code = StatusCode::BAD_REQUEST.as_u16(),
            playback_state = state_name,
            item_id_present = !request.item_id.is_empty(),
            position_ticks = request.position_ticks,
            duration_ticks_present = request.duration_ticks.is_some(),
            "rejected invalid emby playback callback"
        );
        return StatusCode::BAD_REQUEST.into_response();
    }
    let item_id_prefix = playback_identifier_prefix(&request.item_id);
    let Some(access) = state.access.as_ref() else {
        tracing::error!(
            event = "emby_playback_callback_rejected",
            stage = "access_service",
            status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            playback_state = state_name,
            item_id_prefix = %item_id_prefix,
            "playback access service is unavailable"
        );
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(
            AccessPrincipal::new(user.id, user.is_admin),
            &request.item_id,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(
                event = "emby_playback_callback_rejected",
                stage = "item_access",
                status_code = StatusCode::NOT_FOUND.as_u16(),
                playback_state = state_name,
                item_id_prefix = %item_id_prefix,
                "playback item is not accessible"
            );
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(error) => {
            tracing::error!(
                event = "emby_playback_callback_rejected",
                stage = "item_access",
                status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                playback_state = state_name,
                item_id_prefix = %item_id_prefix,
                error = %error,
                "failed to check playback item access"
            );
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    let Some(database) = state.database.as_ref() else {
        tracing::error!(
            event = "emby_playback_callback_rejected",
            stage = "database",
            status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            playback_state = state_name,
            item_id_prefix = %item_id_prefix,
            "playback database is unavailable"
        );
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let media_source_id = request
        .media_source_id
        .as_deref()
        .filter(|value| !value.is_empty());
    if let Some(media_source_id) = media_source_id {
        match database
            .media_source_belongs_to_item(media_source_id, &request.item_id)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(
                    event = "emby_playback_callback_rejected",
                    stage = "media_source",
                    status_code = StatusCode::NOT_FOUND.as_u16(),
                    playback_state = state_name,
                    item_id_prefix = %item_id_prefix,
                    "playback media source does not belong to item"
                );
                return StatusCode::NOT_FOUND.into_response();
            }
            Err(error) => {
                tracing::error!(
                    event = "emby_playback_callback_rejected",
                    stage = "media_source",
                    status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                    playback_state = state_name,
                    item_id_prefix = %item_id_prefix,
                    error = %error,
                    "failed to check playback media source"
                );
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        }
    }
    let mut header_device = emby_device_info_from_headers(&headers);
    if header_device.client.is_empty()
        || header_device.device.is_empty()
        || header_device.device_id.is_empty()
        || header_device.version.is_empty()
    {
        let token = emby_token_from_headers(&headers).or_else(|| query.api_key.clone());
        if let (Some(auth), Some(token)) = (state.emby_auth.as_ref(), token) {
            match auth.device_info(&token).await {
                Ok(Some(device)) => merge_emby_device_info(&mut header_device, device),
                Ok(None) => {}
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
    }
    let device_id = request
        .device_id
        .filter(|value| !value.is_empty())
        .or_else(|| (!header_device.device_id.is_empty()).then_some(header_device.device_id))
        .unwrap_or_else(|| "unknown".to_owned());
    let client = request
        .client
        .as_deref()
        .or_else(|| (!header_device.client.is_empty()).then_some(header_device.client.as_str()));
    let device_name = request
        .device_name
        .as_deref()
        .or_else(|| (!header_device.device.is_empty()).then_some(header_device.device.as_str()));
    let client_version = request
        .client_version
        .as_deref()
        .or_else(|| (!header_device.version.is_empty()).then_some(header_device.version.as_str()));
    let device_type = request
        .device_type
        .as_deref()
        .or_else(|| (!header_device.device.is_empty()).then_some(header_device.device.as_str()));
    let play_session_id = request
        .play_session_id
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{}:{device_id}", request.item_id));
    let user_id = user.id.to_string();
    let played_percent = match database.user_played_percent(&user_id).await {
        Ok(value) => value,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let previous_session = match database
        .find_playback_session(&user_id, &play_session_id)
        .await
    {
        Ok(session) => session,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let activity_event = playback_activity_event_type(previous_session.as_ref(), state_name);
    let occurred_at = current_unix_timestamp();
    let webhook_event = webhook_event_type_for_playback(
        activity_event,
        should_publish_playback_progress(
            previous_session.as_ref(),
            state_name,
            request.position_ticks,
            occurred_at,
        ),
    );
    let remote_ip = request_client_ip(&headers, &state.remote_access);
    match database
        .record_playback_event(NewPlaybackEvent {
            user_id: &user_id,
            item_id: &request.item_id,
            media_source_id,
            play_session_id: &play_session_id,
            device_id: &device_id,
            client,
            device_name,
            client_version,
            device_type,
            remote_ip: remote_ip.as_deref(),
            state: state_name,
            position_ticks: request.position_ticks,
            duration_ticks: request.duration_ticks,
            played_percent,
            is_paused: request.is_paused || state_name == "PAUSED",
        })
        .await
    {
        Ok(()) => {
            if database
                .sync_played_container_states(&user_id, &request.item_id)
                .await
                .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            if let Some(event_type) = activity_event {
                record_activity_event(
                    Some(database),
                    &state.admin_events,
                    &user_id,
                    event_type,
                    Some(&request.item_id),
                    json!({
                        "client": client,
                        "clientVersion": client_version,
                        "deviceName": device_name,
                        "deviceType": device_type,
                        "state": state_name,
                    }),
                )
                .await;
            }
            if let Some(event_type) = webhook_event {
                publish_playback_webhook(
                    &state,
                    event_type,
                    occurred_at,
                    &request.item_id,
                    media_source_id,
                    &play_session_id,
                    state_name,
                    request.position_ticks,
                    request.duration_ticks,
                    request.is_paused || state_name == "PAUSED",
                    client,
                    device_name,
                    device_type,
                    client_version,
                )
                .await;
            }
            tracing::info!(
                event = "emby_playback_callback_recorded",
                playback_state = state_name,
                item_id_prefix = %item_id_prefix,
                position_ticks = request.position_ticks,
                duration_ticks_present = request.duration_ticks.is_some(),
                is_paused = request.is_paused || state_name == "PAUSED",
                client = playback_client_label(client),
                "recorded emby playback callback"
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            tracing::error!(
                event = "emby_playback_callback_rejected",
                stage = "storage",
                status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                playback_state = state_name,
                item_id_prefix = %item_id_prefix,
                error = %error,
                "failed to record emby playback callback"
            );
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

fn playback_identifier_prefix(value: &str) -> String {
    value.chars().take(8).collect()
}

fn playback_client_label(value: Option<&str>) -> &'static str {
    match value.map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("vidhub") => "vidhub",
        Some(value) if value.eq_ignore_ascii_case("senplayer") => "senplayer",
        Some(value) if value.eq_ignore_ascii_case("infuse") => "infuse",
        Some(_) => "other",
        None => "unknown",
    }
}

const PLAYBACK_WEBHOOK_PROGRESS_INTERVAL_SECONDS: i64 = 30;

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

fn should_publish_playback_progress(
    previous: Option<&StoredPlaybackSession>,
    state_name: &str,
    position_ticks: i64,
    occurred_at: i64,
) -> bool {
    matches!(state_name, "PLAYING" | "PAUSED")
        && previous.is_some_and(|session| {
            occurred_at.saturating_sub(session.last_event_at)
                >= PLAYBACK_WEBHOOK_PROGRESS_INTERVAL_SECONDS
                && position_ticks > session.position_ticks
        })
}

fn webhook_event_type_for_playback(
    activity_event: Option<&str>,
    progress_due: bool,
) -> Option<WebhookEventType> {
    match activity_event {
        Some("PLAYBACK_STARTED") => Some(WebhookEventType::PlaybackStarted),
        Some("PLAYBACK_PAUSED") => Some(WebhookEventType::PlaybackPaused),
        Some("PLAYBACK_STOPPED") => Some(WebhookEventType::PlaybackStopped),
        _ if progress_due => Some(WebhookEventType::PlaybackProgress),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
async fn publish_playback_webhook(
    state: &AppState,
    event_type: WebhookEventType,
    occurred_at: i64,
    item_id: &str,
    media_source_id: Option<&str>,
    play_session_id: &str,
    state_name: &str,
    position_ticks: i64,
    duration_ticks: Option<i64>,
    is_paused: bool,
    client: Option<&str>,
    device_name: Option<&str>,
    device_type: Option<&str>,
    client_version: Option<&str>,
) {
    let Some(webhooks) = state.webhooks.as_ref() else {
        return;
    };
    let dedupe_key = format!(
        "playback:{play_session_id}:{}:{occurred_at}",
        event_type.as_str()
    );
    let data = json!({
        "itemId": item_id,
        "mediaSourceId": media_source_id,
        "playSessionId": play_session_id,
        "state": state_name,
        "positionTicks": position_ticks,
        "durationTicks": duration_ticks,
        "isPaused": is_paused,
        "client": bounded_playback_text(client),
        "deviceName": bounded_playback_text(device_name),
        "deviceType": bounded_playback_text(device_type),
        "clientVersion": bounded_playback_text(client_version),
    });
    if let Err(error) = webhooks
        .publish(event_type, &dedupe_key, occurred_at, data)
        .await
    {
        tracing::warn!(
            event = "playback_webhook_enqueue_failed",
            webhook_event_type = event_type.as_str(),
            error = %error,
            "failed to enqueue playback webhook"
        );
    }
}

fn bounded_playback_text(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (value.chars().count() <= 128).then(|| value.to_owned())
}

async fn emby_sessions(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let user_id = user.id.to_string();
    let sessions = match database
        .list_playback_sessions((!user.is_admin).then_some(user_id.as_str()))
        .await
    {
        Ok(sessions) => sessions,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    Json(sessions.iter().map(emby_session_json).collect::<Vec<_>>()).into_response()
}

fn emby_session_json(session: &crate::storage::StoredPlaybackSession) -> Value {
    json!({
        "Id": session.id,
        "UserId": session.user_id,
        "ItemId": session.item_id,
        "MediaSourceId": session.media_source_id,
        "PlaySessionId": session.play_session_id,
        "Client": session.client,
        "DeviceId": session.device_id,
        "DeviceName": session.device_name,
        "DeviceType": session.device_type,
        "ApplicationVersion": session.client_version,
        "RemoteEndPoint": session.remote_ip,
        "PlayState": {
            "PositionTicks": session.position_ticks,
            "IsPaused": session.is_paused,
            "CanSeek": true,
            "PlayMethod": "DirectPlay",
        },
        "RunTimeTicks": session.duration_ticks,
        "LastActivityDate": session.last_event_at,
    })
}

async fn lux_get_playback(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let user_id = user.id.to_string();
    let user_state = match database.find_user_item_state(&user_id, &item_id).await {
        Ok(state) => state,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let active_session = match database
        .find_active_playback_session(&user_id, &item_id)
        .await
    {
        Ok(session) => session,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    Json(json!({
        "itemId": item_id,
        "positionTicks": user_state.as_ref().map(|value| value.position_ticks).unwrap_or_default(),
        "isPlayed": user_state.as_ref().map(|value| value.is_played).unwrap_or(false),
        "isFavorite": user_state.as_ref().map(|value| value.is_favorite).unwrap_or(false),
        "playCount": user_state.as_ref().map(|value| value.play_count).unwrap_or_default(),
        "state": active_session.as_ref().map(|value| value.state.as_str()),
        "isPaused": active_session.as_ref().map(|value| value.is_paused).unwrap_or(false),
        "lastEventAt": active_session.as_ref().map(|value| value.last_event_at),
    }))
    .into_response()
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum LuxPlaybackState {
    #[default]
    Playing,
    Paused,
    Stopped,
}

impl LuxPlaybackState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Playing => "PLAYING",
            Self::Paused => "PAUSED",
            Self::Stopped => "STOPPED",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LuxProgressRequest {
    position_ticks: i64,
    duration_ticks: Option<i64>,
    #[serde(default)]
    state: LuxPlaybackState,
}

async fn lux_post_progress(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<LuxProgressRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    if request.position_ticks < 0 || request.duration_ticks.is_some_and(|duration| duration < 0) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let user_id = user.id.to_string();
    let play_session_id = format!("lux-web:{user_id}:{item_id}");
    let playback_state = request.state;
    let previous_session = match database
        .find_playback_session(&user_id, &play_session_id)
        .await
    {
        Ok(session) => session,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let activity_event =
        playback_activity_event_type(previous_session.as_ref(), playback_state.as_str());
    let occurred_at = current_unix_timestamp();
    let webhook_event = webhook_event_type_for_playback(
        activity_event,
        should_publish_playback_progress(
            previous_session.as_ref(),
            playback_state.as_str(),
            request.position_ticks,
            occurred_at,
        ),
    );
    let remote_ip = request_client_ip(&headers, &state.remote_access);
    match database
        .record_playback_event(NewPlaybackEvent {
            user_id: &user_id,
            item_id: &item_id,
            media_source_id: None,
            play_session_id: &play_session_id,
            device_id: "lux-web",
            client: Some("Lux"),
            device_name: Some("Web"),
            client_version: None,
            device_type: Some("Web"),
            remote_ip: remote_ip.as_deref(),
            state: playback_state.as_str(),
            position_ticks: request.position_ticks,
            duration_ticks: request.duration_ticks,
            played_percent: match database.user_played_percent(&user_id).await {
                Ok(value) => value,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            },
            is_paused: matches!(playback_state, LuxPlaybackState::Paused),
        })
        .await
    {
        Ok(()) => {
            if database
                .sync_played_container_states(&user_id, &item_id)
                .await
                .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            if let Some(event_type) = activity_event {
                record_activity_event(
                    Some(database),
                    &state.admin_events,
                    &user_id,
                    event_type,
                    Some(&item_id),
                    json!({
                        "client": "Lux",
                        "deviceType": "Web",
                        "deviceName": "Web",
                        "state": playback_state.as_str(),
                    }),
                )
                .await;
            }
            if let Some(event_type) = webhook_event {
                publish_playback_webhook(
                    &state,
                    event_type,
                    occurred_at,
                    &item_id,
                    None,
                    &play_session_id,
                    playback_state.as_str(),
                    request.position_ticks,
                    request.duration_ticks,
                    matches!(playback_state, LuxPlaybackState::Paused),
                    Some("Lux"),
                    Some("Web"),
                    Some("Web"),
                    None,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn emby_mark_played(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    handle_emby_user_flag(headers, user_id, item_id, query, state, true, true).await
}

async fn emby_unmark_played(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    handle_emby_user_flag(headers, user_id, item_id, query, state, true, false).await
}

async fn emby_mark_favorite(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    handle_emby_user_flag(headers, user_id, item_id, query, state, false, true).await
}

async fn emby_unmark_favorite(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    handle_emby_user_flag(headers, user_id, item_id, query, state, false, false).await
}

async fn handle_emby_user_flag(
    headers: HeaderMap,
    user_id: String,
    item_id: String,
    query: EmbyTokenQuery,
    state: AppState,
    played: bool,
    value: bool,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = if played {
        database
            .set_user_item_played(&user_id, &item_id, value)
            .await
    } else {
        database
            .set_user_item_favorite(&user_id, &item_id, value)
            .await
    };
    match result {
        Ok(()) => {
            if played
                && database
                    .sync_played_container_states(&user_id, &item_id)
                    .await
                    .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LuxFavoriteRequest {
    favorite: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LuxPlayedRequest {
    played: bool,
}

async fn lux_set_favorite(
    headers: HeaderMap,
    Path(item_id): Path<String>,
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
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .set_user_item_favorite(&user.id.to_string(), &item_id, request.favorite)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn lux_set_played(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<LuxPlayedRequest>,
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
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .set_user_item_played(&user.id.to_string(), &item_id, request.played)
        .await
    {
        Ok(()) => {
            if database
                .sync_played_container_states(&user.id.to_string(), &item_id)
                .await
                .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
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

fn normalize_filmly_null_languages(items: &mut [Value]) {
    for item in items {
        let Some(sources) = item.get_mut("MediaSources").and_then(Value::as_array_mut) else {
            continue;
        };
        for source in sources {
            let Some(streams) = source.get_mut("MediaStreams").and_then(Value::as_array_mut) else {
                continue;
            };
            for stream in streams {
                if let Some(object) = stream.as_object_mut()
                    && object.get("Language").is_some_and(Value::is_null)
                {
                    object.insert("Language".to_owned(), json!("und"));
                }
            }
        }
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
    if compact.len() != 32 || !compact.as_bytes().get(12).is_some_and(|byte| *byte == b'7') {
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
    let is_remote = source.source_kind == "STRM_URL"
        && source
            .external_url
            .as_deref()
            .is_some_and(is_http_strm_target);
    let is_resolver_target = strm_resolver_available
        && source.source_kind == "STRM_URL"
        && source
            .external_url
            .as_deref()
            .is_some_and(|target| !is_http_strm_target(target));
    let stream_suffix = source
        .container
        .as_deref()
        .filter(|container| {
            !(source.source_kind == "STRM_URL" && container.eq_ignore_ascii_case("strm"))
        })
        .map(|container| format!(".{container}"))
        .unwrap_or_default();
    let direct_stream_url = if source.source_kind == "LOCAL_FILE" || is_resolver_target {
        Some(format!(
            "/Videos/{item_id}/{}/stream{stream_suffix}",
            source.id
        ))
    } else {
        None
    };
    let is_remote_playback = is_remote || is_resolver_target;
    let is_playable = source.source_kind == "LOCAL_FILE" || is_remote_playback;
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
        "AddApiKeyToDirectStreamUrl": false,
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
    let mut value = json!({
        "Index": stream.index,
        "Type": emby_stream_type(&stream.stream_type),
        "Codec": stream.codec,
        "Language": stream.language,
        "DisplayTitle": stream.title,
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

fn emby_library_view_json(
    library: &LibraryRecord,
    server_id: &str,
    child_count: i64,
    compatibility: EmbyClientCompatibility,
) -> Value {
    json!({
        "Name": library.name,
        "SortName": library.name,
        "Id": library.id,
        "ServerId": server_id,
        "Type": "CollectionFolder",
        "IsFolder": true,
        "MediaType": "Video",
        "CollectionType": emby_collection_type(library.kind, compatibility),
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
    compatibility: EmbyClientCompatibility,
) -> Value {
    let media_strategy = view
        .library
        .media_strategy_json
        .as_deref()
        .and_then(|value| serde_json::from_str::<MediaStrategySettings>(value).ok())
        .unwrap_or_else(|| global_media_strategy.clone());
    let collection_type = emby_collection_type(view.library.kind, compatibility);
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
            compatibility,
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
    compatibility: EmbyClientCompatibility,
) -> Value {
    let collection_type = emby_collection_type(view.library.kind, compatibility);
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

fn emby_collection_type(
    kind: LibraryKind,
    compatibility: EmbyClientCompatibility,
) -> Option<&'static str> {
    match kind {
        LibraryKind::Movie => Some("movies"),
        LibraryKind::Series => Some("tvshows"),
        LibraryKind::Mixed => match compatibility {
            EmbyClientCompatibility::Generic => Some("mixed"),
            EmbyClientCompatibility::VidHub => None,
        },
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

fn emby_user_json(user: &UserRecord, server_id: &str, server_name: &str) -> Value {
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
        "Configuration": emby_user_configuration_json(),
        "Policy": emby_user_policy_json(user),
    })
}

fn emby_user_configuration_json() -> Value {
    json!({
        "AudioLanguagePreference": "",
        "PlayDefaultAudioTrack": true,
        "SubtitleLanguagePreference": "",
        "DisplayMissingEpisodes": false,
        "GroupedFolders": [],
        "SubtitleMode": "Default",
        "DisplayCollectionsView": true,
        "EnableLocalPassword": false,
        "OrderedViews": [],
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

async fn live() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn ready(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let Some(database) = state.database else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
        );
    };
    let config_available = match state.config_dir.as_deref() {
        Some(path) => fs::metadata(path)
            .await
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false),
        None => false,
    };

    if !config_available {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "config_unavailable" })),
        );
    }

    let schema_version = match database.schema_version().await {
        Ok(schema_version) => schema_version,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
            );
        }
    };
    if database.probe_write().await.is_err() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "not_ready",
                "reason": "database_write_unavailable",
                "schemaVersion": schema_version,
                "databaseWritable": false,
            })),
        );
    }
    (
        StatusCode::OK,
        Json(json!({
            "status": "ready",
            "schemaVersion": schema_version,
            "databaseWritable": true,
        })),
    )
}

async fn version(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let Some(database) = state.database else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
        );
    };

    match database.schema_version().await {
        Ok(schema_version) => (
            StatusCode::OK,
            Json(json!({
                "luxVersion": VERSION,
                "commit": COMMIT,
                "schemaVersion": schema_version
            })),
        ),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
        ),
    }
}

async fn setup_status(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let Some(setup) = state.setup.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
        );
    };

    match setup.status().await {
        Ok(initialized) => (StatusCode::OK, Json(json!({ "initialized": initialized }))),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
        ),
    }
}

async fn setup_database_status(State(state): State<AppState>) -> Response {
    let Some(database_setup) = state.database_setup.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
        )
            .into_response();
    };

    match database_setup.status().await {
        Ok(status) => (
            StatusCode::OK,
            Json(json!({
                "configured": status.configured,
                "backend": status.backend,
                "currentBackend": status.current_backend,
                "restartRequired": status.restart_required
            })),
        )
            .into_response(),
        Err(error) => database_setup_error(&HeaderMap::new(), error),
    }
}

#[derive(Deserialize)]
#[serde(tag = "backend", rename_all = "SCREAMING_SNAKE_CASE")]
enum SetupDatabaseRequest {
    #[serde(rename = "SQLITE")]
    Sqlite,
    #[serde(rename = "POSTGRESQL")]
    Postgres {
        host: String,
        port: u16,
        database: String,
        username: String,
        password: String,
        #[serde(default = "default_postgres_ssl_mode")]
        ssl_mode: String,
    },
}

fn default_postgres_ssl_mode() -> String {
    "prefer".to_owned()
}

impl SetupDatabaseRequest {
    fn into_configuration(self) -> DatabaseConfiguration {
        match self {
            Self::Sqlite => DatabaseConfiguration::Sqlite,
            Self::Postgres {
                host,
                port,
                database,
                username,
                password,
                ssl_mode,
            } => DatabaseConfiguration::Postgres(PostgresConnection {
                host,
                port,
                database,
                username,
                password,
                ssl_mode,
            }),
        }
    }
}

async fn setup_database_test(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<SetupDatabaseRequest>,
) -> Response {
    let Some(setup) = state.setup.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match setup.status().await {
        Ok(false) => {}
        Ok(true) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::SetupAlreadyCompleted,
                "初始设置已经完成",
            )
            .into_response();
        }
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "数据库不可用",
            )
            .into_response();
        }
    }
    let Some(database_setup) = state.database_setup.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let configuration = request.into_configuration();
    match database_setup.test(&configuration).await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "backend": configuration.backend() })),
        )
            .into_response(),
        Err(error) => database_setup_error(&headers, error),
    }
}

async fn setup_database_select(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<SetupDatabaseRequest>,
) -> Response {
    let Some(setup) = state.setup.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match setup.status().await {
        Ok(true) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::SetupAlreadyCompleted,
                "初始设置已经完成",
            )
            .into_response();
        }
        Ok(false) => {}
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "数据库不可用",
            )
            .into_response();
        }
    }
    let Some(database_setup) = state.database_setup.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let configuration = request.into_configuration();
    match database_setup.select(&configuration).await {
        Ok(result) => (
            StatusCode::OK,
            Json(json!({
                "selected": true,
                "backend": result.backend,
                "restartRequired": result.restart_required
            })),
        )
            .into_response(),
        Err(error) => database_setup_error(&headers, error),
    }
}

fn database_setup_error(headers: &HeaderMap, error: DatabaseSetupError) -> Response {
    let (status, code, message) = match error {
        DatabaseSetupError::Configuration(_) => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "数据库配置无效",
        ),
        DatabaseSetupError::Storage(_) => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::DatabaseConnectionFailed,
            "无法连接数据库，请检查地址、端口、用户名和密码",
        ),
    };
    api_error(headers, status, code, message).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetupCompleteRequest {
    username: String,
    #[serde(default)]
    display_name: String,
    password: String,
    #[serde(default)]
    first_library: Option<SetupFirstLibraryRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetupFirstLibraryRequest {
    name: String,
    #[serde(default = "default_setup_library_kind")]
    kind: String,
    #[serde(default)]
    realtime_watch_enabled: bool,
    #[serde(default)]
    root_path: Option<String>,
}

fn default_setup_library_kind() -> String {
    "MIXED".to_owned()
}

async fn setup_complete(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<SetupCompleteRequest>,
) -> Response {
    let Some(setup) = state.setup.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };

    if state.database_selection_required {
        let Some(database_setup) = state.database_setup.as_ref() else {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "服务尚未就绪",
            )
            .into_response();
        };
        match database_setup.status().await {
            Ok(status) if !status.configured => {
                return api_error(
                    &headers,
                    StatusCode::CONFLICT,
                    lux::ApiErrorCode::DatabaseConfigurationRequired,
                    "请先选择数据库",
                )
                .into_response();
            }
            Ok(status) if status.restart_required => {
                return api_error(
                    &headers,
                    StatusCode::CONFLICT,
                    lux::ApiErrorCode::DatabaseRestartRequired,
                    "数据库配置已保存，请重启 Lux 后继续",
                )
                .into_response();
            }
            Ok(_) => {}
            Err(_) => {
                return api_error(
                    &headers,
                    StatusCode::SERVICE_UNAVAILABLE,
                    lux::ApiErrorCode::DatabaseUnavailable,
                    "数据库不可用",
                )
                .into_response();
            }
        }
    }

    let setup_kind = match request
        .first_library
        .as_ref()
        .map(|library| library.kind.parse::<LibraryKind>())
        .transpose()
    {
        Ok(kind) => kind,
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库类型无效",
            )
            .into_response();
        }
    };
    if let Some(first_library) = request.first_library.as_ref() {
        if first_library.name.trim().is_empty() || first_library.name.chars().count() > 128 {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库名称无效",
            )
            .into_response();
        }
        if let Some(root_path) = first_library.root_path.as_deref() {
            if let Err(error) = crate::library::inspect_root_path(FsPath::new(root_path)).await {
                return library_error(&headers, LibraryServiceError::from(error));
            }
        }
    }

    match setup
        .complete(&request.username, &request.display_name, &request.password)
        .await
    {
        Ok(user) => {
            let mut library_json_value = None;
            if let (Some(first_library), Some(kind), Some(libraries)) =
                (request.first_library, setup_kind, state.libraries.as_ref())
            {
                let library = match libraries
                    .create_library_with_scraper(
                        &first_library.name,
                        kind,
                        first_library.realtime_watch_enabled,
                        None,
                        true,
                    )
                    .await
                {
                    Ok(library) => library,
                    Err(error) => return library_error(&headers, error),
                };
                let mut roots = Vec::new();
                let mut warnings = Vec::new();
                if let Some(root_path) = first_library.root_path {
                    match libraries.add_root(library.id, &root_path).await {
                        Ok(result) => {
                            roots.push(result.root);
                            warnings = result
                                .warnings
                                .iter()
                                .map(|warning| warning.as_str())
                                .collect::<Vec<_>>();
                        }
                        Err(error) => return library_error(&headers, error),
                    }
                }
                let scan_job = match spawn_library_scan(&state, library.id).await {
                    Ok(job) => job,
                    Err(error) => {
                        tracing::warn!(library_id = %library.id, %error, "initial library scan could not be started");
                        None
                    }
                };
                library_json_value = Some(json!({
                    "library": library_json(&library, &roots),
                    "warnings": warnings,
                    "scanJob": scan_job.as_ref().map(scan_job_json),
                }));
            }
            let mut response = json!({
                "initialized": true,
                "user": user_json(&user),
            });
            if let Some(library) = library_json_value {
                response["library"] = library["library"].clone();
                response["warnings"] = library["warnings"].clone();
                response["scanJob"] = library["scanJob"].clone();
            }
            (StatusCode::CREATED, Json(response)).into_response()
        }
        Err(SetupError::AlreadyCompleted) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::SetupAlreadyCompleted,
            "初始化已完成",
        )
        .into_response(),
        Err(SetupError::UserStore(
            UserStoreError::InvalidUsername | UserStoreError::Password(_),
        )) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户名或密码无效",
        )
        .into_response(),
        Err(SetupError::UserStore(_)) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "初始化暂时不可用",
        )
        .into_response(),
    }
}

#[derive(Deserialize)]
struct AuthLoginRequest {
    username: String,
    password: String,
}

async fn auth_login(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<AuthLoginRequest>,
) -> Response {
    let Some(auth) = state.auth.clone() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };

    let login_key = login_attempt_key(&headers, &request.username);
    if !state.login_rate_limiter.is_allowed(&login_key).await {
        return api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::InvalidCredentials,
            "用户名或密码错误",
        )
        .into_response();
    }
    let session = match auth.login(&request.username, &request.password).await {
        Ok(Some(session)) => {
            state.login_rate_limiter.record_success(&login_key).await;
            session
        }
        Ok(None) => {
            state.login_rate_limiter.record_failure(&login_key).await;
            return api_error(
                &headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::InvalidCredentials,
                "用户名或密码错误",
            )
            .into_response();
        }
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "登录暂时不可用",
            )
            .into_response();
        }
    };
    if state.remote_access.is_remote(
        header_str(&headers, "x-lux-peer-ip"),
        header_str(&headers, "x-forwarded-for"),
    ) && !session.user.can_remote_access
    {
        let _ = auth.logout(&session.session_token).await;
        return api_error(
            &headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "当前账户不允许远程访问",
        )
        .into_response();
    }

    let user_id = session.user.id.to_string();
    record_activity_event(
        state.database.as_ref(),
        &state.admin_events,
        &user_id,
        "AUTH_LOGIN",
        None,
        json!({}),
    )
    .await;

    let mut response_headers = HeaderMap::new();
    let secure_cookie = secure_cookie_for_request(&headers, &state.remote_access);
    let Some(session_cookie) = build_cookie(
        "lux_session",
        &session.session_token,
        true,
        None,
        secure_cookie,
    ) else {
        return api_error(
            &headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "无法创建会话",
        )
        .into_response();
    };
    let Some(csrf_cookie) =
        build_cookie("lux_csrf", &session.csrf_token, false, None, secure_cookie)
    else {
        return api_error(
            &headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "无法创建会话",
        )
        .into_response();
    };
    response_headers.append(SET_COOKIE, session_cookie);
    response_headers.append(SET_COOKIE, csrf_cookie);
    (
        StatusCode::OK,
        response_headers,
        Json(json!({ "user": user_json(&session.user), "csrfToken": session.csrf_token })),
    )
        .into_response()
}

async fn auth_me(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let server_name = current_emby_server_name(&state).await;
    Json(json!({
        "user": user_json(&user),
        "serverName": server_name,
    }))
    .into_response()
}

async fn auth_settings(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.user_played_percent(&user.id.to_string()).await {
        Ok(played_percent) => Json(json!({ "playedPercent": played_percent })).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthSettingsPatch {
    played_percent: i64,
}

async fn auth_update_settings(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<AuthSettingsPatch>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    if !(1..=100).contains(&request.played_percent) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "自动标记已看百分比必须在 1 到 100 之间",
        )
        .into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .set_user_played_percent(&user.id.to_string(), request.played_percent)
        .await
    {
        Ok(()) => Json(json!({ "playedPercent": request.played_percent })).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn auth_avatar(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(avatars) = state.user_avatars.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match avatars.load(user.id).await {
        Ok(Some(avatar)) => match Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, avatar.content_type)
            .header(CACHE_CONTROL, "private, no-cache")
            .body(Body::from(avatar.bytes))
        {
            Ok(response) => response,
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => user_avatar_error(&headers, error),
    }
}

async fn auth_update_avatar(
    headers: HeaderMap,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(avatars) = state.user_avatars.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    match avatars.store(user.id, content_type, &body).await {
        Ok(()) => Json(json!({
            "avatarUrl": "/api/v1/auth/avatar",
        }))
        .into_response(),
        Err(error) => user_avatar_error(&headers, error),
    }
}

async fn auth_sessions(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let Some(auth) = state.auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(session_token) = request_cookie(&headers, "lux_session") else {
        return api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response();
    };
    let session = match auth.resolve(&session_token).await {
        Ok(Some(session)) => session,
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要登录",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match auth.list_sessions(&session.user.id, &session_token).await {
        Ok(sessions) => Json(json!({
            "sessions": sessions.iter().map(|session| json!({
                "id": session.id,
                "createdAt": session.created_at,
                "updatedAt": session.updated_at,
                "expiresAt": session.expires_at,
                "lastSeenAt": session.last_seen_at,
                "isCurrent": session.is_current,
            })).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn auth_revoke_session(
    headers: HeaderMap,
    Path(session_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let Some(auth) = state.auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(session_token) = request_cookie(&headers, "lux_session") else {
        return api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response();
    };
    let session = match auth.resolve(&session_token).await {
        Ok(Some(session)) => session,
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要登录",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(csrf_token) = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
    else {
        return api_error(
            &headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::CsrfFailed,
            "CSRF 校验失败",
        )
        .into_response();
    };
    if !auth.verify_csrf(&session, csrf_token) {
        return api_error(
            &headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::CsrfFailed,
            "CSRF 校验失败",
        )
        .into_response();
    }
    let sessions = match auth.list_sessions(&session.user.id, &session_token).await {
        Ok(sessions) => sessions,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if sessions
        .iter()
        .any(|entry| entry.id == session_id && entry.is_current)
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "不能撤销当前会话，请使用退出登录",
        )
        .into_response();
    }
    match auth.revoke_session(&session.user.id, &session_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "会话不存在",
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn auth_logout(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let Some(auth) = state.auth else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let Some(session_token) = request_cookie(&headers, "lux_session") else {
        return api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response();
    };
    let session = match auth.resolve(&session_token).await {
        Ok(Some(session)) => session,
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要登录",
            )
            .into_response();
        }
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "认证暂时不可用",
            )
            .into_response();
        }
    };
    let Some(csrf_token) = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
    else {
        return api_error(
            &headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::CsrfFailed,
            "CSRF 校验失败",
        )
        .into_response();
    };
    if !auth.verify_csrf(&session, csrf_token) {
        return api_error(
            &headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::CsrfFailed,
            "CSRF 校验失败",
        )
        .into_response();
    }
    if auth.logout(&session_token).await.is_err() {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "退出登录暂时不可用",
        )
        .into_response();
    }

    let mut response_headers = HeaderMap::new();
    let secure_cookie = secure_cookie_for_request(&headers, &state.remote_access);
    if let Some(cookie) = build_cookie("lux_session", "", true, Some(0), secure_cookie) {
        response_headers.append(SET_COOKIE, cookie);
    }
    if let Some(cookie) = build_cookie("lux_csrf", "", false, Some(0), secure_cookie) {
        response_headers.append(SET_COOKIE, cookie);
    }
    (StatusCode::NO_CONTENT, response_headers).into_response()
}

#[derive(Deserialize, Default)]
struct LuxPageQuery {
    #[serde(default)]
    page: Option<i64>,
    #[serde(rename = "pageSize", default)]
    page_size: Option<i64>,
    #[serde(rename = "itemType", default)]
    item_type: Option<String>,
    #[serde(default)]
    year: Option<i64>,
    #[serde(default)]
    is_played: Option<bool>,
    #[serde(default)]
    is_favorite: Option<bool>,
    #[serde(rename = "sort_by", alias = "sortBy", default)]
    sort_by: Option<String>,
    #[serde(rename = "sort_order", alias = "sortOrder", default)]
    sort_order: Option<String>,
    #[serde(rename = "metadataStatus", default)]
    metadata_status: Option<String>,
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

async fn require_web_user(headers: &HeaderMap, state: &AppState) -> Result<UserRecord, Response> {
    if lux_api_key_from_headers(headers).is_some() {
        let user = resolve_shared_admin_api_key(headers, state).await?;
        let Some(user) = user else {
            return Err(api_error(
                headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要有效的 API Key",
            )
            .into_response());
        };
        if state.remote_access.is_remote(
            header_str(headers, "x-lux-peer-ip"),
            header_str(headers, "x-forwarded-for"),
        ) && !user.can_remote_access
        {
            return Err(api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::PermissionDenied,
                "当前管理员不允许远程访问",
            )
            .into_response());
        }
        return Ok(user);
    }
    let Some(auth) = state.auth.as_ref() else {
        return Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response());
    };
    let Some(session_token) = request_cookie(headers, "lux_session") else {
        return Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response());
    };
    match auth.resolve(&session_token).await {
        Ok(Some(session)) => {
            if state.remote_access.is_remote(
                header_str(headers, "x-lux-peer-ip"),
                header_str(headers, "x-forwarded-for"),
            ) && !session.user.can_remote_access
            {
                return Err(api_error(
                    headers,
                    StatusCode::FORBIDDEN,
                    lux::ApiErrorCode::PermissionDenied,
                    "当前账户不允许远程访问",
                )
                .into_response());
            }
            Ok(session.user)
        }
        Ok(None) => Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response()),
        Err(_) => Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "认证暂时不可用",
        )
        .into_response()),
    }
}

async fn require_web_csrf(headers: &HeaderMap, state: &AppState) -> Result<(), Response> {
    if lux_api_key_from_headers(headers).is_some() {
        let user = resolve_shared_admin_api_key(headers, state).await?;
        if user.is_some() {
            return Ok(());
        }
        return Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要有效的 API Key",
        )
        .into_response());
    }
    let Some(auth) = state.auth.as_ref() else {
        return Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response());
    };
    let Some(session_token) = request_cookie(headers, "lux_session") else {
        return Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response());
    };
    let session = match auth.resolve(&session_token).await {
        Ok(Some(session)) => session,
        Ok(None) => {
            return Err(api_error(
                headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要登录",
            )
            .into_response());
        }
        Err(_) => {
            return Err(api_error(
                headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "认证暂时不可用",
            )
            .into_response());
        }
    };
    let Some(csrf_token) = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
    else {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::CsrfFailed,
            "CSRF 校验失败",
        )
        .into_response());
    };
    if !auth.verify_csrf(&session, csrf_token) {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::CsrfFailed,
            "CSRF 校验失败",
        )
        .into_response());
    }
    Ok(())
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LuxSearchQuery {
    #[serde(alias = "query")]
    q: Option<String>,
    page: Option<i64>,
    page_size: Option<i64>,
}

async fn lux_search(
    headers: HeaderMap,
    Query(query): Query<LuxSearchQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(raw_query) = query.q.as_deref() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(search_query) = normalize_search_query(raw_query) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(like_query) = normalize_search_like_query(raw_query) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog
        .search_items(
            AccessPrincipal::new(user.id, user.is_admin),
            &search_query,
            &like_query,
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
            StatusCode::FORBIDDEN.into_response()
        }
    }
}

async fn lux_home(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(home) = state.home.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let user_id = user.id.to_string();
    let accessible_library_ids = match access.accessible_library_ids(principal).await {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let snapshot = match home
        .snapshot(principal, accessible_library_ids.clone())
        .await
    {
        Ok(value) => value,
        Err(HomeError::Catalog(_) | HomeError::Libraries(_)) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let accessible_library_ids = accessible_library_ids.into_iter().collect::<HashSet<_>>();
    let latest_groups = snapshot
        .latest_groups
        .iter()
        .filter(|(library_id, _)| accessible_library_ids.contains(library_id))
        .cloned()
        .collect::<Vec<_>>();
    let latest_items = latest_groups
        .iter()
        .flat_map(|(_, items)| items.iter().cloned())
        .collect::<Vec<_>>();
    let all_items = snapshot
        .continue_watching
        .items
        .iter()
        .chain(snapshot.recently_added.items.iter())
        .chain(snapshot.recommended.iter())
        .chain(latest_items.iter())
        .cloned()
        .collect::<Vec<_>>();
    let user_values = match lux_catalog_item_values_by_id(database, &user_id, &all_items).await {
        Ok(values) => values,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let continue_watching_items =
        lux_catalog_items_from_values(&snapshot.continue_watching.items, &user_values);
    let recently_added_items =
        lux_catalog_items_from_values(&snapshot.recently_added.items, &user_values);
    let recommended_items = lux_catalog_items_from_values(&snapshot.recommended, &user_values);
    let latest_values = lux_catalog_items_from_values(&latest_items, &user_values);
    let mut latest_by_library = BTreeMap::<String, Vec<Value>>::new();
    for (item, value) in latest_items.iter().zip(latest_values) {
        latest_by_library
            .entry(item.library_id.clone())
            .or_default()
            .push(value);
    }
    let mut visible = Vec::new();
    for view in &snapshot.views {
        let library_id = view.library.id.to_string();
        if !accessible_library_ids.contains(&library_id) {
            continue;
        }
        visible.push(json!({
            "id": view.library.id,
            "name": view.library.name,
            "kind": view.library.kind.as_str(),
            "coverImageUrl": library_cover_url(&view.library),
            "latest": latest_by_library
                .get(&library_id)
                .cloned()
                .unwrap_or_default(),
        }));
    }
    Json(json!({
        "continueWatching": continue_watching_items,
        "continueWatchingTotal": snapshot.continue_watching.total,
        "recentlyAdded": recently_added_items,
        "recentlyAddedTotal": snapshot.recently_added.total,
        "recommended": recommended_items,
        "libraries": visible,
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
struct EmbySearchQuery {
    #[serde(
        rename = "api_key",
        alias = "apiKey",
        alias = "ApiKey",
        alias = "X-Emby-Token",
        alias = "x-emby-token",
        alias = "X-MediaBrowser-Token",
        alias = "x-media-browser-token"
    )]
    api_key: Option<String>,
    #[serde(rename = "SearchTerm", alias = "searchTerm")]
    search_term: Option<String>,
    #[serde(rename = "StartIndex", alias = "startIndex")]
    start_index: Option<i64>,
    #[serde(rename = "Limit", alias = "limit")]
    limit: Option<i64>,
}

async fn emby_search_hints(
    headers: HeaderMap,
    Query(query): Query<EmbySearchQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Some(raw_query) = query.search_term.as_deref() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(search_query) = normalize_search_query(raw_query) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(like_query) = normalize_search_like_query(raw_query) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let page_query = EmbyItemsQuery {
        start_index: query.start_index,
        limit: query.limit,
        ..EmbyItemsQuery::default()
    };
    let (offset, limit) = match emby_page_params(&page_query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let page = match catalog
        .search_items(
            AccessPrincipal::new(user.id, user.is_admin),
            &search_query,
            &like_query,
            offset,
            limit,
        )
        .await
    {
        Ok(page) => page,
        Err(CatalogError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            return StatusCode::FORBIDDEN.into_response();
        }
    };
    let hints = page
        .items
        .iter()
        .map(|item| {
            json!({
                "Id": item.id,
                "Name": item.title,
                "Type": emby_item_type(&item.item_type),
                "MediaType": "Video",
                "ProductionYear": item.production_year,
                "RunTimeTicks": item.runtime_ticks,
            })
        })
        .collect::<Vec<_>>();
    Json(json!({
        "SearchHints": hints,
        "TotalRecordCount": page.total,
    }))
    .into_response()
}

async fn lux_list_libraries(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let show_metadata_pending = match read_media_strategy_settings(database).await {
        Ok(settings) => settings.show_metadata_pending,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    match libraries.list_libraries().await {
        Ok(views) => {
            let mut visible = Vec::new();
            for view in views {
                let can_view = match access
                    .can_view_library(principal, &view.library.id.to_string())
                    .await
                {
                    Ok(can_view) => can_view,
                    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                };
                if view.library.is_enabled && can_view {
                    visible.push(json!({
                        "id": view.library.id.to_string(),
                        "name": view.library.name,
                        "kind": view.library.kind.as_str(),
                        "coverImageUrl": library_cover_url(&view.library),
                    }));
                }
            }
            Json(json!({
                "libraries": visible,
                "showMetadataPending": show_metadata_pending,
            }))
            .into_response()
        }
        Err(error) => library_error(&headers, error),
    }
}

async fn lux_library_cover(
    headers: HeaderMap,
    method: Method,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    match access
        .can_view_library(principal, &library_id.to_string())
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(covers) = state.library_covers.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let cover = match covers.resolve(library_id).await {
        Ok(Some(cover)) => cover,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(LibraryCoverError::LibraryNotFound | LibraryCoverError::InvalidPath) => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(LibraryCoverError::Storage(_)) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(
            LibraryCoverError::Io { .. }
            | LibraryCoverError::ImageWrite(_)
            | LibraryCoverError::FontNotFound
            | LibraryCoverError::Render(_)
            | LibraryCoverError::RenderPanicked
            | LibraryCoverError::GeneratedCoverRace
            | LibraryCoverError::GenerationUnavailable,
        ) => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(
            LibraryCoverError::UnsupportedContentType(_)
            | LibraryCoverError::InvalidContent { .. }
            | LibraryCoverError::TooLarge { .. },
        ) => return StatusCode::NOT_FOUND.into_response(),
    };
    if headers
        .get("if-none-match")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|tag| tag.trim() == cover.etag))
    {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header("ETag", &cover.etag)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let Ok(file) = tokio::fs::File::open(&cover.path).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        Body::from_stream(tokio_util::io::ReaderStream::new(file))
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", &cover.content_type)
        .header("Content-Length", cover.content_length)
        .header("ETag", &cover.etag)
        .header("Cache-Control", "private, max-age=3600")
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn lux_list_favorites(
    headers: HeaderMap,
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
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let filter = CatalogFilter {
        is_favorite: Some(true),
        sort_by: CatalogSort::DateCreated,
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
            StatusCode::FORBIDDEN.into_response()
        }
    }
}

async fn lux_list_library_items(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id.to_string(),
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库 ID 无效",
            )
            .into_response();
        }
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
    let Some(catalog) = state.catalog.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let metadata_pending = match query.metadata_status.as_deref() {
        None => false,
        Some(value) if value.eq_ignore_ascii_case("PENDING") => true,
        Some(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "元数据状态无效",
            )
            .into_response();
        }
    };
    let filter = catalog_filter_from_values(
        query.item_type.as_deref(),
        query.year.map(|year| year.to_string()).as_deref(),
        query.is_played,
        query.is_favorite,
        query.sort_by.as_deref(),
        query.sort_order.as_deref(),
        metadata_pending,
    );
    match catalog
        .list_library_items_filtered(principal, &library_id, &filter, offset, limit)
        .await
    {
        Ok(page) => {
            match lux_catalog_page_json_for_user(database, &user.id.to_string(), &page).await {
                Ok(body) => Json(body).into_response(),
                Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
        Err(CatalogError::LibraryNotFound) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体库不存在",
        )
        .into_response(),
        Err(CatalogError::AccessDenied) => api_error(
            &headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "没有媒体库访问权限",
        )
        .into_response(),
        Err(CatalogError::Storage(_)) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        )
        .into_response(),
    }
}

async fn lux_get_item(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let Some(catalog) = state.catalog.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog.find_item(principal, &item_id).await {
        Ok(Some(item)) => match database
            .find_user_item_state(&user.id.to_string(), &item.id)
            .await
        {
            Ok(user_state) => {
                let actors = match state.people.as_ref() {
                    Some(people) => match people.list_item_actors(&item.id).await {
                        Ok(actors) => actors,
                        Err(error) => {
                            tracing::warn!(
                                item_id = %item.id,
                                %error,
                                "derived actor relation is unavailable; returning an empty cast"
                            );
                            Vec::new()
                        }
                    },
                    None => Vec::new(),
                };
                let nfo = match state.local_nfo.as_ref() {
                    Some(local_nfo) => match local_nfo.read_item(&item.id).await {
                        Ok(nfo) => nfo,
                        Err(error) => {
                            tracing::warn!(
                                item_id = %item.id,
                                %error,
                                "derived local NFO cache is unavailable; returning partial item detail"
                            );
                            None
                        }
                    },
                    None => None,
                };
                let mut body = lux_catalog_item_json_with_user_state(&item, user_state.as_ref());
                if let Value::Object(object) = &mut body {
                    object.insert("actors".to_owned(), json!(actors));
                    object.insert("nfo".to_owned(), json!(nfo));
                    if let Some(nfo) = nfo.as_ref() {
                        apply_local_nfo_details(object, nfo);
                    }
                }
                Json(body).into_response()
            }
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体条目不存在",
        )
        .into_response(),
        Err(CatalogError::Storage(_)) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        )
        .into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            unreachable!("inaccessible item is returned as not found")
        }
    }
}

async fn lux_get_metadata(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_media_item_metadata(&item_id).await {
        Ok(Some(metadata)) => Json(metadata_json(
            &metadata.title,
            metadata.original_title.as_deref(),
            metadata.overview.as_deref(),
            metadata.production_year,
            metadata.locked_fields_json.as_deref(),
        ))
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn lux_update_metadata(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<UpdateItemMetadataRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(writes) = state.metadata_writes.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match writes
        .write_item_metadata(
            &item_id,
            MetadataWriteRequest {
                title: request.title,
                original_title: request.original_title,
                overview: request.overview,
                production_year: request.production_year,
                locked_fields: request.locked_fields.into_iter().collect(),
            },
        )
        .await
    {
        Ok(result) => {
            record_audit_event(
                &state,
                &headers,
                "METADATA_EDITED",
                Some("item"),
                Some(&item_id),
                "{}",
            )
            .await;
            let locked_fields_json =
                serde_json::to_string(&result.locked_fields).unwrap_or_else(|_| "[]".to_owned());
            Json(metadata_json(
                &result.title,
                result.original_title.as_deref(),
                result.overview.as_deref(),
                result.production_year.map(i64::from),
                Some(&locked_fields_json),
            ))
            .into_response()
        }
        Err(error) => metadata_write_error(&headers, error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateItemMetadataRequest {
    title: String,
    original_title: Option<String>,
    overview: Option<String>,
    production_year: Option<i32>,
    #[serde(default)]
    locked_fields: Vec<MetadataField>,
}

fn metadata_json(
    title: &str,
    original_title: Option<&str>,
    overview: Option<&str>,
    production_year: Option<i64>,
    locked_fields_json: Option<&str>,
) -> Value {
    let locked_fields = locked_fields_json
        .and_then(|value| serde_json::from_str::<Vec<MetadataField>>(value).ok())
        .unwrap_or_default();
    json!({
        "title": title,
        "originalTitle": original_title,
        "overview": overview,
        "productionYear": production_year,
        "lockedFields": locked_fields,
    })
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LuxChildrenQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    item_type: Option<String>,
    season_id: Option<String>,
}

async fn lux_get_children(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<LuxChildrenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
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
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let Some(parent) = (match catalog.find_item(principal, &item_id).await {
        Ok(parent) => parent,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let result = match parent.item_type.as_str() {
        "BOX_SET" => {
            catalog
                .list_collection_items(principal, &item_id, offset, limit)
                .await
        }
        "SERIES"
            if query
                .item_type
                .as_deref()
                .is_some_and(|item_type| item_type.eq_ignore_ascii_case("EPISODE"))
                || query.season_id.is_some() =>
        {
            catalog
                .list_series_episodes(
                    principal,
                    &item_id,
                    query.season_id.as_deref(),
                    offset,
                    limit,
                )
                .await
        }
        "SERIES" => {
            catalog
                .list_children(principal, &item_id, "SEASON", offset, limit)
                .await
        }
        _ => Ok(CatalogPage {
            items: Vec::new(),
            total: 0,
            offset,
            limit,
        }),
    };
    match result {
        Ok(page) => {
            match lux_catalog_page_json_for_user(database, &user.id.to_string(), &page).await {
                Ok(body) => Json(body).into_response(),
                Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn lux_get_collection(
    headers: HeaderMap,
    Path(collection_id): Path<String>,
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
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let Some(collection) = (match catalog.find_item(principal, &collection_id).await {
        Ok(collection) => collection,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if collection.item_type != "BOX_SET" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let collection_state = match database
        .find_user_item_state(&user.id.to_string(), &collection.id)
        .await
    {
        Ok(state) => state,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match catalog
        .list_collection_items(principal, &collection_id, offset, limit)
        .await
    {
        Ok(page) => {
            let items =
                match lux_catalog_items_json_for_user(database, &user.id.to_string(), &page.items)
                    .await
                {
                    Ok(items) => items,
                    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                };
            Json(json!({
                "collection": lux_catalog_item_json_with_user_state(&collection, collection_state.as_ref()),
                "items": items,
                "total": page.total,
                "page": page.offset / page.limit + 1,
                "pageSize": page.limit,
            }))
            .into_response()
        }
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn lux_image(
    headers: HeaderMap,
    method: Method,
    Path((item_id, image_type)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let Some(images) = state.images.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    serve_image(
        images,
        principal,
        &headers,
        &method,
        &item_id,
        &image_type,
        0,
    )
    .await
}

async fn lux_image_at_index(
    headers: HeaderMap,
    method: Method,
    Path((item_id, image_type, image_index)): Path<(String, String, String)>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let Ok(image_index) = image_index.parse::<i64>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(images) = state.images.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    serve_image(
        images,
        principal,
        &headers,
        &method,
        &item_id,
        &image_type,
        image_index,
    )
    .await
}

async fn lux_list_item_images(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(images) = state.image_writes.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match images.list_item_images(&item_id).await {
        Ok(images) => Json(json!({
            "images": images.iter().map(|image| item_image_json(&item_id, image)).collect::<Vec<_>>()
        })).into_response(),
        Err(error) => image_write_error(&headers, error),
    }
}

async fn lux_search_item_images(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<ItemImageSearchRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(candidates) = state.image_candidates.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match candidates
        .search(
            &item_id,
            &request.image_type,
            request.language.as_deref(),
            request.source.as_deref(),
        )
        .await
    {
        Ok(images) => Json(json!({
            "images": images.iter().map(image_candidate_json).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(error) => image_candidate_error(&headers, error),
    }
}

async fn lux_select_item_image(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<ItemImageSelectRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(images) = state.image_writes.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let report = match images
        .download_item_image_from_scraper_candidate(&item_id, &request.image_type, &request.url)
        .await
    {
        Ok(report) => report,
        Err(error) => return image_write_error(&headers, error),
    };
    let image = match images.list_item_images(&item_id).await {
        Ok(images) => images.into_iter().find(|image| image.id == report.id),
        Err(error) => return image_write_error(&headers, error),
    };
    let Some(image) = image else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    record_audit_event(
        &state,
        &headers,
        "IMAGE_SELECTED",
        Some("item_image"),
        Some(&image.id),
        "{}",
    )
    .await;
    Json(json!({ "image": item_image_json(&item_id, &image) })).into_response()
}

async fn lux_subtitle(
    headers: HeaderMap,
    method: Method,
    Path((item_id, stream_index)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Ok(stream_index) = stream_index.parse::<i64>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    serve_subtitle(
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
        &method,
        &item_id,
        None,
        stream_index,
    )
    .await
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LuxStreamQuery {
    #[serde(alias = "MediaSourceId")]
    source_id: Option<String>,
}

async fn lux_stream(
    headers: HeaderMap,
    method: Method,
    Path(item_id): Path<String>,
    Query(query): Query<LuxStreamQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    serve_media_file(
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
        &headers,
        &method,
        &item_id,
        query.source_id.as_deref(),
        None,
    )
    .await
}

async fn lux_download(
    headers: HeaderMap,
    method: Method,
    Path(item_id): Path<String>,
    Query(query): Query<LuxStreamQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    if !user.can_download {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(downloads) = state.downloads.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let artifact = match downloads
        .prepare(&item_id, query.source_id.as_deref())
        .await
    {
        Ok(artifact) => artifact,
        Err(error) => return download_error_response(error),
    };
    let mut response = serve_download_artifact(downloads, &artifact, &method, &headers).await;
    add_download_header_with_name(&mut response, artifact.file_name());
    response
}

async fn emby_image(
    headers: HeaderMap,
    method: Method,
    Path((item_id, image_type)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let filmly_compat = state.filmly_image_compat_mode == FilmlyImageCompatMode::Compat
        && is_filmly_image_request(&headers)
        && query.tag.is_none();
    let user = match require_emby_user_with_query(&headers, &state, &query).await {
        Ok(user) => Some(user),
        Err(StatusCode::UNAUTHORIZED) => None,
        Err(status) => return status.into_response(),
    };
    let principal = user
        .as_ref()
        .map(|user| AccessPrincipal::new(user.id, user.is_admin));
    if normalize_image_type(&image_type) == Some("POSTER")
        && let Some(response) = serve_emby_library_cover(
            &state,
            principal,
            query.tag.as_deref(),
            &headers,
            &method,
            &item_id,
            0,
        )
        .await
    {
        return response;
    }
    if let Some(response) = serve_emby_person_item_image(
        &state,
        &headers,
        &method,
        &item_id,
        &image_type,
        0,
        query.tag.as_deref(),
    )
    .await
    {
        return response;
    }
    let Some(images) = state.images.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    // Filmly's native image loader drops Emby auth headers and image tags. Its Windows
    // WebView can also issue the backdrop request with a browser UA, so keep the exception
    // limited to untagged backdrop artwork; media streams and tagged images remain gated.
    let untagged_backdrop_compat = state.filmly_image_compat_mode == FilmlyImageCompatMode::Compat
        && user.is_none()
        && query.tag.is_none()
        && normalize_image_type(&image_type) == Some("FANART");
    if (filmly_compat || untagged_backdrop_compat) && user.is_none() {
        return serve_filmly_compat_image(images, &headers, &method, &item_id, &image_type, 0)
            .await;
    }
    match principal {
        Some(principal) => {
            serve_image(
                images,
                principal,
                &headers,
                &method,
                &item_id,
                &image_type,
                0,
            )
            .await
        }
        None => {
            serve_tagged_image(
                images,
                &headers,
                &method,
                &item_id,
                &image_type,
                0,
                query.tag.as_deref(),
            )
            .await
        }
    }
}

fn is_filmly_image_request(headers: &HeaderMap) -> bool {
    header_str(headers, "user-agent").is_some_and(is_filmly_user_agent)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum FilmlyImageCompatMode {
    Generic,
    #[default]
    Compat,
}

fn filmly_image_compat_mode_from_env_value(value: Option<&str>) -> FilmlyImageCompatMode {
    if value.is_some_and(|value| value.trim().eq_ignore_ascii_case("generic")) {
        FilmlyImageCompatMode::Generic
    } else {
        FilmlyImageCompatMode::Compat
    }
}

fn is_filmly_user_agent(value: &str) -> bool {
    value.split_ascii_whitespace().next().is_some_and(|client| {
        client.starts_with("网易爆米花")
            || client.starts_with("%E7%BD%91%E6%98%93%E7%88%86%E7%B1%B3%E8%8A%B1")
            || client
                .get(.."Filmly/".len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Filmly/"))
    })
}

async fn emby_image_at_index(
    headers: HeaderMap,
    method: Method,
    Path((item_id, image_type, image_index)): Path<(String, String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let Ok(image_index) = image_index.parse::<i64>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let filmly_compat = state.filmly_image_compat_mode == FilmlyImageCompatMode::Compat
        && is_filmly_image_request(&headers)
        && query.tag.is_none();
    let user = match require_emby_user_with_query(&headers, &state, &query).await {
        Ok(user) => Some(user),
        Err(StatusCode::UNAUTHORIZED) => None,
        Err(status) => return status.into_response(),
    };
    let principal = user
        .as_ref()
        .map(|user| AccessPrincipal::new(user.id, user.is_admin));
    if normalize_image_type(&image_type) == Some("POSTER")
        && let Some(response) = serve_emby_library_cover(
            &state,
            principal,
            query.tag.as_deref(),
            &headers,
            &method,
            &item_id,
            image_index,
        )
        .await
    {
        return response;
    }
    if let Some(response) = serve_emby_person_item_image(
        &state,
        &headers,
        &method,
        &item_id,
        &image_type,
        image_index,
        query.tag.as_deref(),
    )
    .await
    {
        return response;
    }
    let Some(images) = state.images.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if filmly_compat && user.is_none() {
        return serve_filmly_compat_image(
            images,
            &headers,
            &method,
            &item_id,
            &image_type,
            image_index,
        )
        .await;
    }
    match principal {
        Some(principal) => {
            serve_image(
                images,
                principal,
                &headers,
                &method,
                &item_id,
                &image_type,
                image_index,
            )
            .await
        }
        None => {
            serve_tagged_image(
                images,
                &headers,
                &method,
                &item_id,
                &image_type,
                image_index,
                query.tag.as_deref(),
            )
            .await
        }
    }
}

async fn serve_emby_person_item_image(
    state: &AppState,
    headers: &HeaderMap,
    method: &Method,
    item_id: &str,
    image_type: &str,
    image_index: i64,
    tag: Option<&str>,
) -> Option<Response> {
    if image_index != 0 || normalize_image_type(image_type) != Some("POSTER") {
        return None;
    }
    let expected_tag = emby_person_image_tag(item_id);
    if tag.filter(|tag| !tag.is_empty()) != Some(expected_tag.as_str()) {
        return None;
    }
    let people = state.people.as_ref()?;
    let image = match people.profile_image_for_emby_name_or_id(item_id).await {
        Ok(Some(image)) => image,
        Ok(None) | Err(PeopleError::InvalidComponent(_)) => return None,
        Err(_) => return Some(StatusCode::SERVICE_UNAVAILABLE.into_response()),
    };
    Some(
        serve_image_file(
            &image.path,
            image.content_type,
            image.content_length,
            &format!("\"{expected_tag}\""),
            headers,
            method,
        )
        .await,
    )
}

async fn emby_subtitle_with_source(
    headers: HeaderMap,
    method: Method,
    Path((item_id, media_source_id, stream_index)): Path<(String, String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Ok(stream_index) = stream_index.parse::<i64>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    serve_subtitle(
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
        &method,
        &item_id,
        Some(&media_source_id),
        stream_index,
    )
    .await
}

async fn emby_subtitle_without_source(
    headers: HeaderMap,
    method: Method,
    Path((item_id, stream_index)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Ok(stream_index) = stream_index.parse::<i64>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    serve_subtitle(
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
        &method,
        &item_id,
        None,
        stream_index,
    )
    .await
}

#[derive(Default)]
struct EmbyStreamQuery {
    api_key: Option<String>,
    media_source_id: Option<String>,
}

fn emby_stream_query_from_raw(raw_query: RawQuery) -> EmbyStreamQuery {
    let mut query = EmbyStreamQuery::default();
    let Some(raw_query) = raw_query.0 else {
        return query;
    };

    for (name, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
        if query.api_key.is_none()
            && (name.eq_ignore_ascii_case("api_key")
                || name.eq_ignore_ascii_case("apiKey")
                || name.eq_ignore_ascii_case("ApiKey")
                || name.eq_ignore_ascii_case("X-Emby-Token")
                || name.eq_ignore_ascii_case("X-MediaBrowser-Token")
                || name.eq_ignore_ascii_case("x-media-browser-token"))
        {
            query.api_key = Some(value.into_owned());
        } else if query.media_source_id.is_none()
            && (name.eq_ignore_ascii_case("mediaSourceId")
                || name.eq_ignore_ascii_case("MediaSourceId")
                || name.eq_ignore_ascii_case("media_source_id"))
        {
            query.media_source_id = Some(value.into_owned());
        }
    }

    query
}

fn emby_stream_query_from_path(
    mut query: EmbyStreamQuery,
    container: &str,
) -> (String, EmbyStreamQuery) {
    let Some((container, embedded_query)) = container.split_once('?') else {
        return (container.to_owned(), query);
    };
    let embedded = emby_stream_query_from_raw(RawQuery(Some(embedded_query.to_owned())));
    if query.api_key.is_none() {
        query.api_key = embedded.api_key;
    }
    if query.media_source_id.is_none() {
        query.media_source_id = embedded.media_source_id;
    }
    (container.to_owned(), query)
}

async fn emby_stream(
    headers: HeaderMap,
    method: Method,
    Path(item_id): Path<String>,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let query = emby_stream_query_from_raw(raw_query);
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    serve_media_file(
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
        &headers,
        &method,
        &item_id,
        query.media_source_id.as_deref(),
        None,
    )
    .await
}

async fn emby_stream_with_container(
    headers: HeaderMap,
    method: Method,
    Path((item_id, container)): Path<(String, String)>,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let query = emby_stream_query_from_raw(raw_query);
    let (container, query) = emby_stream_query_from_path(query, &container);
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    serve_media_file(
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
        &headers,
        &method,
        &item_id,
        query.media_source_id.as_deref(),
        Some(&container),
    )
    .await
}

async fn emby_stream_with_source(
    headers: HeaderMap,
    method: Method,
    Path((item_id, media_source_id)): Path<(String, String)>,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let query = emby_stream_query_from_raw(raw_query);
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    serve_media_file(
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
        &headers,
        &method,
        &item_id,
        Some(&media_source_id),
        None,
    )
    .await
}

async fn emby_stream_with_source_and_container(
    headers: HeaderMap,
    method: Method,
    Path((item_id, media_source_id, container)): Path<(String, String, String)>,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let query = emby_stream_query_from_raw(raw_query);
    let (container, query) = emby_stream_query_from_path(query, &container);
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    serve_media_file(
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
        &headers,
        &method,
        &item_id,
        Some(&media_source_id),
        Some(&container),
    )
    .await
}

async fn emby_download(
    headers: HeaderMap,
    method: Method,
    Path(item_id): Path<String>,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let query = emby_stream_query_from_raw(raw_query);
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    if !user.can_download {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(downloads) = state.downloads.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let artifact = match downloads
        .prepare(&item_id, query.media_source_id.as_deref())
        .await
    {
        Ok(artifact) => artifact,
        Err(error) => return download_error_response(error),
    };
    let mut response = serve_download_artifact(downloads, &artifact, &method, &headers).await;
    add_download_header_with_name(&mut response, artifact.file_name());
    response
}

fn add_download_header_with_name(response: &mut Response, file_name: &str) {
    if !response.status().is_success() {
        return;
    }
    let encoded = percent_encode_filename(file_name);
    let fallback = ascii_download_filename(file_name);
    let value = format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}");
    if let Ok(value) = HeaderValue::from_str(&value) {
        response.headers_mut().insert("Content-Disposition", value);
    }
}

fn ascii_download_filename(value: &str) -> String {
    let fallback = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if fallback.is_empty() {
        "download".to_owned()
    } else {
        fallback
    }
}

fn percent_encode_filename(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .flat_map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                vec![char::from(*byte)]
            } else {
                format!("%{byte:02X}").chars().collect()
            }
        })
        .collect()
}

async fn serve_media_file(
    state: &AppState,
    principal: AccessPrincipal,
    headers: &HeaderMap,
    method: &Method,
    item_id: &str,
    media_source_id: Option<&str>,
    _requested_container: Option<&str>,
) -> Response {
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access.can_view_item(principal, item_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let source = database
        .find_playback_source(item_id, media_source_id)
        .await;
    let source = match source {
        Ok(Some(source)) => source,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if source.source_kind == "STRM_URL" {
        let Some(external_url) = source.external_url else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let location = if is_http_strm_target(&external_url) {
            external_url
        } else {
            let Some(plugins) = state.plugins.as_ref() else {
                return StatusCode::NOT_IMPLEMENTED.into_response();
            };
            match plugins.resolve_strm_target(&external_url).await {
                Ok(Some(url)) => url,
                Ok(None) => return StatusCode::NOT_IMPLEMENTED.into_response(),
                Err(PluginServiceError::InvalidResponse) => {
                    return StatusCode::BAD_GATEWAY.into_response();
                }
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        };
        let Some(location) = normalize_strm_http_location(&location) else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
        return match Response::builder()
            .status(StatusCode::TEMPORARY_REDIRECT)
            .header("Location", location)
            .body(Body::empty())
        {
            Ok(response) => response,
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
    }
    if source.source_kind != "LOCAL_FILE" {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    let path = match canonical_local_media_path(&source.root_path, &source.relative_path).await {
        Ok(path) => path,
        Err(LocalPathError::Missing) => return StatusCode::NOT_FOUND.into_response(),
        Err(LocalPathError::Forbidden) => return StatusCode::FORBIDDEN.into_response(),
    };
    serve_media_path(headers, method, &path).await
}

fn download_error_response(error: DownloadError) -> Response {
    let status = match error {
        DownloadError::ItemNotFound => StatusCode::NOT_FOUND,
        DownloadError::PathOutsideRoot(_) => StatusCode::FORBIDDEN,
        DownloadError::InvalidFileName(_)
        | DownloadError::RemoteUrl(
            crate::application::remote_url_policy::RemoteMediaUrlError::Invalid
            | crate::application::remote_url_policy::RemoteMediaUrlError::BlockedHost,
        ) => StatusCode::BAD_REQUEST,
        DownloadError::RemoteUrl(
            crate::application::remote_url_policy::RemoteMediaUrlError::ResolutionFailed,
        )
        | DownloadError::RemoteRequest => StatusCode::BAD_GATEWAY,
        DownloadError::Io(_)
        | DownloadError::Storage(_)
        | DownloadError::ProxyConfiguration(_)
        | DownloadError::ClientBuild(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    status.into_response()
}

async fn serve_download_artifact(
    downloads: &DownloadService,
    artifact: &DownloadArtifact,
    method: &Method,
    headers: &HeaderMap,
) -> Response {
    if let Some(path) = artifact.local_path() {
        return serve_media_path(headers, method, path).await;
    }
    let range = headers.get("range").and_then(|value| value.to_str().ok());
    let upstream = match downloads.fetch_remote(artifact, method, range).await {
        Ok(response) => response,
        Err(error) => return download_error_response(error),
    };
    let status = match StatusCode::from_u16(upstream.status().as_u16()) {
        Ok(status) => status,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let upstream_headers = upstream.headers().clone();
    let is_success = status.is_success();
    let body = if is_success && method != Method::HEAD {
        Body::from_stream(upstream.bytes_stream())
    } else {
        Body::empty()
    };
    let mut response = Response::builder().status(status);
    for header_name in [
        "accept-ranges",
        "content-length",
        "content-range",
        "content-type",
        "etag",
        "last-modified",
    ] {
        if let Some(value) = upstream_headers.get(header_name) {
            response = response.header(header_name, value.clone());
        }
    }
    if is_success && upstream_headers.get("content-type").is_none() {
        response = response.header("content-type", "application/octet-stream");
    }
    match response.body(body) {
        Ok(response) => response,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn serve_media_path(headers: &HeaderMap, method: &Method, path: &FsPath) -> Response {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    let metadata = match fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let size = metadata.len();
    let modified = metadata.modified().ok();
    let etag = media_etag(size, modified);
    let last_modified = modified.and_then(|value| {
        value
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|_| httpdate::fmt_http_date(value))
    });
    let range = match parse_single_range(
        headers
            .get("range")
            .map(|value| value.to_str().unwrap_or("")),
        size,
    ) {
        Ok(range) => range,
        Err(RangeError::Invalid | RangeError::Unsatisfiable) => {
            let mut response = Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header("Accept-Ranges", "bytes")
                .header("Content-Range", format!("bytes */{size}"))
                .header("Content-Length", 0)
                .header("ETag", &etag)
                .header("Content-Type", media_content_type(extension.as_deref()));
            if let Some(last_modified) = &last_modified {
                response = response.header("Last-Modified", last_modified);
            }
            return response
                .body(Body::empty())
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    };
    let (status, start, length, content_range) = match range {
        ByteRange::Full => (StatusCode::OK, 0, size, None),
        ByteRange::Partial { start, end } => (
            StatusCode::PARTIAL_CONTENT,
            start,
            end - start + 1,
            Some(format!("bytes {start}-{end}/{size}")),
        ),
    };
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let Ok(mut file) = fs::File::open(&path).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if file.seek(SeekFrom::Start(start)).await.is_err() {
            return StatusCode::NOT_FOUND.into_response();
        }
        Body::from_stream(tokio_util::io::ReaderStream::new(file.take(length)))
    };
    let mut response = Response::builder()
        .status(status)
        .header("Accept-Ranges", "bytes")
        .header("Content-Length", length)
        .header("Content-Type", media_content_type(extension.as_deref()))
        .header("ETag", &etag);
    if let Some(content_range) = content_range {
        response = response.header("Content-Range", content_range);
    }
    if let Some(last_modified) = &last_modified {
        response = response.header("Last-Modified", last_modified);
    }
    response
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

enum LocalPathError {
    Missing,
    Forbidden,
}

async fn canonical_local_media_path(
    root_path: &str,
    relative_path: &str,
) -> Result<PathBuf, LocalPathError> {
    let relative = FsPath::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(LocalPathError::Forbidden);
    }
    let root = fs::canonicalize(root_path)
        .await
        .map_err(|_| LocalPathError::Missing)?;
    let path = fs::canonicalize(root.join(relative))
        .await
        .map_err(|_| LocalPathError::Missing)?;
    if !path.starts_with(&root) || path == root {
        return Err(LocalPathError::Forbidden);
    }
    Ok(path)
}

fn media_etag(size: u64, modified: Option<std::time::SystemTime>) -> String {
    let modified = modified
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| format!("{}-{}", value.as_secs(), value.subsec_nanos()))
        .unwrap_or_else(|| "unknown".to_owned());
    format!("\"{size:x}-{modified}\"")
}

fn media_content_type(extension: Option<&str>) -> &'static str {
    match extension {
        Some("mkv") => "video/x-matroska",
        Some("mp4" | "m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("avi") => "video/x-msvideo",
        Some("ts" | "m2ts") => "video/mp2t",
        Some("flv") => "video/x-flv",
        _ => "application/octet-stream",
    }
}

async fn serve_subtitle(
    state: &AppState,
    principal: AccessPrincipal,
    method: &Method,
    item_id: &str,
    media_source_id: Option<&str>,
    stream_index: i64,
) -> Response {
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access.can_view_item(principal, item_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let subtitle = match database
        .find_external_subtitle(item_id, media_source_id, stream_index)
        .await
    {
        Ok(Some(subtitle)) => subtitle,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let relative = std::path::Path::new(&subtitle.external_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let root = match tokio::fs::canonicalize(&subtitle.root_path).await {
        Ok(root) => root,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let path = root.join(relative);
    let path = match tokio::fs::canonicalize(&path).await {
        Ok(path) => path,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    if !path.starts_with(&root) || path == root {
        return StatusCode::FORBIDDEN.into_response();
    }
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    if metadata.len() > 10 * 1024 * 1024 {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    let content_type = match extension.as_deref() {
        Some("vtt") => "text/vtt; charset=utf-8",
        Some("srt" | "ass" | "ssa" | "sub") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    };
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let Ok(file) = tokio::fs::File::open(&path).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        Body::from_stream(tokio_util::io::ReaderStream::new(file))
    };
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .header("Content-Length", metadata.len());
    if let Some(language) = subtitle.language {
        builder = builder.header("Content-Language", language);
    }
    builder
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn serve_image(
    images: &ImageService,
    principal: AccessPrincipal,
    headers: &HeaderMap,
    method: &Method,
    item_id: &str,
    image_type: &str,
    image_index: i64,
) -> Response {
    let Some(image_type) = normalize_image_type(image_type) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let image = match images
        .resolve(principal, item_id, image_type, image_index)
        .await
    {
        Ok(Some(image)) => image,
        Ok(None) if image_type == "POSTER" => match images
            .resolve(principal, item_id, "THUMB", image_index)
            .await
        {
            Ok(Some(image)) => image,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(ImageError::Forbidden | ImageError::TooLarge { .. }) => {
                return StatusCode::FORBIDDEN.into_response();
            }
            Err(ImageError::Io { .. }) => return StatusCode::NOT_FOUND.into_response(),
            Err(ImageError::Storage(_)) => {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        },
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(ImageError::Forbidden | ImageError::TooLarge { .. }) => {
            return StatusCode::FORBIDDEN.into_response();
        }
        Err(ImageError::Io { .. }) => return StatusCode::NOT_FOUND.into_response(),
        Err(ImageError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    serve_image_file(
        &image.path,
        image.content_type,
        image.content_length,
        &image.etag,
        headers,
        method,
    )
    .await
}

async fn serve_tagged_image(
    images: &ImageService,
    headers: &HeaderMap,
    method: &Method,
    item_id: &str,
    image_type: &str,
    image_index: i64,
    tag: Option<&str>,
) -> Response {
    let Some(tag) = tag.filter(|tag| !tag.is_empty()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(image_type) = normalize_image_type(image_type) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let image = match images
        .resolve_tagged(item_id, image_type, image_index, tag)
        .await
    {
        Ok(Some(image)) => image,
        Ok(None) if image_type == "POSTER" => match images
            .resolve_tagged(item_id, "THUMB", image_index, tag)
            .await
        {
            Ok(Some(image)) => image,
            Ok(None) => return StatusCode::UNAUTHORIZED.into_response(),
            Err(ImageError::Forbidden | ImageError::TooLarge { .. }) => {
                return StatusCode::FORBIDDEN.into_response();
            }
            Err(ImageError::Io { .. }) => return StatusCode::NOT_FOUND.into_response(),
            Err(ImageError::Storage(_)) => {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        },
        Ok(None) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(ImageError::Forbidden | ImageError::TooLarge { .. }) => {
            return StatusCode::FORBIDDEN.into_response();
        }
        Err(ImageError::Io { .. }) => return StatusCode::NOT_FOUND.into_response(),
        Err(ImageError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    serve_image_file(
        &image.path,
        image.content_type,
        image.content_length,
        &image.etag,
        headers,
        method,
    )
    .await
}

async fn serve_filmly_compat_image(
    images: &ImageService,
    headers: &HeaderMap,
    method: &Method,
    item_id: &str,
    image_type: &str,
    image_index: i64,
) -> Response {
    let Some(image_type) = normalize_image_type(image_type) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let image = match images
        .resolve_filmly_compat(item_id, image_type, image_index)
        .await
    {
        Ok(Some(image)) => image,
        Ok(None) if image_type == "POSTER" => match images
            .resolve_filmly_compat(item_id, "THUMB", image_index)
            .await
        {
            Ok(Some(image)) => image,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(ImageError::Forbidden | ImageError::TooLarge { .. }) => {
                return StatusCode::FORBIDDEN.into_response();
            }
            Err(ImageError::Io { .. }) => return StatusCode::NOT_FOUND.into_response(),
            Err(ImageError::Storage(_)) => {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        },
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(ImageError::Forbidden | ImageError::TooLarge { .. }) => {
            return StatusCode::FORBIDDEN.into_response();
        }
        Err(ImageError::Io { .. }) => return StatusCode::NOT_FOUND.into_response(),
        Err(ImageError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    serve_image_file(
        &image.path,
        image.content_type,
        image.content_length,
        &image.etag,
        headers,
        method,
    )
    .await
}

async fn serve_emby_library_cover(
    state: &AppState,
    principal: Option<AccessPrincipal>,
    capability_tag: Option<&str>,
    headers: &HeaderMap,
    method: &Method,
    library_id: &str,
    image_index: i64,
) -> Option<Response> {
    let library_id = library_id.parse::<crate::domain::ids::LibraryId>().ok()?;
    let covers = state.library_covers.as_ref()?;
    let cover = match covers.resolve(library_id).await {
        Ok(Some(cover)) => cover,
        Ok(None) => return None,
        Err(LibraryCoverError::Storage(_)) => {
            return Some(StatusCode::SERVICE_UNAVAILABLE.into_response());
        }
        Err(_) => return Some(StatusCode::NOT_FOUND.into_response()),
    };
    if image_index != 0 {
        return Some(StatusCode::NOT_FOUND.into_response());
    }
    if let Some(capability_tag) = capability_tag {
        if cover.etag.trim_matches('"') != capability_tag {
            return None;
        }
    } else {
        let principal = principal?;
        let Some(access) = state.access.as_ref() else {
            return Some(StatusCode::SERVICE_UNAVAILABLE.into_response());
        };
        match access
            .can_view_library(principal, &library_id.to_string())
            .await
        {
            Ok(true) => {}
            Ok(false) => return Some(StatusCode::NOT_FOUND.into_response()),
            Err(_) => return Some(StatusCode::SERVICE_UNAVAILABLE.into_response()),
        }
    }
    Some(
        serve_image_file(
            &cover.path,
            &cover.content_type,
            cover.content_length,
            &cover.etag,
            headers,
            method,
        )
        .await,
    )
}

async fn serve_image_file(
    path: &FsPath,
    content_type: &str,
    content_length: u64,
    etag: &str,
    headers: &HeaderMap,
    method: &Method,
) -> Response {
    if headers
        .get("if-none-match")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|tag| tag.trim() == etag))
    {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header("ETag", etag)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let Ok(file) = tokio::fs::File::open(path).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        Body::from_stream(tokio_util::io::ReaderStream::new(file))
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .header("Content-Length", content_length)
        .header("ETag", etag)
        .header("Cache-Control", "private, max-age=3600")
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn lux_page_params(query: &LuxPageQuery) -> Result<(i64, i64), &'static str> {
    page_params(query.page, query.page_size)
}

fn metadata_page_params(query: &MetadataCandidateQuery) -> Result<(i64, i64), &'static str> {
    page_params(query.page, query.page_size)
}

fn page_params(page: Option<i64>, page_size: Option<i64>) -> Result<(i64, i64), &'static str> {
    let page = page.unwrap_or(1);
    let page_size = page_size.unwrap_or(50);
    if page < 1 || !(1..=100).contains(&page_size) {
        return Err("分页参数无效");
    }
    let offset = (page - 1)
        .checked_mul(page_size)
        .ok_or("分页参数超出范围")?;
    Ok((offset, page_size))
}

async fn lux_catalog_items_json_for_user(
    database: &Database,
    user_id: &str,
    items: &[CatalogItem],
) -> Result<Vec<Value>, StorageError> {
    let values = lux_catalog_item_values_by_id(database, user_id, items).await?;
    Ok(lux_catalog_items_from_values(items, &values))
}

async fn lux_catalog_item_values_by_id(
    database: &Database,
    user_id: &str,
    items: &[CatalogItem],
) -> Result<HashMap<String, Value>, StorageError> {
    let mut item_ids = Vec::with_capacity(items.len());
    let mut seen = HashSet::with_capacity(items.len());
    for item in items {
        if seen.insert(item.id.clone()) {
            item_ids.push(item.id.clone());
        }
    }
    let states = database.list_user_item_states(user_id, &item_ids).await?;
    let pending_item_ids = database.list_pending_metadata_item_ids(&item_ids).await?;
    Ok(items
        .iter()
        .map(|item| {
            let mut value = lux_catalog_item_json_with_user_state(item, states.get(&item.id));
            if let Value::Object(object) = &mut value {
                object.insert(
                    "metadataPending".to_owned(),
                    Value::Bool(pending_item_ids.contains(&item.id)),
                );
            }
            (item.id.clone(), value)
        })
        .collect())
}

fn lux_catalog_items_from_values(
    items: &[CatalogItem],
    values: &HashMap<String, Value>,
) -> Vec<Value> {
    items
        .iter()
        .filter_map(|item| values.get(&item.id).cloned())
        .collect()
}

async fn lux_catalog_page_json_for_user(
    database: &Database,
    user_id: &str,
    page: &CatalogPage,
) -> Result<Value, StorageError> {
    let items = lux_catalog_items_json_for_user(database, user_id, &page.items).await?;
    Ok(json!({
        "items": items,
        "total": page.total,
        "page": page.offset / page.limit + 1,
        "pageSize": page.limit,
    }))
}

fn lux_catalog_item_json(item: &CatalogItem) -> Value {
    json!({
        "id": item.id,
        "libraryId": item.library_id,
        "itemType": item.item_type,
        "title": item.title,
        "sortTitle": item.sort_title,
        "originalTitle": item.original_title,
        "overview": item.overview,
        "premiereDate": item.premiere_date,
        "lastAirDate": item.last_air_date,
        "status": item.status,
        "originalLanguage": item.original_language,
        "providerIds": item.provider_ids,
        "parentId": item.parent_id,
        "seriesId": item.series_id,
        "parentIndexNumber": item.season_number,
        "indexNumber": item.episode_number,
        "seasonCount": item.season_count,
        "episodeCount": item.episode_count,
        "productionYear": item.production_year,
        "rating": item.rating,
        "ratingSource": item.rating_source,
        "runtimeTicks": item.runtime_ticks,
        "imageTags": {
            "poster": item.poster_image_tag,
            "fanart": item.fanart_image_tag,
            "thumb": item.thumb_image_tag,
            "logo": item.logo_image_tag,
        },
        "mediaSources": item.media_sources.iter().map(lux_catalog_source_json).collect::<Vec<_>>(),
    })
}

fn lux_catalog_item_json_with_user_state(
    item: &CatalogItem,
    user_state: Option<&crate::storage::StoredUserItemState>,
) -> Value {
    let mut value = lux_catalog_item_json(item);
    if let Value::Object(object) = &mut value {
        object.insert("userData".to_owned(), lux_user_data_json(user_state));
    }
    value
}

fn apply_local_nfo_details(object: &mut serde_json::Map<String, Value>, nfo: &LocalNfoDetails) {
    if let Some(rating) = nfo.rating {
        object.insert("rating".to_owned(), json!(rating));
        object.insert("ratingSource".to_owned(), json!("NFO"));
    }
    if let Some(premiered) = nfo
        .premiered
        .as_deref()
        .or(nfo.release_date.as_deref())
        .or(nfo.aired.as_deref())
    {
        object.insert("premiereDate".to_owned(), json!(premiered));
    }
    if let Some(status) = nfo.status.as_deref() {
        object.insert("status".to_owned(), json!(status));
    }
    if let Some(language) = nfo.original_language.as_deref() {
        object.insert("originalLanguage".to_owned(), json!(language));
    }
    if let Some(last_air_date) = nfo.last_air_date.as_deref() {
        object.insert("lastAirDate".to_owned(), json!(last_air_date));
    }
    if let Some(runtime) = nfo.runtime {
        if let Some(runtime_ticks) = i64::from(runtime)
            .checked_mul(60)
            .and_then(|value| value.checked_mul(10_000_000))
        {
            object.insert("runtimeTicks".to_owned(), json!(runtime_ticks));
        }
    }
    if !nfo.provider_ids.is_empty() {
        let mut provider_ids = object
            .get("providerIds")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (provider, id) in &nfo.provider_ids {
            provider_ids.insert(provider.clone(), json!(id));
        }
        object.insert("providerIds".to_owned(), Value::Object(provider_ids));
    }
}

fn lux_user_data_json(state: Option<&crate::storage::StoredUserItemState>) -> Value {
    json!({
        "positionTicks": state.map(|value| value.position_ticks).unwrap_or_default(),
        "playCount": state.map(|value| value.play_count).unwrap_or_default(),
        "isFavorite": state.map(|value| value.is_favorite).unwrap_or(false),
        "isPlayed": state.map(|value| value.is_played).unwrap_or(false),
    })
}

async fn lux_get_person_image(
    headers: HeaderMap,
    Path(person_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    lux_get_person_image_inner(headers, None, person_id, state).await
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

fn lux_catalog_source_json(source: &crate::application::catalog::CatalogSource) -> Value {
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
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateLibraryRequest {
    name: String,
    kind: String,
    #[serde(default = "default_realtime_watch_enabled")]
    realtime_watch_enabled: bool,
    #[serde(default = "default_realtime_metadata_auto_match_enabled")]
    realtime_metadata_auto_match_enabled: bool,
    scraper_id: Option<String>,
    chapter_source_id: Option<String>,
}

fn default_realtime_watch_enabled() -> bool {
    true
}

fn default_realtime_metadata_auto_match_enabled() -> bool {
    true
}

#[derive(Deserialize)]
struct AddLibraryRootRequest {
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateLibraryRequest {
    name: Option<String>,
    kind: Option<String>,
    is_enabled: Option<bool>,
    realtime_watch_enabled: Option<bool>,
    realtime_metadata_auto_match_enabled: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    #[allow(dead_code)]
    /// Accepted for legacy clients; realtime incremental scanning has no schedule.
    incremental_schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    reconciliation_schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    metadata_schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    scraper_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    chapter_source_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    media_strategy: Option<Option<MediaStrategySettings>>,
    scan_concurrency: Option<i64>,
    probe_concurrency: Option<i64>,
}

fn deserialize_optional_optional<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetLibraryAccessRequest {
    can_view: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MetadataCandidateQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    #[serde(alias = "q")]
    search: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataCandidateSearchRequest {
    query: String,
    year: Option<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemImageSearchRequest {
    image_type: String,
    language: Option<String>,
    source: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemImageSelectRequest {
    image_type: String,
    url: String,
    #[allow(dead_code)]
    language: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateExternalSubtitleRequest {
    source_id: String,
    title: Option<String>,
    language: Option<String>,
    is_default: bool,
    is_forced: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataReidentifyRequest {
    #[serde(default)]
    item_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataBatchConfirmationRequest {
    item_ids: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MetadataReidentifyListQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    status: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum MetadataRefreshRequestMode {
    FillMissing,
    FullRefresh,
}

#[derive(Deserialize)]
struct MetadataRefreshRequest {
    mode: MetadataRefreshRequestMode,
}

impl MetadataRefreshRequestMode {
    const fn application_mode(&self) -> crate::application::reidentify::MetadataRefreshMode {
        match self {
            Self::FillMissing => crate::application::reidentify::MetadataRefreshMode::FillMissing,
            Self::FullRefresh => crate::application::reidentify::MetadataRefreshMode::FullRefresh,
        }
    }

    const fn as_str(&self) -> &'static str {
        match self {
            Self::FillMissing => "FILL_MISSING",
            Self::FullRefresh => "FULL_REFRESH",
        }
    }
}

async fn admin_settings(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let (played_percent, minimum_ticks) = match database.resume_settings().await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let media_strategy = match read_media_strategy_settings(database).await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let server_name = match database.server_name().await {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        Ok(_) => DEFAULT_SERVER_NAME.to_owned(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let network_proxy = network_proxy_settings(&state).await;
    let danmaku = danmaku_settings(&state).await;
    Json(json!({
        "serverName": server_name,
        "resumePlayedPercent": played_percent,
        "resumeMinTicks": minimum_ticks,
        "mediaStrategy": media_strategy,
        "networkProxy": network_proxy,
        "danmaku": danmaku,
    }))
    .into_response()
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DanmakuSettingsResponse {
    configured: bool,
    url: Option<String>,
    source: &'static str,
    restart_required: bool,
}

async fn danmaku_settings(state: &AppState) -> DanmakuSettingsResponse {
    let Some(config_dir) = state.config_dir.as_deref() else {
        return DanmakuSettingsResponse {
            configured: false,
            url: None,
            source: "none",
            restart_required: false,
        };
    };
    if let Some(value) = read_danmaku_provider_url_async(config_dir).await {
        let url = validate_provider_base_url(&value)
            .ok()
            .map(|value| value.redacted().to_owned());
        return DanmakuSettingsResponse {
            configured: url.is_some(),
            url,
            source: "settings",
            restart_required: false,
        };
    }
    DanmakuSettingsResponse {
        configured: false,
        url: None,
        source: "none",
        restart_required: false,
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkProxySettingsResponse {
    configured: bool,
    url: Option<String>,
    has_credentials: bool,
    source: &'static str,
    restart_required: bool,
}

async fn network_proxy_settings(state: &AppState) -> NetworkProxySettingsResponse {
    if let Some(config_dir) = state.config_dir.as_deref()
        && let Some(proxy_url) = read_network_proxy_url_async(config_dir).await
    {
        let url = redact_proxy_url(&proxy_url).ok();
        let has_credentials = proxy_url_has_credentials(&proxy_url).unwrap_or(false);
        return NetworkProxySettingsResponse {
            configured: true,
            url,
            has_credentials,
            source: "settings",
            restart_required: true,
        };
    }
    if let Ok(Some(proxy_url)) = proxy_url_from_env() {
        return NetworkProxySettingsResponse {
            configured: true,
            url: redact_proxy_url(&proxy_url).ok(),
            has_credentials: proxy_url_has_credentials(&proxy_url).unwrap_or(false),
            source: "environment",
            restart_required: true,
        };
    }
    if standard_environment_proxy_configured() {
        return NetworkProxySettingsResponse {
            configured: true,
            url: None,
            has_credentials: false,
            source: "environment",
            restart_required: true,
        };
    }
    NetworkProxySettingsResponse {
        configured: false,
        url: None,
        has_credentials: false,
        source: "none",
        restart_required: true,
    }
}

fn standard_environment_proxy_configured() -> bool {
    [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .into_iter()
    .any(|name| {
        std::env::var(name)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    })
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct NetworkProxyTestRequest {
    network_proxy_url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkProxyDiagnosticsResponse {
    proxy_source: &'static str,
    probes: Vec<NetworkProxyProbeResponse>,
    egress_ip: Option<String>,
    egress_country: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkProxyProbeResponse {
    id: &'static str,
    label: &'static str,
    latency_ms: Option<u64>,
    status: Option<u16>,
    reachable: bool,
    error: Option<&'static str>,
}

async fn admin_test_network_proxy(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<NetworkProxyTestRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let (proxy_url, proxy_source) =
        match network_proxy_for_test(&state, request.network_proxy_url).await {
            Ok(value) => value,
            Err(()) => {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "网络代理地址无效",
                )
                .into_response();
            }
        };
    let diagnostics = test_network(proxy_url.as_deref()).await;
    Json(network_proxy_diagnostics_response(
        proxy_source,
        diagnostics,
    ))
    .into_response()
}

async fn network_proxy_for_test(
    state: &AppState,
    requested_proxy: Option<String>,
) -> Result<(Option<String>, &'static str), ()> {
    let current = match state.config_dir.as_deref() {
        Some(config_dir) => read_network_proxy_url_async(config_dir).await,
        None => None,
    };
    if let Some(requested_proxy) = requested_proxy {
        let normalized = normalize_proxy_url(&requested_proxy).map_err(|_| ())?;
        let keep_current_credentials = current.as_deref().is_some_and(|current| {
            !proxy_url_has_credentials(&normalized).unwrap_or(true)
                && redact_proxy_url(current).ok() == redact_proxy_url(&normalized).ok()
        });
        return Ok((
            Some(if keep_current_credentials {
                current.unwrap_or(normalized)
            } else {
                normalized
            }),
            if keep_current_credentials {
                "settings"
            } else {
                "input"
            },
        ));
    }
    if let Some(current) = current {
        return Ok((Some(current), "settings"));
    }
    if let Ok(Some(proxy_url)) = proxy_url_from_env() {
        return Ok((Some(proxy_url), "environment"));
    }
    Ok((
        None,
        if standard_environment_proxy_configured() {
            "environment"
        } else {
            "none"
        },
    ))
}

fn network_proxy_diagnostics_response(
    proxy_source: &'static str,
    diagnostics: NetworkDiagnostics,
) -> NetworkProxyDiagnosticsResponse {
    NetworkProxyDiagnosticsResponse {
        proxy_source,
        probes: diagnostics
            .probes
            .into_iter()
            .map(network_proxy_probe_response)
            .collect(),
        egress_ip: diagnostics.egress_ip,
        egress_country: diagnostics.egress_country,
    }
}

fn network_proxy_probe_response(result: NetworkProbeResult) -> NetworkProxyProbeResponse {
    NetworkProxyProbeResponse {
        id: result.id,
        label: result.label,
        latency_ms: result.latency_ms,
        status: result.status,
        reachable: result.reachable,
        error: result.error,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdatePlaybackSettingsRequest {
    server_name: Option<String>,
    resume_played_percent: Option<i64>,
    resume_min_ticks: Option<i64>,
    media_strategy: Option<MediaStrategySettings>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MediaStrategySettings {
    metadata_language: String,
    image_language: String,
    region: String,
    scraper_id: Option<String>,
    #[serde(default = "default_metadata_refresh_mode")]
    metadata_refresh_mode: String,
    #[serde(default = "default_show_metadata_pending")]
    show_metadata_pending: bool,
    apply_scope: String,
    images: MediaImageStrategySettings,
    subtitles: MediaSubtitleStrategySettings,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MediaImageStrategySettings {
    poster: bool,
    artwork: bool,
    banner: bool,
    logo: bool,
    thumbnail: bool,
    #[serde(default)]
    disc: bool,
    #[serde(default)]
    wallpaper: bool,
    max_backdrop_count: i64,
    min_download_width: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MediaSubtitleStrategySettings {
    auto_download: bool,
    languages: Vec<String>,
    forced_only: bool,
    hearing_impaired: bool,
}

impl Default for MediaStrategySettings {
    fn default() -> Self {
        Self {
            metadata_language: "zh-CN".to_owned(),
            image_language: "zh-CN".to_owned(),
            region: "CN".to_owned(),
            scraper_id: None,
            metadata_refresh_mode: default_metadata_refresh_mode(),
            show_metadata_pending: true,
            apply_scope: "NEW_CONTENT".to_owned(),
            images: MediaImageStrategySettings {
                poster: true,
                artwork: false,
                banner: false,
                logo: true,
                thumbnail: true,
                disc: false,
                wallpaper: false,
                max_backdrop_count: 1,
                min_download_width: 1280,
            },
            subtitles: MediaSubtitleStrategySettings {
                auto_download: false,
                languages: vec!["zh-CN".to_owned()],
                forced_only: false,
                hearing_impaired: false,
            },
        }
    }
}

fn default_metadata_refresh_mode() -> String {
    "FILL_MISSING".to_owned()
}

fn default_show_metadata_pending() -> bool {
    true
}

async fn read_media_strategy_settings(database: &Database) -> Result<MediaStrategySettings, ()> {
    let stored = database.media_strategy_settings().await.map_err(|_| ())?;
    match stored {
        Some(value) => serde_json::from_str(&value).map_err(|_| ()),
        None => Ok(MediaStrategySettings::default()),
    }
}

fn valid_strategy_code(value: &str, max_length: usize) -> bool {
    !value.is_empty()
        && value.chars().count() <= max_length
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn valid_optional_strategy_code(value: &str, max_length: usize) -> bool {
    value.is_empty() || valid_strategy_code(value, max_length)
}

fn valid_plugin_id(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 64
        && value
            .split('.')
            .all(|segment| valid_strategy_code(segment, 32))
}

fn normalize_server_name(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 80 || value.chars().any(char::is_control) {
        return None;
    }
    Some(value.to_owned())
}

fn validate_media_strategy(settings: &MediaStrategySettings) -> bool {
    valid_strategy_code(&settings.metadata_language, 32)
        && valid_optional_strategy_code(&settings.image_language, 32)
        && valid_strategy_code(&settings.region, 16)
        && matches!(
            settings.apply_scope.as_str(),
            "NEW_CONTENT" | "SELECTED_CONTENT" | "ALL_CONTENT"
        )
        && matches!(
            settings.metadata_refresh_mode.as_str(),
            "FILL_MISSING" | "FULL_REFRESH"
        )
        && settings
            .scraper_id
            .as_deref()
            .map(valid_plugin_id)
            .unwrap_or(true)
        && (0..=20).contains(&settings.images.max_backdrop_count)
        && (0..=8192).contains(&settings.images.min_download_width)
        && (1..=8).contains(&settings.subtitles.languages.len())
        && settings
            .subtitles
            .languages
            .iter()
            .all(|value| valid_strategy_code(value, 32))
}

async fn admin_get_api_key(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin_web_session(&headers, &state, false).await {
        return response;
    }
    let Some(service) = state.admin_api_key.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.current().await {
        Ok(api_key) => Json(json!({
            "configured": api_key.is_some(),
            "apiKey": api_key,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_rotate_api_key(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin_web_session(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.admin_api_key.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.rotate().await {
        Ok(api_key) => {
            record_audit_event(
                &state,
                &headers,
                "ADMIN_API_KEY_ROTATED",
                Some("admin_api_key"),
                None,
                "{}",
            )
            .await;
            Json(json!({
                "configured": true,
                "apiKey": api_key,
            }))
            .into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_revoke_api_key(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin_web_session(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.admin_api_key.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.revoke().await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "ADMIN_API_KEY_REVOKED",
                Some("admin_api_key"),
                None,
                "{}",
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_update_settings(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<UpdatePlaybackSettingsRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let requested_proxy = request.extra.get("networkProxyUrl").cloned();
    let requested_danmaku = request
        .extra
        .get("danmakuProviderUrl")
        .cloned()
        .or_else(|| {
            request
                .extra
                .get("danmaku")
                .and_then(|value| value.get("providerBaseUrl"))
                .cloned()
        });
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let (current_percent, current_ticks) = match database.resume_settings().await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let current_media_strategy = match read_media_strategy_settings(database).await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let current_server_name = match database.server_name().await {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        Ok(_) => DEFAULT_SERVER_NAME.to_owned(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let percent = request.resume_played_percent.unwrap_or(current_percent);
    let minimum_ticks = request.resume_min_ticks.unwrap_or(current_ticks);
    let media_strategy = request.media_strategy.unwrap_or(current_media_strategy);
    let server_name = match request.server_name {
        Some(name) => match normalize_server_name(&name) {
            Some(name) => name,
            None => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => current_server_name,
    };
    if !(1..=100).contains(&percent)
        || minimum_ticks < 0
        || !validate_media_strategy(&media_strategy)
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "全局媒体策略无效",
        )
        .into_response();
    }
    if let Some(scraper_id) = media_strategy.scraper_id.as_deref() {
        if let Err(response) = validate_scraper_selection(&headers, &state, Some(scraper_id)).await
        {
            return response;
        }
    }
    let media_strategy_json = match serde_json::to_string(&media_strategy) {
        Ok(value) => value,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if let Some(requested_proxy) = requested_proxy {
        let Some(config_dir) = state.config_dir.as_deref() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let proxy_url = match requested_proxy {
            Value::Null => None,
            Value::String(value) => {
                let normalized = match normalize_proxy_url(&value) {
                    Ok(value) => value,
                    Err(_) => {
                        return api_error(
                            &headers,
                            StatusCode::BAD_REQUEST,
                            lux::ApiErrorCode::InvalidRequest,
                            "网络代理地址无效",
                        )
                        .into_response();
                    }
                };
                let current = read_network_proxy_url_async(config_dir).await;
                let keep_current_credentials = current.as_deref().is_some_and(|current| {
                    !proxy_url_has_credentials(&normalized).unwrap_or(true)
                        && redact_proxy_url(current).ok() == redact_proxy_url(&normalized).ok()
                });
                Some(if keep_current_credentials {
                    current.unwrap_or(normalized)
                } else {
                    normalized
                })
            }
            _ => {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "网络代理地址无效",
                )
                .into_response();
            }
        };
        if write_network_proxy_url(config_dir, proxy_url.as_deref())
            .await
            .is_err()
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    if let Some(requested_danmaku) = requested_danmaku {
        let Some(config_dir) = state.config_dir.as_deref() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let provider_url = match requested_danmaku {
            Value::Null => None,
            Value::String(value) => {
                let value = value.trim();
                if value.is_empty() {
                    None
                } else if validate_provider_base_url(value).is_err() {
                    return api_error(
                        &headers,
                        StatusCode::BAD_REQUEST,
                        lux::ApiErrorCode::InvalidRequest,
                        "弹幕接口地址无效",
                    )
                    .into_response();
                } else {
                    Some(value.to_owned())
                }
            }
            _ => {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "弹幕接口地址无效",
                )
                .into_response();
            }
        };
        if write_danmaku_provider_url(config_dir, provider_url.as_deref())
            .await
            .is_err()
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    match database
        .set_server_settings(percent, minimum_ticks, &media_strategy_json)
        .await
    {
        Ok(()) => {
            if database.set_server_name(&server_name).await.is_err() {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            record_audit_event(
                &state,
                &headers,
                "SETTINGS_UPDATED",
                Some("settings"),
                None,
                &format!(r#"{{"resumePlayedPercent":{percent},"resumeMinTicks":{minimum_ticks}}}"#),
            )
            .await;
            Json(json!({
                "serverName": server_name,
                "resumePlayedPercent": percent,
                "resumeMinTicks": minimum_ticks,
                "mediaStrategy": media_strategy,
                "networkProxy": network_proxy_settings(&state).await,
                "danmaku": danmaku_settings(&state).await,
            }))
            .into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_set_library_access(
    headers: HeaderMap,
    Path((user_id, library_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<SetLibraryAccessRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let user_id = match user_id.parse::<crate::domain::ids::UserId>() {
        Ok(id) => id.to_string(),
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "用户 ID 无效",
            )
            .into_response();
        }
    };
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id.to_string(),
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库 ID 无效",
            )
            .into_response();
        }
    };
    let Some(database) = state.database.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let user_exists = match database.user_exists(&user_id).await {
        Ok(exists) => exists,
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "数据库暂时不可用",
            )
            .into_response();
        }
    };
    if !user_exists {
        return api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户不存在",
        )
        .into_response();
    }
    let library_exists = match database.library_exists(&library_id).await {
        Ok(exists) => exists,
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "数据库暂时不可用",
            )
            .into_response();
        }
    };
    if !library_exists {
        return api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体库不存在",
        )
        .into_response();
    }
    match database
        .set_user_library_access(&user_id, &library_id, request.can_view)
        .await
    {
        Ok(()) => {
            if let Some(home) = state.home.as_ref() {
                home.invalidate();
            }
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_ACCESS_UPDATED",
                Some("user_library_access"),
                Some(&user_id),
                &format!(
                    r#"{{"libraryId":"{library_id}","canView":{}}}"#,
                    request.can_view
                ),
            )
            .await;
            Json(json!({
                "userId": user_id,
                "libraryId": library_id,
                "canView": request.can_view,
            }))
            .into_response()
        }
        Err(_) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        )
        .into_response(),
    }
}

async fn admin_start_scan(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(library_id) = library_id.parse::<crate::domain::ids::LibraryId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体库 ID 无效",
        )
        .into_response();
    };
    let Some(scan_jobs) = state.scan_jobs.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let job = match scan_jobs.create_movie_scan_job(library_id).await {
        Ok(job) => job,
        Err(ScanJobError::LibraryNotFound) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体库不存在",
            )
            .into_response();
        }
        Err(ScanJobError::AlreadyActive(_)) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库已有扫描任务运行",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let worker = scan_jobs.clone();
    let job_id = job.id.clone();
    let probe = state.probe.clone();
    let metadata = state.metadata_reidentify.clone();
    let thumbnails = state.thumbnails.clone();
    tokio::spawn(async move {
        let _ = worker
            .run_to_completion_with_metadata_and_thumbnails(
                &job_id,
                BACKGROUND_SCAN_BATCH_SIZE,
                probe,
                metadata,
                thumbnails,
            )
            .await;
    });
    let target_id = job.id.clone();
    record_audit_event(
        &state,
        &headers,
        "SCAN_STARTED",
        Some("scan_job"),
        Some(&target_id),
        "{}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job": scan_job_json(&job) })),
    )
        .into_response()
}

async fn admin_start_library_reidentify(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(library_id) = library_id.parse::<crate::domain::ids::LibraryId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体库 ID 无效",
        )
        .into_response();
    };
    let Some(reidentify) = state.metadata_reidentify.clone() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据刮削器匹配服务尚未配置",
        )
        .into_response();
    };
    let job = match reidentify
        .create_library_refresh_job(
            &library_id.to_string(),
            crate::application::reidentify::MetadataRefreshMode::FillMissing,
        )
        .await
    {
        Ok(job) => job,
        Err(error) => return metadata_reidentify_error(&headers, error),
    };
    let job_id = job.id.clone();
    tokio::spawn(async move {
        reidentify.run(&job_id).await;
    });
    record_audit_event(
        &state,
        &headers,
        "METADATA_REIDENTIFY_STARTED",
        Some("library"),
        Some(&library_id.to_string()),
        &format!(
            r#"{{"itemCount":{},"jobId":"{}"}}"#,
            job.total_count, job.id
        ),
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "totalCount": job.total_count,
            "job": metadata_reidentify_job_json(&job),
        })),
    )
        .into_response()
}

async fn spawn_library_scan(
    state: &AppState,
    library_id: crate::domain::ids::LibraryId,
) -> Result<Option<ScanJob>, ScanJobError> {
    let Some(scan_jobs) = state.scan_jobs.as_ref() else {
        return Ok(None);
    };
    let job = match scan_jobs
        .create_movie_scan_job_with_metadata(library_id, true)
        .await
    {
        Ok(job) => job,
        Err(ScanJobError::AlreadyActive(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    let worker = scan_jobs.clone();
    let job_id = job.id.clone();
    let probe = state.probe.clone();
    let metadata = state.metadata_reidentify.clone();
    let thumbnails = state.thumbnails.clone();
    tokio::spawn(async move {
        let _ = worker
            .run_to_completion_with_metadata_and_thumbnails(
                &job_id,
                BACKGROUND_SCAN_BATCH_SIZE,
                probe,
                metadata,
                thumbnails,
            )
            .await;
    });
    Ok(Some(job))
}

async fn admin_start_library_metadata_refresh(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<MetadataRefreshRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(library_id) = library_id.parse::<crate::domain::ids::LibraryId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体库 ID 无效",
        )
        .into_response();
    };
    let Some(reidentify) = state.metadata_reidentify.clone() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据刷新服务尚未配置",
        )
        .into_response();
    };
    let mode = request.mode.application_mode();
    let job = match reidentify
        .create_library_refresh_job(&library_id.to_string(), mode)
        .await
    {
        Ok(job) => job,
        Err(error) => return metadata_reidentify_error(&headers, error),
    };
    let job_id = job.id.clone();
    tokio::spawn(async move {
        reidentify.run(&job_id).await;
    });
    record_audit_event(
        &state,
        &headers,
        "METADATA_REFRESH_STARTED",
        Some("library"),
        Some(&library_id.to_string()),
        &format!(
            r#"{{"itemCount":{},"jobId":"{}","mode":"{}"}}"#,
            job.total_count,
            job.id,
            request.mode.as_str()
        ),
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "totalCount": job.total_count,
            "mode": request.mode.as_str(),
            "job": metadata_reidentify_job_json(&job),
        })),
    )
        .into_response()
}

async fn admin_start_item_scan(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let library_id = match database.find_item_library_id(&item_id).await {
        Ok(Some(library_id)) => library_id,
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体条目不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    admin_start_scan(headers, Path(library_id), State(state)).await
}

async fn admin_start_item_metadata_refresh(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<MetadataRefreshRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(item_id) = item_id.parse::<crate::domain::ids::ItemId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    };
    let Some(reidentify) = state.metadata_reidentify.clone() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据刷新服务尚未配置",
        )
        .into_response();
    };
    let mode = request.mode.application_mode();
    let job = match reidentify
        .create_item_refresh_job(&item_id.to_string(), mode)
        .await
    {
        Ok(job) => job,
        Err(error) => return metadata_reidentify_error(&headers, error),
    };
    let job_id = job.id.clone();
    tokio::spawn(async move {
        reidentify.run(&job_id).await;
    });
    record_audit_event(
        &state,
        &headers,
        "METADATA_REFRESH_STARTED",
        Some("item"),
        Some(&item_id.to_string()),
        &format!(
            r#"{{"jobId":"{}","mode":"{}"}}"#,
            job.id,
            request.mode.as_str()
        ),
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "totalCount": job.total_count,
            "mode": request.mode.as_str(),
            "job": metadata_reidentify_job_json(&job),
        })),
    )
        .into_response()
}

async fn admin_update_item_subtitle(
    headers: HeaderMap,
    Path((item_id, stream_index)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<UpdateExternalSubtitleRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() || request.source_id.trim().is_empty()
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "字幕或媒体条目参数无效",
        )
        .into_response();
    }
    let Ok(stream_index) = stream_index.parse::<i64>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "字幕轨道编号无效",
        )
        .into_response();
    };
    if stream_index < 0
        || request.source_id.chars().count() > 128
        || request
            .title
            .as_deref()
            .is_some_and(|value| value.chars().count() > 256)
        || request
            .language
            .as_deref()
            .is_some_and(|value| value.chars().count() > 32)
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "字幕属性长度无效",
        )
        .into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let title = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let language = request
        .language
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let updated = match database
        .update_external_subtitle(ExternalSubtitleUpdate {
            item_id: &item_id,
            media_source_id: request.source_id.trim(),
            stream_index,
            title,
            language,
            is_default: request.is_default,
            is_forced: request.is_forced,
        })
        .await
    {
        Ok(updated) => updated,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if !updated {
        return api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "外挂字幕不存在",
        )
        .into_response();
    }
    record_audit_event(
        &state,
        &headers,
        "SUBTITLE_UPDATED",
        Some("media_stream"),
        Some(&format!("{}:{}", request.source_id.trim(), stream_index)),
        "{}",
    )
    .await;
    Json(json!({
        "sourceId": request.source_id.trim(),
        "streamIndex": stream_index,
        "title": title,
        "language": language,
        "isDefault": request.is_default,
        "isForced": request.is_forced,
    }))
    .into_response()
}

async fn admin_delete_item(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<LuxStreamQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(deletion) = state.deletion.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let report = match deletion.delete(&item_id, query.source_id.as_deref()).await {
        Ok(report) => report,
        Err(MediaDeleteError::ItemNotFound) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体文件不存在",
            )
            .into_response();
        }
        Err(MediaDeleteError::PathOutsideRoot(_)) => {
            return api_error(
                &headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::PermissionDenied,
                "媒体路径不在媒体库根目录内",
            )
            .into_response();
        }
        Err(MediaDeleteError::InvalidFileName(_)) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体文件名无效",
            )
            .into_response();
        }
        Err(MediaDeleteError::Io(_) | MediaDeleteError::Storage(_)) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    if let Some(home) = state.home.as_ref() {
        home.invalidate();
    }
    record_audit_event(
        &state,
        &headers,
        "MEDIA_DELETED",
        Some("media_source"),
        Some(&report.source_id),
        &format!(
            r#"{{"itemId":"{}","fileCount":{}}}"#,
            report.item_id, report.deleted_file_count
        ),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

async fn admin_refresh_collection(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(collections) = state.collections.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "刮削器合集服务尚未配置",
        )
        .into_response();
    };
    match collections.refresh_for_item(&item_id).await {
        Ok(report) => {
            record_audit_event(
                &state,
                &headers,
                "COLLECTION_REFRESHED",
                Some("item"),
                Some(&item_id),
                "{}",
            )
            .await;
            (
                StatusCode::OK,
                Json(json!({
                    "sourceItemId": report.source_item_id,
                    "collectionItemId": report.collection_item_id,
                    "memberCount": report.member_count,
                })),
            )
                .into_response()
        }
        Err(CollectionError::MovieProviderIdMissing | CollectionError::NoCollection) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "电影没有可用的刮削器合集",
        )
        .into_response(),
        Err(CollectionError::InvalidProviderId) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "刮削器 provider ID 无效",
        )
        .into_response(),
        Err(
            CollectionError::Tmdb(_)
            | CollectionError::Scraper(_)
            | CollectionError::Storage(_)
            | CollectionError::Metadata(_),
        ) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "刮削器合集刷新失败，可重试",
        )
        .into_response(),
    }
}

async fn admin_cancel_scan(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(scan_jobs) = state.scan_jobs.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match scan_jobs.cancel(&job_id).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "SCAN_CANCELLED",
                Some("scan_job"),
                Some(&job_id),
                "{}",
            )
            .await;
            StatusCode::ACCEPTED.into_response()
        }
        Err(ScanJobError::JobNotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_start_strm_probe(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.strm_probe.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let jobs = match service.create_configured_jobs().await {
        Ok(jobs) => jobs,
        Err(error) => return strm_probe_error(&headers, error),
    };
    for job in &jobs {
        let worker = service.clone();
        let job_id = job.id.clone();
        tokio::spawn(async move {
            if let Err(error) = worker.run(&job_id).await {
                tracing::error!(job_id = %job_id, %error, "STRM probe job stopped");
            }
        });
    }
    let operation_id = jobs
        .first()
        .map(|job| job.operation_id.clone())
        .unwrap_or_default();
    record_audit_event(
        &state,
        &headers,
        "STRM_PROBE_STARTED",
        Some("strm_probe_operation"),
        Some(&operation_id),
        &format!(r#"{{"jobCount":{}}}"#, jobs.len()),
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "operationId": operation_id,
            "jobs": jobs,
        })),
    )
        .into_response()
}

async fn admin_list_strm_probe_jobs(
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
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
    let status = query.status.as_deref().map(str::to_ascii_uppercase);
    if status.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "PENDING" | "RUNNING" | "COMPLETED" | "CANCELLED" | "FAILED"
        )
    }) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "任务状态无效",
        )
        .into_response();
    }
    let Some(service) = state.strm_probe.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.list(status.as_deref(), offset, limit).await {
        Ok(jobs) => Json(json!({
            "jobs": jobs,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => strm_probe_error(&headers, error),
    }
}

async fn admin_get_strm_probe_job(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(service) = state.strm_probe.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.get(&job_id).await {
        Ok(job) => Json(json!({ "job": job })).into_response(),
        Err(error) => strm_probe_error(&headers, error),
    }
}

async fn admin_cancel_strm_probe(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.strm_probe.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.cancel(&job_id).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "STRM_PROBE_CANCEL_REQUESTED",
                Some("strm_probe_job"),
                Some(&job_id),
                "{}",
            )
            .await;
            StatusCode::ACCEPTED.into_response()
        }
        Err(error) => strm_probe_error(&headers, error),
    }
}

async fn admin_retry_strm_probe(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.strm_probe.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let job = match service.retry(&job_id).await {
        Ok(job) => job,
        Err(error) => return strm_probe_error(&headers, error),
    };
    let worker = service.clone();
    let new_job_id = job.id.clone();
    tokio::spawn(async move {
        if let Err(error) = worker.run(&new_job_id).await {
            tracing::error!(job_id = %new_job_id, %error, "retried STRM probe job stopped");
        }
    });
    record_audit_event(
        &state,
        &headers,
        "STRM_PROBE_RETRIED",
        Some("strm_probe_job"),
        Some(&job_id),
        &format!(r#"{{"newJobId":"{}"}}"#, job.id),
    )
    .await;
    (StatusCode::ACCEPTED, Json(json!({ "job": job }))).into_response()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ChapterDetectionRequest {
    #[serde(default)]
    plugin_id: Option<String>,
    #[serde(default)]
    concurrency: Option<i64>,
    #[serde(default)]
    intro_window_seconds: Option<i64>,
    #[serde(default)]
    credits_window_seconds: Option<i64>,
    #[serde(default)]
    match_threshold: Option<u32>,
    #[serde(default)]
    force_refresh: bool,
}

async fn admin_start_chapter_detection(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<ChapterDetectionRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(library_id) = library_id.parse::<crate::domain::ids::LibraryId>() else {
        return chapter_detection_error(&headers, ChapterDetectionError::LibraryNotFound);
    };
    let Some(service) = state.chapter_detection.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let defaults = ChapterDetectionOptions::default();
    let plugin_id = request
        .plugin_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_CHAPTER_DETECTOR_PLUGIN_ID);
    let configured_options = if let Some(plugins) = state.plugins.as_ref() {
        match plugins.chapter_detector_settings(plugin_id).await {
            Ok(settings) => Some(ChapterDetectionOptions {
                concurrency: settings.concurrency,
                intro_window_seconds: settings.intro_window_seconds,
                credits_window_seconds: settings.credits_window_seconds,
                match_threshold: settings.match_threshold,
                force_refresh: false,
            }),
            Err(
                PluginServiceError::InvalidConfig
                | PluginServiceError::UnknownPlugin(_)
                | PluginServiceError::Unavailable(_),
            ) => None,
            Err(error) => {
                return chapter_detection_error(&headers, ChapterDetectionError::Plugin(error));
            }
        }
    } else {
        None
    };
    let configured_options = configured_options.unwrap_or(defaults);
    let options = ChapterDetectionOptions {
        concurrency: request
            .concurrency
            .unwrap_or(configured_options.concurrency),
        intro_window_seconds: request
            .intro_window_seconds
            .unwrap_or(configured_options.intro_window_seconds),
        credits_window_seconds: request
            .credits_window_seconds
            .unwrap_or(configured_options.credits_window_seconds),
        match_threshold: request
            .match_threshold
            .unwrap_or(configured_options.match_threshold),
        force_refresh: request.force_refresh,
    };
    let job = match service
        .create_library_job(library_id, plugin_id, options)
        .await
    {
        Ok(job) => job,
        Err(error) => return chapter_detection_error(&headers, error),
    };
    let worker = service.clone();
    let job_id = job.id.clone();
    tokio::spawn(async move {
        if let Err(error) = worker.run(&job_id).await {
            tracing::error!(job_id = %job_id, %error, "chapter detection job stopped");
        }
    });
    record_audit_event(
        &state,
        &headers,
        "CHAPTER_DETECTION_STARTED",
        Some("chapter_detection_job"),
        Some(&job.id),
        &format!(
            r#"{{"libraryId":"{}","pluginId":"{}"}}"#,
            job.library_id, job.plugin_id
        ),
    )
    .await;
    (StatusCode::ACCEPTED, Json(json!({ "job": job }))).into_response()
}

async fn admin_list_chapter_detection_jobs(
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
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
    let status = query.status.as_deref().map(str::to_ascii_uppercase);
    if status.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "PENDING" | "RUNNING" | "COMPLETED" | "CANCELLED" | "FAILED"
        )
    }) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "任务状态无效",
        )
        .into_response();
    }
    let Some(service) = state.chapter_detection.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.list(status.as_deref(), offset, limit).await {
        Ok(jobs) => Json(json!({
            "jobs": jobs,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => chapter_detection_error(&headers, error),
    }
}

async fn admin_get_chapter_detection_job(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(service) = state.chapter_detection.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.get(&job_id).await {
        Ok(job) => Json(json!({ "job": job })).into_response(),
        Err(error) => chapter_detection_error(&headers, error),
    }
}

async fn admin_cancel_chapter_detection(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.chapter_detection.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.cancel(&job_id).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "CHAPTER_DETECTION_CANCEL_REQUESTED",
                Some("chapter_detection_job"),
                Some(&job_id),
                "{}",
            )
            .await;
            StatusCode::ACCEPTED.into_response()
        }
        Err(error) => chapter_detection_error(&headers, error),
    }
}

async fn admin_retry_chapter_detection(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.chapter_detection.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let job = match service.retry(&job_id).await {
        Ok(job) => job,
        Err(error) => return chapter_detection_error(&headers, error),
    };
    let worker = service.clone();
    let new_job_id = job.id.clone();
    tokio::spawn(async move {
        if let Err(error) = worker.run(&new_job_id).await {
            tracing::error!(job_id = %new_job_id, %error, "retried chapter detection job stopped");
        }
    });
    record_audit_event(
        &state,
        &headers,
        "CHAPTER_DETECTION_RETRIED",
        Some("chapter_detection_job"),
        Some(&job_id),
        &format!(r#"{{"newJobId":"{}"}}"#, job.id),
    )
    .await;
    (StatusCode::ACCEPTED, Json(json!({ "job": job }))).into_response()
}

fn chapter_detection_error(headers: &HeaderMap, error: ChapterDetectionError) -> Response {
    let (status, code, message) = match error {
        ChapterDetectionError::InvalidOptions
        | ChapterDetectionError::InvalidPluginResult
        | ChapterDetectionError::SourceChanged
        | ChapterDetectionError::LibraryNotSupported => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "章节检测参数或插件结果无效",
        ),
        ChapterDetectionError::AlreadyActive => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "已有章节检测任务运行中",
        ),
        ChapterDetectionError::LibraryNotFound | ChapterDetectionError::JobNotFound => (
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "章节检测目标不存在",
        ),
        ChapterDetectionError::NotRetryable => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "章节检测任务不可重试",
        ),
        ChapterDetectionError::NotCancellable => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "章节检测任务不可取消",
        ),
        ChapterDetectionError::PluginUnavailable(_) => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "章节检测插件不可用",
        ),
        ChapterDetectionError::WorkerFailed
        | ChapterDetectionError::Plugin(_)
        | ChapterDetectionError::Storage(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "章节检测服务暂时不可用",
        ),
    };
    api_error(headers, status, code, message).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DanmakuMatchRequest {
    #[serde(default = "default_danmaku_concurrency")]
    concurrency: i64,
    #[serde(default)]
    overwrite: bool,
}

const fn default_danmaku_concurrency() -> i64 {
    2
}

async fn admin_start_danmaku_match(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<DanmakuMatchRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(value) => value,
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库 ID 无效",
            )
            .into_response();
        }
    };
    let Some(service) = state.danmaku.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let job = match service
        .create_job(library_id, request.concurrency, request.overwrite)
        .await
    {
        Ok(job) => job,
        Err(error) => return danmaku_service_error(&headers, error),
    };
    let worker = service.clone();
    let job_id = job.id.clone();
    tokio::spawn(async move {
        if let Err(error) = worker.run(&job_id).await {
            tracing::error!(job_id = %job_id, %error, "danmaku match job stopped");
        }
    });
    record_audit_event(
        &state,
        &headers,
        "DANMAKU_MATCH_STARTED",
        Some("danmaku_match_job"),
        Some(&job.id),
        "{}",
    )
    .await;
    (StatusCode::ACCEPTED, Json(json!({ "job": job }))).into_response()
}

async fn admin_list_danmaku_match_jobs(
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(value) => value,
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
    let status = query.status.as_deref().map(str::to_ascii_uppercase);
    if status.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "PENDING" | "RUNNING" | "COMPLETED" | "CANCELLED" | "FAILED"
        )
    }) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "任务状态无效",
        )
        .into_response();
    }
    let Some(service) = state.danmaku.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.list(status.as_deref(), offset, limit).await {
        Ok(jobs) => Json(json!({
            "jobs": jobs,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => danmaku_service_error(&headers, error),
    }
}

async fn admin_get_danmaku_match_job(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(service) = state.danmaku.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.get(&job_id).await {
        Ok(job) => Json(json!({ "job": job })).into_response(),
        Err(error) => danmaku_service_error(&headers, error),
    }
}

async fn admin_cancel_danmaku_match(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.danmaku.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.cancel(&job_id).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "DANMAKU_MATCH_CANCEL_REQUESTED",
                Some("danmaku_match_job"),
                Some(&job_id),
                "{}",
            )
            .await;
            StatusCode::ACCEPTED.into_response()
        }
        Err(error) => danmaku_service_error(&headers, error),
    }
}

async fn admin_retry_danmaku_match(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.danmaku.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let job = match service.retry(&job_id).await {
        Ok(job) => job,
        Err(error) => return danmaku_service_error(&headers, error),
    };
    let worker = service.clone();
    let new_job_id = job.id.clone();
    tokio::spawn(async move {
        if let Err(error) = worker.run(&new_job_id).await {
            tracing::error!(job_id = %new_job_id, %error, "retried danmaku match job stopped");
        }
    });
    record_audit_event(
        &state,
        &headers,
        "DANMAKU_MATCH_RETRIED",
        Some("danmaku_match_job"),
        Some(&job_id),
        &format!(r#"{{"newJobId":"{}"}}"#, job.id),
    )
    .await;
    (StatusCode::ACCEPTED, Json(json!({ "job": job }))).into_response()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AdminJobsQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    status: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AdminJobEventsQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    level: Option<String>,
    event_code: Option<String>,
}

#[derive(Deserialize, Default)]
struct AdminLogExportQuery {
    from: Option<String>,
    to: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AdminScheduledTasksQuery {
    page: Option<i64>,
    page_size: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdminScheduledTaskRequest {
    owner_type: String,
    owner_id: String,
    task_type: String,
    schedule: Option<String>,
    is_enabled: Option<bool>,
}

const SCHEDULE_TASK_TYPES: [&str; 2] = ["RECONCILIATION_SCAN", "METADATA_PARSE"];

async fn admin_list_scheduled_tasks(
    headers: HeaderMap,
    Query(query): Query<AdminScheduledTasksQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
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
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.list_scheduled_task_configs(offset, limit).await {
        Ok((tasks, total)) => Json(json!({
            "scheduledTasks": tasks.iter().map(scheduled_task_json).collect::<Vec<_>>(),
            "total": total,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_upsert_scheduled_task(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<AdminScheduledTaskRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let owner_type = request.owner_type.trim().to_ascii_uppercase();
    if owner_type != "GLOBAL" && owner_type != "LIBRARY" {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "计划归属必须是全局或媒体库",
        )
        .into_response();
    }
    let task_type = request.task_type.trim().to_ascii_uppercase();
    let is_global_strm = owner_type == "GLOBAL"
        && task_type == crate::application::schedule::STRM_MEDIA_INFO_TASK_TYPE;
    if !SCHEDULE_TASK_TYPES.contains(&task_type.as_str()) && !is_global_strm {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "任务类型无效",
        )
        .into_response();
    }
    if owner_type == "GLOBAL" {
        if !request.owner_id.trim().eq_ignore_ascii_case("global") {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "全局计划的 ownerId 必须是 global",
            )
            .into_response();
        }
        if is_global_strm {
            let Some(database) = state.database.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            match database
                .find_scheduled_task_config("GLOBAL", "global", &task_type)
                .await
            {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return api_error(
                        &headers,
                        StatusCode::NOT_FOUND,
                        lux::ApiErrorCode::NotFound,
                        "任务尚未注册",
                    )
                    .into_response();
                }
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
            let Some(schedule) = request
                .schedule
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "STRM 媒体信息任务必须保留 Cron 执行计划",
                )
                .into_response();
            };
            let Some(plugins) = state.plugins.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            if let Err(error) = plugins.update_media_info_schedule(schedule).await {
                return plugin_error(&headers, error);
            }
            let task = match database
                .find_scheduled_task_config("GLOBAL", "global", &task_type)
                .await
            {
                Ok(Some(task)) => task,
                Ok(None) => {
                    return api_error(
                        &headers,
                        StatusCode::NOT_FOUND,
                        lux::ApiErrorCode::NotFound,
                        "任务尚未注册",
                    )
                    .into_response();
                }
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            record_audit_event(
                &state,
                &headers,
                "SCHEDULE_UPDATED",
                Some("scheduled_task"),
                Some("global:STRM_MEDIA_INFO"),
                "{}",
            )
            .await;
            return (
                StatusCode::OK,
                Json(json!({ "scheduledTask": scheduled_task_json(&task) })),
            )
                .into_response();
        }
        let enabled = request.is_enabled.unwrap_or(request.schedule.is_some());
        let schedule = if enabled {
            request.schedule.as_deref().map(str::trim)
        } else {
            None
        };
        let Some(database) = state.database.as_ref() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let task = match database
            .upsert_scheduled_task_config("GLOBAL", "global", &task_type, schedule, enabled)
            .await
        {
            Ok(Some(task)) => task,
            Ok(None) => {
                return api_error(
                    &headers,
                    StatusCode::NOT_FOUND,
                    lux::ApiErrorCode::NotFound,
                    "任务尚未注册",
                )
                .into_response();
            }
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        let target_id = format!("global:{task_type}");
        record_audit_event(
            &state,
            &headers,
            "SCHEDULE_UPDATED",
            Some("scheduled_task"),
            Some(&target_id),
            "{}",
        )
        .await;
        return (
            StatusCode::OK,
            Json(json!({ "scheduledTask": scheduled_task_json(&task) })),
        )
            .into_response();
    }
    let library_id = match request
        .owner_id
        .trim()
        .parse::<crate::domain::ids::LibraryId>()
    {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidLibraryId(error.to_string()),
            );
        }
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .find_scheduled_task_config("LIBRARY", &library_id.to_string(), &task_type)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "任务尚未注册",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let enabled = request.is_enabled.unwrap_or(request.schedule.is_some());
    let schedule = if enabled {
        request.schedule.map(|value| value.trim().to_owned())
    } else {
        None
    };
    let mut settings = LibrarySettingsPatch::default();
    match task_type.as_str() {
        "RECONCILIATION_SCAN" => settings.reconciliation_schedule = Some(schedule),
        "METADATA_PARSE" => settings.metadata_schedule = Some(schedule),
        _ => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "任务类型无效",
            )
            .into_response();
        }
    }
    if let Err(error) = libraries.update_settings(library_id, settings).await {
        return library_error(&headers, error);
    }
    let task = match database
        .find_scheduled_task_config("LIBRARY", &library_id.to_string(), &task_type)
        .await
    {
        Ok(Some(task)) => task,
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "任务尚未注册",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let target_id = format!("{}:{}", library_id, task_type);
    record_audit_event(
        &state,
        &headers,
        "SCHEDULE_UPDATED",
        Some("scheduled_task"),
        Some(&target_id),
        "{}",
    )
    .await;
    (
        StatusCode::OK,
        Json(json!({ "scheduledTask": scheduled_task_json(&task) })),
    )
        .into_response()
}

async fn admin_list_jobs(
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
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
    let status = query.status.as_deref().map(str::to_ascii_uppercase);
    if status.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "PENDING" | "RUNNING" | "COMPLETED" | "CANCELLED" | "FAILED"
        )
    }) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "任务状态无效",
        )
        .into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .list_scan_jobs(status.as_deref(), offset, limit)
        .await
    {
        Ok(jobs) => Json(json!({
            "jobs": jobs.iter().map(scan_job_json_from_storage).collect::<Vec<_>>(),
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_get_job(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_scan_job(&job_id).await {
        Ok(Some(job)) => Json(json!({ "job": scan_job_json_from_storage(&job) })).into_response(),
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "任务不存在",
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_list_job_events(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Query(query): Query<AdminJobEventsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
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
    let level = query.level.as_deref().map(str::to_ascii_uppercase);
    if level
        .as_deref()
        .is_some_and(|value| !matches!(value, "INFO" | "WARN" | "ERROR"))
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "日志级别无效",
        )
        .into_response();
    }
    let event_code = query
        .event_code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_uppercase);
    if event_code.as_deref().is_some_and(|value| {
        value.chars().count() > 64
            || value.chars().any(|character| {
                !(character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_')
            })
    }) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "事件代码无效",
        )
        .into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_scan_job(&job_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "任务不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let total = match database
        .count_scan_job_events(&job_id, level.as_deref(), event_code.as_deref())
        .await
    {
        Ok(total) => total,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match database
        .list_scan_job_events(
            &job_id,
            level.as_deref(),
            event_code.as_deref(),
            offset,
            limit,
        )
        .await
    {
        Ok(events) => Json(json!({
            "events": events.iter().map(scan_job_event_json).collect::<Vec<_>>(),
            "total": total,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_retry_scan(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(scan_jobs) = state.scan_jobs.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let job = match scan_jobs.retry(&job_id).await {
        Ok(job) => job,
        Err(ScanJobError::JobNotFound) => return StatusCode::NOT_FOUND.into_response(),
        Err(ScanJobError::AlreadyActive(_)) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::InvalidRequest,
                "任务仍在运行或不可重试",
            )
            .into_response();
        }
        Err(ScanJobError::LibraryNotFound) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let worker = scan_jobs.clone();
    let new_job_id = job.id.clone();
    let probe = state.probe.clone();
    let metadata = state.metadata_reidentify.clone();
    let thumbnails = state.thumbnails.clone();
    tokio::spawn(async move {
        let _ = worker
            .run_to_completion_with_metadata_and_thumbnails(
                &new_job_id,
                BACKGROUND_SCAN_BATCH_SIZE,
                probe,
                metadata,
                thumbnails,
            )
            .await;
    });
    record_audit_event(
        &state,
        &headers,
        "SCAN_RETRIED",
        Some("scan_job"),
        Some(&job_id),
        &format!(r#"{{"newJobId":"{}"}}"#, job.id),
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job": scan_job_json(&job) })),
    )
        .into_response()
}

fn scan_job_json_from_storage(job: &crate::storage::StoredScanJob) -> Value {
    json!({
        "id": job.id,
        "libraryId": job.library_id,
        "jobType": job.job_type,
        "status": job.status,
        "generation": job.generation,
        "cursor": job.cursor,
        "processedCount": job.processed_count,
        "totalCount": job.total_count,
        "discoveryCompleted": job.discovery_completed,
        "cancelRequested": job.cancel_requested,
        "error": job.error,
        "finishedAt": job.finished_at,
    })
}

fn scheduled_task_json(task: &crate::storage::StoredScheduledTaskConfig) -> Value {
    let resource_limit =
        serde_json::from_str::<Value>(&task.resource_limit_json).unwrap_or_else(|_| json!({}));
    let owner_name = task
        .library_name
        .clone()
        .or_else(|| (task.owner_type == "GLOBAL").then(|| "全局".to_owned()));
    json!({
        "id": format!("{}:{}:{}", task.owner_type, task.owner_id, task.task_type),
        "ownerType": task.owner_type,
        "ownerId": task.owner_id,
        "ownerName": owner_name,
        "taskType": task.task_type,
        "name": task.task_name,
        "description": task.task_description,
        "sourceType": task.source_type,
        "pluginId": task.plugin_id,
        "schedule": task.cron_or_interval,
        "isEnabled": task.is_enabled,
        "resourceLimit": resource_limit,
        "createdAt": task.created_at,
        "updatedAt": task.updated_at,
    })
}

fn scan_job_event_json(event: &crate::storage::StoredScanJobEvent) -> Value {
    let details = serde_json::from_str::<Value>(&event.details_json)
        .unwrap_or_else(|_| json!({ "invalid": true }));
    json!({
        "id": event.id,
        "jobId": event.job_id,
        "level": event.level,
        "eventCode": event.event_code,
        "message": event.message,
        "details": details,
        "createdAt": event.created_at,
    })
}

fn scan_job_json(job: &crate::application::scanner::ScanJob) -> Value {
    json!({
        "id": job.id,
        "libraryId": job.library_id,
        "jobType": job.job_type,
        "status": job.status,
        "generation": job.generation,
        "cursor": job.cursor,
        "processedCount": job.processed_count,
        "totalCount": job.total_count,
        "discoveryCompleted": job.discovery_completed,
        "cancelRequested": job.cancel_requested,
        "error": job.error,
        "finishedAt": job.finished_at,
    })
}

async fn require_admin(
    headers: &HeaderMap,
    state: &AppState,
    require_csrf: bool,
) -> Result<(), Response> {
    let user = require_web_user(headers, state).await?;
    if !user.can_manage_server {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "没有服务器管理权限",
        )
        .into_response());
    }
    if require_csrf && lux_api_key_from_headers(headers).is_none() {
        let Some(auth) = state.auth.as_ref() else {
            return Err(api_error(
                headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "服务尚未就绪",
            )
            .into_response());
        };
        let Some(session_token) = request_cookie(headers, "lux_session") else {
            return Err(api_error(
                headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要登录",
            )
            .into_response());
        };
        let session = match auth.resolve(&session_token).await {
            Ok(Some(session)) => session,
            Ok(None) => {
                return Err(api_error(
                    headers,
                    StatusCode::UNAUTHORIZED,
                    lux::ApiErrorCode::AuthenticationRequired,
                    "需要登录",
                )
                .into_response());
            }
            Err(_) => {
                return Err(api_error(
                    headers,
                    StatusCode::SERVICE_UNAVAILABLE,
                    lux::ApiErrorCode::DatabaseUnavailable,
                    "认证暂时不可用",
                )
                .into_response());
            }
        };
        let Some(csrf_token) = headers
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok())
        else {
            return Err(api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::CsrfFailed,
                "CSRF 校验失败",
            )
            .into_response());
        };
        if !auth.verify_csrf(&session, csrf_token) {
            return Err(api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::CsrfFailed,
                "CSRF 校验失败",
            )
            .into_response());
        }
    }
    Ok(())
}

async fn require_admin_web_session(
    headers: &HeaderMap,
    state: &AppState,
    require_csrf: bool,
) -> Result<(), Response> {
    if lux_api_key_from_headers(headers).is_some() {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "API Key 不能管理 API Key",
        )
        .into_response());
    }
    require_admin(headers, state, require_csrf).await
}

async fn resolve_shared_admin_api_key(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<Option<UserRecord>, Response> {
    let Some(candidate) = lux_api_key_from_headers(headers) else {
        return Ok(None);
    };
    let Some(service) = state.admin_api_key.as_ref() else {
        return Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "认证服务尚未就绪",
        )
        .into_response());
    };
    service.resolve(&candidate).await.map_err(|_| {
        api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "认证暂时不可用",
        )
        .into_response()
    })
}

fn lux_api_key_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("X-Lux-Api-Key")
        .or_else(|| headers.get("X-Emby-Token"))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            headers
                .get("Authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().strip_prefix("Bearer "))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
}

async fn normalize_lux_api_key_query(request: Request<Body>, next: Next) -> Response {
    let mut request = request;
    if request.uri().path().starts_with("/api/v1")
        && !request.headers().contains_key("X-Lux-Api-Key")
        && !request.headers().contains_key("X-Emby-Token")
        && !request.headers().contains_key("X-MediaBrowser-Token")
        && !request.headers().contains_key("Authorization")
        && let Some(query) = request.uri().query()
        && let Some(key) =
            url::form_urlencoded::parse(query.as_bytes()).find_map(|(name, value)| {
                name.eq_ignore_ascii_case("api_key")
                    .then_some(value.into_owned())
            })
        && let Ok(value) = HeaderValue::from_str(&key)
    {
        request.headers_mut().insert("X-Lux-Api-Key", value);
    }
    next.run(request).await
}

async fn record_audit_event(
    state: &AppState,
    headers: &HeaderMap,
    event_type: &str,
    target_type: Option<&str>,
    target_id: Option<&str>,
    metadata_json: &str,
) {
    let Some(database) = state.database.as_ref() else {
        return;
    };
    let (actor_user_id, metadata_json) = if let Some(candidate) = lux_api_key_from_headers(headers)
    {
        let Some(service) = state.admin_api_key.as_ref() else {
            return;
        };
        let Ok(Some(_)) = service.resolve(&candidate).await else {
            return;
        };
        (None, audit_metadata_for_shared_api_key(metadata_json))
    } else {
        let (Some(auth), Some(session_token)) =
            (state.auth.as_ref(), request_cookie(headers, "lux_session"))
        else {
            return;
        };
        let Ok(Some(session)) = auth.resolve(&session_token).await else {
            return;
        };
        (Some(session.user.id.to_string()), metadata_json.to_owned())
    };
    if database
        .insert_audit_event(crate::storage::NewAuditEvent {
            actor_user_id: actor_user_id.as_deref(),
            event_type,
            target_type,
            target_id,
            metadata_json: &metadata_json,
        })
        .await
        .is_ok()
    {
        state
            .admin_events
            .publish(admin_event_scope_for_audit(event_type));
    }
}

fn audit_metadata_for_shared_api_key(metadata_json: &str) -> String {
    let Ok(mut metadata) = serde_json::from_str::<Value>(metadata_json) else {
        return "{\"auth\":\"admin_api_key\"}".to_owned();
    };
    if let Value::Object(object) = &mut metadata {
        object.insert("auth".to_owned(), Value::String("admin_api_key".to_owned()));
    } else {
        metadata = json!({ "auth": "admin_api_key", "details": metadata });
    }
    serde_json::to_string(&metadata).unwrap_or_else(|_| "{\"auth\":\"admin_api_key\"}".to_owned())
}

fn admin_event_scope_for_audit(event_type: &str) -> AdminEventScope {
    if event_type == "SETTINGS_UPDATED" {
        return AdminEventScope::Settings;
    }
    if event_type.starts_with("PLUGIN_") {
        return AdminEventScope::Plugins;
    }
    if event_type.starts_with("USER_") || event_type == "LIBRARY_ACCESS_UPDATED" {
        return AdminEventScope::Users;
    }
    if event_type.starts_with("LIBRARY_") || event_type == "SCHEDULE_UPDATED" {
        return AdminEventScope::Libraries;
    }
    if event_type.starts_with("METADATA_") {
        return AdminEventScope::Metadata;
    }
    if event_type.starts_with("SCAN_")
        || event_type.starts_with("STRM_")
        || event_type.starts_with("DANMAKU_")
    {
        return AdminEventScope::Jobs;
    }
    AdminEventScope::All
}

async fn record_activity_event(
    database: Option<&Database>,
    admin_events: &AdminEventHub,
    user_id: &str,
    event_type: &str,
    target_id: Option<&str>,
    metadata: Value,
) {
    let Some(database) = database else {
        return;
    };
    let metadata_json = match serde_json::to_string(&metadata) {
        Ok(metadata_json) => metadata_json,
        Err(_) => return,
    };
    if database
        .insert_audit_event(crate::storage::NewAuditEvent {
            actor_user_id: Some(user_id),
            event_type,
            target_type: target_id.map(|_| "media_item"),
            target_id,
            metadata_json: &metadata_json,
        })
        .await
        .is_ok()
    {
        admin_events.publish(AdminEventScope::Dashboard);
    }
}

fn playback_activity_event_type(
    previous: Option<&StoredPlaybackSession>,
    state_name: &str,
) -> Option<&'static str> {
    if previous.is_some_and(|session| session.state == state_name)
        || (previous.is_none() && state_name == "STOPPED")
    {
        return None;
    }
    match state_name {
        "PLAYING" => Some("PLAYBACK_STARTED"),
        "PAUSED" => Some("PLAYBACK_PAUSED"),
        "STOPPED" => Some("PLAYBACK_STOPPED"),
        _ => None,
    }
}

async fn admin_list_libraries(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(libraries) = state.libraries.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match libraries.list_libraries().await {
        Ok(views) => Json(json!({
            "libraries": views
                .iter()
                .map(|view| library_json(&view.library, &view.roots))
                .collect::<Vec<_>>()
        }))
        .into_response(),
        Err(error) => library_error(&headers, error),
    }
}

async fn admin_list_directories(
    headers: HeaderMap,
    Query(query): Query<DirectoryBrowseQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) if params.0 <= 10_000 => params,
        Ok(_) | Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "分页参数无效",
            )
            .into_response();
        }
    };
    let path = query.path.as_deref().unwrap_or("/");
    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(50);
    match list_directories(FsPath::new(path), offset as usize, limit as usize).await {
        Ok(result) => Json(json!({
            "path": result.path,
            "parentPath": result.parent_path,
            "directories": result.directories.iter().map(|entry| json!({
                "name": entry.name,
                "path": entry.path,
            })).collect::<Vec<_>>(),
            "page": page,
            "pageSize": page_size,
            "hasMore": result.has_more,
        }))
        .into_response(),
        Err(DirectoryBrowserError::InvalidPath | DirectoryBrowserError::NotDirectory) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "目录路径无效",
        )
        .into_response(),
        Err(DirectoryBrowserError::Unavailable) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "目录不可访问",
        )
        .into_response(),
    }
}

async fn admin_list_users(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users.list_users().await {
        Ok(users) => Json(json!({
            "users": users.iter().map(user_json).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

async fn admin_list_user_library_access(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .list_accessible_library_ids(&user_id.to_string())
        .await
    {
        Ok(library_ids) => Json(json!({ "libraryIds": library_ids })).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_list_audit(
    headers: HeaderMap,
    Query(query): Query<MetadataCandidateQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match metadata_page_params(&query) {
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
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.list_audit_events(offset, limit).await {
        Ok(events) => Json(json!({
            "events": events.iter().map(|event| json!({
                "id": event.id,
                "actorUserId": event.actor_user_id,
                "actorUsername": event.actor_username,
                "eventType": event.event_type,
                "targetType": event.target_type,
                "targetId": event.target_id,
                "metadata": serde_json::from_str::<Value>(&event.metadata_json)
                    .unwrap_or_else(|_| json!({})),
                "createdAt": event.created_at,
            })).collect::<Vec<_>>(),
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_list_logs(
    headers: HeaderMap,
    Query(query): Query<MetadataCandidateQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_audit(headers, Query(query), State(state)).await
}

async fn admin_export_logs(
    headers: HeaderMap,
    Query(query): Query<AdminLogExportQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let range = match LogDateRange::from_query(query.from.as_deref(), query.to.as_deref()) {
        Ok(range) => range,
        Err(error) => return log_export_error_response(&headers, error),
    };
    let Some(config_dir) = state.config_dir.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match export_logs(config_dir, range).await {
        Ok(export) => {
            let (content_type, filename, contents) = match export {
                LogExport::Daily { contents, filename } => {
                    ("application/x-ndjson", filename, contents)
                }
                LogExport::Archive { contents, filename } => {
                    ("application/zip", filename, contents)
                }
            };
            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", content_type)
                .header(
                    "Content-Disposition",
                    format!("attachment; filename=\"{filename}\""),
                )
                .header("Cache-Control", "no-store")
                .body(Body::from(contents))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(error) => log_export_error_response(&headers, error),
    }
}

fn log_export_error_response(headers: &HeaderMap, error: LogExportError) -> Response {
    match error {
        LogExportError::InvalidDate
        | LogExportError::DateRangeReversed
        | LogExportError::DateRangeTooLarge => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            &error.to_string(),
        )
        .into_response(),
        LogExportError::ExportTooLarge => api_error(
            headers,
            StatusCode::PAYLOAD_TOO_LARGE,
            lux::ApiErrorCode::InvalidRequest,
            &error.to_string(),
        )
        .into_response(),
        LogExportError::NoLogs => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            &error.to_string(),
        )
        .into_response(),
        LogExportError::Io(_) | LogExportError::Archive(_) | LogExportError::Worker(_) => {
            tracing::warn!(%error, "log export failed");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn probe_directory_writable(path: &FsPath) -> bool {
    let probe_path = path.join(format!(".lux-health-probe-{}", uuid::Uuid::now_v7()));
    let payload = [0_u8; 4096];
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe_path)
            .await?;
        file.write_all(&payload).await?;
        file.sync_all().await
    }
    .await;
    let _ = fs::remove_file(probe_path).await;
    result.is_ok()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WebhookDestinationCreateRequest {
    name: String,
    url: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    allow_private_network: bool,
    #[serde(default)]
    event_types: Vec<String>,
    payload_format: Option<String>,
    secret: Option<String>,
    provider_plugin_id: Option<String>,
    provider_config: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WebhookDestinationUpdateRequest {
    name: Option<String>,
    url: Option<String>,
    enabled: Option<bool>,
    allow_private_network: Option<bool>,
    event_types: Option<Vec<String>>,
    payload_format: Option<String>,
    provider_plugin_id: Option<String>,
    provider_config: Option<Value>,
}

const fn default_enabled() -> bool {
    true
}

async fn admin_list_webhook_destinations(
    headers: HeaderMap,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match lux_page_params(&query) {
        Ok(value) => value,
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
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service.list_destinations(offset, limit).await {
        Ok(destinations) => Json(json!({
            "destinations": destinations,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

async fn admin_get_webhook_destination(
    headers: HeaderMap,
    Path(destination_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service.get_destination(&destination_id).await {
        Ok(Some(destination)) => Json(json!({ "destination": destination })).into_response(),
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "Webhook 目标不存在",
        )
        .into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

async fn admin_create_webhook_destination(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<WebhookDestinationCreateRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    let provider_plugin_id = request
        .provider_plugin_id
        .as_deref()
        .unwrap_or(BUILTIN_WEBHOOK_PROVIDER_ID);
    let provider_config = request
        .provider_config
        .as_ref()
        .cloned()
        .unwrap_or_else(|| json!({}));
    match service
        .create_destination_with_provider(
            &request.name,
            &request.url,
            request.enabled,
            request.allow_private_network,
            &request.event_types,
            request.secret.as_deref(),
            request.payload_format.as_deref().unwrap_or("LUX"),
            provider_plugin_id,
            &provider_config,
        )
        .await
    {
        Ok((destination, secret)) => (
            StatusCode::CREATED,
            Json(json!({ "destination": destination, "secret": secret })),
        )
            .into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

async fn admin_update_webhook_destination(
    headers: HeaderMap,
    Path(destination_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<WebhookDestinationUpdateRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service
        .update_destination_with_provider(
            &destination_id,
            request.name.as_deref(),
            request.url.as_deref(),
            request.enabled,
            request.allow_private_network,
            request.event_types.as_deref(),
            request.payload_format.as_deref(),
            request.provider_plugin_id.as_deref(),
            request.provider_config.as_ref(),
        )
        .await
    {
        Ok(destination) => Json(json!({ "destination": destination })).into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

async fn admin_delete_webhook_destination(
    headers: HeaderMap,
    Path(destination_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service.delete_destination(&destination_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

async fn admin_test_webhook_destination(
    headers: HeaderMap,
    Path(destination_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service.test_destination(&destination_id).await {
        Ok(status) => Json(json!({ "status": status })).into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

async fn admin_rotate_webhook_secret(
    headers: HeaderMap,
    Path(destination_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service.rotate_secret(&destination_id).await {
        Ok(secret) => Json(json!({ "secret": secret })).into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

async fn admin_list_webhook_deliveries(
    headers: HeaderMap,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match lux_page_params(&query) {
        Ok(value) => value,
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
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service.list_deliveries(offset, limit).await {
        Ok(deliveries) => Json(json!({
            "deliveries": deliveries,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

async fn admin_retry_webhook_delivery(
    headers: HeaderMap,
    Path(delivery_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service.retry_delivery(&delivery_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

fn webhook_error_response(headers: &HeaderMap, error: WebhookError) -> Response {
    match error {
        WebhookError::Invalid(message) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            &message,
        )
        .into_response(),
        WebhookError::NotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "Webhook 目标或投递记录不存在",
        )
        .into_response(),
        WebhookError::Storage(_)
        | WebhookError::Io(_)
        | WebhookError::Serialization(_)
        | WebhookError::HttpResponse { .. }
        | WebhookError::PluginRetryable { .. }
        | WebhookError::PluginFailed(_)
        | WebhookError::Plugin(_)
        | WebhookError::SecretUnavailable
        | WebhookError::RequestSetup(_) => {
            tracing::warn!("webhook operation failed");
            api_error(
                headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "Webhook 通知服务暂时不可用",
            )
            .into_response()
        }
    }
}

async fn admin_health(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    match admin_health_payload(&state).await {
        Ok(payload) => Json(payload).into_response(),
        Err(status) => status.into_response(),
    }
}

async fn admin_events(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }

    let mut receiver = state.admin_events.subscribe();
    let (mut writer, reader) = tokio::io::duplex(16 * 1024);
    tokio::spawn(async move {
        if writer
            .write_all(b"event: ready\ndata: {\"version\":1}\n\n")
            .await
            .is_err()
        {
            return;
        }

        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.tick().await;
        loop {
            tokio::select! {
                event = receiver.recv() => {
                    let scope = match event {
                        Ok(scope) => scope,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => AdminEventScope::All,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    let frame = format!(
                        "event: invalidate\ndata: {{\"scope\":\"{}\"}}\n\n",
                        scope.as_str(),
                    );
                    if writer.write_all(frame.as_bytes()).await.is_err() {
                        break;
                    }
                }
                _ = heartbeat.tick() => {
                    if writer.write_all(b": keep-alive\n\n").await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut response = Response::new(Body::from_stream(tokio_util::io::ReaderStream::new(reader)));
    response.headers_mut().insert(
        "Content-Type",
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response.headers_mut().insert(
        "Cache-Control",
        HeaderValue::from_static("no-cache, no-store, must-revalidate"),
    );
    response
        .headers_mut()
        .insert("X-Accel-Buffering", HeaderValue::from_static("no"));
    response
}

async fn admin_health_payload(state: &AppState) -> Result<Value, StatusCode> {
    let Some(database) = state.database.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let schema_version = match database.schema_version().await {
        Ok(version) => version,
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let resources = state.resources.snapshot().await;
    let database_writable = database.probe_write().await.is_ok();
    let config_available = match state.config_dir.as_deref() {
        Some(path) => fs::metadata(path)
            .await
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false),
        None => false,
    };
    let config_writable = match state.config_dir.as_deref() {
        Some(path) if config_available => probe_directory_writable(path).await,
        _ => false,
    };
    let ffprobe_available = Command::new("ffprobe")
        .arg("-version")
        .output()
        .await
        .is_ok_and(|output| output.status.success());
    let libraries = match state.libraries.as_ref() {
        Some(libraries) => match libraries.list_libraries().await {
            Ok(views) => views
                .iter()
                .map(|view| {
                    json!({
                        "id": view.library.id.to_string(),
                        "name": view.library.name,
                        "isEnabled": view.library.is_enabled,
                        "rootCount": view.roots.len(),
                        "availableRootCount": view.roots.iter().filter(|root| root.is_available).count(),
                        "writableRootCount": view.roots.iter().filter(|root| root.is_writable).count(),
                    })
                })
                .collect::<Vec<_>>(),
            Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
        },
        None => Vec::new(),
    };
    let jobs = match database.list_scan_jobs(None, 0, 10_000).await {
        Ok(jobs) => json!({
            "scanRunning": jobs.iter().filter(|job| matches!(job.status.as_str(), "PENDING" | "RUNNING")).count(),
            "scanFailed": jobs.iter().filter(|job| job.status == "FAILED").count(),
        }),
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let metadata_reidentify_running = match database.list_active_metadata_reidentify_job_ids().await
    {
        Ok(ids) => ids.len(),
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let status = if database_writable && config_available && config_writable && ffprobe_available {
        "ok"
    } else {
        "degraded"
    };
    let (database_backend, journal_mode) = match database.backend() {
        DatabaseBackend::Sqlite => ("SQLITE", "wal"),
        DatabaseBackend::Postgres => ("POSTGRESQL", ""),
    };
    Ok(json!({
        "status": status,
        "schemaVersion": schema_version,
        "runtime": { "seconds": resources.runtime_seconds },
        "resources": resources,
        "database": {
            "status": if database_writable { "ok" } else { "degraded" },
            "backend": database_backend,
            "journalMode": journal_mode,
            "writable": database_writable,
        },
        "config": { "available": config_available, "writable": config_writable },
        "ffprobe": { "available": ffprobe_available },
        "tmdb": { "configured": state.tmdb.is_some() },
        "jobs": {
            "scanRunning": jobs["scanRunning"],
            "scanFailed": jobs["scanFailed"],
            "metadataReidentifyRunning": metadata_reidentify_running,
        },
        "libraries": libraries,
    }))
}

const DEFAULT_SERVER_NAME: &str = "Lux Server";
const DASHBOARD_ACTIVITY_LIMIT: i64 = 24;
const DASHBOARD_PLAYBACK_LIMIT: usize = 24;

async fn admin_dashboard(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let health = match admin_health_payload(&state).await {
        Ok(health) => health,
        Err(status) => return status.into_response(),
    };
    let server_name = match database.server_name().await {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        Ok(_) => DEFAULT_SERVER_NAME.to_owned(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let stats = match database.dashboard_stats().await {
        Ok(stats) => stats,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let sessions = match database.list_playback_sessions(None).await {
        Ok(sessions) => sessions,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => match users.list_users().await {
            Ok(users) => users,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let activity = match database
        .list_activity_events(DASHBOARD_ACTIVITY_LIMIT)
        .await
    {
        Ok(events) => events
            .iter()
            .map(dashboard_activity_json)
            .collect::<Vec<_>>(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let now_playing = match dashboard_playback_json(&state, &sessions, &users).await {
        Ok(sessions) => sessions,
        Err(status) => return status.into_response(),
    };
    Json(json!({
        "server": {
            "name": server_name,
            "version": VERSION,
            "commit": COMMIT,
            "schemaVersion": health["schemaVersion"],
        },
        "stats": dashboard_stats_json(&stats),
        "health": health,
        "nowPlaying": now_playing,
        "activity": activity,
    }))
    .into_response()
}

fn dashboard_stats_json(stats: &DashboardStats) -> Value {
    json!({
        "movieCount": stats.movie_count,
        "seriesCount": stats.series_count,
        "userCount": stats.user_count,
    })
}

async fn dashboard_playback_json(
    state: &AppState,
    sessions: &[StoredPlaybackSession],
    users: &[UserRecord],
) -> Result<Vec<Value>, StatusCode> {
    let Some(catalog) = state.catalog.as_ref() else {
        return Ok(Vec::new());
    };
    let user_names = users
        .iter()
        .map(|user| (user.id.to_string(), user.display_name.clone()))
        .collect::<BTreeMap<_, _>>();
    let principal = AccessPrincipal::new(crate::domain::ids::UserId::new(), true);
    let mut values = Vec::new();
    for session in sessions.iter().take(DASHBOARD_PLAYBACK_LIMIT) {
        let item = match catalog.find_item(principal, &session.item_id).await {
            Ok(Some(item)) => item,
            Ok(None) => continue,
            Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
        };
        let series = if item.item_type == "EPISODE" {
            match item.series_id.as_deref() {
                Some(series_id) => match catalog.find_item(principal, series_id).await {
                    Ok(series) => series,
                    Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
                },
                None => None,
            }
        } else {
            None
        };
        let remote_ip_location = session.remote_ip.as_deref().and_then(|remote_ip| {
            state
                .ip_location
                .as_ref()
                .and_then(|service| service.cached_or_schedule(remote_ip))
        });
        values.push(dashboard_playback_item_json(
            session,
            &item,
            series.as_ref(),
            user_names
                .get(&session.user_id)
                .map(String::as_str)
                .unwrap_or("未知账户"),
            remote_ip_location.as_ref(),
        ));
    }
    Ok(values)
}

fn dashboard_playback_item_json(
    session: &StoredPlaybackSession,
    item: &CatalogItem,
    series: Option<&CatalogItem>,
    user_name: &str,
    remote_ip_location: Option<&IpLocation>,
) -> Value {
    let source = session
        .media_source_id
        .as_deref()
        .and_then(|source_id| {
            item.media_sources
                .iter()
                .find(|source| source.id == source_id)
        })
        .or_else(|| item.media_sources.iter().find(|source| source.is_default))
        .or_else(|| item.media_sources.first());
    json!({
        "id": session.id,
        "userId": session.user_id,
        "userName": user_name,
        "itemId": item.id,
        "title": item.title,
        "originalTitle": item.original_title,
        "itemType": item.item_type,
        "seriesId": item.series_id,
        "seriesTitle": series.map(|item| item.title.as_str()),
        "productionYear": item.production_year,
        "parentIndexNumber": item.season_number,
        "indexNumber": item.episode_number,
        "posterAvailable": item.poster_image_tag.is_some(),
        "positionTicks": session.position_ticks,
        "durationTicks": session.duration_ticks.or(item.runtime_ticks),
        "state": session.state,
        "isPaused": session.is_paused,
        "lastEventAt": session.last_event_at,
        "client": session.client,
        "clientVersion": session.client_version,
        "deviceId": session.device_id,
        "deviceName": session.device_name,
        "deviceType": session.device_type,
        "remoteIp": session.remote_ip,
        "remoteIpLocation": remote_ip_location.map(dashboard_ip_location_json),
        "playSessionId": session.play_session_id,
        "source": source.map(dashboard_source_json),
    })
}

fn dashboard_ip_location_json(location: &IpLocation) -> Value {
    json!({
        "location": location.formatted_location(),
        "district": location.district,
        "street": location.street,
        "isp": location.isp,
    })
}

fn dashboard_source_json(source: &CatalogSource) -> Value {
    let video = source
        .streams
        .iter()
        .find(|stream| stream.stream_type == "VIDEO");
    let audio = source
        .streams
        .iter()
        .find(|stream| stream.stream_type == "AUDIO");
    json!({
        "id": source.id,
        "qualityLabel": source.quality_label,
        "editionName": source.edition_name,
        "container": source.container,
        "bitrate": source.bitrate,
        "durationTicks": source.duration_ticks,
        "video": video.map(|stream| json!({
            "codec": stream.codec,
            "title": stream.title,
            "details": stream.details,
        })),
        "audio": audio.map(|stream| json!({
            "codec": stream.codec,
            "language": stream.language,
            "title": stream.title,
        })),
    })
}

fn dashboard_activity_json(event: &crate::storage::StoredActivityEvent) -> Value {
    json!({
        "id": event.id,
        "userId": event.actor_user_id,
        "userName": event.actor_username,
        "eventType": event.event_type,
        "targetType": event.target_type,
        "targetId": event.target_id,
        "targetTitle": event.target_title,
        "metadata": serde_json::from_str::<Value>(&event.metadata_json)
            .unwrap_or_else(|_| json!({})),
        "createdAt": event.created_at,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateUserRequest {
    username: String,
    #[serde(default)]
    display_name: String,
    password: String,
    #[serde(default)]
    is_admin: bool,
}

async fn admin_create_user(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<CreateUserRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users
        .create_user(
            &request.username,
            &request.display_name,
            &request.password,
            request.is_admin,
        )
        .await
    {
        Ok(user) => {
            let target_id = user.id.to_string();
            record_audit_event(
                &state,
                &headers,
                "USER_CREATED",
                Some("user"),
                Some(&target_id),
                "{}",
            )
            .await;
            (
                StatusCode::CREATED,
                Json(json!({ "user": user_json(&user) })),
            )
                .into_response()
        }
        Err(error) => user_store_error(&headers, error),
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct UpdateUserRequest {
    display_name: Option<String>,
    password: Option<String>,
    is_disabled: Option<bool>,
    is_admin: Option<bool>,
    can_manage_server: Option<bool>,
    can_remote_access: Option<bool>,
    can_download: Option<bool>,
}

async fn admin_update_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<UpdateUserRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users
        .update_user(
            &user_id,
            UserUpdate {
                display_name: request.display_name.as_deref(),
                password: request.password.as_deref(),
                is_disabled: request.is_disabled,
                is_admin: request.is_admin,
                can_manage_server: request.can_manage_server,
                can_remote_access: request.can_remote_access,
                can_download: request.can_download,
            },
        )
        .await
    {
        Ok(Some(user)) => {
            record_audit_event(
                &state,
                &headers,
                "USER_UPDATED",
                Some("user"),
                Some(&user_id),
                "{}",
            )
            .await;
            Json(json!({ "user": user_json(&user) })).into_response()
        }
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户不存在",
        )
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

async fn admin_disable_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users
        .update_user(
            &user_id,
            UserUpdate {
                is_disabled: Some(true),
                ..UserUpdate::default()
            },
        )
        .await
    {
        Ok(Some(user)) => {
            record_audit_event(
                &state,
                &headers,
                "USER_DISABLED",
                Some("user"),
                Some(&user_id),
                "{}",
            )
            .await;
            Json(json!({ "user": user_json(&user) })).into_response()
        }
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户不存在",
        )
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

fn user_store_error(headers: &HeaderMap, error: UserStoreError) -> Response {
    match error {
        UserStoreError::InvalidUsername | UserStoreError::Password(_) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户请求无效",
        )
        .into_response(),
        UserStoreError::LastManager => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::PermissionDenied,
            "至少需要一个启用的服务器管理账户",
        )
        .into_response(),
        UserStoreError::InvalidUserId(_) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response(),
        UserStoreError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "用户数据暂时不可用",
        )
        .into_response(),
        UserStoreError::SetupAlreadyCompleted => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "初始化已完成",
        )
        .into_response(),
    }
}

async fn admin_list_pending_metadata(
    headers: HeaderMap,
    Query(query): Query<MetadataCandidateQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match metadata_page_params(&query) {
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
    let Some(candidates) = state.metadata_candidates.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match candidates.list_pending(offset, limit).await {
        Ok(page) => Json(metadata_candidate_page_json(&page)).into_response(),
        Err(error) => metadata_candidate_error(&headers, error),
    }
}

async fn admin_list_metadata_reidentify(
    headers: HeaderMap,
    Query(query): Query<MetadataReidentifyListQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
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
    let status = query.status.as_deref().map(str::to_ascii_uppercase);
    if status.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "QUEUED" | "RUNNING" | "COMPLETED" | "CANCELLED" | "FAILED"
        )
    }) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "元数据任务状态无效",
        )
        .into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .list_metadata_reidentify_jobs(status.as_deref(), offset, limit)
        .await
    {
        Ok(jobs) => Json(json!({
            "jobs": jobs.iter().map(metadata_reidentify_job_summary_json).collect::<Vec<_>>(),
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_start_metadata_reidentify(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<MetadataReidentifyRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if request
        .item_ids
        .iter()
        .any(|item_id| item_id.parse::<crate::domain::ids::ItemId>().is_err())
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(reidentify) = state.metadata_reidentify.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据刮削器匹配服务尚未配置",
        )
        .into_response();
    };
    let job = match reidentify.create_job(request.item_ids).await {
        Ok(job) => job,
        Err(error) => return metadata_reidentify_error(&headers, error),
    };
    let worker = reidentify.clone();
    let job_id = job.id.clone();
    tokio::spawn(async move {
        worker.run(&job_id).await;
    });
    record_audit_event(
        &state,
        &headers,
        "METADATA_REIDENTIFY_STARTED",
        Some("metadata_reidentify_job"),
        Some(&job.id),
        "{}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job": metadata_reidentify_job_json(&job) })),
    )
        .into_response()
}

async fn admin_confirm_metadata(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<MetadataBatchConfirmationRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if request.item_ids.is_empty() || request.item_ids.len() > 100 {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "批量确认条目数量必须在 1 到 100 之间",
        )
        .into_response();
    }
    let requested_item_count = request.item_ids.len();
    let item_ids: Vec<String> = request
        .item_ids
        .into_iter()
        .filter(|item_id| item_id.parse::<crate::domain::ids::ItemId>().is_ok())
        .collect();
    if item_ids.len() != item_ids.iter().collect::<HashSet<_>>().len() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "批量确认条目不能重复",
        )
        .into_response();
    }
    if item_ids.len() != requested_item_count {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(selection) = state.metadata_selection.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据写回服务尚未就绪",
        )
        .into_response();
    };
    let mut confirmed_count = 0_usize;
    let mut failed_item_ids = Vec::new();
    for item_id in &item_ids {
        match selection.confirm_best_pending(item_id).await {
            Ok(_) => confirmed_count += 1,
            Err(_) => failed_item_ids.push(item_id.clone()),
        }
    }
    record_audit_event(
        &state,
        &headers,
        "METADATA_BATCH_CONFIRMED",
        Some("metadata_items"),
        None,
        &json!({
            "requestedCount": item_ids.len(),
            "confirmedCount": confirmed_count,
            "failedCount": failed_item_ids.len(),
        })
        .to_string(),
    )
    .await;
    Json(json!({
        "confirmedCount": confirmed_count,
        "failedCount": failed_item_ids.len(),
        "failedItemIds": failed_item_ids,
    }))
    .into_response()
}

async fn admin_get_metadata_reidentify(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(reidentify) = state.metadata_reidentify.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据刮削器匹配服务尚未配置",
        )
        .into_response();
    };
    match reidentify.get_job(&job_id).await {
        Ok(job) => Json(json!({ "job": metadata_reidentify_job_json(&job) })).into_response(),
        Err(error) => metadata_reidentify_error(&headers, error),
    }
}

async fn admin_retry_metadata_reidentify(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(reidentify) = state.metadata_reidentify.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据刮削器匹配服务尚未配置",
        )
        .into_response();
    };
    let job = match reidentify.retry_job(&job_id).await {
        Ok(job) => job,
        Err(error) => return metadata_reidentify_error(&headers, error),
    };
    let worker = reidentify.clone();
    let worker_job_id = job.id.clone();
    tokio::spawn(async move {
        worker.run(&worker_job_id).await;
    });
    record_audit_event(
        &state,
        &headers,
        "METADATA_REIDENTIFY_RETRIED",
        Some("metadata_reidentify_job"),
        Some(&job.id),
        "{}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job": metadata_reidentify_job_json(&job) })),
    )
        .into_response()
}

async fn admin_cancel_metadata_reidentify(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(reidentify) = state.metadata_reidentify.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据刮削器匹配服务尚未配置",
        )
        .into_response();
    };
    match reidentify.cancel(&job_id).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "METADATA_REIDENTIFY_CANCELLED",
                Some("metadata_reidentify_job"),
                Some(&job_id),
                "{}",
            )
            .await;
            StatusCode::ACCEPTED.into_response()
        }
        Err(error) => metadata_reidentify_error(&headers, error),
    }
}

async fn admin_list_item_candidates(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<MetadataCandidateQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let (offset, limit) = match metadata_page_params(&query) {
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
    let Some(candidates) = state.metadata_candidates.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match candidates
        .list_for_item(&item_id, query.search.as_deref(), offset, limit)
        .await
    {
        Ok(page) => Json(metadata_candidate_page_json(&page)).into_response(),
        Err(error) => metadata_candidate_error(&headers, error),
    }
}

async fn admin_search_item_candidates(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<MetadataCandidateSearchRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(fallback_tmdb) = state.tmdb.as_ref().cloned() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "刮削器尚未配置",
        )
        .into_response();
    };
    let tmdb = if let Some(resolver) = state.scraper_resolver.as_ref() {
        match resolver.for_item(&item_id).await {
            Ok(Some(scraper)) => TmdbProvider::from_scraper(scraper),
            Ok(None) => fallback_tmdb,
            Err(error) => {
                return api_error(
                    &headers,
                    StatusCode::SERVICE_UNAVAILABLE,
                    lux::ApiErrorCode::DatabaseUnavailable,
                    &format!("刮削器不可用: {error}"),
                )
                .into_response();
            }
        }
    } else {
        fallback_tmdb
    };
    let Some(candidates) = state.metadata_candidates.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据候选服务尚未就绪",
        )
        .into_response();
    };
    match candidates
        .search_and_store(&item_id, &request.query, request.year, &tmdb)
        .await
    {
        Ok(page) => {
            record_audit_event(
                &state,
                &headers,
                "METADATA_SEARCHED",
                Some("item"),
                Some(&item_id),
                "{}",
            )
            .await;
            Json(metadata_candidate_page_json(&page)).into_response()
        }
        Err(error) => metadata_candidate_error(&headers, error),
    }
}

async fn admin_list_item_images(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_media_item_metadata(&item_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体条目不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(images) = state.image_writes.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match images.list_item_images(&item_id).await {
        Ok(images) => Json(json!({
            "images": images.iter().map(|image| json!({
                "id": image.id,
                "itemId": image.item_id,
                "imageType": image.image_type,
                "imageIndex": image.image_index,
                "fileSize": image.file_size,
                "contentTag": image.content_tag,
                "source": image.source,
            })).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(error) => image_write_error(&headers, error),
    }
}

async fn admin_delete_item_image(
    headers: HeaderMap,
    Path((item_id, image_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err()
        || image_id.parse::<uuid::Uuid>().is_err()
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "图片或媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(images) = state.image_writes.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match images.delete_item_image(&item_id, &image_id).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "IMAGE_DELETED",
                Some("item_image"),
                Some(&image_id),
                "{}",
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => image_write_error(&headers, error),
    }
}

#[derive(Deserialize)]
struct MetadataSelectionRequest {
    mode: MetadataSelectionMode,
}

async fn admin_select_candidate(
    headers: HeaderMap,
    Path((item_id, candidate_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<MetadataSelectionRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(selection) = state.metadata_selection.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据写回服务尚未就绪",
        )
        .into_response();
    };
    match selection
        .select(&item_id, &candidate_id, request.mode)
        .await
    {
        Ok(report) => {
            record_audit_event(
                &state,
                &headers,
                "METADATA_SELECTED",
                Some("item"),
                Some(&report.item_id),
                "{}",
            )
            .await;
            Json(json!({
                "itemId": report.item_id,
                "candidateId": report.candidate_id,
                "mode": report.mode.as_str(),
                "status": report.status,
                "imageTypes": report.image_types,
                "actorCount": report.actor_count,
            }))
            .into_response()
        }
        Err(error) => metadata_selection_error(&headers, error),
    }
}

fn metadata_selection_error(headers: &HeaderMap, error: MetadataSelectionError) -> Response {
    match error {
        MetadataSelectionError::ItemNotFound | MetadataSelectionError::CandidateNotFound => {
            api_error(
                headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体条目或候选不存在",
            )
            .into_response()
        }
        MetadataSelectionError::CandidateNotPending(_) => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "候选已处理，不能重复选择",
        )
        .into_response(),
        MetadataSelectionError::InvalidCandidate(_) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "候选数据无效",
        )
        .into_response(),
        MetadataSelectionError::Nfo(_)
        | MetadataSelectionError::Image(_)
        | MetadataSelectionError::People(_) => api_error(
            headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "元数据写回失败，可重试",
        )
        .into_response(),
        MetadataSelectionError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据保存暂时不可用，可重试",
        )
        .into_response(),
    }
}

fn image_write_error(headers: &HeaderMap, error: ImageWriteError) -> Response {
    match error {
        ImageWriteError::ItemNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体条目或图片不存在",
        )
        .into_response(),
        ImageWriteError::PathOutsideRoot(_) | ImageWriteError::SymlinkTarget(_) => api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "图片路径不在媒体根目录内",
        )
        .into_response(),
        ImageWriteError::Storage(_) | ImageWriteError::Io { .. } => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "图片操作暂时失败",
        )
        .into_response(),
        _ => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "图片请求无效",
        )
        .into_response(),
    }
}

fn item_image_json(item_id: &str, image: &crate::storage::StoredItemImage) -> Value {
    let image_type = image.image_type.to_ascii_lowercase();
    let index = if image.image_index > 0 {
        format!("/{}", image.image_index)
    } else {
        String::new()
    };
    json!({
        "id": image.id,
        "itemId": item_id,
        "imageType": image.image_type,
        "imageIndex": image.image_index,
        "fileSize": image.file_size,
        "contentTag": image.content_tag,
        "source": image.source,
        "language": Value::Null,
        "url": format!("/api/v1/items/{}/images/{}{}", encode_path_segment(item_id), image_type, index),
    })
}

fn image_candidate_json(image: &crate::application::images::ImageCandidate) -> Value {
    json!({
        "id": image.id,
        "imageType": image.image_type,
        "imageIndex": image.image_index,
        "language": image.language,
        "width": image.width,
        "height": image.height,
        "source": image.source,
        "url": image.url,
    })
}

fn encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                vec![char::from(byte)]
            } else {
                format!("%{byte:02X}").chars().collect()
            }
        })
        .collect()
}

fn image_candidate_error(headers: &HeaderMap, error: ImageCandidateError) -> Response {
    match error {
        ImageCandidateError::ItemNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体条目不存在",
        )
        .into_response(),
        ImageCandidateError::ItemNotIdentified => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目尚未完成元数据匹配，暂时无法搜索图片",
        )
        .into_response(),
        ImageCandidateError::InvalidItem
        | ImageCandidateError::InvalidImageType(_)
        | ImageCandidateError::InvalidLanguage
        | ImageCandidateError::InvalidSource => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "图片搜索请求无效",
        )
        .into_response(),
        ImageCandidateError::Tmdb(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::Internal,
            "刮削器暂时不可用",
        )
        .into_response(),
        ImageCandidateError::Scraper(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::Internal,
            "刮削器暂时不可用",
        )
        .into_response(),
        ImageCandidateError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "图片搜索暂时不可用",
        )
        .into_response(),
    }
}

fn metadata_write_error(headers: &HeaderMap, error: NfoWriteError) -> Response {
    match error {
        NfoWriteError::ItemNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体条目不存在或没有本地媒体源",
        )
        .into_response(),
        NfoWriteError::InvalidMetadata(_) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "元数据请求无效",
        )
        .into_response(),
        NfoWriteError::PathOutsideRoot(_) | NfoWriteError::SymlinkTarget(_) => api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "元数据路径不在媒体根目录内",
        )
        .into_response(),
        NfoWriteError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据保存暂时不可用",
        )
        .into_response(),
        NfoWriteError::Nfo(_)
        | NfoWriteError::InvalidXml(_)
        | NfoWriteError::Io { .. }
        | NfoWriteError::ConcurrentModification(_) => api_error(
            headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "元数据写回失败，可重试",
        )
        .into_response(),
    }
}

fn metadata_candidate_page_json(page: &MetadataCandidatePage) -> Value {
    json!({
        "items": page.items.iter().map(|item| json!({
            "id": item.id,
            "itemId": item.item_id,
            "itemTitle": item.item_title,
            "provider": item.provider,
            "providerId": item.provider_id,
            "candidate": item.candidate,
            "score": item.score,
            "status": item.status,
            "expiresAt": item.expires_at,
            "fieldDiffs": item.field_diffs.iter().map(|diff| json!({
                "field": diff.field,
                "current": diff.current,
                "candidate": diff.candidate,
                "provenance": diff.provenance,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "total": page.total,
        "page": page.offset / page.limit + 1,
        "pageSize": page.limit,
    })
}

fn metadata_reidentify_job_json(
    job: &crate::application::reidentify::MetadataReidentifyJob,
) -> Value {
    json!({
        "id": job.id,
        "status": job.status,
        "mode": job.mode,
        "processedCount": job.processed_count,
        "totalCount": job.total_count,
        "error": job.error,
        "createdAt": job.created_at,
        "updatedAt": job.updated_at,
        "startedAt": job.started_at,
        "finishedAt": job.finished_at,
        "cancelRequested": job.cancel_requested,
        "libraryId": job.library_id,
        "pendingCount": job.pending_count,
        "items": job.items.iter().map(|item| json!({
            "jobId": item.job_id,
            "itemId": item.item_id,
            "status": item.status,
            "candidateCount": item.candidate_count,
            "error": item.error,
            "updatedAt": item.updated_at,
        })).collect::<Vec<_>>(),
    })
}

fn metadata_reidentify_job_summary_json(
    job: &crate::storage::StoredMetadataReidentifyJob,
) -> Value {
    json!({
        "id": job.id,
        "status": job.status,
        "mode": job.mode,
        "processedCount": job.processed_count,
        "totalCount": job.total_count,
        "error": job.error,
        "createdAt": job.created_at,
        "updatedAt": job.updated_at,
        "startedAt": job.started_at,
        "finishedAt": job.finished_at,
        "cancelRequested": job.cancel_requested,
        "libraryId": job.library_id,
        "pendingCount": job.pending_count,
    })
}

fn metadata_reidentify_error(headers: &HeaderMap, error: MetadataReidentifyError) -> Response {
    match error {
        MetadataReidentifyError::InvalidItemCount
        | MetadataReidentifyError::InvalidRefreshMode
        | MetadataReidentifyError::InvalidSearch
        | MetadataReidentifyError::Candidate(MetadataCandidateError::InvalidSearch)
        | MetadataReidentifyError::Candidate(MetadataCandidateError::Tmdb(
            TmdbError::InvalidRequest(_),
        )) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "批量元数据匹配请求无效",
        )
        .into_response(),
        MetadataReidentifyError::ItemNotFound(_)
        | MetadataReidentifyError::JobNotFound
        | MetadataReidentifyError::Candidate(MetadataCandidateError::ItemNotFound) => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体条目或元数据匹配任务不存在",
        )
        .into_response(),
        MetadataReidentifyError::JobNotRetryable => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "该批量元数据匹配任务当前不可重试",
        )
        .into_response(),
        MetadataReidentifyError::JobNotCancelable => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "该批量元数据匹配任务当前不可取消",
        )
        .into_response(),
        MetadataReidentifyError::Candidate(MetadataCandidateError::InvalidCandidateJson(_)) => {
            api_error(
                headers,
                StatusCode::INTERNAL_SERVER_ERROR,
                lux::ApiErrorCode::Internal,
                "候选数据损坏",
            )
            .into_response()
        }
        MetadataReidentifyError::Candidate(MetadataCandidateError::Tmdb(_))
        | MetadataReidentifyError::Candidate(MetadataCandidateError::Scraper(_))
        | MetadataReidentifyError::Scraper(_)
        | MetadataReidentifyError::Selection(_)
        | MetadataReidentifyError::SelectionUnavailable
        | MetadataReidentifyError::LowConfidence
        | MetadataReidentifyError::Candidate(MetadataCandidateError::Storage(_))
        | MetadataReidentifyError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "批量元数据匹配暂时不可用",
        )
        .into_response(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetadataCandidateFailureKind {
    ItemNotFound,
    InvalidSearch,
    InvalidCandidateJson,
    TmdbInvalidRequest,
    TmdbUnavailable,
    ScraperUnavailable,
    StorageUnavailable,
}

impl MetadataCandidateFailureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ItemNotFound => "ITEM_NOT_FOUND",
            Self::InvalidSearch => "INVALID_SEARCH",
            Self::InvalidCandidateJson => "INVALID_CANDIDATE_JSON",
            Self::TmdbInvalidRequest => "TMDB_INVALID_REQUEST",
            Self::TmdbUnavailable => "TMDB_UNAVAILABLE",
            Self::ScraperUnavailable => "SCRAPER_UNAVAILABLE",
            Self::StorageUnavailable => "STORAGE_UNAVAILABLE",
        }
    }
}

fn metadata_candidate_failure_kind(error: &MetadataCandidateError) -> MetadataCandidateFailureKind {
    match error {
        MetadataCandidateError::ItemNotFound => MetadataCandidateFailureKind::ItemNotFound,
        MetadataCandidateError::InvalidSearch => MetadataCandidateFailureKind::InvalidSearch,
        MetadataCandidateError::InvalidCandidateJson(_) => {
            MetadataCandidateFailureKind::InvalidCandidateJson
        }
        MetadataCandidateError::Tmdb(TmdbError::InvalidRequest(_)) => {
            MetadataCandidateFailureKind::TmdbInvalidRequest
        }
        MetadataCandidateError::Tmdb(_) => MetadataCandidateFailureKind::TmdbUnavailable,
        MetadataCandidateError::Scraper(_) => MetadataCandidateFailureKind::ScraperUnavailable,
        MetadataCandidateError::Storage(_) => MetadataCandidateFailureKind::StorageUnavailable,
    }
}

fn metadata_candidate_error(headers: &HeaderMap, error: MetadataCandidateError) -> Response {
    let failure_kind = metadata_candidate_failure_kind(&error);
    let request_id = header_str(headers, "x-request-id").unwrap_or("unknown");
    tracing::warn!(
        event = "metadata_candidate_request_failed",
        error_kind = failure_kind.as_str(),
        request_id = %request_id,
        "metadata candidate request failed"
    );

    match error {
        MetadataCandidateError::ItemNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体条目不存在",
        )
        .into_response(),
        MetadataCandidateError::InvalidSearch => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "候选搜索条件无效",
        )
        .into_response(),
        MetadataCandidateError::InvalidCandidateJson(_) => api_error(
            headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "候选数据损坏",
        )
        .into_response(),
        MetadataCandidateError::Tmdb(TmdbError::InvalidRequest(_)) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "刮削器搜索条件无效",
        )
        .into_response(),
        MetadataCandidateError::Tmdb(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "刮削器暂时不可用，请稍后重试",
        )
        .into_response(),
        MetadataCandidateError::Scraper(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "刮削器暂时不可用，请稍后重试",
        )
        .into_response(),
        MetadataCandidateError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        )
        .into_response(),
    }
}

async fn admin_list_plugins(
    headers: HeaderMap,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_plugins_with_scope(headers, query, state, false).await
}

async fn admin_list_notification_providers(
    headers: HeaderMap,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
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
    let Some(plugins) = state.plugins.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match plugins.list_notification_plugins(offset, limit).await {
        Ok(page) => Json(plugin_page_json(&page)).into_response(),
        Err(error) => plugin_error(&headers, error),
    }
}

async fn admin_list_chapter_sources(
    headers: HeaderMap,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
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
    let Some(plugins) = state.plugins.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match plugins.list_chapter_sources(offset, limit).await {
        Ok(page) => Json(json!({
            "sources": page.sources.iter().map(|source| json!({
                "id": source.id,
                "name": source.name,
                "description": source.description,
                "version": source.version,
                "capabilities": source.capabilities,
                "lookup": source.lookup,
            })).collect::<Vec<_>>(),
            "total": page.total,
            "page": page.offset / page.limit + 1,
            "pageSize": page.limit,
        }))
        .into_response(),
        Err(error) => plugin_error(&headers, error),
    }
}

async fn admin_plugin_store(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    Json(json!({
        "url": plugins.plugin_store_source().await,
        "defaultUrl": crate::application::plugin_store::DEFAULT_PLUGIN_STORE_URL,
    }))
    .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginStoreUpdateRequest {
    url: String,
}

async fn admin_update_plugin_store(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<PluginStoreUpdateRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match plugins.update_plugin_store_source(&request.url).await {
        Ok(url) => {
            record_audit_event(
                &state,
                &headers,
                "PLUGIN_STORE_UPDATED",
                Some("plugin_store"),
                None,
                "{}",
            )
            .await;
            Json(json!({
                "url": url,
                "defaultUrl": crate::application::plugin_store::DEFAULT_PLUGIN_STORE_URL,
            }))
            .into_response()
        }
        Err(crate::application::plugins::PluginServiceError::Store(
            crate::application::plugin_store::PluginStoreError::InvalidSource,
        )) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "插件商店地址无效",
        )
        .into_response(),
        Err(error) => plugin_error(&headers, error),
    }
}

async fn admin_list_installed_plugins(
    headers: HeaderMap,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_plugins_with_scope(headers, query, state, true).await
}

async fn admin_list_plugins_with_scope(
    headers: HeaderMap,
    query: LuxPageQuery,
    state: AppState,
    installed_only: bool,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
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
    let Some(plugins) = state.plugins.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let page = if installed_only {
        plugins.list_installed(offset, limit).await
    } else {
        plugins.list(offset, limit).await
    };
    match page {
        Ok(page) => Json(plugin_page_json(&page)).into_response(),
        Err(error) => plugin_error(&headers, error),
    }
}

async fn admin_install_plugin(
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match plugins.install(&plugin_id).await {
        Ok(result) => {
            record_audit_event(
                &state,
                &headers,
                "PLUGIN_INSTALLED",
                Some("plugin"),
                Some(&plugin_id),
                "{}",
            )
            .await;
            let status = if result.was_installed {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            (
                status,
                Json(json!({ "plugin": plugin_json(&result.plugin) })),
            )
                .into_response()
        }
        Err(error) => plugin_error(&headers, error),
    }
}

async fn admin_uninstall_plugin(
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match plugins.uninstall(&plugin_id).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "PLUGIN_UNINSTALLED",
                Some("plugin"),
                Some(&plugin_id),
                "{}",
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => plugin_error(&headers, error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginEnabledRequest {
    enabled: bool,
}

async fn admin_update_plugin_enabled(
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PluginEnabledRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match plugins.set_enabled(&plugin_id, request.enabled).await {
        Ok(plugin) => {
            record_audit_event(
                &state,
                &headers,
                if request.enabled {
                    "PLUGIN_ENABLED"
                } else {
                    "PLUGIN_DISABLED"
                },
                Some("plugin"),
                Some(&plugin_id),
                "{}",
            )
            .await;
            (
                StatusCode::OK,
                Json(json!({ "plugin": plugin_json(&plugin) })),
            )
                .into_response()
        }
        Err(error) => plugin_error(&headers, error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginConfigRequest {
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    preferred_language: Option<String>,
    #[serde(default)]
    language_fallback_enabled: Option<bool>,
    #[serde(default)]
    fallback_languages: Option<Vec<String>>,
    #[serde(default)]
    alternate_api_enabled: Option<bool>,
    #[serde(default)]
    api_base_url: Option<String>,
    #[serde(flatten)]
    values: serde_json::Map<String, Value>,
}

async fn admin_update_plugin_config(
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PluginConfigRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let api_key = request.api_key.as_deref().map(str::trim);
    let result = if plugin_id == crate::application::plugins::TMDB_PLUGIN_ID
        || plugin_id == crate::application::plugins::TMDB_DYNAMIC_PLUGIN_ID
    {
        plugins
            .update_config(
                &plugin_id,
                crate::application::plugins::TmdbConfigUpdate {
                    api_key,
                    preferred_language: request.preferred_language.as_deref(),
                    language_fallback_enabled: request.language_fallback_enabled,
                    fallback_languages: request.fallback_languages,
                    alternate_api_enabled: request.alternate_api_enabled,
                    api_base_url: request.api_base_url.as_deref(),
                },
            )
            .await
    } else {
        plugins
            .update_dynamic_config(&plugin_id, request.values)
            .await
    };
    match result {
        Ok(plugin) => {
            if plugin_id == crate::application::plugins::TMDB_PLUGIN_ID
                || plugin_id == crate::application::plugins::TMDB_DYNAMIC_PLUGIN_ID
            {
                if let Some(tmdb) = state.tmdb.as_ref() {
                    if let Some(api_key) = api_key {
                        tmdb.set_api_key((!api_key.is_empty()).then_some(api_key))
                            .await;
                    }
                }
                plugins.restart(&plugin_id).await;
            }
            record_audit_event(
                &state,
                &headers,
                "PLUGIN_CONFIG_UPDATED",
                Some("plugin"),
                Some(&plugin_id),
                "{}",
            )
            .await;
            (
                StatusCode::OK,
                Json(json!({ "plugin": plugin_json(&plugin) })),
            )
                .into_response()
        }
        Err(error) => plugin_error(&headers, error),
    }
}

async fn validate_scraper_selection(
    headers: &HeaderMap,
    state: &AppState,
    scraper_id: Option<&str>,
) -> Result<(), Response> {
    let Some(plugins) = state.plugins.as_ref() else {
        return Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response());
    };
    plugins
        .validate_selection(scraper_id)
        .await
        .map_err(|error| plugin_error(headers, error).into_response())
}

async fn validate_chapter_source_selection(
    headers: &HeaderMap,
    state: &AppState,
    kind: LibraryKind,
    chapter_source_id: Option<&str>,
) -> Result<(), Response> {
    let Some(chapter_source_id) = chapter_source_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    if !kind.supports_chapter_source() {
        return Err(api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "片头片尾数据源只能用于剧集或混合媒体库",
        )
        .into_response());
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE.into_response());
    };
    match plugins
        .has_available_chapter_source(chapter_source_id)
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(plugin_error(
            headers,
            PluginServiceError::Unavailable(chapter_source_id.to_owned()),
        )),
        Err(error) => Err(plugin_error(headers, error)),
    }
}

async fn admin_create_library(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<CreateLibraryRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(libraries) = state.libraries.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    if let Err(response) =
        validate_scraper_selection(&headers, &state, request.scraper_id.as_deref()).await
    {
        return response;
    }
    let kind = match request.kind.parse::<LibraryKind>() {
        Ok(kind) => kind,
        Err(_error) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库类型无效",
            )
            .into_response();
        }
    };
    if let Err(response) = validate_chapter_source_selection(
        &headers,
        &state,
        kind,
        request.chapter_source_id.as_deref(),
    )
    .await
    {
        return response;
    }
    match libraries
        .create_library_with_scraper_and_chapter_source(
            &request.name,
            kind,
            request.realtime_watch_enabled,
            request.scraper_id.as_deref(),
            request.chapter_source_id.as_deref(),
            request.realtime_metadata_auto_match_enabled,
        )
        .await
    {
        Ok(library) => {
            if let Some(home) = state.home.as_ref() {
                home.invalidate();
            }
            if let Some(plugins) = state.plugins.as_ref()
                && let Err(error) = plugins.sync_chapter_detection_scheduled_tasks().await
            {
                return plugin_error(&headers, error);
            }
            let library_id = library.id.to_string();
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_CREATED",
                Some("library"),
                Some(&library_id),
                "{}",
            )
            .await;
            (
                StatusCode::CREATED,
                Json(json!({
                    "library": library_json(&library, &[]),
                    "warnings": []
                })),
            )
                .into_response()
        }
        Err(error) => library_error(&headers, error),
    }
}

async fn admin_update_library(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<UpdateLibraryRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidLibraryId(error.to_string()),
            );
        }
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let kind = match request.kind.as_deref() {
        Some(value) => match value.parse::<LibraryKind>() {
            Ok(kind) => Some(kind),
            Err(_error) => {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "媒体库类型无效",
                )
                .into_response();
            }
        },
        None => None,
    };
    let current_library = match libraries.get_library(library_id).await {
        Ok(library) => library,
        Err(error) => return library_error(&headers, error),
    };
    if let Err(response) = validate_chapter_source_selection(
        &headers,
        &state,
        kind.unwrap_or(current_library.kind),
        request
            .chapter_source_id
            .as_ref()
            .and_then(|value| value.as_deref()),
    )
    .await
    {
        return response;
    }
    let effective_kind = kind.unwrap_or(current_library.kind);
    let chapter_source_id = request.chapter_source_id.clone().or_else(|| {
        (!effective_kind.supports_chapter_source() && current_library.chapter_source_id.is_some())
            .then_some(None)
    });
    let media_strategy_json = match request.media_strategy.as_ref() {
        None => None,
        Some(None) => Some(None),
        Some(Some(strategy)) => {
            if !validate_media_strategy(strategy) {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "媒体库策略无效",
                )
                .into_response();
            }
            if let Some(scraper_id) = strategy.scraper_id.as_deref() {
                if let Err(response) =
                    validate_scraper_selection(&headers, &state, Some(scraper_id)).await
                {
                    return response;
                }
            }
            match serde_json::to_string(strategy) {
                Ok(value) => Some(Some(value)),
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
    };
    let settings = LibrarySettingsPatch {
        name: request.name,
        kind,
        is_enabled: request.is_enabled,
        realtime_watch_enabled: request.realtime_watch_enabled,
        realtime_metadata_auto_match_enabled: request.realtime_metadata_auto_match_enabled,
        reconciliation_schedule: request.reconciliation_schedule,
        metadata_schedule: request.metadata_schedule,
        scraper_id: request.scraper_id.clone(),
        chapter_source_id,
        media_strategy_json,
        scan_concurrency: request.scan_concurrency,
        probe_concurrency: request.probe_concurrency,
    };
    if let Some(scraper_id) = request
        .scraper_id
        .as_ref()
        .and_then(|value| value.as_deref())
    {
        if let Err(response) = validate_scraper_selection(&headers, &state, Some(scraper_id)).await
        {
            return response;
        }
    }
    match libraries.update_settings(library_id, settings).await {
        Ok(view) => {
            if let Some(home) = state.home.as_ref() {
                home.invalidate();
            }
            if let Some(plugins) = state.plugins.as_ref()
                && let Err(error) = plugins.sync_chapter_detection_scheduled_tasks().await
            {
                return plugin_error(&headers, error);
            }
            let target_id = library_id.to_string();
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_UPDATED",
                Some("library"),
                Some(&target_id),
                "{}",
            )
            .await;
            (
                StatusCode::OK,
                Json(json!({
                    "library": library_json(&view.library, &view.roots)
                })),
            )
                .into_response()
        }
        Err(error) => library_error(&headers, error),
    }
}

async fn admin_update_library_cover(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidLibraryId(error.to_string()),
            );
        }
    };
    let Some(covers) = state.library_covers.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    match covers.store(library_id, content_type, &body).await {
        Ok(cover) => {
            let target_id = library_id.to_string();
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_COVER_UPDATED",
                Some("library"),
                Some(&target_id),
                "{}",
            )
            .await;
            (
                StatusCode::OK,
                Json(json!({
                    "library": {
                        "id": target_id,
                        "coverImageUrl": format!("/api/v1/libraries/{target_id}/cover"),
                        "contentType": cover.content_type,
                        "contentLength": cover.content_length,
                    }
                })),
            )
                .into_response()
        }
        Err(error) => library_cover_error(&headers, error),
    }
}

async fn admin_run_auto_library_cover(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidLibraryId(error.to_string()),
            );
        }
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let library_id_text = library_id.to_string();
    match database
        .find_scheduled_task_config(
            "LIBRARY",
            &library_id_text,
            crate::application::library_covers::AUTO_LIBRARY_COVER_TASK_TYPE,
        )
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "自动媒体库封面任务尚未注册",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(covers) = state.library_covers.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let worker_library_id = library_id;
    tokio::spawn(async move {
        match covers.run_manually(worker_library_id).await {
            Ok(crate::application::library_covers::AutoLibraryCoverResult::Generated) => {
                tracing::info!(library_id = %worker_library_id, "manual automatic library cover generation completed");
            }
            Ok(
                crate::application::library_covers::AutoLibraryCoverResult::ExistingCover
                | crate::application::library_covers::AutoLibraryCoverResult::BelowThreshold
                | crate::application::library_covers::AutoLibraryCoverResult::TaskNotRegistered
                | crate::application::library_covers::AutoLibraryCoverResult::AlreadyHandled,
            ) => {
                tracing::info!(library_id = %worker_library_id, "manual automatic library cover generation skipped");
            }
            Err(error) => {
                tracing::warn!(library_id = %worker_library_id, %error, "manual automatic library cover generation failed");
            }
        }
    });
    record_audit_event(
        &state,
        &headers,
        "LIBRARY_COVER_GENERATION_STARTED",
        Some("library"),
        Some(&library_id_text),
        "{\"mode\":\"manual\"}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "status": "QUEUED",
            "taskType": crate::application::library_covers::AUTO_LIBRARY_COVER_TASK_TYPE,
        })),
    )
        .into_response()
}

async fn admin_delete_library_root(
    headers: HeaderMap,
    Path((library_id, root_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidLibraryId(error.to_string()),
            );
        }
    };
    let root_id = match root_id.parse::<crate::domain::ids::LibraryRootId>() {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidRootId(error.to_string()),
            );
        }
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match libraries.delete_root(library_id, root_id).await {
        Ok(()) => {
            if let Some(home) = state.home.as_ref() {
                home.invalidate();
            }
            let target_id = root_id.to_string();
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_ROOT_DELETED",
                Some("library_root"),
                Some(&target_id),
                "{}",
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => library_error(&headers, error),
    }
}

async fn admin_delete_library(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidLibraryId(error.to_string()),
            );
        }
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match libraries.delete_library(library_id).await {
        Ok(()) => {
            if let Some(home) = state.home.as_ref() {
                home.invalidate();
            }
            let target_id = library_id.to_string();
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_DELETED",
                Some("library"),
                Some(&target_id),
                "{}",
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => library_error(&headers, error),
    }
}

async fn admin_add_library_root(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<AddLibraryRootRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidLibraryId(error.to_string()),
            );
        }
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match libraries.add_root(library_id, &request.path).await {
        Ok(result) => {
            if let Some(home) = state.home.as_ref() {
                home.invalidate();
            }
            let target_id = library_id.to_string();
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_ROOT_ADDED",
                Some("library"),
                Some(&target_id),
                "{}",
            )
            .await;
            let scan_job = match spawn_library_scan(&state, library_id).await {
                Ok(job) => job,
                Err(error) => {
                    tracing::warn!(library_id = %target_id, %error, "library root added but automatic scan could not be started");
                    None
                }
            };
            (
                StatusCode::CREATED,
                Json(json!({
                    "root": root_json(&result.root),
                    "warnings": result.warnings.iter().map(|warning| warning.as_str()).collect::<Vec<_>>(),
                    "scanJob": scan_job.as_ref().map(scan_job_json),
                })),
            )
                .into_response()
        }
        Err(error) => library_error(&headers, error),
    }
}

fn library_cover_error(headers: &HeaderMap, error: LibraryCoverError) -> Response {
    match error {
        LibraryCoverError::UnsupportedContentType(_) | LibraryCoverError::InvalidContent { .. } => {
            api_error(
                headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "封面图格式无效，仅支持 JPEG、PNG 或 WebP",
            )
            .into_response()
        }
        LibraryCoverError::TooLarge { .. } => api_error(
            headers,
            StatusCode::PAYLOAD_TOO_LARGE,
            lux::ApiErrorCode::InvalidRequest,
            "封面图不能超过 5 MiB",
        )
        .into_response(),
        LibraryCoverError::LibraryNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体库不存在",
        )
        .into_response(),
        LibraryCoverError::InvalidPath => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "媒体库封面路径无效",
        )
        .into_response(),
        LibraryCoverError::Io { .. }
        | LibraryCoverError::ImageWrite(_)
        | LibraryCoverError::Storage(_)
        | LibraryCoverError::FontNotFound
        | LibraryCoverError::Render(_)
        | LibraryCoverError::RenderPanicked
        | LibraryCoverError::GeneratedCoverRace
        | LibraryCoverError::GenerationUnavailable => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "媒体库封面保存失败",
        )
        .into_response(),
    }
}

fn library_error(headers: &HeaderMap, error: LibraryServiceError) -> Response {
    let (status, code, message) = match error {
        LibraryServiceError::InvalidName
        | LibraryServiceError::InvalidSchedule
        | LibraryServiceError::InvalidConcurrency
        | LibraryServiceError::InvalidLibraryId(_)
        | LibraryServiceError::InvalidRootId(_)
        | LibraryServiceError::InvalidKind(_)
        | LibraryServiceError::InvalidScraperId
        | LibraryServiceError::InvalidChapterSourceId => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体库请求无效",
        ),
        LibraryServiceError::LibraryNotFound => (
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体库不存在",
        ),
        LibraryServiceError::LibraryBusy => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "媒体库仍有扫描任务运行",
        ),
        LibraryServiceError::RootNotFound => (
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体根路径不存在",
        ),
        LibraryServiceError::DuplicateRoot => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::LibraryRootDuplicate,
            "根路径已存在",
        ),
        LibraryServiceError::OverlappingRoot => (
            StatusCode::UNPROCESSABLE_ENTITY,
            lux::ApiErrorCode::LibraryRootOverlap,
            "根路径与同一媒体库的其他路径重叠",
        ),
        LibraryServiceError::Path(error) => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::LibraryPathUnavailable,
            if error.is_unavailable() {
                "媒体目录不可用"
            } else {
                "媒体目录无效"
            },
        ),
        LibraryServiceError::RootNotFoundAfterInsert => (
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "媒体根路径保存失败",
        ),
        LibraryServiceError::Storage(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        ),
    };
    api_error(headers, status, code, message).into_response()
}

fn plugin_error(headers: &HeaderMap, error: PluginServiceError) -> Response {
    match error {
        PluginServiceError::UnknownPlugin(_) => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "插件不存在",
        )
        .into_response(),
        PluginServiceError::Unavailable(_) => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::PluginUnavailable,
            "插件尚未安装或配置完成",
        )
        .into_response(),
        PluginServiceError::InvalidConfig => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "插件配置无效",
        )
        .into_response(),
        PluginServiceError::ConfigIo(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "插件配置保存失败",
        )
        .into_response(),
        PluginServiceError::Runtime(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::PluginUnavailable,
            "插件进程暂时不可用",
        )
        .into_response(),
        PluginServiceError::Store(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::PluginUnavailable,
            "插件商店暂时不可用",
        )
        .into_response(),
        PluginServiceError::InvalidResponse => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::PluginUnavailable,
            "插件返回的数据无效",
        )
        .into_response(),
        PluginServiceError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        )
        .into_response(),
    }
}

fn danmaku_service_error(headers: &HeaderMap, error: DanmakuServiceError) -> Response {
    match error {
        DanmakuServiceError::InvalidConcurrency
        | DanmakuServiceError::InvalidProviderUrl(_)
        | DanmakuServiceError::ProviderNotConfigured => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "弹幕匹配配置无效或尚未配置",
        )
        .into_response(),
        DanmakuServiceError::AlreadyActive => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "已有弹幕匹配任务运行",
        )
        .into_response(),
        DanmakuServiceError::LibraryNotFound
        | DanmakuServiceError::SourceNotFound
        | DanmakuServiceError::JobNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "弹幕匹配对象不存在",
        )
        .into_response(),
        DanmakuServiceError::NotRetryable => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "弹幕匹配任务当前不可重试",
        )
        .into_response(),
        DanmakuServiceError::WorkerFailed | DanmakuServiceError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "弹幕匹配服务暂时不可用",
        )
        .into_response(),
    }
}

fn strm_probe_error(headers: &HeaderMap, error: StrmProbeError) -> Response {
    match error {
        StrmProbeError::InvalidLibraryCount
        | StrmProbeError::InvalidConcurrency
        | StrmProbeError::InvalidThumbnailPosition => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "STRM 探测参数无效",
        )
        .into_response(),
        StrmProbeError::AlreadyActive => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "已有 STRM 探测任务运行",
        )
        .into_response(),
        StrmProbeError::NotRetryable => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "任务当前不可重试",
        )
        .into_response(),
        StrmProbeError::LibraryNotFound | StrmProbeError::JobNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "STRM 探测对象不存在",
        )
        .into_response(),
        StrmProbeError::WorkerFailed | StrmProbeError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "STRM 探测服务暂时不可用",
        )
        .into_response(),
        StrmProbeError::Plugin(PluginServiceError::InvalidConfig) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "STRM 插件配置无效",
        )
        .into_response(),
        StrmProbeError::Plugin(error) => plugin_error(headers, error),
    }
}

fn plugin_page_json(page: &PluginPage) -> Value {
    json!({
        "plugins": page.plugins.iter().map(plugin_json).collect::<Vec<_>>(),
        "total": page.total,
        "page": page.offset / page.limit + 1,
        "pageSize": page.limit,
    })
}

async fn admin_run_plugin(
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if plugin_id != crate::application::plugins::MEDIA_INFO_PLUGIN_ID {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "该插件不支持后台运行",
        )
        .into_response();
    }
    let Some(service) = state.strm_probe.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let jobs = match service.create_configured_jobs().await {
        Ok(jobs) => jobs,
        Err(error) => return strm_probe_error(&headers, error),
    };
    for job in &jobs {
        let worker = service.clone();
        let job_id = job.id.clone();
        tokio::spawn(async move {
            if let Err(error) = worker.run(&job_id).await {
                tracing::error!(job_id = %job_id, %error, "configured STRM probe job stopped");
            }
        });
    }
    let operation_id = jobs
        .first()
        .map(|job| job.operation_id.clone())
        .unwrap_or_default();
    record_audit_event(
        &state,
        &headers,
        "STRM_PROBE_STARTED",
        Some("strm_probe_operation"),
        Some(&operation_id),
        &format!(r#"{{"jobCount":{}}}"#, jobs.len()),
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "operationId": operation_id,
            "jobs": jobs,
        })),
    )
        .into_response()
}

fn plugin_json(plugin: &crate::application::plugins::PluginView) -> Value {
    json!({
        "id": plugin.id,
        "name": plugin.name,
        "description": plugin.description,
        "category": plugin.category,
        "version": plugin.version,
        "runtime": plugin.runtime,
        "capabilities": plugin.capabilities,
        "status": plugin.status,
        "running": plugin.running,
        "lastError": plugin.last_error,
        "installed": plugin.installed,
        "enabled": plugin.enabled,
        "configured": plugin.configured,
        "available": plugin.available,
        "unavailableReason": plugin.unavailable_reason,
        "configurable": plugin.configurable,
        "configFields": plugin.config_fields.iter().map(|field| json!({
            "key": field.key,
            "label": field.label,
            "type": field.input_type,
            "required": field.required,
            "sensitive": field.sensitive,
            "description": field.description,
            "multiple": field.multiple,
            "optionsSource": field.options_source,
            "defaultValue": field.default_value,
            "minimum": field.minimum,
            "maximum": field.maximum,
            "options": field.options.iter().map(|option| json!({
                "value": option.value,
                "label": option.label,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "configValues": plugin.config_values,
        "configSource": plugin.config_source,
    })
}

fn library_json(library: &LibraryRecord, roots: &[LibraryRootRecord]) -> Value {
    json!({
        "id": library.id.to_string(),
        "name": library.name,
        "kind": library.kind.as_str(),
        "scraperId": library.scraper_id,
        "chapterSourceId": library.chapter_source_id,
        "coverImageUrl": library_cover_url(library),
        "isEnabled": library.is_enabled,
        "realtimeWatchEnabled": library.realtime_watch_enabled,
        "realtimeMetadataAutoMatchEnabled": library.realtime_metadata_auto_match_enabled,
        "incrementalSchedule": library.incremental_schedule,
        "reconciliationSchedule": library.reconciliation_schedule,
        "metadataSchedule": library.metadata_schedule,
        "mediaStrategy": library_media_strategy_json(library.media_strategy_json.as_deref()),
        "scanConcurrency": library.scan_concurrency,
        "probeConcurrency": library.probe_concurrency,
        "lastScanAt": library.last_scan_at,
        "roots": roots.iter().map(root_json).collect::<Vec<_>>(),
    })
}

fn library_media_strategy_json(value: Option<&str>) -> Option<Value> {
    let strategy = serde_json::from_str::<MediaStrategySettings>(value?).ok()?;
    serde_json::to_value(strategy).ok()
}

fn library_cover_url(library: &LibraryRecord) -> Option<String> {
    library
        .cover_image_path
        .as_ref()
        .map(|_| format!("/api/v1/libraries/{}/cover", library.id))
}

fn root_json(root: &LibraryRootRecord) -> Value {
    json!({
        "id": root.id.to_string(),
        "libraryId": root.library_id.to_string(),
        "canonicalPath": root.canonical_path,
        "displayPath": root.display_path,
        "isAvailable": root.is_available,
        "isWritable": root.is_writable,
        "lastCheckedAt": root.last_checked_at,
        "unavailableSince": root.unavailable_since,
        "scanCursor": root.scan_cursor,
    })
}

fn user_json(user: &UserRecord) -> Value {
    json!({
        "id": user.id.to_string(),
        "usernameNormalized": user.username_normalized,
        "displayName": user.display_name,
        "isDisabled": user.is_disabled,
        "isAdmin": user.is_admin,
        "canManageServer": user.can_manage_server,
        "canRemoteAccess": user.can_remote_access,
        "canDownload": user.can_download
    })
}

fn api_error(
    headers: &HeaderMap,
    status: StatusCode,
    code: lux::ApiErrorCode,
    message: &str,
) -> (StatusCode, Json<Value>) {
    let request_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    tracing::Span::current().record("errorCode", code.as_str());
    let body = lux::ApiError::new(code, message, request_id);
    (
        status,
        Json(json!({
            "error": {
                "code": body.code,
                "message": body.message,
                "requestId": body.request_id
            }
        })),
    )
}

fn user_avatar_error(headers: &HeaderMap, error: UserAvatarError) -> Response {
    match error {
        UserAvatarError::UnsupportedContentType | UserAvatarError::InvalidContent => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "头像格式无效，仅支持 JPEG、PNG 或 WebP",
        )
        .into_response(),
        UserAvatarError::TooLarge { .. } => api_error(
            headers,
            StatusCode::PAYLOAD_TOO_LARGE,
            lux::ApiErrorCode::InvalidRequest,
            "头像不能超过 5 MiB",
        )
        .into_response(),
        UserAvatarError::InvalidPath(_) | UserAvatarError::Io { .. } => api_error(
            headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "头像暂时无法保存",
        )
        .into_response(),
    }
}

fn request_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value.split(';').find_map(|part| {
                let (cookie_name, cookie_value) = part.trim().split_once('=')?;
                (cookie_name == name).then(|| cookie_value.to_owned())
            })
        })
}

fn secure_cookie_for_request(headers: &HeaderMap, policy: &RemoteAccessPolicy) -> bool {
    policy.is_secure_request(
        header_str(headers, "x-lux-peer-ip"),
        header_str(headers, "x-forwarded-proto"),
    )
}

fn build_cookie(
    name: &str,
    value: &str,
    http_only: bool,
    max_age: Option<i64>,
    secure: bool,
) -> Option<HeaderValue> {
    let mut cookie = format!("{name}={value}; Path=/;");
    if secure {
        cookie.push_str(" Secure;");
    }
    cookie.push_str(" SameSite=Lax");
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    if let Some(max_age) = max_age {
        cookie.push_str(&format!("; Max-Age={max_age}"));
    }
    HeaderValue::from_str(&cookie).ok()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        CatalogSort, EmbyClientCompatibility, FilmlyImageCompatMode, MediaStrategySettings,
        MetadataCandidateFailureKind, build_cookie, catalog_filter_from_emby, emby_collection_type,
        emby_media_source_json, emby_media_source_json_with_resolver, emby_media_stream_item_id,
        emby_media_stream_json, emby_playback_info_item_id,
        filmly_image_compat_mode_from_env_value, is_catalog_aggregation_path,
        is_emby_legacy_strm_path, is_emby_media_stream_segment, is_emby_playback_callback_path,
        is_emby_subtitle_path, is_emby_video_path, is_filmly_user_agent,
        is_registered_emby_video_path, lux_catalog_source_json, metadata_candidate_failure_kind,
        normalize_filmly_null_languages, normalize_strm_http_location, playback_client_label,
        playback_identifier_prefix, record_activity_event, safe_trace_path,
        secure_cookie_for_request, validate_media_strategy,
    };
    use crate::application::admin_events::{AdminEventHub, AdminEventScope};
    use crate::application::candidates::MetadataCandidateError;
    use crate::application::catalog::{CatalogChapter, CatalogSource, CatalogStream};
    use crate::application::scraper::ScraperError;
    use crate::application::setup::SetupService;
    use crate::application::tmdb::TmdbError;
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
    fn emby_collection_type_uses_mixed_for_mixed_libraries() {
        assert_eq!(
            emby_collection_type(LibraryKind::Mixed, EmbyClientCompatibility::Generic),
            Some("mixed")
        );
    }

    #[test]
    fn emby_collection_type_uses_legacy_null_for_vidhub_mixed_libraries() {
        assert_eq!(
            emby_collection_type(LibraryKind::Mixed, EmbyClientCompatibility::VidHub),
            None
        );
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
                MetadataCandidateError::Tmdb(TmdbError::InvalidRequest("secret detail".to_owned())),
                MetadataCandidateFailureKind::TmdbInvalidRequest,
                "TMDB_INVALID_REQUEST",
            ),
            (
                MetadataCandidateError::Tmdb(TmdbError::Timeout),
                MetadataCandidateFailureKind::TmdbUnavailable,
                "TMDB_UNAVAILABLE",
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
        assert!(body["DirectStreamUrl"].is_null());
        assert_eq!(body["DefaultAudioStreamIndex"], -1);
        assert!(body.get("Chapters").is_none());
        assert_eq!(body["MediaStreams"][0]["Width"], 1920);
        assert_eq!(body["MediaStreams"][0]["Height"], 1080);
        assert_eq!(body["MediaStreams"][0]["Profile"], "High");
    }

    #[test]
    fn filmly_episode_stream_normalization_replaces_only_null_languages() {
        let mut items = vec![json!({
            "MediaSources": [{
                "MediaStreams": [
                    {"Type": "Video", "Language": null},
                    {"Type": "Audio", "Language": "chi"},
                    {"Type": "Subtitle"}
                ]
            }]
        })];

        normalize_filmly_null_languages(&mut items);

        assert_eq!(
            items[0]["MediaSources"][0]["MediaStreams"][0]["Language"],
            "und"
        );
        assert_eq!(
            items[0]["MediaSources"][0]["MediaStreams"][1]["Language"],
            "chi"
        );
        assert!(
            items[0]["MediaSources"][0]["MediaStreams"][2]
                .get("Language")
                .is_none()
        );
    }

    #[test]
    fn filmly_episode_normalization_is_scoped_to_filmly_user_agents() {
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
    fn resolver_targets_use_a_protected_lux_stream_entrypoint() {
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

        assert_eq!(body["Protocol"], "Http");
        assert_eq!(body["IsRemote"], true);
        assert_eq!(body["SupportsDirectPlay"], true);
        assert_eq!(
            body["DirectStreamUrl"],
            "/Videos/item-1/source-1/stream.mkv"
        );
        assert_eq!(body["Path"], "/cloud/library/movie.mp4");
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
}
