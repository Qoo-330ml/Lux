use super::*;

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/libraries",
            get(admin_list_libraries).post(admin_create_library),
        )
        .route("/api/v1/admin/directories", get(admin_list_directories))
        .route(
            "/api/v1/admin/libraries/{library_id}",
            patch(admin_update_library).delete(admin_delete_library),
        )
        .route(
            "/api/v1/admin/library-deletion-jobs",
            get(admin_list_library_deletion_jobs),
        )
        .route(
            "/api/v1/admin/library-deletion-jobs/{job_id}/retry",
            post(admin_retry_library_deletion),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/cover",
            put(admin_update_library_cover)
                .layer(DefaultBodyLimit::max(MAX_LIBRARY_COVER_BYTES as usize)),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/cover/auto",
            post(admin_run_auto_library_cover),
        )
        .route(
            "/api/v1/admin/library-cover-jobs",
            get(admin_list_library_cover_jobs),
        )
        .route(
            "/api/v1/admin/library-cover-jobs/{job_id}",
            get(admin_get_library_cover_job),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/roots",
            post(admin_add_library_root),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/roots/{root_id}",
            delete(admin_delete_library_root),
        )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateLibraryRequest {
    pub(crate) name: String,
    pub(crate) kind: String,
    #[serde(default = "default_realtime_watch_enabled")]
    pub(crate) realtime_watch_enabled: bool,
    #[serde(default = "default_realtime_metadata_auto_match_enabled")]
    pub(crate) realtime_metadata_auto_match_enabled: bool,
    pub(crate) scraper_id: Option<String>,
    pub(crate) scrapers: Option<Vec<LibraryScraperRequest>>,
    pub(crate) chapter_source_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryScraperRequest {
    pub(crate) scraper_id: String,
    pub(crate) role: LibraryScraperRole,
}

pub(crate) fn default_realtime_watch_enabled() -> bool {
    true
}

pub(crate) fn default_realtime_metadata_auto_match_enabled() -> bool {
    true
}

#[derive(Deserialize)]
pub(crate) struct AddLibraryRootRequest {
    pub(crate) path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScanPathRequest {
    pub(crate) root_id: Option<String>,
    pub(crate) path: String,
    #[serde(default = "default_recursive_scan")]
    pub(crate) recursive: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RefreshLocalMetadataRequest {
    pub(crate) root_id: Option<String>,
    pub(crate) paths: Vec<String>,
}

fn default_recursive_scan() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateLibraryRequest {
    pub(crate) name: Option<String>,
    pub(crate) kind: Option<String>,
    pub(crate) is_enabled: Option<bool>,
    pub(crate) realtime_watch_enabled: Option<bool>,
    pub(crate) realtime_metadata_auto_match_enabled: Option<bool>,
    /// Accepted for legacy clients; realtime incremental scanning has no schedule, so this
    /// request field is validated by the deserializer and then ignored.
    #[allow(dead_code)]
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub(crate) incremental_schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub(crate) reconciliation_schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub(crate) metadata_schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub(crate) scraper_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub(crate) scrapers: Option<Option<Vec<LibraryScraperRequest>>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub(crate) chapter_source_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub(crate) media_strategy: Option<Option<MediaStrategySettings>>,
    pub(crate) scan_concurrency: Option<i64>,
    pub(crate) probe_concurrency: Option<i64>,
}

pub(crate) fn deserialize_optional_optional<'de, D, T>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

pub(crate) async fn admin_list_libraries(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(libraries) = state.libraries() else {
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

pub(crate) async fn admin_list_directories(
    headers: HeaderMap,
    Query(query): Query<DirectoryBrowseQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) if params.0 <= 10_000 => params,
        Ok(_) | Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "分页参数无效",
            )
            .into_response();
        }
    };
    let path = query.path.as_deref().unwrap_or("/");
    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(50);
    match list_directories(FsPath::new(path), offset as usize, limit as usize).await {
        Ok(result) => Json(json!({
            "path": result.path,
            "parentPath": result.parent_path,
            "directories": result.directories.iter().map(|entry| json!({
                "name": entry.name,
                "path": entry.path,
            })).collect::<Vec<_>>(),
            "page": page,
            "pageSize": page_size,
            "hasMore": result.has_more,
        }))
        .into_response(),
        Err(DirectoryBrowserError::InvalidPath | DirectoryBrowserError::NotDirectory) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "目录路径无效",
        )
        .into_response(),
        Err(DirectoryBrowserError::Unavailable) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "目录不可访问",
        )
        .into_response(),
    }
}

