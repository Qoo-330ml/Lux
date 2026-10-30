//! Imported playback history has to survive the item it belongs to: deleting a library must not
//! erase the record of what the user watched, and re-adding the same title elsewhere must bring it
//! back exactly once.

use std::time::Duration;

use luxd::{config::Config, storage::Database};

const DAY_SECONDS: i64 = 86_400;

async fn test_database(
    temp_dir: &tempfile::TempDir,
) -> Result<Database, Box<dyn std::error::Error>> {
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    Ok(Database::connect(&config).await?)
}

async fn seed_library(database: &Database, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES (?, ?, 'MOVIE')")
        .bind(id)
        .bind(id)
        .execute(database.pool())
        .await?;
    Ok(())
}

async fn seed_user(database: &Database, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES (?, ?, ?, 'not-a-real-hash')",
    )
    .bind(id)
    .bind(id)
    .bind(id)
    .execute(database.pool())
    .await?;
    Ok(())
}

async fn seed_item(
    database: &Database,
    id: &str,
    library_id: &str,
    removed_at: Option<i64>,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status, removed_at
         ) VALUES (?, ?, 'MOVIE', ?, ?, 'ONLINE_CONFIRMED', ?)",
    )
    .bind(id)
    .bind(library_id)
    .bind(id)
    .bind(id)
    .bind(removed_at)
    .execute(database.pool())
    .await?;
    Ok(())
}

async fn seed_identity(
    database: &Database,
    item_id: &str,
    provider: &str,
    provider_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO media_item_provider_ids (media_item_id, item_type, provider, provider_id)
         VALUES (?, 'MOVIE', ?, ?)",
    )
    .bind(item_id)
    .bind(provider)
    .bind(provider_id)
    .execute(database.pool())
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn seed_event(
    database: &Database,
    id: &str,
    user_id: &str,
    item_id: &str,
    event_type: &str,
    occurred_at: i64,
    source_event_key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO playback_history_events (
             id, user_id, item_id, event_type, position_ticks, duration_ticks, occurred_at,
             source, source_event_key
         ) VALUES (?, ?, ?, ?, 120, 3600, ?, 'emby-import', ?)",
    )
    .bind(id)
    .bind(user_id)
    .bind(item_id)
    .bind(event_type)
    .bind(occurred_at)
    .bind(source_event_key)
    .execute(database.pool())
    .await?;
    Ok(())
}

async fn event_rows(
    database: &Database,
) -> Result<Vec<(String, String, String)>, Box<dyn std::error::Error>> {
    let rows = sqlx::query_as::<_, (String, String, String)>(
        "SELECT id, item_id, source_event_key FROM playback_history_events ORDER BY id",
    )
    .fetch_all(database.pool())
    .await?;
    Ok(rows)
}

async fn archived_events(
    database: &Database,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT id, source_event_key FROM playback_history_event_archive ORDER BY id",
    )
    .fetch_all(database.pool())
    .await?;
    Ok(rows)
}

async fn item_exists(
    database: &Database,
    item_id: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_items WHERE id = ?")
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    Ok(count == 1)
}

