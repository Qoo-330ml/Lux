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
#[path = "admin/admin_emby_migration.rs"]
mod admin_emby_migration;
#[path = "admin/admin_jobs.rs"]
mod admin_jobs;
#[path = "admin/admin_scheduled_tasks.rs"]
mod admin_scheduled_tasks;
pub(super) use admin_emby_migration::emby_migration_error;
pub(super) use admin_jobs::{
    AdminJobsQuery, AdminLogExportQuery, scan_job_json, scan_job_json_from_storage,
    spawn_library_scan, strm_probe_error,
};

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
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
        .merge(admin_emby_migration::api_routes())
        .merge(admin_scheduled_tasks::api_routes())
        .merge(admin_jobs::api_routes())
}
