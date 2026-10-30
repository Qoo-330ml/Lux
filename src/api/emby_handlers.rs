use super::*;

use crate::application::{
    plugin_protocol::{PluginEmbyRouteRequest, PluginEmbyRouteResponse, PluginMediaInfoTarget},
    probe::{media_probe_result_from_rpc, parse_media_info_json},
};
use crate::storage::MediaInfoChapterUpdate;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use quick_xml::{Reader, escape::unescape, events::Event};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::domain::ids::UserId;

pub(super) async fn emby_sync_media_info(
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    // The route imports media information into the catalog, so an empty body is not a reason to
    // skip the administrator check.
    if !auth_principal.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(plugins) = state.plugins() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = "/Items/SyncMediaInfo";
    let target = match plugins.emby_route_target("POST", path).await {
        Ok(Some(target)) => target,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let host_capabilities = vec!["media.info.import".to_owned()];
    let route_request = PluginEmbyRouteRequest {
        method: "POST".to_owned(),
        path: path.to_owned(),
        query: raw_query.as_deref().and_then(sanitize_plugin_route_query),
        headers: filtered_plugin_route_headers(&headers),
        body_base64: BASE64.encode(&body),
        host_capabilities: host_capabilities.clone(),
    };
    let route_response = match plugins.call_emby_route(&target, route_request).await {
        Ok(response) => response,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let PluginEmbyRouteResponse {
        status_code,
        headers,
        body_base64,
        media_info_import,
    } = route_response;
    let body = match BASE64.decode(body_base64) {
        Ok(body) => body,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let status = match StatusCode::from_u16(status_code) {
        Ok(status) => status,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if status == StatusCode::OK {
        let route_response = PluginEmbyRouteResponse {
            status_code,
            headers: headers.clone(),
            body_base64: String::new(),
            media_info_import: media_info_import.clone(),
        };
        if !route_response.validate_media_info_import(&host_capabilities, &target.capabilities) {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let (path, probe, chapters) = if let Some(operation) = media_info_import {
            let path = match resolve_media_info_target(&state, &operation.target).await {
                Ok(Some(path)) => path,
                Ok(None) | Err(_) => return StatusCode::BAD_REQUEST.into_response(),
            };
            let probe = media_probe_result_from_rpc(operation.media);
            let chapters = operation
                .chapters
                .into_iter()
                .map(|chapter| MediaInfoChapterUpdate {
                    start_position_ticks: chapter.start_position_ticks,
                    name: chapter.name,
                    chapter_index: chapter.chapter_index,
                })
                .collect();
            (path, probe, chapters)
        } else if !body.is_empty() {
            let path = raw_query.as_deref().and_then(|query| {
                url::form_urlencoded::parse(query.as_bytes()).find_map(|(key, value)| {
                    key.eq_ignore_ascii_case("path").then(|| value.into_owned())
                })
            });
            let path = if let Some(path) = path {
                path
            } else {
                let Some(id) = raw_query.as_deref().and_then(|query| {
                    url::form_urlencoded::parse(query.as_bytes()).find_map(|(key, value)| {
                        key.eq_ignore_ascii_case("id").then(|| value.into_owned())
                    })
                }) else {
                    return StatusCode::BAD_REQUEST.into_response();
                };
                let id = emby_internal_id(&id);
                let database = require_service!(state.database);
                let Some(source) = database
                    .find_strm_source_by_emby_id(&id)
                    .await
                    .ok()
                    .flatten()
                else {
                    return StatusCode::BAD_REQUEST.into_response();
                };
                format!("{}/{}", source.root_path, source.relative_path)
            };
            let probe = match parse_media_info_json(&body) {
                Ok(probe) => probe,
                Err(_) => return StatusCode::BAD_REQUEST.into_response(),
            };
            let chapters = match parse_media_info_chapters(&body) {
                Ok(chapters) => chapters,
                Err(_) => return StatusCode::BAD_REQUEST.into_response(),
            };
            (path, probe, chapters)
        } else {
            return StatusCode::OK.into_response();
        };
        let service = require_service!(state.probe);
        if service
            .import_media_info_for_absolute_path(&path, &probe, &chapters)
            .await
            .is_err()
        {
            return StatusCode::BAD_REQUEST.into_response();
        }
    }
    let mut response = Response::builder().status(status);
    for (name, value) in headers {
        if !matches!(
            name.to_ascii_lowercase().as_str(),
            "cache-control" | "content-type" | "location"
        ) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            response = response.header(name, value);
        }
    }
    response
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn resolve_media_info_target(
    state: &AppState,
    target: &PluginMediaInfoTarget,
) -> Result<Option<String>, ()> {
    let Some(database) = state.database.as_ref() else {
        return Err(());
    };
    let source = if let Some(path) = target.path.as_deref() {
        database
            .find_strm_source_by_absolute_path(path)
            .await
            .map_err(|_| ())?
            .or(database
                .find_strm_source_by_path_suffix(path)
                .await
                .map_err(|_| ())?)
    } else if let Some(id) = target
        .media_source_id
        .as_deref()
        .or(target.item_id.as_deref())
    {
        database
            .find_strm_source_by_emby_id(&emby_internal_id(id))
            .await
            .map_err(|_| ())?
    } else {
        return Ok(None);
    };
    Ok(source.map(|source| format!("{}/{}", source.root_path, source.relative_path)))
}

fn parse_media_info_chapters(bytes: &[u8]) -> Result<Vec<MediaInfoChapterUpdate>, ()> {
    let document: Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    let bundle = document
        .as_array()
        .and_then(|bundles| bundles.first())
        .and_then(Value::as_object)
        .ok_or(())?;
    let values = bundle.get("Chapters").and_then(Value::as_array).ok_or(())?;
    if values.len() > 512 {
        return Err(());
    }
    let mut chapters = Vec::with_capacity(values.len());
    let mut indexes = std::collections::HashSet::new();
    for value in values {
        let object = value.as_object().ok_or(())?;
        let start = object
            .get("StartPositionTicks")
            .and_then(|value| value.as_i64())
            .ok_or(())?;
        let index = object
            .get("ChapterIndex")
            .and_then(|value| value.as_i64())
            .ok_or(())?;
        if start < 0 || index < 0 || !indexes.insert(index) {
            return Err(());
        }
        let name = object
            .get("Name")
            .and_then(Value::as_str)
            .map(str::to_owned);
        chapters.push(MediaInfoChapterUpdate {
            start_position_ticks: start,
            name,
            chapter_index: index,
        });
    }
    Ok(chapters)
}

fn filtered_plugin_route_headers(
    headers: &HeaderMap,
) -> std::collections::BTreeMap<String, String> {
    const ALLOWED: &[&str] = &[
        "accept",
        "content-type",
        "user-agent",
        "x-emby-client",
        "x-emby-device-id",
        "x-emby-device-name",
        "x-emby-version",
    ];
    ALLOWED
        .iter()
        .filter_map(|name| {
            headers
                .get(*name)
                .and_then(|value| value.to_str().ok())
                .map(|value| ((*name).to_owned(), value.to_owned()))
        })
        .collect()
}

fn sanitize_plugin_route_query(raw_query: &str) -> Option<String> {
    let query = raw_query
        .split('&')
        .filter(|part| {
            let key = url::form_urlencoded::parse(part.as_bytes())
                .next()
                .map(|(key, _)| key.to_ascii_lowercase())
                .unwrap_or_else(|| {
                    part.split('=')
                        .next()
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                });
            !matches!(
                key.as_str(),
                "api_key"
                    | "apikey"
                    | "x-emby-token"
                    | "x-mediabrowser-token"
                    | "x-media-browser-token"
                    | "x-emby-authorization"
                    | "authorization"
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    (!query.is_empty()).then_some(query)
}

#[cfg(test)]
mod emby_route_tests {
    use super::sanitize_plugin_route_query;

    #[test]
    fn removes_plain_and_percent_encoded_auth_query_keys() {
        assert_eq!(
            sanitize_plugin_route_query(
                "Path=%2Fprobe.strm&api_key=secret&%61pi_key=encoded&Authorization=token"
            ),
            Some("Path=%2Fprobe.strm".to_owned())
        );
    }
}

#[derive(Deserialize, Default)]
pub(super) struct DanmakuQuery {
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

pub(super) async fn emby_danmaku_info(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<DanmakuQuery>,
    State(state): State<AppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let access = require_service!(state.access);
    let public_item_id = item_id.clone();
    let item_id = emby_internal_id(&item_id);
    let principal = match emby_access_principal(&auth_principal, None) {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    match access.can_view_item(principal, &item_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::FORBIDDEN.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let service = require_service!(state.danmaku);
    match service.read_sidecar(&item_id).await {
        Ok(Some(_)) => Json(json!({
            "hasDanmaku": true,
            "format": "xml",
            "url": format!("/api/danmu/{public_item_id}/raw"),
            "rawUrl": format!("/api/danmu/{public_item_id}/raw"),
            "option": query.option,
        }))
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_danmaku_raw(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<DanmakuQuery>,
    State(state): State<AppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let access = require_service!(state.access);
    let item_id = emby_internal_id(&item_id);
    let principal = match emby_access_principal(&auth_principal, None) {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    match access.can_view_item(principal, &item_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::FORBIDDEN.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let service = require_service!(state.danmaku);
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

pub(super) async fn current_emby_server_name(state: &AppState) -> String {
    let Some(database) = state.database.as_ref() else {
        return DEFAULT_SERVER_NAME.to_owned();
    };
    match database.server_name().await {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        Ok(_) | Err(_) => DEFAULT_SERVER_NAME.to_owned(),
    }
}

pub(super) async fn emby_public_system_info(State(state): State<AppState>) -> Json<Value> {
    let startup_wizard_completed = match state.setup() {
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

pub(super) async fn emby_system_info(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(status) = require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
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

/// `Auth/Keys` is what Emby proxies call to validate an `api_key` before serving a stream. Lux
/// never hands out individually managed keys, so a valid credential simply sees an empty list.
/// Playback-only tokens reach this route (it is on the playback allow-list) but get no new keys.
pub(super) async fn emby_auth_keys(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(status) =
        require_playback_route_principal(&headers, &state, query.api_key.as_deref()).await
    {
        return status.into_response();
    }
    Json(json!({ "Items": [], "TotalRecordCount": 0 })).into_response()
}

pub(super) async fn emby_refresh_item(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<EmbyRefreshQuery>,
    State(state): State<AppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.auth.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !auth_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    if query.recursive == Some(false) {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    let scan_jobs = require_service!(state.scan_jobs);
    let item_id = emby_internal_id(&item_id);
    let (job, scope) = match scan_jobs.create_folder_scan_job(&item_id).await {
        Ok(job) => (job, "FOLDER"),
        Err(ScanJobError::ItemNotFound) => {
            let Ok(library_id) = item_id.parse::<crate::domain::ids::LibraryId>() else {
                return StatusCode::NOT_FOUND.into_response();
            };
            match scan_jobs.create_library_root_scan_job(library_id).await {
                Ok(job) => (job, "LIBRARY_ROOT"),
                Err(ScanJobError::LibraryNotFound) => {
                    return StatusCode::NOT_FOUND.into_response();
                }
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
        Err(ScanJobError::LibraryNotFound) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    spawn_emby_scan_job(&state, job.id.clone());
    record_audit_event(
        &state,
        &headers,
        "EMBY_REFRESH_STARTED",
        Some("scan_job"),
        Some(&job.id),
        "{}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "scope": scope,
            "job": scan_job_json(&job),
        })),
    )
        .into_response()
}

pub(super) async fn emby_refresh_library(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !auth_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let database = require_service!(state.database);
    let scan_jobs = require_service!(state.scan_jobs);
    let library_ids = match database.list_enabled_library_ids().await {
        Ok(library_ids) => library_ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let mut jobs = Vec::with_capacity(library_ids.len());
    for library_id in library_ids {
        let Ok(library_id) = library_id.parse::<crate::domain::ids::LibraryId>() else {
            continue;
        };
        match scan_jobs.create_movie_scan_job(library_id).await {
            Ok(job) => {
                let job_id = job.id.clone();
                jobs.push(scan_job_json(&job));
                spawn_emby_scan_job(&state, job_id);
            }
            Err(ScanJobError::AlreadyActive(job_id)) => {
                match database.find_scan_job(&job_id).await {
                    Ok(Some(job)) => jobs.push(scan_job_json_from_storage(&job)),
                    Ok(None) => {}
                    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                }
            }
            Err(ScanJobError::LibraryNotFound) => {}
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "scope": "ALL",
            "jobs": jobs,
        })),
    )
        .into_response()
}

pub(super) async fn emby_media_updated(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    Json(request): Json<EmbyMediaUpdatedRequest>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !auth_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let database = require_service!(state.database);
    let scan_jobs = require_service!(state.scan_jobs);
    if request.updates.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    // Each entry costs a filesystem probe, so a single notification cannot be unbounded.
    if request.updates.len() > EMBY_MEDIA_UPDATED_MAX_ENTRIES {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let roots = match database.list_all_library_roots().await {
        Ok(roots) => roots,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let mut changes_by_library = HashMap::<
        crate::domain::ids::LibraryId,
        Vec<crate::application::scanner::IncrementalScanChange>,
    >::new();
    // Directories whose sidecars an external tool says it rewrote ("Modified"). The videos in
    // them are unchanged, so the incremental scan below would skip them: re-index NFO and images
    // for exactly these directories as well.
    let mut refresh_by_root =
        HashMap::<(crate::domain::ids::LibraryId, String), Vec<String>>::new();
    for update in request.updates {
        let path = PathBuf::from(update.path.trim());
        let Some((root, root_path)) = emby_matching_root(&roots, &path) else {
            continue;
        };
        let Ok(library_id) = root.library_id.parse() else {
            continue;
        };
        let Ok(relative_path) = path.strip_prefix(&root_path) else {
            continue;
        };
        let Some(relative_path) = relative_path.to_str() else {
            continue;
        };
        if relative_path.is_empty() {
            continue;
        }
        if update.update_type.trim().eq_ignore_ascii_case("modified") {
            let directory = if tokio::fs::metadata(&path)
                .await
                .is_ok_and(|metadata| metadata.is_dir())
            {
                Some(relative_path.to_owned())
            } else {
                FsPath::new(relative_path)
                    .parent()
                    .and_then(|parent| parent.to_str())
                    .filter(|parent| !parent.is_empty())
                    .map(str::to_owned)
            };
            if let Some(directory) = directory {
                refresh_by_root
                    .entry((library_id, root.id.clone()))
                    .or_default()
                    .push(directory);
            }
        }
        changes_by_library.entry(library_id).or_default().push(
            crate::application::scanner::IncrementalScanChange {
                root_id: root.id.clone(),
                relative_path: relative_path.to_owned(),
                kind: emby_update_change_kind(&update.update_type),
            },
        );
    }
    if changes_by_library.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut refreshed_entries = 0_usize;
    for ((library_id, root_id), directories) in refresh_by_root {
        match scan_jobs
            .start_local_metadata_refresh(library_id, Some(&root_id), &directories)
            .await
        {
            Ok(accepted) => refreshed_entries += accepted.entries,
            Err(error) => {
                tracing::warn!(%library_id, ?error, "local metadata refresh was not queued");
            }
        }
    }

    let mut jobs = Vec::with_capacity(changes_by_library.len());
    for (library_id, changes) in changes_by_library {
        match scan_jobs
            .enqueue_incremental_changes(library_id, changes)
            .await
        {
            Ok(job) => jobs.push(job),
            Err(ScanJobError::LibraryNotFound | ScanJobError::NoChanges) => continue,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }
    if jobs.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    for job in &jobs {
        spawn_emby_scan_job(&state, job.id.clone());
    }
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "scope": "PATH",
            "localMetadataEntries": refreshed_entries,
            "jobs": jobs.iter().map(scan_job_json).collect::<Vec<_>>(),
        })),
    )
        .into_response()
}

pub(super) async fn emby_scheduled_tasks(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !auth_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let database = require_service!(state.database);
    let mut active_full_scan = None;
    for status in ["PENDING", "RUNNING"] {
        let jobs = match database.list_scan_jobs(Some(status), 0, 10_000).await {
            Ok(jobs) => jobs,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        if let Some(job) = jobs
            .into_iter()
            .find(|job| job.job_type != "INCREMENTAL_SCAN")
        {
            active_full_scan = Some(job);
            break;
        }
    }
    let state = active_full_scan.as_ref().map_or("Idle", |_| "Running");
    let progress = active_full_scan
        .as_ref()
        .filter(|job| job.total_count > 0)
        .map(|job| {
            ((job.processed_count.max(0) as f64 / job.total_count as f64) * 100.0).clamp(0.0, 100.0)
        })
        .unwrap_or(0.0);
    Json(json!([{
        "Id": "lux-refresh-media-library",
        "Name": "Scan media library",
        "Key": "RefreshMediaLibrary",
        "Description": "Scans the media library for new and updated items.",
        "Category": "Library",
        "IsHidden": false,
        "IsEnabled": true,
        "State": state,
        "CurrentProgressPercentage": progress,
        "LastExecutionResult": Value::Null,
        "Triggers": [],
    }]))
    .into_response()
}

fn emby_update_change_kind(update_type: &str) -> crate::application::watch::ChangeKind {
    match update_type.trim().to_ascii_lowercase().as_str() {
        "created" | "create" => crate::application::watch::ChangeKind::Create,
        "deleted" | "delete" | "removed" | "remove" => {
            crate::application::watch::ChangeKind::Remove
        }
        "renamed" | "rename" => crate::application::watch::ChangeKind::Rename,
        _ => crate::application::watch::ChangeKind::Modify,
    }
}

fn emby_matching_root<'a>(
    roots: &'a [crate::storage::StoredLibraryRoot],
    path: &FsPath,
) -> Option<(&'a crate::storage::StoredLibraryRoot, PathBuf)> {
    let mut matching: Option<(&'a crate::storage::StoredLibraryRoot, PathBuf)> = None;
    for root in roots {
        for root_path in [&root.canonical_path, &root.display_path] {
            let root_path = FsPath::new(root_path);
            if !path.starts_with(root_path)
                || matching.as_ref().is_some_and(|(_, current)| {
                    current.components().count() >= root_path.components().count()
                })
            {
                continue;
            }
            matching = Some((root, root_path.to_path_buf()));
        }
    }
    matching
}

fn spawn_emby_scan_job(state: &AppState, job_id: String) {
    let Some(scan_jobs) = state.scan_jobs() else {
        return;
    };
    crate::application::admin_scan_jobs::spawn_scan_job_worker(
        scan_jobs,
        &job_id,
        crate::application::admin_scan_jobs::ScanWorkerServices {
            probe: state.probe().cloned(),
            metadata: state.metadata_reidentify().cloned(),
            thumbnails: state.thumbnails().cloned(),
        },
    );
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyDisplayPreferencesQuery {
    #[serde(flatten)]
    auth: EmbyTokenQuery,
    #[serde(rename = "UserId", alias = "userId", alias = "userid", default)]
    user_id: Option<String>,
    #[serde(rename = "Client", alias = "client", default)]
    client: Option<String>,
}

pub(super) async fn emby_display_preferences(
    headers: HeaderMap,
    Path(display_preferences_id): Path<String>,
    Query(query): Query<EmbyDisplayPreferencesQuery>,
    State(state): State<AppState>,
) -> Response {
    let principal = match require_emby_principal_with_query(&headers, &state, &query.auth).await {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    if principal.user_id().is_none() && query.user_id.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let requested_user_id = query.user_id.as_deref();
    if let Err(status) =
        emby_access_principal_for_target(&state, &principal, requested_user_id).await
    {
        return status.into_response();
    }
    let Some(client) = query
        .client
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    Json(json!({
        "Id": display_preferences_id,
        "ViewType": "Poster",
        "SortBy": "SortName",
        "IndexBy": serde_json::Value::Null,
        "RememberIndexing": false,
        "PrimaryImageHeight": 250,
        "PrimaryImageWidth": 250,
        "CustomPrefs": {},
        "ScrollDirection": "Horizontal",
        "ShowBackdrop": true,
        "RememberSorting": false,
        "SortOrder": "Ascending",
        "ShowSidebar": false,
        "Client": client,
    }))
    .into_response()
}

pub(super) async fn emby_ping(
    _headers: HeaderMap,
    Query(_query): Query<EmbyTokenQuery>,
    State(_state): State<AppState>,
) -> Response {
    StatusCode::OK.into_response()
}

pub(super) async fn emby_public_users(State(state): State<AppState>) -> Json<Value> {
    let server_id = state.server_id.clone();
    let Some(auth) = state.emby_auth() else {
        return Json(json!([]));
    };
    let server_name = current_emby_server_name(&state).await;
    let users = auth.public_users().await.unwrap_or_default();
    let snapshots = emby_user_access_snapshots(&state, &users).await;
    Json(Value::Array(
        users
            .iter()
            .map(|user| {
                emby_user_json(
                    user,
                    &server_id,
                    &server_name,
                    emby_user_configuration_json(&[]),
                    snapshots.get(&user.id.to_string()),
                )
            })
            .collect(),
    ))
}

pub(super) async fn emby_users(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !auth_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let auth = require_service!(state.emby_auth);
    let users = match auth.public_users().await {
        Ok(users) => users,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let server_name = current_emby_server_name(&state).await;
    let snapshots = emby_user_access_snapshots(&state, &users).await;
    Json(Value::Array(
        users
            .iter()
            .map(|user| {
                emby_user_json(
                    user,
                    &state.server_id,
                    &server_name,
                    emby_user_configuration_json(&[]),
                    snapshots.get(&user.id.to_string()),
                )
            })
            .collect(),
    ))
    .into_response()
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyUsersQuery {
    #[serde(flatten)]
    auth: EmbyTokenQuery,
    #[serde(
        rename = "IsHidden",
        alias = "isHidden",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    is_hidden: Option<bool>,
    #[serde(
        rename = "IsDisabled",
        alias = "isDisabled",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    is_disabled: Option<bool>,
    #[serde(rename = "StartIndex", alias = "startIndex", default)]
    start_index: Option<i64>,
    #[serde(rename = "Limit", alias = "limit", default)]
    limit: Option<i64>,
    #[serde(
        rename = "NameStartsWithOrGreater",
        alias = "nameStartsWithOrGreater",
        default
    )]
    name_starts_with_or_greater: Option<String>,
    #[serde(rename = "SortOrder", alias = "sortOrder", default)]
    sort_order: Option<String>,
}

pub(super) async fn emby_query_users(
    headers: HeaderMap,
    Query(query): Query<EmbyUsersQuery>,
    State(state): State<AppState>,
) -> Response {
    let acting_principal =
        match require_emby_principal_with_query(&headers, &state, &query.auth).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !acting_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let (offset, limit) = match emby_users_page_params(&query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let descending = match query
        .sort_order
        .as_deref()
        .unwrap_or("Ascending")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "ascending" => false,
        "descending" => true,
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    let name_starts_with_or_greater = query
        .name_starts_with_or_greater
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    if query.is_hidden == Some(true) {
        return Json(json!({
            "Items": [],
            "TotalRecordCount": 0
        }))
        .into_response();
    }
    let auth = require_service!(state.emby_auth);
    let (users, total_count) = match auth
        .query_users(
            query.is_disabled,
            name_starts_with_or_greater.as_deref(),
            descending,
            offset,
            limit,
        )
        .await
    {
        Ok(result) => result,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let server_name = current_emby_server_name(&state).await;
    let snapshots = emby_user_access_snapshots(&state, &users).await;
    let user_ids = users
        .iter()
        .map(|user| user.id.to_string())
        .collect::<Vec<_>>();
    // Reading the order and the Emby configuration per user made this listing cost a handful of
    // queries per page entry. Resolve both for the whole page instead.
    let access_requests = users
        .iter()
        .map(|user| (user.id.to_string(), user.is_admin))
        .collect::<Vec<_>>();
    let accessible_libraries = match state.access() {
        Some(access) => access
            .accessible_library_ids_for_users(&access_requests)
            .await
            .unwrap_or_default(),
        None => std::collections::HashMap::new(),
    };
    let mut order_requests = Vec::with_capacity(users.len());
    for user in &users {
        let user_id = user.id.to_string();
        let principal = AccessPrincipal::new(user.id, user.is_admin);
        let snapshot = snapshots.get(&user_id);
        let accessible = accessible_libraries
            .get(&user_id)
            .cloned()
            .unwrap_or_default();
        let Ok((order_scope, _)) = super::emby_catalog::emby_library_order_scope_with_access(
            &state,
            principal,
            accessible,
            snapshot.and_then(|snapshot| snapshot.restrictions.as_ref()),
        )
        .await
        else {
            continue;
        };
        order_requests.push((user_id, user.is_admin, order_scope));
    }
    let ordered_views = match state.libraries() {
        Some(libraries) => libraries
            .saved_library_orders_for_users(&order_requests)
            .await
            .unwrap_or_default(),
        None => std::collections::HashMap::new(),
    };
    let stored_configurations = match state.database.as_ref() {
        Some(database) => database
            .list_user_emby_configurations(&user_ids)
            .await
            .unwrap_or_default(),
        None => std::collections::HashMap::new(),
    };
    let force_admin_library_order = match state.database.as_ref() {
        Some(database) => database.force_admin_library_order().await.unwrap_or(false),
        None => false,
    };
    let mut items = Vec::with_capacity(users.len());
    for user in users {
        let user_id = user.id.to_string();
        let views = ordered_views
            .get(&user_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|library_id| emby_public_id(&library_id))
            .collect::<Vec<_>>();
        let configuration = emby_user_configuration_from_stored(
            stored_configurations.get(&user_id).map(String::as_str),
            &user,
            &views,
            force_admin_library_order,
        );
        items.push(emby_user_json(
            &user,
            &state.server_id,
            &server_name,
            configuration,
            snapshots.get(&user_id),
        ));
    }
    Json(json!({
        "Items": items,
        "TotalRecordCount": total_count,
    }))
    .into_response()
}

fn emby_users_page_params(query: &EmbyUsersQuery) -> Result<(i64, i64), StatusCode> {
    let offset = query.start_index.unwrap_or(0);
    let limit = match query.limit {
        Some(value) if value > 0 => value,
        _ => 50,
    };
    if offset < 0 || !(1..=100).contains(&limit) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok((offset, limit))
}

pub(super) async fn emby_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let principal = match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    let _access_principal =
        match emby_access_principal_for_target(&state, &principal, Some(&user_id)).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let auth = require_service!(state.emby_auth);
    let user = match auth.user_by_id(&user_id).await {
        Ok(Some(user)) if !user.is_disabled => user,
        Ok(Some(_) | None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let server_name = current_emby_server_name(&state).await;
    let ordered_views = emby_ordered_views(&state, &user).await;
    let configuration = emby_user_configuration(&state, &user, &ordered_views).await;
    let snapshots = emby_user_access_snapshots(&state, std::slice::from_ref(&user)).await;
    Json(emby_user_json(
        &user,
        &state.server_id,
        &server_name,
        configuration,
        snapshots.get(&user.id.to_string()),
    ))
    .into_response()
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyCollectionMutationQuery {
    #[serde(flatten)]
    pub(super) auth: EmbyTokenQuery,
    #[serde(rename = "Name", alias = "name", default)]
    pub(super) name: Option<String>,
    #[serde(rename = "Ids", alias = "ids", default)]
    pub(super) ids: Option<String>,
}

fn parse_emby_collection_item_ids(ids: Option<&str>) -> Result<Vec<String>, StatusCode> {
    let ids = ids
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(emby_internal_id)
        .collect::<Vec<_>>();
    if ids.len() > 1_000 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(ids)
}

pub(super) async fn emby_create_collection(
    headers: HeaderMap,
    Query(query): Query<EmbyCollectionMutationQuery>,
    State(state): State<AppState>,
) -> Response {
    let principal =
        match require_emby_principal(&headers, &state, query.auth.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(name) = query
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if name.chars().count() > 256 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let ids = match parse_emby_collection_item_ids(query.ids.as_deref()) {
        Ok(ids) => ids,
        Err(status) => return status.into_response(),
    };
    let collections = require_service!(state.collections);
    match collections.create_emby_collection(name, &ids).await {
        Ok(Some(collection)) => Json(json!({
            "Id": emby_public_id(&collection.collection_item_id),
            "Name": collection.title,
            "Type": "BoxSet",
            "IsFolder": true,
            "CollectionType": "movies"
        }))
        .into_response(),
        Ok(None) => StatusCode::BAD_REQUEST.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_add_collection_items(
    headers: HeaderMap,
    Path(collection_id): Path<String>,
    Query(query): Query<EmbyCollectionMutationQuery>,
    State(state): State<AppState>,
) -> Response {
    emby_mutate_collection_items(&headers, &collection_id, query, &state, true).await
}

pub(super) async fn emby_remove_collection_items(
    headers: HeaderMap,
    Path(collection_id): Path<String>,
    Query(query): Query<EmbyCollectionMutationQuery>,
    State(state): State<AppState>,
) -> Response {
    emby_mutate_collection_items(&headers, &collection_id, query, &state, false).await
}

async fn emby_mutate_collection_items(
    headers: &HeaderMap,
    collection_id: &str,
    query: EmbyCollectionMutationQuery,
    state: &AppState,
    add: bool,
) -> Response {
    let principal =
        match require_emby_principal(headers, state, query.auth.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let ids = match parse_emby_collection_item_ids(query.ids.as_deref()) {
        Ok(ids) => ids,
        Err(status) => return status.into_response(),
    };
    let collections = require_service!(state.collections);
    let collection_id = emby_internal_id(collection_id);
    let result = if add {
        collections
            .add_emby_collection_items(&collection_id, &ids)
            .await
    } else {
        collections
            .remove_emby_collection_items(&collection_id, &ids)
            .await
    };
    match result {
        Ok(Some(_)) => StatusCode::NO_CONTENT.into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub(super) struct EmbyCreateUserRequest {
    name: Option<String>,
    copy_from_user_id: Option<String>,
    user_copy_options: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyCreateUserQuery {
    #[serde(flatten)]
    auth: EmbyTokenQuery,
    #[serde(rename = "Name", alias = "name", default)]
    name: Option<String>,
}

pub(super) async fn emby_create_user(
    headers: HeaderMap,
    Query(query): Query<EmbyCreateUserQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let request = match parse_emby_create_user_request(&headers, &body, query.name.as_deref()) {
        Ok(request) => request,
        Err(status) => return status.into_response(),
    };
    let acting_principal =
        match require_emby_principal(&headers, &state, query.auth.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !acting_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(name) = request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let database = require_service!(state.database);
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let source = if let Some(source_id) = request.copy_from_user_id.as_deref() {
        if source_id.parse::<UserId>().is_err() {
            return StatusCode::BAD_REQUEST.into_response();
        }
        let auth = require_service!(state.emby_auth);
        match auth.user_by_id(source_id).await {
            Ok(Some(source)) => Some(source),
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    } else {
        None
    };
    let copy_policy = source.is_some()
        && request
            .user_copy_options
            .as_deref()
            .is_some_and(|options| has_emby_copy_option(options, "UserPolicy"));
    let copy_configuration = source.is_some()
        && request
            .user_copy_options
            .as_deref()
            .is_some_and(|options| has_emby_copy_option(options, "UserConfiguration"));
    let default_permissions =
        match crate::application::settings::read_new_user_permissions(database).await {
            Ok(permissions) => permissions,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    let mut created = match users
        .create_user_without_password_with_permission_snapshot(
            name,
            name,
            copy_policy && source.as_ref().is_some_and(|user| user.is_admin),
            default_permissions.stored_snapshot(),
        )
        .await
    {
        Ok(user) => user,
        Err(UserStoreError::InvalidUsername) => return StatusCode::BAD_REQUEST.into_response(),
        Err(UserStoreError::Storage(error)) if error.is_unique_violation() => {
            return StatusCode::CONFLICT.into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if copy_policy {
        let Some(source) = source.as_ref() else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        created = match users
            .update_user(
                &created.id.to_string(),
                UserUpdate {
                    is_admin: Some(source.is_admin),
                    can_manage_server: Some(source.can_manage_server),
                    is_disabled: Some(source.is_disabled),
                    can_remote_access: Some(source.can_remote_access),
                    can_download: Some(source.can_download),
                    ..UserUpdate::default()
                },
            )
            .await
        {
            Ok(Some(user)) => user,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(UserStoreError::LastManager) => return StatusCode::CONFLICT.into_response(),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    }
    if copy_configuration {
        let Some(source) = source.as_ref() else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let source_id = source.id.to_string();
        let target_id = created.id.to_string();
        if let Err(error) = database
            .copy_user_library_settings(&source_id, &target_id)
            .await
        {
            tracing::error!(error = %error, "failed to copy emby user library settings");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        if let Ok(Some(configuration)) = database.find_user_emby_configuration(&source_id).await
            && let Err(error) = database
                .set_user_emby_configuration(&target_id, &configuration)
                .await
        {
            tracing::error!(error = %error, "failed to copy emby user configuration");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        created = match users.find_by_id(&target_id).await {
            Ok(Some(user)) => user,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    }
    let server_name = current_emby_server_name(&state).await;
    let ordered_views = emby_ordered_views(&state, &created).await;
    let configuration = emby_user_configuration(&state, &created, &ordered_views).await;
    let snapshots = emby_user_access_snapshots(&state, std::slice::from_ref(&created)).await;
    Json(emby_user_json(
        &created,
        &state.server_id,
        &server_name,
        configuration,
        snapshots.get(&created.id.to_string()),
    ))
    .into_response()
}

pub(super) async fn emby_delete_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    if user_id.parse::<UserId>().is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let acting_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !acting_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let database = require_service!(state.database);
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users.soft_delete_user(&user_id).await {
        Ok(Some(hls_session_ids)) => {
            if let Some(web_playback) = state.web_playback() {
                web_playback
                    .stop_server_hls_sessions(&hls_session_ids)
                    .await;
            }
            StatusCode::OK.into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(UserStoreError::LastManager) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub(super) struct EmbyUserUpdateRequest {
    id: Option<String>,
    name: Option<String>,
    configuration: Option<Value>,
    policy: Option<EmbyUserPolicyRequest>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub(super) struct EmbyUserPolicyRequest {
    is_administrator: Option<bool>,
    is_disabled: Option<bool>,
    enable_remote_access: Option<bool>,
    enable_content_downloading: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub(super) struct EmbyUpdateUserPasswordRequest {
    id: Option<String>,
    current_pw: Option<String>,
    new_pw: Option<String>,
    reset_password: Option<bool>,
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyUserImageQuery {
    #[serde(flatten)]
    auth: EmbyTokenQuery,
    #[serde(rename = "Index", alias = "index")]
    index: Option<i32>,
}

fn emby_request_content_type(headers: &HeaderMap) -> &str {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or_default()
}

fn parse_emby_xml_fields(body: &[u8]) -> Result<HashMap<String, String>, StatusCode> {
    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::new();
    let mut fields = HashMap::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(element)) => {
                stack.push(xml_element_name(element.name().as_ref())?);
            }
            Ok(Event::Empty(element)) => {
                fields.insert(xml_element_name(element.name().as_ref())?, String::new());
            }
            Ok(Event::Text(text)) => {
                if let Some(field) = stack.last() {
                    let decoded = text.decode().map_err(|_| StatusCode::BAD_REQUEST)?;
                    let value = unescape(decoded.as_ref())
                        .map_err(|_| StatusCode::BAD_REQUEST)?
                        .into_owned();
                    fields.insert(field.clone(), value);
                }
            }
            Ok(Event::CData(text)) => {
                if let Some(field) = stack.last() {
                    let value = text
                        .decode()
                        .map_err(|_| StatusCode::BAD_REQUEST)?
                        .into_owned();
                    fields.insert(field.clone(), value);
                }
            }
            Ok(Event::End(_)) => {
                if stack.pop().is_none() {
                    return Err(StatusCode::BAD_REQUEST);
                }
            }
            Ok(Event::Eof) => break,
            Ok(
                Event::Decl(_)
                | Event::Comment(_)
                | Event::PI(_)
                | Event::DocType(_)
                | Event::GeneralRef(_),
            ) => {}
            Err(_) => return Err(StatusCode::BAD_REQUEST),
        }
        buffer.clear();
    }

    if stack.is_empty() {
        Ok(fields)
    } else {
        Err(StatusCode::BAD_REQUEST)
    }
}

fn xml_element_name(name: &[u8]) -> Result<String, StatusCode> {
    let name = std::str::from_utf8(name).map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(name.rsplit(':').next().unwrap_or(name).to_owned())
}

fn xml_optional_bool(
    fields: &HashMap<String, String>,
    name: &str,
) -> Result<Option<bool>, StatusCode> {
    let Some(value) = fields.get(name) else {
        return Ok(None);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(Some(true)),
        "false" | "0" => Ok(Some(false)),
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

fn parse_emby_user_update_request(
    headers: &HeaderMap,
    body: &[u8],
) -> Result<EmbyUserUpdateRequest, StatusCode> {
    match emby_request_content_type(headers) {
        "application/json" => serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST),
        "application/xml" | "text/xml" => {
            let fields = parse_emby_xml_fields(body)?;
            Ok(EmbyUserUpdateRequest {
                id: fields.get("Id").cloned(),
                name: fields.get("Name").cloned(),
                configuration: None,
                policy: None,
            })
        }
        _ => Err(StatusCode::UNSUPPORTED_MEDIA_TYPE),
    }
}

fn parse_emby_create_user_request(
    headers: &HeaderMap,
    body: &[u8],
    query_name: Option<&str>,
) -> Result<EmbyCreateUserRequest, StatusCode> {
    if body.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Ok(EmbyCreateUserRequest {
            name: query_name.map(str::to_owned),
            ..EmbyCreateUserRequest::default()
        });
    }
    match emby_request_content_type(headers) {
        "application/json" => serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST),
        "application/xml" | "text/xml" => {
            let fields = parse_emby_xml_fields(body)?;
            let user_copy_options = fields.get("UserCopyOptions").map(|value| {
                value
                    .split([',', ';'])
                    .map(str::trim)
                    .filter(|option| !option.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            });
            Ok(EmbyCreateUserRequest {
                name: fields.get("Name").cloned(),
                copy_from_user_id: fields.get("CopyFromUserId").cloned(),
                user_copy_options,
            })
        }
        _ => Err(StatusCode::UNSUPPORTED_MEDIA_TYPE),
    }
}

fn has_emby_copy_option(options: &[String], requested: &str) -> bool {
    options
        .iter()
        .any(|option| option.trim().eq_ignore_ascii_case(requested))
}

fn parse_emby_user_policy_request(
    headers: &HeaderMap,
    body: &[u8],
) -> Result<EmbyUserPolicyRequest, StatusCode> {
    match emby_request_content_type(headers) {
        "application/json" => serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST),
        "application/xml" | "text/xml" => {
            let fields = parse_emby_xml_fields(body)?;
            Ok(EmbyUserPolicyRequest {
                is_administrator: xml_optional_bool(&fields, "IsAdministrator")?,
                is_disabled: xml_optional_bool(&fields, "IsDisabled")?,
                enable_remote_access: xml_optional_bool(&fields, "EnableRemoteAccess")?,
                enable_content_downloading: xml_optional_bool(&fields, "EnableContentDownloading")?,
            })
        }
        _ => Err(StatusCode::UNSUPPORTED_MEDIA_TYPE),
    }
}

fn parse_emby_password_request(
    headers: &HeaderMap,
    body: &[u8],
) -> Result<EmbyUpdateUserPasswordRequest, StatusCode> {
    match emby_request_content_type(headers) {
        "application/json" => serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST),
        "application/xml" | "text/xml" => {
            let fields = parse_emby_xml_fields(body)?;
            Ok(EmbyUpdateUserPasswordRequest {
                id: fields.get("Id").cloned(),
                current_pw: fields.get("CurrentPw").cloned(),
                new_pw: fields.get("NewPw").cloned(),
                reset_password: xml_optional_bool(&fields, "ResetPassword")?,
            })
        }
        _ => Err(StatusCode::UNSUPPORTED_MEDIA_TYPE),
    }
}

fn check_emby_target_id(body_id: Option<&str>, path_id: &str) -> Result<(), StatusCode> {
    if body_id.is_some_and(|body_id| body_id != path_id) || path_id.parse::<UserId>().is_err() {
        Err(StatusCode::BAD_REQUEST)
    } else {
        Ok(())
    }
}

fn check_emby_image_index(index: Option<i32>) -> Result<(), StatusCode> {
    match index {
        None | Some(0) => Ok(()),
        Some(index) if index < 0 => Err(StatusCode::BAD_REQUEST),
        Some(_) => Err(StatusCode::NOT_FOUND),
    }
}

fn check_primary_user_image(image_type: &str) -> Result<(), StatusCode> {
    if image_type.eq_ignore_ascii_case("Primary") {
        Ok(())
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

pub(super) async fn emby_update_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let request = match parse_emby_user_update_request(&headers, &body) {
        Ok(request) => request,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = check_emby_target_id(request.id.as_deref(), &user_id) {
        return status.into_response();
    }
    let acting_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !acting_principal.can_manage_server()
        && acting_principal
            .user_id()
            .is_none_or(|id| id.to_string() != user_id)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if request.policy.is_some() && !acting_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let database = require_service!(state.database);
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if request
        .name
        .as_deref()
        .is_some_and(|name| name.trim().is_empty())
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if request
        .configuration
        .as_ref()
        .is_some_and(|configuration| !configuration.is_object())
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let policy = request.policy.as_ref();
    match users
        .update_user_with_hls_session_ids(
            &user_id,
            UserUpdate {
                display_name: request.name.as_deref(),
                is_admin: policy.and_then(|policy| policy.is_administrator),
                can_manage_server: policy.and_then(|policy| policy.is_administrator),
                is_disabled: policy.and_then(|policy| policy.is_disabled),
                can_remote_access: policy.and_then(|policy| policy.enable_remote_access),
                can_download: policy.and_then(|policy| policy.enable_content_downloading),
                ..UserUpdate::default()
            },
        )
        .await
    {
        Ok(Some((user, hls_session_ids))) => {
            if let Some(web_playback) = state.web_playback() {
                web_playback
                    .stop_server_hls_sessions(&hls_session_ids)
                    .await;
            }
            if let Some(incoming) = request.configuration {
                let ordered_views = emby_ordered_views(&state, &user).await;
                let mut configuration =
                    emby_user_configuration(&state, &user, &ordered_views).await;
                merge_emby_json_object(&mut configuration, incoming);
                let Ok(serialized) = serde_json::to_string(&configuration) else {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                };
                if let Err(error) = database
                    .set_user_emby_configuration(&user_id, &serialized)
                    .await
                {
                    tracing::error!(error = %error, "failed to persist emby user configuration");
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
            }
            StatusCode::OK.into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(UserStoreError::LastManager) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_update_user_policy(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let request = match parse_emby_user_policy_request(&headers, &body) {
        Ok(request) => request,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = check_emby_target_id(None, &user_id) {
        return status.into_response();
    }
    let acting_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !acting_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let database = require_service!(state.database);
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users
        .update_user_with_hls_session_ids(
            &user_id,
            UserUpdate {
                is_admin: request.is_administrator,
                can_manage_server: request.is_administrator,
                is_disabled: request.is_disabled,
                can_remote_access: request.enable_remote_access,
                can_download: request.enable_content_downloading,
                ..UserUpdate::default()
            },
        )
        .await
    {
        Ok(Some((_, hls_session_ids))) => {
            if let Some(web_playback) = state.web_playback() {
                web_playback
                    .stop_server_hls_sessions(&hls_session_ids)
                    .await;
            }
            StatusCode::OK.into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(UserStoreError::LastManager) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_update_user_password(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let request = match parse_emby_password_request(&headers, &body) {
        Ok(request) => request,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = check_emby_target_id(request.id.as_deref(), &user_id) {
        return status.into_response();
    }
    let acting_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !acting_principal.can_manage_server()
        && acting_principal
            .user_id()
            .is_none_or(|id| id.to_string() != user_id)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(new_password) = request.new_pw.as_deref() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if new_password.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let database = require_service!(state.database);
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if let Some(current_password) = request.current_pw.as_deref() {
        let Some(acting_user) = acting_principal.user() else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        match users
            .authenticate(&acting_user.username_normalized, current_password)
            .await
        {
            Ok(Some(_)) => {}
            Ok(None) => return StatusCode::FORBIDDEN.into_response(),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }
    let _ = request.reset_password;
    match users
        .update_user(
            &user_id,
            UserUpdate {
                password: Some(new_password),
                ..UserUpdate::default()
            },
        )
        .await
    {
        Ok(Some(_)) => StatusCode::OK.into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(UserStoreError::LastManager) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn emby_user_avatar_response(
    image_type: &str,
    user_id: &str,
    index: Option<i32>,
    state: &AppState,
    head_only: bool,
) -> Response {
    if let Err(status) = check_primary_user_image(image_type)
        .and_then(|_| check_emby_target_id(None, user_id))
        .and_then(|_| check_emby_image_index(index))
    {
        return status.into_response();
    }
    let avatars = require_service!(state.user_avatars);
    let Some(target_user_id) = user_id.parse::<UserId>().ok() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match avatars.load(target_user_id).await {
        Ok(Some(avatar)) => {
            let builder = Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, avatar.content_type)
                .header(CACHE_CONTROL, "private, no-cache")
                .header("Content-Length", avatar.bytes.len().to_string());
            let body = if head_only {
                Body::empty()
            } else {
                Body::from(avatar.bytes)
            };
            builder
                .body(body)
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_user_avatar(
    headers: HeaderMap,
    Path((user_id, image_type)): Path<(String, String)>,
    Query(query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
) -> Response {
    // Avatars are user data: require a valid Emby credential before serving them.
    if let Err(status) =
        require_emby_principal(&headers, &state, query.auth.api_key.as_deref()).await
    {
        return status.into_response();
    }
    emby_user_avatar_response(&image_type, &user_id, query.index, &state, false).await
}

pub(super) async fn emby_user_avatar_head(
    headers: HeaderMap,
    Path((user_id, image_type)): Path<(String, String)>,
    Query(query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(status) =
        require_emby_principal(&headers, &state, query.auth.api_key.as_deref()).await
    {
        return status.into_response();
    }
    emby_user_avatar_response(&image_type, &user_id, query.index, &state, true).await
}

async fn update_emby_user_avatar(
    headers: HeaderMap,
    user_id: String,
    image_type: String,
    query: EmbyUserImageQuery,
    state: AppState,
    body: Bytes,
    index: Option<i32>,
) -> Response {
    if let Err(status) = check_primary_user_image(&image_type)
        .and_then(|_| check_emby_target_id(None, &user_id))
        .and_then(|_| check_emby_image_index(index))
    {
        return status.into_response();
    }
    let principal =
        match require_emby_principal(&headers, &state, query.auth.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !principal.can_manage_server()
        && principal
            .user_id()
            .is_none_or(|id| id.to_string() != user_id)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let avatars = require_service!(state.user_avatars);
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    match avatars
        .store(
            match user_id.parse::<UserId>() {
                Ok(user_id) => user_id,
                Err(_) => return StatusCode::BAD_REQUEST.into_response(),
            },
            content_type,
            &body,
        )
        .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(UserAvatarError::UnsupportedContentType | UserAvatarError::InvalidContent) => {
            StatusCode::BAD_REQUEST.into_response()
        }
        Err(UserAvatarError::TooLarge { .. }) => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(UserAvatarError::InvalidPath(_) | UserAvatarError::Io { .. }) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

pub(super) async fn emby_update_user_avatar(
    headers: HeaderMap,
    Path((user_id, image_type)): Path<(String, String)>,
    Query(query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let index = query.index;
    update_emby_user_avatar(headers, user_id, image_type, query, state, body, index).await
}

pub(super) async fn emby_user_avatar_at_index(
    Path((user_id, image_type, image_index)): Path<(String, String, i32)>,
    Query(_query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
) -> Response {
    emby_user_avatar_response(&image_type, &user_id, Some(image_index), &state, false).await
}

pub(super) async fn emby_user_avatar_at_index_head(
    Path((user_id, image_type, image_index)): Path<(String, String, i32)>,
    Query(_query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
) -> Response {
    emby_user_avatar_response(&image_type, &user_id, Some(image_index), &state, true).await
}

pub(super) async fn emby_update_user_avatar_at_index(
    headers: HeaderMap,
    Path((user_id, image_type, image_index)): Path<(String, String, i32)>,
    Query(query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    update_emby_user_avatar(
        headers,
        user_id,
        image_type,
        query,
        state,
        body,
        Some(image_index),
    )
    .await
}

pub(super) async fn emby_delete_user_avatar_at_index(
    headers: HeaderMap,
    Path((user_id, image_type, image_index)): Path<(String, String, i32)>,
    Query(query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
) -> Response {
    delete_emby_user_avatar(
        headers,
        user_id,
        image_type,
        query,
        state,
        Some(image_index),
    )
    .await
}

async fn delete_emby_user_avatar(
    headers: HeaderMap,
    user_id: String,
    image_type: String,
    query: EmbyUserImageQuery,
    state: AppState,
    index: Option<i32>,
) -> Response {
    if let Err(status) = check_primary_user_image(&image_type)
        .and_then(|_| check_emby_target_id(None, &user_id))
        .and_then(|_| check_emby_image_index(index))
    {
        return status.into_response();
    }
    let principal =
        match require_emby_principal(&headers, &state, query.auth.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !principal.can_manage_server()
        && principal
            .user_id()
            .is_none_or(|id| id.to_string() != user_id)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let avatars = require_service!(state.user_avatars);
    let Some(target_user_id) = user_id.parse::<UserId>().ok() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match avatars.remove(target_user_id).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_delete_user_avatar(
    headers: HeaderMap,
    Path((user_id, image_type)): Path<(String, String)>,
    Query(query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
) -> Response {
    let index = query.index;
    delete_emby_user_avatar(headers, user_id, image_type, query, state, index).await
}

pub(super) async fn emby_ordered_views(state: &AppState, user: &UserRecord) -> Vec<String> {
    let Some(libraries) = state.libraries() else {
        return Vec::new();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let Ok((order_scope, _)) =
        super::emby_catalog::emby_library_order_scope(state, principal).await
    else {
        return Vec::new();
    };
    libraries
        .saved_library_order_for_user(&user.id.to_string(), user.is_admin, &order_scope)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|library_id| emby_public_id(&library_id))
        .collect()
}

#[derive(Deserialize)]
pub(super) struct EmbyAuthenticateRequest {
    #[serde(rename = "Username")]
    username: String,
    #[serde(rename = "Pw")]
    password: String,
}

#[derive(Deserialize)]
pub(super) struct EmbyAuthenticateUserRequest {
    #[serde(rename = "Pw")]
    password: String,
}

pub(super) async fn emby_authenticate(
    headers: HeaderMap,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let request = match parse_emby_authenticate_request(&headers, &body) {
        Ok(request) => request,
        Err(status) => return status.into_response(),
    };
    let Some(auth) = state.emby_auth().cloned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let login_key = login_attempt_key(&headers, &request.username);
    if !state.login_rate_limiter.is_allowed(&login_key).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let client_ip = match super::request_access_ip(&headers, &state).await {
        Ok(client_ip) => client_ip,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let device = emby_device_info_from_headers(&headers);
    match auth
        .authenticate_with_ip(&request.username, &request.password, &device, client_ip)
        .await
    {
        Ok(Some(result)) => {
            emby_authentication_success_response(&headers, &state, &auth, &login_key, result).await
        }
        Ok(None) => {
            state.login_rate_limiter.record_failure(&login_key).await;
            if let Ok(Some(user_id)) = auth
                .find_failed_authentication_user_id(&request.username)
                .await
            {
                record_failed_authentication_event(
                    state.database.as_ref(),
                    &state.admin_events,
                    &user_id,
                    "LUX_EMBY",
                    request_client_ip(&headers, &state).await,
                )
                .await;
            }
            StatusCode::UNAUTHORIZED.into_response()
        }
        Err(crate::auth::emby::EmbyAuthError::AccessRulesUnavailable) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub(super) async fn emby_authenticate_user_id(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let request = match parse_emby_authenticate_user_request(&headers, &body) {
        Ok(request) => request,
        Err(status) => return status.into_response(),
    };
    let Some(auth) = state.emby_auth().cloned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let login_key = login_attempt_key(&headers, &user_id);
    if !state.login_rate_limiter.is_allowed(&login_key).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let client_ip = match super::request_access_ip(&headers, &state).await {
        Ok(client_ip) => client_ip,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let device = emby_device_info_from_headers(&headers);
    match auth
        .authenticate_user_id_with_ip(&user_id, &request.password, &device, client_ip)
        .await
    {
        Ok(Some(result)) => {
            emby_authentication_success_response(&headers, &state, &auth, &login_key, result).await
        }
        Ok(None) => {
            state.login_rate_limiter.record_failure(&login_key).await;
            if let Ok(Some(user)) = auth.user_by_id(&user_id).await {
                record_failed_authentication_event(
                    state.database.as_ref(),
                    &state.admin_events,
                    &user.id.to_string(),
                    "LUX_EMBY",
                    request_client_ip(&headers, &state).await,
                )
                .await;
            }
            StatusCode::UNAUTHORIZED.into_response()
        }
        Err(crate::auth::emby::EmbyAuthError::AccessRulesUnavailable) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn emby_authentication_success_response(
    headers: &HeaderMap,
    state: &AppState,
    auth: &EmbyAuthService,
    login_key: &str,
    result: crate::auth::emby::EmbyAuthResult,
) -> Response {
    if state.remote_access.is_remote(
        header_str(headers, "x-lux-peer-ip"),
        header_str(headers, "x-forwarded-for"),
    ) && !result.user.can_remote_access
    {
        let _ = auth.logout(&result.token).await;
        return StatusCode::FORBIDDEN.into_response();
    }
    state.login_rate_limiter.record_success(login_key).await;
    let user_id = result.user.id.to_string();
    if let Some(device_id) = bounded_emby_device_field(&result.device.device_id) {
        if let Some(database) = state.database.as_ref() {
            let device_name = bounded_emby_device_field(&result.device.device);
            let client_name = bounded_emby_device_field(&result.device.client);
            let client_version = bounded_emby_device_field(&result.device.version);
            let remote_ip = request_client_ip(headers, state).await;
            let seen_at = super::playback::current_unix_timestamp();
            if let Err(error) = database
                .upsert_user_device(NewUserDeviceObservation {
                    user_id: &user_id,
                    device_id: &device_id,
                    device_name: device_name.as_deref(),
                    device_type: Some("EMBY"),
                    client_name: client_name.as_deref().unwrap_or("Unknown"),
                    client_version: client_version.as_deref(),
                    last_remote_ip: remote_ip.as_deref(),
                    seen_at,
                })
                .await
            {
                tracing::warn!(%error, "failed to update Emby user device inventory");
            }
        }
    }
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
            "remoteIp": request_client_ip(headers, state).await,
        }),
    )
    .await;
    let server_name = current_emby_server_name(state).await;
    let ordered_views = emby_ordered_views(state, &result.user).await;
    let configuration = emby_user_configuration(state, &result.user, &ordered_views).await;
    let snapshots = emby_user_access_snapshots(state, std::slice::from_ref(&result.user)).await;
    Json(json!({
        "User": emby_user_json(
            &result.user,
            &state.server_id,
            &server_name,
            configuration,
            snapshots.get(&result.user.id.to_string()),
        ),
        "SessionInfo": emby_login_session_json(&result, &state.server_id),
        "AccessToken": result.token,
        "ServerId": state.server_id
    }))
    .into_response()
}

fn bounded_emby_device_field(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

fn parse_emby_authenticate_user_request(
    headers: &HeaderMap,
    body: &[u8],
) -> Result<EmbyAuthenticateUserRequest, StatusCode> {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or_default();
    if content_type != "application/json" {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST)
}

pub(super) fn parse_emby_authenticate_request(
    headers: &HeaderMap,
    body: &[u8],
) -> Result<EmbyAuthenticateRequest, StatusCode> {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or_default();

    match content_type {
        "application/json" => serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST),
        "application/x-www-form-urlencoded" => {
            let mut username = None;
            let mut password = None;
            for (key, value) in url::form_urlencoded::parse(body) {
                match key.as_ref() {
                    "Username" => username = Some(value.into_owned()),
                    "Pw" => password = Some(value.into_owned()),
                    _ => {}
                }
            }
            Ok(EmbyAuthenticateRequest {
                username: username.ok_or(StatusCode::BAD_REQUEST)?,
                password: password.ok_or(StatusCode::BAD_REQUEST)?,
            })
        }
        _ => Err(StatusCode::UNSUPPORTED_MEDIA_TYPE),
    }
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyTokenQuery {
    #[serde(
        rename = "api_key",
        alias = "apiKey",
        alias = "ApiKey",
        alias = "X-Emby-Token",
        alias = "x-emby-token",
        alias = "X-MediaBrowser-Token",
        alias = "x-media-browser-token"
    )]
    pub(super) api_key: Option<String>,
    #[serde(rename = "tag", alias = "Tag")]
    pub(super) tag: Option<String>,
    #[serde(rename = "Fields", default)]
    pub(super) fields: Option<String>,
    #[serde(rename = "ActiveWithinSeconds", alias = "activeWithinSeconds", default)]
    pub(super) active_within_seconds: Option<i64>,
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyRefreshQuery {
    #[serde(flatten)]
    pub(super) auth: EmbyTokenQuery,
    #[serde(
        rename = "Recursive",
        alias = "recursive",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    pub(super) recursive: Option<bool>,
}

/// Upper bound for one `/Library/Media/Updated` batch.
const EMBY_MEDIA_UPDATED_MAX_ENTRIES: usize = 256;

#[derive(Deserialize, Default)]
pub(super) struct EmbyMediaUpdatedRequest {
    #[serde(rename = "Updates", alias = "updates", default)]
    pub(super) updates: Vec<EmbyMediaUpdatedEntry>,
}

#[derive(Deserialize)]
pub(super) struct EmbyMediaUpdatedEntry {
    #[serde(rename = "Path", alias = "path", default)]
    pub(super) path: String,
    #[serde(rename = "UpdateType", alias = "updateType", default)]
    pub(super) update_type: String,
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyPersonsQuery {
    #[serde(flatten)]
    pub(super) auth: EmbyTokenQuery,
    #[serde(rename = "UserId", alias = "userId", alias = "userid", default)]
    pub(super) user_id: Option<String>,
    #[serde(rename = "ParentId", alias = "parentId", default)]
    pub(super) parent_id: Option<String>,
    #[serde(rename = "PersonTypes", alias = "personTypes", default)]
    pub(super) person_types: Option<String>,
    #[serde(rename = "StartIndex", alias = "startIndex", default)]
    pub(super) start_index: Option<i64>,
    #[serde(rename = "Limit", alias = "limit", default)]
    pub(super) limit: Option<i64>,
    #[serde(
        rename = "Recursive",
        alias = "recursive",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    pub(super) recursive: Option<bool>,
    #[serde(rename = "SortBy", alias = "sortBy", default)]
    pub(super) sort_by: Option<String>,
    #[serde(rename = "SortOrder", alias = "sortOrder", default)]
    pub(super) sort_order: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyPersonQuery {
    #[serde(flatten)]
    pub(super) auth: EmbyTokenQuery,
    #[serde(rename = "UserId", alias = "userId", alias = "userid", default)]
    pub(super) user_id: Option<String>,
}

pub(super) async fn resolve_emby_user_with_auth(
    headers: &HeaderMap,
    query: &EmbyTokenQuery,
    auth: &EmbyAuthService,
    client_ip: Option<std::net::IpAddr>,
) -> Result<UserRecord, StatusCode> {
    resolve_emby_user_scoped(headers, query, auth, client_ip, false).await
}

async fn resolve_emby_user_scoped(
    headers: &HeaderMap,
    query: &EmbyTokenQuery,
    auth: &EmbyAuthService,
    client_ip: Option<std::net::IpAddr>,
    allow_playback_token: bool,
) -> Result<UserRecord, StatusCode> {
    let token = emby_token_from_headers(headers)
        .or_else(|| query.api_key.clone())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let resolved = if allow_playback_token {
        auth.resolve_emby_route_token_with_ip(&token, client_ip)
            .await
    } else {
        auth.resolve_token_with_ip(&token, client_ip).await
    };
    match resolved {
        Ok(Some(user)) => Ok(user),
        Ok(None) => Err(StatusCode::UNAUTHORIZED),
        Err(crate::auth::emby::EmbyAuthError::AccessRulesUnavailable) => {
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub(super) fn emby_token_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers.get(super::emby::NORMALIZED_CREDENTIAL_HEADER) {
        return value.to_str().ok().map(str::to_owned);
    }
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

pub(super) fn emby_device_info_from_headers(headers: &HeaderMap) -> EmbyDeviceInfo {
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

pub(super) fn merge_emby_device_info(target: &mut EmbyDeviceInfo, fallback: EmbyDeviceInfo) {
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

pub(super) fn emby_token_header_value(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(token) = value.strip_prefix("Bearer ") {
        return (!token.is_empty()).then(|| token.to_owned());
    }
    emby_authorization_token(value).or_else(|| (!value.is_empty()).then(|| value.to_owned()))
}

pub(super) fn emby_authorization_token(value: &str) -> Option<String> {
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

pub(super) async fn require_emby_user(
    headers: &HeaderMap,
    state: &AppState,
    api_key: Option<&str>,
) -> Result<UserRecord, StatusCode> {
    let query = EmbyTokenQuery {
        api_key: api_key.map(str::to_owned),
        tag: None,
        fields: None,
        active_within_seconds: None,
    };
    require_emby_user_with_query(headers, state, &query).await
}

pub(super) async fn require_emby_user_with_query(
    headers: &HeaderMap,
    state: &AppState,
    query: &EmbyTokenQuery,
) -> Result<UserRecord, StatusCode> {
    let Some(auth) = state.emby_auth() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let client_ip = super::request_access_ip(headers, state)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let user = resolve_emby_user_with_auth(headers, query, auth, client_ip).await?;
    if state.remote_access.is_remote(
        header_str(headers, "x-lux-peer-ip"),
        header_str(headers, "x-forwarded-for"),
    ) && !user.can_remote_access
    {
        return Err(StatusCode::FORBIDDEN);
    }
    if let Some(token) = emby_token_from_headers(headers).or_else(|| query.api_key.clone()) {
        record_emby_device_access(headers, state, auth, &token, &user.id.to_string()).await;
    }
    Ok(user)
}

pub(super) async fn require_emby_principal(
    headers: &HeaderMap,
    state: &AppState,
    api_key: Option<&str>,
) -> Result<crate::auth::users::AuthenticationPrincipal, StatusCode> {
    require_emby_principal_allowing_playback_tokens(headers, state, api_key, false).await
}

/// Principal check for routes that belong to the playback allow-list. Only those routes accept a
/// playback-only token; the router has already rejected every other Emby path for such a token.
pub(super) async fn require_playback_route_principal(
    headers: &HeaderMap,
    state: &AppState,
    api_key: Option<&str>,
) -> Result<crate::auth::users::AuthenticationPrincipal, StatusCode> {
    require_emby_principal_allowing_playback_tokens(headers, state, api_key, true).await
}

async fn require_emby_principal_allowing_playback_tokens(
    headers: &HeaderMap,
    state: &AppState,
    api_key: Option<&str>,
    allow_playback_token: bool,
) -> Result<crate::auth::users::AuthenticationPrincipal, StatusCode> {
    let query = EmbyTokenQuery {
        api_key: api_key.map(str::to_owned),
        tag: None,
        fields: None,
        active_within_seconds: None,
    };
    require_emby_principal_scoped(headers, state, &query, allow_playback_token).await
}

pub(super) async fn require_emby_principal_with_query(
    headers: &HeaderMap,
    state: &AppState,
    query: &EmbyTokenQuery,
) -> Result<crate::auth::users::AuthenticationPrincipal, StatusCode> {
    require_emby_principal_scoped(headers, state, query, false).await
}

async fn require_emby_principal_scoped(
    headers: &HeaderMap,
    state: &AppState,
    query: &EmbyTokenQuery,
    allow_playback_token: bool,
) -> Result<crate::auth::users::AuthenticationPrincipal, StatusCode> {
    let Some(auth) = state.emby_auth() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let token = emby_token_from_headers(headers)
        .or_else(|| query.api_key.clone())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if crate::auth::emby::is_playback_token(&token) && !allow_playback_token {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let principal = if let Some(service) = state.admin_api_key() {
        match service.resolve_principal(&token).await {
            Ok(Some(principal)) => Some(principal),
            Ok(None) => None,
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        }
    } else {
        None
    };
    let principal = if let Some(principal) = principal {
        principal
    } else {
        let client_ip = super::request_access_ip(headers, state)
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let resolved = if allow_playback_token {
            auth.resolve_emby_route_token_with_ip(&token, client_ip)
                .await
        } else {
            auth.resolve_token_with_ip(&token, client_ip).await
        };
        match resolved {
            Ok(Some(user)) => crate::auth::users::AuthenticationPrincipal::User(user),
            Ok(None) => return Err(StatusCode::UNAUTHORIZED),
            Err(crate::auth::emby::EmbyAuthError::AccessRulesUnavailable) => {
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        }
    };
    if state.remote_access.is_remote(
        header_str(headers, "x-lux-peer-ip"),
        header_str(headers, "x-forwarded-for"),
    ) && !principal.can_remote_access()
    {
        return Err(StatusCode::FORBIDDEN);
    }
    if let Some(user) = principal.user() {
        record_emby_device_access(headers, state, auth, &token, &user.id.to_string()).await;
    }
    Ok(principal)
}

async fn record_emby_device_access(
    headers: &HeaderMap,
    state: &AppState,
    auth: &EmbyAuthService,
    token: &str,
    user_id: &str,
) {
    let Some(database) = state.database.as_ref() else {
        return;
    };
    if !auth.device_access_refresh_due(token) {
        return;
    }
    let device = match auth.device_info(token).await {
        Ok(Some(device)) => device,
        Ok(None) => return,
        Err(_) => {
            tracing::warn!("failed to read Emby device for user activity");
            return;
        }
    };
    let Some(device_id) = bounded_emby_device_field(&device.device_id) else {
        return;
    };
    let device_name = bounded_emby_device_field(&device.device);
    let client_name = bounded_emby_device_field(&device.client);
    let client_version = bounded_emby_device_field(&device.version);
    let last_remote_ip = request_client_ip(headers, state).await;
    match database
        .upsert_user_device(NewUserDeviceObservation {
            user_id,
            device_id: &device_id,
            device_name: device_name.as_deref(),
            device_type: Some("EMBY"),
            client_name: client_name.as_deref().unwrap_or("Unknown"),
            client_version: client_version.as_deref(),
            last_remote_ip: last_remote_ip.as_deref(),
            seen_at: super::playback::current_unix_timestamp(),
        })
        .await
    {
        Ok(()) => auth.mark_device_access_refreshed(token),
        Err(_) => tracing::warn!("failed to update Emby user device activity"),
    }
}

pub(super) fn emby_access_principal(
    principal: &crate::auth::users::AuthenticationPrincipal,
    target_user_id: Option<&str>,
) -> Result<AccessPrincipal, StatusCode> {
    let target_user_id = target_user_id
        .map(str::parse::<UserId>)
        .transpose()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if let Some(user) = principal.user() {
        if target_user_id.is_some_and(|target_user_id| !user.is_admin && target_user_id != user.id)
        {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(AccessPrincipal::new(
            target_user_id.unwrap_or(user.id),
            user.is_admin,
        ))
    } else {
        Ok(AccessPrincipal::server_admin(target_user_id))
    }
}

pub(super) async fn emby_access_principal_for_target(
    state: &AppState,
    principal: &crate::auth::users::AuthenticationPrincipal,
    target_user_id: Option<&str>,
) -> Result<AccessPrincipal, StatusCode> {
    let access_principal = emby_access_principal(principal, target_user_id)?;
    let Some(target_user_id) = access_principal.user_id else {
        return Ok(access_principal);
    };
    if principal.user_id() == Some(target_user_id) {
        return Ok(access_principal);
    }
    let Some(auth) = state.emby_auth() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    match auth.user_by_id(&target_user_id.to_string()).await {
        Ok(Some(user)) if !user.is_disabled => Ok(access_principal),
        Ok(Some(_) | None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

pub(super) async fn emby_logout(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> StatusCode {
    let Some(auth) = state.emby_auth() else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    let token = emby_token_from_headers(&headers).or(query.api_key);
    let Some(token) = token else {
        return StatusCode::UNAUTHORIZED;
    };
    match auth.logout(&token).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Loads restriction and library-access state for the users that are about to be serialized.
async fn emby_user_access_snapshots(
    state: &AppState,
    users: &[UserRecord],
) -> std::collections::HashMap<String, crate::storage::UserAccessSnapshot> {
    let Some(database) = state.database.as_ref() else {
        return std::collections::HashMap::new();
    };
    let user_ids = users
        .iter()
        .map(|user| user.id.to_string())
        .collect::<Vec<_>>();
    if user_ids.is_empty() {
        return std::collections::HashMap::new();
    }
    database
        .list_user_access_snapshots(&user_ids)
        .await
        .unwrap_or_default()
}

fn emby_user_json(
    user: &UserRecord,
    server_id: &str,
    server_name: &str,
    configuration: Value,
    snapshot: Option<&crate::storage::UserAccessSnapshot>,
) -> Value {
    json!({
        "Id": user.id.to_string(),
        "ServerId": server_id,
        "ServerName": server_name,
        "Name": user.display_name,
        "HasPassword": user.has_password,
        "HasConfiguredPassword": user.has_password,
        "HasConfiguredEasyPassword": false,
        "EnableAutoLogin": false,
        "LastLoginDate": emby_user_date(user.last_login_at),
        "LastActivityDate": emby_user_date(user.last_activity_at),
        "Configuration": configuration,
        "Policy": emby_user_policy_json(user, snapshot),
    })
}

fn emby_user_date(timestamp: Option<i64>) -> Value {
    timestamp
        .and_then(|timestamp| OffsetDateTime::from_unix_timestamp(timestamp).ok())
        .and_then(|timestamp| timestamp.format(&Rfc3339).ok())
        .map_or(Value::Null, Value::String)
}

async fn emby_user_configuration(
    state: &AppState,
    user: &UserRecord,
    ordered_views: &[String],
) -> Value {
    let Some(database) = state.database.as_ref() else {
        return emby_user_configuration_json(ordered_views);
    };
    let stored = database
        .find_user_emby_configuration(&user.id.to_string())
        .await
        .ok()
        .flatten();
    let force_admin_library_order = if user.is_admin {
        false
    } else {
        database.force_admin_library_order().await.unwrap_or(false)
    };
    emby_user_configuration_from_stored(
        stored.as_deref(),
        user,
        ordered_views,
        force_admin_library_order,
    )
}

fn emby_user_configuration_from_stored(
    serialized: Option<&str>,
    user: &UserRecord,
    ordered_views: &[String],
    force_admin_library_order: bool,
) -> Value {
    let mut configuration = emby_user_configuration_json(ordered_views);
    if let Some(serialized) = serialized
        && let Ok(stored) = serde_json::from_str::<Value>(serialized)
    {
        merge_emby_json_object(&mut configuration, stored);
    }
    if !user.is_admin
        && force_admin_library_order
        && let Some(object) = configuration.as_object_mut()
    {
        object.insert("OrderedViews".to_owned(), json!(ordered_views));
    }
    normalize_emby_ordered_views(&mut configuration);
    configuration
}

fn normalize_emby_ordered_views(configuration: &mut Value) {
    let Some(views) = configuration
        .as_object_mut()
        .and_then(|object| object.get_mut("OrderedViews"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for view in views {
        if let Value::String(view_id) = view {
            *view_id = emby_public_id(view_id);
        }
    }
}

fn merge_emby_json_object(target: &mut Value, overlay: Value) {
    let (Some(target), Value::Object(overlay)) = (target.as_object_mut(), overlay) else {
        return;
    };
    for (key, value) in overlay {
        target.insert(key, value);
    }
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

/// Maps Lux rating labels onto Emby's numeric parental rating scale (best effort).
///
/// Emby compares `MaxParentalRating` with its own numeric official ratings, so only the common
/// motion-picture and TV labels are translated; anything else stays unspecified.
fn emby_parental_rating(rating: Option<&str>) -> Option<i64> {
    let rating = rating?.trim().to_ascii_uppercase();
    let value = match rating.as_str() {
        "G" | "TV-Y" | "TV-Y7" | "U" => 1,
        "PG" | "TV-G" | "TV-PG" => 2,
        "PG-13" | "TV-14" => 3,
        "R" => 4,
        "NC-17" | "TV-MA" => 5,
        "X" => 6,
        _ => return None,
    };
    Some(value)
}

fn emby_user_policy_json(
    user: &UserRecord,
    snapshot: Option<&crate::storage::UserAccessSnapshot>,
) -> Value {
    let restrictions = snapshot.and_then(|snapshot| snapshot.restrictions.as_ref());
    let blocked_tags = restrictions
        .map(|restrictions| restrictions.blocked_tags.clone())
        .unwrap_or_default();
    let max_parental_rating = restrictions
        .and_then(|restrictions| emby_parental_rating(restrictions.maximum_rating.as_deref()));
    let block_unrated_items =
        if restrictions.is_some_and(|restrictions| !restrictions.allow_unrated) {
            json!(["Movie", "Series", "Episode"])
        } else {
            json!([])
        };
    let (enable_all_folders, enabled_folders) = match snapshot {
        Some(snapshot)
            if snapshot
                .library_access_mode
                .eq_ignore_ascii_case("SELECTED") =>
        {
            (
                false,
                snapshot
                    .library_ids
                    .iter()
                    .map(|library_id| json!(emby_public_id(library_id)))
                    .collect::<Vec<_>>(),
            )
        }
        _ => (true, Vec::new()),
    };
    json!({
        "IsAdministrator": user.is_admin,
        "IsHidden": false,
        "IsHiddenRemotely": false,
        "IsDisabled": user.is_disabled,
        "MaxParentalRating": max_parental_rating,
        "BlockedTags": blocked_tags,
        "EnableUserPreferenceAccess": true,
        "AccessSchedules": [],
        "BlockUnratedItems": block_unrated_items,
        "EnableRemoteControlOfOtherUsers": false,
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
        "EnableSubtitleManagement": false,
        "EnableSyncTranscoding": false,
        "EnableMediaConversion": false,
        "EnabledDevices": [],
        "EnableAllDevices": true,
        "EnabledChannels": [],
        "EnableAllChannels": false,
        "EnabledFolders": enabled_folders,
        "EnableAllFolders": enable_all_folders,
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

#[cfg(test)]
mod user_configuration_tests {
    use super::emby_user_configuration_from_stored;
    use crate::auth::users::UserRecord;
    use crate::domain::ids::UserId;
    use serde_json::json;

    fn user(is_admin: bool) -> UserRecord {
        UserRecord {
            id: UserId::new(),
            username_normalized: "watcher".to_owned(),
            display_name: "Watcher".to_owned(),
            has_password: true,
            is_disabled: false,
            is_admin,
            can_manage_server: is_admin,
            can_remote_access: true,
            can_download: true,
            last_login_at: None,
            last_activity_at: None,
        }
    }

    #[test]
    fn stored_configuration_is_merged_and_the_forced_order_replaces_views() {
        let views = vec!["view-a".to_owned(), "view-b".to_owned()];
        let stored = r#"{"DisplayMissingEpisodes":true,"OrderedViews":["stored-view"]}"#;

        let configuration =
            emby_user_configuration_from_stored(Some(stored), &user(false), &views, true);
        assert_eq!(configuration["DisplayMissingEpisodes"], json!(true));
        assert_eq!(configuration["OrderedViews"], json!(views));

        let configuration =
            emby_user_configuration_from_stored(Some(stored), &user(false), &views, false);
        assert_eq!(
            configuration["OrderedViews"],
            json!(["stored-view"]),
            "the stored order survives unless the server forces the administrator order"
        );

        let configuration =
            emby_user_configuration_from_stored(Some(stored), &user(true), &views, true);
        assert_eq!(
            configuration["OrderedViews"],
            json!(["stored-view"]),
            "administrators always keep their own stored order"
        );

        let configuration = emby_user_configuration_from_stored(None, &user(false), &views, false);
        assert_eq!(configuration["OrderedViews"], json!(views));
        let configuration =
            emby_user_configuration_from_stored(Some("not-json"), &user(false), &views, false);
        assert_eq!(configuration["OrderedViews"], json!(views));
    }
}

#[cfg(test)]
mod parental_rating_tests {
    use super::emby_parental_rating;

    #[test]
    fn known_rating_labels_map_to_the_emby_scale() {
        assert_eq!(emby_parental_rating(Some("G")), Some(1));
        assert_eq!(emby_parental_rating(Some("pg")), Some(2));
        assert_eq!(emby_parental_rating(Some("PG-13")), Some(3));
        assert_eq!(emby_parental_rating(Some("TV-MA")), Some(5));
        assert_eq!(emby_parental_rating(Some("unrated")), None);
        assert_eq!(emby_parental_rating(None), None);
    }
}