async fn seed_expired_identified_library(
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_library(database, "library-old").await?;
    seed_user(database, "user-1").await?;
    seed_item(
        database,
        "movie-old",
        "library-old",
        Some(unixepoch() - 40 * DAY_SECONDS),
    )
    .await?;
    seed_identity(database, "movie-old", "tmdb", "680").await?;
    seed_event(
        database,
        "event-start",
        "user-1",
        "movie-old",
        "PLAY_STARTED",
        1_700_000_000,
        "emby-event-1",
    )
    .await?;
    seed_event(
        database,
        "event-stop",
        "user-1",
        "movie-old",
        "STOPPED",
        1_700_000_600,
        "emby-event-2",
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn purge_archives_playback_history_before_deleting_the_item()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_expired_identified_library(&database).await?;

    let purged = database
        .purge_expired_removed_media_items(30 * DAY_SECONDS, 100, 1, Duration::ZERO)
        .await?;

    assert_eq!(purged, 1);
    assert!(
        !item_exists(&database, "movie-old").await?,
        "an identified tombstone must not be pinned by its history any more"
    );
    assert!(
        event_rows(&database).await?.is_empty(),
        "the cascade removes the rows it copied out"
    );
    assert_eq!(
        archived_events(&database).await?,
        vec![
            ("event-start".to_owned(), "emby-event-1".to_owned()),
            ("event-stop".to_owned(), "emby-event-2".to_owned()),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn archived_history_returns_with_the_same_title_in_another_library()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_expired_identified_library(&database).await?;
    database
        .purge_expired_removed_media_items(30 * DAY_SECONDS, 100, 1, Duration::ZERO)
        .await?;

    seed_library(&database, "library-new").await?;
    seed_item(&database, "movie-new", "library-new", None).await?;
    seed_identity(&database, "movie-new", "tmdb", "680").await?;

    let restored = database.restore_detached_playback_history(200).await?;

    assert_eq!(restored, 2);
    assert_eq!(
        event_rows(&database).await?,
        vec![
            (
                "event-start".to_owned(),
                "movie-new".to_owned(),
                "emby-event-1".to_owned()
            ),
            (
                "event-stop".to_owned(),
                "movie-new".to_owned(),
                "emby-event-2".to_owned()
            ),
        ],
        "the history has to follow the identity, not the old item id"
    );
    assert!(
        archived_events(&database).await?.is_empty(),
        "a consumed archive row must not be applied twice"
    );

    let restored_again = database.restore_detached_playback_history(200).await?;
    assert_eq!(restored_again, 0);
    assert_eq!(event_rows(&database).await?.len(), 2, "no duplicate rows");
    Ok(())
}

#[tokio::test]
async fn reattaching_never_duplicates_history_that_still_exists()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_library(&database, "library-new").await?;
    seed_user(&database, "user-1").await?;
    seed_item(&database, "movie-new", "library-new", None).await?;
    seed_identity(&database, "movie-new", "tmdb", "680").await?;
    // The same import key is already on the live item: a re-import happened before the old row was
    // archived, so reattaching has to keep exactly one row.
    seed_event(
        &database,
        "event-existing",
        "user-1",
        "movie-new",
        "PLAY_STARTED",
        1_700_000_000,
        "emby-event-1",
    )
    .await?;
    sqlx::query(
        "INSERT INTO playback_history_event_archive (
             id, item_type, provider, provider_id, user_id, event_type, position_ticks,
             duration_ticks, occurred_at, source, source_event_key, source_item_id
         ) VALUES ('event-archived', 'MOVIE', 'tmdb', '680', 'user-1', 'PLAY_STARTED', 120, 3600,
                   1700000000, 'emby-import', 'emby-event-1', 'movie-old')",
    )
    .execute(database.pool())
    .await?;

    let restored = database.restore_detached_playback_history(200).await?;

    assert_eq!(restored, 0, "the importer key already exists");
    assert_eq!(
        event_rows(&database).await?.len(),
        1,
        "history must not be counted twice"
    );
    assert!(
        archived_events(&database).await?.is_empty(),
        "an event that is already present is still consumed"
    );
    Ok(())
}

#[tokio::test]
async fn purge_keeps_an_unidentified_item_and_its_history() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_library(&database, "library-old").await?;
    seed_user(&database, "user-1").await?;
    seed_item(
        &database,
        "movie-local",
        "library-old",
        Some(unixepoch() - 40 * DAY_SECONDS),
    )
    .await?;
    seed_event(
        &database,
        "event-local",
        "user-1",
        "movie-local",
        "PLAY_STARTED",
        1_700_000_000,
        "emby-event-local",
    )
    .await?;

    let purged = database
        .purge_expired_removed_media_items(30 * DAY_SECONDS, 100, 1, Duration::ZERO)
        .await?;

    assert_eq!(
        purged, 0,
        "there is no identity to reattach this history by"
    );
    assert!(item_exists(&database, "movie-local").await?);
    assert_eq!(event_rows(&database).await?.len(), 1);
    assert!(archived_events(&database).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn archived_history_is_not_restored_onto_a_different_item_type()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_expired_identified_library(&database).await?;
    database
        .purge_expired_removed_media_items(30 * DAY_SECONDS, 100, 1, Duration::ZERO)
        .await?;

    // TMDb 680 is also a series id; history of the movie must not land on the series.
    seed_library(&database, "library-new").await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('series-new', 'library-new', 'SERIES', 'Series', 'series', 'ONLINE_CONFIRMED')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_item_provider_ids (media_item_id, item_type, provider, provider_id)
         VALUES ('series-new', 'SERIES', 'tmdb', '680')",
    )
    .execute(database.pool())
    .await?;

    let restored = database.restore_detached_playback_history(200).await?;

    assert_eq!(restored, 0);
    assert!(event_rows(&database).await?.is_empty());
    assert_eq!(
        archived_events(&database).await?.len(),
        2,
        "the history waits for the movie to come back"
    );
    Ok(())
}

fn unixepoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}
