use super::*;

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/scheduled-tasks",
            get(admin_list_scheduled_tasks).put(admin_upsert_scheduled_task),
        )
        .route(
            "/api/v1/admin/scheduled-tasks/run",
            post(admin_run_scheduled_task),
        )
        .route(
            "/api/v1/admin/scheduled-task-plans",
            get(admin_list_scheduled_task_plans).post(admin_create_scheduled_task_plan),
        )
        .route(
            "/api/v1/admin/scheduled-task-plans/{plan_id}",
            patch(admin_update_scheduled_task_plan).delete(admin_delete_scheduled_task_plan),
        )
        .route(
            "/api/v1/admin/scheduled-task-plans/{plan_id}/run",
            post(admin_run_scheduled_task_plan),
        )
        .route("/api/v1/admin/task-activity", get(admin_list_task_activity))
        .route(
            "/api/v1/admin/library-pipeline-activity",
            get(admin_list_library_pipeline_activity),
        )
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminScheduledTasksQuery {
    pub(crate) page: Option<i64>,
    pub(crate) page_size: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminLibraryPipelineActivityQuery {
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
    let scheduled_tasks = require_service!(state.scheduled_tasks);
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
    let database = require_service!(state.database);
    let activities =
        match crate::application::admin_task_activity::list_admin_task_activity(database).await {
            Ok(activities) => activities,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    Json(json!({
        "activities": activities.iter().map(admin_task_activity_json).collect::<Vec<_>>()
    }))
    .into_response()
}

pub(crate) async fn admin_list_library_pipeline_activity(
    headers: HeaderMap,
    Query(query): Query<AdminLibraryPipelineActivityQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(20);
    if !(1..=50).contains(&page_size) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "分页参数无效",
        )
        .into_response();
    }
    let (offset, page_size) = match page_params(Some(page), Some(page_size)) {
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
    let page = offset / page_size + 1;
    let database = require_service!(state.database);
    match crate::application::admin_task_activity::list_admin_library_pipeline_activity(
        database, page, page_size,
    )
    .await
    {
        Ok(activity) => Json(admin_library_pipeline_activity_json(&activity)).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

fn admin_library_pipeline_activity_json(
    activity: &crate::application::admin_task_activity::AdminLibraryPipelineActivityPage,
) -> Value {
    use crate::application::admin_task_activity::{
        AdminLibraryPipelineActivity, AdminLibraryPipelineStageActivity,
    };

    fn library_json(library: &AdminLibraryPipelineActivity) -> Value {
        json!({
            "libraryId": library.id,
            "libraryName": library.name,
            "stages": library.stages.iter().map(stage_json).collect::<Vec<_>>(),
        })
    }

    fn stage_json(stage: &AdminLibraryPipelineStageActivity) -> Value {
        use crate::application::admin_task_activity::AdminLibraryPipelineStageKind;

        let stage_name = match stage.kind {
            AdminLibraryPipelineStageKind::Index => "INDEX",
            AdminLibraryPipelineStageKind::LocalMetadata => "LOCAL_METADATA",
            AdminLibraryPipelineStageKind::Probe => "PROBE",
            AdminLibraryPipelineStageKind::Thumbnail => "THUMBNAIL",
            AdminLibraryPipelineStageKind::AutomaticMetadata => "AUTOMATIC_METADATA",
        };
        json!({
            "stage": stage_name,
            "planned": stage.planned,
            "statusCounts": {
                "queued": stage.counts.queued,
                "running": stage.counts.running,
                "completed": stage.counts.completed,
                "failed": stage.counts.failed,
                "cancelled": stage.counts.cancelled,
            },
            "recentActivityAt": stage.recent_activity_at,
        })
    }

    json!({
        "page": activity.page,
        "pageSize": activity.page_size,
        "totalLibraries": activity.total_libraries,
        "libraries": activity.libraries.iter().map(library_json).collect::<Vec<_>>(),
    })
}

fn admin_task_activity_json(
    activity: &crate::application::admin_task_activity::AdminTaskActivity,
) -> Value {
    use crate::application::admin_task_activity::AdminTaskActivity;

    match activity {
        AdminTaskActivity::Scan {
            id,
            task_type,
            library_id,
            status,
            processed_count,
            total_count,
            processed_media_count,
            discovery_completed,
            cancel_requested,
            current_item,
            scan_phase,
        } => json!({
            "id": id,
            "kind": "scan",
            "taskType": task_type,
            "libraryId": library_id,
            "status": status,
            "processedCount": processed_count,
            "totalCount": total_count,
            "processedMediaCount": processed_media_count,
            "discoveryCompleted": discovery_completed,
            "cancelRequested": cancel_requested,
            "currentItem": current_item,
            "scanPhase": scan_phase,
        }),
        AdminTaskActivity::Metadata {
            id,
            task_type,
            library_id,
            status,
            processed_count,
            total_count,
            cancel_requested,
            current_item,
        } => json!({
            "id": id,
            "kind": "metadata",
            "taskType": task_type,
            "libraryId": library_id,
            "status": status,
            "processedCount": processed_count,
            "totalCount": total_count,
            "cancelRequested": cancel_requested,
            "currentItem": current_item,
        }),
        AdminTaskActivity::Strm {
            id,
            library_id,
            status,
            processed_count,
            total_count,
            cancel_requested,
        } => json!({
            "id": id,
            "kind": "strm",
            "taskType": "STRM_MEDIA_INFO",
            "libraryId": library_id,
            "status": status,
            "processedCount": processed_count,
            "totalCount": total_count,
            "cancelRequested": cancel_requested,
        }),
        AdminTaskActivity::Chapter {
            id,
            library_id,
            status,
            processed_count,
            total_count,
            cancel_requested,
            current_item,
        } => json!({
            "id": id,
            "kind": "chapter",
            "taskType": "CHAPTER_DETECTION",
            "libraryId": library_id,
            "status": status,
            "processedCount": processed_count,
            "totalCount": total_count,
            "cancelRequested": cancel_requested,
            "currentItem": current_item,
        }),
        AdminTaskActivity::Danmaku {
            id,
            library_id,
            status,
            processed_count,
            total_count,
            cancel_requested,
            current_item,
        } => json!({
            "id": id,
            "kind": "danmaku",
            "taskType": "DANMAKU_MATCH",
            "libraryId": library_id,
            "status": status,
            "processedCount": processed_count,
            "totalCount": total_count,
            "cancelRequested": cancel_requested,
            "currentItem": current_item,
        }),
        AdminTaskActivity::Cover {
            id,
            library_id,
            status,
            processed_count,
            total_count,
        } => json!({
            "id": id,
            "kind": "cover",
            "taskType": "AUTO_LIBRARY_COVER",
            "libraryId": library_id,
            "status": status,
            "processedCount": processed_count,
            "totalCount": total_count,
        }),
    }
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
    let database = require_service!(state.database);
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
    let database = require_service!(state.database);
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
    let database = require_service!(state.database);
    let is_enabled = request.is_enabled.unwrap_or(schedule.is_some());
    let plan_request = crate::application::admin_scheduled_tasks::ScheduledTaskPlanCreate {
        task_type,
        name: name.to_owned(),
        schedule,
        is_enabled,
        resource_limit: request.resource_limit,
        library_ids: request.library_ids,
    };
    match crate::application::admin_scheduled_tasks::create_scheduled_task_plan(
        database,
        plan_request,
    )
    .await
    {
        Ok(Some(plan)) => (
            StatusCode::CREATED,
            Json(json!({ "plan": scheduled_task_plan_json(&plan) })),
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanCreateError::TaskNotRegistered) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "选中的媒体库尚未注册该任务",
        ).into_response(),
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanCreateError::InvalidLibraries) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "计划名称和媒体库不能为空",
        ).into_response(),
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanCreateError::InvalidResourceLimit) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "资源限制必须是 JSON 对象",
        ).into_response(),
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanCreateError::ResourceLimitSerialization) => StatusCode::BAD_REQUEST.into_response(),
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanCreateError::Storage(StorageError::Conflict(message))) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            &message,
        ).into_response(),
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanCreateError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
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
    let database = require_service!(state.database);
    let update = crate::application::admin_scheduled_tasks::ScheduledTaskPlanUpdate {
        name: request.name,
        schedule: request.schedule,
        is_enabled: request.is_enabled,
        library_ids: request.library_ids,
        resource_limit: request.resource_limit,
    };
    match crate::application::admin_scheduled_tasks::update_scheduled_task_plan(
        database, &plan_id, update,
    )
    .await
    {
        Ok(Some(plan)) => Json(json!({ "plan": scheduled_task_plan_json(&plan) })).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanUpdateError::NotFound) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "执行计划不存在",
        ).into_response(),
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanUpdateError::InvalidNameOrLibraries) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "计划名称和媒体库不能为空",
        ).into_response(),
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanUpdateError::InvalidSchedule) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "Cron 执行计划无效",
        ).into_response(),
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanUpdateError::InvalidResourceLimit) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "资源限制必须是 JSON 对象",
        ).into_response(),
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanUpdateError::ResourceLimitSerialization) => StatusCode::BAD_REQUEST.into_response(),
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanUpdateError::Storage(StorageError::Conflict(message))) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            &message,
        ).into_response(),
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanUpdateError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
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
    let database = require_service!(state.database);
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
    let database = require_service!(state.database);
    let result = crate::application::admin_scheduled_tasks::run_scheduled_task_plan(
        database,
        state.scheduled_tasks.as_ref(),
        &plan_id,
    )
    .await;
    let result = match result {
        Ok(result) => result,
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanRunError::NotFound) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "执行计划不存在",
            )
            .into_response();
        }
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanRunError::GlobalPlan) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::InvalidRequest,
                "全局插件计划不能通过媒体库计划接口执行",
            )
            .into_response();
        }
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskPlanRunError::ServiceUnavailable) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let runs = result
        .runs
        .iter()
        .map(|(library_id, run)| {
            json!({
                "libraryId": library_id,
                "run": scheduled_task_run_json(run),
            })
        })
        .collect::<Vec<_>>();
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "status": "ACCEPTED",
            "planId": result.plan_id,
            "taskType": result.task_type,
            "runs": runs,
            "skippedLibraryIds": result.skipped_library_ids,
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
    let (owner, target_owner) = if owner_type == "GLOBAL" {
        if !request.owner_id.trim().eq_ignore_ascii_case("global") {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "全局计划的 ownerId 必须是 global",
            )
            .into_response();
        }
        (
            crate::application::admin_scheduled_tasks::ScheduledTaskOwner::Global,
            "global".to_owned(),
        )
    } else {
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
        (
            crate::application::admin_scheduled_tasks::ScheduledTaskOwner::Library(library_id),
            library_id.to_string(),
        )
    };
    let update = crate::application::admin_scheduled_tasks::ScheduledTaskConfigUpdate {
        owner,
        task_type: task_type.clone(),
        schedule: request.schedule,
        is_enabled: request.is_enabled,
    };
    let task = match crate::application::admin_scheduled_tasks::update_scheduled_task_config(
        state.database.as_ref(),
        state.libraries.as_ref(),
        state.plugins.as_ref(),
        update,
    )
    .await
    {
        Ok(task) => task,
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskConfigUpdateError::TaskNotRegistered) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "任务尚未注册",
            )
            .into_response();
        }
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskConfigUpdateError::ChapterPluginNotRegistered) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "片头片尾检测任务插件未注册",
            )
            .into_response();
        }
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskConfigUpdateError::RequiredSchedule(required)) => {
            let message = match required {
                crate::application::admin_scheduled_tasks::RequiredScheduledTaskSchedule::StrmMediaInfo => {
                    "STRM 媒体信息任务必须保留 Cron 执行计划"
                }
                crate::application::admin_scheduled_tasks::RequiredScheduledTaskSchedule::DanmakuMatch => {
                    "弹幕匹配任务必须保留 Cron 执行计划"
                }
                crate::application::admin_scheduled_tasks::RequiredScheduledTaskSchedule::ChapterDetection => {
                    "片头片尾检测任务必须保留 Cron 执行计划"
                }
            };
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskConfigUpdateError::InvalidSchedule) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "Cron 执行计划无效",
            )
            .into_response();
        }
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskConfigUpdateError::ServiceUnavailable)
        | Err(crate::application::admin_scheduled_tasks::ScheduledTaskConfigUpdateError::StorageUnavailable) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskConfigUpdateError::Library(error)) => {
            return library_error(&headers, error);
        }
        Err(crate::application::admin_scheduled_tasks::ScheduledTaskConfigUpdateError::Plugin(error)) => {
            return plugin_error(&headers, error);
        }
    };
    let target_id = format!("{target_owner}:{task_type}");
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
