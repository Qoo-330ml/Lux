use serde_json::Value;

use crate::{
    application::{
        libraries::{LibraryService, LibraryServiceError, LibrarySettingsPatch},
        plugins::{PluginService, PluginServiceError},
        schedule::{DANMAKU_MATCH_TASK_TYPE, STRM_MEDIA_INFO_TASK_TYPE, validate_cron},
        scheduled_tasks::ScheduledTaskService,
    },
    domain::ids::LibraryId,
    storage::{Database, StorageError, StoredScheduledTaskConfig, StoredScheduledTaskPlan},
};

pub(crate) enum ScheduledTaskOwner {
    Global,
    Library(LibraryId),
}

pub(crate) struct ScheduledTaskConfigUpdate {
    pub owner: ScheduledTaskOwner,
    pub task_type: String,
    pub schedule: Option<String>,
    pub is_enabled: Option<bool>,
}

pub(crate) enum ScheduledTaskConfigUpdateError {
    TaskNotRegistered,
    ChapterPluginNotRegistered,
    RequiredSchedule(RequiredScheduledTaskSchedule),
    InvalidSchedule,
    ServiceUnavailable,
    StorageUnavailable,
    Library(LibraryServiceError),
    Plugin(PluginServiceError),
}

pub(crate) enum RequiredScheduledTaskSchedule {
    StrmMediaInfo,
    DanmakuMatch,
    ChapterDetection,
}

pub(crate) async fn update_scheduled_task_config(
    database: Option<&Database>,
    libraries: Option<&LibraryService>,
    plugins: Option<&PluginService>,
    request: ScheduledTaskConfigUpdate,
) -> Result<StoredScheduledTaskConfig, ScheduledTaskConfigUpdateError> {
    match request.owner {
        ScheduledTaskOwner::Global => {
            let is_strm = request.task_type == STRM_MEDIA_INFO_TASK_TYPE;
            let is_danmaku = request.task_type == DANMAKU_MATCH_TASK_TYPE;
            if is_strm || is_danmaku {
                let database =
                    database.ok_or(ScheduledTaskConfigUpdateError::ServiceUnavailable)?;
                match database
                    .find_scheduled_task_config("GLOBAL", "global", &request.task_type)
                    .await
                    .map_err(|_| ScheduledTaskConfigUpdateError::StorageUnavailable)?
                {
                    Some(_) => {}
                    None => return Err(ScheduledTaskConfigUpdateError::TaskNotRegistered),
                }
                let Some(schedule) = request
                    .schedule
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    return Err(ScheduledTaskConfigUpdateError::RequiredSchedule(
                        if is_strm {
                            RequiredScheduledTaskSchedule::StrmMediaInfo
                        } else {
                            RequiredScheduledTaskSchedule::DanmakuMatch
                        },
                    ));
                };
                let plugins = plugins.ok_or(ScheduledTaskConfigUpdateError::ServiceUnavailable)?;
                let update = if is_strm {
                    plugins.update_media_info_schedule(schedule).await
                } else {
                    plugins.update_danmaku_schedule(schedule).await
                };
                update.map_err(ScheduledTaskConfigUpdateError::Plugin)?;
                return database
                    .find_scheduled_task_config("GLOBAL", "global", &request.task_type)
                    .await
                    .map_err(|_| ScheduledTaskConfigUpdateError::StorageUnavailable)?
                    .ok_or(ScheduledTaskConfigUpdateError::TaskNotRegistered);
            }

            let enabled = request.is_enabled.unwrap_or(request.schedule.is_some());
            let schedule = if enabled {
                request.schedule.as_deref().map(str::trim)
            } else {
                None
            };
            let database = database.ok_or(ScheduledTaskConfigUpdateError::ServiceUnavailable)?;
            database
                .upsert_scheduled_task_config(
                    "GLOBAL",
                    "global",
                    &request.task_type,
                    schedule,
                    enabled,
                )
                .await
                .map_err(|_| ScheduledTaskConfigUpdateError::StorageUnavailable)?
                .ok_or(ScheduledTaskConfigUpdateError::TaskNotRegistered)
        }
        ScheduledTaskOwner::Library(library_id) => {
            let libraries = libraries.ok_or(ScheduledTaskConfigUpdateError::ServiceUnavailable)?;
            let database = database.ok_or(ScheduledTaskConfigUpdateError::ServiceUnavailable)?;
            let library_id_text = library_id.to_string();
            match database
                .find_scheduled_task_config("LIBRARY", &library_id_text, &request.task_type)
                .await
                .map_err(|_| ScheduledTaskConfigUpdateError::StorageUnavailable)?
            {
                Some(_) => {}
                None => return Err(ScheduledTaskConfigUpdateError::TaskNotRegistered),
            }
            let enabled = request.is_enabled.unwrap_or(request.schedule.is_some());
            let schedule = if enabled {
                request
                    .schedule
                    .as_ref()
                    .map(|value| value.trim().to_owned())
            } else {
                None
            };
            if schedule
                .as_deref()
                .is_some_and(|schedule| validate_cron(schedule).is_err())
            {
                return Err(ScheduledTaskConfigUpdateError::InvalidSchedule);
            }
            let mut settings = LibrarySettingsPatch::default();
            match request.task_type.as_str() {
                "RECONCILIATION_SCAN" => {
                    settings.reconciliation_schedule = Some(schedule);
                    libraries
                        .update_settings(library_id, settings)
                        .await
                        .map_err(ScheduledTaskConfigUpdateError::Library)?;
                    fetch_library_scheduled_task(database, &library_id_text, &request.task_type)
                        .await
                }
                "METADATA_PARSE" => {
                    settings.metadata_schedule = Some(schedule);
                    libraries
                        .update_settings(library_id, settings)
                        .await
                        .map_err(ScheduledTaskConfigUpdateError::Library)?;
                    fetch_library_scheduled_task(database, &library_id_text, &request.task_type)
                        .await
                }
                "AUTO_LIBRARY_COVER" => database
                    .upsert_scheduled_task_config(
                        "LIBRARY",
                        &library_id_text,
                        &request.task_type,
                        schedule.as_deref(),
                        enabled,
                    )
                    .await
                    .map_err(|_| ScheduledTaskConfigUpdateError::StorageUnavailable)?
                    .ok_or(ScheduledTaskConfigUpdateError::TaskNotRegistered),
                "CHAPTER_DETECTION" => {
                    let Some(schedule) = schedule.as_deref() else {
                        return Err(ScheduledTaskConfigUpdateError::RequiredSchedule(
                            RequiredScheduledTaskSchedule::ChapterDetection,
                        ));
                    };
                    let task = database
                        .find_scheduled_task_config("LIBRARY", &library_id_text, &request.task_type)
                        .await
                        .map_err(|_| ScheduledTaskConfigUpdateError::StorageUnavailable)?
                        .ok_or(ScheduledTaskConfigUpdateError::TaskNotRegistered)?;
                    let plugin_id = task
                        .plugin_id
                        .ok_or(ScheduledTaskConfigUpdateError::ChapterPluginNotRegistered)?;
                    let plugins =
                        plugins.ok_or(ScheduledTaskConfigUpdateError::ServiceUnavailable)?;
                    plugins
                        .update_chapter_detector_schedule(&plugin_id, schedule)
                        .await
                        .map_err(ScheduledTaskConfigUpdateError::Plugin)?;
                    fetch_library_scheduled_task(database, &library_id_text, &request.task_type)
                        .await
                }
                _ => Err(ScheduledTaskConfigUpdateError::TaskNotRegistered),
            }
        }
    }
}

