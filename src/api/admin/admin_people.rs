use super::*;

pub(super) fn api_routes() -> Router<ReadyAppState> {
    Router::new()
        .route(
            "/api/v1/admin/people/index-rebuild",
            get(admin_list_people_index_rebuild),
        )
        .route(
            "/api/v1/admin/people/index-rebuild/{library_id}",
            post(admin_queue_people_index_rebuild),
        )
        .route(
            "/api/v1/admin/people/index-rebuild/{library_id}/cancel",
            post(admin_cancel_people_index_rebuild),
        )
        .route(
            "/api/v1/admin/people/matches",
            get(admin_list_pending_person_matches),
        )
        .route(
            "/api/v1/admin/people/matches/{candidate_id}/confirm",
            post(admin_confirm_person_match),
        )
        .route(
            "/api/v1/admin/people/matches/{candidate_id}/reject",
            post(admin_reject_person_match),
        )
        .route(
            "/api/v1/admin/people/matches/{candidate_id}/undo",
            post(admin_undo_person_match),
        )
        .route(
            "/api/v1/admin/people/{person_id}/split",
            post(admin_split_person_identity),
        )
        .route(
            "/api/v1/admin/people/{person_id}/locks",
            post(admin_set_person_field_locks),
        )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersonMatchConfirmRequest {
    pub(crate) target_person_id: String,
    #[serde(default)]
    pub(crate) evidence: Value,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersonMatchRejectRequest {
    #[serde(default)]
    pub(crate) evidence: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersonIdentitySplitRequest {
    pub(crate) provider: String,
    pub(crate) provider_id: String,
    pub(crate) display_name: String,
    #[serde(default)]
    pub(crate) evidence: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersonFieldLocksRequest {
    #[serde(default)]
    pub(crate) fields: Vec<String>,
    #[serde(default)]
    pub(crate) evidence: Value,
}

pub(crate) async fn admin_list_people_index_rebuild(
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
    let Some(people) = state.people() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "人物索引服务尚未就绪",
        )
        .into_response();
    };
    match crate::application::admin_people::list_person_index_rebuild_jobs(people, offset, limit)
        .await
    {
        Ok(page) => Json(json!({
            "jobs": page.jobs.iter().map(people_index_rebuild_job_json).collect::<Vec<_>>(),
            "total": page.total,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(_) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "人物索引任务暂时不可用",
        )
        .into_response(),
    }
}

pub(crate) async fn admin_queue_people_index_rebuild(
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
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体库不存在",
        )
        .into_response();
    };
    let library_id_string = library_id.to_string();
    let Some(people) = state.people() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "人物索引服务尚未就绪",
        )
        .into_response();
    };
    let job = match crate::application::admin_people::queue_person_index_rebuild(people, library_id)
        .await
    {
        Ok(job) => job,
        Err(crate::application::admin_people::PersonIndexRebuildRequestError::AlreadyRunning) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::InvalidRequest,
                "人物索引重建正在运行",
            )
            .into_response();
        }
        Err(crate::application::admin_people::PersonIndexRebuildRequestError::LibraryNotFound) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体库不存在或未启用",
            )
            .into_response();
        }
        Err(crate::application::admin_people::PersonIndexRebuildRequestError::Unavailable) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "人物索引任务暂时不可用",
            )
            .into_response();
        }
    };
    record_audit_event(
        &state,
        &headers,
        "PEOPLE_INDEX_REBUILD_QUEUED",
        Some("library"),
        Some(&library_id_string),
        "{}",
    )
    .await;
    crate::application::admin_people::start_person_index_rebuild(people);
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job": people_index_rebuild_job_json(&job) })),
    )
        .into_response()
}

pub(crate) async fn admin_cancel_people_index_rebuild(
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
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体库不存在",
        )
        .into_response();
    };
    let library_id_string = library_id.to_string();
    let Some(people) = state.people() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "人物索引服务尚未就绪",
        )
        .into_response();
    };
    let job =
        match crate::application::admin_people::cancel_person_index_rebuild(people, library_id)
            .await
        {
            Ok(job) => job,
            Err(
                crate::application::admin_people::PersonIndexRebuildCancelError::NotCancellable,
            ) => {
                return api_error(
                    &headers,
                    StatusCode::CONFLICT,
                    lux::ApiErrorCode::InvalidRequest,
                    "没有可取消的人物索引重建任务",
                )
                .into_response();
            }
            Err(crate::application::admin_people::PersonIndexRebuildCancelError::Unavailable) => {
                return api_error(
                    &headers,
                    StatusCode::SERVICE_UNAVAILABLE,
                    lux::ApiErrorCode::DatabaseUnavailable,
                    "人物索引任务暂时不可用",
                )
                .into_response();
            }
        };
    record_audit_event(
        &state,
        &headers,
        "PEOPLE_INDEX_REBUILD_CANCEL_REQUESTED",
        Some("library"),
        Some(&library_id_string),
        &json!({"status": job.status}).to_string(),
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job": people_index_rebuild_job_json(&job) })),
    )
        .into_response()
}

pub(crate) fn people_index_rebuild_job_json(
    job: &crate::storage::StoredPersonIndexRebuildJob,
) -> Value {
    json!({
        "libraryId": job.library_id,
        "status": job.status,
        "cursorId": job.cursor_id,
        "processedCount": job.processed_count,
        "totalCount": job.total_count,
        "cancelRequested": job.cancel_requested,
    })
}

pub(crate) async fn admin_list_pending_person_matches(
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
    let Some(people) = state.people() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match people
        .list_pending_person_match_candidates(offset, limit)
        .await
    {
        Ok((items, total)) => Json(json!({
            "items": items,
            "total": total,
            "offset": offset,
            "limit": limit,
        }))
        .into_response(),
        Err(error) => people_match_error(&headers, error),
    }
}

