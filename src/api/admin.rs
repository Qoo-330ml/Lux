use super::*;

#[path = "admin_handlers.rs"]
mod admin_handlers;
pub(super) use admin_handlers::*;
#[path = "admin/admin_users.rs"]
mod admin_users;
pub(super) use admin_users::user_json;
#[path = "admin/admin_settings.rs"]
mod admin_settings;
pub(super) use admin_settings::*;
#[path = "admin/admin_libraries.rs"]
mod admin_libraries;
pub(super) use admin_libraries::*;
#[path = "admin/admin_items.rs"]
mod admin_items;
pub(super) use admin_items::*;
#[path = "admin/admin_metadata.rs"]
mod admin_metadata;
#[path = "admin/admin_people.rs"]
mod admin_people;
#[path = "admin/admin_plugins.rs"]
mod admin_plugins;
pub(super) use admin_metadata::{
    ItemImageSearchRequest, ItemImageSelectRequest, MetadataCandidateQuery, image_candidate_error,
    image_candidate_json, image_write_error, item_image_json, metadata_write_error,
};
#[cfg(test)]
pub(super) use admin_metadata::{
    MetadataCandidateFailureKind, metadata_candidate_error, metadata_candidate_failure_kind,
};
pub(super) use admin_plugins::*;

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
            "/api/v1/admin/notification-destinations",
            get(admin_list_webhook_destinations).post(admin_create_webhook_destination),
        )
        .route(
            "/api/v1/admin/notification-destinations/{destination_id}",
            get(admin_get_webhook_destination)
                .patch(admin_update_webhook_destination)
                .delete(admin_delete_webhook_destination),
        )
        .route(
            "/api/v1/admin/notification-destinations/{destination_id}/test",
            post(admin_test_webhook_destination),
        )
        .route(
            "/api/v1/admin/notification-destinations/{destination_id}/rotate-secret",
            post(admin_rotate_webhook_secret),
        )
        .route(
            "/api/v1/admin/notification-deliveries",
            get(admin_list_webhook_deliveries),
        )
        .route(
            "/api/v1/admin/notification-deliveries/{delivery_id}/retry",
            post(admin_retry_webhook_delivery),
        )
        .route("/api/v1/admin/health", get(admin_health))
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
        .route("/api/v1/admin/dashboard", get(admin_dashboard))
        .route("/api/v1/admin/events", get(admin_events))
        .route("/api/v1/events", get(user_events))
        .route("/api/v1/admin/audit", get(admin_list_audit))
        .route("/api/v1/admin/logs/export", get(admin_export_logs))
        .route("/api/v1/admin/logs", get(admin_list_logs))
        .merge(admin_users::api_routes())
        .merge(admin_settings::api_routes())
        .merge(admin_libraries::api_routes())
        .merge(admin_items::api_routes())
        .merge(admin_people::api_routes())
        .merge(admin_metadata::api_routes())
        .merge(admin_plugins::api_routes())
}
