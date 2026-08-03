//! Lux server binary entry point.

use luxd::{
    api::{AppState, app_with_state},
    application::{
        probe::{FfprobeRunner, MediaProbeService},
        reidentify::MetadataReidentifyService,
        scanner::ScanJobService,
        settings::{read_tmdb_api_key, read_tmdb_token},
        setup::SetupService,
        tmdb::TmdbClient,
    },
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
    observability::init();
    let database = Database::connect(&config).await?;
    let schema_version = database.schema_version().await?;
    info!(schema_version, "database migrations applied");
    let setup = SetupService::new(database.clone())?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    resume_scan_jobs(
        ScanJobService::new(database.clone()),
        Some(MediaProbeService::new(
            database.clone(),
            FfprobeRunner::default(),
        )),
    )
    .await;
    if let Ok(tmdb) = TmdbClient::from_env_or_config(
        read_tmdb_api_key(&config.config_dir),
        read_tmdb_token(&config.config_dir),
    ) {
        resume_metadata_reidentify_jobs(MetadataReidentifyService::new(database.clone(), tmdb))
            .await;
    }
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        auth,
        emby_auth,
    ));

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

async fn resume_scan_jobs(service: ScanJobService, probe: Option<MediaProbeService>) {
    let Ok(job_ids) = service.active_job_ids().await else {
        error!("failed to discover active scan jobs during startup");
        return;
    };
    for job_id in job_ids {
        let worker = service.clone();
        let worker_probe = probe.clone();
        tokio::spawn(async move {
            if let Err(error) = worker.run_to_completion(&job_id, 100, worker_probe).await {
                tracing::error!(job_id = %job_id, %error, "resumed scan job stopped");
            }
        });
    }
}

async fn resume_metadata_reidentify_jobs(service: MetadataReidentifyService) {
    let Ok(job_ids) = service.active_job_ids().await else {
        error!("failed to discover active metadata reidentify jobs during startup");
        return;
    };
    for job_id in job_ids {
        let worker = service.clone();
        tokio::spawn(async move {
            worker.run(&job_id).await;
        });
    }
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