pub(crate) async fn admin_confirm_person_match(
    headers: HeaderMap,
    Path(candidate_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PersonMatchConfirmRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(people) = state.people() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let evidence_json = match serde_json::to_string(&request.evidence) {
        Ok(value) if value.len() <= 16 * 1024 => value,
        Ok(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "匹配证据过大",
            )
            .into_response();
        }
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "匹配证据无效",
            )
            .into_response();
        }
    };
    match people
        .confirm_person_match_candidate(&candidate_id, &request.target_person_id, &evidence_json)
        .await
    {
        Ok(move_result) => {
            record_audit_event(
                &state,
                &headers,
                "PERSON_MATCH_CONFIRMED",
                Some("person_match_candidate"),
                Some(&candidate_id),
                &json!({
                    "targetPersonId": request.target_person_id,
                    "previousPersonId": move_result.previous_person_id,
                    "evidence": request.evidence,
                })
                .to_string(),
            )
            .await;
            Json(json!({
                "candidateId": candidate_id,
                "status": "CONFIRMED",
                "targetPersonId": request.target_person_id,
                "previousPersonId": move_result.previous_person_id,
            }))
            .into_response()
        }
        Err(error) => people_match_error(&headers, error),
    }
}

pub(crate) async fn admin_reject_person_match(
    headers: HeaderMap,
    Path(candidate_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PersonMatchRejectRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(people) = state.people() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let evidence_json = match serde_json::to_string(&request.evidence) {
        Ok(value) if value.len() <= 16 * 1024 => value,
        Ok(_) | Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "匹配证据无效或过大",
            )
            .into_response();
        }
    };
    match people
        .reject_person_match_candidate(&candidate_id, &evidence_json)
        .await
    {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "PERSON_MATCH_REJECTED",
                Some("person_match_candidate"),
                Some(&candidate_id),
                &evidence_json,
            )
            .await;
            Json(json!({
                "candidateId": candidate_id,
                "status": "REJECTED",
            }))
            .into_response()
        }
        Err(error) => people_match_error(&headers, error),
    }
}

pub(crate) async fn admin_undo_person_match(
    headers: HeaderMap,
    Path(candidate_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PersonMatchRejectRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(people) = state.people() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let evidence_json = match serde_json::to_string(&request.evidence) {
        Ok(value) if value.len() <= 16 * 1024 => value,
        Ok(_) | Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "撤销证据无效或过大",
            )
            .into_response();
        }
    };
    match people
        .undo_person_match_candidate(&candidate_id, &evidence_json)
        .await
    {
        Ok(move_result) => {
            record_audit_event(
                &state,
                &headers,
                "PERSON_MATCH_UNDONE",
                Some("person_match_candidate"),
                Some(&candidate_id),
                &evidence_json,
            )
            .await;
            Json(json!({
                "candidateId": candidate_id,
                "status": "UNDONE",
                "previousPersonId": move_result.previous_person_id,
            }))
            .into_response()
        }
        Err(error) => people_match_error(&headers, error),
    }
}

pub(crate) async fn admin_split_person_identity(
    headers: HeaderMap,
    Path(person_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PersonIdentitySplitRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(people) = state.people() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let evidence_json = match serde_json::to_string(&request.evidence) {
        Ok(value) if value.len() <= 16 * 1024 => value,
        Ok(_) | Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "拆分证据无效或过大",
            )
            .into_response();
        }
    };
    match people
        .split_person_identity(
            &person_id,
            &request.provider,
            &request.provider_id,
            &request.display_name,
            &evidence_json,
        )
        .await
    {
        Ok(new_person_id) => {
            record_audit_event(
                &state,
                &headers,
                "PERSON_IDENTITY_SPLIT",
                Some("person"),
                Some(&person_id),
                &json!({
                    "provider": request.provider,
                    "providerId": request.provider_id,
                    "newPersonId": new_person_id,
                    "displayName": request.display_name,
                    "evidence": request.evidence,
                })
                .to_string(),
            )
            .await;
            Json(json!({
                "sourcePersonId": person_id,
                "newPersonId": new_person_id,
                "status": "SPLIT",
            }))
            .into_response()
        }
        Err(error) => people_match_error(&headers, error),
    }
}

pub(crate) async fn admin_set_person_field_locks(
    headers: HeaderMap,
    Path(person_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PersonFieldLocksRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(people) = state.people() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let evidence_json = match serde_json::to_string(&request.evidence) {
        Ok(value) if value.len() <= 16 * 1024 => value,
        Ok(_) | Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "人物字段锁定证据无效或过大",
            )
            .into_response();
        }
    };
    match people
        .set_person_field_locks(&person_id, &request.fields, &evidence_json)
        .await
    {
        Ok(fields) => {
            record_audit_event(
                &state,
                &headers,
                "PERSON_FIELD_LOCKS_UPDATED",
                Some("person"),
                Some(&person_id),
                &json!({"fields": fields, "evidence": request.evidence}).to_string(),
            )
            .await;
            Json(json!({
                "personId": person_id,
                "lockedFields": fields,
                "status": "UPDATED",
            }))
            .into_response()
        }
        Err(error) => people_match_error(&headers, error),
    }
}

pub(crate) fn people_match_error(headers: &HeaderMap, error: PeopleError) -> Response {
    match error {
        PeopleError::InvalidComponent(_) | PeopleError::Serialization(_) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "人物匹配请求无效",
        )
        .into_response(),
        PeopleError::Storage(message) if message.contains("candidate") => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "人物匹配候选状态已变化或不存在",
        )
        .into_response(),
        PeopleError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "人物索引暂时不可用",
        )
        .into_response(),
        _ => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "人物匹配处理失败",
        )
        .into_response(),
    }
}
