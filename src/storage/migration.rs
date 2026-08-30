use super::*;

pub(super) async fn remove_sqlite_title_year_unique(
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
    if has_legacy_unique == 0 {
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
            "DROP TRIGGER IF EXISTS media_items_search_ai",
            "DROP TRIGGER IF EXISTS media_items_search_au",
            "DROP TRIGGER IF EXISTS media_items_search_ad",
            "DROP TRIGGER IF EXISTS media_item_provider_ids_ai",
            "DROP TRIGGER IF EXISTS media_item_provider_ids_au",
            "DROP TRIGGER IF EXISTS trg_media_sources_availability_insert",
            "DROP TRIGGER IF EXISTS trg_media_sources_availability_update",
            "DROP TRIGGER IF EXISTS trg_media_sources_availability_delete",
            "DROP TRIGGER IF EXISTS trg_filesystem_entries_availability_update",
            "CREATE TABLE media_items_new (
                id TEXT PRIMARY KEY NOT NULL,
                library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
                item_type TEXT NOT NULL CHECK (item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE', 'BOX_SET', 'FOLDER', 'UNRESOLVED')),
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
                nfo_metadata_json TEXT,
                nfo_metadata_fingerprint BLOB
            )",
            "INSERT INTO media_items_new (
                id, library_id, item_type, parent_id, series_id, season_number, episode_number,
                absolute_number, title, sort_title, original_title, overview, production_year,
                premiere_date, runtime_ticks, provider_ids_json, metadata_provenance_json,
                locked_fields_json, identification_status, added_at, removed_at,
                metadata_fingerprint, identity_key, rating, rating_source, metadata_scraper_id,
                last_air_date, status,
                original_language, has_available_source, poster_fallback_required,
                nfo_metadata_json, nfo_metadata_fingerprint
             )
             SELECT
                id, library_id, item_type, parent_id, series_id, season_number, episode_number,
                absolute_number, title, sort_title, original_title, overview, production_year,
                premiere_date, runtime_ticks, provider_ids_json, metadata_provenance_json,
                locked_fields_json, identification_status, added_at, removed_at,
                metadata_fingerprint, identity_key, rating, rating_source, metadata_scraper_id,
                last_air_date, status,
                original_language, has_available_source, poster_fallback_required,
                nfo_metadata_json, nfo_metadata_fingerprint
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
            "CREATE INDEX idx_media_items_people_visible
             ON media_items(library_id, id)
             WHERE removed_at IS NULL",
            "CREATE INDEX idx_media_items_migration_title
             ON media_items(item_type, sort_title, production_year, library_id, id)
             WHERE removed_at IS NULL",
            "CREATE TRIGGER media_items_search_ai AFTER INSERT ON media_items BEGIN
                INSERT INTO media_search (item_id, title, sort_title, original_title, aliases)
                VALUES (NEW.id, NEW.title, NEW.sort_title, COALESCE(NEW.original_title, ''),
                        COALESCE((SELECT group_concat(alias, ' ') FROM item_aliases WHERE item_id = NEW.id), ''));
            END",
            "CREATE TRIGGER media_items_search_au AFTER UPDATE OF title, sort_title, original_title ON media_items BEGIN
                DELETE FROM media_search WHERE item_id = OLD.id;
                INSERT INTO media_search (item_id, title, sort_title, original_title, aliases)
                VALUES (NEW.id, NEW.title, NEW.sort_title, COALESCE(NEW.original_title, ''),
                        COALESCE((SELECT group_concat(alias, ' ') FROM item_aliases WHERE item_id = NEW.id), ''));
            END",
            "CREATE TRIGGER media_items_search_ad AFTER DELETE ON media_items BEGIN
                DELETE FROM media_search WHERE item_id = OLD.id;
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
             BEGIN
                 UPDATE media_items
                 SET has_available_source = 1
                 WHERE id = NEW.item_id
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
    sqlx::query(sqlx::AssertSqlSafe(adapt_sql_for_backend(
        backend,
        "INSERT INTO lux_meta (key, value) VALUES ('server_id', ?)
         ON CONFLICT(key) DO NOTHING",
    )))
    .bind(generated)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    sqlx::query_scalar("SELECT value FROM lux_meta WHERE key = 'server_id'")
        .fetch_one(pool)
        .await
}
