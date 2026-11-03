//! Danmaku subsystem facade.
//!
//! The behaviour is split into focused modules; this module keeps the public
//! paths (`crate::application::danmaku::*`) the rest of the crate already uses:
//!
//! - `path`: sidecar path derivation and media-root path safety.
//! - `xml`: XML validation, entry-level validity statistics and its error type.
//! - `normalize`: canonical rewrite of a stored sidecar (drop invalid entries,
//!   clamp the font size, trim the text).
//! - `sidecar`: atomic write, bounded read, fingerprint, playback reads.
//! - `matching`: match-title candidates, provider base URL validation and the
//!   plugin error-code mapping.
//! - `job`: match job lifecycle, cancellation, retry, the worker pool that calls
//!   the plugin and writes the sidecar, and the per-item worker.
//! - `manual`: the administrator-facing manual match (candidate search, the
//!   single-item job and sidecar removal, ADR-052 P5).

mod job;
mod manual;
mod matching;
mod normalize;
mod path;
mod sidecar;
mod xml;

pub use job::{DEFAULT_DANMAKU_CONCURRENCY, DanmakuMatchJob, DanmakuServiceError};
pub use manual::{DEFAULT_MANUAL_SEARCH_LIMIT, ManualMatchJobStart, ManualMatchTarget};
pub use matching::{ProviderBaseUrl, ProviderUrlError, validate_provider_base_url};
pub use normalize::{DanmakuNormalization, normalize_danmaku_xml};
pub use path::{DanmakuPathError, danmaku_sidecar_path};
pub use sidecar::{DanmakuSidecarScope, DanmakuWriteError, atomic_write_danmaku_xml};
pub use xml::{
    DanmakuXmlError, DanmakuXmlStats, MAX_DANMAKU_COLOR, MAX_DANMAKU_FONT_SIZE,
    MAX_DANMAKU_MODE, MAX_DANMAKU_TEXT_CHARS, MAX_DANMAKU_TIME_SECONDS, MAX_DANMAKU_XML_BYTES,
    MIN_DANMAKU_FONT_SIZE, MIN_DANMAKU_MODE, inspect_danmaku_xml, validate_danmaku_xml,
};

pub(crate) use sidecar::{fingerprint, read_danmaku_sidecar_from_root};

use crate::{
    application::plugins::PluginService, observability::resources::ResourceMetrics,
    storage::Database,
};

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
}
