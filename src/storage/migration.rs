use super::*;

const MEDIA_CONTENT_RESTRICTION_INDEX_INVALIDATION_TRIGGER: &str =
    "CREATE TRIGGER IF NOT EXISTS trg_media_items_content_restriction_index_invalidate_nfo
     AFTER UPDATE OF nfo_metadata_json ON media_items
     WHEN OLD.nfo_metadata_json IS NOT NEW.nfo_metadata_json
     BEGIN
         DELETE FROM media_content_restriction_index WHERE item_id = NEW.id;
         INSERT INTO media_content_restriction_index_queue (item_id)
         VALUES (NEW.id)
         ON CONFLICT(item_id) DO UPDATE SET queued_at = unixepoch();
     END";

const MEDIA_CONTENT_RESTRICTION_INDEX_QUEUE_TRIGGER: &str =
    "CREATE TRIGGER IF NOT EXISTS trg_media_items_content_restriction_index_queue_insert
     AFTER INSERT ON media_items
     BEGIN
         INSERT INTO media_content_restriction_index_queue (item_id)
         VALUES (NEW.id)
         ON CONFLICT(item_id) DO UPDATE SET queued_at = unixepoch();
     END";

/// Repairs SQLite catalog constraints that the migration history cannot express.
///
/// SQLite cannot alter a `CHECK` constraint in place, and the historical base migrations
/// (`0005`, `0006`) must keep replaying byte-for-byte for existing databases. This rebuild runs
/// after every migration, detects the legacy shapes through `sqlite_master`, and only then
/// recreates `libraries`/`media_items` with the current constraints. It is idempotent: a database
/// that already has the modern shape costs two catalog reads and no DDL.
///
/// A future task should replace this with a replayable rebuild migration, which requires pinning
/// the column set of the migration it is inserted at; see the storage review notes.
pub(super) async fn migrate_sqlite_catalog_constraints(
    pool: &AnyPool,
    path: &Path,
) -> Result<(), StorageError> {
    let mut connection = pool.acquire().await.map_err(|source| StorageError::Sqlx {
        path: path.to_path_buf(),
        source,
    })?;
    let has_legacy_unique = sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS (
             SELECT 1
             FROM sqlite_master
             WHERE type = 'table'
               AND name = 'media_items'
               AND sql LIKE '%UNIQUE (library_id, sort_title, production_year)%'
         )",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(|source| StorageError::Sqlx {
        path: path.to_path_buf(),
        source,
    })?;
    let library_schema = sqlx::query_scalar::<_, String>(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'libraries'",
    )
    .fetch_optional(&mut *connection)
    .await
    .map_err(|source| StorageError::Sqlx {
        path: path.to_path_buf(),
        source,
    })?;
    let media_item_schema = sqlx::query_scalar::<_, String>(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'media_items'",
    )
    .fetch_optional(&mut *connection)
    .await
    .map_err(|source| StorageError::Sqlx {
        path: path.to_path_buf(),
        source,
    })?;
    let needs_library_type_upgrade = library_schema
        .as_deref()
        .is_some_and(|schema| !schema.contains("'HOMEVIDEOS'"));
    let needs_media_item_type_upgrade = media_item_schema
        .as_deref()
        .is_some_and(|schema| !schema.contains("'VIDEO'"));
    if has_legacy_unique == 0 && !needs_library_type_upgrade && !needs_media_item_type_upgrade {
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_media_items_people_visible
             ON media_items(library_id, id)
             WHERE removed_at IS NULL",
        )
        .execute(&mut *connection)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: path.to_path_buf(),
            source,
        })?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_media_items_merged_into
             ON media_items(merged_into_item_id)",
        )
        .execute(&mut *connection)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: path.to_path_buf(),
            source,
        })?;
        sqlx::query(MEDIA_CONTENT_RESTRICTION_INDEX_INVALIDATION_TRIGGER)
            .execute(&mut *connection)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: path.to_path_buf(),
                source,
            })?;
        sqlx::query(MEDIA_CONTENT_RESTRICTION_INDEX_QUEUE_TRIGGER)
            .execute(&mut *connection)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: path.to_path_buf(),
                source,
            })?;
        return Ok(());
    }

    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: path.to_path_buf(),
            source,
        })?;

    let migration_result = async {
        let mut transaction = connection
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: path.to_path_buf(),
                source,
            })?;
        let statements = [
            "CREATE TABLE libraries_new (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                kind TEXT NOT NULL CHECK (kind IN ('MOVIE', 'SERIES', 'MIXED', 'HOMEVIDEOS')),
                is_enabled INTEGER NOT NULL DEFAULT 1 CHECK (is_enabled IN (0, 1)),
                realtime_watch_enabled INTEGER NOT NULL DEFAULT 0 CHECK (realtime_watch_enabled IN (0, 1)),
                incremental_schedule TEXT,
                reconciliation_schedule TEXT,
                metadata_schedule TEXT,
                scan_concurrency INTEGER NOT NULL DEFAULT 2 CHECK (scan_concurrency > 0),
                probe_concurrency INTEGER NOT NULL DEFAULT 1 CHECK (probe_concurrency > 0),
                last_scan_at INTEGER,
                created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
                scraper_id TEXT,
                cover_image_path TEXT,
                cover_image_content_type TEXT,
                cover_image_size INTEGER,
                cover_image_tag TEXT,
                media_strategy_json TEXT,
                realtime_metadata_auto_match_enabled INTEGER NOT NULL DEFAULT 0 CHECK (realtime_metadata_auto_match_enabled IN (0, 1)),
                scan_missing_metadata_auto_match_enabled INTEGER NOT NULL DEFAULT 1 CHECK (scan_missing_metadata_auto_match_enabled IN (0, 1)),
                chapter_source_id TEXT,
                system_kind TEXT CHECK (system_kind IS NULL OR system_kind = 'COLLECTIONS'),
                deletion_state TEXT NOT NULL DEFAULT 'ACTIVE'
                    CHECK (deletion_state IN ('ACTIVE', 'DELETING'))
            )",
            "INSERT INTO libraries_new (
                id, name, kind, is_enabled, realtime_watch_enabled, incremental_schedule,
                reconciliation_schedule, metadata_schedule, scan_concurrency, probe_concurrency,
                last_scan_at, created_at, updated_at, scraper_id, cover_image_path,
                cover_image_content_type, cover_image_size, cover_image_tag, media_strategy_json,
                realtime_metadata_auto_match_enabled, scan_missing_metadata_auto_match_enabled,
                chapter_source_id, system_kind, deletion_state
             )
             SELECT
                id, name, kind, is_enabled, realtime_watch_enabled, incremental_schedule,
                reconciliation_schedule, metadata_schedule, scan_concurrency, probe_concurrency,
                last_scan_at, created_at, updated_at, scraper_id, cover_image_path,
                cover_image_content_type, cover_image_size, cover_image_tag, media_strategy_json,
                realtime_metadata_auto_match_enabled, scan_missing_metadata_auto_match_enabled,
                chapter_source_id, system_kind, deletion_state
             FROM libraries",
            "DROP TABLE libraries",
            "ALTER TABLE libraries_new RENAME TO libraries",
            "CREATE UNIQUE INDEX idx_libraries_system_kind
             ON libraries(system_kind)
             WHERE system_kind IS NOT NULL",
            "DROP TRIGGER IF EXISTS media_items_search_ai",
            "DROP TRIGGER IF EXISTS media_items_search_au",
            "DROP TRIGGER IF EXISTS media_items_search_ad",
            "DROP TRIGGER IF EXISTS item_aliases_search_ai",
            "DROP TRIGGER IF EXISTS item_aliases_search_au",
            "DROP TRIGGER IF EXISTS item_aliases_search_ad",
            "DROP TRIGGER IF EXISTS media_item_provider_ids_ai",
            "DROP TRIGGER IF EXISTS media_item_provider_ids_au",
            "DROP TRIGGER IF EXISTS trg_media_sources_availability_insert",
            "DROP TRIGGER IF EXISTS trg_media_sources_availability_update",
            "DROP TRIGGER IF EXISTS trg_media_sources_availability_delete",
            "DROP TRIGGER IF EXISTS trg_filesystem_entries_availability_update",
            "DROP TRIGGER IF EXISTS trg_media_items_content_restriction_index_invalidate_nfo",
            "CREATE TABLE media_items_new (
                id TEXT PRIMARY KEY NOT NULL,
                library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
                item_type TEXT NOT NULL CHECK (item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE', 'BOX_SET', 'FOLDER', 'UNRESOLVED', 'VIDEO')),
                parent_id TEXT,
                series_id TEXT,
                season_number INTEGER,
                episode_number INTEGER,
                absolute_number INTEGER,
                title TEXT NOT NULL,
                sort_title TEXT NOT NULL,
                original_title TEXT,
                overview TEXT,
                production_year INTEGER,
                premiere_date TEXT,
                runtime_ticks INTEGER,
                provider_ids_json TEXT,
                metadata_provenance_json TEXT,
                locked_fields_json TEXT,
                identification_status TEXT NOT NULL CHECK (identification_status IN ('LOCAL_CONFIRMED', 'ONLINE_CONFIRMED', 'PENDING', 'FAILED')),
                added_at INTEGER NOT NULL DEFAULT (unixepoch()),
                updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
                removed_at INTEGER,
                metadata_fingerprint BLOB,
                identity_key TEXT,
                rating REAL,
                rating_source TEXT,
                metadata_scraper_id TEXT,
                last_air_date TEXT,
                status TEXT,
                original_language TEXT,
                has_available_source INTEGER NOT NULL DEFAULT 0 CHECK (has_available_source IN (0, 1)),
                poster_fallback_required INTEGER NOT NULL DEFAULT 0 CHECK (poster_fallback_required IN (0, 1)),
                merged_into_item_id TEXT REFERENCES media_items(id) ON DELETE SET NULL,
                nfo_metadata_json TEXT,
                nfo_metadata_fingerprint BLOB,
                nfo_collection_reference_key TEXT
            )",
            "INSERT INTO media_items_new (
                id, library_id, item_type, parent_id, series_id, season_number, episode_number,
                absolute_number, title, sort_title, original_title, overview, production_year,
                premiere_date, runtime_ticks, provider_ids_json, metadata_provenance_json,
                locked_fields_json, identification_status, added_at, updated_at, removed_at,
                metadata_fingerprint, identity_key, rating, rating_source, metadata_scraper_id,
                last_air_date, status,
                original_language, has_available_source, poster_fallback_required,
                merged_into_item_id,
                nfo_metadata_json, nfo_metadata_fingerprint,
                nfo_collection_reference_key
             )
             SELECT
                id, library_id, item_type, parent_id, series_id, season_number, episode_number,
                absolute_number, title, sort_title, original_title, overview, production_year,
                premiere_date, runtime_ticks, provider_ids_json, metadata_provenance_json,
                locked_fields_json, identification_status, added_at, updated_at, removed_at,
                metadata_fingerprint, identity_key, rating, rating_source, metadata_scraper_id,
                last_air_date, status,
                original_language, has_available_source, poster_fallback_required,
                merged_into_item_id,
                nfo_metadata_json, nfo_metadata_fingerprint,
                nfo_collection_reference_key
             FROM media_items",
            "DROP TABLE media_items",
            "ALTER TABLE media_items_new RENAME TO media_items",
            "CREATE INDEX idx_media_items_library_sort ON media_items(library_id, sort_title, id)",
            "CREATE UNIQUE INDEX idx_media_items_identity_key
             ON media_items(identity_key)
             WHERE identity_key IS NOT NULL",
            "CREATE INDEX idx_media_items_parent_removed
             ON media_items(parent_id, removed_at)",
            "CREATE INDEX idx_media_items_series_removed
             ON media_items(series_id, removed_at)",
            "CREATE INDEX idx_media_items_library_type_visible
             ON media_items(library_id, item_type, id)
             WHERE removed_at IS NULL",
            "CREATE INDEX idx_media_items_library_added_visible
             ON media_items(library_id, added_at DESC, sort_title, id)
             WHERE removed_at IS NULL
               AND item_type <> 'FOLDER'
               AND has_available_source = 1",
            "CREATE INDEX idx_media_items_parent_available
             ON media_items(parent_id, removed_at, has_available_source)",
            "CREATE INDEX idx_media_items_series_available
             ON media_items(series_id, removed_at, has_available_source)",
            "CREATE INDEX idx_media_items_people_visible
             ON media_items(library_id, id)
             WHERE removed_at IS NULL",
            "CREATE INDEX idx_media_items_home_unavailable_series
             ON media_items(library_id, added_at DESC, sort_title, id)
             WHERE removed_at IS NULL
               AND item_type = 'SERIES'
               AND has_available_source = 0",
            "CREATE INDEX idx_media_items_merged_into
             ON media_items(merged_into_item_id)",
            "CREATE INDEX idx_media_items_migration_title
             ON media_items(item_type, sort_title, production_year, library_id, id)
             WHERE removed_at IS NULL",
            "CREATE INDEX idx_media_items_updated_at
             ON media_items(updated_at, id)",
            "CREATE INDEX idx_media_items_library_name_sort
             ON media_items(library_id, LOWER(title), id)
             WHERE removed_at IS NULL",
            "CREATE INDEX idx_media_items_library_sortname_sort
             ON media_items(library_id, LOWER(sort_title), id)
             WHERE removed_at IS NULL",
            "CREATE INDEX idx_media_items_library_added_sort
             ON media_items(library_id, added_at DESC, LOWER(title), id)
             WHERE removed_at IS NULL",
            "CREATE INDEX idx_media_items_library_premiere_sort
             ON media_items(
                 library_id,
                 (CASE WHEN NULLIF(premiere_date, '') IS NULL AND production_year IS NULL THEN 1 ELSE 0 END),
                 (COALESCE(NULLIF(premiere_date, ''), CAST(production_year AS TEXT) || '-01-01')) DESC,
                 LOWER(title),
                 id
             )
             WHERE removed_at IS NULL",
            "CREATE INDEX idx_media_items_library_rating_sort
             ON media_items(
                 library_id,
                 (CASE WHEN rating IS NULL THEN 1 ELSE 0 END) ASC,
                 rating DESC,
                 LOWER(title),
                 id
             )
             WHERE removed_at IS NULL",
            "CREATE TRIGGER media_items_search_ai AFTER INSERT ON media_items BEGIN
                INSERT INTO catalog_outbox (kind, item_id) VALUES ('search', NEW.id);
            END",
            "CREATE TRIGGER media_items_search_au AFTER UPDATE OF title, sort_title, original_title ON media_items
             WHEN OLD.title IS NOT NEW.title
               OR OLD.sort_title IS NOT NEW.sort_title
               OR OLD.original_title IS NOT NEW.original_title
             BEGIN
                INSERT INTO catalog_outbox (kind, item_id) VALUES ('search', NEW.id);
            END",
            "CREATE TRIGGER media_items_search_ad AFTER DELETE ON media_items BEGIN
                INSERT INTO catalog_outbox (kind, item_id) VALUES ('search', OLD.id);
            END",
            "CREATE TRIGGER item_aliases_search_ai AFTER INSERT ON item_aliases BEGIN
                INSERT INTO catalog_outbox (kind, item_id) VALUES ('search', NEW.item_id);
            END",
            "CREATE TRIGGER item_aliases_search_au AFTER UPDATE OF alias, alias_normalized, item_id ON item_aliases
             WHEN OLD.alias IS NOT NEW.alias OR OLD.alias_normalized IS NOT NEW.alias_normalized OR OLD.item_id IS NOT NEW.item_id
             BEGIN
                INSERT INTO catalog_outbox (kind, item_id) VALUES ('search', NEW.item_id);
                INSERT INTO catalog_outbox (kind, item_id)
                SELECT 'search', OLD.item_id WHERE OLD.item_id IS NOT NEW.item_id;
             END",
            "CREATE TRIGGER item_aliases_search_ad AFTER DELETE ON item_aliases BEGIN
                INSERT INTO catalog_outbox (kind, item_id) VALUES ('search', OLD.item_id);
            END",
            "CREATE TRIGGER media_item_provider_ids_ai
             AFTER INSERT ON media_items
             BEGIN
                 INSERT OR IGNORE INTO media_item_provider_ids
                     (media_item_id, item_type, provider, provider_id)
                 SELECT NEW.id, NEW.item_type, lower(json_each.key), json_each.value
                 FROM json_each(
                     CASE
                         WHEN json_valid(NEW.provider_ids_json) THEN NEW.provider_ids_json
                         ELSE '{}'
                     END
                 ) AS json_each
                 WHERE json_each.type = 'text';
             END",
            "CREATE TRIGGER media_item_provider_ids_au
             AFTER UPDATE OF item_type, provider_ids_json ON media_items
             BEGIN
                 DELETE FROM media_item_provider_ids WHERE media_item_id = NEW.id;
                 INSERT OR IGNORE INTO media_item_provider_ids
                     (media_item_id, item_type, provider, provider_id)
                 SELECT NEW.id, NEW.item_type, lower(json_each.key), json_each.value
                 FROM json_each(
                     CASE
                         WHEN json_valid(NEW.provider_ids_json) THEN NEW.provider_ids_json
                         ELSE '{}'
                     END
                 ) AS json_each
                 WHERE json_each.type = 'text';
             END",
            "CREATE TRIGGER trg_media_sources_availability_insert
             AFTER INSERT ON media_sources
             WHEN COALESCE((
                 SELECT has_available_source FROM media_items WHERE id = NEW.item_id
             ), 0) = 0
             BEGIN
                 UPDATE media_items
                 SET has_available_source = 1
                 WHERE id = NEW.item_id
                   AND has_available_source = 0
                   AND EXISTS (
                       SELECT 1
                       FROM filesystem_entries
                       WHERE id = NEW.filesystem_entry_id
                         AND is_missing = 0
                   );
             END",
            "CREATE TRIGGER trg_media_sources_availability_update
             AFTER UPDATE OF item_id, filesystem_entry_id ON media_sources
             BEGIN
                 UPDATE media_items
                 SET has_available_source = EXISTS (
                     SELECT 1
                     FROM media_sources ms
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     WHERE ms.item_id = media_items.id
                       AND fe.is_missing = 0
                 )
                 WHERE id IN (OLD.item_id, NEW.item_id);
             END",
            "CREATE TRIGGER trg_media_sources_availability_delete
             AFTER DELETE ON media_sources
             BEGIN
                 UPDATE media_items
                 SET has_available_source = EXISTS (
                     SELECT 1
                     FROM media_sources ms
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     WHERE ms.item_id = media_items.id
                       AND fe.is_missing = 0
                 )
                 WHERE id = OLD.item_id;
             END",
            "CREATE TRIGGER trg_filesystem_entries_availability_update
             AFTER UPDATE OF is_missing ON filesystem_entries
             WHEN OLD.is_missing <> NEW.is_missing
             BEGIN
                 UPDATE media_items
                 SET has_available_source = EXISTS (
                     SELECT 1
                     FROM media_sources ms
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     WHERE ms.item_id = media_items.id
                       AND fe.is_missing = 0
                 )
                 WHERE id IN (
                     SELECT item_id
                     FROM media_sources
                     WHERE filesystem_entry_id = NEW.id
                 );
             END",
            "CREATE TRIGGER collection_media_item_sync_au
             AFTER UPDATE OF removed_at, has_available_source, library_id, item_type ON media_items
             WHEN OLD.removed_at IS NOT NEW.removed_at
               OR OLD.has_available_source IS NOT NEW.has_available_source
               OR OLD.library_id IS NOT NEW.library_id
               OR OLD.item_type IS NOT NEW.item_type
             BEGIN
                 INSERT INTO collection_sync_outbox (
                     collection_provider, collection_provider_id, generation, updated_at
                 )
                 SELECT DISTINCT reference.collection_provider, reference.collection_provider_id,
                        1, unixepoch()
                 FROM collection_item_references reference
                 WHERE reference.item_id = NEW.id
                 ON CONFLICT (collection_provider, collection_provider_id) DO UPDATE SET
                     generation = collection_sync_outbox.generation + 1,
                     updated_at = excluded.updated_at;
             END",
            "CREATE TRIGGER collection_media_item_sync_bd
             BEFORE DELETE ON media_items
             BEGIN
                 INSERT INTO collection_sync_outbox (
                     collection_provider, collection_provider_id, generation, updated_at
                 )
                 SELECT DISTINCT reference.collection_provider, reference.collection_provider_id,
                        1, unixepoch()
                 FROM collection_item_references reference
                 WHERE reference.item_id = OLD.id
                 ON CONFLICT (collection_provider, collection_provider_id) DO UPDATE SET
                     generation = collection_sync_outbox.generation + 1,
                     updated_at = excluded.updated_at;
             END",
            "CREATE TRIGGER collection_library_sync_au
             AFTER UPDATE OF is_enabled ON libraries
             WHEN OLD.is_enabled IS NOT NEW.is_enabled
             BEGIN
                 INSERT INTO collection_sync_outbox (
                     collection_provider, collection_provider_id, generation, updated_at
                 )
                 SELECT DISTINCT reference.collection_provider, reference.collection_provider_id,
                        1, unixepoch()
                 FROM media_items item
                 JOIN collection_item_references reference ON reference.item_id = item.id
                 WHERE item.library_id = NEW.id
                 ON CONFLICT (collection_provider, collection_provider_id) DO UPDATE SET
                     generation = collection_sync_outbox.generation + 1,
                     updated_at = excluded.updated_at;
             END",
            "CREATE TRIGGER collection_library_sync_bd
             BEFORE DELETE ON libraries
             BEGIN
                 INSERT INTO collection_sync_outbox (
                     collection_provider, collection_provider_id, generation, updated_at
                 )
                 SELECT DISTINCT reference.collection_provider, reference.collection_provider_id,
                        1, unixepoch()
                 FROM media_items item
                 JOIN collection_item_references reference ON reference.item_id = item.id
                 WHERE item.library_id = OLD.id
                 ON CONFLICT (collection_provider, collection_provider_id) DO UPDATE SET
                     generation = collection_sync_outbox.generation + 1,
                     updated_at = excluded.updated_at;
             END",
            MEDIA_CONTENT_RESTRICTION_INDEX_INVALIDATION_TRIGGER,
            MEDIA_CONTENT_RESTRICTION_INDEX_QUEUE_TRIGGER,
        ];
        for statement in statements {
            sqlx::query(statement)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: path.to_path_buf(),
                source,
            })?;
        Ok::<(), StorageError>(())
    }
    .await;

    let foreign_keys_result = sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: path.to_path_buf(),
            source,
        });
    migration_result.and(foreign_keys_result.map(|_| ()))
}

