use super::*;

fn scan_worker_services(
    state: &AppState,
) -> crate::application::admin_scan_jobs::ScanWorkerServices {
    crate::application::admin_scan_jobs::ScanWorkerServices {
        probe: state.probe.clone(),
        metadata: state.metadata_reidentify.clone(),
        thumbnails: state.thumbnails.clone(),
    }
}

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/libraries/{library_id}/scan",
            post(admin_start_scan),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/scan-path",
            post(admin_start_library_path_scan),
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
}

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
    let job = match crate::application::admin_scan_jobs::start_movie_scan_job(
        scan_jobs,
        library_id,
        scan_worker_services(&state),
    )
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
    let job = match crate::application::admin_scan_jobs::start_path_scan_job(
        scan_jobs,
        library_id,
        request.root_id.as_deref(),
        &request.path,
        scan_worker_services(&state),
    )
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
    crate::application::admin_scan_jobs::start_library_scan(
        scan_jobs,
        library_id,
        scan_worker_services(state),
    )
    .await
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
    let jobs =
        match crate::application::admin_job_workers::start_admin_strm_probe_jobs(service).await {
            Ok(jobs) => jobs,
            Err(error) => return strm_probe_error(&headers, error),
        };
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
    let job =
        match crate::application::admin_job_workers::retry_strm_probe_job(service, &job_id).await {
            Ok(job) => job,
            Err(error) => return strm_probe_error(&headers, error),
        };
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
    let overrides = crate::application::admin_job_workers::ChapterDetectionOverrides {
        concurrency: request.concurrency,
        intro_window_seconds: request.intro_window_seconds,
        credits_window_seconds: request.credits_window_seconds,
        match_threshold: request.match_threshold,
        force_refresh: request.force_refresh,
    };
    let job = match crate::application::admin_job_workers::start_admin_chapter_detection_job(
        state.chapter_detection.as_ref(),
        state.plugins.as_ref(),
        library_id,
        request.plugin_id.as_deref(),
        overrides,
    )
    .await
    {
        Ok(job) => job,
        Err(
            crate::application::admin_job_workers::AdminChapterDetectionError::ServiceUnavailable,
        ) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(crate::application::admin_job_workers::AdminChapterDetectionError::Detection(
            error,
        )) => {
            return chapter_detection_error(&headers, error);
        }
    };
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
    let job =
        match crate::application::admin_job_workers::retry_chapter_detection_job(service, &job_id)
            .await
        {
            Ok(job) => job,
            Err(error) => return chapter_detection_error(&headers, error),
        };
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
    let job = match crate::application::admin_job_workers::start_danmaku_match_job(
        service,
        library_id,
        request.concurrency,
        request.overwrite,
    )
    .await
    {
        Ok(job) => job,
        Err(error) => return danmaku_service_error(&headers, error),
    };
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
    let job = match crate::application::admin_job_workers::retry_danmaku_match_job(service, &job_id)
        .await
    {
        Ok(job) => job,
        Err(error) => return danmaku_service_error(&headers, error),
    };
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
    let file_events = match crate::application::admin_scan_jobs::list_admin_scan_job_events(
        state.database.as_ref(),
        state.config_dir.as_deref(),
        &job_id,
        level.as_deref(),
        event_code.as_deref(),
        offset,
        limit,
    )
    .await
    {
        Ok(events) => events,
        Err(crate::application::admin_scan_jobs::AdminScanJobEventsError::NotFound) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "任务不存在",
            )
            .into_response();
        }
        Err(crate::application::admin_scan_jobs::AdminScanJobEventsError::Unavailable) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
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
    let job = match crate::application::admin_scan_jobs::retry_scan_job(
        scan_jobs,
        &job_id,
        scan_worker_services(&state),
    )
    .await
    {
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
