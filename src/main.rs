//! Lux server binary entry point.

use luxd::{
    api::{AppState, app_with_state},
    application::restart::{RestartHandle, ShutdownReason},
    application::{settings::read_network_proxy_url, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::{Config, DatabaseConfiguration},
    discovery::{DiscoveryConfig, DiscoveryService},
    observability,
    storage::{Database, StorageError},
};
use std::net::SocketAddr;
use tokio::{net::TcpListener, sync::watch};
use tracing::{error, info};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let config = Config::from_env()?;
    let discovery_config = DiscoveryConfig::from_env(config.http_addr)?;
    let (_logging_guard, _log_store) = observability::init(&config.config_dir).await;
    // Resolve the container runtime gateway once, off the request path: the access-IP resolver
    // reads it for every forwarded request, and `/proc` lookups must not run on Tokio workers.
    luxd::application::container_gateway::warm_up();
    luxd::application::plugin_compat::migrate_legacy_tmdb_config(&config.config_dir).await?;
    let (database, requires_database_selection) = prepare_database(&config).await?;
    repair_historical_data(&database).await;
    start_shared_workers(&database).await?;
    let (control_tx, control_rx) = watch::channel(ShutdownReason::Running);
    let app_state = build_app_state(
        &config,
        &database,
        requires_database_selection,
        control_tx.clone(),
    )
    .await?;
    serve(
        &config,
        discovery_config,
        database,
        app_state,
        control_tx,
        control_rx,
    )
    .await
}

/// Opens the database, applies migrations, and drains the legacy log tables.
async fn prepare_database(config: &Config) -> Result<(Database, bool), BoxError> {
    let explicit_database_configuration = config.load_explicit_database_configuration().await?;
    let legacy_sqlite_database = config.has_legacy_sqlite_database().await;
    if explicit_database_configuration.is_none() && !legacy_sqlite_database {
        config.mark_database_selection_pending().await?;
    }
    let database_configuration = explicit_database_configuration
        .clone()
        .or_else(|| legacy_sqlite_database.then_some(DatabaseConfiguration::Sqlite));
    let database = match database_configuration.as_ref() {
        Some(configuration) => Database::connect_with_configuration(config, configuration).await?,
        None => Database::connect(config).await?,
    };
    let schema_version = database.schema_version().await?;
    info!(schema_version, "database migrations applied");
    let migrated_scan_events = match database.migrate_legacy_scan_job_events_to_logs().await {
        Ok(migrated) => migrated,
        Err(error) => {
            error!(
                error_code = legacy_log_migration_error_code(&error),
                "legacy scan event migration failed; startup stopped"
            );
            return Err(std::io::Error::other("legacy scan event migration failed").into());
        }
    };
    if migrated_scan_events > 0 {
        info!(
            migrated_scan_events,
            "legacy scan events migrated to config logs"
        );
    }
    let migrated_audit_events = match database.migrate_legacy_audit_events_to_logs().await {
        Ok(migrated) => migrated,
        Err(error) => {
            error!(
                error_code = legacy_log_migration_error_code(&error),
                "legacy audit event migration failed; startup stopped"
            );
            return Err(std::io::Error::other("legacy audit event migration failed").into());
        }
    };
    if migrated_audit_events > 0 {
        info!(
            migrated_audit_events,
            "legacy audit events migrated to config logs"
        );
    }
    let requires_database_selection =
        explicit_database_configuration.is_none() && !legacy_sqlite_database;
    Ok((database, requires_database_selection))
}

/// Maps a legacy-log migration failure to the `error_code` logged by both startup migrations.
fn legacy_log_migration_error_code(error: &StorageError) -> &'static str {
    match error {
        StorageError::Io { source, .. } => match source.kind() {
            std::io::ErrorKind::PermissionDenied => "CONFIG_LOG_PERMISSION_DENIED",
            std::io::ErrorKind::InvalidData => "CONFIG_LOG_INVALID_DATA",
            _ => "CONFIG_LOG_WRITE_FAILED",
        },
        StorageError::Sqlx { .. } => "DATABASE_LOG_MIGRATION_FAILED",
        StorageError::Conflict(_) => "LEGACY_LOG_DATA_INVALID",
        _ => "LOG_MIGRATION_FAILED",
    }
}

