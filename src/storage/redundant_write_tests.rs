//! Guards for W3: an UPDATE that would store the values already in the row must not write.
//!
//! Every assertion here uses an observable side effect of a *write* instead of comparing values,
//! because a redundant UPDATE that stores the same value is invisible in the row itself:
//!
//! - `media_items` / `media_sources` carry `AFTER UPDATE` triggers that append to `catalog_outbox`,
//!   so a skipped write cannot add an outbox row;
//! - the guarded statements also stamp `updated_at = unixepoch()`, so the tests pin that column to
//!   a sentinel value that `unixepoch()` can never return and check that it survives.
//!
//! `sqlite3_total_changes()` is not used: the storage pool owns several connections, so the counter
//! of the connection the test happens to read is not the counter of the connection that wrote.

use super::*;

use crate::{application::libraries::LibraryService, domain::ids::LibraryId, library::LibraryKind};

/// `updated_at` sentinel. Any statement under test stamps `unixepoch()`, which is always larger.
const WRITE_STAMP: i64 = 4_242_424;

async fn outbox_rows(
    database: &Database,
    item_id: &str,
) -> Result<i64, Box<dyn std::error::Error>> {
    let count =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM catalog_outbox WHERE item_id = ?")
            .bind(item_id)
            .fetch_one(database.pool())
            .await?;
    Ok(count)
}

async fn media_item_updated_at(
    database: &Database,
    item_id: &str,
) -> Result<i64, Box<dyn std::error::Error>> {
    let updated_at =
        sqlx::query_scalar::<_, i64>("SELECT updated_at FROM media_items WHERE id = ?")
            .bind(item_id)
            .fetch_one(database.pool())
            .await?;
    Ok(updated_at)
}

async fn media_source_updated_at(
    database: &Database,
    filesystem_entry_id: &str,
) -> Result<i64, Box<dyn std::error::Error>> {
    let updated_at = sqlx::query_scalar::<_, i64>(
        "SELECT updated_at FROM media_sources WHERE filesystem_entry_id = ?",
    )
    .bind(filesystem_entry_id)
    .fetch_one(database.pool())
    .await?;
    Ok(updated_at)
}

async fn stamp_media_item(
    database: &Database,
    item_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query("UPDATE media_items SET updated_at = ? WHERE id = ?")
        .bind(WRITE_STAMP)
        .bind(item_id)
        .execute(database.pool())
        .await?;
    Ok(())
}

async fn stamp_media_source(
    database: &Database,
    filesystem_entry_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query("UPDATE media_sources SET updated_at = ? WHERE filesystem_entry_id = ?")
        .bind(WRITE_STAMP)
        .bind(filesystem_entry_id)
        .execute(database.pool())
        .await?;
    Ok(())
}

struct LibraryFixture {
    _temp_dir: tempfile::TempDir,
    database: Database,
    library_id: LibraryId,
    root_id: String,
}

/// A movie library with one root, the fixture every write guard needs.
async fn movie_library_fixture() -> Result<LibraryFixture, Box<dyn std::error::Error>> {
    let (temp_dir, database) = test_database().await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Write guards", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&media_root).await?;
    let root = libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?
        .root;
    Ok(LibraryFixture {
        _temp_dir: temp_dir,
        database,
        library_id: library.id,
        root_id: root.id.to_string(),
    })
}

/// A `movie.mkv` filesystem entry with one local source pointing at `item_id`.
async fn insert_local_movie_source(
    fixture: &LibraryFixture,
    entry_id: &str,
    source_id: &str,
    item_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let library_id = fixture.library_id.to_string();
    fixture
        .database
        .insert_media_item(NewMediaItem {
            id: item_id,
            library_id: &library_id,
            title: "Movie",
            sort_title: "movie",
            original_title: Some("Movie"),
            production_year: Some(2020),
            provider_ids_json: None,
        })
        .await?;
    fixture
        .database
        .insert_filesystem_entry(NewFilesystemEntry {
            id: entry_id,
            library_root_id: &fixture.root_id,
            relative_path: "movie.mkv",
            entry_kind: "FILE",
            size: 10,
            modified_at: 20,
            device: None,
            inode: Some(30),
            fingerprint: b"fingerprint",
            last_seen_generation: "generation-1",
        })
        .await?;
    fixture
        .database
        .insert_media_source(NewMediaSource {
            id: source_id,
            item_id,
            source_kind: "LOCAL_FILE",
            filesystem_entry_id: entry_id,
            edition_name: None,
            quality_label: None,
            container: "mkv",
            size: 10,
            external_url: None,
            strm_target_kind: None,
            is_default: true,
        })
        .await?;
    Ok(())
}