pub(super) async fn validate_postgres_schema(pool: &AnyPool) -> Result<(), StorageError> {
    let path = PathBuf::from("external PostgreSQL");
    let application_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.tables
         WHERE table_schema = 'public' AND table_name <> '_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await
    .map_err(|source| StorageError::Sqlx {
        path: path.clone(),
        source,
    })?;
    if application_table_count == 0 {
        return Ok(());
    }

    let lux_meta_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.tables
         WHERE table_schema = 'public' AND table_name = 'lux_meta'",
    )
    .fetch_one(pool)
    .await
    .map_err(|source| StorageError::Sqlx { path, source })?;
    if lux_meta_count == 0 {
        return Err(StorageError::Configuration(
            DatabaseConfigurationError::Invalid(
                "PostgreSQL 数据库必须为空或已经是 Lux 数据库".to_owned(),
            ),
        ));
    }
    Ok(())
}
pub(super) async fn ensure_server_id(
    pool: &AnyPool,
    backend: DatabaseBackend,
) -> Result<String, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let existing =
        sqlx::query_scalar::<_, String>("SELECT value FROM lux_meta WHERE key = 'server_id'")
            .fetch_optional(&mut *transaction)
            .await?;
    if let Some(server_id) = existing {
        transaction.commit().await?;
        return Ok(server_id);
    }

    let generated = Uuid::now_v7().to_string();
    sqlx::query(adapt_sql_for_backend(
        backend,
        "INSERT INTO lux_meta (key, value) VALUES ('server_id', ?)
         ON CONFLICT(key) DO NOTHING",
    ))
    .bind(generated)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    sqlx::query_scalar("SELECT value FROM lux_meta WHERE key = 'server_id'")
        .fetch_one(pool)
        .await
}

