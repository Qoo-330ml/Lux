use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
};

use crate::{
    application::{
        admin_health::{AdminHealthProbeCache, AdminHealthReport, admin_health_report},
        catalog::{CatalogItem, CatalogService},
        ip_location::{IpLocation, IpLocationService},
        libraries::LibraryService,
    },
    auth::users::{UserRecord, UserStore},
    domain::ids::UserId,
    observability::resources::ResourceMetrics,
    storage::{
        DashboardStats, Database, StoredActivityEvent, StoredPlaybackSession,
        StoredWebPlaybackSession,
    },
};

pub(crate) const DEFAULT_SERVER_NAME: &str = "Lux Server";
const DASHBOARD_ACTIVITY_LIMIT: i64 = 24;
const DASHBOARD_PLAYBACK_LIMIT: usize = 24;

pub(crate) struct AdminDashboardServices<'a> {
    pub(crate) libraries: Option<&'a LibraryService>,
    pub(crate) catalog: Option<&'a CatalogService>,
    pub(crate) ip_location: Option<&'a IpLocationService>,
    pub(crate) health_probe_cache: &'a AdminHealthProbeCache,
}

pub(crate) struct AdminDashboardSnapshot {
    pub(crate) server_name: String,
    pub(crate) schema_version: i64,
    pub(crate) health: AdminHealthReport,
    pub(crate) stats: DashboardStats,
    pub(crate) now_playing: Vec<DashboardPlayback>,
    pub(crate) activity: Vec<DashboardActivity>,
}

pub(crate) struct DashboardPlayback {
    pub(crate) session: StoredPlaybackSession,
    pub(crate) item: CatalogItem,
    pub(crate) series: Option<CatalogItem>,
    pub(crate) user_name: String,
    pub(crate) remote_ip_location: Option<IpLocation>,
    pub(crate) web_session: Option<StoredWebPlaybackSession>,
}

pub(crate) struct DashboardActivity {
    pub(crate) event: StoredActivityEvent,
    pub(crate) series: Option<DashboardActivitySeries>,
    pub(crate) remote_ip_location: Option<IpLocation>,
}

#[derive(Clone)]
pub(crate) struct DashboardActivitySeries {
    pub(crate) title: String,
    pub(crate) season_number: Option<i64>,
    pub(crate) season_count: Option<i64>,
}

#[derive(Debug)]
pub(crate) enum AdminDashboardError {
    HealthUnavailable,
    ServerNameUnavailable,
    StatisticsUnavailable,
    PlaybackSessionsUnavailable,
    UsersUnavailable,
    ActivityUnavailable,
    CatalogUnavailable,
}

pub(crate) async fn load_admin_dashboard(
    database: &Database,
    config_dir: Option<&Path>,
    resources: &ResourceMetrics,
    services: AdminDashboardServices<'_>,
) -> Result<AdminDashboardSnapshot, AdminDashboardError> {
    let health = admin_health_report(
        database,
        config_dir,
        services.libraries,
        resources,
        services.health_probe_cache,
    )
    .await
    .map_err(|_| AdminDashboardError::HealthUnavailable)?;
    let schema_version = health.schema_version();
    let server_name = match database
        .server_name()
        .await
        .map_err(|_| AdminDashboardError::ServerNameUnavailable)?
    {
        Some(name) if !name.trim().is_empty() => name,
        _ => DEFAULT_SERVER_NAME.to_owned(),
    };
    let stats = database
        .dashboard_stats()
        .await
        .map_err(|_| AdminDashboardError::StatisticsUnavailable)?;
    let sessions = database
        .list_playback_sessions(None, None)
        .await
        .map_err(|_| AdminDashboardError::PlaybackSessionsUnavailable)?;
    let users = UserStore::new(database.clone())
        .map_err(|_| AdminDashboardError::UsersUnavailable)?
        .list_users()
        .await
        .map_err(|_| AdminDashboardError::UsersUnavailable)?;
    let events = database
        .list_activity_events(DASHBOARD_ACTIVITY_LIMIT)
        .await
        .map_err(|_| AdminDashboardError::ActivityUnavailable)?;
    let activity_series = dashboard_activity_series_metadata(services.catalog, &events).await?;
    let activity = events
        .into_iter()
        .map(|event| {
            let metadata = serde_json::from_str::<serde_json::Value>(&event.metadata_json)
                .unwrap_or_else(|_| serde_json::json!({}));
            let remote_ip_location = metadata
                .get("remoteIp")
                .and_then(serde_json::Value::as_str)
                .and_then(|remote_ip| {
                    services
                        .ip_location
                        .and_then(|service| service.cached_or_schedule(remote_ip))
                });
            DashboardActivity {
                series: activity_series.get(&event.id).cloned(),
                event,
                remote_ip_location,
            }
        })
        .collect();
    let now_playing = dashboard_playback_data(
        database,
        services.catalog,
        services.ip_location,
        &sessions,
        &users,
    )
    .await?;

    Ok(AdminDashboardSnapshot {
        server_name,
        schema_version,
        health,
        stats,
        now_playing,
        activity,
    })
}

