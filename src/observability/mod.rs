pub mod logs;
pub mod resources;

use std::path::Path;

use tracing_appender::{
    non_blocking::{NonBlockingBuilder, WorkerGuard},
    rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

pub async fn init(config_dir: &Path) -> Option<WorkerGuard> {
    let log_dir = logs::log_dir(config_dir);
    if let Err(error) = tokio::fs::create_dir_all(&log_dir).await {
        eprintln!("Lux file logging unavailable; continuing with stdout logging: {error}");
        init_stdout();
        return None;
    }

    let appender = match tokio::task::spawn_blocking(move || {
        RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix("lux")
            .filename_suffix("log")
            .build(log_dir)
    })
    .await
    {
        Ok(Ok(appender)) => appender,
        Ok(Err(error)) => {
            eprintln!("Lux file logging unavailable; continuing with stdout logging: {error}");
            init_stdout();
            return None;
        }
        Err(error) => {
            eprintln!(
                "Lux file logging worker unavailable; continuing with stdout logging: {error}"
            );
            init_stdout();
            return None;
        }
    };

    let (file_writer, guard) = NonBlockingBuilder::default()
        .thread_name("lux-log-writer")
        .finish(appender);
    let filter = env_filter();
    let stdout_layer = fmt::layer().json().with_writer(std::io::stdout);
    let file_layer = fmt::layer().json().with_writer(file_writer);
    if tracing_subscriber::registry()
        .with(filter)
        .with(stdout_layer)
        .with(file_layer)
        .try_init()
        .is_err()
    {
        drop(guard);
        return None;
    }
    Some(guard)
}

fn init_stdout() {
    let _ = fmt().json().with_env_filter(env_filter()).try_init();
}

fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("luxd=info,tower_http=info"))
}
