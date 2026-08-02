pub mod lux;

use std::{
    path::{Component, Path as FsPath, PathBuf},
    time::UNIX_EPOCH,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{AUTHORIZATION, COOKIE, SET_COOKIE},
    },
    response::{IntoResponse, Response},
    routing::{get, patch, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs;
use tower_http::{ServiceBuilderExt, request_id::MakeRequestUuid, trace::TraceLayer};

use crate::{
    COMMIT, VERSION,
    application::playback::{ByteRange, RangeError, parse_single_range},
    application::setup::{SetupError, SetupService},
    application::{
        access::{AccessPrincipal, MediaAccessService},
        candidates::{
            MetadataCandidateError, MetadataCandidatePage, MetadataCandidateService,
            MetadataSelectionError, MetadataSelectionMode, MetadataSelectionService,
        },
        catalog::{CatalogError, CatalogItem, CatalogPage, CatalogService},
        images::{ImageError, ImageService, ImageWriteService, normalize_image_type},
        libraries::{LibraryService, LibraryServiceError, LibrarySettingsPatch},
        scanner::{ScanJobError, ScanJobService},
    },
    auth::users::{UserRecord, UserStoreError},
    auth::{
        emby::{EmbyAuthService, EmbyDeviceInfo},
        sessions::WebAuthService,
    },
    config::Config,
    library::{LibraryKind, LibraryRecord, LibraryRootRecord},
    storage::{Database, NewPlaybackEvent},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};

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
    access: Option<MediaAccessService>,
    metadata_candidates: Option<MetadataCandidateService>,
    metadata_selection: Option<MetadataSelectionService>,
    scan_jobs: Option<ScanJobService>,
}

impl AppState {
    pub fn ready(
        config: Config,
        database: Database,
        setup: SetupService,
        auth: WebAuthService,
        emby_auth: EmbyAuthService,
    ) -> Self {
        let server_id = database.server_id().to_owned();
        let access = MediaAccessService::new(database.clone());
        let metadata_selection = ImageWriteService::new(database.clone())
            .ok()
            .map(|images| MetadataSelectionService::new(database.clone(), images));
        Self {
            database: Some(database.clone()),
            config_dir: Some(config.config_dir),
            server_id,
            setup: Some(setup),
            auth: Some(auth),
            emby_auth: Some(emby_auth),
            libraries: Some(LibraryService::new(database.clone())),
            catalog: Some(CatalogService::new(database.clone(), access.clone())),
            images: Some(ImageService::new(database.clone(), access.clone())),
            access: Some(access),
            metadata_candidates: Some(MetadataCandidateService::new(database.clone())),
            metadata_selection,
            scan_jobs: Some(ScanJobService::new(database.clone())),
        }
    }
}

pub fn app() -> Router {
    app_with_state(AppState::default())
}

pub fn app_with_state(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/api/v1/version", get(version))
        .route("/api/v1/setup/status", get(setup_status))
        .route("/api/v1/setup/complete", post(setup_complete))
        .route("/api/v1/auth/login", post(auth_login))
        .route("/api/v1/auth/logout", post(auth_logout))
        .route("/api/v1/auth/me", get(auth_me))
        .route(
            "/api/v1/admin/libraries",
            get(admin_list_libraries).post(admin_create_library),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}",
            patch(admin_update_library),
        )
        .route(
            "/api/v1/admin/metadata/pending",
            get(admin_list_pending_metadata),
        )
        .route(
            "/api/v1/admin/items/{item_id}/identify/candidates",
            get(admin_list_item_candidates),
        )
        .route(
            "/api/v1/admin/items/{item_id}/identify/candidates/{candidate_id}/select",
            post(admin_select_candidate),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/roots",
            post(admin_add_library_root),
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
            "/api/v1/admin/jobs/{job_id}/cancel",
            post(admin_cancel_scan),
        )
        .route(
            "/api/v1/admin/settings",
            get(admin_settings).patch(admin_update_settings),
        )
        .route("/api/v1/libraries", get(lux_list_libraries))
        .route(
            "/api/v1/libraries/{library_id}/items",
            get(lux_list_library_items),
        )
        .route("/api/v1/items/{item_id}", get(lux_get_item))
        .route(
            "/api/v1/items/{item_id}/images/{image_type}",
            get(lux_image).head(lux_image),
        )
        .route(
            "/api/v1/items/{item_id}/images/{image_type}/{image_index}",
            get(lux_image_at_index).head(lux_image_at_index),
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
        .merge(emby_routes())
        .nest("/emby", emby_routes())
        .with_state(state)
        .layer(
            tower::ServiceBuilder::new()
                .set_x_request_id(MakeRequestUuid)
                .layer(TraceLayer::new_for_http().make_span_with(
                    |request: &axum::http::Request<_>| {
                        tracing::info_span!(
                            "request",
                            method = %request.method(),
                            path = %safe_trace_path(request.uri()),
                            version = ?request.version(),
                        )
                    },
                ))
                .propagate_x_request_id(),
        )
}

