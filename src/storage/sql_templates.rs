use crate::config::DatabaseBackend;
use std::{
    collections::HashMap,
    fmt::Write as _,
    sync::{Arc, Mutex},
};

const MAX_CACHED_TEMPLATE_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_CACHED_TEMPLATE_ENTRIES: usize = 256;
const MAX_CACHED_TAILS_PER_TEMPLATE: usize = 4;
const MAX_TEMPLATE_BINDINGS: usize = 65_535;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TemplateBackend {
    Sqlite,
    Postgres,
}

impl From<DatabaseBackend> for TemplateBackend {
    fn from(backend: DatabaseBackend) -> Self {
        match backend {
            DatabaseBackend::Sqlite => Self::Sqlite,
            DatabaseBackend::Postgres => Self::Postgres,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TemplateFamily {
    FilesystemEntries,
    FilesystemEntryRecords,
    ManifestFilesystemBaselines,
    ManifestDiscoveryKnownPaths,
    ManifestPresenceUpdate,
    ManifestPresenceReadback,
    ManifestSeenPathsInsert,
    ManifestDirectoryFrontierInsert,
    ManifestDirectoryFrontierComplete,
}

impl TemplateFamily {
    fn max_in_capacity(self, backend: TemplateBackend) -> Option<u16> {
        match self {
            Self::FilesystemEntries | Self::FilesystemEntryRecords => Some(500),
            Self::ManifestFilesystemBaselines => Some(match backend {
                TemplateBackend::Sqlite => 2_900,
                TemplateBackend::Postgres => 5_000,
            }),
            Self::ManifestDiscoveryKnownPaths
            | Self::ManifestPresenceUpdate
            | Self::ManifestPresenceReadback
            | Self::ManifestSeenPathsInsert
            | Self::ManifestDirectoryFrontierInsert
            | Self::ManifestDirectoryFrontierComplete => None,
        }
    }

    fn exact_row_shape(self, backend: TemplateBackend) -> Option<(u16, usize, usize)> {
        match self {
            Self::ManifestDiscoveryKnownPaths | Self::ManifestSeenPathsInsert => {
                Some((manifest_path_max_rows(backend), 1, 2))
            }
            Self::ManifestPresenceReadback => Some((manifest_path_max_rows(backend), 3, 2)),
            Self::ManifestPresenceUpdate => Some((manifest_path_max_rows(backend), 7, 2)),
            Self::ManifestDirectoryFrontierInsert => Some((200, 3, 0)),
            Self::ManifestDirectoryFrontierComplete => Some((200, 1, 2)),
            Self::FilesystemEntries
            | Self::FilesystemEntryRecords
            | Self::ManifestFilesystemBaselines => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TemplateVersion {
    V1,
    V2,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TemplateShape {
    InCapacity(u16),
    ExactRows(u16),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct TemplateKey {
    family: TemplateFamily,
    backend: TemplateBackend,
    version: TemplateVersion,
    shape: TemplateShape,
    binding_count: usize,
}

impl TemplateKey {
    fn new(
        family: TemplateFamily,
        backend: DatabaseBackend,
        version: TemplateVersion,
        shape: TemplateShape,
    ) -> Result<Self, TemplateError> {
        let backend = TemplateBackend::from(backend);
        let binding_count = match shape {
            TemplateShape::InCapacity(capacity) => {
                let max_capacity = family
                    .max_in_capacity(backend)
                    .ok_or(TemplateError::InvalidShape)?;
                if capacity == 0
                    || capacity > max_capacity
                    || (!capacity.is_power_of_two() && capacity != max_capacity)
                {
                    return Err(TemplateError::InvalidShape);
                }
                usize::from(capacity).saturating_add(1)
            }
            TemplateShape::ExactRows(rows) => {
                let (max_rows, bindings_per_row, fixed_bindings) = family
                    .exact_row_shape(backend)
                    .ok_or(TemplateError::InvalidShape)?;
                if rows == 0 || rows > max_rows {
                    return Err(TemplateError::InvalidShape);
                }
                usize::from(rows)
                    .saturating_mul(bindings_per_row)
                    .saturating_add(fixed_bindings)
            }
        };
        if binding_count > MAX_TEMPLATE_BINDINGS {
            return Err(TemplateError::BindingBudgetExceeded);
        }

        Ok(Self {
            family,
            backend,
            version,
            shape,
            binding_count,
        })
    }

    fn binding_count(self) -> usize {
        self.binding_count
    }

    fn in_capacity(self) -> Option<usize> {
        match self.shape {
            TemplateShape::InCapacity(capacity) => Some(usize::from(capacity)),
            TemplateShape::ExactRows(_) => None,
        }
    }

    fn exact_rows(self) -> Option<usize> {
        match self.shape {
            TemplateShape::ExactRows(rows) => Some(usize::from(rows)),
            TemplateShape::InCapacity(_) => None,
        }
    }

    fn is_full_batch(self) -> bool {
        match self.shape {
            TemplateShape::InCapacity(capacity) => self
                .family
                .max_in_capacity(self.backend)
                .is_some_and(|maximum| capacity == maximum),
            TemplateShape::ExactRows(rows) => self
                .family
                .exact_row_shape(self.backend)
                .is_some_and(|(maximum, _, _)| rows == maximum),
        }
    }

    fn belongs_to_same_template(self, other: Self) -> bool {
        self.family == other.family
            && self.backend == other.backend
            && self.version == other.version
    }
}

fn manifest_path_max_rows(backend: TemplateBackend) -> u16 {
    match backend {
        TemplateBackend::Sqlite => 2_900,
        TemplateBackend::Postgres => 5_000,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemplateError {
    InvalidShape,
    BindingBudgetExceeded,
    BindingCountMismatch { expected: usize, actual: usize },
    FormattingFailed,
}

struct TemplateBuilder {
    backend: TemplateBackend,
    expected_bindings: usize,
    emitted_bindings: usize,
    sql: String,
}

impl TemplateBuilder {
    fn new(key: TemplateKey) -> Self {
        let placeholder_bytes: usize = match key.backend {
            TemplateBackend::Sqlite => 1,
            TemplateBackend::Postgres => 6,
        };
        let initial_capacity = key
            .binding_count
            .saturating_mul(placeholder_bytes.saturating_add(2))
            .saturating_add(128);
        Self {
            backend: key.backend,
            expected_bindings: key.binding_count,
            emitted_bindings: 0,
            sql: String::with_capacity(initial_capacity),
        }
    }

    fn push_static(&mut self, sql: &'static str) {
        self.sql.push_str(sql);
    }

    fn push_bind(&mut self) -> Result<(), TemplateError> {
        if self.emitted_bindings >= self.expected_bindings {
            return Err(TemplateError::BindingBudgetExceeded);
        }
        match self.backend {
            TemplateBackend::Sqlite => self.sql.push('?'),
            TemplateBackend::Postgres => {
                self.sql.push('$');
                write!(&mut self.sql, "{}", self.emitted_bindings + 1)
                    .map_err(|_| TemplateError::FormattingFailed)?;
            }
        }
        self.emitted_bindings += 1;
        Ok(())
    }

    fn finish(self) -> Result<Arc<str>, TemplateError> {
        if self.emitted_bindings != self.expected_bindings {
            return Err(TemplateError::BindingCountMismatch {
                expected: self.expected_bindings,
                actual: self.emitted_bindings,
            });
        }
        Ok(Arc::<str>::from(self.sql))
    }
}

struct CachedTemplate {
    sql: Arc<str>,
    last_used: u64,
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<TemplateKey, CachedTemplate>,
    cached_text_bytes: usize,
    clock: u64,
}

pub(crate) struct SqlTemplateCache {
    state: Mutex<CacheState>,
    max_text_bytes: usize,
    max_entries: usize,
}

impl Default for SqlTemplateCache {
    fn default() -> Self {
        Self::with_limits(MAX_CACHED_TEMPLATE_TEXT_BYTES, MAX_CACHED_TEMPLATE_ENTRIES)
    }
}

impl SqlTemplateCache {
    fn with_limits(max_text_bytes: usize, max_entries: usize) -> Self {
        Self {
            state: Mutex::new(CacheState::default()),
            max_text_bytes,
            max_entries,
        }
    }

    fn get_or_build(
        &self,
        key: TemplateKey,
        build: impl FnOnce(&mut TemplateBuilder) -> Result<(), TemplateError>,
    ) -> Result<Arc<str>, TemplateError> {
        if let Ok(mut state) = self.state.lock() {
            state.clock = state.clock.wrapping_add(1);
            let last_used = state.clock;
            if let Some(entry) = state.entries.get_mut(&key) {
                entry.last_used = last_used;
                return Ok(Arc::clone(&entry.sql));
            }
        }

        let mut builder = TemplateBuilder::new(key);
        build(&mut builder)?;
        let sql = builder.finish()?;
        if sql.len() > self.max_text_bytes || self.max_entries == 0 {
            return Ok(sql);
        }

        let Ok(mut state) = self.state.lock() else {
            return Ok(sql);
        };
        state.clock = state.clock.wrapping_add(1);
        let last_used = state.clock;
        if let Some(entry) = state.entries.get_mut(&key) {
            entry.last_used = last_used;
            return Ok(Arc::clone(&entry.sql));
        }

        if !key.is_full_batch() {
            let mut tail_count = state
                .entries
                .keys()
                .filter(|candidate| {
                    candidate.belongs_to_same_template(key) && !candidate.is_full_batch()
                })
                .count();
            while tail_count >= MAX_CACHED_TAILS_PER_TEMPLATE {
                let oldest_tail_key = state
                    .entries
                    .iter()
                    .filter(|(candidate, _)| {
                        candidate.belongs_to_same_template(key) && !candidate.is_full_batch()
                    })
                    .min_by_key(|(_, entry)| entry.last_used)
                    .map(|(candidate, _)| *candidate);
                let Some(oldest_tail_key) = oldest_tail_key else {
                    break;
                };
                if let Some(removed) = state.entries.remove(&oldest_tail_key) {
                    state.cached_text_bytes =
                        state.cached_text_bytes.saturating_sub(removed.sql.len());
                    tail_count -= 1;
                }
            }
        }

        while state.entries.len() >= self.max_entries
            || state.cached_text_bytes.saturating_add(sql.len()) > self.max_text_bytes
        {
            let oldest_key = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key);
            let Some(oldest_key) = oldest_key else {
                break;
            };
            if let Some(removed) = state.entries.remove(&oldest_key) {
                state.cached_text_bytes = state.cached_text_bytes.saturating_sub(removed.sql.len());
            }
        }

        state.cached_text_bytes = state.cached_text_bytes.saturating_add(sql.len());
        state.entries.insert(
            key,
            CachedTemplate {
                sql: Arc::clone(&sql),
                last_used,
            },
        );
        Ok(sql)
    }

    #[cfg(test)]
    pub(crate) fn entry_count(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.entries.len())
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn cached_text_bytes(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.cached_text_bytes)
            .unwrap_or_default()
    }
}

pub(crate) fn filesystem_entries_by_paths_template(
    cache: &SqlTemplateCache,
    backend: DatabaseBackend,
    in_capacity: u16,
) -> Result<Arc<str>, TemplateError> {
    let key = TemplateKey::new(
        TemplateFamily::FilesystemEntries,
        backend,
        TemplateVersion::V1,
        TemplateShape::InCapacity(in_capacity),
    )?;
    cache.get_or_build(key, |builder| {
        let capacity = key.in_capacity().ok_or(TemplateError::InvalidShape)?;
        builder.push_static("SELECT id FROM filesystem_entries WHERE library_root_id = ");
        builder.push_bind()?;
        builder.push_static(" AND relative_path IN (");
        for index in 0..capacity {
            if index > 0 {
                builder.push_static(", ");
            }
            builder.push_bind()?;
        }
        builder.push_static(")");
        Ok(())
    })
}

pub(crate) fn filesystem_entry_records_by_paths_template(
    cache: &SqlTemplateCache,
    backend: DatabaseBackend,
    in_capacity: u16,
) -> Result<Arc<str>, TemplateError> {
    let key = TemplateKey::new(
        TemplateFamily::FilesystemEntryRecords,
        backend,
        TemplateVersion::V1,
        TemplateShape::InCapacity(in_capacity),
    )?;
    cache.get_or_build(key, |builder| {
        let capacity = key.in_capacity().ok_or(TemplateError::InvalidShape)?;
        builder.push_static(
            r#"SELECT fe.id, fe.relative_path, fe.entry_kind, fe.size, fe.modified_at,
                    fe.device, fe.inode, fe.fingerprint, fe.last_seen_generation,
                    fe.is_missing, ms.item_id,
                    CASE WHEN parent.removed_at IS NULL THEN parent.identity_key END
                        AS parent_identity_key,
                    item.item_type AS item_type,
                    CASE WHEN item.removed_at IS NULL THEN item.identity_key END
                        AS item_identity_key,
                    series.provider_ids_json AS series_provider_ids_json
             FROM filesystem_entries fe
             LEFT JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
             LEFT JOIN media_items item ON item.id = ms.item_id
             LEFT JOIN media_items parent ON parent.id = item.parent_id
             LEFT JOIN media_items series ON series.id = item.series_id
             WHERE fe.library_root_id = "#,
        );
        builder.push_bind()?;
        builder.push_static(" AND fe.relative_path IN (");
        for index in 0..capacity {
            if index > 0 {
                builder.push_static(", ");
            }
            builder.push_bind()?;
        }
        builder.push_static(")");
        Ok(())
    })
}

pub(crate) fn manifest_filesystem_baselines_by_paths_template(
    cache: &SqlTemplateCache,
    backend: DatabaseBackend,
    in_capacity: u16,
) -> Result<Arc<str>, TemplateError> {
    let key = TemplateKey::new(
        TemplateFamily::ManifestFilesystemBaselines,
        backend,
        TemplateVersion::V1,
        TemplateShape::InCapacity(in_capacity),
    )?;
    cache.get_or_build(key, |builder| {
        let capacity = key.in_capacity().ok_or(TemplateError::InvalidShape)?;
        builder.push_static(
            "SELECT id, relative_path, entry_kind, size, modified_at, device, inode, \
             fingerprint, is_missing FROM filesystem_entries WHERE library_root_id = ",
        );
        builder.push_bind()?;
        builder.push_static(" AND relative_path IN (");
        for index in 0..capacity {
            if index > 0 {
                builder.push_static(", ");
            }
            builder.push_bind()?;
        }
        builder.push_static(")");
        Ok(())
    })
}

fn exact_rows_key(
    family: TemplateFamily,
    backend: DatabaseBackend,
    version: TemplateVersion,
    row_count: usize,
) -> Result<TemplateKey, TemplateError> {
    let rows = u16::try_from(row_count).map_err(|_| TemplateError::InvalidShape)?;
    TemplateKey::new(family, backend, version, TemplateShape::ExactRows(rows))
}

fn push_value_rows(
    builder: &mut TemplateBuilder,
    row_count: usize,
    binds_per_row: usize,
) -> Result<(), TemplateError> {
    for row in 0..row_count {
        if row > 0 {
            builder.push_static(", ");
        }
        builder.push_static("(");
        for binding in 0..binds_per_row {
            if binding > 0 {
                builder.push_static(", ");
            }
            builder.push_bind()?;
        }
        builder.push_static(")");
    }
    Ok(())
}

pub(crate) fn manifest_discovery_known_paths_template(
    cache: &SqlTemplateCache,
    backend: DatabaseBackend,
    discovery_format_version: i64,
    row_count: usize,
) -> Result<Arc<str>, TemplateError> {
    let is_v3 = discovery_format_version == 3;
    let key = exact_rows_key(
        TemplateFamily::ManifestDiscoveryKnownPaths,
        backend,
        if is_v3 {
            TemplateVersion::V2
        } else {
            TemplateVersion::V1
        },
        row_count,
    )?;
    cache.get_or_build(key, |builder| {
        let rows = key.exact_rows().ok_or(TemplateError::InvalidShape)?;
        if is_v3 {
            builder.push_static("WITH incoming(relative_path) AS (VALUES ");
            push_value_rows(builder, rows, 1)?;
            builder.push_static(
                ") SELECT incoming.relative_path FROM incoming WHERE EXISTS (SELECT 1 FROM filesystem_entries entry WHERE entry.library_root_id = ",
            );
            builder.push_bind()?;
            builder.push_static(
                " AND entry.relative_path = incoming.relative_path AND entry.last_seen_generation = ",
            );
            builder.push_bind()?;
            builder.push_static(")");
        } else {
            builder.push_static(
                "SELECT DISTINCT relative_path FROM scan_manifest_entries WHERE manifest_id = ",
            );
            builder.push_bind()?;
            builder.push_static(" AND library_root_id = ");
            builder.push_bind()?;
            builder.push_static(" AND entry_kind = 'FILE' AND relative_path IN (");
            for row in 0..rows {
                if row > 0 {
                    builder.push_static(", ");
                }
                builder.push_bind()?;
            }
            builder.push_static(")");
        }
        Ok(())
    })
}

pub(crate) fn manifest_presence_update_template(
    cache: &SqlTemplateCache,
    backend: DatabaseBackend,
    row_count: usize,
) -> Result<Arc<str>, TemplateError> {
    let key = exact_rows_key(
        TemplateFamily::ManifestPresenceUpdate,
        backend,
        TemplateVersion::V1,
        row_count,
    )?;
    cache.get_or_build(key, |builder| {
        let rows = key.exact_rows().ok_or(TemplateError::InvalidShape)?;
        if backend == DatabaseBackend::Postgres {
            builder.push_static(
                "WITH incoming(filesystem_entry_id, relative_path, fingerprint, size, modified_at, device, inode) AS (VALUES ",
            );
            push_value_rows(builder, rows, 7)?;
            builder.push_static(
                ") UPDATE filesystem_entries AS current SET size = incoming.size, modified_at = incoming.modified_at, device = incoming.device, inode = incoming.inode, last_seen_generation = ",
            );
            builder.push_bind()?;
            builder.push_static(
                ", last_seen_change_kind = NULL FROM incoming WHERE current.id = incoming.filesystem_entry_id AND current.library_root_id = ",
            );
            builder.push_bind()?;
            builder.push_static(
                " AND current.relative_path = incoming.relative_path AND current.fingerprint = incoming.fingerprint AND current.entry_kind = 'FILE' AND current.is_missing = 0 AND current.entry_kind = 'FILE' AND current.is_missing = 0",
            );
        } else {
            builder.push_static(
                "WITH incoming(filesystem_entry_id, relative_path, fingerprint, size, modified_at, device, inode) AS (VALUES ",
            );
            push_value_rows(builder, rows, 7)?;
            builder.push_static(
                "), matching(filesystem_entry_id) AS (SELECT incoming.filesystem_entry_id FROM incoming JOIN filesystem_entries current ON current.id = incoming.filesystem_entry_id AND current.relative_path = incoming.relative_path AND current.fingerprint = incoming.fingerprint WHERE current.library_root_id = ",
            );
            builder.push_bind()?;
            builder.push_static(
                " AND current.entry_kind = 'FILE' AND current.is_missing = 0) UPDATE filesystem_entries SET size = (SELECT size FROM incoming WHERE filesystem_entry_id = filesystem_entries.id), modified_at = (SELECT modified_at FROM incoming WHERE filesystem_entry_id = filesystem_entries.id), device = (SELECT device FROM incoming WHERE filesystem_entry_id = filesystem_entries.id), inode = (SELECT inode FROM incoming WHERE filesystem_entry_id = filesystem_entries.id), last_seen_generation = ",
            );
            builder.push_bind()?;
            builder.push_static(
                ", last_seen_change_kind = NULL WHERE id IN (SELECT filesystem_entry_id FROM matching)",
            );
        }
        Ok(())
    })
}

pub(crate) fn manifest_presence_readback_template(
    cache: &SqlTemplateCache,
    backend: DatabaseBackend,
    row_count: usize,
) -> Result<Arc<str>, TemplateError> {
    let key = exact_rows_key(
        TemplateFamily::ManifestPresenceReadback,
        backend,
        TemplateVersion::V1,
        row_count,
    )?;
    cache.get_or_build(key, |builder| {
        let rows = key.exact_rows().ok_or(TemplateError::InvalidShape)?;
        builder.push_static(
            "WITH incoming(filesystem_entry_id, relative_path, fingerprint) AS (VALUES ",
        );
        push_value_rows(builder, rows, 3)?;
        builder.push_static(
            ") SELECT incoming.relative_path FROM incoming JOIN filesystem_entries entry ON entry.id = incoming.filesystem_entry_id AND entry.relative_path = incoming.relative_path WHERE entry.library_root_id = ",
        );
        builder.push_bind()?;
        builder.push_static(
            " AND entry.entry_kind = 'FILE' AND entry.is_missing = 0 AND entry.last_seen_generation = ",
        );
        builder.push_bind()?;
        builder.push_static(" AND entry.fingerprint = incoming.fingerprint");
        Ok(())
    })
}

pub(crate) fn manifest_seen_paths_insert_template(
    cache: &SqlTemplateCache,
    backend: DatabaseBackend,
    row_count: usize,
) -> Result<Arc<str>, TemplateError> {
    let key = exact_rows_key(
        TemplateFamily::ManifestSeenPathsInsert,
        backend,
        TemplateVersion::V1,
        row_count,
    )?;
    cache.get_or_build(key, |builder| {
        let rows = key.exact_rows().ok_or(TemplateError::InvalidShape)?;
        builder.push_static("WITH incoming_seen_paths(relative_path) AS (VALUES ");
        push_value_rows(builder, rows, 1)?;
        builder.push_static(
            ") INSERT INTO scan_manifest_seen_paths (manifest_id, library_root_id, relative_path) SELECT ",
        );
        builder.push_bind()?;
        builder.push_static(", ");
        builder.push_bind()?;
        builder.push_static(
            ", incoming_seen_paths.relative_path FROM incoming_seen_paths WHERE TRUE ON CONFLICT(manifest_id, library_root_id, relative_path) DO NOTHING",
        );
        Ok(())
    })
}

pub(crate) fn manifest_directory_frontier_insert_template(
    cache: &SqlTemplateCache,
    backend: DatabaseBackend,
    row_count: usize,
) -> Result<Arc<str>, TemplateError> {
    let key = exact_rows_key(
        TemplateFamily::ManifestDirectoryFrontierInsert,
        backend,
        TemplateVersion::V1,
        row_count,
    )?;
    cache.get_or_build(key, |builder| {
        let rows = key.exact_rows().ok_or(TemplateError::InvalidShape)?;
        builder.push_static(
            "INSERT INTO scan_manifest_directories (manifest_id, library_root_id, relative_path, state) VALUES ",
        );
        for row in 0..rows {
            if row > 0 {
                builder.push_static(", ");
            }
            builder.push_static("(");
            builder.push_bind()?;
            builder.push_static(", ");
            builder.push_bind()?;
            builder.push_static(", ");
            builder.push_bind()?;
            builder.push_static(", 'PENDING')");
        }
        builder.push_static(
            " ON CONFLICT(manifest_id, library_root_id, relative_path) DO NOTHING",
        );
        Ok(())
    })
}

pub(crate) fn manifest_directory_frontier_complete_template(
    cache: &SqlTemplateCache,
    backend: DatabaseBackend,
    row_count: usize,
) -> Result<Arc<str>, TemplateError> {
    let key = exact_rows_key(
        TemplateFamily::ManifestDirectoryFrontierComplete,
        backend,
        TemplateVersion::V1,
        row_count,
    )?;
    cache.get_or_build(key, |builder| {
        let rows = key.exact_rows().ok_or(TemplateError::InvalidShape)?;
        builder.push_static(
            "UPDATE scan_manifest_directories SET state = 'COMPLETE', error = NULL, updated_at = unixepoch() WHERE manifest_id = ",
        );
        builder.push_bind()?;
        builder.push_static(" AND library_root_id = ");
        builder.push_bind()?;
        builder.push_static(
            " AND state <> 'COMPLETE' AND relative_path IN (",
        );
        for row in 0..rows {
            if row > 0 {
                builder.push_static(", ");
            }
            builder.push_bind()?;
        }
        builder.push_static(")");
        Ok(())
    })
}

pub(crate) fn filesystem_entries_by_paths_capacity(
    backend: DatabaseBackend,
    path_count: usize,
) -> Result<u16, TemplateError> {
    in_capacity_for_family(TemplateFamily::FilesystemEntryRecords, backend, path_count)
}

pub(crate) fn manifest_filesystem_baselines_by_paths_capacity(
    backend: DatabaseBackend,
    path_count: usize,
) -> Result<u16, TemplateError> {
    in_capacity_for_family(
        TemplateFamily::ManifestFilesystemBaselines,
        backend,
        path_count,
    )
}

fn in_capacity_for_family(
    family: TemplateFamily,
    backend: DatabaseBackend,
    path_count: usize,
) -> Result<u16, TemplateError> {
    let max_capacity = usize::from(
        family
            .max_in_capacity(backend.into())
            .ok_or(TemplateError::InvalidShape)?,
    );
    if path_count == 0 || path_count > max_capacity {
        return Err(TemplateError::InvalidShape);
    }
    let bucket = path_count
        .checked_next_power_of_two()
        .unwrap_or(max_capacity)
        .min(max_capacity);
    u16::try_from(bucket).map_err(|_| TemplateError::InvalidShape)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DatabaseBackend;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    use std::thread;

    fn key(backend: DatabaseBackend, capacity: u16) -> TemplateKey {
        TemplateKey::new(
            TemplateFamily::FilesystemEntries,
            backend,
            TemplateVersion::V1,
            TemplateShape::InCapacity(capacity),
        )
        .expect("valid filesystem path capacity")
    }

    fn build_path_query(
        cache: &SqlTemplateCache,
        key: TemplateKey,
    ) -> Result<Arc<str>, TemplateError> {
        filesystem_entries_by_paths_template(
            cache,
            match key.backend {
                TemplateBackend::Sqlite => DatabaseBackend::Sqlite,
                TemplateBackend::Postgres => DatabaseBackend::Postgres,
            },
            u16::try_from(key.in_capacity().ok_or(TemplateError::InvalidShape)?)
                .map_err(|_| TemplateError::InvalidShape)?,
        )
    }

    #[test]
    fn backend_templates_emit_complete_sql_and_numbered_postgres_bindings() {
        let cache = SqlTemplateCache::default();
        let sqlite =
            build_path_query(&cache, key(DatabaseBackend::Sqlite, 4)).expect("SQLite template");
        assert_eq!(
            sqlite.as_ref(),
            "SELECT id FROM filesystem_entries WHERE library_root_id = ? AND relative_path IN (?, ?, ?, ?)"
        );

        let postgres = build_path_query(&cache, key(DatabaseBackend::Postgres, 16))
            .expect("PostgreSQL template");
        assert_eq!(
            postgres.as_ref(),
            "SELECT id FROM filesystem_entries WHERE library_root_id = $1 AND relative_path IN ($2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)"
        );
    }

    #[test]
    fn filesystem_entry_record_template_keeps_the_full_projection_and_bind_order() {
        let cache = SqlTemplateCache::default();
        let sqlite = filesystem_entry_records_by_paths_template(&cache, DatabaseBackend::Sqlite, 2)
            .expect("SQLite filesystem row template");
        assert_eq!(
            sqlite.as_ref(),
            r#"SELECT fe.id, fe.relative_path, fe.entry_kind, fe.size, fe.modified_at,
                    fe.device, fe.inode, fe.fingerprint, fe.last_seen_generation,
                    fe.is_missing, ms.item_id,
                    CASE WHEN parent.removed_at IS NULL THEN parent.identity_key END
                        AS parent_identity_key,
                    item.item_type AS item_type,
                    CASE WHEN item.removed_at IS NULL THEN item.identity_key END
                        AS item_identity_key,
                    series.provider_ids_json AS series_provider_ids_json
             FROM filesystem_entries fe
             LEFT JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
             LEFT JOIN media_items item ON item.id = ms.item_id
             LEFT JOIN media_items parent ON parent.id = item.parent_id
             LEFT JOIN media_items series ON series.id = item.series_id
             WHERE fe.library_root_id = ? AND fe.relative_path IN (?, ?)"#
        );

        let postgres =
            filesystem_entry_records_by_paths_template(&cache, DatabaseBackend::Postgres, 2)
                .expect("PostgreSQL filesystem row template");
        assert!(
            postgres.ends_with("WHERE fe.library_root_id = $1 AND fe.relative_path IN ($2, $3)")
        );
        assert!(postgres.contains("series.provider_ids_json AS series_provider_ids_json"));
    }

    #[test]
    fn manifest_baseline_templates_respect_backend_limits_and_bind_order() {
        let cache = SqlTemplateCache::default();
        let sqlite =
            manifest_filesystem_baselines_by_paths_template(&cache, DatabaseBackend::Sqlite, 4)
                .expect("SQLite manifest baseline template");
        assert_eq!(
            sqlite.as_ref(),
            "SELECT id, relative_path, entry_kind, size, modified_at, device, inode, fingerprint, is_missing FROM filesystem_entries WHERE library_root_id = ? AND relative_path IN (?, ?, ?, ?)"
        );

        let postgres =
            manifest_filesystem_baselines_by_paths_template(&cache, DatabaseBackend::Postgres, 4)
                .expect("PostgreSQL manifest baseline template");
        assert!(
            postgres.ends_with("WHERE library_root_id = $1 AND relative_path IN ($2, $3, $4, $5)")
        );

        let sqlite_max =
            manifest_filesystem_baselines_by_paths_template(&cache, DatabaseBackend::Sqlite, 2_900)
                .expect("SQLite maximum manifest baseline template");
        assert_eq!(sqlite_max.matches('?').count(), 2_901);
        let postgres_max = manifest_filesystem_baselines_by_paths_template(
            &cache,
            DatabaseBackend::Postgres,
            5_000,
        )
        .expect("PostgreSQL maximum manifest baseline template");
        assert!(postgres_max.ends_with("$5001)"));
        assert_eq!(postgres_max.matches('$').count(), 5_001);

        assert_eq!(
            manifest_filesystem_baselines_by_paths_capacity(DatabaseBackend::Sqlite, 3),
            Ok(4)
        );
        assert_eq!(
            manifest_filesystem_baselines_by_paths_capacity(DatabaseBackend::Sqlite, 2_049),
            Ok(2_900)
        );
        assert_eq!(
            manifest_filesystem_baselines_by_paths_capacity(DatabaseBackend::Sqlite, 2_901),
            Err(TemplateError::InvalidShape)
        );
        assert_eq!(
            manifest_filesystem_baselines_by_paths_capacity(DatabaseBackend::Postgres, 4_097),
            Ok(5_000)
        );
        assert_eq!(
            manifest_filesystem_baselines_by_paths_capacity(DatabaseBackend::Postgres, 0),
            Err(TemplateError::InvalidShape)
        );
        assert_eq!(
            manifest_filesystem_baselines_by_paths_capacity(DatabaseBackend::Postgres, 5_001),
            Err(TemplateError::InvalidShape)
        );
    }

    #[test]
    fn invalid_shapes_and_placeholder_budget_are_rejected_before_publication() {
        let cache = SqlTemplateCache::default();
        assert!(
            TemplateKey::new(
                TemplateFamily::FilesystemEntries,
                DatabaseBackend::Sqlite,
                TemplateVersion::V1,
                TemplateShape::InCapacity(0),
            )
            .is_err()
        );
        assert!(
            TemplateKey::new(
                TemplateFamily::FilesystemEntries,
                DatabaseBackend::Sqlite,
                TemplateVersion::V1,
                TemplateShape::InCapacity(3),
            )
            .is_err()
        );
        assert!(
            TemplateKey::new(
                TemplateFamily::FilesystemEntries,
                DatabaseBackend::Postgres,
                TemplateVersion::V1,
                TemplateShape::InCapacity(1024),
            )
            .is_err()
        );
        assert!(
            TemplateKey::new(
                TemplateFamily::FilesystemEntries,
                DatabaseBackend::Postgres,
                TemplateVersion::V1,
                TemplateShape::InCapacity(512),
            )
            .is_err()
        );
        assert!(key(DatabaseBackend::Postgres, 500).is_full_batch());

        let missing_bind = key(DatabaseBackend::Postgres, 2);
        let error = cache
            .get_or_build(missing_bind, |builder| {
                builder.push_static("SELECT $");
                builder.push_bind()?;
                Ok(())
            })
            .expect_err("missing bindings must fail");
        assert!(matches!(error, TemplateError::BindingCountMismatch { .. }));

        let extra_bind = key(DatabaseBackend::Postgres, 2);
        let error = cache
            .get_or_build(extra_bind, |builder| {
                builder.push_static("SELECT ");
                builder.push_bind()?;
                builder.push_static(", ");
                builder.push_bind()?;
                builder.push_static(", ");
                builder.push_bind()?;
                builder.push_static(", ");
                builder.push_bind()?;
                Ok(())
            })
            .expect_err("extra bindings must fail");
        assert_eq!(error, TemplateError::BindingBudgetExceeded);
        assert_eq!(cache.entry_count(), 0);
    }

    #[test]
    fn in_capacity_uses_power_of_two_buckets_and_the_original_batch_limit() {
        assert_eq!(
            filesystem_entries_by_paths_capacity(DatabaseBackend::Sqlite, 1),
            Ok(1)
        );
        assert_eq!(
            filesystem_entries_by_paths_capacity(DatabaseBackend::Postgres, 3),
            Ok(4)
        );
        assert_eq!(
            filesystem_entries_by_paths_capacity(DatabaseBackend::Sqlite, 257),
            Ok(500)
        );
        assert_eq!(
            filesystem_entries_by_paths_capacity(DatabaseBackend::Postgres, 500),
            Ok(500)
        );
        assert_eq!(
            filesystem_entries_by_paths_capacity(DatabaseBackend::Sqlite, 0),
            Err(TemplateError::InvalidShape)
        );
        assert_eq!(
            filesystem_entries_by_paths_capacity(DatabaseBackend::Postgres, 501),
            Err(TemplateError::InvalidShape)
        );
    }

    #[test]
    fn repeated_keys_reuse_the_same_arc_and_skip_the_builder() {
        let cache = SqlTemplateCache::default();
        let query_key = key(DatabaseBackend::Postgres, 8);
        let builds = AtomicUsize::new(0);
        let first = cache
            .get_or_build(query_key, |builder| {
                builds.fetch_add(1, Ordering::Relaxed);
                builder.push_static("SELECT ");
                for index in 0..query_key.binding_count() {
                    if index > 0 {
                        builder.push_static(", ");
                    }
                    builder.push_bind()?;
                }
                Ok(())
            })
            .expect("first build");
        let second = cache
            .get_or_build(query_key, |_| {
                builds.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })
            .expect("cache hit");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(builds.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn concurrent_misses_publish_one_shared_arc() {
        let cache = Arc::new(SqlTemplateCache::default());
        let start = Arc::new(Barrier::new(9));
        let builds = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();

        for _ in 0..8 {
            let cache = Arc::clone(&cache);
            let start = Arc::clone(&start);
            let builds = Arc::clone(&builds);
            workers.push(thread::spawn(move || {
                start.wait();
                let query_key = key(DatabaseBackend::Postgres, 64);
                cache
                    .get_or_build(query_key, |builder| {
                        builds.fetch_add(1, Ordering::Relaxed);
                        builder.push_static("SELECT ");
                        for index in 0..query_key.binding_count() {
                            if index > 0 {
                                builder.push_static(", ");
                            }
                            builder.push_bind()?;
                        }
                        Ok(())
                    })
                    .expect("template build or publication")
            }));
        }
        start.wait();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker completed"))
            .collect::<Vec<_>>();

        assert!(
            results
                .iter()
                .all(|result| Arc::ptr_eq(&results[0], result))
        );
        assert!((1..=8).contains(&builds.load(Ordering::Relaxed)));
    }

    #[test]
    fn builder_runs_without_holding_the_cache_lock() {
        let cache = SqlTemplateCache::default();
        let query_key = key(DatabaseBackend::Postgres, 2);
        cache
            .get_or_build(query_key, |builder| {
                let lock = cache
                    .state
                    .try_lock()
                    .expect("builder runs outside the lock");
                drop(lock);
                builder.push_static("SELECT ");
                builder.push_bind()?;
                builder.push_static(", ");
                builder.push_bind()?;
                builder.push_static(", ");
                builder.push_bind()?;
                Ok(())
            })
            .expect("template build");
    }

    #[test]
    fn cache_enforces_text_and_entry_budgets_and_skips_oversized_text() {
        let cache = SqlTemplateCache::with_limits(8, 2);
        let oversized_key = key(DatabaseBackend::Postgres, 2);
        let builds = AtomicUsize::new(0);
        let build_oversized = || {
            cache.get_or_build(oversized_key, |builder| {
                builds.fetch_add(1, Ordering::Relaxed);
                builder.push_static("12345678 ");
                builder.push_bind()?;
                builder.push_static(", ");
                builder.push_bind()?;
                builder.push_static(", ");
                builder.push_bind()?;
                Ok(())
            })
        };
        let first = build_oversized().expect("oversized template remains usable");
        let second = build_oversized().expect("uncached oversized template remains usable");
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(builds.load(Ordering::Relaxed), 2);
        assert_eq!(cache.cached_text_bytes(), 0);

        let bounded_cache = SqlTemplateCache::with_limits(128, 2);
        let build_key = |capacity| {
            let query_key = key(DatabaseBackend::Postgres, capacity);
            bounded_cache
                .get_or_build(query_key, |builder| {
                    builder.push_static("SELECT ");
                    for index in 0..query_key.binding_count() {
                        if index > 0 {
                            builder.push_static(", ");
                        }
                        builder.push_bind()?;
                    }
                    Ok(())
                })
                .expect("bounded template")
        };
        build_key(1);
        build_key(2);
        build_key(4);
        assert_eq!(bounded_cache.entry_count(), 2);
        assert!(bounded_cache.cached_text_bytes() <= 128);
    }

    #[test]
    fn each_template_keeps_one_full_batch_and_at_most_four_tail_shapes() {
        let cache = SqlTemplateCache::default();
        let full_batch = key(DatabaseBackend::Postgres, 500);
        for query_key in [full_batch, key(DatabaseBackend::Postgres, 1)]
            .into_iter()
            .chain([2, 4, 8, 16].map(|capacity| key(DatabaseBackend::Postgres, capacity)))
        {
            cache
                .get_or_build(query_key, |builder| {
                    builder.push_static("SELECT ");
                    for index in 0..query_key.binding_count() {
                        if index > 0 {
                            builder.push_static(", ");
                        }
                        builder.push_bind()?;
                    }
                    Ok(())
                })
                .expect("valid finite shape");
        }

        let state = cache.state.lock().expect("cache state");
        assert_eq!(state.entries.len(), 5);
        assert!(state.entries.contains_key(&full_batch));
        assert!(
            !state
                .entries
                .contains_key(&key(DatabaseBackend::Postgres, 1))
        );
        assert!(state.cached_text_bytes <= MAX_CACHED_TEMPLATE_TEXT_BYTES);
    }

    #[test]
    fn manifest_discovery_templates_keep_versions_and_bind_order() {
        let cache = SqlTemplateCache::default();
        let sqlite_v3 =
            manifest_discovery_known_paths_template(&cache, DatabaseBackend::Sqlite, 3, 2)
                .expect("SQLite format-3 known paths");
        assert_eq!(
            sqlite_v3.as_ref(),
            "WITH incoming(relative_path) AS (VALUES (?), (?)) SELECT incoming.relative_path FROM incoming WHERE EXISTS (SELECT 1 FROM filesystem_entries entry WHERE entry.library_root_id = ? AND entry.relative_path = incoming.relative_path AND entry.last_seen_generation = ?)"
        );

        let postgres_legacy =
            manifest_discovery_known_paths_template(&cache, DatabaseBackend::Postgres, 2, 3)
                .expect("PostgreSQL legacy known paths");
        assert_eq!(
            postgres_legacy.as_ref(),
            "SELECT DISTINCT relative_path FROM scan_manifest_entries WHERE manifest_id = $1 AND library_root_id = $2 AND entry_kind = 'FILE' AND relative_path IN ($3, $4, $5)"
        );

        let postgres_v3 =
            manifest_discovery_known_paths_template(&cache, DatabaseBackend::Postgres, 3, 2)
                .expect("PostgreSQL format-3 known paths");
        assert!(postgres_v3.contains("VALUES ($1), ($2)"));
        assert!(postgres_v3.ends_with(
            "entry.library_root_id = $3 AND entry.relative_path = incoming.relative_path AND entry.last_seen_generation = $4)"
        ));
        assert!(!Arc::ptr_eq(&sqlite_v3, &postgres_v3));
    }

    #[test]
    fn manifest_presence_templates_keep_exact_rows_and_postgres_numbering() {
        let cache = SqlTemplateCache::default();
        let sqlite_update = manifest_presence_update_template(&cache, DatabaseBackend::Sqlite, 2)
            .expect("SQLite presence update");
        assert!(sqlite_update.starts_with(
            "WITH incoming(filesystem_entry_id, relative_path, fingerprint, size, modified_at, device, inode) AS (VALUES (?, ?, ?, ?, ?, ?, ?), (?, ?, ?, ?, ?, ?, ?)), matching(filesystem_entry_id) AS ("
        ));
        assert!(sqlite_update.ends_with(
            "last_seen_generation = ?, last_seen_change_kind = NULL WHERE id IN (SELECT filesystem_entry_id FROM matching)"
        ));

        let postgres_update =
            manifest_presence_update_template(&cache, DatabaseBackend::Postgres, 2)
                .expect("PostgreSQL presence update");
        assert!(postgres_update.starts_with(
            "WITH incoming(filesystem_entry_id, relative_path, fingerprint, size, modified_at, device, inode) AS (VALUES ($1, $2, $3, $4, $5, $6, $7), ($8, $9, $10, $11, $12, $13, $14))"
        ));
        assert!(postgres_update.ends_with(
            "last_seen_generation = $15, last_seen_change_kind = NULL FROM incoming WHERE current.id = incoming.filesystem_entry_id AND current.library_root_id = $16 AND current.relative_path = incoming.relative_path AND current.fingerprint = incoming.fingerprint AND current.entry_kind = 'FILE' AND current.is_missing = 0 AND current.entry_kind = 'FILE' AND current.is_missing = 0"
        ));

        let readback = manifest_presence_readback_template(&cache, DatabaseBackend::Postgres, 2)
            .expect("PostgreSQL presence readback");
        assert_eq!(
            readback.as_ref(),
            "WITH incoming(filesystem_entry_id, relative_path, fingerprint) AS (VALUES ($1, $2, $3), ($4, $5, $6)) SELECT incoming.relative_path FROM incoming JOIN filesystem_entries entry ON entry.id = incoming.filesystem_entry_id AND entry.relative_path = incoming.relative_path WHERE entry.library_root_id = $7 AND entry.entry_kind = 'FILE' AND entry.is_missing = 0 AND entry.last_seen_generation = $8 AND entry.fingerprint = incoming.fingerprint"
        );

        let ledger = manifest_seen_paths_insert_template(&cache, DatabaseBackend::Sqlite, 2)
            .expect("SQLite seen-path insert");
        assert_eq!(
            ledger.as_ref(),
            "WITH incoming_seen_paths(relative_path) AS (VALUES (?), (?)) INSERT INTO scan_manifest_seen_paths (manifest_id, library_root_id, relative_path) SELECT ?, ?, incoming_seen_paths.relative_path FROM incoming_seen_paths WHERE TRUE ON CONFLICT(manifest_id, library_root_id, relative_path) DO NOTHING"
        );
        let postgres_ledger =
            manifest_seen_paths_insert_template(&cache, DatabaseBackend::Postgres, 2)
                .expect("PostgreSQL seen-path insert");
        assert!(postgres_ledger.starts_with(
            "WITH incoming_seen_paths(relative_path) AS (VALUES ($1), ($2)) INSERT INTO scan_manifest_seen_paths"
        ));
        assert!(postgres_ledger.contains("SELECT $3, $4, incoming_seen_paths.relative_path"));

        let repeated = manifest_presence_update_template(&cache, DatabaseBackend::Postgres, 2)
            .expect("reused PostgreSQL presence template");
        assert!(Arc::ptr_eq(&postgres_update, &repeated));
    }

    #[test]
    fn manifest_presence_templates_allow_backend_batch_bind_limits() {
        let cache = SqlTemplateCache::default();
        let postgres = manifest_presence_update_template(&cache, DatabaseBackend::Postgres, 5_000)
            .expect("PostgreSQL maximum presence batch");
        assert_eq!(postgres.matches('$').count(), 35_002);
        assert!(postgres.ends_with("current.is_missing = 0"));
        assert_eq!(
            manifest_presence_update_template(&cache, DatabaseBackend::Postgres, 5_001),
            Err(TemplateError::InvalidShape)
        );

        let sqlite = manifest_presence_update_template(&cache, DatabaseBackend::Sqlite, 2_900)
            .expect("SQLite maximum presence batch");
        assert_eq!(sqlite.matches('?').count(), 20_302);
        assert_eq!(
            manifest_presence_update_template(&cache, DatabaseBackend::Sqlite, 2_901),
            Err(TemplateError::InvalidShape)
        );
        assert_eq!(cache.entry_count(), 2);

        let tail_cache = SqlTemplateCache::default();
        for rows in [5_000, 1, 2, 3, 4, 5] {
            manifest_seen_paths_insert_template(&tail_cache, DatabaseBackend::Postgres, rows)
                .expect("bounded exact-row insert template");
        }
        assert_eq!(tail_cache.entry_count(), 5);
        let full_key = TemplateKey::new(
            TemplateFamily::ManifestSeenPathsInsert,
            DatabaseBackend::Postgres,
            TemplateVersion::V1,
            TemplateShape::ExactRows(5_000),
        )
        .expect("maximum exact-row key");
        let evicted_tail_key = TemplateKey::new(
            TemplateFamily::ManifestSeenPathsInsert,
            DatabaseBackend::Postgres,
            TemplateVersion::V1,
            TemplateShape::ExactRows(1),
        )
        .expect("first exact-row tail key");
        let state = tail_cache.state.lock().expect("exact-row cache state");
        assert!(state.entries.contains_key(&full_key));
        assert!(!state.entries.contains_key(&evicted_tail_key));
    }

    #[test]
    fn manifest_directory_frontier_templates_preserve_sql_and_bind_order() {
        let cache = SqlTemplateCache::default();
        let sqlite_insert =
            manifest_directory_frontier_insert_template(&cache, DatabaseBackend::Sqlite, 2)
                .expect("SQLite directory insert template");
        assert_eq!(
            sqlite_insert.as_ref(),
            "INSERT INTO scan_manifest_directories (manifest_id, library_root_id, relative_path, state) VALUES (?, ?, ?, 'PENDING'), (?, ?, ?, 'PENDING') ON CONFLICT(manifest_id, library_root_id, relative_path) DO NOTHING"
        );

        let postgres_insert =
            manifest_directory_frontier_insert_template(&cache, DatabaseBackend::Postgres, 2)
                .expect("PostgreSQL directory insert template");
        assert!(postgres_insert.ends_with(
            "VALUES ($1, $2, $3, 'PENDING'), ($4, $5, $6, 'PENDING') ON CONFLICT(manifest_id, library_root_id, relative_path) DO NOTHING"
        ));

        let sqlite_complete =
            manifest_directory_frontier_complete_template(&cache, DatabaseBackend::Sqlite, 3)
                .expect("SQLite directory completion template");
        assert_eq!(
            sqlite_complete.as_ref(),
            "UPDATE scan_manifest_directories SET state = 'COMPLETE', error = NULL, updated_at = unixepoch() WHERE manifest_id = ? AND library_root_id = ? AND state <> 'COMPLETE' AND relative_path IN (?, ?, ?)"
        );

        let postgres_complete =
            manifest_directory_frontier_complete_template(&cache, DatabaseBackend::Postgres, 3)
                .expect("PostgreSQL directory completion template");
        assert!(postgres_complete.ends_with(
            "WHERE manifest_id = $1 AND library_root_id = $2 AND state <> 'COMPLETE' AND relative_path IN ($3, $4, $5)"
        ));
        assert!(Arc::ptr_eq(
            &postgres_complete,
            &manifest_directory_frontier_complete_template(&cache, DatabaseBackend::Postgres, 3,)
                .expect("reused PostgreSQL directory completion template")
        ));
        assert_eq!(cache.entry_count(), 4);
    }

    #[test]
    fn manifest_directory_frontier_templates_enforce_exact_batch_bounds() {
        let cache = SqlTemplateCache::default();
        assert_eq!(
            manifest_directory_frontier_insert_template(&cache, DatabaseBackend::Sqlite, 1)
                .expect("SQLite single-directory insert")
                .as_ref(),
            "INSERT INTO scan_manifest_directories (manifest_id, library_root_id, relative_path, state) VALUES (?, ?, ?, 'PENDING') ON CONFLICT(manifest_id, library_root_id, relative_path) DO NOTHING"
        );
        assert_eq!(
            manifest_directory_frontier_complete_template(&cache, DatabaseBackend::Postgres, 1)
                .expect("PostgreSQL single-directory completion")
                .as_ref(),
            "UPDATE scan_manifest_directories SET state = 'COMPLETE', error = NULL, updated_at = unixepoch() WHERE manifest_id = $1 AND library_root_id = $2 AND state <> 'COMPLETE' AND relative_path IN ($3)"
        );
        let sqlite_insert =
            manifest_directory_frontier_insert_template(&cache, DatabaseBackend::Sqlite, 200)
                .expect("SQLite maximum directory insert batch");
        assert_eq!(sqlite_insert.matches('?').count(), 600);
        let postgres_complete =
            manifest_directory_frontier_complete_template(&cache, DatabaseBackend::Postgres, 200)
                .expect("PostgreSQL maximum directory completion batch");
        assert_eq!(postgres_complete.matches('$').count(), 202);
        assert!(postgres_complete.contains("relative_path IN ($3, $4, $5, $6, $7, $8"));
        assert!(postgres_complete.contains("$201, $202)"));

        for backend in [DatabaseBackend::Sqlite, DatabaseBackend::Postgres] {
            assert_eq!(
                manifest_directory_frontier_insert_template(&cache, backend, 0),
                Err(TemplateError::InvalidShape)
            );
            assert_eq!(
                manifest_directory_frontier_insert_template(&cache, backend, 201),
                Err(TemplateError::InvalidShape)
            );
            assert_eq!(
                manifest_directory_frontier_complete_template(&cache, backend, 0),
                Err(TemplateError::InvalidShape)
            );
            assert_eq!(
                manifest_directory_frontier_complete_template(&cache, backend, 201),
                Err(TemplateError::InvalidShape)
            );
        }
    }

    #[test]
    fn poisoned_cache_falls_back_to_uncached_construction() {
        let cache = Arc::new(SqlTemplateCache::default());
        let poison_cache = Arc::clone(&cache);
        let _ = thread::spawn(move || {
            let _guard = poison_cache.state.lock().expect("fresh cache lock");
            panic!("poison test lock");
        })
        .join();

        let query_key = key(DatabaseBackend::Postgres, 1);
        let builds = AtomicUsize::new(0);
        for _ in 0..2 {
            cache
                .get_or_build(query_key, |builder| {
                    builds.fetch_add(1, Ordering::Relaxed);
                    builder.push_static("SELECT ");
                    builder.push_bind()?;
                    builder.push_static(", ");
                    builder.push_bind()?;
                    Ok(())
                })
                .expect("poisoned cache must remain usable");
        }
        assert_eq!(builds.load(Ordering::Relaxed), 2);
    }
}
