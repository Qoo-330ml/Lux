use super::*;

#[path = "admin/admin_common.rs"]
mod admin_common;
pub(super) use admin_common::{
    api_error, build_cookie, lux_api_key_from_headers, normalize_lux_api_key_query, request_cookie,
    require_admin, require_admin_web_session, resolve_shared_admin_api_key,
    secure_cookie_for_request, user_avatar_error,
};
#[path = "admin/admin_users.rs"]
mod admin_users;
pub(super) use admin_users::user_json;
#[path = "admin/admin_settings.rs"]
mod admin_settings;
pub(super) use admin_settings::{
    MediaStrategySettings, read_media_strategy_settings, validate_media_strategy,
};
#[path = "admin/admin_libraries.rs"]
mod admin_libraries;
pub(super) use admin_libraries::{
    RefreshLocalMetadataRequest, ScanPathRequest, default_realtime_watch_enabled,
    deserialize_optional_optional, library_cover_url, library_error, library_json,
    validate_scraper_selection,
};
#[path = "admin/admin_items.rs"]
mod admin_items;
pub(super) use admin_items::delete_media_item;
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
pub(super) use admin_plugins::plugin_error;
#[path = "admin/admin_dashboard.rs"]
mod admin_dashboard;
#[path = "admin/admin_emby_migration.rs"]
mod admin_emby_migration;
#[path = "admin/admin_health.rs"]
mod admin_health;
pub(super) use crate::application::admin_dashboard::DEFAULT_SERVER_NAME;
#[path = "admin/admin_database_diagnostics.rs"]
mod admin_database_diagnostics;
#[path = "admin/admin_jobs.rs"]
mod admin_jobs;
#[path = "admin/admin_logs_events.rs"]
mod admin_logs_events;
#[path = "admin/admin_scheduled_tasks.rs"]
mod admin_scheduled_tasks;
#[path = "admin/admin_webhooks.rs"]
mod admin_webhooks;
pub(super) use admin_emby_migration::emby_migration_error;
pub(super) use admin_jobs::{
    AdminJobsQuery, scan_job_json, scan_job_json_from_storage, spawn_library_scan, strm_probe_error,
};
pub(super) use admin_logs_events::{
    playback_activity_event_type, record_activity_event, record_audit_event,
};

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
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
        .merge(admin_webhooks::api_routes())
        .merge(admin_logs_events::api_routes())
        .merge(admin_health::api_routes())
        .merge(admin_database_diagnostics::api_routes())
        .merge(admin_dashboard::api_routes())
}