/// Marker written next to the configuration once a server identity exists.
///
/// The database cannot answer "was this server set up before?" on its own: a missing file and a
/// first install look identical to SQLite, which creates the file on connect. The marker lives
/// beside the database precisely so the two can be compared.
const SERVER_STATE_FILE: &str = "server-state.json";

/// Refuses to start a previously initialized server against an empty database.
///
/// Restoring only the configuration directory, or mounting the wrong volume, used to look like a
/// brand-new install: an empty `lux.db` was created, migrations built a fresh schema, and the setup
/// wizard appeared over libraries the operator believed were still there. The check runs before the
/// connection is opened, so the failed attempt does not even create the file.
pub(super) async fn refuse_empty_database_when_initialized(
    config_dir: &Path,
    path: &Path,
    backend: DatabaseBackend,
    database_url: &str,
) -> Result<(), StorageError> {
    if !server_state_marker_present(config_dir).await? {
        return Ok(());
    }
    let has_identity = match backend {
        DatabaseBackend::Sqlite => sqlite_database_has_identity(path).await?,
        DatabaseBackend::Postgres => postgres_database_has_identity(database_url).await?,
    };
    if has_identity {
        return Ok(());
    }
    Err(StorageError::EmptyDatabase {
        path: path.to_path_buf(),
    })
}