/// Applies the one-time repairs that must finish before the HTTP listener accepts requests.
///
/// Measured steady-state cost on a repaired database (66k media items, see
/// `docs/STARTUP-REPAIRS.md`): every step below costs one `lux_meta` read and writes nothing,
/// because the repairs already record their completion there. `run_database_lifecycle_cleanup`
/// used to be the exception, re-running its claim protocol on every start; it now records the
/// completed version with `Database::run_startup_repair_once` and only its recurring scan payload
/// sweep still runs. New repairs belong behind that helper instead of an ad-hoc marker.
async fn repair_historical_data(database: &Database) {
    match luxd::application::image_repairs::repair_episode_image_path_conflicts(database).await {
        Ok(report) if report.repaired > 0 || report.skipped > 0 => {
            info!(
                repaired = report.repaired,
                skipped = report.skipped,
                "episode image path repair completed"
            );
        }
        Ok(_) => {}
        Err(error) => error!(%error, "episode image path repair failed"),
    }
    match database.normalize_media_source_defaults().await {
        Ok(0) => {}
        Ok(repaired) => info!(
            repaired,
            "media items normalized to a single default source"
        ),
        Err(error) => error!(%error, "default media source normalization failed"),
    }
    match database.run_database_lifecycle_cleanup().await {
        Ok(Some(report)) => {
            info!(
                scan_job_paths_deleted = report.scan_job_paths_deleted,
                reconciliation_entries_deleted = report.reconciliation_entries_deleted,
                scan_job_targets_deleted = report.scan_job_targets_deleted,
                scan_jobs_summarized = report.scan_jobs_summarized,
                "one-time database lifecycle cleanup completed"
            );
        }
        Ok(None) => {}
        Err(error) => error!(%error, "one-time database lifecycle cleanup failed"),
    }
}

/// Spawns the maintenance workers and clears work a previous run left behind.
async fn start_shared_workers(database: &Database) -> Result<(), BoxError> {
    database.spawn_removed_media_item_purge();
    database.spawn_task_history_retention();
    database.spawn_postgres_query_statistics_extension();
    database.spawn_database_maintenance();
    database.spawn_detached_state_reconcile();
    database.spawn_query_statistics_refresh();
    let cancelled_jobs = database.cancel_incomplete_jobs_for_shutdown().await?;
    if cancelled_jobs > 0 {
        info!(
            cancelled_jobs,
            "unfinished background jobs cancelled before startup"
        );
    }
    Ok(())
}

/// Builds the HTTP state and starts every worker the request path hands work to.
async fn build_app_state(
    config: &Config,
    database: &Database,
    requires_database_selection: bool,
    control_tx: watch::Sender<ShutdownReason>,
) -> Result<AppState, BoxError> {
    // Resolve the device-node and symlink probes once, on a blocking thread, before the state
    // exists: both read the filesystem the first time they are used, and neither the first admin
    // request nor the first scan may pay for that on a Tokio worker.
    tokio::task::spawn_blocking(luxd::application::warm_up_startup_probes)
        .await
        .map_err(|error| BoxError::from(std::io::Error::other(error.to_string())))?;
    let setup = SetupService::new(database.clone())?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let mut app_state = AppState::ready_with_proxy(
        config.clone(),
        database.clone(),
        setup,
        auth,
        emby_auth,
        read_network_proxy_url(&config.config_dir),
    )
    .with_restart_handle(RestartHandle::new(control_tx));
    if requires_database_selection {
        app_state = app_state.require_database_selection();
    }
    app_state.start_content_restriction_index_worker();
    app_state.rebuild_people_index().await;
    app_state.start_local_metadata_worker().await;
    app_state.resume_interrupted_metadata_scraping();
    app_state.start_realtime_watchers().await;
    app_state.start_scheduled_tasks().await;
    app_state.start_webhook_worker().await;
    Ok(app_state)
}