async fn fetch_library_scheduled_task(
    database: &Database,
    library_id: &str,
    task_type: &str,
) -> Result<StoredScheduledTaskConfig, ScheduledTaskConfigUpdateError> {
    database
        .find_scheduled_task_config("LIBRARY", library_id, task_type)
        .await
        .map_err(|_| ScheduledTaskConfigUpdateError::StorageUnavailable)?
        .ok_or(ScheduledTaskConfigUpdateError::TaskNotRegistered)
}

pub(crate) struct ScheduledTaskPlanCreate {
    pub task_type: String,
    pub name: String,
    pub schedule: Option<String>,
    pub is_enabled: bool,
    pub resource_limit: Option<Value>,
    pub library_ids: Vec<String>,
}

pub(crate) enum ScheduledTaskPlanCreateError {
    TaskNotRegistered,
    InvalidLibraries,
    InvalidResourceLimit,
    ResourceLimitSerialization,
    Storage(StorageError),
}

pub(crate) async fn create_scheduled_task_plan(
    database: &Database,
    request: ScheduledTaskPlanCreate,
) -> Result<Option<StoredScheduledTaskPlan>, ScheduledTaskPlanCreateError> {
    let Some(template_library_id) = request.library_ids.first() else {
        return Err(ScheduledTaskPlanCreateError::InvalidLibraries);
    };
    let template = database
        .find_scheduled_task_config("LIBRARY", template_library_id, &request.task_type)
        .await
        .map_err(ScheduledTaskPlanCreateError::Storage)?
        .ok_or(ScheduledTaskPlanCreateError::TaskNotRegistered)?;

    let resource_limit_json = match request.resource_limit {
        Some(value) if !value.is_object() => {
            return Err(ScheduledTaskPlanCreateError::InvalidResourceLimit);
        }
        Some(value) => serde_json::to_string(&value)
            .map_err(|_| ScheduledTaskPlanCreateError::ResourceLimitSerialization)?,
        None => template.resource_limit_json.clone(),
    };
    let plan_id = uuid::Uuid::now_v7().to_string();
    database
        .create_scheduled_task_plan(
            &plan_id,
            &request.task_type,
            &request.name,
            &template.task_name,
            &template.task_description,
            &template.source_type,
            template.plugin_id.as_deref(),
            request.schedule.as_deref(),
            request.is_enabled,
            &resource_limit_json,
            &request.library_ids,
        )
        .await
        .map_err(ScheduledTaskPlanCreateError::Storage)
}