async fn server_state_marker_present(config_dir: &Path) -> Result<bool, StorageError> {
    let marker = config_dir.join(SERVER_STATE_FILE);
    match fs::read(&marker).await {
        Ok(bytes) => {
            if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
                // A corrupt marker is not evidence that the server is new, and guessing "new"
                // here is the one guess that can overwrite real data with a fresh schema.
                tracing::warn!(
                    path = %marker.display(),
                    "server state marker is unreadable; treating this directory as initialized"
                );
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(StorageError::Io {
            path: marker,
            source,
        }),
    }
}

async fn sqlite_database_has_identity(path: &Path) -> Result<bool, StorageError> {
    if fs::metadata(path).await.is_err() {
        return Ok(false);
    }
    // Read-only, and before the read-write pool exists: opening read-write would create the very
    // empty database this check is looking for.
    let url = format!("sqlite://{}?mode=ro", path.display());
    let options = AnyConnectOptions::from_str(&url).map_err(|source| StorageError::Sqlx {
        path: path.to_path_buf(),
        source,
    })?;
    let pool = match AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            // Being unable to read the file is not proof that it is empty; refusing would turn a
            // permissions problem into an outage the operator cannot diagnose.
            tracing::warn!(
                path = %path.display(),
                %error,
                "could not verify the existing database; continuing"
            );
            return Ok(true);
        }
    };
    let has_meta_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'lux_meta'",
    )
    .fetch_one(&pool)
    .await
    .map_err(|source| StorageError::Sqlx {
        path: path.to_path_buf(),
        source,
    })?;
    let has_identity = if has_meta_table == 0 {
        false
    } else {
        let identities: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM lux_meta WHERE key = 'server_id'")
                .fetch_one(&pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: path.to_path_buf(),
                    source,
                })?;
        identities > 0
    };
    pool.close().await;
    Ok(has_identity)
}