/// Binds the listeners, serves until shutdown, then drains and closes everything exactly once.
async fn serve(
    config: &Config,
    discovery_config: DiscoveryConfig,
    database: Database,
    app_state: AppState,
    control_tx: watch::Sender<ShutdownReason>,
    control_rx: watch::Receiver<ShutdownReason>,
) -> Result<(), BoxError> {
    // These workers must not delay the listener; only the mail worker needs draining on shutdown.
    let mail_worker = luxd::application::mail_worker::MailDeliveryWorker::start(
        database.clone(),
        config.config_dir.clone(),
    );
    // A large existing library can contain hundreds of thousands of episodes.
    // Repairing their legacy identities is a one-time background operation and
    // must not block the HTTP listener from becoming available.
    let scanner = luxd::application::scanner::LibraryScanner::new(database.clone());
    tokio::spawn(async move {
        match scanner.repair_legacy_identity_keys().await {
            Ok(repaired_identity_keys) if repaired_identity_keys > 0 => {
                info!(repaired_identity_keys, "legacy media identities repaired");
            }
            Ok(_) => {}
            Err(error) => error!(%error, "legacy media identity repair failed"),
        }
    });
    let discovery = DiscoveryService::bind_with_database(discovery_config, &database).await?;
    let discovery_addr = discovery.local_addr()?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let discovery_task = tokio::spawn(async move {
        if let Err(error) = discovery.run(shutdown_rx).await {
            error!(%error, "LAN discovery service stopped unexpectedly");
        }
    });
    info!(address = %discovery_addr, "lux LAN discovery listening");

    let listener = TcpListener::bind(config.http_addr).await?;
    info!(address = %config.http_addr, version = luxd::VERSION, "luxd listening");
    luxd::application::scan_fingerprint_migration::start(database.clone(), control_rx.clone());
    app_state.start_database_diagnostics().await;
    let shutdown_state = app_state.clone();
    let app = app_with_state(app_state);

    let serve_result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(
        control_rx,
        control_tx.clone(),
        shutdown_tx.clone(),
    ))
    .await;
    let _ = shutdown_tx.send(true);
    let _ = discovery_task.await;
    mail_worker.shutdown().await;
    serve_result?;
    let shutdown_reason = *control_tx.borrow();
    if let Err(error) = shutdown_state.shutdown_background_workers_checked().await {
        return Err(std::io::Error::other(error).into());
    }
    match database.cancel_incomplete_jobs_for_shutdown().await {
        Ok(cancelled_jobs) if cancelled_jobs > 0 => {
            info!(
                cancelled_jobs,
                "unfinished background jobs cancelled before shutdown"
            );
        }
        Ok(_) => {}
        Err(error) => error!(%error, "failed to cancel unfinished background jobs before shutdown"),
    }
    database.close().await;
    if shutdown_reason == ShutdownReason::Restart {
        info!("restarting lux process");
        restart_process()?;
    }
    Ok(())
}

async fn shutdown_signal(
    mut control_rx: watch::Receiver<ShutdownReason>,
    control_tx: watch::Sender<ShutdownReason>,
    shutdown: watch::Sender<bool>,
) {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            error!(%error, "failed to install Ctrl-C handler");
        }
    };

    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(error) => error!(%error, "failed to install SIGTERM handler"),
            }
        };

        tokio::select! {
            _ = ctrl_c => {
                let _ = control_tx.send(ShutdownReason::Shutdown);
            }
            _ = terminate => {
                let _ = control_tx.send(ShutdownReason::Shutdown);
            }
            changed = control_rx.changed() => {
                if changed.is_err() {
                    let _ = control_tx.send(ShutdownReason::Shutdown);
                }
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = ctrl_c => {
                let _ = control_tx.send(ShutdownReason::Shutdown);
            }
            changed = control_rx.changed() => {
                if changed.is_err() {
                    let _ = control_tx.send(ShutdownReason::Shutdown);
                }
            }
        }
    }

    // Only signal the HTTP server to stop here. Workers are drained once, after `serve` returns
    // and no further requests can start work, by the caller below.
    let _ = shutdown.send(true);
}

#[cfg(unix)]
fn restart_process() -> Result<(), BoxError> {
    use std::os::unix::process::CommandExt;

    let executable = std::env::current_exe()?;
    let error = std::process::Command::new(executable)
        .args(std::env::args_os().skip(1))
        .exec();
    Err(error.into())
}

#[cfg(not(unix))]
fn restart_process() -> Result<(), BoxError> {
    Err("在线重启当前平台的 Lux 进程不可用".into())
}
