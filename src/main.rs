//! Lux server binary entry point.

use luxd::{
    api::{AppState, app_with_state},
    application::{settings::read_network_proxy_url, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    observability,
    storage::Database,
};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::from_env()?;
    let _logging_guard = observability::init(&config.config_dir).await;
    let explicit_database_configuration = config.load_explicit_database_configuration().await?;
    let legacy_sqlite_database = config.has_legacy_sqlite_database().await;
    let database_configuration = explicit_database_configuration
        .clone()
        .or_else(|| legacy_sqlite_database.then_some(luxd::config::DatabaseConfiguration::Sqlite));
    let database = match database_configuration.as_ref() {
        Some(configuration) => Database::connect_with_configuration(&config, configuration).await?,
        None => Database::connect(&config).await?,
    };
    let schema_version = database.schema_version().await?;
    info!(schema_version, "database migrations applied");
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
    );
    if explicit_database_configuration.is_none() && !legacy_sqlite_database {
        app_state = app_state.require_database_selection();
    }
    app_state.resume_scan_jobs().await;
    app_state.start_realtime_watchers().await;
    app_state.resume_strm_probe_jobs().await;
    app_state.resume_danmaku_match_jobs().await;
    app_state.resume_metadata_reidentify_jobs().await;
    let app = app_with_state(app_state);

    let listener = TcpListener::bind(config.http_addr).await?;
    info!(address = %config.http_addr, version = luxd::VERSION, "luxd listening");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    database.close().await;
    Ok(())
}

async fn shutdown_signal() {
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
            _ = ctrl_c => {}
            _ = terminate => {}
        }
    }

    #[cfg(not(unix))]
    ctrl_c.await;
}
