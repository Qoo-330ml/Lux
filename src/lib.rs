//! Lux server library.

pub mod api;
pub mod config;
pub mod observability;
pub mod storage;

/// Package version exposed by the service.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use api::app;
