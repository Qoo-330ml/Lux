//! Moves old playback history out of SQLite into monthly JSONL files.
//!
//! `playback_history_events` grows for every playback and is only used by recent-history pages.
//! Rows older than the retention window are appended to
//! `<config>/archive/playback-history/<YYYY-MM>.jsonl`; only after the file is flushed and synced
//! are the corresponding rows deleted, so a crashed or unwritable archive keeps the data for the
//! next attempt.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::storage::Database;

/// History kept in SQLite before it is archived to JSONL.
pub(crate) const PLAYBACK_HISTORY_RETENTION_SECONDS: i64 = 90 * 24 * 60 * 60;

#[derive(Serialize)]
struct ArchivedPlaybackEvent<'a> {
    id: &'a str,
    user_id: &'a str,
    item_id: &'a str,
    event_type: &'a str,
    position_ticks: i64,
    duration_ticks: Option<i64>,
    occurred_at: i64,
    source: &'a str,
}

/// Appends one bounded batch of expired history to JSONL and deletes it from SQLite.
pub(crate) async fn archive_playback_history(
    config_dir: &Path,
    database: &Database,
    now: i64,
    batch: i64,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let cutoff = now.saturating_sub(PLAYBACK_HISTORY_RETENTION_SECONDS);
    let rows = database
        .list_playback_history_events_for_archive(cutoff, batch)
        .await?;
    if rows.is_empty() {
        return Ok(0);
    }

    let directory = config_dir.join("archive").join("playback-history");
    tokio::fs::create_dir_all(&directory).await?;
    let mut by_month: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for event in &rows {
        let mut line = serde_json::to_vec(&ArchivedPlaybackEvent {
            id: &event.id,
            user_id: &event.user_id,
            item_id: &event.item_id,
            event_type: &event.event_type,
            position_ticks: event.position_ticks,
            duration_ticks: event.duration_ticks,
            occurred_at: event.occurred_at,
            source: &event.source,
        })?;
        line.push(b'\n');
        by_month
            .entry(month_key(event.occurred_at)?)
            .or_default()
            .extend_from_slice(&line);
    }
    for (month, bytes) in by_month {
        let path = directory.join(format!("{month}.jsonl"));
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(&bytes).await?;
        file.flush().await?;
        file.sync_all().await?;
    }

    let ids = rows.into_iter().map(|row| row.id).collect::<Vec<_>>();
    Ok(database.delete_playback_history_events(&ids).await?)
}

fn month_key(occurred_at: i64) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let timestamp = time::OffsetDateTime::from_unix_timestamp(occurred_at)?;
    Ok(format!(
        "{:04}-{:02}",
        timestamp.year(),
        u8::from(timestamp.month())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::libraries::LibraryService;
    use crate::auth::users::UserStore;
    use crate::library::LibraryKind;
    use crate::storage::test_database;

    async fn fixture()
    -> Result<(tempfile::TempDir, Database, String, String), Box<dyn std::error::Error>> {
        let (temp, database) = test_database().await?;
        let admin = UserStore::new(database.clone())?
            .create_initial_admin("Archive", "Archive", "correct horse battery staple")
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Archive", LibraryKind::Movie, false)
            .await?;
        let item_id = uuid::Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status,
                 has_available_source
             ) VALUES (?, ?, 'MOVIE', 'Archive', 'archive', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(&item_id)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        Ok((temp, database, admin.id.to_string(), item_id))
    }

    async fn insert_event(
        database: &Database,
        id: &str,
        user_id: &str,
        item_id: &str,
        occurred_at: i64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        sqlx::query(
            "INSERT INTO playback_history_events (
                 id, user_id, item_id, event_type, position_ticks, duration_ticks,
                 occurred_at, source, source_event_key
             ) VALUES (?, ?, ?, 'PLAY_STARTED', 0, NULL, ?, 'test', ?)",
        )
        .bind(id)
        .bind(user_id)
        .bind(item_id)
        .bind(occurred_at)
        .bind(id)
        .execute(database.pool())
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn archives_expired_events_to_monthly_jsonl_and_deletes_them()
    -> Result<(), Box<dyn std::error::Error>> {
        let (temp, database, user_id, item_id) = fixture().await?;
        let now = 400 * 24 * 60 * 60;
        let old = now - PLAYBACK_HISTORY_RETENTION_SECONDS - 3600;
        insert_event(&database, "old-event", &user_id, &item_id, old).await?;
        insert_event(&database, "fresh-event", &user_id, &item_id, now).await?;
        let config_dir = temp.path().join("config");

        let archived = archive_playback_history(&config_dir, &database, now, 100)
            .await
            .map_err(|error| error.to_string())?;
        assert_eq!(archived, 1);

        let month = month_key(old).map_err(|error| error.to_string())?;
        let path = config_dir
            .join("archive")
            .join("playback-history")
            .join(format!("{month}.jsonl"));
        let content = tokio::fs::read_to_string(&path).await?;
        assert!(content.contains("\"id\":\"old-event\""));
        assert!(!content.contains("fresh-event"));

        let old_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM playback_history_events WHERE id = 'old-event'",
        )
        .fetch_one(database.pool())
        .await?;
        let fresh_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM playback_history_events WHERE id = 'fresh-event'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(old_count, 0);
        assert_eq!(fresh_count, 1);

        // A second pass with nothing to archive must not rewrite the file.
        assert_eq!(
            archive_playback_history(&config_dir, &database, now, 100)
                .await
                .map_err(|error| error.to_string())?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn archive_failure_keeps_the_rows_for_the_next_attempt()
    -> Result<(), Box<dyn std::error::Error>> {
        let (temp, database, user_id, item_id) = fixture().await?;
        let now = 400 * 24 * 60 * 60;
        let old = now - PLAYBACK_HISTORY_RETENTION_SECONDS - 3600;
        insert_event(&database, "kept-event", &user_id, &item_id, old).await?;
        let config_dir = temp.path().join("config");
        tokio::fs::create_dir_all(&config_dir).await?;
        // A file where the archive directory should be makes the write fail deterministically.
        tokio::fs::write(config_dir.join("archive"), b"not a directory").await?;

        let result = archive_playback_history(&config_dir, &database, now, 100).await;
        assert!(result.is_err(), "archive write must surface the failure");
        let kept: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM playback_history_events WHERE id = 'kept-event'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(kept, 1, "rows may only be deleted after a durable archive");
        Ok(())
    }
}
