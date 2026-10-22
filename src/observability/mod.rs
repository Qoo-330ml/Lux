pub mod logs;
pub mod resources;

use std::{
    path::Path,
    sync::atomic::{AtomicU8, Ordering},
};

use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// 0 = not initialised yet, 1 = file logging available, 2 = stdout only.
static FILE_LOGGING_STATE: AtomicU8 = AtomicU8::new(0);

/// Whether structured logs are also written to the config directory (`None` before init runs).
pub fn file_logging_available() -> Option<bool> {
    match FILE_LOGGING_STATE.load(Ordering::Relaxed) {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

pub async fn init(config_dir: &Path) -> (Option<WorkerGuard>, Option<logs::LogStore>) {
    let log_store = match logs::LogStore::open(config_dir).await {
        Ok(log_store) => log_store,
        Err(error) => {
            // Initialize stdout first so the failure is a structured log line that ships with the
            // other server logs instead of a bare stderr note nobody collects.
            init_stdout();
            tracing::error!(
                %error,
                "Lux file logging unavailable; continuing with stdout logging"
            );
            FILE_LOGGING_STATE.store(2, Ordering::Relaxed);
            return (None, None);
        }
    };

    FILE_LOGGING_STATE.store(1, Ordering::Relaxed);
    let (file_writer, guard) = NonBlockingBuilder::default()
        .thread_name("lux-log-writer")
        .finish(log_store.writer());
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
        return (None, Some(log_store));
    }
    (Some(guard), Some(log_store))
}

fn init_stdout() {
    let _ = fmt().json().with_env_filter(env_filter()).try_init();
}

fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("luxd=info,tower_http=info"))
}
