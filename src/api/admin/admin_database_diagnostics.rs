use super::*;

pub(super) fn api_routes() -> Router<ReadyAppState> {
    Router::new()
        .route(
            "/api/v1/admin/database-diagnostics",
            get(admin_database_diagnostics_status),
        )
        .route(
            "/api/v1/admin/database-diagnostics/run",
            post(admin_start_database_diagnostics),
        )
        .route(
            "/api/v1/admin/database-diagnostics/export",
            get(admin_export_database_diagnostics),
        )
}

pub(crate) async fn admin_database_diagnostics_status(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let diagnostics = require_service!(state.database_diagnostics);
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
    let diagnostics = require_service!(state.database_diagnostics);
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
    let diagnostics = require_service!(state.database_diagnostics);
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
