//! Lux server binary entry point.

use luxd::{
    api::{AppState, app_with_state},
    application::restart::{RestartHandle, ShutdownReason},
    application::{settings::read_network_proxy_url, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    discovery::{DiscoveryConfig, DiscoveryService},
    observability,
    storage::{Database, StorageError},
};
use std::net::SocketAddr;
use tokio::{net::TcpListener, sync::watch};
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::from_env()?;
    let discovery_config = DiscoveryConfig::from_env(config.http_addr)?;
    let (_logging_guard, _log_store) = observability::init(&config.config_dir).await;
    luxd::application::plugin_compat::migrate_legacy_tmdb_config(&config.config_dir).await?;
    let explicit_database_configuration = config.load_explicit_database_configuration().await?;
    let legacy_sqlite_database = config.has_legacy_sqlite_database().await;
    if explicit_database_configuration.is_none() && !legacy_sqlite_database {
        config.mark_database_selection_pending().await?;
    }
    let database_configuration = explicit_database_configuration
        .clone()
        .or_else(|| legacy_sqlite_database.then_some(luxd::config::DatabaseConfiguration::Sqlite));
    let database = match database_configuration.as_ref() {
        Some(configuration) => Database::connect_with_configuration(&config, configuration).await?,
        None => Database::connect(&config).await?,
    };
    let schema_version = database.schema_version().await?;
    info!(schema_version, "database migrations applied");
    let migrated_scan_events = match database.migrate_legacy_scan_job_events_to_logs().await {
        Ok(migrated) => migrated,
        Err(error) => {
            let error_code = match &error {
                StorageError::Io { source, .. }
                    if source.kind() == std::io::ErrorKind::PermissionDenied =>
                {
                    "CONFIG_LOG_PERMISSION_DENIED"
                }
                StorageError::Io { source, .. }
                    if source.kind() == std::io::ErrorKind::InvalidData =>
                {
                    "CONFIG_LOG_INVALID_DATA"
                }
                StorageError::Io { .. } => "CONFIG_LOG_WRITE_FAILED",
                StorageError::Sqlx { .. } => "DATABASE_LOG_MIGRATION_FAILED",
                StorageError::Conflict(_) => "LEGACY_LOG_DATA_INVALID",
                _ => "LOG_MIGRATION_FAILED",
            };
            error!(
                error_code,
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
            let error_code = match &error {
                StorageError::Io { source, .. }
                    if source.kind() == std::io::ErrorKind::PermissionDenied =>
                {
                    "CONFIG_LOG_PERMISSION_DENIED"
                }
                StorageError::Io { source, .. }
                    if source.kind() == std::io::ErrorKind::InvalidData =>
                {
                    "CONFIG_LOG_INVALID_DATA"
                }
                StorageError::Io { .. } => "CONFIG_LOG_WRITE_FAILED",
                StorageError::Sqlx { .. } => "DATABASE_LOG_MIGRATION_FAILED",
                StorageError::Conflict(_) => "LEGACY_LOG_DATA_INVALID",
                _ => "LOG_MIGRATION_FAILED",
            };
            error!(
                error_code,
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
    match luxd::application::image_repairs::repair_episode_image_path_conflicts(&database).await {
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
    let cancelled_jobs = database.cancel_incomplete_jobs_for_shutdown().await?;
    if cancelled_jobs > 0 {
        info!(
            cancelled_jobs,
            "unfinished background jobs cancelled before startup"
        );
    }
    let setup = SetupService::new(database.clone())?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let (control_tx, control_rx) = watch::channel(ShutdownReason::Running);
    let mut app_state = AppState::ready_with_proxy(
        config.clone(),
        database.clone(),
        setup,
        auth,
        emby_auth,
        read_network_proxy_url(&config.config_dir),
    )
    .with_restart_handle(RestartHandle::new(control_tx.clone()));
    if explicit_database_configuration.is_none() && !legacy_sqlite_database {
        app_state = app_state.require_database_selection();
    }
    app_state.rebuild_people_index().await;
    app_state.start_local_metadata_worker().await;
    app_state.start_realtime_watchers().await;
    app_state.start_scheduled_tasks().await;
    app_state.start_webhook_worker();
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
        shutdown_state.clone(),
    ))
    .await;
    let _ = shutdown_tx.send(true);
    let _ = discovery_task.await;
    serve_result?;
    let shutdown_reason = *control_tx.borrow();
    shutdown_state.shutdown_background_workers().await;

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
    app_state: AppState,
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

    app_state.shutdown_background_workers().await;
    let _ = shutdown.send(true);
}

#[cfg(unix)]
fn restart_process() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::os::unix::process::CommandExt;

    let executable = std::env::current_exe()?;
    let error = std::process::Command::new(executable)
        .args(std::env::args_os().skip(1))
        .exec();
    Err(error.into())
}

#[cfg(not(unix))]
fn restart_process() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Err("在线重启当前平台的 Lux 进程不可用".into())
}
