use crate::config::DatabaseBackend;
use std::{
    collections::HashMap,
    fmt::Write as _,
    sync::{Arc, Mutex},
};

const MAX_CACHED_TEMPLATE_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_CACHED_TEMPLATE_ENTRIES: usize = 256;
const MAX_CACHED_TAILS_PER_TEMPLATE: usize = 4;
const MAX_TEMPLATE_BINDINGS: usize = 32_766;

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
}

impl TemplateFamily {
    fn max_in_capacity(self, backend: TemplateBackend) -> u16 {
        match self {
            Self::FilesystemEntries | Self::FilesystemEntryRecords => 500,
            Self::ManifestFilesystemBaselines => match backend {
                TemplateBackend::Sqlite => 2_900,
                TemplateBackend::Postgres => 5_000,
            },
        }
    }

    fn fixed_bindings(self) -> usize {
        match self {
            Self::FilesystemEntries
            | Self::FilesystemEntryRecords
            | Self::ManifestFilesystemBaselines => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TemplateVersion {
    V1,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TemplateShape {
    InCapacity(u16),
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
        let TemplateShape::InCapacity(capacity) = shape;
        let max_capacity = family.max_in_capacity(backend);
        if capacity == 0
            || capacity > max_capacity
            || (!capacity.is_power_of_two() && capacity != max_capacity)
        {
            return Err(TemplateError::InvalidShape);
        }

        let binding_count = usize::from(capacity) + family.fixed_bindings();
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

    fn in_capacity(self) -> usize {
        let TemplateShape::InCapacity(capacity) = self.shape;
        usize::from(capacity)
    }

    fn is_full_batch(self) -> bool {
        self.in_capacity() == usize::from(self.family.max_in_capacity(self.backend))
    }

    fn belongs_to_same_template(self, other: Self) -> bool {
        self.family == other.family
            && self.backend == other.backend
            && self.version == other.version
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
        builder.push_static("SELECT id FROM filesystem_entries WHERE library_root_id = ");
        builder.push_bind()?;
        builder.push_static(" AND relative_path IN (");
        for index in 0..key.in_capacity() {
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
        for index in 0..key.in_capacity() {
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
        builder.push_static(
            "SELECT id, relative_path, entry_kind, size, modified_at, device, inode, \
             fingerprint, is_missing FROM filesystem_entries WHERE library_root_id = ",
        );
        builder.push_bind()?;
        builder.push_static(" AND relative_path IN (");
        for index in 0..key.in_capacity() {
            if index > 0 {
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
    let max_capacity = usize::from(family.max_in_capacity(backend.into()));
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
            u16::try_from(key.in_capacity()).map_err(|_| TemplateError::InvalidShape)?,
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
