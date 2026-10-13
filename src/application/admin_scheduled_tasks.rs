use serde_json::Value;

use crate::{
    application::{schedule::validate_cron, scheduled_tasks::ScheduledTaskService},
    storage::{Database, StorageError, StoredScheduledTaskPlan},
};

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