pub(crate) async fn validate_scraper_selection(
    headers: &HeaderMap,
    state: &AppState,
    scraper_id: Option<&str>,
) -> Result<(), Response> {
    let Some(plugins) = state.plugins() else {
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

pub(crate) fn create_library_scrapers(
    request: &CreateLibraryRequest,
) -> Result<Vec<LibraryScraper>, &'static str> {
    if request.scrapers.is_some() && request.scraper_id.is_some() {
        return Err("scraperId 和 scrapers 不能同时提交");
    }
    if let Some(scrapers) = request.scrapers.as_deref() {
        return Ok(scrapers
            .iter()
            .enumerate()
            .map(|(position, scraper)| LibraryScraper {
                scraper_id: scraper.scraper_id.clone(),
                position: i64::try_from(position).unwrap_or(i64::MAX),
                role: scraper.role,
            })
            .collect());
    }
    Ok(request
        .scraper_id
        .as_deref()
        .map(|scraper_id| LibraryScraper {
            scraper_id: scraper_id.to_owned(),
            position: 0,
            role: LibraryScraperRole::Primary,
        })
        .into_iter()
        .collect())
}

type LibraryScraperUpdate = (Option<Option<String>>, Option<Option<Vec<LibraryScraper>>>);

pub(crate) fn update_library_scrapers(
    request: &UpdateLibraryRequest,
) -> Result<LibraryScraperUpdate, &'static str> {
    if request.scrapers.is_some() && request.scraper_id.is_some() {
        return Err("scraperId 和 scrapers 不能同时提交");
    }
    if let Some(scrapers) = request.scrapers.as_ref() {
        return Ok((
            None,
            Some(scrapers.as_ref().map(|scrapers| {
                scrapers
                    .iter()
                    .enumerate()
                    .map(|(position, scraper)| LibraryScraper {
                        scraper_id: scraper.scraper_id.clone(),
                        position: i64::try_from(position).unwrap_or(i64::MAX),
                        role: scraper.role,
                    })
                    .collect()
            })),
        ));
    }
    Ok((request.scraper_id.clone(), None))
}

pub(crate) async fn validate_scraper_selections(
    headers: &HeaderMap,
    state: &AppState,
    scrapers: &[LibraryScraper],
) -> Result<(), Response> {
    for scraper in scrapers {
        validate_scraper_selection(headers, state, Some(&scraper.scraper_id)).await?;
    }
    Ok(())
}

pub(crate) async fn validate_chapter_source_selection(
    headers: &HeaderMap,
    state: &AppState,
    kind: LibraryKind,
    chapter_source_id: Option<&str>,
) -> Result<(), Response> {
    let Some(chapter_source_id) = chapter_source_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    if !kind.supports_chapter_source() {
        return Err(api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "片头片尾数据源只能用于剧集或混合媒体库",
        )
        .into_response());
    }
    let Some(plugins) = state.plugins() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE.into_response());
    };
    match plugins
        .has_available_chapter_source(chapter_source_id)
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(plugin_error(
            headers,
            PluginServiceError::Unavailable(chapter_source_id.to_owned()),
        )),
        Err(error) => Err(plugin_error(headers, error)),
    }
}

