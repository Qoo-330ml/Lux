//! Danmaku subsystem facade.
//!
//! The behaviour is split into focused modules; this module keeps the public
//! paths (`crate::application::danmaku::*`) the rest of the crate already uses:
//!
//! - [`path`]: sidecar path derivation and media-root path safety.
//! - [`xml`]: XML validation and its error type.
//! - [`sidecar`]: atomic write, bounded read, fingerprint, playback reads.
//! - [`matching`]: match-title candidates, plugin invocation, error-code mapping.
//! - [`job`]: match job lifecycle, cancellation, retry and the worker pool.

mod job;
mod matching;
mod path;
mod sidecar;
mod xml;

pub use job::{DEFAULT_DANMAKU_CONCURRENCY, DanmakuMatchJob, DanmakuServiceError};
pub use matching::{ProviderBaseUrl, ProviderUrlError, validate_provider_base_url};
pub use path::{DanmakuPathError, danmaku_sidecar_path};
pub use sidecar::{DanmakuWriteError, atomic_write_danmaku_xml};
pub use xml::{DanmakuXmlError, MAX_DANMAKU_XML_BYTES, validate_danmaku_xml};

pub(crate) use sidecar::{fingerprint, read_danmaku_sidecar_from_root};

use tokio::fs;

use crate::{
    application::plugins::PluginService, observability::resources::ResourceMetrics,
    storage::Database,
};

use path::safe_danmaku_source_paths;
use sidecar::read_danmaku_file;

/// Danmaku application service.
///
/// The struct (and its private fields) live in the facade so every submodule can
/// add an `impl` block; behaviour stays in the focused modules.
#[derive(Clone)]
pub struct DanmakuService {
    database: Database,
    plugins: Option<PluginService>,
    resources: ResourceMetrics,
}

impl DanmakuService {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            plugins: None,
            resources: ResourceMetrics::new(),
        }
    }

    pub fn with_plugins(mut self, plugins: PluginService) -> Self {
        self.plugins = Some(plugins);
        self
    }

    pub fn with_resource_metrics(mut self, resources: ResourceMetrics) -> Self {
        self.resources = resources;
        self
    }

    pub async fn read_sidecar(
        &self,
        item_id: &str,
    ) -> Result<Option<Vec<u8>>, DanmakuServiceError> {
        self.read_sidecar_for_source(item_id, None).await
    }

    pub async fn read_sidecar_for_source(
        &self,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<Option<Vec<u8>>, DanmakuServiceError> {
        let Some(source) = self
            .database
            .find_danmaku_source_for_item(item_id, source_id)
            .await?
        else {
            return Ok(None);
        };
        let Ok((_, target)) = safe_danmaku_source_paths(&source).await else {
            return Ok(None);
        };
        let Ok(metadata) = fs::symlink_metadata(&target).await else {
            return Ok(None);
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Ok(None);
        }
        let Ok(bytes) = read_danmaku_file(&target).await else {
            return Ok(None);
        };
        if validate_danmaku_xml(&bytes).is_err() {
            return Ok(None);
        }
        Ok(Some(bytes))
    }

    pub async fn read_registered_sidecar_for_source(
        &self,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<Option<Vec<u8>>, DanmakuServiceError> {
        let Some(source) = self
            .database
            .find_registered_danmaku_source_for_item(item_id, source_id)
            .await?
        else {
            return Ok(None);
        };
        let Ok((_, target)) = safe_danmaku_source_paths(&source).await else {
            return Ok(None);
        };
        let Ok(metadata) = fs::symlink_metadata(&target).await else {
            return Ok(None);
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Ok(None);
        }
        let Ok(bytes) = read_danmaku_file(&target).await else {
            return Ok(None);
        };
        if validate_danmaku_xml(&bytes).is_err() {
            return Ok(None);
        }
        Ok(Some(bytes))
    }
}
