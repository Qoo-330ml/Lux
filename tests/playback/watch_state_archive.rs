//! Watch state has to survive the item it was recorded against: deleting a library and adding the
//! same files elsewhere must not turn watched films back into unwatched ones.

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

/// Seeds one catalog row. `removed_at` is what the retention purge looks at.
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

/// The identity index is normally filled by a trigger from `provider_ids_json`; writing it directly
/// keeps these tests about the archive rather than about identification.
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
async fn seed_state(
    database: &Database,
    user_id: &str,
    item_id: &str,
    position_ticks: i64,
    is_played: bool,
    is_favorite: bool,
    play_count: i64,
    last_played_at: Option<i64>,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO user_item_state (
             user_id, item_id, position_ticks, is_played, is_favorite, play_count, last_played_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(item_id)
    .bind(position_ticks)
    .bind(i64::from(is_played))
    .bind(i64::from(is_favorite))
    .bind(play_count)
    .bind(last_played_at)
    .execute(database.pool())
    .await?;
    Ok(())
}

type StoredState = (i64, i64, i64, i64, Option<i64>, i64);

async fn stored_state(
    database: &Database,
    user_id: &str,
    item_id: &str,
) -> Result<Option<StoredState>, Box<dyn std::error::Error>> {
    let row = sqlx::query_as::<_, (i64, i64, i64, i64, Option<i64>, i64)>(
        "SELECT position_ticks, is_played, is_favorite, play_count, last_played_at, version
         FROM user_item_state WHERE user_id = ? AND item_id = ?",
    )
    .bind(user_id)
    .bind(item_id)
    .fetch_optional(database.pool())
    .await?;
    Ok(row)
}

async fn archived_rows(
    database: &Database,
) -> Result<Vec<(String, String, String, i64, i64)>, Box<dyn std::error::Error>> {
    let rows = sqlx::query_as::<_, (String, String, String, i64, i64)>(
        "SELECT provider, provider_id, item_type, is_played, play_count
         FROM user_item_state_archive ORDER BY provider",
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

#[tokio::test]
async fn purge_archives_watch_state_before_deleting_a_removed_item()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_library(&database, "library-old").await?;
    seed_user(&database, "user-1").await?;
    seed_item(
        &database,
        "movie-old",
        "library-old",
        Some(unixepoch() - 40 * DAY_SECONDS),
    )
    .await?;
    seed_identity(&database, "movie-old", "tmdb", "550").await?;
    seed_state(
        &database,
        "user-1",
        "movie-old",
        0,
        true,
        true,
        3,
        Some(1_700_000_000),
    )
    .await?;

    let purged = database
        .purge_expired_removed_media_items(30 * DAY_SECONDS, 100, 1, Duration::ZERO)
        .await?;

    assert_eq!(purged, 1, "the identified tombstone must be reclaimable");
    assert!(
        !item_exists(&database, "movie-old").await?,
        "the item row still exists, so the retention purge did not run"
    );
    let archived = archived_rows(&database).await?;
    assert_eq!(
        archived,
        vec![(
            "tmdb".to_owned(),
            "550".to_owned(),
            "MOVIE".to_owned(),
            1,
            3
        )],
        "played flag and play count must survive the delete"
    );
    Ok(())
}

#[tokio::test]
async fn purge_keeps_items_without_a_provider_identity() -> Result<(), Box<dyn std::error::Error>> {
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
    seed_state(
        &database,
        "user-1",
        "movie-local",
        60,
        false,
        false,
        0,
        Some(1),
    )
    .await?;

    let purged = database
        .purge_expired_removed_media_items(30 * DAY_SECONDS, 100, 1, Duration::ZERO)
        .await?;

    assert_eq!(purged, 0, "nothing matches, so nothing may be dropped");
    assert!(
        item_exists(&database, "movie-local").await?,
        "an unidentified item has no identity to reattach its state by"
    );
    assert!(archived_rows(&database).await?.is_empty());
    assert!(
        stored_state(&database, "user-1", "movie-local")
            .await?
            .is_some(),
        "the state must still be reachable while the item is kept"
    );
    Ok(())
}

#[tokio::test]
async fn archived_state_is_reattached_to_the_same_title_in_another_library()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_library(&database, "library-old").await?;
    seed_library(&database, "library-new").await?;
    seed_user(&database, "user-1").await?;
    seed_item(
        &database,
        "movie-old",
        "library-old",
        Some(unixepoch() - 40 * DAY_SECONDS),
    )
    .await?;
    seed_identity(&database, "movie-old", "tmdb", "550").await?;
    seed_state(
        &database,
        "user-1",
        "movie-old",
        0,
        true,
        false,
        2,
        Some(1_700_000_000),
    )
    .await?;
    database
        .purge_expired_removed_media_items(30 * DAY_SECONDS, 100, 1, Duration::ZERO)
        .await?;

    // The same film is added again, in a different library, with a new item id.
    seed_item(&database, "movie-new", "library-new", None).await?;
    seed_identity(&database, "movie-new", "tmdb", "550").await?;

    let restored = database.restore_detached_watch_state(100).await?;

    assert_eq!(restored, 1);
    let state = stored_state(&database, "user-1", "movie-new")
        .await?
        .expect("restored state");
    assert_eq!(state.0, 0, "position of a fully played item");
    assert_eq!(state.1, 1, "played flag must come back");
    assert_eq!(state.2, 0);
    assert_eq!(state.3, 2, "play count must come back");
    assert_eq!(state.4, Some(1_700_000_000));
    assert!(
        archived_rows(&database).await?.is_empty(),
        "a consumed archive row must not be applied twice"
    );
    Ok(())
}

#[tokio::test]
async fn archived_state_never_lowers_what_the_new_item_already_has()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_library(&database, "library-old").await?;
    seed_library(&database, "library-new").await?;
    seed_user(&database, "user-1").await?;
    seed_item(
        &database,
        "movie-old",
        "library-old",
        Some(unixepoch() - 40 * DAY_SECONDS),
    )
    .await?;
    seed_identity(&database, "movie-old", "tmdb", "550").await?;
    seed_state(
        &database,
        "user-1",
        "movie-old",
        0,
        true,
        false,
        1,
        Some(1_600_000_000),
    )
    .await?;
    database
        .purge_expired_removed_media_items(30 * DAY_SECONDS, 100, 1, Duration::ZERO)
        .await?;

    seed_item(&database, "movie-new", "library-new", None).await?;
    seed_identity(&database, "movie-new", "tmdb", "550").await?;
    // The new copy already carries a longer watch history than the archived one.
    seed_state(
        &database,
        "user-1",
        "movie-new",
        0,
        true,
        true,
        5,
        Some(1_800_000_000),
    )
    .await?;

    let version_before = stored_state(&database, "user-1", "movie-new")
        .await?
        .expect("existing state")
        .5;
    let restored = database.restore_detached_watch_state(100).await?;

    assert_eq!(restored, 0, "nothing had to change");
    let state = stored_state(&database, "user-1", "movie-new")
        .await?
        .expect("existing state");
    assert_eq!(state.3, 5, "the higher play count wins");
    assert_eq!(state.2, 1, "the favourite flag is sticky");
    assert_eq!(
        state.5, version_before,
        "an unchanged row keeps its version"
    );
    Ok(())
}