fn safe_trace_path(uri: &axum::http::Uri) -> &str {
    uri.path()
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
        .route("/Users/{user_id}/Items/NextUp", get(emby_user_next_up))
        .route("/Users/{user_id}/Items", get(emby_user_items))
        .route("/Users/{user_id}/Items/{item_id}", get(emby_user_item))
        .route("/Shows/{series_id}/Seasons", get(emby_show_seasons))
        .route("/Shows/{series_id}/Episodes", get(emby_show_episodes))
        .route("/Items", get(emby_items))
        .route("/Items/{item_id}", get(emby_item))
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
    if let Err(status) = require_emby_token(&headers, &query, auth).await {
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
    match require_emby_token(&headers, &query, auth).await {
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
    let Some(auth) = state.emby_auth else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let device = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(EmbyDeviceInfo::parse)
        .unwrap_or_default();
    match auth
        .authenticate(&request.username, &request.password, &device)
        .await
    {
        Ok(Some(result)) => Json(json!({
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
        .into_response(),
        Ok(None) => StatusCode::UNAUTHORIZED.into_response(),
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
) -> Result<(), StatusCode> {
    resolve_emby_user_with_auth(headers, query, auth)
        .await
        .map(|_| ())
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
    resolve_emby_user_with_auth(headers, &query, auth).await
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
                    items.push(emby_library_view_json(&view.library, &state.server_id));
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
    let mut resume_items = Vec::new();
    for item in page.items {
        if !matches!(item.item_type.as_str(), "MOVIE" | "EPISODE") {
            continue;
        }
        let Some(item_state) = (match database.find_user_item_state(&user_id, &item.id).await {
            Ok(state) => state,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }) else {
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

async fn emby_catalog_page_for_user(
    state: &AppState,
    user_id: &str,
    page: &CatalogPage,
) -> Response {
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut items = Vec::with_capacity(page.items.len());
    for item in &page.items {
        let user_state = match database.find_user_item_state(user_id, &item.id).await {
            Ok(state) => state,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        items.push(emby_catalog_item_json_with_state(
            item,
            &state.server_id,
            user_state.as_ref(),
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
    if !query.include_item_types.as_deref().is_none_or(|types| {
        types
            .split(',')
            .any(|item_type| item_type.eq_ignore_ascii_case("Movie"))
    }) {
        return Json(json!({
            "Items": [],
            "TotalRecordCount": 0,
            "StartIndex": 0,
        }))
        .into_response();
    }
    let (offset, limit) = match emby_page_params(query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let page = match query.parent_id.as_deref() {
        Some(parent_id) => {
            let Ok(parent_id) = parent_id.parse::<crate::domain::ids::LibraryId>() else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            catalog
                .list_library_items(principal, &parent_id.to_string(), offset, limit)
                .await
        }
        None => catalog.list_all_items(principal, offset, limit).await,
    };
    match page {
        Ok(page) => Json(emby_catalog_page_json(&page, &state.server_id)).into_response(),
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
    match database.find_user_item_state(&user_id, &item_id).await {
        Ok(state) => Json(json!({
            "itemId": item_id,
            "positionTicks": state.as_ref().map(|value| value.position_ticks).unwrap_or_default(),
            "isPlayed": state.as_ref().map(|value| value.is_played).unwrap_or(false),
            "isFavorite": state.as_ref().map(|value| value.is_favorite).unwrap_or(false),
            "playCount": state.as_ref().map(|value| value.play_count).unwrap_or_default(),
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LuxProgressRequest {
    position_ticks: i64,
    duration_ticks: Option<i64>,
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
    match database
        .record_playback_event(NewPlaybackEvent {
            user_id: &user_id,
            item_id: &item_id,
            media_source_id: None,
            play_session_id: &play_session_id,
            device_id: "lux-web",
            client: Some("Lux"),
            device_name: Some("Web"),
            state: "PLAYING",
            position_ticks: request.position_ticks,
            duration_ticks: request.duration_ticks,
            is_paused: false,
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

fn emby_catalog_page_json(page: &CatalogPage, server_id: &str) -> Value {
    json!({
        "Items": page.items.iter().map(|item| emby_catalog_item_json(item, server_id)).collect::<Vec<_>>(),
        "TotalRecordCount": page.total,
        "StartIndex": page.offset,
    })
}

fn emby_catalog_item_json(item: &CatalogItem, server_id: &str) -> Value {
    emby_catalog_item_json_with_state(item, server_id, None)
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
    json!({
        "Name": item.title,
        "OriginalTitle": item.original_title,
        "Id": item.id,
        "ServerId": server_id,
        "Type": emby_item_type(&item.item_type),
        "MediaType": "Video",
        "IsFolder": matches!(item.item_type.as_str(), "SERIES" | "SEASON"),
        "ParentId": item.parent_id,
        "SeriesId": item.series_id,
        "ParentIndexNumber": item.season_number,
        "Index": item.episode_number,
        "ProductionYear": item.production_year,
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
        "ImageTags": item
            .poster_image_tag
            .as_ref()
            .map(|tag| json!({"Primary": tag}))
            .unwrap_or_else(|| json!({})),
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
        "Container": source.container,
        "Size": source.size,
        "Bitrate": source.bitrate,
        "RunTimeTicks": source.duration_ticks,
        "Protocol": if is_remote { "Http" } else { "File" },
        "Type": "Default",
        "IsRemote": is_remote,
        "SupportsDirectPlay": direct_stream_url.is_some(),
        "SupportsDirectStream": false,
        "SupportsTranscoding": false,
        "DirectStreamUrl": direct_stream_url,
        "MediaStreams": source.streams.iter().map(|stream| json!({
            "Index": stream.index,
            "Type": emby_stream_type(&stream.stream_type),
            "Codec": stream.codec,
            "Language": stream.language,
            "DisplayTitle": stream.title,
            "IsExternal": stream.is_external,
            "IsDefault": stream.is_default,
            "IsForced": stream.is_forced,
        })).collect::<Vec<_>>(),
    })
}

fn emby_library_view_json(library: &LibraryRecord, server_id: &str) -> Value {
    json!({
        "Name": library.name,
        "Id": library.id,
        "ServerId": server_id,
        "Type": "CollectionFolder",
        "IsFolder": true,
        "CollectionType": "movies",
        "ImageTags": {},
    })
}

fn emby_item_type(item_type: &str) -> &'static str {
    match item_type {
        "MOVIE" => "Movie",
        "SERIES" => "Series",
        "SEASON" => "Season",
        "EPISODE" => "Episode",
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

    match database.schema_version().await {
        Ok(schema_version) => (
            StatusCode::OK,
            Json(json!({ "status": "ready", "schemaVersion": schema_version })),
        ),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
        ),
    }
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
    let Some(setup) = state.setup else {
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
}

async fn setup_complete(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<SetupCompleteRequest>,
) -> (StatusCode, Json<Value>) {
    let Some(setup) = state.setup else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        );
    };

    match setup
        .complete(&request.username, &request.display_name, &request.password)
        .await
    {
        Ok(user) => (
            StatusCode::CREATED,
            Json(json!({ "initialized": true, "user": user_json(&user) })),
        ),
        Err(SetupError::AlreadyCompleted) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::SetupAlreadyCompleted,
            "初始化已完成",
        ),
        Err(SetupError::UserStore(
            UserStoreError::InvalidUsername | UserStoreError::Password(_),
        )) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户名或密码无效",
        ),
        Err(SetupError::UserStore(_)) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "初始化暂时不可用",
        ),
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
    let Some(auth) = state.auth else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };

    let session = match auth.login(&request.username, &request.password).await {
        Ok(Some(session)) => session,
        Ok(None) => {
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

    let mut response_headers = HeaderMap::new();
    let Some(session_cookie) = build_cookie("lux_session", &session.session_token, true, None)
    else {
        return api_error(
            &headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "无法创建会话",
        )
        .into_response();
    };
    let Some(csrf_cookie) = build_cookie("lux_csrf", &session.csrf_token, false, None) else {
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
    match auth.resolve(&session_token).await {
        Ok(Some(session)) => Json(json!({ "user": user_json(&session.user) })).into_response(),
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
    if let Some(cookie) = build_cookie("lux_session", "", true, Some(0)) {
        response_headers.append(SET_COOKIE, cookie);
    }
    if let Some(cookie) = build_cookie("lux_csrf", "", false, Some(0)) {
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
        Ok(Some(session)) => Ok(session.user),
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
                    }));
                }
            }
            Json(json!({ "libraries": visible })).into_response()
        }
        Err(error) => library_error(&headers, error),
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
    match catalog
        .list_library_items(principal, &library_id, offset, limit)
        .await
    {
        Ok(page) => Json(lux_catalog_page_json(&page)).into_response(),
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
    match catalog.find_item(principal, &item_id).await {
        Ok(Some(item)) => Json(lux_catalog_item_json(&item)).into_response(),
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

async fn serve_media_file(
    state: &AppState,
    principal: AccessPrincipal,
    headers: &HeaderMap,
    method: &Method,
    item_id: &str,
    media_source_id: Option<&str>,
    requested_container: Option<&str>,
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
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    if requested_container.is_some_and(|container| {
        extension.as_deref()
            != Some(
                container
                    .trim_start_matches('.')
                    .to_ascii_lowercase()
                    .as_str(),
            )
    }) {
        return StatusCode::NOT_FOUND.into_response();
    }
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
    if headers
        .get("if-none-match")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|tag| tag.trim() == image.etag))
    {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header("ETag", &image.etag)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let Ok(file) = tokio::fs::File::open(&image.path).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        Body::from_stream(tokio_util::io::ReaderStream::new(file))
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", image.content_type)
        .header("Content-Length", image.content_length)
        .header("ETag", &image.etag)
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

fn lux_catalog_page_json(page: &CatalogPage) -> Value {
    json!({
        "items": page.items.iter().map(lux_catalog_item_json).collect::<Vec<_>>(),
        "total": page.total,
        "page": page.offset / page.limit + 1,
        "pageSize": page.limit,
    })
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
        "productionYear": item.production_year,
        "runtimeTicks": item.runtime_ticks,
        "imageTags": {
            "poster": item.poster_image_tag,
            "fanart": item.fanart_image_tag,
        },
        "mediaSources": item.media_sources.iter().map(lux_catalog_source_json).collect::<Vec<_>>(),
    })
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
}

#[derive(Deserialize)]
struct AddLibraryRootRequest {
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateLibraryRequest {
    realtime_watch_enabled: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    incremental_schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    reconciliation_schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    metadata_schedule: Option<Option<String>>,
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

async fn admin_settings(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.resume_settings().await {
        Ok((played_percent, minimum_ticks)) => Json(json!({
            "resumePlayedPercent": played_percent,
            "resumeMinTicks": minimum_ticks,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdatePlaybackSettingsRequest {
    resume_played_percent: Option<i64>,
    resume_min_ticks: Option<i64>,
}

async fn admin_update_settings(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<UpdatePlaybackSettingsRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let (current_percent, current_ticks) = match database.resume_settings().await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let percent = request.resume_played_percent.unwrap_or(current_percent);
    let minimum_ticks = request.resume_min_ticks.unwrap_or(current_ticks);
    if !(1..=100).contains(&percent) || minimum_ticks < 0 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match database.set_resume_settings(percent, minimum_ticks).await {
        Ok(()) => Json(json!({
            "resumePlayedPercent": percent,
            "resumeMinTicks": minimum_ticks,
        }))
        .into_response(),
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
        Ok(()) => Json(json!({
            "userId": user_id,
            "libraryId": library_id,
            "canView": request.can_view,
        }))
        .into_response(),
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
    tokio::spawn(async move {
        loop {
            match worker.run_batch(&job_id, 100).await {
                Ok(report) if report.completed => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job": scan_job_json(&job) })),
    )
        .into_response()
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
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(ScanJobError::JobNotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
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
        Ok(report) => Json(json!({
            "itemId": report.item_id,
            "candidateId": report.candidate_id,
            "mode": report.mode.as_str(),
            "status": report.status,
            "imageTypes": report.image_types,
        }))
        .into_response(),
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
        MetadataSelectionError::Nfo(_) | MetadataSelectionError::Image(_) => api_error(
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
        MetadataCandidateError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        )
        .into_response(),
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
        .create_library(&request.name, kind, request.realtime_watch_enabled)
        .await
    {
        Ok(library) => (
            StatusCode::CREATED,
            Json(json!({
                "library": library_json(&library, &[]),
                "warnings": []
            })),
        )
            .into_response(),
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
    let settings = LibrarySettingsPatch {
        realtime_watch_enabled: request.realtime_watch_enabled,
        incremental_schedule: request.incremental_schedule,
        reconciliation_schedule: request.reconciliation_schedule,
        metadata_schedule: request.metadata_schedule,
        scan_concurrency: request.scan_concurrency,
        probe_concurrency: request.probe_concurrency,
    };
    match libraries.update_settings(library_id, settings).await {
        Ok(view) => (
            StatusCode::OK,
            Json(json!({
                "library": library_json(&view.library, &view.roots)
            })),
        )
            .into_response(),
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
        Ok(result) => (
            StatusCode::CREATED,
            Json(json!({
                "root": root_json(&result.root),
                "warnings": result.warnings.iter().map(|warning| warning.as_str()).collect::<Vec<_>>()
            })),
        )
            .into_response(),
        Err(error) => library_error(&headers, error),
    }
}

fn library_error(headers: &HeaderMap, error: LibraryServiceError) -> Response {
    let (status, code, message) = match error {
        LibraryServiceError::InvalidName
        | LibraryServiceError::InvalidSchedule
        | LibraryServiceError::InvalidConcurrency
        | LibraryServiceError::InvalidLibraryId(_)
        | LibraryServiceError::InvalidRootId(_)
        | LibraryServiceError::InvalidKind(_) => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体库请求无效",
        ),
        LibraryServiceError::LibraryNotFound => (
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体库不存在",
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

fn library_json(library: &LibraryRecord, roots: &[LibraryRootRecord]) -> Value {
    json!({
        "id": library.id.to_string(),
        "name": library.name,
        "kind": library.kind.as_str(),
        "isEnabled": library.is_enabled,
        "realtimeWatchEnabled": library.realtime_watch_enabled,
        "incrementalSchedule": library.incremental_schedule,
        "reconciliationSchedule": library.reconciliation_schedule,
        "metadataSchedule": library.metadata_schedule,
        "scanConcurrency": library.scan_concurrency,
        "probeConcurrency": library.probe_concurrency,
        "lastScanAt": library.last_scan_at,
        "roots": roots.iter().map(root_json).collect::<Vec<_>>(),
    })
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

fn build_cookie(
    name: &str,
    value: &str,
    http_only: bool,
    max_age: Option<i64>,
) -> Option<HeaderValue> {
    let mut cookie = format!("{name}={value}; Path=/; Secure; SameSite=Lax");
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
    use super::safe_trace_path;
    use axum::http::Uri;

    #[test]
    fn trace_path_excludes_query_credentials() {
        let uri: Uri = "/System/Info?api_key=do-not-log".parse().unwrap();

        assert_eq!(safe_trace_path(&uri), "/System/Info");
    }
}