async fn postgres_database_has_identity(database_url: &str) -> Result<bool, StorageError> {
    let path = PathBuf::from("external PostgreSQL");
    let options =
        AnyConnectOptions::from_str(database_url).map_err(|source| StorageError::Sqlx {
            path: path.clone(),
            source,
        })?;
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: path.clone(),
            source,
        })?;
    let has_meta_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables
         WHERE table_schema = 'public' AND table_name = 'lux_meta'",
    )
    .fetch_one(&pool)
    .await
    .map_err(|source| StorageError::Sqlx {
        path: path.clone(),
        source,
    })?;
    let has_identity = if has_meta_table == 0 {
        false
    } else {
        let identities: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM lux_meta WHERE key = 'server_id'")
                .fetch_one(&pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: path.clone(),
                    source,
                })?;
        identities > 0
    };
    pool.close().await;
    Ok(has_identity)
}

/// Records that this configuration directory now belongs to an initialized server.
///
/// Written once, after the server identity exists, and never rewritten: it is the evidence the
/// startup guard reads on the next boot.
pub(super) async fn record_server_state(
    config_dir: &Path,
    server_id: &str,
) -> Result<(), StorageError> {
    let marker = config_dir.join(SERVER_STATE_FILE);
    match fs::metadata(&marker).await {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(StorageError::Io {
                path: marker,
                source,
            });
        }
    }
    let state = serde_json::json!({
        "serverId": server_id,
        "initializedAt": crate::clock::unix_timestamp(),
    });
    let bytes = serde_json::to_vec_pretty(&state)
        .map_err(|source| StorageError::Serialization(source.to_string()))?;
    let temporary = config_dir.join(format!(".{SERVER_STATE_FILE}.{}.tmp", Uuid::now_v7()));
    fs::write(&temporary, bytes)
        .await
        .map_err(|source| StorageError::Io {
            path: temporary.clone(),
            source,
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|source| StorageError::Io {
                path: temporary.clone(),
                source,
            })?;
    }
    fs::rename(&temporary, &marker)
        .await
        .map_err(|source| StorageError::Io {
            path: marker,
            source,
        })
}
