use super::*;

#[path = "jobs/local_metadata.rs"]
mod local_metadata;
use local_metadata::MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES;
pub(crate) use local_metadata::StoredScanLocalMetadataBackfillPage;
#[path = "jobs/metadata_reidentify.rs"]
mod metadata_reidentify;
use local_metadata::escape_sql_like_pattern;
#[path = "jobs/scan.rs"]
mod scan;
#[cfg(test)]
use scan::prune_sidecar_directories;
#[cfg(test)]
use scan::sidecar_target_query;
#[path = "jobs/shutdown.rs"]
mod shutdown;

#[path = "jobs/strm_probe.rs"]
mod strm_probe;

#[cfg(test)]
pub(crate) use super::media_sources::MAX_MEDIA_SOURCE_DELETE_BATCH_SIZE;
use std::{collections::HashMap, time::Instant};

#[cfg(test)]
#[path = "jobs/tests.rs"]
mod tests;
