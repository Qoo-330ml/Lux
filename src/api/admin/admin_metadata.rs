use super::*;

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
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
        .route("/api/v1/admin/items/merge", post(admin_merge_items))
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
            "/api/v1/admin/libraries/{library_id}/refresh-local-metadata",
            post(admin_refresh_library_local_metadata),
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
            "/api/v1/admin/items/{item_id}/metadata/refresh",
            post(admin_start_item_metadata_refresh),
        )
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetadataCandidateQuery {
    pub(crate) page: Option<i64>,
    pub(crate) page_size: Option<i64>,
    #[serde(alias = "q")]
    pub(crate) search: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetadataCandidateSearchRequest {
    pub(crate) query: String,
    pub(crate) year: Option<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ItemImageSearchRequest {
    pub(crate) image_type: String,
    pub(crate) language: Option<String>,
    pub(crate) source: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ItemImageSelectRequest {
    pub(crate) image_type: String,
    pub(crate) url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetadataReidentifyRequest {
    #[serde(default)]
    pub(crate) item_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetadataBatchConfirmationRequest {
    pub(crate) item_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ItemMergeRequest {
    pub(crate) item_ids: Vec<String>,
    pub(crate) primary_item_id: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetadataReidentifyListQuery {
    pub(crate) page: Option<i64>,
    pub(crate) page_size: Option<i64>,
    pub(crate) status: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum MetadataRefreshRequestMode {
    FillMissing,
    FullRefresh,
}

#[derive(Deserialize)]
pub(crate) struct MetadataRefreshRequest {
    pub(crate) mode: MetadataRefreshRequestMode,
}

impl MetadataRefreshRequestMode {
    pub(super) const fn application_mode(
        &self,
    ) -> crate::application::reidentify::MetadataRefreshMode {
        match self {
            Self::FillMissing => crate::application::reidentify::MetadataRefreshMode::FillMissing,
            Self::FullRefresh => crate::application::reidentify::MetadataRefreshMode::FullRefresh,
        }
    }

    pub(super) const fn as_str(&self) -> &'static str {
        match self {
            Self::FillMissing => "FILL_MISSING",
            Self::FullRefresh => "FULL_REFRESH",
        }
    }
}

fn metadata_dispatch_unavailable(
    headers: &HeaderMap,
    job_id: &str,
    error: crate::application::reidentify::MetadataDispatchError,
) -> Response {
    tracing::warn!(job_id, %error, "administrator metadata job could not be dispatched");
    api_error(
        headers,
        StatusCode::SERVICE_UNAVAILABLE,
        lux::ApiErrorCode::DatabaseUnavailable,
        "元数据任务队列暂不可用，请稍后重试",
    )
    .into_response()
}

pub(crate) async fn admin_refresh_library_local_metadata(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<RefreshLocalMetadataRequest>,
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
    let accepted = match scan_jobs
        .start_local_metadata_refresh(library_id, request.root_id.as_deref(), &request.paths)
        .await
    {
        Ok(accepted) => accepted,
        Err(ScanJobError::LibraryNotFound) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体库不存在",
            )
            .into_response();
        }
        Err(
            ScanJobError::NoChanges
            | ScanJobError::Scanner(
                ScannerError::InvalidRootId(_) | ScannerError::InvalidRelativePath(_),
            ),
        ) => {
            return api_error(
                &headers,
                StatusCode::UNPROCESSABLE_ENTITY,
                lux::ApiErrorCode::InvalidRequest,
                "目录列表为空，或根目录 / 相对路径无效",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    record_audit_event(
        &state,
        &headers,
        "LOCAL_METADATA_REFRESH_STARTED",
        Some("library"),
        Some(&library_id.to_string()),
        "{}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "scope": "LOCAL_METADATA",
            "directories": accepted.directories,
            "entries": accepted.entries,
        })),
    )
        .into_response()
}

pub(crate) async fn admin_start_library_reidentify(
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
    if let Err(error) = reidentify.dispatch_job(&job).await {
        return metadata_dispatch_unavailable(&headers, &job.id, error);
    }
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

pub(crate) async fn admin_start_library_metadata_refresh(
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
    if let Err(error) = reidentify.dispatch_job(&job).await {
        return metadata_dispatch_unavailable(&headers, &job.id, error);
    }
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

pub(crate) async fn admin_list_pending_metadata(
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

pub(crate) async fn admin_list_metadata_reidentify(
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
            "QUEUED"
                | "RUNNING"
                | "COMPLETED"
                | "COMPLETED_WITH_ISSUES"
                | "DEFERRED"
                | "CANCELLED"
                | "FAILED"
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

pub(crate) async fn admin_start_metadata_reidentify(
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
    if let Err(error) = reidentify.dispatch_job(&job).await {
        return metadata_dispatch_unavailable(&headers, &job.id, error);
    }
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

pub(crate) async fn admin_confirm_metadata(
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

pub(crate) async fn admin_merge_items(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<ItemMergeRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if !(2..=100).contains(&request.item_ids.len()) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "合并条目数量必须在 2 到 100 之间",
        )
        .into_response();
    }
    if request
        .item_ids
        .iter()
        .any(|item_id| item_id.parse::<crate::domain::ids::ItemId>().is_err())
        || request
            .primary_item_id
            .parse::<crate::domain::ids::ItemId>()
            .is_err()
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    if request.item_ids.iter().collect::<HashSet<_>>().len() != request.item_ids.len() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "待合并条目不能重复",
        )
        .into_response();
    }
    let Some(service) = state.item_merge.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = match crate::application::admin_media_mutations::merge_media_items(
        service,
        state.home.as_ref(),
        state.catalog.as_ref(),
        &request.primary_item_id,
        &request.item_ids,
    )
    .await
    {
        Ok(result) => result,
        Err(StorageError::Conflict(message)) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::InvalidRequest,
                &message,
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    record_audit_event(
        &state,
        &headers,
        "MEDIA_ITEMS_MERGED",
        Some("media_items"),
        Some(&result.primary_item_id),
        &json!({
            "libraryId": result.library_id,
            "itemType": result.item_type,
            "mergedItemIds": result.merged_item_ids,
        })
        .to_string(),
    )
    .await;
    Json(json!({
        "primaryItemId": result.primary_item_id,
        "mergedItemIds": result.merged_item_ids,
    }))
    .into_response()
}

pub(crate) async fn admin_get_metadata_reidentify(
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

pub(crate) async fn admin_retry_metadata_reidentify(
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
    if let Err(error) = reidentify.dispatch_job(&job).await {
        return metadata_dispatch_unavailable(&headers, &job.id, error);
    }
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

pub(crate) async fn admin_cancel_metadata_reidentify(
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

pub(crate) async fn admin_list_item_candidates(
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

pub(crate) async fn admin_search_item_candidates(
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
    let Some(metadata_reidentify) = state.metadata_reidentify.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据候选服务尚未就绪",
        )
        .into_response();
    };
    match metadata_reidentify
        .search_item_candidates(&item_id, &request.query, request.year)
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

pub(crate) async fn admin_list_item_images(
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
    match crate::application::admin_media_mutations::list_admin_item_images(
        state.database.as_ref(),
        state.image_writes.as_ref(),
        &item_id,
    )
    .await
    {
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
        Err(crate::application::admin_media_mutations::AdminItemImageListError::Unavailable) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        Err(crate::application::admin_media_mutations::AdminItemImageListError::ItemNotFound) => {
            api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体条目不存在",
            )
            .into_response()
        }
        Err(crate::application::admin_media_mutations::AdminItemImageListError::Image(error)) => {
            image_write_error(&headers, error)
        }
    }
}

pub(crate) async fn admin_delete_item_image(
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
pub(crate) struct MetadataSelectionRequest {
    pub(crate) mode: MetadataSelectionMode,
}

pub(crate) async fn admin_select_candidate(
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
            if let Some(reidentify) = state.metadata_reidentify.as_ref()
                && let Err(error) = reidentify
                    .enqueue_selected_actor_enrichment(&report.item_id, &report.candidate_id)
                    .await
            {
                tracing::warn!(
                    item_id = %report.item_id,
                    candidate_id = %report.candidate_id,
                    %error,
                    "selected actor metadata could not be queued"
                );
            }
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

pub(crate) fn metadata_selection_error(
    headers: &HeaderMap,
    error: MetadataSelectionError,
) -> Response {
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

pub(crate) fn image_write_error(headers: &HeaderMap, error: ImageWriteError) -> Response {
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
        ImageWriteError::AttemptInProgress => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::Internal,
            "该图片正在下载，请稍后重试",
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

pub(crate) fn item_image_json(item_id: &str, image: &crate::storage::StoredItemImage) -> Value {
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

pub(crate) fn image_candidate_json(image: &crate::application::images::ImageCandidate) -> Value {
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

pub(crate) fn encode_path_segment(value: &str) -> String {
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

pub(crate) fn image_candidate_error(headers: &HeaderMap, error: ImageCandidateError) -> Response {
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

pub(crate) fn metadata_write_error(headers: &HeaderMap, error: NfoWriteError) -> Response {
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

pub(crate) fn metadata_candidate_page_json(page: &MetadataCandidatePage) -> Value {
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

pub(crate) fn metadata_reidentify_job_json(
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
        "jobScope": job.job_scope,
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

pub(crate) fn metadata_reidentify_job_summary_json(
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
        "jobScope": job.job_scope,
        "pendingCount": job.pending_count,
    })
}

pub(crate) fn metadata_reidentify_error(
    headers: &HeaderMap,
    error: MetadataReidentifyError,
) -> Response {
    match error {
        MetadataReidentifyError::InvalidItemCount
        | MetadataReidentifyError::InvalidRefreshMode
        | MetadataReidentifyError::InvalidSearch
        | MetadataReidentifyError::Candidate(MetadataCandidateError::InvalidSearch) => api_error(
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
        MetadataReidentifyError::LibraryJobAlreadyActive(_) => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "已有整库元数据任务正在运行，请等待完成或先取消该任务",
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
        MetadataReidentifyError::Candidate(MetadataCandidateError::Scraper(_))
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
pub(crate) enum MetadataCandidateFailureKind {
    ItemNotFound,
    InvalidSearch,
    InvalidCandidateJson,
    ScraperUnavailable,
    StorageUnavailable,
}

impl MetadataCandidateFailureKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ItemNotFound => "ITEM_NOT_FOUND",
            Self::InvalidSearch => "INVALID_SEARCH",
            Self::InvalidCandidateJson => "INVALID_CANDIDATE_JSON",
            Self::ScraperUnavailable => "SCRAPER_UNAVAILABLE",
            Self::StorageUnavailable => "STORAGE_UNAVAILABLE",
        }
    }
}

pub(crate) fn metadata_candidate_failure_kind(
    error: &MetadataCandidateError,
) -> MetadataCandidateFailureKind {
    match error {
        MetadataCandidateError::ItemNotFound => MetadataCandidateFailureKind::ItemNotFound,
        MetadataCandidateError::InvalidSearch => MetadataCandidateFailureKind::InvalidSearch,
        MetadataCandidateError::InvalidCandidateJson(_) => {
            MetadataCandidateFailureKind::InvalidCandidateJson
        }
        MetadataCandidateError::Scraper(_) => MetadataCandidateFailureKind::ScraperUnavailable,
        MetadataCandidateError::Storage(_) => MetadataCandidateFailureKind::StorageUnavailable,
    }
}

pub(crate) fn metadata_candidate_error(
    headers: &HeaderMap,
    error: MetadataCandidateError,
) -> Response {
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
        MetadataCandidateError::Scraper(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::PluginUnavailable,
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

pub(crate) async fn admin_start_item_metadata_refresh(
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
    if let Err(error) = reidentify.dispatch_job(&job).await {
        return metadata_dispatch_unavailable(&headers, &job.id, error);
    }
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