pub(crate) async fn admin_create_library(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<CreateLibraryRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(libraries) = state.libraries() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let scrapers = match create_library_scrapers(&request) {
        Ok(scrapers) => scrapers,
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
    if let Err(response) = validate_scraper_selections(&headers, &state, &scrapers).await {
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
    if let Err(response) = validate_chapter_source_selection(
        &headers,
        &state,
        kind,
        request.chapter_source_id.as_deref(),
    )
    .await
    {
        return response;
    }
    match crate::application::admin_library_settings::create_library(
        libraries,
        state.home(),
        state.plugins(),
        crate::application::admin_library_settings::CreateLibraryInput {
            name: &request.name,
            kind,
            realtime_watch_enabled: request.realtime_watch_enabled,
            scrapers: &scrapers,
            chapter_source_id: request.chapter_source_id.as_deref(),
            realtime_metadata_auto_match_enabled: request.realtime_metadata_auto_match_enabled,
        },
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
        Err(crate::application::admin_library_settings::LibrarySettingsWriteError::Library(
            error,
        )) => library_error(&headers, error),
        Err(
            crate::application::admin_library_settings::LibrarySettingsWriteError::ChapterSchedule(
                error,
            ),
        ) => plugin_error(&headers, error),
    }
}

pub(crate) async fn admin_update_library(
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
    let Some(libraries) = state.libraries() else {
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
    let current_library = match libraries.get_library(library_id).await {
        Ok(library) => library,
        Err(error) => return library_error(&headers, error),
    };
    if let Err(response) = validate_chapter_source_selection(
        &headers,
        &state,
        kind.unwrap_or(current_library.kind),
        request
            .chapter_source_id
            .as_ref()
            .and_then(|value| value.as_deref()),
    )
    .await
    {
        return response;
    }
    let effective_kind = kind.unwrap_or(current_library.kind);
    let chapter_source_id = request.chapter_source_id.clone().or_else(|| {
        (!effective_kind.supports_chapter_source() && current_library.chapter_source_id.is_some())
            .then_some(None)
    });
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
            if let Some(scraper_id) = strategy.scraper_id.as_deref()
                && let Err(response) =
                    validate_scraper_selection(&headers, &state, Some(scraper_id)).await
            {
                return response;
            }
            match serde_json::to_string(strategy) {
                Ok(value) => Some(Some(value)),
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
    };
    let (scraper_id, scrapers) = match update_library_scrapers(&request) {
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
    if let Some(scrapers) = scrapers.as_ref().and_then(|value| value.as_ref())
        && let Err(response) = validate_scraper_selections(&headers, &state, scrapers).await
    {
        return response;
    }
    let settings = LibrarySettingsPatch {
        name: request.name,
        kind,
        is_enabled: request.is_enabled,
        realtime_watch_enabled: request.realtime_watch_enabled,
        realtime_metadata_auto_match_enabled: request.realtime_metadata_auto_match_enabled,
        reconciliation_schedule: request.reconciliation_schedule,
        metadata_schedule: request.metadata_schedule,
        scraper_id,
        scrapers: scrapers.map(|value| value.unwrap_or_default()),
        chapter_source_id,
        media_strategy_json,
        scan_concurrency: request.scan_concurrency,
        probe_concurrency: request.probe_concurrency,
    };
    match crate::application::admin_library_settings::update_library_settings(
        libraries,
        state.home(),
        state.plugins(),
        library_id,
        settings,
    )
    .await
    {
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
        Err(crate::application::admin_library_settings::LibrarySettingsWriteError::Library(
            error,
        )) => library_error(&headers, error),
        Err(
            crate::application::admin_library_settings::LibrarySettingsWriteError::ChapterSchedule(
                error,
            ),
        ) => plugin_error(&headers, error),
    }
}

pub(crate) async fn admin_update_library_cover(
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
    let covers = require_service!(state.library_covers);
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    match covers.store(library_id, content_type, &body).await {
        Ok(cover) => {
            let target_id = library_id.to_string();
            let cover_url =
                library_cover_url_for_tag(&target_id, Some(cover.etag.trim_matches('"')));
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
                        "coverImageUrl": cover_url,
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

pub(crate) async fn admin_list_library_cover_jobs(
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
    let service = require_service!(state.library_covers);
    match service.list(status.as_deref(), offset, limit).await {
        Ok(jobs) => Json(json!({
            "jobs": jobs,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => library_cover_error(&headers, error),
    }
}

pub(crate) async fn admin_get_library_cover_job(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let service = require_service!(state.library_covers);
    match service.get(&job_id).await {
        Ok(job) => Json(json!({ "job": job })).into_response(),
        Err(error) => library_cover_error(&headers, error),
    }
}

pub(crate) async fn admin_run_auto_library_cover(
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
    let library_id_text = library_id.to_string();
    let covers = require_service!(state.library_covers);
    let job = match covers.dispatch_manual_job(library_id).await {
        Ok(job) => job,
        Err(
            crate::application::library_covers::ManualLibraryCoverDispatchError::TaskNotRegistered,
        ) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "自动媒体库封面任务尚未注册",
            )
            .into_response();
        }
        Err(
            crate::application::library_covers::ManualLibraryCoverDispatchError::StorageUnavailable,
        ) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(crate::application::library_covers::ManualLibraryCoverDispatchError::Job(error)) => {
            return library_cover_error(&headers, error);
        }
    };
    record_audit_event(
        &state,
        &headers,
        "LIBRARY_COVER_GENERATION_STARTED",
        Some("library"),
        Some(&library_id_text),
        "{\"mode\":\"manual\"}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "status": "QUEUED",
            "taskType": crate::application::library_covers::AUTO_LIBRARY_COVER_TASK_TYPE,
            "job": job,
        })),
    )
        .into_response()
}

pub(crate) async fn admin_delete_library_root(
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
    let libraries = require_service!(state.libraries);
    match crate::application::admin_library_settings::delete_library_root(
        libraries,
        state.home(),
        library_id,
        root_id,
    )
    .await
    {
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

pub(crate) async fn admin_delete_library(
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
    let database = require_service!(state.database);
    let libraries = require_service!(state.libraries);
    let service = crate::application::library_deletion::LibraryDeletionService::new(
        database.clone(),
        state.scan_jobs().cloned(),
        state.plugins().cloned(),
        state.home().cloned(),
    );
    let job = match service.request(library_id).await {
        Ok(job) => job,
        Err(crate::application::library_deletion::LibraryDeletionError::LibraryNotFound) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体库不存在",
            )
            .into_response();
        }
        Err(crate::application::library_deletion::LibraryDeletionError::RetryRequired) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::VersionConflict,
                "该媒体库的删除任务已停止，请在任务与日志中重试",
            )
            .into_response();
        }
        Err(crate::application::library_deletion::LibraryDeletionError::Storage(error)) => {
            tracing::error!(library_id = %library_id, %error, "failed to enqueue library deletion");
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "媒体库删除任务提交失败",
            )
            .into_response();
        }
        Err(error) => {
            tracing::error!(library_id = %library_id, %error, "failed to enqueue library deletion");
            return api_error(
                &headers,
                StatusCode::INTERNAL_SERVER_ERROR,
                lux::ApiErrorCode::Internal,
                "媒体库删除任务提交失败",
            )
            .into_response();
        }
    };
    let target_id = library_id.to_string();
    libraries.notify_library_changed();
    if let Some(home) = state.home() {
        home.invalidate();
    }
    record_audit_event(
        &state,
        &headers,
        "LIBRARY_DELETION_REQUESTED",
        Some("library"),
        Some(&target_id),
        &format!(r#"{{"jobId":"{}"}}"#, job.id),
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({"job": library_deletion_job_json(&job)})),
    )
        .into_response()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryDeletionJobsQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    status: Option<String>,
}

pub(crate) async fn admin_list_library_deletion_jobs(
    headers: HeaderMap,
    Query(query): Query<LibraryDeletionJobsQuery>,
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
    let status = query.status.map(|value| value.to_ascii_uppercase());
    if status.as_deref().is_some_and(|status| {
        !matches!(
            status,
            "PENDING" | "RUNNING" | "COMPLETED" | "FAILED" | "CANCELLED"
        )
    }) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "删除任务状态筛选无效",
        )
        .into_response();
    }
    let database = require_service!(state.database);
    let service = crate::application::library_deletion::LibraryDeletionService::new(
        database.clone(),
        state.scan_jobs().cloned(),
        state.plugins().cloned(),
        state.home().cloned(),
    );
    match service.list(offset, limit, status.as_deref()).await {
        Ok((jobs, total)) => (
            StatusCode::OK,
            Json(json!({
                "jobs": jobs.iter().map(library_deletion_job_json).collect::<Vec<_>>(),
                "page": query.page.unwrap_or(1),
                "pageSize": limit,
                "total": total,
            })),
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "failed to list library deletion jobs");
            api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "删除任务暂时不可用",
            )
            .into_response()
        }
    }
}

