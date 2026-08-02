//! Lux server library.

pub mod api;
pub mod application;
pub mod auth;
pub mod config;
pub mod domain;
pub mod observability;
pub mod storage;

/// Package version exposed by the service.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Short source revision embedded by the build script when Git metadata is available.
pub const COMMIT: &str = match option_env!("LUX_GIT_COMMIT") {
    Some(commit) => commit,
    None => "unknown",
};

pub use api::app;
