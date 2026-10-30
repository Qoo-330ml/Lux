use super::*;

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/emby-migration/test",
            post(admin_test_emby_migration),
        )
        .route(
            "/api/v1/admin/emby-migration/source-users",
            get(admin_list_emby_migration_source_users),
        )
        .route(
            "/api/v1/admin/emby-migration",
            get(admin_list_emby_migrations).post(admin_create_emby_migration),
        )
        .route(
            "/api/v1/admin/emby-migration/{job_id}",
            get(admin_get_emby_migration),
        )
        .route(
            "/api/v1/admin/emby-migration/{job_id}/cancel",
            post(admin_cancel_emby_migration),
        )
        .route(
            "/api/v1/admin/emby-migration/{job_id}/retry",
            post(admin_retry_emby_migration),
        )
        .route(
            "/api/v1/admin/emby-migration/{job_id}/users",
            get(admin_list_emby_migration_users),
        )
        .route(
            "/api/v1/admin/emby-migration/{job_id}/matches",
            get(admin_list_emby_migration_matches),
        )
        .route(
            "/api/v1/admin/emby-migration/{job_id}/imports",
            get(admin_list_emby_migration_imports),
        )
        .route(
            "/api/v1/admin/emby-migration/{job_id}/person-favorites",
            get(admin_list_emby_migration_person_favorites),
        )
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
    let service = require_service!(state.emby_migration);
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
            crate::application::admin_job_workers::dispatch_emby_migration_job(
                service.clone(),
                job_id.clone(),
            );
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
    let service = require_service!(state.emby_migration);
    let page = match service.list_job_page(offset, limit).await {
        Ok(page) => page,
        Err(error) => return emby_migration_error(&headers, error),
    };
    Json(json!({
        "jobs": page.jobs,
        "total": page.total,
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
    let service = require_service!(state.emby_migration);
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
    let service = require_service!(state.emby_migration);
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
            crate::application::admin_job_workers::dispatch_emby_migration_job(
                service.clone(),
                job_id.clone(),
            );
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
    let service = require_service!(state.emby_migration);
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
