use super::*;

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/items/{item_id}/scan",
            post(admin_start_item_scan),
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
            "/api/v1/admin/items/{item_id}/merges",
            get(admin_list_item_merge_records),
        )
        .route("/api/v1/admin/items/unmerge", post(admin_unmerge_items))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateExternalSubtitleRequest {
    pub(crate) source_id: String,
    pub(crate) title: Option<String>,
    pub(crate) language: Option<String>,
    pub(crate) is_default: bool,
    pub(crate) is_forced: bool,
}

pub(crate) async fn admin_start_item_scan(
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
    let scan_jobs = require_service!(state.scan_jobs);
    let job = match crate::application::admin_scan_jobs::start_item_folder_scan_job(
        scan_jobs,
        &item_id,
        crate::application::admin_scan_jobs::ScanWorkerServices {
            probe: state.probe().cloned(),
            metadata: state.metadata_reidentify().cloned(),
            thumbnails: state.thumbnails().cloned(),
        },
    )
    .await
    {
        Ok(job) => job,
        Err(ScanJobError::ItemNotFound) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体条目不存在",
            )
            .into_response();
        }
        Err(ScanJobError::LibraryNotFound) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体条目不存在",
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

pub(crate) async fn admin_update_item_subtitle(
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
    let database = require_service!(state.database);
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

pub(crate) async fn admin_delete_item(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<LuxStreamQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    delete_media_item(&headers, &state, &item_id, query.source_id.as_deref()).await
}

pub(crate) async fn delete_media_item(
    headers: &HeaderMap,
    state: &AppState,
    item_id: &str,
    source_id: Option<&str>,
) -> Response {
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let deletion = require_service!(state.deletion);
    let report = match crate::application::admin_media_mutations::delete_media_item(
        deletion,
        state.home(),
        item_id,
        source_id,
    )
    .await
    {
        Ok(report) => report,
        Err(MediaDeleteError::ItemNotFound) => {
            return api_error(
                headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体文件不存在",
            )
            .into_response();
        }
        Err(MediaDeleteError::PathOutsideRoot(_)) => {
            return api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::PermissionDenied,
                "媒体路径不在媒体库根目录内",
            )
            .into_response();
        }
        Err(MediaDeleteError::InvalidFileName(_)) => {
            return api_error(
                headers,
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
    let audit_target = if report.source_ids.len() == 1 {
        ("media_source", report.source_ids[0].as_str())
    } else {
        ("media_item", report.item_id.as_str())
    };
    record_audit_event(
        state,
        headers,
        "MEDIA_DELETED",
        Some(audit_target.0),
        Some(audit_target.1),
        &format!(
            r#"{{"itemId":"{}","fileCount":{},"sourceCount":{}}}"#,
            report.item_id,
            report.deleted_file_count,
            report.source_ids.len()
        ),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

pub(crate) async fn admin_refresh_collection(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(collections) = state.collections() else {
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
            CollectionError::Scraper(_)
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

#[derive(Deserialize, Default)]
pub(crate) struct ItemMergeRecordsQuery {
    pub(crate) offset: Option<i64>,
    pub(crate) limit: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UnmergeItemsRequest {
    pub(crate) primary_item_id: String,
    pub(crate) merged_item_id: String,
}

pub(crate) async fn admin_list_item_merge_records(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<ItemMergeRecordsQuery>,
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
    let merge = require_service!(state.item_merge);
    let offset = query.offset.unwrap_or(0).max(0);
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    match crate::application::admin_media_mutations::list_media_merge_records(
        merge, &item_id, offset, limit,
    )
    .await
    {
        Ok(records) => Json(json!({
            "offset": offset,
            "limit": limit,
            "records": records
                .into_iter()
                .map(|record| json!({
                    "id": record.id,
                    "mergedItemId": record.merged_item_id,
                    "mergedItemTitle": record.merged_item_title,
                    "itemType": record.item_type,
                    "origin": record.origin,
                    "mergedAt": record.merged_at,
                    "undoneAt": record.undone_at,
                    "sourceCount": record.source_count,
                }))
                .collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_unmerge_items(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<UnmergeItemsRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if request
        .primary_item_id
        .parse::<crate::domain::ids::ItemId>()
        .is_err()
        || request
            .merged_item_id
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
    let merge = require_service!(state.item_merge);
    let result = match crate::application::admin_media_mutations::unmerge_media_items(
        merge,
        state.home(),
        state.catalog(),
        &request.primary_item_id,
        &request.merged_item_id,
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
        "MEDIA_ITEM_UNMERGED",
        Some("media_items"),
        Some(&result.primary_item_id),
        &json!({
            "libraryId": result.library_id,
            "itemType": result.item_type,
            "mergedItemId": result.merged_item_id,
            "restoredItemCount": result.restored_item_ids.len(),
            "restoredSourceCount": result.restored_source_count,
        })
        .to_string(),
    )
    .await;
    Json(json!({
        "primaryItemId": result.primary_item_id,
        "mergedItemId": result.merged_item_id,
        "libraryId": result.library_id,
        "itemType": result.item_type,
        "restoredItemIds": result.restored_item_ids,
        "restoredSourceCount": result.restored_source_count,
        "userStateRestored": false,
    }))
    .into_response()
}
