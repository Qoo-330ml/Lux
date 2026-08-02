//! Lux server binary entry point.

use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::sessions::WebAuthService,
    config::Config,
    observability,
    storage::Database,
};
use tokio::net::TcpListener;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::from_env()?;
    observability::init();
    let database = Database::connect(&config).await?;
    let schema_version = database.schema_version().await?;
    info!(schema_version, "database migrations applied");
    let setup = SetupService::new(database.clone())?;
    let auth = WebAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        auth,
    ));

    let listener = TcpListener::bind(config.http_addr).await?;
    info!(address = %config.http_addr, version = luxd::VERSION, "luxd listening");

    axum::serve(listener, app)
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