pub(crate) async fn admin_retry_library_deletion(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let job_id = match job_id.parse::<crate::domain::ids::LibraryDeletionJobId>() {
        Ok(job_id) => job_id.to_string(),
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "删除任务 ID 无效",
            )
            .into_response();
        }
    };
    let database = require_service!(state.database);
    let service = crate::application::library_deletion::LibraryDeletionService::new(
        database.clone(),
        state.scan_jobs().cloned(),
        state.plugins().cloned(),
        state.home().cloned(),
    );
    let job = match service.retry(&job_id).await {
        Ok(job) => job,
        Err(crate::application::library_deletion::LibraryDeletionError::JobNotFound) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "删除任务不存在",
            )
            .into_response();
        }
        Err(crate::application::library_deletion::LibraryDeletionError::JobNotRetryable) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::VersionConflict,
                "该删除任务当前不可重试",
            )
            .into_response();
        }
        Err(error) => {
            tracing::error!(job_id, %error, "failed to retry library deletion");
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "删除任务重试失败",
            )
            .into_response();
        }
    };
    record_audit_event(
        &state,
        &headers,
        "LIBRARY_DELETION_RETRIED",
        Some("library_deletion_job"),
        Some(&job.id),
        "{}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({"job": library_deletion_job_json(&job)})),
    )
        .into_response()
}

