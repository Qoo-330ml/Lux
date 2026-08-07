pub mod resources;

use tracing_subscriber::{EnvFilter, fmt};

pub fn init() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("luxd=info,tower_http=info"));

    let _ = fmt().json().with_env_filter(filter).try_init();
}