#[tokio::test]
async fn state_of_a_soft_deleted_item_reaches_its_live_twin_without_being_consumed()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_library(&database, "library-a").await?;
    seed_library(&database, "library-b").await?;
    seed_user(&database, "user-1").await?;
    // The file moved: the scan soft-deleted the old item and created a new one for the new path.
    seed_item(
        &database,
        "movie-moved",
        "library-a",
        Some(unixepoch() - DAY_SECONDS),
    )
    .await?;
    seed_identity(&database, "movie-moved", "tmdb", "603").await?;
    seed_state(
        &database,
        "user-1",
        "movie-moved",
        600,
        false,
        false,
        0,
        Some(1_700_000_500),
    )
    .await?;
    seed_item(&database, "movie-here", "library-b", None).await?;
    seed_identity(&database, "movie-here", "tmdb", "603").await?;

    let restored = database.restore_detached_watch_state(100).await?;

    assert_eq!(restored, 1);
    let target = stored_state(&database, "user-1", "movie-here")
        .await?
        .expect("state copied onto the live item");
    assert_eq!(target.0, 600, "the resume position has to move across");
    assert_eq!(target.4, Some(1_700_000_500));
    assert!(
        stored_state(&database, "user-1", "movie-moved")
            .await?
            .is_some(),
        "the source item keeps its own state in case its file comes back"
    );

    // A second pass has nothing left to do and must not churn the version that caches key on.
    let version_after_first_pass = target.5;
    let restored = database.restore_detached_watch_state(100).await?;
    assert_eq!(restored, 0, "the pass has to converge");
    assert_eq!(
        stored_state(&database, "user-1", "movie-here")
            .await?
            .expect("state")
            .5,
        version_after_first_pass
    );
    Ok(())
}

#[tokio::test]
async fn archived_state_waits_until_the_title_reappears() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_library(&database, "library-old").await?;
    seed_user(&database, "user-1").await?;
    seed_item(
        &database,
        "movie-gone",
        "library-old",
        Some(unixepoch() - 40 * DAY_SECONDS),
    )
    .await?;
    seed_identity(&database, "movie-gone", "imdb", "tt0068646").await?;
    seed_state(
        &database,
        "user-1",
        "movie-gone",
        0,
        true,
        false,
        1,
        Some(1_700_000_000),
    )
    .await?;
    database
        .purge_expired_removed_media_items(30 * DAY_SECONDS, 100, 1, Duration::ZERO)
        .await?;

    let restored = database.restore_detached_watch_state(100).await?;

    assert_eq!(restored, 0);
    assert_eq!(
        archived_rows(&database).await?,
        vec![(
            "imdb".to_owned(),
            "tt0068646".to_owned(),
            "MOVIE".to_owned(),
            1,
            1
        )],
        "the row has to stay until something is identified as the same title"
    );
    Ok(())
}

#[tokio::test]
async fn a_movie_and_a_series_sharing_a_number_stay_separate()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_library(&database, "library-old").await?;
    seed_library(&database, "library-new").await?;
    seed_user(&database, "user-1").await?;
    seed_item(
        &database,
        "movie-old",
        "library-old",
        Some(unixepoch() - 40 * DAY_SECONDS),
    )
    .await?;
    seed_identity(&database, "movie-old", "tmdb", "1399").await?;
    seed_state(
        &database,
        "user-1",
        "movie-old",
        0,
        true,
        false,
        1,
        Some(1_700_000_000),
    )
    .await?;
    database
        .purge_expired_removed_media_items(30 * DAY_SECONDS, 100, 1, Duration::ZERO)
        .await?;

    // TMDb 1399 is a series; the archived movie must not be restored onto it.
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('series-new', 'library-new', 'SERIES', 'Series', 'series', 'ONLINE_CONFIRMED')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_item_provider_ids (media_item_id, item_type, provider, provider_id)
         VALUES ('series-new', 'SERIES', 'tmdb', '1399')",
    )
    .execute(database.pool())
    .await?;

    let restored = database.restore_detached_watch_state(100).await?;

    assert_eq!(restored, 0);
    assert!(
        stored_state(&database, "user-1", "series-new")
            .await?
            .is_none()
    );
    Ok(())
}

fn unixepoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}
