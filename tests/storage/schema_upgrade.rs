//! Schema upgrades that must keep existing rows working (ADR-052 P5 provenance columns).
//!
//! The upgrade is exercised the way a deployment sees it: build the schema *without* the new
//! migration, write data, then apply the new migration and check that the old rows kept their
//! meaning, that the indexes still exist, and that the new columns enforce their contract.

use std::{fs, path::Path};

use sqlx::{SqlitePool, migrate::Migrator, sqlite::SqlitePoolOptions};

const NEW_MIGRATION: &str = "0246_danmaku_manual_match_provenance.sql";

/// Copies the SQLite migrations into `target`, skipping `skip`.
fn copy_migrations(target: &Path, skip: &str) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(target)?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy() == skip {
            continue;
        }
        fs::copy(entry.path(), target.join(name))?;
    }
    Ok(())
}

fn copy_one_migration(target: &Path, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    fs::copy(source.join(name), target.join(name))?;
    Ok(())
}

async fn open_pool(path: &Path) -> Result<SqlitePool, Box<dyn std::error::Error>> {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true);
    Ok(SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?)
}

/// Writes one library, media source and danmaku job (plus its item) against the pre-upgrade schema.
async fn seed_pre_upgrade_rows(pool: &SqlitePool) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('legacy-library', 'Legacy', 'MOVIE')",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO library_roots (
            id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('legacy-root', 'legacy-library', '/media', '/media', 1, 1)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title, identification_status, identity_key
         ) VALUES ('legacy-item', 'legacy-library', 'MOVIE', 'Legacy Movie',
                   'Legacy Movie', 'LOCAL_CONFIRMED', 'legacy-item')",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO filesystem_entries (
            id, library_root_id, relative_path, entry_kind, size,
            modified_at, last_seen_generation, is_missing
         ) VALUES ('legacy-entry', 'legacy-root', 'Legacy.Movie.mkv', 'FILE', 1, 1, 'legacy', 0)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO media_sources (
            id, item_id, source_kind, filesystem_entry_id, container, size, is_default, probe_status
         ) VALUES ('legacy-source', 'legacy-item', 'LOCAL_FILE', 'legacy-entry', 'mkv', 1, 1, 'PENDING')",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO danmaku_match_jobs (id, library_id, status, overwrite, concurrency, total_count)
         VALUES ('legacy-job', 'legacy-library', 'FAILED', 1, 2, 1)",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO danmaku_match_job_items (
            id, job_id, media_source_id, status, provider_episode_id
         ) VALUES ('legacy-job:legacy-source', 'legacy-job', 'legacy-source', 'WRITTEN', 'ep-old')",
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
async fn danmaku_provenance_columns_upgrade_existing_rows_in_place()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let migrations = directory.path().join("migrations-old");
    copy_migrations(&migrations, NEW_MIGRATION)?;
    let pool = open_pool(&directory.path().join("lux.db")).await?;
    Migrator::new(migrations.as_path()).await?.run(&pool).await?;
    let old_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(old_version, 245);
    seed_pre_upgrade_rows(&pool).await?;

    // Apply only the new migration, exactly as an upgraded deployment would.
    copy_one_migration(&migrations, NEW_MIGRATION)?;
    Migrator::new(migrations.as_path()).await?.run(&pool).await?;

    let new_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(new_version, 246);

    // The old rows keep their meaning: a sweep job without a recorded provider.
    let (kind, status, overwrite, concurrency, total_count): (String, String, i64, i64, i64) =
        sqlx::query_as(
            "SELECT kind, status, overwrite, concurrency, total_count
             FROM danmaku_match_jobs WHERE id = 'legacy-job'",
        )
        .fetch_one(&pool)
        .await?;
    assert_eq!(kind, "SWEEP");
    assert_eq!(status, "FAILED");
    assert_eq!(overwrite, 1);
    assert_eq!(concurrency, 2);
    assert_eq!(total_count, 1);
    let (item_status, item_episode, item_provider): (String, Option<String>, Option<String>) =
        sqlx::query_as(
            "SELECT status, provider_episode_id, provider
             FROM danmaku_match_job_items WHERE id = 'legacy-job:legacy-source'",
        )
        .fetch_one(&pool)
        .await?;
    assert_eq!(item_status, "WRITTEN");
    assert_eq!(item_episode.as_deref(), Some("ep-old"));
    assert_eq!(item_provider, None);

    // New rows carry the manual provenance, and the CHECK constraint holds.
    sqlx::query(
        "INSERT INTO danmaku_match_jobs (id, library_id, status, overwrite, concurrency, total_count, kind)
         VALUES ('manual-job', 'legacy-library', 'PENDING', 0, 2, 1, 'MANUAL')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO danmaku_match_job_items (
            id, job_id, media_source_id, status, provider, provider_anime_id, provider_episode_id
         ) VALUES ('manual-job:legacy-source', 'manual-job', 'legacy-source', 'PENDING',
                   'dandanplay', 'anime-1', 'ep-1')",
    )
    .execute(&pool)
    .await?;
    let stored: (String, Option<String>) = sqlx::query_as(
        "SELECT job.kind, item.provider
         FROM danmaku_match_jobs job
         JOIN danmaku_match_job_items item ON item.job_id = job.id
         WHERE job.id = 'manual-job'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(stored.0, "MANUAL");
    assert_eq!(stored.1.as_deref(), Some("dandanplay"));
    assert!(
        sqlx::query(
            "INSERT INTO danmaku_match_jobs (
                id, library_id, status, overwrite, concurrency, total_count, kind
             ) VALUES ('bad-kind', 'legacy-library', 'FAILED', 0, 2, 0, 'OTHER')",
        )
        .execute(&pool)
        .await
        .is_err(),
        "kind must stay constrained to the documented values"
    );

    // The indexes the delete path and the single-active-job rule depend on survived the upgrade,
    // and the partial unique index still blocks a second active job for one library.
    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name IN
             ('danmaku_match_jobs', 'danmaku_match_job_items')",
    )
    .fetch_all(&pool)
    .await?;
    for expected in [
        "idx_danmaku_match_jobs_status",
        "idx_danmaku_match_jobs_one_active_library",
        "idx_danmaku_match_jobs_library_id",
        "idx_danmaku_match_job_items_status",
        "idx_danmaku_match_job_items_media_source_id",
    ] {
        assert!(
            indexes.iter().any(|index| index == expected),
            "missing index after the upgrade: {expected}"
        );
    }
    assert!(
        sqlx::query(
            "INSERT INTO danmaku_match_jobs (id, library_id, status, overwrite, concurrency, total_count)
             VALUES ('second-active', 'legacy-library', 'PENDING', 0, 2, 0)",
        )
        .execute(&pool)
        .await
        .is_err(),
        "one library must still have at most one active danmaku job"
    );

    pool.close().await;
    Ok(())
}
