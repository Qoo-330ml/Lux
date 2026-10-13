use super::*;
use crate::application::scanner::BACKGROUND_SCAN_BATCH_SIZE;

pub(crate) async fn admin_start_scan(
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

pub(crate) async fn admin_start_library_path_scan(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<ScanPathRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if !request.recursive {
        return api_error(
            &headers,
            StatusCode::UNPROCESSABLE_ENTITY,
            lux::ApiErrorCode::InvalidRequest,
            "按路径刷新暂只支持递归扫描",
        )
        .into_response();
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
    let job = match scan_jobs
        .create_path_scan_job(library_id, request.root_id.as_deref(), &request.path)
        .await
    {
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
        Err(ScanJobError::Scanner(
            ScannerError::InvalidRootId(_) | ScannerError::InvalidRelativePath(_),
        )) => {
            return api_error(
                &headers,
                StatusCode::UNPROCESSABLE_ENTITY,
                lux::ApiErrorCode::InvalidRequest,
                "扫描根目录或相对路径无效",
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
        "SCAN_PATH_STARTED",
        Some("scan_job"),
        Some(&target_id),
        "{}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "scope": "PATH",
            "path": request.path,
            "job": scan_job_json(&job),
        })),
    )
        .into_response()
}

/// Re-indexes NFO and images for exact directories without a scan. Meant for tools that write
/// or repair sidecars next to unchanged videos (a scraper publishing a poster, Immortal).
pub(crate) async fn spawn_library_scan(
    state: &AppState,
    library_id: crate::domain::ids::LibraryId,
) -> Result<Option<ScanJob>, ScanJobError> {
    let Some(scan_jobs) = state.scan_jobs.as_ref() else {
        return Ok(None);
    };
    let job = match scan_jobs
        .create_movie_scan_job_with_metadata(library_id, false)
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

pub(crate) async fn admin_cancel_scan(
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

pub(crate) async fn admin_start_strm_probe(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
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

pub(crate) async fn admin_list_strm_probe_jobs(
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

pub(crate) async fn admin_get_strm_probe_job(
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

pub(crate) async fn admin_cancel_strm_probe(
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

pub(crate) async fn admin_retry_strm_probe(
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
pub(crate) struct ChapterDetectionRequest {
    #[serde(default)]
    pub(crate) plugin_id: Option<String>,
    #[serde(default)]
    pub(crate) concurrency: Option<i64>,
    #[serde(default)]
    pub(crate) intro_window_seconds: Option<i64>,
    #[serde(default)]
    pub(crate) credits_window_seconds: Option<i64>,
    #[serde(default)]
    pub(crate) match_threshold: Option<u32>,
    #[serde(default)]
    pub(crate) force_refresh: bool,
}

pub(crate) async fn admin_start_chapter_detection(
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

pub(crate) async fn admin_list_chapter_detection_jobs(
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

pub(crate) async fn admin_get_chapter_detection_job(
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

pub(crate) async fn admin_cancel_chapter_detection(
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

pub(crate) async fn admin_retry_chapter_detection(
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

pub(crate) fn chapter_detection_error(
    headers: &HeaderMap,
    error: ChapterDetectionError,
) -> Response {
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
pub(crate) struct DanmakuMatchRequest {
    #[serde(default = "default_danmaku_concurrency")]
    pub(crate) concurrency: i64,
    #[serde(default)]
    pub(crate) overwrite: bool,
}

const fn default_danmaku_concurrency() -> i64 {
    crate::application::danmaku::DEFAULT_DANMAKU_CONCURRENCY
}

pub(crate) async fn admin_start_danmaku_match(
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

pub(crate) async fn admin_list_danmaku_match_jobs(
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

pub(crate) async fn admin_get_danmaku_match_job(
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

pub(crate) async fn admin_cancel_danmaku_match(
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

pub(crate) async fn admin_retry_danmaku_match(
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
pub(crate) struct AdminJobsQuery {
    pub(crate) page: Option<i64>,
    pub(crate) page_size: Option<i64>,
    pub(crate) status: Option<String>,
    pub(crate) search: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminJobEventsQuery {
    pub(crate) page: Option<i64>,
    pub(crate) page_size: Option<i64>,
    pub(crate) level: Option<String>,
    pub(crate) event_code: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct AdminLogExportQuery {
    pub(crate) from: Option<String>,
    pub(crate) to: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminScheduledTasksQuery {
    pub(crate) page: Option<i64>,
    pub(crate) page_size: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminScheduledTaskPlansQuery {
    pub(crate) page: Option<i64>,
    pub(crate) page_size: Option<i64>,
    pub(crate) task_type: Option<String>,
    pub(crate) search: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminScheduledTaskRequest {
    pub(crate) owner_type: String,
    pub(crate) owner_id: String,
    pub(crate) task_type: String,
    pub(crate) schedule: Option<String>,
    pub(crate) is_enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminScheduledTaskRunRequest {
    pub(crate) owner_type: String,
    pub(crate) owner_id: String,
    pub(crate) task_type: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminScheduledTaskPlanCreateRequest {
    pub(crate) task_type: String,
    pub(crate) name: String,
    pub(crate) schedule: Option<String>,
    pub(crate) is_enabled: Option<bool>,
    pub(crate) library_ids: Vec<String>,
    pub(crate) resource_limit: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminScheduledTaskPlanUpdateRequest {
    pub(crate) name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub(crate) schedule: Option<Option<String>>,
    pub(crate) is_enabled: Option<bool>,
    pub(crate) library_ids: Option<Vec<String>>,
    pub(crate) resource_limit: Option<Value>,
}

const SCHEDULE_TASK_TYPES: [&str; 4] = [
    "RECONCILIATION_SCAN",
    "METADATA_PARSE",
    "CHAPTER_DETECTION",
    "AUTO_LIBRARY_COVER",
];

pub(crate) async fn admin_run_scheduled_task(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<AdminScheduledTaskRunRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(scheduled_tasks) = state.scheduled_tasks.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let owner_type = request.owner_type.trim();
    let owner_id = request.owner_id.trim();
    let task_type = request.task_type.trim();
    match scheduled_tasks
        .run_task(owner_type, owner_id, task_type)
        .await
    {
        Ok(run) => (
            StatusCode::ACCEPTED,
            Json(json!({
                "status": "ACCEPTED",
                "taskType": run.task_type(),
                "run": scheduled_task_run_json(&run),
            })),
        )
            .into_response(),
        Err(error) => scheduled_task_error(&headers, error),
    }
}

pub(crate) fn scheduled_task_run_json(run: &ScheduledTaskRun) -> Value {
    match run {
        ScheduledTaskRun::Reconciliation { job } => json!({ "jobId": job.id }),
        ScheduledTaskRun::Metadata { job } => json!({ "jobId": job.id }),
        ScheduledTaskRun::StrmMediaInfo {
            operation_id, jobs, ..
        } => json!({
            "operationId": operation_id,
            "jobIds": jobs.iter().map(|job| job.id.clone()).collect::<Vec<_>>(),
        }),
        ScheduledTaskRun::ChapterDetection { job } => json!({ "jobId": job.id }),
        ScheduledTaskRun::AutoLibraryCover { job } => {
            json!({ "jobId": job.id, "libraryId": job.library_id })
        }
        ScheduledTaskRun::DanmakuMatch { jobs } => {
            json!({ "jobIds": jobs.iter().map(|job| job.id.clone()).collect::<Vec<_>>() })
        }
    }
}

pub(crate) fn scheduled_task_error(headers: &HeaderMap, error: ScheduledTaskError) -> Response {
    let (status, code, message) = match error {
        ScheduledTaskError::InvalidOwner
        | ScheduledTaskError::UnsupportedTask
        | ScheduledTaskError::NotRegistered => (
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "注册任务不存在",
        ),
        ScheduledTaskError::Disabled => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "任务当前不可执行",
        ),
        ScheduledTaskError::Scan(ScanJobError::AlreadyActive(_))
        | ScheduledTaskError::Strm(StrmProbeError::AlreadyActive)
        | ScheduledTaskError::Chapter(ChapterDetectionError::AlreadyActive)
        | ScheduledTaskError::Cover(LibraryCoverError::AlreadyActive)
        | ScheduledTaskError::Danmaku(DanmakuServiceError::AlreadyActive) => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "同类任务已有运行中的作业",
        ),
        ScheduledTaskError::ServiceUnavailable
        | ScheduledTaskError::Scan(_)
        | ScheduledTaskError::Metadata(_)
        | ScheduledTaskError::MetadataDispatch(_)
        | ScheduledTaskError::Strm(_)
        | ScheduledTaskError::Chapter(_)
        | ScheduledTaskError::Cover(_)
        | ScheduledTaskError::Danmaku(_)
        | ScheduledTaskError::Storage(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "注册任务暂时无法执行",
        ),
    };
    api_error(headers, status, code, message).into_response()
}

pub(crate) async fn admin_list_task_activity(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut activities = Vec::new();
    let scan_jobs = match database.list_scan_jobs_for_activity(100).await {
        Ok(jobs) => jobs,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    activities.extend(scan_jobs.iter().map(|job| {
        json!({
            "id": job.id,
            "kind": "scan",
            "taskType": job.job_type,
            "libraryId": job.library_id,
            "status": job.status,
            "processedCount": job.processed_count,
            "totalCount": job.total_count,
            "discoveryCompleted": job.discovery_completed,
            "cancelRequested": job.cancel_requested,
            "currentItem": job.current_item,
            "scanPhase": job.scan_phase,
        })
    }));
    let metadata_jobs = match database
        .list_metadata_reidentify_jobs_for_activity(100)
        .await
    {
        Ok(jobs) => jobs,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let metadata_labels = match database
        .list_current_metadata_reidentify_items(
            &metadata_jobs
                .iter()
                .map(|job| job.id.clone())
                .collect::<Vec<_>>(),
        )
        .await
    {
        Ok(items) => activity_item_labels(items),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    activities.extend(metadata_jobs.iter().map(|job| {
        json!({
            "id": job.id,
            "kind": "metadata",
            "taskType": job.mode,
            "libraryId": job.library_id,
            "status": job.status,
            "processedCount": job.processed_count,
            "totalCount": job.total_count,
            "cancelRequested": job.cancel_requested,
            "currentItem": metadata_labels.get(&job.id),
        })
    }));

    for status in ["PENDING", "RUNNING"] {
        let strm_jobs = match database.list_strm_probe_jobs(Some(status), 0, 100).await {
            Ok(jobs) => jobs,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        activities.extend(strm_jobs.iter().map(|job| {
            json!({
                "id": job.id,
                "kind": "strm",
                "taskType": "STRM_MEDIA_INFO",
                "libraryId": job.library_id,
                "status": job.status,
                "processedCount": job.processed_count,
                "totalCount": job.total_count,
                "cancelRequested": job.cancel_requested,
            })
        }));

        let chapter_jobs = match database
            .list_chapter_detection_jobs(Some(status), 0, 100)
            .await
        {
            Ok(jobs) => jobs,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        let chapter_labels = match database
            .list_current_chapter_detection_items(
                &chapter_jobs
                    .iter()
                    .map(|job| job.id.clone())
                    .collect::<Vec<_>>(),
            )
            .await
        {
            Ok(items) => activity_item_labels(items),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        activities.extend(chapter_jobs.iter().map(|job| {
            json!({
                "id": job.id,
                "kind": "chapter",
                "taskType": "CHAPTER_DETECTION",
                "libraryId": job.library_id,
                "status": job.status,
                "processedCount": job.processed_count,
                "totalCount": job.total_count,
                "cancelRequested": job.cancel_requested,
                "currentItem": chapter_labels.get(&job.id),
            })
        }));

        let danmaku_jobs = match database.list_danmaku_match_jobs(Some(status), 0, 100).await {
            Ok(jobs) => jobs,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        let danmaku_labels = match database
            .list_current_danmaku_match_items(
                &danmaku_jobs
                    .iter()
                    .map(|job| job.id.clone())
                    .collect::<Vec<_>>(),
            )
            .await
        {
            Ok(items) => activity_item_labels(items),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        activities.extend(danmaku_jobs.iter().map(|job| {
            json!({
                "id": job.id,
                "kind": "danmaku",
                "taskType": "DANMAKU_MATCH",
                "libraryId": job.library_id,
                "status": job.status,
                "processedCount": job.processed_count,
                "totalCount": job.total_count,
                "cancelRequested": job.cancel_requested,
                "currentItem": danmaku_labels.get(&job.id),
            })
        }));

        let library_cover_jobs = match database.list_library_cover_jobs(Some(status), 0, 100).await
        {
            Ok(jobs) => jobs,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        activities.extend(library_cover_jobs.iter().map(|job| {
            json!({
                "id": job.id,
                "kind": "cover",
                "taskType": "AUTO_LIBRARY_COVER",
                "libraryId": job.library_id,
                "status": job.status,
                "processedCount": job.processed_count,
                "totalCount": job.total_count,
            })
        }));
    }
    activities.sort_by(|left, right| {
        right["status"]
            .as_str()
            .unwrap_or_default()
            .cmp(left["status"].as_str().unwrap_or_default())
            .then_with(|| {
                left["id"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["id"].as_str().unwrap_or_default())
            })
    });
    Json(json!({ "activities": activities })).into_response()
}

fn activity_item_labels(
    items: Vec<(String, crate::storage::StoredJobActivityItem)>,
) -> std::collections::HashMap<String, String> {
    items
        .into_iter()
        .map(|(job_id, item)| (job_id, activity_item_label(&item)))
        .collect()
}

fn activity_item_label(item: &crate::storage::StoredJobActivityItem) -> String {
    if item.item_type != "EPISODE" {
        return item.title.clone();
    }
    let series_title = item
        .series_title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(item.title.as_str());
    let episode_number = item.episode_number.map(|number| format!("E{number:02}"));
    let season_number = item.season_number.map(|number| format!("S{number:02}"));
    let season_episode = match (season_number, episode_number) {
        (Some(season), Some(episode)) => Some(format!("{season}{episode}")),
        (Some(season), None) => Some(season),
        (None, Some(episode)) => Some(episode),
        (None, None) => None,
    };
    [
        Some(series_title.to_owned()),
        season_episode,
        Some(item.title.clone()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

pub(crate) async fn admin_list_scheduled_tasks(
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

pub(crate) async fn admin_list_scheduled_task_plans(
    headers: HeaderMap,
    Query(query): Query<AdminScheduledTaskPlansQuery>,
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
    let task_type = query
        .task_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_uppercase);
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .list_scheduled_task_plans(offset, limit, task_type.as_deref(), query.search.as_deref())
        .await
    {
        Ok((plans, total)) => Json(json!({
            "plans": plans.iter().map(scheduled_task_plan_json).collect::<Vec<_>>(),
            "total": total,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_create_scheduled_task_plan(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<AdminScheduledTaskPlanCreateRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let task_type = request.task_type.trim().to_ascii_uppercase();
    if !SCHEDULE_TASK_TYPES.contains(&task_type.as_str()) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "任务类型无效",
        )
        .into_response();
    }
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > 128 || request.library_ids.is_empty() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "计划名称和媒体库不能为空",
        )
        .into_response();
    }
    let schedule = match request
        .schedule
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) if validate_cron(value).is_err() => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "Cron 执行计划无效",
            )
            .into_response();
        }
        Some(value) => Some(value.to_owned()),
        None => None,
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let template = match database
        .find_scheduled_task_config("LIBRARY", &request.library_ids[0], &task_type)
        .await
    {
        Ok(Some(task)) => task,
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::InvalidRequest,
                "选中的媒体库尚未注册该任务",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let resource_limit_json = match request.resource_limit {
        Some(value) if !value.is_object() => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "资源限制必须是 JSON 对象",
            )
            .into_response();
        }
        Some(value) => match serde_json::to_string(&value) {
            Ok(value) => value,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => template.resource_limit_json.clone(),
    };
    let plan_id = uuid::Uuid::now_v7().to_string();
    let is_enabled = request.is_enabled.unwrap_or(schedule.is_some());
    match database
        .create_scheduled_task_plan(
            &plan_id,
            &task_type,
            name,
            &template.task_name,
            &template.task_description,
            &template.source_type,
            template.plugin_id.as_deref(),
            schedule.as_deref(),
            is_enabled,
            &resource_limit_json,
            &request.library_ids,
        )
        .await
    {
        Ok(Some(plan)) => (
            StatusCode::CREATED,
            Json(json!({ "plan": scheduled_task_plan_json(&plan) })),
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(StorageError::Conflict(message)) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            &message,
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_update_scheduled_task_plan(
    headers: HeaderMap,
    Path(plan_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<AdminScheduledTaskPlanUpdateRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(existing) = database
        .find_scheduled_task_plan(&plan_id)
        .await
        .ok()
        .flatten()
    else {
        return api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "执行计划不存在",
        )
        .into_response();
    };
    let name = request
        .name
        .as_deref()
        .unwrap_or(&existing.plan_name)
        .trim();
    let library_ids = request.library_ids.clone().unwrap_or_else(|| {
        existing
            .libraries
            .iter()
            .map(|library| library.id.clone())
            .collect()
    });
    if name.is_empty() || name.chars().count() > 128 || library_ids.is_empty() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "计划名称和媒体库不能为空",
        )
        .into_response();
    }
    let schedule = match request.schedule.as_ref() {
        None => existing.cron_or_interval.clone(),
        Some(None) => None,
        Some(Some(value)) if value.trim().is_empty() => None,
        Some(Some(value)) if validate_cron(value.trim()).is_err() => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "Cron 执行计划无效",
            )
            .into_response();
        }
        Some(Some(value)) => Some(value.trim().to_owned()),
    };
    let is_enabled = request.is_enabled.unwrap_or_else(|| {
        if request.schedule.is_some() {
            schedule.is_some()
        } else {
            existing.is_enabled
        }
    });
    let resource_limit_json = match request.resource_limit {
        Some(value) if !value.is_object() => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "资源限制必须是 JSON 对象",
            )
            .into_response();
        }
        Some(value) => match serde_json::to_string(&value) {
            Ok(value) => value,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => existing.resource_limit_json.clone(),
    };
    match database
        .update_scheduled_task_plan(
            &plan_id,
            name,
            schedule.as_deref(),
            is_enabled,
            &library_ids,
            &existing.task_name,
            &existing.task_description,
            &existing.source_type,
            existing.plugin_id.as_deref(),
            &resource_limit_json,
        )
        .await
    {
        Ok(Some(plan)) => Json(json!({ "plan": scheduled_task_plan_json(&plan) })).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(StorageError::Conflict(message)) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            &message,
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_delete_scheduled_task_plan(
    headers: HeaderMap,
    Path(plan_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.delete_scheduled_task_plan(&plan_id).await {
        Ok(true) => {
            record_audit_event(
                &state,
                &headers,
                "SCHEDULE_PLAN_DELETED",
                Some("scheduled_task_plan"),
                Some(&plan_id),
                "{}",
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "执行计划不存在",
        )
        .into_response(),
        Err(StorageError::Conflict(message)) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            &message,
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_run_scheduled_task_plan(
    headers: HeaderMap,
    Path(plan_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(plan) = database
        .find_scheduled_task_plan(&plan_id)
        .await
        .ok()
        .flatten()
    else {
        return api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "执行计划不存在",
        )
        .into_response();
    };
    if plan.scope_type != "LIBRARY" {
        return api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "全局插件计划不能通过媒体库计划接口执行",
        )
        .into_response();
    }
    let Some(scheduled_tasks) = state.scheduled_tasks.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut runs = Vec::new();
    let mut skipped_library_ids = Vec::new();
    for library in plan.libraries {
        match scheduled_tasks
            .run_task("LIBRARY", &library.id, &plan.task_type)
            .await
        {
            Ok(run) => runs.push(json!({
                "libraryId": library.id,
                "run": scheduled_task_run_json(&run),
            })),
            Err(error) => {
                tracing::warn!(
                    plan_id = %plan_id,
                    library_id = %library.id,
                    %error,
                    "scheduled task plan member was not started"
                );
                skipped_library_ids.push(library.id);
            }
        }
    }
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "status": "ACCEPTED",
            "planId": plan_id,
            "taskType": plan.task_type,
            "runs": runs,
            "skippedLibraryIds": skipped_library_ids,
        })),
    )
        .into_response()
}

pub(crate) async fn admin_upsert_scheduled_task(
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
    let is_global_danmaku = owner_type == "GLOBAL"
        && task_type == crate::application::schedule::DANMAKU_MATCH_TASK_TYPE;
    if !SCHEDULE_TASK_TYPES.contains(&task_type.as_str()) && !is_global_strm && !is_global_danmaku {
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
        if is_global_strm || is_global_danmaku {
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
                    if is_global_strm {
                        "STRM 媒体信息任务必须保留 Cron 执行计划"
                    } else {
                        "弹幕匹配任务必须保留 Cron 执行计划"
                    },
                )
                .into_response();
            };
            let Some(plugins) = state.plugins.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            let result = if is_global_strm {
                plugins.update_media_info_schedule(schedule).await
            } else {
                plugins.update_danmaku_schedule(schedule).await
            };
            if let Err(error) = result {
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
                Some(&format!("global:{task_type}")),
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
    if let Some(schedule) = schedule.as_deref()
        && validate_cron(schedule).is_err()
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "Cron 执行计划无效",
        )
        .into_response();
    }
    let mut settings = LibrarySettingsPatch::default();
    match task_type.as_str() {
        "RECONCILIATION_SCAN" => settings.reconciliation_schedule = Some(schedule),
        "METADATA_PARSE" => settings.metadata_schedule = Some(schedule),
        "AUTO_LIBRARY_COVER" => {
            let task = match database
                .upsert_scheduled_task_config(
                    "LIBRARY",
                    &library_id.to_string(),
                    &task_type,
                    schedule.as_deref(),
                    enabled,
                )
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
            return (
                StatusCode::OK,
                Json(json!({ "scheduledTask": scheduled_task_json(&task) })),
            )
                .into_response();
        }
        "CHAPTER_DETECTION" => {
            let Some(schedule) = schedule.as_deref() else {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "片头片尾检测任务必须保留 Cron 执行计划",
                )
                .into_response();
            };
            let plugin_id = match database
                .find_scheduled_task_config("LIBRARY", &library_id.to_string(), &task_type)
                .await
            {
                Ok(Some(task)) => match task.plugin_id {
                    Some(plugin_id) => plugin_id,
                    None => {
                        return api_error(
                            &headers,
                            StatusCode::NOT_FOUND,
                            lux::ApiErrorCode::NotFound,
                            "片头片尾检测任务插件未注册",
                        )
                        .into_response();
                    }
                },
                Ok(None) => {
                    return api_error(
                        &headers,
                        StatusCode::NOT_FOUND,
                        lux::ApiErrorCode::NotFound,
                        "片头片尾检测任务插件未注册",
                    )
                    .into_response();
                }
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            let Some(plugins) = state.plugins.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            if let Err(error) = plugins
                .update_chapter_detector_schedule(&plugin_id, schedule)
                .await
            {
                return plugin_error(&headers, error);
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
            return (
                StatusCode::OK,
                Json(json!({ "scheduledTask": scheduled_task_json(&task) })),
            )
                .into_response();
        }
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

pub(crate) async fn admin_list_jobs(
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

pub(crate) async fn admin_get_job(
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

pub(crate) async fn admin_list_job_events(
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
    let Some(config_dir) = state.config_dir.as_deref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let file_events = match crate::observability::logs::LogStore::new(config_dir)
        .list_scan_job_events(
            &job_id,
            level.as_deref(),
            event_code.as_deref(),
            offset,
            limit,
        )
        .await
    {
        Ok(events) => events,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    Json(json!({
        "events": file_events.1.iter().map(scan_job_event_json).collect::<Vec<_>>(),
        "total": file_events.0,
        "page": offset / limit + 1,
        "pageSize": limit,
    }))
    .into_response()
}

pub(crate) async fn admin_retry_scan(
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

pub(crate) fn scan_job_json_from_storage(job: &crate::storage::StoredScanJob) -> Value {
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
        "createdAt": job.created_at,
        "startedAt": job.started_at,
        "finishedAt": job.finished_at,
        "currentItem": job.current_item,
        "scanPhase": job.scan_phase,
    })
}

pub(crate) fn scheduled_task_json(task: &crate::storage::StoredScheduledTaskConfig) -> Value {
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

pub(crate) fn scheduled_task_plan_json(plan: &crate::storage::StoredScheduledTaskPlan) -> Value {
    let resource_limit =
        serde_json::from_str::<Value>(&plan.resource_limit_json).unwrap_or_else(|_| json!({}));
    json!({
        "id": plan.id,
        "taskType": plan.task_type,
        "name": plan.plan_name,
        "taskName": plan.task_name,
        "description": plan.task_description,
        "sourceType": plan.source_type,
        "pluginId": plan.plugin_id,
        "schedule": plan.cron_or_interval,
        "isEnabled": plan.is_enabled,
        "resourceLimit": resource_limit,
        "scopeType": plan.scope_type,
        "isDefault": plan.is_default,
        "libraries": plan.libraries.iter().map(|library| json!({
            "id": library.id,
            "name": library.name,
        })).collect::<Vec<_>>(),
        "libraryCount": plan.libraries.len(),
        "createdAt": plan.created_at,
        "updatedAt": plan.updated_at,
    })
}

pub(crate) fn scan_job_event_json(event: &crate::observability::logs::ScanJobLogEvent) -> Value {
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

pub(crate) fn scan_job_json(job: &crate::application::scanner::ScanJob) -> Value {
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
        "createdAt": job.created_at,
        "startedAt": job.started_at,
        "finishedAt": job.finished_at,
        "currentItem": job.current_item,
        "scanPhase": job.scan_phase,
    })
}

pub(crate) async fn require_admin(
    headers: &HeaderMap,
    state: &AppState,
    require_csrf: bool,
) -> Result<(), Response> {
    if let Some(principal) = resolve_shared_admin_api_key(headers, state).await? {
        if !principal.can_manage_server()
            || (state.remote_access.is_remote(
                header_str(headers, "x-lux-peer-ip"),
                header_str(headers, "x-forwarded-for"),
            ) && !principal.can_remote_access())
        {
            return Err(api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::PermissionDenied,
                "没有服务器管理权限",
            )
            .into_response());
        }
        return Ok(());
    }
    if users::lux_user_token_from_headers(headers).is_some() {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "客户端令牌不能调用管理员接口",
        )
        .into_response());
    }
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
    if require_csrf && !users::has_client_token(headers) {
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

pub(crate) async fn require_admin_web_session(
    headers: &HeaderMap,
    state: &AppState,
    require_csrf: bool,
) -> Result<(), Response> {
    if users::has_client_token(headers) {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "客户端令牌不能管理 API Key",
        )
        .into_response());
    }
    require_admin(headers, state, require_csrf).await
}

pub(crate) async fn resolve_shared_admin_api_key(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<Option<crate::auth::users::AuthenticationPrincipal>, Response> {
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
    service.resolve_principal(&candidate).await.map_err(|_| {
        api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "认证暂时不可用",
        )
        .into_response()
    })
}

pub(crate) fn lux_api_key_from_headers(headers: &HeaderMap) -> Option<String> {
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

pub(crate) async fn normalize_lux_api_key_query(request: Request<Body>, next: Next) -> Response {
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

pub(crate) async fn record_audit_event(
    state: &AppState,
    headers: &HeaderMap,
    event_type: &str,
    target_type: Option<&str>,
    target_id: Option<&str>,
    metadata_json: &str,
) {
    if state.database.is_none() {
        return;
    }
    let (actor_user_id, actor_username, metadata_json) =
        if let Some(candidate) = lux_api_key_from_headers(headers) {
            let Some(service) = state.admin_api_key.as_ref() else {
                return;
            };
            let Ok(Some(_)) = service.resolve_principal(&candidate).await else {
                return;
            };
            (None, None, audit_metadata_for_shared_api_key(metadata_json))
        } else {
            let (Some(auth), Some(session_token)) =
                (state.auth.as_ref(), request_cookie(headers, "lux_session"))
            else {
                return;
            };
            let Ok(Some(session)) = auth.resolve(&session_token).await else {
                return;
            };
            (
                Some(session.user.id.to_string()),
                Some(session.user.username_normalized.clone()),
                metadata_json.to_owned(),
            )
        };
    let Some(config_dir) = state.config_dir.as_deref() else {
        return;
    };
    let event_id = Uuid::now_v7().to_string();
    match crate::observability::logs::LogStore::new(config_dir)
        .append_audit_event(crate::observability::logs::NewAuditLogEvent {
            id: &event_id,
            actor_user_id: actor_user_id.as_deref(),
            actor_username: actor_username.as_deref(),
            event_type,
            target_type,
            target_id,
            metadata_json: &metadata_json,
        })
        .await
    {
        Ok(()) => state
            .admin_events
            .publish(admin_event_scope_for_audit(event_type)),
        Err(error) => tracing::error!(
            event_type,
            error_kind = ?error.kind(),
            "failed to persist admin audit event"
        ),
    }
}

pub(crate) fn audit_metadata_for_shared_api_key(metadata_json: &str) -> String {
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

pub(crate) fn admin_event_scope_for_audit(event_type: &str) -> AdminEventScope {
    if event_type == "SETTINGS_UPDATED" {
        return AdminEventScope::Settings;
    }
    if event_type.starts_with("PLUGIN_") {
        return AdminEventScope::Plugins;
    }
    if event_type.starts_with("USER_") || event_type == "LIBRARY_ACCESS_UPDATED" {
        return AdminEventScope::Users;
    }
    if event_type.starts_with("LIBRARY_") || event_type.starts_with("SCHEDULE_") {
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

pub(crate) async fn record_activity_event(
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

pub(crate) fn playback_activity_event_type(
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

pub(crate) async fn admin_list_audit(
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
    let Some(config_dir) = state.config_dir.as_deref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let file_events = match crate::observability::logs::LogStore::new(config_dir)
        .list_audit_events(offset, limit)
        .await
    {
        Ok((_, events)) => events,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let events = file_events
        .into_iter()
        .map(|event| {
            json!({
                "id": event.id,
                "actorUserId": event.actor_user_id,
                "actorUsername": event.actor_username,
                "eventType": event.event_type,
                "targetType": event.target_type,
                "targetId": event.target_id,
                "metadata": serde_json::from_str::<Value>(&event.metadata_json)
                    .unwrap_or_else(|_| json!({})),
                "createdAt": event.created_at,
            })
        })
        .collect::<Vec<_>>();
    Json(json!({
        "events": events,
        "page": offset / limit + 1,
        "pageSize": limit,
    }))
    .into_response()
}

pub(crate) async fn admin_list_logs(
    headers: HeaderMap,
    Query(query): Query<MetadataCandidateQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_audit(headers, Query(query), State(state)).await
}

pub(crate) async fn admin_export_logs(
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

pub(crate) fn log_export_error_response(headers: &HeaderMap, error: LogExportError) -> Response {
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

pub(crate) async fn probe_directory_writable(path: &FsPath) -> bool {
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
pub(crate) struct WebhookDestinationCreateRequest {
    pub(crate) name: String,
    pub(crate) url: String,
    #[serde(default = "default_enabled")]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) allow_private_network: bool,
    #[serde(default)]
    pub(crate) event_types: Vec<String>,
    pub(crate) payload_format: Option<String>,
    pub(crate) secret: Option<String>,
    pub(crate) provider_plugin_id: Option<String>,
    pub(crate) provider_config: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WebhookDestinationUpdateRequest {
    pub(crate) name: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) enabled: Option<bool>,
    pub(crate) allow_private_network: Option<bool>,
    pub(crate) event_types: Option<Vec<String>>,
    pub(crate) payload_format: Option<String>,
    pub(crate) provider_plugin_id: Option<String>,
    pub(crate) provider_config: Option<Value>,
}

const fn default_enabled() -> bool {
    true
}

pub(crate) async fn admin_list_webhook_destinations(
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

pub(crate) async fn admin_get_webhook_destination(
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

pub(crate) async fn admin_create_webhook_destination(
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

pub(crate) async fn admin_update_webhook_destination(
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

pub(crate) async fn admin_delete_webhook_destination(
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

pub(crate) async fn admin_test_webhook_destination(
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

pub(crate) async fn admin_rotate_webhook_secret(
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

pub(crate) async fn admin_list_webhook_deliveries(
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

pub(crate) async fn admin_retry_webhook_delivery(
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

pub(crate) fn webhook_error_response(headers: &HeaderMap, error: WebhookError) -> Response {
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

pub(crate) async fn admin_health(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    match admin_health_payload(&state).await {
        Ok(payload) => Json(payload).into_response(),
        Err(status) => status.into_response(),
    }
}

pub(crate) async fn admin_events(headers: HeaderMap, State(state): State<AppState>) -> Response {
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

    event_stream_response(reader)
}

pub(crate) fn event_stream_response(reader: tokio::io::DuplexStream) -> Response {
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

pub(crate) async fn user_events(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_web_user(&headers, &state).await {
        return response;
    }

    let mut receiver = state.user_events.subscribe();
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
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => crate::application::admin_events::UserEventScope::Home,
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

    event_stream_response(reader)
}

pub(crate) async fn admin_health_payload(state: &AppState) -> Result<Value, StatusCode> {
    let Some(database) = state.database.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let schema_version = match database.schema_version().await {
        Ok(version) => version,
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let resources = state.resources.snapshot().await;
    let database_pool = database.pool_snapshot();
    let probes = state
        .admin_health_probe_cache
        .get_or_probe(|| async {
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
            AdminHealthProbeSnapshot {
                database_writable,
                config_available,
                config_writable,
                ffprobe_available,
            }
        })
        .await;
    let AdminHealthProbeSnapshot {
        database_writable,
        config_available,
        config_writable,
        ffprobe_available,
    } = probes;
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
    let jobs = match database.count_scan_jobs_by_status().await {
        Ok(counts) => json!({
            "scanRunning": counts.running,
            "scanFailed": counts.failed,
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
            "pool": {
                "maxConnections": database_pool.max_connections,
                "size": database_pool.size,
                "idle": database_pool.idle,
                "inUse": database_pool.in_use,
                "saturated": database_pool.saturated,
            },
        },
        "config": { "available": config_available, "writable": config_writable },
        "ffprobe": { "available": ffprobe_available },
        "jobs": {
            "scanRunning": jobs["scanRunning"],
            "scanFailed": jobs["scanFailed"],
            "metadataReidentifyRunning": metadata_reidentify_running,
        },
        "libraries": libraries,
    }))
}

pub(crate) async fn admin_database_diagnostics_status(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(diagnostics) = state.database_diagnostics.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut response = Json(diagnostics.status().await).into_response();
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response
}

pub(crate) async fn admin_start_database_diagnostics(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(diagnostics) = state.database_diagnostics.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match diagnostics.request_collection().await {
        Ok(status) => {
            let mut response = (StatusCode::ACCEPTED, Json(status)).into_response();
            response.headers_mut().insert(
                CACHE_CONTROL,
                HeaderValue::from_static("no-store, max-age=0"),
            );
            response
        }
        Err(
            crate::application::database_diagnostics::DatabaseDiagnosticsStartError::AlreadyRunning,
        ) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "数据库体检正在运行",
        )
        .into_response(),
    }
}

pub(crate) async fn admin_export_database_diagnostics(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(diagnostics) = state.database_diagnostics.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(report) = diagnostics.report().await else {
        return api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "数据库体检报告尚未就绪",
        )
        .into_response();
    };
    database_diagnostics_export_response(report)
}

fn database_diagnostics_export_response(report: Value) -> Response {
    let mut response = Json(report).into_response();
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"lux-database-diagnostics.json\""),
    );
    response
}

#[cfg(test)]
mod database_diagnostics_export_tests {
    use super::*;

    #[tokio::test]
    async fn report_export_is_a_no_store_json_attachment() {
        let response = database_diagnostics_export_response(json!({ "backend": "POSTGRESQL" }));

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store, max-age=0");
        assert_eq!(
            response.headers()[CONTENT_DISPOSITION],
            "attachment; filename=\"lux-database-diagnostics.json\""
        );
        let body = to_bytes(response.into_body(), 1024)
            .await
            .expect("JSON body");
        assert_eq!(body.as_ref(), br#"{"backend":"POSTGRESQL"}"#);
    }
}

pub(crate) const DEFAULT_SERVER_NAME: &str = "Lux Server";
const DASHBOARD_ACTIVITY_LIMIT: i64 = 24;
const DASHBOARD_PLAYBACK_LIMIT: usize = 24;

pub(crate) async fn admin_dashboard(headers: HeaderMap, State(state): State<AppState>) -> Response {
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
    let sessions = match database.list_playback_sessions(None, None).await {
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
        Ok(events) => {
            let series_by_event_id = match dashboard_activity_series_metadata(&state, &events).await
            {
                Ok(series) => series,
                Err(status) => return status.into_response(),
            };
            events
                .iter()
                .map(|event| {
                    dashboard_activity_json(
                        event,
                        state.ip_location.as_ref(),
                        series_by_event_id.get(&event.id),
                    )
                })
                .collect::<Vec<_>>()
        }
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

pub(crate) fn dashboard_stats_json(stats: &DashboardStats) -> Value {
    json!({
        "movieCount": stats.movie_count,
        "seriesCount": stats.series_count,
        "userCount": stats.user_count,
    })
}

pub(crate) async fn dashboard_playback_json(
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
    let sessions = sessions
        .iter()
        .take(DASHBOARD_PLAYBACK_LIMIT)
        .collect::<Vec<_>>();
    let web_play_session_ids = sessions
        .iter()
        .filter(|session| {
            session.play_session_id.starts_with("lux-emby:")
                || session.play_session_id.starts_with("lux-web:")
        })
        .map(|session| session.play_session_id.clone())
        .collect::<Vec<_>>();
    let Some(database) = state.database.as_ref() else {
        return Ok(Vec::new());
    };
    let web_sessions = database
        .find_web_playback_sessions_for_playbacks(&web_play_session_ids)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .into_iter()
        .map(|session| (session.play_session_id.clone(), session))
        .collect::<HashMap<_, _>>();
    let item_ids = sessions
        .iter()
        .map(|session| session.item_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let items_by_id = catalog
        .find_items(principal, &item_ids)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let series_ids = items_by_id
        .values()
        .filter(|item| item.item_type == "EPISODE")
        .filter_map(|item| item.series_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let series_by_id = catalog
        .find_items(principal, &series_ids)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let mut values = Vec::new();
    for session in sessions {
        let Some(item) = items_by_id.get(&session.item_id) else {
            continue;
        };
        let series = item
            .series_id
            .as_deref()
            .and_then(|series_id| series_by_id.get(series_id));
        let remote_ip_location = session.remote_ip.as_deref().and_then(|remote_ip| {
            state
                .ip_location
                .as_ref()
                .and_then(|service| service.cached_or_schedule(remote_ip))
        });
        values.push(dashboard_playback_item_json(
            session,
            item,
            series,
            user_names
                .get(&session.user_id)
                .map(String::as_str)
                .unwrap_or("未知账户"),
            remote_ip_location.as_ref(),
            web_sessions.get(&session.play_session_id),
        ));
    }
    Ok(values)
}

pub(crate) fn dashboard_playback_item_json(
    session: &StoredPlaybackSession,
    item: &CatalogItem,
    series: Option<&CatalogItem>,
    user_name: &str,
    remote_ip_location: Option<&IpLocation>,
    web_session: Option<&crate::storage::StoredWebPlaybackSession>,
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
        "playMethod": dashboard_play_method(session, web_session),
        "playbackPlan": web_session.map(|session| session.plan.as_str()),
        "serverTier": web_session.map(|session| session.tier),
        "output": web_session.and_then(|web_session| {
            (web_session.plan == "SERVER_HLS").then(|| dashboard_playback_output(web_session, source))
        }),
        "source": source.map(dashboard_source_json),
    })
}

fn dashboard_play_method(
    session: &StoredPlaybackSession,
    web_session: Option<&crate::storage::StoredWebPlaybackSession>,
) -> &'static str {
    let Some(web_session) = web_session.filter(|session| session.plan == "SERVER_HLS") else {
        return if session.play_session_id.starts_with("lux-emby:") {
            "Transcode"
        } else {
            "DirectPlay"
        };
    };
    if web_session.tier <= i64::from(ServerTier::Remux.number()) {
        "DirectStream"
    } else {
        "Transcode"
    }
}

fn dashboard_playback_output(
    web_session: &crate::storage::StoredWebPlaybackSession,
    source: Option<&CatalogSource>,
) -> Value {
    let source_video_codec = source.and_then(|source| {
        source
            .streams
            .iter()
            .find(|stream| stream.stream_type.eq_ignore_ascii_case("VIDEO"))
            .and_then(|stream| stream.codec.clone())
    });
    let source_audio_codec = source.and_then(|source| {
        source
            .streams
            .iter()
            .find(|stream| stream.stream_type.eq_ignore_ascii_case("AUDIO"))
            .and_then(|stream| stream.codec.clone())
    });
    let video_transcoded = web_session.tier >= i64::from(ServerTier::HardwareTranscode.number());
    let audio_transcoded = web_session.tier >= i64::from(ServerTier::AudioTranscode.number());
    json!({
        "container": web_session.transcoding_container,
        "videoCodec": web_session.video_codec.as_deref().or_else(|| {
            video_transcoded.then_some("h264").or(source_video_codec.as_deref())
        }),
        "audioCodec": web_session.audio_codec.as_deref().or_else(|| {
            audio_transcoded.then_some("aac").or(source_audio_codec.as_deref())
        }),
        "videoBitrate": web_session.video_bitrate,
        "audioBitrate": web_session.audio_bitrate,
    })
}

pub(crate) fn dashboard_ip_location_json(location: &IpLocation) -> Value {
    json!({
        "location": location.formatted_location(),
        "district": location.district,
        "street": location.street,
        "isp": location.isp,
    })
}

pub(crate) fn dashboard_source_json(source: &CatalogSource) -> Value {
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

pub(crate) fn dashboard_activity_json(
    event: &crate::storage::StoredActivityEvent,
    ip_location: Option<&IpLocationService>,
    series: Option<&DashboardActivitySeriesMetadata>,
) -> Value {
    let metadata =
        serde_json::from_str::<Value>(&event.metadata_json).unwrap_or_else(|_| json!({}));
    let remote_ip = metadata.get("remoteIp").and_then(Value::as_str);
    let remote_ip_location = remote_ip.and_then(|remote_ip| {
        ip_location.and_then(|service| service.cached_or_schedule(remote_ip))
    });
    json!({
        "id": event.id,
        "userId": event.actor_user_id,
        "userName": event.actor_username,
        "eventType": event.event_type,
        "targetType": event.target_type,
        "targetId": event.target_id,
        "targetTitle": event.target_title,
        "targetSeriesTitle": series.map(|series| series.title.as_str()),
        "targetSeasonNumber": series.and_then(|series| series.season_number),
        "targetSeriesSeasonCount": series.and_then(|series| series.season_count),
        "metadata": metadata,
        "remoteIp": remote_ip,
        "remoteIpLocation": remote_ip_location
            .as_ref()
            .map(dashboard_ip_location_json),
        "createdAt": event.created_at,
    })
}

pub(crate) struct DashboardActivitySeriesMetadata {
    title: String,
    season_number: Option<i64>,
    season_count: Option<i64>,
}

async fn dashboard_activity_series_metadata(
    state: &AppState,
    events: &[crate::storage::StoredActivityEvent],
) -> Result<HashMap<String, DashboardActivitySeriesMetadata>, StatusCode> {
    let Some(catalog) = state.catalog.as_ref() else {
        return Ok(HashMap::new());
    };
    let item_ids = events
        .iter()
        .filter(|event| event.target_type.as_deref() == Some("media_item"))
        .filter_map(|event| event.target_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if item_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let principal = AccessPrincipal::new(crate::domain::ids::UserId::new(), true);
    let items_by_id = catalog
        .find_items(principal, &item_ids)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let series_ids = items_by_id
        .values()
        .filter(|item| item.item_type == "EPISODE")
        .filter_map(|item| item.series_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let series_by_id = catalog
        .find_items(principal, &series_ids)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    Ok(events
        .iter()
        .filter_map(|event| {
            let item = event
                .target_id
                .as_deref()
                .and_then(|item_id| items_by_id.get(item_id))?;
            if item.item_type != "EPISODE" {
                return None;
            }
            let series = item
                .series_id
                .as_deref()
                .and_then(|series_id| series_by_id.get(series_id))?;
            Some((
                event.id.clone(),
                DashboardActivitySeriesMetadata {
                    title: series.title.clone(),
                    season_number: item.season_number,
                    season_count: series.season_count,
                },
            ))
        })
        .collect())
}

pub(crate) async fn admin_test_emby_migration(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.emby_migration.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.test_connection().await {
        Ok(info) => Json(info).into_response(),
        Err(error) => emby_migration_error(&headers, error),
    }
}

pub(crate) async fn admin_list_emby_migration_source_users(
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
    let Some(service) = state.emby_migration.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service
        .list_source_users_filtered(offset, limit, query.search.as_deref())
        .await
    {
        Ok(page) => Json(json!({
            "users": page.users,
            "total": page.total,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => emby_migration_error(&headers, error),
    }
}

pub(crate) async fn admin_create_emby_migration(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<CreateMigrationRequest>,
) -> Response {
    let actor = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.emby_migration.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.create_job(&actor.id.to_string(), request).await {
        Ok(job) => {
            let job_id = job.id.clone();
            service.clone().spawn(job_id.clone());
            record_audit_event(
                &state,
                &headers,
                "EMBY_MIGRATION_CREATED",
                Some("emby_migration_job"),
                Some(&job_id),
                "{}",
            )
            .await;
            (StatusCode::ACCEPTED, Json(json!({ "job": job }))).into_response()
        }
        Err(error) => emby_migration_error(&headers, error),
    }
}

pub(crate) async fn admin_list_emby_migrations(
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
    let Some(service) = state.emby_migration.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let jobs = match service.list_jobs(offset, limit).await {
        Ok(jobs) => jobs,
        Err(error) => return emby_migration_error(&headers, error),
    };
    let total = match service.count_jobs().await {
        Ok(total) => total,
        Err(error) => return emby_migration_error(&headers, error),
    };
    Json(json!({
        "jobs": jobs,
        "total": total,
        "page": offset / limit + 1,
        "pageSize": limit,
    }))
    .into_response()
}

pub(crate) async fn admin_get_emby_migration(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(service) = state.emby_migration.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.get_job(&job_id).await {
        Ok(job) => Json(json!({ "job": job })).into_response(),
        Err(error) => emby_migration_error(&headers, error),
    }
}

pub(crate) async fn admin_cancel_emby_migration(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.emby_migration.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.cancel_job(&job_id).await {
        Ok(true) => Json(json!({ "cancelRequested": true })).into_response(),
        Ok(false) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "迁移任务不存在或已经结束",
        )
        .into_response(),
        Err(error) => emby_migration_error(&headers, error),
    }
}

pub(crate) async fn admin_retry_emby_migration(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.emby_migration.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.resume_job(&job_id).await {
        Ok(true) => {
            service.clone().spawn(job_id.clone());
            (
                StatusCode::ACCEPTED,
                Json(json!({ "jobId": job_id, "status": "ACCEPTED" })),
            )
                .into_response()
        }
        Ok(false) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "迁移任务不能恢复",
        )
        .into_response(),
        Err(error) => emby_migration_error(&headers, error),
    }
}

pub(crate) fn emby_migration_error(
    headers: &HeaderMap,
    error: EmbyMigrationServiceError,
) -> Response {
    let (status, message) = match error {
        EmbyMigrationServiceError::InvalidInput(_) => {
            (StatusCode::BAD_REQUEST, "Emby 迁移参数无效")
        }
        EmbyMigrationServiceError::NotFound => (StatusCode::NOT_FOUND, "迁移任务不存在"),
        EmbyMigrationServiceError::InvalidState => {
            (StatusCode::CONFLICT, "迁移任务状态不允许此操作")
        }
        EmbyMigrationServiceError::AlreadyActive => {
            (StatusCode::CONFLICT, "已有 Emby 迁移正在执行或等待执行")
        }
        EmbyMigrationServiceError::Plugin(
            crate::application::plugins::PluginServiceError::InvalidConfig,
        ) => (StatusCode::BAD_REQUEST, "Emby 迁移插件配置无效"),
        EmbyMigrationServiceError::Plugin(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "Emby 迁移插件暂时不可用")
        }
        EmbyMigrationServiceError::Storage(_)
        | EmbyMigrationServiceError::User(_)
        | EmbyMigrationServiceError::Io(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "Emby 迁移服务暂时不可用")
        }
    };
    api_error(headers, status, lux::ApiErrorCode::InvalidRequest, message).into_response()
}

pub(crate) async fn admin_list_emby_migration_users(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_emby_migration_report(
        headers,
        job_id,
        query,
        state,
        EmbyMigrationReportKind::Users,
    )
    .await
}

pub(crate) async fn admin_list_emby_migration_matches(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_emby_migration_report(
        headers,
        job_id,
        query,
        state,
        EmbyMigrationReportKind::Matches,
    )
    .await
}

pub(crate) async fn admin_list_emby_migration_imports(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_emby_migration_report(
        headers,
        job_id,
        query,
        state,
        EmbyMigrationReportKind::Imports,
    )
    .await
}

pub(crate) async fn admin_list_emby_migration_person_favorites(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_emby_migration_report(
        headers,
        job_id,
        query,
        state,
        EmbyMigrationReportKind::PersonFavorites,
    )
    .await
}

#[derive(Clone, Copy)]
pub(crate) enum EmbyMigrationReportKind {
    Users,
    Matches,
    Imports,
    PersonFavorites,
}

pub(crate) async fn admin_list_emby_migration_report(
    headers: HeaderMap,
    job_id: String,
    query: AdminJobsQuery,
    state: AppState,
    kind: EmbyMigrationReportKind,
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
    let Some(service) = state.emby_migration.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = match kind {
        EmbyMigrationReportKind::Users => service
            .list_user_links(&job_id, offset, limit)
            .await
            .map(|items| json!({ "users": items })),
        EmbyMigrationReportKind::Matches => service
            .list_item_matches(&job_id, offset, limit)
            .await
            .map(|items| json!({ "matches": items })),
        EmbyMigrationReportKind::Imports => service
            .list_import_records(&job_id, offset, limit)
            .await
            .map(|items| json!({ "imports": items })),
        EmbyMigrationReportKind::PersonFavorites => service
            .list_person_favorite_records(&job_id, offset, limit)
            .await
            .map(|items| json!({ "personFavorites": items })),
    };
    match result {
        Ok(mut response) => {
            response["page"] = json!(offset / limit + 1);
            response["pageSize"] = json!(limit);
            Json(response).into_response()
        }
        Err(error) => emby_migration_error(&headers, error),
    }
}

pub(crate) fn danmaku_service_error(headers: &HeaderMap, error: DanmakuServiceError) -> Response {
    match error {
        DanmakuServiceError::InvalidConcurrency | DanmakuServiceError::ProviderNotConfigured => {
            api_error(
                headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "弹幕匹配配置无效或尚未配置",
            )
            .into_response()
        }
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
        DanmakuServiceError::LibraryNotSelected => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "该媒体库未在弹幕插件配置中启用",
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

pub(crate) fn strm_probe_error(headers: &HeaderMap, error: StrmProbeError) -> Response {
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

pub(crate) fn api_error(
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

pub(crate) fn user_avatar_error(headers: &HeaderMap, error: UserAvatarError) -> Response {
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

pub(crate) fn request_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
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

pub(crate) fn secure_cookie_for_request(headers: &HeaderMap, policy: &RemoteAccessPolicy) -> bool {
    policy.is_secure_request(
        header_str(headers, "x-lux-peer-ip"),
        header_str(headers, "x-forwarded-proto"),
    )
}

pub(crate) fn build_cookie(
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