/// A `.strm` filesystem entry with one URL source pointing at `item_id`.
async fn insert_strm_source(
    fixture: &LibraryFixture,
    entry_id: &str,
    source_id: &str,
    item_id: &str,
    external_url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let library_id = fixture.library_id.to_string();
    fixture
        .database
        .insert_media_item(NewMediaItem {
            id: item_id,
            library_id: &library_id,
            title: "Stream",
            sort_title: "stream",
            original_title: Some("Stream"),
            production_year: Some(2020),
            provider_ids_json: None,
        })
        .await?;
    fixture
        .database
        .insert_filesystem_entry(NewFilesystemEntry {
            id: entry_id,
            library_root_id: &fixture.root_id,
            relative_path: "stream.strm",
            entry_kind: "FILE",
            size: 12,
            modified_at: 34,
            device: None,
            inode: None,
            fingerprint: b"fingerprint",
            last_seen_generation: "generation-1",
        })
        .await?;
    fixture
        .database
        .insert_media_source(NewMediaSource {
            id: source_id,
            item_id,
            source_kind: "STRM_URL",
            filesystem_entry_id: entry_id,
            edition_name: None,
            quality_label: None,
            container: "strm",
            size: 12,
            external_url: Some(external_url),
            strm_target_kind: Some("URL"),
            is_default: true,
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn redundant_strm_target_write_leaves_the_source_row_untouched()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = movie_library_fixture().await?;
    let database = &fixture.database;
    insert_strm_source(
        &fixture,
        "strm-entry",
        "strm-source",
        "strm-item",
        "http://example.invalid/a.mkv",
    )
    .await?;
    database
        .update_media_source_strm_target(
            "strm-entry",
            Some("URL"),
            Some("http://example.invalid/a.mkv"),
        )
        .await?;

    stamp_media_source(database, "strm-entry").await?;
    let outbox_before = outbox_rows(database, "strm-item").await?;
    database
        .update_media_source_strm_target(
            "strm-entry",
            Some("URL"),
            Some("http://example.invalid/a.mkv"),
        )
        .await?;
    assert_eq!(
        media_source_updated_at(database, "strm-entry").await?,
        WRITE_STAMP,
        "an unchanged STRM target must not rewrite the media source row"
    );
    assert_eq!(
        outbox_rows(database, "strm-item").await?,
        outbox_before,
        "an unchanged STRM target must not fire the catalog outbox trigger"
    );

    database
        .update_media_source_strm_target(
            "strm-entry",
            Some("URL"),
            Some("http://example.invalid/b.mkv"),
        )
        .await?;
    assert_ne!(
        media_source_updated_at(database, "strm-entry").await?,
        WRITE_STAMP,
        "a changed STRM target must still be written"
    );
    assert!(
        outbox_rows(database, "strm-item").await? > outbox_before,
        "a changed STRM target must still fire the catalog outbox trigger"
    );
    let stored_url: Option<String> = sqlx::query_scalar::<_, _>(
        "SELECT external_url FROM media_sources WHERE id = 'strm-source'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored_url.as_deref(), Some("http://example.invalid/b.mkv"));
    Ok(())
}

#[tokio::test]
async fn redundant_variant_label_write_leaves_the_source_row_untouched()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = movie_library_fixture().await?;
    let database = &fixture.database;
    insert_local_movie_source(&fixture, "label-entry", "label-source", "label-item").await?;

    database
        .update_media_source_variant_labels("label-entry", Some("Director's cut"), Some("1080p"))
        .await?;
    stamp_media_source(database, "label-entry").await?;
    let outbox_before = outbox_rows(database, "label-item").await?;
    database
        .update_media_source_variant_labels("label-entry", Some("Director's cut"), Some("1080p"))
        .await?;
    assert_eq!(
        media_source_updated_at(database, "label-entry").await?,
        WRITE_STAMP,
        "unchanged variant labels must not rewrite the media source row"
    );
    assert_eq!(outbox_rows(database, "label-item").await?, outbox_before);

    database
        .update_media_source_variant_labels("label-entry", Some("Director's cut"), Some("2160p"))
        .await?;
    assert_ne!(
        media_source_updated_at(database, "label-entry").await?,
        WRITE_STAMP,
        "changed variant labels must still be written"
    );
    let stored_label: Option<String> = sqlx::query_scalar::<_, _>(
        "SELECT quality_label FROM media_sources WHERE id = 'label-source'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored_label.as_deref(), Some("2160p"));
    Ok(())
}

#[tokio::test]
async fn restore_of_a_live_item_does_not_write_and_restores_removed_ones()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = movie_library_fixture().await?;
    let database = &fixture.database;
    insert_local_movie_source(&fixture, "restore-entry", "restore-source", "restore-item").await?;

    stamp_media_item(database, "restore-item").await?;
    let outbox_before = outbox_rows(database, "restore-item").await?;
    database.restore_media_item("restore-item").await?;
    assert_eq!(
        media_item_updated_at(database, "restore-item").await?,
        WRITE_STAMP,
        "restoring an item that is not removed must not write it"
    );
    assert_eq!(outbox_rows(database, "restore-item").await?, outbox_before);

    sqlx::query("UPDATE media_items SET removed_at = unixepoch() WHERE id = 'restore-item'")
        .execute(database.pool())
        .await?;
    stamp_media_item(database, "restore-item").await?;
    let outbox_before = outbox_rows(database, "restore-item").await?;
    database.restore_media_item("restore-item").await?;
    assert_ne!(
        media_item_updated_at(database, "restore-item").await?,
        WRITE_STAMP,
        "a removed item must still be restored"
    );
    assert!(outbox_rows(database, "restore-item").await? > outbox_before);
    let removed_at: Option<i64> =
        sqlx::query_scalar::<_, _>("SELECT removed_at FROM media_items WHERE id = 'restore-item'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(removed_at, None);
    Ok(())
}

#[tokio::test]
async fn marking_an_already_missing_entry_does_not_write_it()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = movie_library_fixture().await?;
    let database = &fixture.database;
    insert_local_movie_source(&fixture, "missing-entry", "missing-source", "missing-item").await?;

    database
        .mark_filesystem_entry_missing_by_path(&fixture.root_id, "movie.mkv")
        .await?;
    let first_stamp: i64 = sqlx::query_scalar::<_, _>(
        "SELECT updated_at FROM filesystem_entries WHERE id = 'missing-entry'",
    )
    .fetch_one(database.pool())
    .await?;
    sqlx::query("UPDATE filesystem_entries SET updated_at = ? WHERE id = 'missing-entry'")
        .bind(WRITE_STAMP)
        .execute(database.pool())
        .await?;
    database
        .mark_filesystem_entry_missing_by_path(&fixture.root_id, "movie.mkv")
        .await?;
    let second_stamp: i64 = sqlx::query_scalar::<_, _>(
        "SELECT updated_at FROM filesystem_entries WHERE id = 'missing-entry'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        second_stamp, WRITE_STAMP,
        "marking an already missing entry must not write it again"
    );
    assert!(
        first_stamp != WRITE_STAMP,
        "the first mark must still write the entry"
    );

    sqlx::query("UPDATE filesystem_entries SET is_missing = 0 WHERE id = 'missing-entry'")
        .execute(database.pool())
        .await?;
    database
        .mark_filesystem_entry_missing_by_path(&fixture.root_id, "movie.mkv")
        .await?;
    let is_missing: i64 = sqlx::query_scalar::<_, _>(
        "SELECT is_missing FROM filesystem_entries WHERE id = 'missing-entry'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(is_missing, 1);
    Ok(())
}

#[tokio::test]
async fn unchanged_incremental_strm_seen_does_not_rewrite_the_source()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = movie_library_fixture().await?;
    let database = &fixture.database;
    insert_strm_source(
        &fixture,
        "seen-entry",
        "seen-source",
        "seen-item",
        "http://example.invalid/a.mkv",
    )
    .await?;

    database
        .mark_filesystem_entry_seen_with_strm_target(
            "seen-entry",
            "generation-2",
            None,
            None,
            Some("URL"),
            Some("http://example.invalid/a.mkv"),
        )
        .await?;
    stamp_media_source(database, "seen-entry").await?;
    let outbox_before = outbox_rows(database, "seen-item").await?;
    database
        .mark_filesystem_entry_seen_with_strm_target(
            "seen-entry",
            "generation-3",
            None,
            None,
            Some("URL"),
            Some("http://example.invalid/a.mkv"),
        )
        .await?;
    assert_eq!(
        media_source_updated_at(database, "seen-entry").await?,
        WRITE_STAMP,
        "an unchanged incremental STRM file must not rewrite its media source"
    );
    assert_eq!(outbox_rows(database, "seen-item").await?, outbox_before);

    database
        .mark_filesystem_entry_seen_with_strm_target(
            "seen-entry",
            "generation-4",
            None,
            None,
            Some("URL"),
            Some("http://example.invalid/b.mkv"),
        )
        .await?;
    assert_ne!(
        media_source_updated_at(database, "seen-entry").await?,
        WRITE_STAMP,
        "a changed incremental STRM target must still be written"
    );
    Ok(())
}

#[tokio::test]
async fn repeating_a_metadata_check_with_the_same_fingerprint_does_not_write()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = movie_library_fixture().await?;
    let database = &fixture.database;
    insert_local_movie_source(&fixture, "check-entry", "check-source", "check-item").await?;

    database
        .mark_media_item_metadata_checked("check-item", b"metadata-1", Some("collection-1"))
        .await?;
    stamp_media_item(database, "check-item").await?;
    let outbox_before = outbox_rows(database, "check-item").await?;
    database
        .mark_media_item_metadata_checked("check-item", b"metadata-1", Some("collection-1"))
        .await?;
    assert_eq!(
        media_item_updated_at(database, "check-item").await?,
        WRITE_STAMP,
        "re-checking the same metadata fingerprint must not write the item"
    );
    assert_eq!(outbox_rows(database, "check-item").await?, outbox_before);

    database
        .mark_media_item_metadata_checked("check-item", b"metadata-2", Some("collection-2"))
        .await?;
    assert_ne!(
        media_item_updated_at(database, "check-item").await?,
        WRITE_STAMP,
        "a new metadata fingerprint must still be written"
    );
    let stored_key: Option<String> = sqlx::query_scalar::<_, _>(
        "SELECT nfo_collection_reference_key FROM media_items WHERE id = 'check-item'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored_key.as_deref(), Some("collection-2"));
    Ok(())
}

#[tokio::test]
async fn repeating_an_unchanged_nfo_state_sync_does_not_write()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = movie_library_fixture().await?;
    let database = &fixture.database;
    insert_local_movie_source(&fixture, "state-entry", "state-source", "state-item").await?;

    database
        .sync_media_item_nfo_state("state-item", b"nfo-1", b"metadata-1")
        .await?;
    let outbox_before = outbox_rows(database, "state-item").await?;
    database
        .sync_media_item_nfo_state("state-item", b"nfo-1", b"metadata-1")
        .await?;
    assert_eq!(
        outbox_rows(database, "state-item").await?,
        outbox_before,
        "an unchanged NFO state sync must not rewrite the item"
    );

    // A stale cached NFO fingerprint must still be dropped by the next sync.
    sqlx::query(
        "UPDATE media_items SET nfo_metadata_fingerprint = x'dead' WHERE id = 'state-item'",
    )
    .execute(database.pool())
    .await?;
    let outbox_before = outbox_rows(database, "state-item").await?;
    database
        .sync_media_item_nfo_state("state-item", b"nfo-1", b"metadata-1")
        .await?;
    assert!(
        outbox_rows(database, "state-item").await? > outbox_before,
        "a stale NFO fingerprint must still be cleared by a write"
    );
    let nfo_fingerprint: Option<Vec<u8>> = sqlx::query_scalar::<_, Option<Vec<u8>>>(
        "SELECT nfo_metadata_fingerprint FROM media_items WHERE id = 'state-item'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(nfo_fingerprint, None);

    // A new metadata fingerprint must still be stored.
    let outbox_before = outbox_rows(database, "state-item").await?;
    database
        .sync_media_item_nfo_state("state-item", b"nfo-1", b"metadata-2")
        .await?;
    assert!(
        outbox_rows(database, "state-item").await? > outbox_before,
        "a changed metadata fingerprint must still be written"
    );
    let metadata_fingerprint: Option<Vec<u8>> = sqlx::query_scalar::<_, Option<Vec<u8>>>(
        "SELECT metadata_fingerprint FROM media_items WHERE id = 'state-item'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        metadata_fingerprint.as_deref(),
        Some(b"metadata-2".as_slice())
    );
    Ok(())
}

#[tokio::test]
async fn requeueing_already_pending_strm_sources_does_not_write()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = movie_library_fixture().await?;
    let database = &fixture.database;
    insert_strm_source(
        &fixture,
        "probe-entry",
        "probe-source",
        "probe-item",
        "http://example.invalid/a.mkv",
    )
    .await?;
    sqlx::query(
        "UPDATE media_sources SET probe_status = 'PENDING', probe_error = NULL
         WHERE id = 'probe-source'",
    )
    .execute(database.pool())
    .await?;
    let library_id = fixture.library_id.to_string();

    stamp_media_source(database, "probe-entry").await?;
    let outbox_before = outbox_rows(database, "probe-item").await?;
    let mut transaction = database.pool().begin().await?;
    database
        .mark_strm_probe_sources_pending_in_transaction(
            &mut transaction,
            &library_id,
            None,
            Some("probe-source"),
        )
        .await?;
    transaction.commit().await?;
    assert_eq!(
        media_source_updated_at(database, "probe-entry").await?,
        WRITE_STAMP,
        "requeueing an already pending STRM source must not write it"
    );
    assert_eq!(outbox_rows(database, "probe-item").await?, outbox_before);

    sqlx::query("UPDATE media_sources SET probe_status = 'READY' WHERE id = 'probe-source'")
        .execute(database.pool())
        .await?;
    let mut transaction = database.pool().begin().await?;
    database
        .mark_strm_probe_sources_pending_in_transaction(
            &mut transaction,
            &library_id,
            None,
            Some("probe-source"),
        )
        .await?;
    transaction.commit().await?;
    let status: String = sqlx::query_scalar::<_, _>(
        "SELECT probe_status FROM media_sources WHERE id = 'probe-source'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(status, "PENDING");
    assert_ne!(
        media_source_updated_at(database, "probe-entry").await?,
        WRITE_STAMP,
        "a ready STRM source must still be requeued"
    );
    Ok(())
}

#[tokio::test]
async fn global_collection_resync_without_changes_does_not_rewrite_the_work_item()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = movie_library_fixture().await?;
    let database = &fixture.database;
    let library_id = fixture.library_id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status,
             has_available_source
         ) VALUES ('work-source-item', ?, 'MOVIE', 'Movie', 'movie', 'ONLINE_CONFIRMED', 1)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO collection_item_references (
             item_id, movie_provider, movie_provider_id, collection_provider,
             collection_provider_id, collection_title, sort_order
         ) VALUES ('work-source-item', 'tmdb', 'movie-1', 'tmdb', 'series-1', 'Series', 2020)",
    )
    .execute(database.pool())
    .await?;

    assert!(
        database
            .sync_global_collection("tmdb", "series-1", 1)
            .await?
    );
    let work_item_id: String = sqlx::query_scalar::<_, _>(
        "SELECT work_item_id FROM collection_work_items
             WHERE movie_provider = 'tmdb' AND movie_provider_id = 'movie-1'",
    )
    .fetch_one(database.pool())
    .await?;

    // A rescan of the same collection queues the same membership again.
    sqlx::query(
        "INSERT INTO collection_sync_outbox (collection_provider, collection_provider_id, generation)
         VALUES ('tmdb', 'series-1', 5)",
    )
    .execute(database.pool())
    .await?;
    stamp_media_item(database, &work_item_id).await?;
    let outbox_before = outbox_rows(database, &work_item_id).await?;
    assert!(
        database
            .sync_global_collection("tmdb", "series-1", 5)
            .await?
    );
    assert_eq!(
        media_item_updated_at(database, &work_item_id).await?,
        WRITE_STAMP,
        "a collection resync with identical members must not rewrite the work item"
    );
    assert_eq!(
        outbox_rows(database, &work_item_id).await?,
        outbox_before,
        "a collection resync with identical members must not fire the catalog outbox trigger"
    );

    // A real metadata change must still reach the generated work item.
    sqlx::query(
        "UPDATE media_items SET title = 'Renamed', sort_title = 'renamed'
                 WHERE id = 'work-source-item'",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO collection_sync_outbox (collection_provider, collection_provider_id, generation)
         VALUES ('tmdb', 'series-1', 6)",
    )
    .execute(database.pool())
    .await?;
    assert!(
        database
            .sync_global_collection("tmdb", "series-1", 6)
            .await?
    );
    assert_ne!(
        media_item_updated_at(database, &work_item_id).await?,
        WRITE_STAMP,
        "a changed collection member must still rewrite the work item"
    );
    let work_title: String =
        sqlx::query_scalar::<_, _>("SELECT title FROM media_items WHERE id = ?")
            .bind(&work_item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(work_title, "Renamed");
    Ok(())
}