fn library_deletion_job_json(job: &crate::storage::StoredLibraryDeletionJob) -> Value {
    json!({
        "id": job.id,
        "libraryId": job.library_id,
        "libraryName": job.library_name,
        "status": job.status,
        "stage": job.stage,
        "processedCount": job.processed_count,
        "errorCode": job.error_code,
        "createdAt": job.created_at,
        "updatedAt": job.updated_at,
        "startedAt": job.started_at,
        "finishedAt": job.finished_at,
    })
}

pub(crate) async fn admin_add_library_root(
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
    let Some(libraries) = state.libraries() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match crate::application::admin_library_settings::add_library_root(
        libraries,
        state.home(),
        library_id,
        &request.path,
    )
    .await
    {
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

pub(crate) fn library_cover_error(headers: &HeaderMap, error: LibraryCoverError) -> Response {
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
        LibraryCoverError::TaskNotRegistered | LibraryCoverError::JobNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "自动媒体库封面任务不存在",
        )
        .into_response(),
        LibraryCoverError::AlreadyActive => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "自动媒体库封面任务已有运行中的作业",
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
        | LibraryCoverError::Storage(_)
        | LibraryCoverError::FontNotFound
        | LibraryCoverError::Render(_)
        | LibraryCoverError::RenderPanicked
        | LibraryCoverError::GeneratedCoverRace
        | LibraryCoverError::GenerationUnavailable => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "媒体库封面保存失败",
        )
        .into_response(),
    }
}

pub(crate) fn library_error(headers: &HeaderMap, error: LibraryServiceError) -> Response {
    let (status, code, message) = match error {
        LibraryServiceError::InvalidName
        | LibraryServiceError::InvalidSchedule
        | LibraryServiceError::InvalidConcurrency
        | LibraryServiceError::InvalidLibraryId(_)
        | LibraryServiceError::InvalidRootId(_)
        | LibraryServiceError::InvalidKind(_)
        | LibraryServiceError::InvalidScraperId
        | LibraryServiceError::InvalidScraperRole(_)
        | LibraryServiceError::InvalidScraperOrder(_)
        | LibraryServiceError::InvalidChapterSourceId
        | LibraryServiceError::InvalidLibraryOrder(_) => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体库请求无效",
        ),
        LibraryServiceError::LibraryNotFound => (
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体库不存在",
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

pub(crate) fn library_json(library: &LibraryRecord, roots: &[LibraryRootRecord]) -> Value {
    json!({
        "id": library.id.to_string(),
        "name": library.name,
        "kind": library.kind.as_str(),
        "scraperId": library.scraper_id,
        "scrapers": library.scrapers.iter().map(|scraper| json!({
            "scraperId": scraper.scraper_id,
            "position": scraper.position,
            "role": scraper.role.as_str(),
        })).collect::<Vec<_>>(),
        "chapterSourceId": library.chapter_source_id,
        "coverImageUrl": library_cover_url(library),
        "isEnabled": library.is_enabled,
        "realtimeWatchEnabled": library.realtime_watch_enabled,
        "realtimeMetadataAutoMatchEnabled": library.realtime_metadata_auto_match_enabled,
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

pub(crate) fn library_media_strategy_json(value: Option<&str>) -> Option<Value> {
    let strategy = serde_json::from_str::<MediaStrategySettings>(value?).ok()?;
    serde_json::to_value(strategy).ok()
}

pub(crate) fn library_cover_url(library: &LibraryRecord) -> Option<String> {
    library.cover_image_path.as_ref().map(|_| {
        library_cover_url_for_tag(&library.id.to_string(), library.cover_image_tag.as_deref())
    })
}

pub(crate) fn library_cover_url_for_tag(library_id: &str, tag: Option<&str>) -> String {
    let base = format!("/api/v1/libraries/{library_id}/cover");
    tag.map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map_or(base.clone(), |tag| format!("{base}?v={tag}"))
}

pub(crate) fn root_json(root: &LibraryRootRecord) -> Value {
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
