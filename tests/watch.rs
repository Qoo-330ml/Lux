use std::{path::PathBuf, time::Duration};

use luxd::application::watch::{ChangeKind, EventCoalescer, FileChange, LibraryWatcher};
use luxd::{
    application::{libraries::LibraryService, watch::LibraryWatchService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[test]
fn coalescer_merges_same_path_and_keeps_distinct_paths() {
    let path = PathBuf::from("/media/Movie.mkv");
    let other = PathBuf::from("/media/Movie.nfo");
    let mut coalescer = EventCoalescer::new(Duration::from_millis(10));
    coalescer.push(FileChange {
        path: path.clone(),
        kind: ChangeKind::Create,
    });
    coalescer.push(FileChange {
        path: path.clone(),
        kind: ChangeKind::Modify,
    });
    coalescer.push(FileChange {
        path: other.clone(),
        kind: ChangeKind::Remove,
    });
    assert_eq!(
        coalescer.finish(),
        vec![
            FileChange {
                path,
                kind: ChangeKind::Create,
            },
            FileChange {
                path: other,
                kind: ChangeKind::Remove,
            },
        ]
    );
    assert_eq!(LibraryWatcher::channel_capacity(), 256);
}

#[tokio::test]
async fn watcher_receives_temp_directory_changes() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let mut watcher = LibraryWatcher::new(temp_dir.path())?;
    assert!(watcher.watcher_alive());
    let file = temp_dir.path().join("Movie.mkv");
    tokio::fs::write(&file, b"first").await?;
    tokio::time::sleep(Duration::from_millis(20)).await;
    tokio::fs::write(&file, b"second").await?;
    let canonical_file = std::fs::canonicalize(&file)?;
    let batch = tokio::time::timeout(Duration::from_secs(3), watcher.next_batch())
        .await?
        .ok_or("watcher closed")?;
    assert!(batch.iter().any(|change| change.path == canonical_file));
    assert!(
        batch.iter().any(|change| {
            change.kind == ChangeKind::Create || change.kind == ChangeKind::Modify
        })
    );

    let renamed = temp_dir.path().join("Renamed.Movie.mkv");
    tokio::fs::rename(&file, &renamed).await?;
    let canonical_renamed = temp_dir.path().canonicalize()?.join("Renamed.Movie.mkv");
    let rename_batch = tokio::time::timeout(Duration::from_secs(3), watcher.next_batch())
        .await?
        .ok_or("watcher closed")?;
    assert!(
        rename_batch
            .iter()
            .any(|change| change.path == canonical_file || change.path == canonical_renamed)
    );

    tokio::fs::remove_file(&renamed).await?;
    let remove_batch = tokio::time::timeout(Duration::from_secs(3), watcher.next_batch())
        .await?
        .ok_or("watcher closed")?;
    assert!(
        remove_batch
            .iter()
            .any(|change| change.path == canonical_renamed)
    );
    Ok(())
}

#[tokio::test]
async fn realtime_service_indexes_only_the_file_that_changed()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Existing.Movie.2023.mkv"), b"existing").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let watch_task = LibraryWatchService::new(database.clone()).spawn();
    tokio::time::sleep(Duration::from_secs(3)).await;
    tokio::fs::write(root.join("New.Movie.2024.mkv"), b"new").await?;

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_items")
                .fetch_one(database.pool())
                .await?;
            if count == 1 {
                break Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await??;
    let title: String = sqlx::query_scalar("SELECT title FROM media_items")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(title, "New Movie");
    watch_task.abort();
    let _ = watch_task.await;
    Ok(())
}
