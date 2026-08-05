pub mod lux;

use std::{
    collections::BTreeMap,
    path::{Component, Path as FsPath, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State},
    http::{
        HeaderMap, HeaderValue, Method, Request, StatusCode,
        header::{AUTHORIZATION, COOKIE, SET_COOKIE},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::fs;
use tower_http::{
    ServiceBuilderExt,
    request_id::MakeRequestUuid,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

use crate::{
    COMMIT, VERSION,
    application::danmaku::{DanmakuService, DanmakuServiceError, validate_provider_base_url},
    application::deletion::{MediaDeleteError, MediaDeleteService},
    application::downloads::{DownloadArtifact, DownloadError, DownloadService},
    application::playback::{ByteRange, RangeError, parse_single_range},
    application::probe::{FfprobeRunner, MediaProbeService},
    application::setup::{SetupError, SetupService},
    application::{
        access::{AccessPrincipal, MediaAccessService},
        candidates::{
            MetadataCandidateError, MetadataCandidatePage, MetadataCandidateService,
            MetadataSelectionError, MetadataSelectionMode, MetadataSelectionService,
        },
        catalog::{
            CatalogError, CatalogFilter, CatalogItem, CatalogPage, CatalogService, CatalogSort,
            normalize_search_like_query, normalize_search_query,
        },
        collections::{CollectionError, CollectionService},
        images::{
            ImageCandidateError, ImageCandidateService, ImageError, ImageService, ImageWriteError,
            ImageWriteService, normalize_image_type,
        },
        libraries::{LibraryService, LibraryServiceError, LibrarySettingsPatch},
        library_covers::{LibraryCoverError, LibraryCoverService, MAX_LIBRARY_COVER_BYTES},
        metadata::MetadataField,
        network_diagnostics::{NetworkDiagnostics, NetworkProbeResult, test_network},
        nfo::{MetadataWriteRequest, MetadataWriteService, NfoWriteError},
        people::{PeopleError, PeopleService},
        plugins::{PluginPage, PluginService, PluginServiceError},
        reidentify::{MetadataReidentifyError, MetadataReidentifyService},
        scanner::{ScanJob, ScanJobError, ScanJobService},
        scraper::ScraperResolver,
        settings::{
            read_danmaku_provider_url_async, read_network_proxy_url_async,
            write_danmaku_provider_url, write_network_proxy_url,
        },
        strm_probe::{StrmProbeError, StrmProbeService},
        thumbnails::ThumbnailService,
        tmdb::{TmdbClient, TmdbError},
        tmdb_plugin::{TmdbPluginClient, TmdbProvider},
    },
    auth::users::{UserRecord, UserStore, UserStoreError, UserUpdate},
    auth::{
        emby::{EmbyAuthService, EmbyDeviceInfo},
        sessions::WebAuthService,
    },
    config::Config,
    library::{LibraryKind, LibraryRecord, LibraryRootRecord},
    network::{
        RemoteAccessPolicy, normalize_proxy_url, proxy_url_from_env, proxy_url_has_credentials,
        redact_proxy_url,
    },
    security::LoginRateLimiter,
    storage::{Database, ExternalSubtitleUpdate, NewPlaybackEvent, StorageError},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
    process::Command,
};

#[derive(Clone, Default)]
pub struct AppState {
    database: Option<Database>,
    config_dir: Option<PathBuf>,
    server_id: String,
    setup: Option<SetupService>,
    auth: Option<WebAuthService>,
    emby_auth: Option<EmbyAuthService>,
    libraries: Option<LibraryService>,
    catalog: Option<CatalogService>,
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
    danmaku: Option<DanmakuService>,
    plugins: Option<PluginService>,
    scraper_resolver: Option<ScraperResolver>,
    tmdb: Option<TmdbProvider>,
    collections: Option<CollectionService>,
    people: Option<PeopleService>,
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
        let config_dir = config.config_dir.clone();
        let access = MediaAccessService::new(database.clone());
        let image_writes =
            ImageWriteService::new_with_proxy(database.clone(), network_proxy_url.clone()).ok();
        let library_covers = Some(LibraryCoverService::new(
            database.clone(),
            config.config_dir.join("library-covers"),
        ));
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
        let collections = Some(CollectionService::with_resolver(
            database.clone(),
            tmdb.clone(),
            scraper_resolver.clone(),
        ));
        let metadata_reidentify = Some(MetadataReidentifyService::with_resolver_and_selection(
            database.clone(),
            tmdb.clone(),
            scraper_resolver.clone(),
            metadata_selection.clone(),
        ));
        let image_candidates = Some(ImageCandidateService::with_resolver(
            database.clone(),
            tmdb.clone(),
            scraper_resolver.clone(),
        ));
        Self {
            database: Some(database.clone()),
            config_dir: Some(config_dir.clone()),
            server_id,
            setup: Some(setup),
            auth: Some(auth),
            emby_auth: Some(emby_auth),
            libraries: Some(LibraryService::new(database.clone())),
            catalog: Some(CatalogService::new(database.clone(), access.clone())),
            images: Some(ImageService::new(database.clone(), access.clone())),
            image_writes,
            image_candidates,
            library_covers,
            access: Some(access),
            metadata_candidates: Some(MetadataCandidateService::new(database.clone())),
            metadata_selection,
            metadata_writes: Some(MetadataWriteService::new(database.clone())),
            downloads: DownloadService::new_with_proxy(database.clone(), network_proxy_url.clone())
                .ok(),
            metadata_reidentify,
            deletion: Some(MediaDeleteService::new(database.clone())),
            probe: Some(MediaProbeService::new(
                database.clone(),
                FfprobeRunner::default(),
            )),
            thumbnails: Some(ThumbnailService::new(database.clone())),
            scan_jobs: Some(ScanJobService::new(database.clone())),
            strm_probe: Some(StrmProbeService::new(database.clone(), plugins.clone())),
            danmaku: Some(DanmakuService::new(
                database.clone(),
                config_dir.clone(),
                network_proxy_url.clone(),
            )),
            plugins: Some(plugins),
            scraper_resolver: Some(scraper_resolver),
            tmdb: Some(tmdb),
            collections,
            people: Some(PeopleService::new_with_proxy(config_dir, network_proxy_url)),
            remote_access: RemoteAccessPolicy::from_env(),
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
            self.collections = Some(CollectionService::with_resolver(
                database.clone(),
                tmdb.clone(),
                resolver.clone(),
            ));
            self.metadata_reidentify =
                Some(MetadataReidentifyService::with_resolver_and_selection(
                    database.clone(),
                    tmdb.clone(),
                    resolver.clone(),
                    self.metadata_selection.clone(),
                ));
            self.image_candidates = Some(ImageCandidateService::with_resolver(
                database, tmdb, resolver,
            ));
        } else {
            self.collections = Some(CollectionService::new(database.clone(), tmdb.clone()));
            self.metadata_reidentify = Some(MetadataReidentifyService::with_selection(
                database.clone(),
                tmdb.clone(),
                self.metadata_selection.clone(),
            ));
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
                        100,
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

    pub fn with_remote_access_policy(mut self, policy: RemoteAccessPolicy) -> Self {
        self.remote_access = policy;
        self
    }
}

pub fn app() -> Router {
    app_with_state(AppState::default())
}

pub fn app_with_state(state: AppState) -> Router {
    let web_root = web_root();
    Router::new()
        .route("/logo.svg", get(web_logo))
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/api/v1/version", get(version))
        .route("/api/v1/setup/status", get(setup_status))
        .route("/api/v1/setup/complete", post(setup_complete))
        .route("/api/v1/auth/login", post(auth_login))
        .route("/api/v1/auth/logout", post(auth_logout))
        .route("/api/v1/auth/me", get(auth_me))
        .route("/api/v1/auth/sessions", get(auth_sessions))
        .route(
            "/api/v1/auth/sessions/{session_id}",
            delete(auth_revoke_session),
        )
        .route(
            "/api/v1/admin/libraries",
            get(admin_list_libraries).post(admin_create_library),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}",
            patch(admin_update_library).delete(admin_delete_library),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/cover",
            put(admin_update_library_cover)
                .layer(DefaultBodyLimit::max(MAX_LIBRARY_COVER_BYTES as usize)),
        )
        .route("/api/v1/admin/plugins", get(admin_list_plugins))
        .route(
            "/api/v1/admin/plugins/installed",
            get(admin_list_installed_plugins),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/install",
            post(admin_install_plugin),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/config",
            put(admin_update_plugin_config),
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
            "/api/v1/admin/settings/network-proxy/test",
            post(admin_test_network_proxy),
        )
        .route("/api/v1/admin/health", get(admin_health))
        .route("/api/v1/admin/audit", get(admin_list_audit))
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
        .layer(middleware::from_fn(attach_peer_address))
        .layer(middleware::from_fn(normalize_empty_api_service_unavailable))
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
        .route("/Users/{user_id}/Views", get(emby_user_views))
        .route("/Users/{user_id}/Items/Resume", get(emby_user_resume))
        .route("/Users/{user_id}/Items/Latest", get(emby_user_latest))
        .route("/Users/{user_id}/Items/NextUp", get(emby_user_next_up))
        .route("/Users/{user_id}/Items", get(emby_user_items))
        .route("/Users/{user_id}/Items/{item_id}", get(emby_user_item))
        .route("/Shows/{series_id}/Seasons", get(emby_show_seasons))
        .route("/Shows/{series_id}/Episodes", get(emby_show_episodes))
        .route("/Items", get(emby_items))
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
    #[serde(rename = "api_key")]
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

async fn emby_public_system_info(State(state): State<AppState>) -> Json<Value> {
    let startup_wizard_completed = match state.setup.as_ref() {
        Some(setup) => setup.status().await.unwrap_or(false),
        None => false,
    };
    Json(json!({
        "LocalAddress": "",
        "ServerName": "Lux",
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
    Json(json!({
        "LocalAddress": "",
        "ServerName": "Lux",
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
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let Some(auth) = state.emby_auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match require_emby_token(&headers, &query, auth, &state).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(status) => status.into_response(),
    }
}

async fn emby_public_users(State(state): State<AppState>) -> Json<Value> {
    let Some(auth) = state.emby_auth else {
        return Json(json!([]));
    };
    let users = auth.public_users().await.unwrap_or_default();
    Json(Value::Array(users.iter().map(emby_user_json).collect()))
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
    let device = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(EmbyDeviceInfo::parse)
        .unwrap_or_default();
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
            Json(json!({
                "User": emby_user_json(&result.user),
                "SessionInfo": {
                    "Client": result.device.client,
                    "DeviceId": result.device.device_id,
                    "DeviceName": result.device.device,
                    "ApplicationVersion": result.device.version,
                    "UserId": result.user.id.to_string()
                },
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
    #[serde(rename = "api_key")]
    api_key: Option<String>,
}

async fn require_emby_token(
    headers: &HeaderMap,
    query: &EmbyTokenQuery,
    auth: &EmbyAuthService,
    state: &AppState,
) -> Result<(), StatusCode> {
    let user = resolve_emby_user_with_auth(headers, query, auth).await?;
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
) -> Result<UserRecord, StatusCode> {
    let token = headers
        .get("X-Emby-Token")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or_else(|| query.api_key.clone())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    match auth.resolve_token(&token).await {
        Ok(Some(user)) => Ok(user),
        Ok(None) => Err(StatusCode::UNAUTHORIZED),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn require_emby_user(
    headers: &HeaderMap,
    state: &AppState,
    api_key: Option<&str>,
) -> Result<UserRecord, StatusCode> {
    let Some(auth) = state.emby_auth.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let query = EmbyTokenQuery {
        api_key: api_key.map(str::to_owned),
    };
    let user = resolve_emby_user_with_auth(headers, &query, auth).await?;
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
    #[serde(rename = "api_key", default)]
    api_key: Option<String>,
    #[serde(rename = "UserId", default)]
    user_id: Option<String>,
    #[serde(rename = "ParentId", default)]
    parent_id: Option<String>,
    #[serde(rename = "IncludeItemTypes", default)]
    include_item_types: Option<String>,
    #[serde(rename = "SeasonId", default)]
    season_id: Option<String>,
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
}

fn catalog_filter_from_values(
    item_types: Option<&str>,
    years: Option<&str>,
    is_played: Option<bool>,
    is_favorite: Option<bool>,
    sort_by: Option<&str>,
    sort_order: Option<&str>,
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
                .filter_map(|value| match value.to_ascii_lowercase().as_str() {
                    "movie" => Some("MOVIE".to_owned()),
                    "series" | "show" => Some("SERIES".to_owned()),
                    "season" => Some("SEASON".to_owned()),
                    "episode" => Some("EPISODE".to_owned()),
                    "boxset" | "box_set" => Some("BOX_SET".to_owned()),
                    _ => None,
                })
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
        years,
        is_played,
        is_favorite,
        sort_by: match sort_by {
            Some(value) if value.eq_ignore_ascii_case("DateCreated") => CatalogSort::DateCreated,
            Some(value) if value.eq_ignore_ascii_case("PremiereDate") => CatalogSort::PremiereDate,
            Some(value)
                if value.eq_ignore_ascii_case("CommunityRating")
                    || value.eq_ignore_ascii_case("Rating") =>
            {
                CatalogSort::Rating
            }
            _ => CatalogSort::Name,
        },
        descending: sort_order.is_some_and(|value| value.eq_ignore_ascii_case("Descending")),
    }
}

fn catalog_filter_from_emby(query: &EmbyItemsQuery) -> CatalogFilter {
    catalog_filter_from_values(
        query.include_item_types.as_deref(),
        query.years.as_deref(),
        query.is_played,
        query.is_favorite,
        query.sort_by.as_deref(),
        query.sort_order.as_deref(),
    )
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
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let Some(libraries) = state.libraries.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match libraries.list_libraries().await {
        Ok(views) => {
            let mut items = Vec::new();
            for view in views {
                let can_view = match access
                    .can_view_library(principal, &view.library.id.to_string())
                    .await
                {
                    Ok(can_view) => can_view,
                    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                };
                if view.library.is_enabled && can_view {
                    let child_count = match database
                        .count_catalog_items(Some(&view.library.id.to_string()))
                        .await
                    {
                        Ok(count) => count,
                        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                    };
                    items.push(emby_library_view_json(
                        &view.library,
                        &state.server_id,
                        child_count,
                    ));
                }
            }
            let total = items.len();
            Json(json!({ "Items": items, "TotalRecordCount": total, "StartIndex": 0 }))
                .into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
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
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let page = match catalog.list_all_items(principal, 0, i64::MAX).await {
        Ok(page) => page,
        Err(CatalogError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            return StatusCode::FORBIDDEN.into_response();
        }
    };
    let (played_percent, minimum_ticks) = match database.resume_settings().await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let item_ids = page
        .items
        .iter()
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    let user_states = match database.list_user_item_states(&user_id, &item_ids).await {
        Ok(states) => states,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let mut resume_items = Vec::new();
    for item in page.items {
        if !matches!(item.item_type.as_str(), "MOVIE" | "EPISODE") {
            continue;
        }
        let Some(item_state) = user_states.get(&item.id).cloned() else {
            continue;
        };
        let runtime_ticks = item.runtime_ticks.or_else(|| {
            item.media_sources
                .iter()
                .find(|source| source.is_default)
                .or_else(|| item.media_sources.first())
                .and_then(|source| source.duration_ticks)
        });
        let Some(runtime_ticks) = runtime_ticks.filter(|value| *value > 0) else {
            continue;
        };
        let below_played_threshold = i128::from(item_state.position_ticks) * 100
            < i128::from(runtime_ticks) * i128::from(played_percent);
        if !item_state.is_played
            && item_state.position_ticks >= minimum_ticks
            && below_played_threshold
        {
            resume_items.push((item, item_state));
        }
    }
    resume_items.sort_by(|(left, left_state), (right, right_state)| {
        right_state
            .last_played_at
            .cmp(&left_state.last_played_at)
            .then_with(|| left.sort_title.cmp(&right.sort_title))
            .then_with(|| left.id.cmp(&right.id))
    });
    let total = resume_items.len();
    let items = resume_items
        .into_iter()
        .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        .take(usize::try_from(limit).unwrap_or(0))
        .map(|(item, item_state)| {
            emby_catalog_item_json_with_state(&item, &state.server_id, Some(&item_state))
        })
        .collect::<Vec<_>>();
    Json(json!({
        "Items": items,
        "TotalRecordCount": total,
        "StartIndex": offset,
    }))
    .into_response()
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
    query.sort_by = Some("DateCreated".to_owned());
    query.sort_order = Some("Descending".to_owned());
    emby_list_items(
        &headers,
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
        &query,
    )
    .await
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
    let (offset, limit) = match emby_page_params(&query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog
        .list_next_up(
            AccessPrincipal::new(user.id, user.is_admin),
            &user_id,
            offset,
            limit,
        )
        .await
    {
        Ok(page) => emby_catalog_page_for_user(&state, &user_id, &page).await,
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
        Ok(page) => emby_catalog_page_for_user(&state, &user.id.to_string(), &page).await,
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
    match catalog
        .list_series_episodes(
            AccessPrincipal::new(user.id, user.is_admin),
            &series_id,
            query.season_id.as_deref(),
            offset,
            limit,
        )
        .await
    {
        Ok(page) => emby_catalog_page_for_user(&state, &user.id.to_string(), &page).await,
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
        Ok(page) => emby_catalog_page_for_user(&state, &user.id.to_string(), &page).await,
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn emby_catalog_page_for_user(
    state: &AppState,
    user_id: &str,
    page: &CatalogPage,
) -> Response {
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let item_ids = page
        .items
        .iter()
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    let user_states = match database.list_user_item_states(user_id, &item_ids).await {
        Ok(states) => states,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let mut items = Vec::with_capacity(page.items.len());
    for item in &page.items {
        items.push(emby_catalog_item_json_with_state(
            item,
            &state.server_id,
            user_states.get(&item.id),
        ));
    }
    Json(json!({
        "Items": items,
        "TotalRecordCount": page.total,
        "StartIndex": page.offset,
    }))
    .into_response()
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
    emby_list_items(&headers, &state, principal, &query).await
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
    emby_list_items(&headers, &state, principal, &query).await
}

async fn emby_list_items(
    _headers: &HeaderMap,
    state: &AppState,
    principal: AccessPrincipal,
    query: &EmbyItemsQuery,
) -> Response {
    let (offset, limit) = match emby_page_params(query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let filter = catalog_filter_from_emby(query);
    let page = match query.parent_id.as_deref() {
        Some(parent_id) => {
            let Ok(parent_id) = parent_id.parse::<crate::domain::ids::LibraryId>() else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            catalog
                .list_library_items_filtered(
                    principal,
                    &parent_id.to_string(),
                    &filter,
                    offset,
                    limit,
                )
                .await
        }
        None => {
            catalog
                .list_all_items_filtered(principal, &filter, offset, limit)
                .await
        }
    };
    match page {
        Ok(page) => emby_catalog_page_for_user(state, &principal.user_id.to_string(), &page).await,
        Err(CatalogError::LibraryNotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(CatalogError::AccessDenied) => StatusCode::FORBIDDEN.into_response(),
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
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
    emby_item_response(&state, principal, &item_id).await
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
    emby_item_response(&state, principal, &item_id).await
}

async fn emby_item_response(
    state: &AppState,
    principal: AccessPrincipal,
    item_id: &str,
) -> Response {
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog.find_item(principal, item_id).await {
        Ok(Some(item)) => {
            let Some(database) = state.database.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            let user_id = principal.user_id.to_string();
            let user_state = match database.find_user_item_state(&user_id, item_id).await {
                Ok(state) => state,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            Json(emby_catalog_item_json_with_state(
                &item,
                &state.server_id,
                user_state.as_ref(),
            ))
            .into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            unreachable!("inaccessible item is returned as not found")
        }
    }
}

async fn emby_playback_info(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<EmbyStreamQuery>,
    State(state): State<AppState>,
) -> Response {
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
    Json(json!({
        "MediaSources": sources
            .into_iter()
            .map(|source| emby_media_source_json(&item.id, source))
            .collect::<Vec<_>>(),
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
struct PlaybackEventRequest {
    #[serde(rename = "ItemId", alias = "itemId")]
    item_id: String,
    #[serde(rename = "MediaSourceId", alias = "mediaSourceId")]
    media_source_id: Option<String>,
    #[serde(rename = "PlaySessionId", alias = "playSessionId")]
    play_session_id: Option<String>,
    #[serde(rename = "PositionTicks", alias = "positionTicks", default)]
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
        Err(status) => return status.into_response(),
    };
    if request.position_ticks < 0
        || request.duration_ticks.is_some_and(|duration| duration < 0)
        || request.item_id.is_empty()
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(access) = state.access.as_ref() else {
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
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
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
            Ok(false) => return StatusCode::NOT_FOUND.into_response(),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }
    let header_device = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(EmbyDeviceInfo::parse)
        .unwrap_or_default();
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
    let play_session_id = request
        .play_session_id
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{}:{device_id}", request.item_id));
    let user_id = user.id.to_string();
    match database
        .record_playback_event(NewPlaybackEvent {
            user_id: &user_id,
            item_id: &request.item_id,
            media_source_id,
            play_session_id: &play_session_id,
            device_id: &device_id,
            client,
            device_name,
            state: state_name,
            position_ticks: request.position_ticks,
            duration_ticks: request.duration_ticks,
            is_paused: request.is_paused || state_name == "PAUSED",
        })
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
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
    match database
        .record_playback_event(NewPlaybackEvent {
            user_id: &user_id,
            item_id: &item_id,
            media_source_id: None,
            play_session_id: &play_session_id,
            device_id: "lux-web",
            client: Some("Lux"),
            device_name: Some("Web"),
            state: playback_state.as_str(),
            position_ticks: request.position_ticks,
            duration_ticks: request.duration_ticks,
            is_paused: matches!(playback_state, LuxPlaybackState::Paused),
        })
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
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
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
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
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
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
) -> Value {
    let default_source = item
        .media_sources
        .iter()
        .find(|source| source.is_default)
        .or_else(|| item.media_sources.first());
    let runtime_ticks = item
        .runtime_ticks
        .or_else(|| default_source.and_then(|source| source.duration_ticks));
    let mut image_tags = serde_json::Map::new();
    if let Some(tag) = item.poster_image_tag.as_ref() {
        image_tags.insert("Primary".to_owned(), json!(tag));
    }
    if let Some(tag) = item.logo_image_tag.as_ref() {
        image_tags.insert("Logo".to_owned(), json!(tag));
    }
    json!({
        "Name": item.title,
        "OriginalTitle": item.original_title,
        "Id": item.id,
        "ServerId": server_id,
        "Type": emby_item_type(&item.item_type),
        "MediaType": "Video",
        "IsFolder": matches!(item.item_type.as_str(), "SERIES" | "SEASON" | "BOX_SET"),
        "CollectionType": (item.item_type == "BOX_SET").then_some("movies"),
        "ParentId": item.parent_id,
        "SeriesId": item.series_id,
        "ParentIndexNumber": item.season_number,
        "Index": item.episode_number,
        "ProductionYear": item.production_year,
        "CommunityRating": item.rating,
        "Overview": item.overview,
        "RunTimeTicks": runtime_ticks,
        "Container": default_source.and_then(|source| source.container.clone()),
        "Size": default_source.and_then(|source| source.size),
        "Bitrate": default_source.and_then(|source| source.bitrate),
        "MediaSources": item
            .media_sources
            .iter()
            .map(|source| emby_media_source_json(&item.id, source))
            .collect::<Vec<_>>(),
        "ImageTags": image_tags,
        "BackdropImageTags": item
            .fanart_image_tag
            .as_ref()
            .map(|tag| json!([tag]))
            .unwrap_or_else(|| json!([])),
        "UserData": {
            "PlaybackPositionTicks": user_state.map(|state| state.position_ticks).unwrap_or_default(),
            "PlayCount": user_state.map(|state| state.play_count).unwrap_or_default(),
            "IsFavorite": user_state.map(|state| state.is_favorite).unwrap_or(false),
            "Played": user_state.map(|state| state.is_played).unwrap_or(false),
        },
    })
}

fn emby_media_source_json(
    item_id: &str,
    source: &crate::application::catalog::CatalogSource,
) -> Value {
    let direct_stream_url = if source.source_kind == "LOCAL_FILE" {
        let suffix = source
            .container
            .as_deref()
            .map(|container| format!(".{container}"))
            .unwrap_or_default();
        Some(format!("/Videos/{item_id}/{}/stream{suffix}", source.id))
    } else {
        source.external_url.clone()
    };
    let is_remote = source.source_kind == "STRM_URL";
    json!({
        "Id": source.id,
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
        "Protocol": if is_remote { "Http" } else { "File" },
        "Type": "Default",
        "IsRemote": is_remote,
        "SupportsDirectPlay": direct_stream_url.is_some(),
        "SupportsDirectStream": false,
        "SupportsTranscoding": false,
        "DirectStreamUrl": direct_stream_url,
        "MediaStreams": source
            .streams
            .iter()
            .map(emby_media_stream_json)
            .collect::<Vec<_>>(),
    })
}

fn emby_media_stream_json(stream: &crate::application::catalog::CatalogStream) -> Value {
    let mut value = json!({
        "Index": stream.index,
        "Type": emby_stream_type(&stream.stream_type),
        "Codec": stream.codec,
        "Language": stream.language,
        "DisplayTitle": stream.title,
        "IsExternal": stream.is_external,
        "IsDefault": stream.is_default,
        "IsForced": stream.is_forced,
    });
    if let Value::Object(object) = &mut value {
        for (key, detail) in &stream.details {
            object.entry(key.clone()).or_insert_with(|| detail.clone());
        }
    }
    value
}

fn emby_library_view_json(library: &LibraryRecord, server_id: &str, child_count: i64) -> Value {
    json!({
        "Name": library.name,
        "Id": library.id,
        "ServerId": server_id,
        "Type": "CollectionFolder",
        "IsFolder": true,
        "CollectionType": emby_collection_type(library.kind),
        "ChildCount": child_count,
        "ImageTags": library
            .cover_image_tag
            .as_ref()
            .map(|tag| json!({"Primary": tag}))
            .unwrap_or_else(|| json!({})),
        "BackdropImageTags": [],
    })
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

fn emby_user_json(user: &UserRecord) -> Value {
    json!({
        "Id": user.id.to_string(),
        "Name": user.display_name,
        "HasPassword": true,
        "Policy": { "IsAdministrator": user.is_admin }
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
                    .create_library(
                        &first_library.name,
                        kind,
                        first_library.realtime_watch_enabled,
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
    let Some(auth) = state.auth.as_ref() else {
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
    match auth.resolve(&session_token).await {
        Ok(Some(session)) => {
            if state.remote_access.is_remote(
                header_str(&headers, "x-lux-peer-ip"),
                header_str(&headers, "x-forwarded-for"),
            ) && !session.user.can_remote_access
            {
                return api_error(
                    &headers,
                    StatusCode::FORBIDDEN,
                    lux::ApiErrorCode::PermissionDenied,
                    "当前账户不允许远程访问",
                )
                .into_response();
            }
            Json(json!({ "user": user_json(&session.user) })).into_response()
        }
        Ok(None) => api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response(),
        Err(_) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "认证暂时不可用",
        )
        .into_response(),
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
}

async fn require_web_user(headers: &HeaderMap, state: &AppState) -> Result<UserRecord, Response> {
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
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let continue_watching = match catalog
        .list_next_up(principal, &user.id.to_string(), 0, 10)
        .await
    {
        Ok(page) => page,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let recently_added = match catalog.list_recently_added(principal, 0, 12).await {
        Ok(page) => page,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let recommended = match catalog
        .list_recommended(principal, &user.id.to_string(), 12)
        .await
    {
        Ok(items) => items,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let latest_groups = match catalog.list_recently_added_by_library(principal, 12).await {
        Ok(groups) => groups,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let latest_items = latest_groups
        .iter()
        .flat_map(|(_, items)| items.iter().cloned())
        .collect::<Vec<_>>();
    let latest_values = match lux_catalog_items_json_for_user(
        database,
        &user.id.to_string(),
        &latest_items,
    )
    .await
    {
        Ok(items) => items,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let mut latest_by_library = BTreeMap::<String, Vec<Value>>::new();
    for (item, value) in latest_items.iter().zip(latest_values) {
        latest_by_library
            .entry(item.library_id.clone())
            .or_default()
            .push(value);
    }
    let views = match libraries.list_libraries().await {
        Ok(views) => views,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut visible = Vec::new();
    for view in views {
        if !view.library.is_enabled {
            continue;
        }
        match access
            .can_view_library(principal, &view.library.id.to_string())
            .await
        {
            Ok(true) => visible.push(json!({
                "id": view.library.id,
                "name": view.library.name,
                "kind": view.library.kind.as_str(),
                "coverImageUrl": library_cover_url(&view.library),
                "latest": latest_by_library
                    .get(&view.library.id.to_string())
                    .cloned()
                    .unwrap_or_default(),
            })),
            Ok(false) => {}
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }
    let continue_watching_items = match lux_catalog_items_json_for_user(
        database,
        &user.id.to_string(),
        &continue_watching.items,
    )
    .await
    {
        Ok(items) => items,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let recently_added_items = match lux_catalog_items_json_for_user(
        database,
        &user.id.to_string(),
        &recently_added.items,
    )
    .await
    {
        Ok(items) => items,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let recommended_items =
        match lux_catalog_items_json_for_user(database, &user.id.to_string(), &recommended).await {
            Ok(items) => items,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    Json(json!({
        "continueWatching": continue_watching_items,
        "continueWatchingTotal": continue_watching.total,
        "recentlyAdded": recently_added_items,
        "recentlyAddedTotal": recently_added.total,
        "recommended": recommended_items,
        "libraries": visible,
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
struct EmbySearchQuery {
    #[serde(rename = "api_key")]
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
            Json(json!({ "libraries": visible })).into_response()
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
        Err(LibraryCoverError::Io { .. } | LibraryCoverError::ImageWrite(_)) => {
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
    let filter = catalog_filter_from_values(
        query.item_type.as_deref(),
        query.year.map(|year| year.to_string()).as_deref(),
        query.is_played,
        query.is_favorite,
        query.sort_by.as_deref(),
        query.sort_order.as_deref(),
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
                        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                    },
                    None => Vec::new(),
                };
                let mut body = lux_catalog_item_json_with_user_state(&item, user_state.as_ref());
                if let Value::Object(object) = &mut body {
                    object.insert("actors".to_owned(), json!(actors));
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
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    if normalize_image_type(&image_type) == Some("POSTER")
        && let Some(response) =
            serve_emby_library_cover(&state, principal, &headers, &method, &item_id, 0).await
    {
        return response;
    }
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

async fn emby_image_at_index(
    headers: HeaderMap,
    method: Method,
    Path((item_id, image_type, image_index)): Path<(String, String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let Ok(image_index) = image_index.parse::<i64>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if normalize_image_type(&image_type) == Some("POSTER")
        && let Some(response) =
            serve_emby_library_cover(&state, principal, &headers, &method, &item_id, image_index)
                .await
    {
        return response;
    }
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

#[derive(Deserialize, Default)]
struct EmbyStreamQuery {
    #[serde(rename = "api_key")]
    api_key: Option<String>,
    #[serde(alias = "mediaSourceId", alias = "MediaSourceId")]
    media_source_id: Option<String>,
}

async fn emby_stream(
    headers: HeaderMap,
    method: Method,
    Path(item_id): Path<String>,
    Query(query): Query<EmbyStreamQuery>,
    State(state): State<AppState>,
) -> Response {
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
    Query(query): Query<EmbyStreamQuery>,
    State(state): State<AppState>,
) -> Response {
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
    Query(query): Query<EmbyStreamQuery>,
    State(state): State<AppState>,
) -> Response {
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
    Query(query): Query<EmbyStreamQuery>,
    State(state): State<AppState>,
) -> Response {
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
    Query(query): Query<EmbyStreamQuery>,
    State(state): State<AppState>,
) -> Response {
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
    let source = match media_source_id {
        Some(source_id) => {
            database
                .find_media_source_path_by_id(item_id, source_id)
                .await
        }
        None => database.find_media_source_path(item_id).await,
    };
    let source = match source {
        Ok(Some(source)) => source,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
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
    principal: AccessPrincipal,
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
    let item_ids = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
    let states = database.list_user_item_states(user_id, &item_ids).await?;
    let mut values = Vec::with_capacity(items.len());
    for item in items {
        values.push(lux_catalog_item_json_with_user_state(
            item,
            states.get(&item.id),
        ));
    }
    Ok(values)
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
    if let Err(response) = require_web_user(&headers, &state).await {
        return response;
    }
    let Some(people) = state.people.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let image = match people.profile_image(&person_id).await {
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
    #[serde(default)]
    realtime_watch_enabled: bool,
    scraper_id: Option<String>,
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
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    incremental_schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    reconciliation_schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    metadata_schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    scraper_id: Option<Option<String>>,
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
    let network_proxy = network_proxy_settings(&state).await;
    let danmaku = danmaku_settings(&state).await;
    Json(json!({
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

fn validate_media_strategy(settings: &MediaStrategySettings) -> bool {
    valid_strategy_code(&settings.metadata_language, 32)
        && valid_strategy_code(&settings.image_language, 32)
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
            .map(|value| valid_strategy_code(value, 64))
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
    let percent = request.resume_played_percent.unwrap_or(current_percent);
    let minimum_ticks = request.resume_min_ticks.unwrap_or(current_ticks);
    let media_strategy = request.media_strategy.unwrap_or(current_media_strategy);
    if !(1..=100).contains(&percent)
        || minimum_ticks < 0
        || !validate_media_strategy(&media_strategy)
    {
        return StatusCode::BAD_REQUEST.into_response();
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
                &job_id, 100, probe, metadata, thumbnails,
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
    let job = match scan_jobs.create_movie_scan_job(library_id).await {
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
                &job_id, 100, probe, metadata, thumbnails,
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
            CollectionError::Tmdb(_) | CollectionError::Scraper(_) | CollectionError::Storage(_),
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StrmProbeRequest {
    #[serde(default)]
    library_ids: Vec<String>,
    #[serde(default = "default_strm_probe_concurrency")]
    concurrency: i64,
    #[serde(default)]
    include_ready: bool,
    #[serde(default)]
    write_sidecars: bool,
}

const fn default_strm_probe_concurrency() -> i64 {
    2
}

async fn admin_start_strm_probe(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<StrmProbeRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_ids = match request
        .library_ids
        .iter()
        .map(|value| value.parse::<crate::domain::ids::LibraryId>())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(ids) => ids,
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
    let Some(service) = state.strm_probe.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let jobs = match service
        .create_jobs(
            &library_ids,
            request.concurrency,
            request.include_ready,
            request.write_sidecars,
        )
        .await
    {
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

const LIBRARY_SCHEDULE_TASK_TYPES: [&str; 3] =
    ["INCREMENTAL_SCAN", "RECONCILIATION_SCAN", "METADATA_PARSE"];

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
    if owner_type != "LIBRARY" {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "当前只支持媒体库计划",
        )
        .into_response();
    }
    let task_type = request.task_type.trim().to_ascii_uppercase();
    if !LIBRARY_SCHEDULE_TASK_TYPES.contains(&task_type.as_str()) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "任务类型无效",
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
    let enabled = request.is_enabled.unwrap_or(request.schedule.is_some());
    let schedule = if enabled {
        request.schedule.map(|value| value.trim().to_owned())
    } else {
        None
    };
    let mut settings = LibrarySettingsPatch::default();
    match task_type.as_str() {
        "INCREMENTAL_SCAN" => settings.incremental_schedule = Some(schedule),
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
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let task = match database
        .find_scheduled_task_config("LIBRARY", &library_id.to_string(), &task_type)
        .await
    {
        Ok(Some(task)) => task,
        Ok(None) | Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
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
                100,
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
        "cancelRequested": job.cancel_requested,
        "error": job.error,
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
        "ownerType": task.owner_type,
        "ownerId": task.owner_id,
        "ownerName": owner_name,
        "taskType": task.task_type,
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
        "cancelRequested": job.cancel_requested,
        "error": job.error,
    })
}

async fn require_admin(
    headers: &HeaderMap,
    state: &AppState,
    require_csrf: bool,
) -> Result<(), Response> {
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
    if !session.user.can_manage_server {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "没有服务器管理权限",
        )
        .into_response());
    }
    if require_csrf {
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

async fn record_audit_event(
    state: &AppState,
    headers: &HeaderMap,
    event_type: &str,
    target_type: Option<&str>,
    target_id: Option<&str>,
    metadata_json: &str,
) {
    let (Some(auth), Some(database), Some(session_token)) = (
        state.auth.as_ref(),
        state.database.as_ref(),
        request_cookie(headers, "lux_session"),
    ) else {
        return;
    };
    let Ok(Some(session)) = auth.resolve(&session_token).await else {
        return;
    };
    let actor_user_id = session.user.id.to_string();
    let _ = database
        .insert_audit_event(crate::storage::NewAuditEvent {
            actor_user_id: Some(&actor_user_id),
            event_type,
            target_type,
            target_id,
            metadata_json,
        })
        .await;
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

async fn admin_health(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let schema_version = match database.schema_version().await {
        Ok(version) => version,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
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
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        None => Vec::new(),
    };
    let jobs = match database.list_scan_jobs(None, 0, 10_000).await {
        Ok(jobs) => json!({
            "scanRunning": jobs.iter().filter(|job| matches!(job.status.as_str(), "PENDING" | "RUNNING")).count(),
            "scanFailed": jobs.iter().filter(|job| job.status == "FAILED").count(),
        }),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let metadata_reidentify_running = match database.list_active_metadata_reidentify_job_ids().await
    {
        Ok(ids) => ids.len(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let status = if database_writable && config_available && config_writable && ffprobe_available {
        "ok"
    } else {
        "degraded"
    };
    Json(json!({
        "status": status,
        "schemaVersion": schema_version,
        "database": {
            "status": if database_writable { "ok" } else { "degraded" },
            "journalMode": "wal",
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
    .into_response()
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

fn metadata_candidate_error(headers: &HeaderMap, error: MetadataCandidateError) -> Response {
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
    let result = plugins
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
        .await;
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
    match libraries
        .create_library_with_scraper(
            &request.name,
            kind,
            request.realtime_watch_enabled,
            request.scraper_id.as_deref(),
        )
        .await
    {
        Ok(library) => {
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
        incremental_schedule: request.incremental_schedule,
        reconciliation_schedule: request.reconciliation_schedule,
        metadata_schedule: request.metadata_schedule,
        scraper_id: request.scraper_id.clone(),
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
        | LibraryCoverError::Storage(_) => api_error(
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
        | LibraryServiceError::InvalidScraperId => (
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
        StrmProbeError::InvalidLibraryCount | StrmProbeError::InvalidConcurrency => api_error(
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
        "coverImageUrl": library_cover_url(library),
        "isEnabled": library.is_enabled,
        "realtimeWatchEnabled": library.realtime_watch_enabled,
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
        build_cookie, emby_media_source_json, lux_catalog_source_json, safe_trace_path,
        secure_cookie_for_request,
    };
    use crate::application::catalog::{CatalogSource, CatalogStream};
    use crate::network::RemoteAccessPolicy;
    use axum::http::{HeaderMap, HeaderValue, Uri};

    #[test]
    fn direct_http_cookie_is_not_marked_secure() {
        let headers = HeaderMap::new();

        assert!(!secure_cookie_for_request(
            &headers,
            &RemoteAccessPolicy::default()
        ));
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
        headers.insert("x-lux-peer-ip", HeaderValue::from_static("10.0.0.2"));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        let policy = RemoteAccessPolicy::from_cidrs(["10.0.0.0/8"])
            .expect("trusted proxy CIDR should be valid");

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
    fn untrusted_forwarded_https_does_not_mark_cookie_secure() {
        let mut headers = HeaderMap::new();
        headers.insert("x-lux-peer-ip", HeaderValue::from_static("10.0.0.2"));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));

        assert!(!secure_cookie_for_request(
            &headers,
            &RemoteAccessPolicy::default()
        ));
    }

    #[test]
    fn trace_path_excludes_query_credentials() {
        let uri: Uri = "/System/Info?api_key=do-not-log".parse().unwrap();

        assert_eq!(safe_trace_path(&uri), "/System/Info");
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
        };

        let body = emby_media_source_json("item-1", &source);
        assert_eq!(body["Path"], "https://example.invalid/media.mkv");
        assert_eq!(body["Size"], 1_234_567);
        assert_eq!(body["MediaStreams"][0]["Width"], 1920);
        assert_eq!(body["MediaStreams"][0]["Height"], 1080);
        assert_eq!(body["MediaStreams"][0]["Profile"], "High");
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
        };

        let body = lux_catalog_source_json(&source);
        assert_eq!(body["streams"][0]["details"]["Width"], 1920);
    }
}