async fn dashboard_playback_data(
    database: &Database,
    catalog: Option<&CatalogService>,
    ip_location: Option<&IpLocationService>,
    sessions: &[StoredPlaybackSession],
    users: &[UserRecord],
) -> Result<Vec<DashboardPlayback>, AdminDashboardError> {
    let Some(catalog) = catalog else {
        return Ok(Vec::new());
    };
    let user_names = users
        .iter()
        .map(|user| (user.id.to_string(), user.display_name.clone()))
        .collect::<BTreeMap<_, _>>();
    let principal = crate::application::access::AccessPrincipal::new(UserId::new(), true);
    let sessions = sessions
        .iter()
        .take(DASHBOARD_PLAYBACK_LIMIT)
        .collect::<Vec<_>>();
    let web_play_session_ids = sessions
        .iter()
        .filter(|session| {
            session.play_session_id.starts_with("lux-emby:")
                || session.play_session_id.starts_with("lux-web:")
        })
        .map(|session| session.play_session_id.clone())
        .collect::<Vec<_>>();
    let web_sessions = database
        .find_web_playback_sessions_for_playbacks(&web_play_session_ids)
        .await
        .map_err(|_| AdminDashboardError::PlaybackSessionsUnavailable)?
        .into_iter()
        .map(|session| (session.play_session_id.clone(), session))
        .collect::<HashMap<_, _>>();
    let item_ids = sessions
        .iter()
        .map(|session| session.item_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let items_by_id = catalog
        .find_items(principal, &item_ids)
        .await
        .map_err(|_| AdminDashboardError::CatalogUnavailable)?;
    let series_ids = items_by_id
        .values()
        .filter(|item| item.item_type == "EPISODE")
        .filter_map(|item| item.series_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let series_by_id = catalog
        .find_items(principal, &series_ids)
        .await
        .map_err(|_| AdminDashboardError::CatalogUnavailable)?;
    let mut values = Vec::new();
    for session in sessions {
        let Some(item) = items_by_id.get(&session.item_id) else {
            continue;
        };
        let series = item
            .series_id
            .as_deref()
            .and_then(|series_id| series_by_id.get(series_id));
        let remote_ip_location = session.remote_ip.as_deref().and_then(|remote_ip| {
            ip_location.and_then(|service| service.cached_or_schedule(remote_ip))
        });
        values.push(DashboardPlayback {
            session: (*session).clone(),
            item: item.clone(),
            series: series.cloned(),
            user_name: user_names
                .get(&session.user_id)
                .cloned()
                .unwrap_or_else(|| "未知账户".to_owned()),
            remote_ip_location,
            web_session: web_sessions.get(&session.play_session_id).cloned(),
        });
    }
    Ok(values)
}

async fn dashboard_activity_series_metadata(
    catalog: Option<&CatalogService>,
    events: &[StoredActivityEvent],
) -> Result<HashMap<String, DashboardActivitySeries>, AdminDashboardError> {
    let Some(catalog) = catalog else {
        return Ok(HashMap::new());
    };
    let item_ids = events
        .iter()
        .filter(|event| event.target_type.as_deref() == Some("media_item"))
        .filter_map(|event| event.target_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if item_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let principal = crate::application::access::AccessPrincipal::new(UserId::new(), true);
    let items_by_id = catalog
        .find_items(principal, &item_ids)
        .await
        .map_err(|_| AdminDashboardError::CatalogUnavailable)?;
    let series_ids = items_by_id
        .values()
        .filter(|item| item.item_type == "EPISODE")
        .filter_map(|item| item.series_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let series_by_id = catalog
        .find_items(principal, &series_ids)
        .await
        .map_err(|_| AdminDashboardError::CatalogUnavailable)?;

    Ok(events
        .iter()
        .filter_map(|event| {
            let item = event
                .target_id
                .as_deref()
                .and_then(|item_id| items_by_id.get(item_id))?;
            if item.item_type != "EPISODE" {
                return None;
            }
            let series = item
                .series_id
                .as_deref()
                .and_then(|series_id| series_by_id.get(series_id))?;
            Some((
                event.id.clone(),
                DashboardActivitySeries {
                    title: series.title.clone(),
                    season_number: item.season_number,
                    season_count: series.season_count,
                },
            ))
        })
        .collect())
}