pub(crate) struct ScheduledTaskPlanUpdate {
    pub name: Option<String>,
    pub schedule: Option<Option<String>>,
    pub is_enabled: Option<bool>,
    pub library_ids: Option<Vec<String>>,
    pub resource_limit: Option<Value>,
}

pub(crate) enum ScheduledTaskPlanUpdateError {
    NotFound,
    InvalidNameOrLibraries,
    InvalidSchedule,
    InvalidResourceLimit,
    ResourceLimitSerialization,
    Storage(StorageError),
}

pub(crate) async fn update_scheduled_task_plan(
    database: &Database,
    plan_id: &str,
    request: ScheduledTaskPlanUpdate,
) -> Result<Option<StoredScheduledTaskPlan>, ScheduledTaskPlanUpdateError> {
    let existing = match database.find_scheduled_task_plan(plan_id).await {
        Ok(Some(existing)) => existing,
        Ok(None) | Err(_) => return Err(ScheduledTaskPlanUpdateError::NotFound),
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
        return Err(ScheduledTaskPlanUpdateError::InvalidNameOrLibraries);
    }
    let schedule = match request.schedule.as_ref() {
        None => existing.cron_or_interval.clone(),
        Some(None) => None,
        Some(Some(value)) if value.trim().is_empty() => None,
        Some(Some(value)) if validate_cron(value.trim()).is_err() => {
            return Err(ScheduledTaskPlanUpdateError::InvalidSchedule);
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
            return Err(ScheduledTaskPlanUpdateError::InvalidResourceLimit);
        }
        Some(value) => serde_json::to_string(&value)
            .map_err(|_| ScheduledTaskPlanUpdateError::ResourceLimitSerialization)?,
        None => existing.resource_limit_json.clone(),
    };
    database
        .update_scheduled_task_plan(
            plan_id,
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
        .map_err(ScheduledTaskPlanUpdateError::Storage)
}

pub(crate) struct ScheduledTaskPlanRun {
    pub plan_id: String,
    pub task_type: String,
    pub runs: Vec<(
        String,
        crate::application::scheduled_tasks::ScheduledTaskRun,
    )>,
    pub skipped_library_ids: Vec<String>,
}

pub(crate) enum ScheduledTaskPlanRunError {
    NotFound,
    GlobalPlan,
    ServiceUnavailable,
}

pub(crate) async fn run_scheduled_task_plan(
    database: &Database,
    scheduled_tasks: Option<&ScheduledTaskService>,
    plan_id: &str,
) -> Result<ScheduledTaskPlanRun, ScheduledTaskPlanRunError> {
    let plan = database
        .find_scheduled_task_plan(plan_id)
        .await
        .ok()
        .flatten()
        .ok_or(ScheduledTaskPlanRunError::NotFound)?;
    if plan.scope_type != "LIBRARY" {
        return Err(ScheduledTaskPlanRunError::GlobalPlan);
    }
    let scheduled_tasks = scheduled_tasks.ok_or(ScheduledTaskPlanRunError::ServiceUnavailable)?;
    let mut runs = Vec::new();
    let mut skipped_library_ids = Vec::new();
    for library in plan.libraries {
        match scheduled_tasks
            .run_task("LIBRARY", &library.id, &plan.task_type)
            .await
        {
            Ok(run) => runs.push((library.id, run)),
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
    Ok(ScheduledTaskPlanRun {
        plan_id: plan.id,
        task_type: plan.task_type,
        runs,
        skipped_library_ids,
    })
}
