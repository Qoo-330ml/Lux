#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

use luxd::{
    application::{
        chapter_detector::{ChapterDetectionOptions, ChapterDetectionService},
        libraries::{LibraryService, LibrarySettingsPatch},
        plugins::{CHAPTER_DETECTOR_PLUGIN_ID, PluginService},
        scanner::LibraryScanner,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use serde_json::json;

const STRM_ONLY_CHAPTER_SOURCE_ID: &str = "org.lux.example-strm-chapter-source";

#[tokio::test]
async fn detector_job_recovers_running_items_and_preserves_other_marker_sources()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join(format!("plugins/{CHAPTER_DETECTOR_PLUGIN_ID}"));
    tokio::fs::create_dir_all(plugin_dir.join("binaries")).await?;

    let plugin = plugin_dir.join("binaries/plugin");
    fs::write(
        &plugin,
        r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *chapters.detect*)
      keys=$(printf '%s' "$line" | grep -o '"key":"[^"]*"' | sed 's/"key":"//;s/"$//')
      first=$(printf '%s\n' "$keys" | sed -n '1p')
      second=$(printf '%s\n' "$keys" | sed -n '2p')
      printf '{"id":"%s","result":{"markers":[{"key":"%s","markerType":"INTRO_START","startPositionTicks":100000000,"name":"Intro","confidence":0.96},{"key":"%s","markerType":"INTRO_START","startPositionTicks":100000000,"name":"Intro","confidence":0.96}]}}
' "$id" "$first" "$second"
      ;;
    *)
      printf '{"id":"%s","result":{"available":true,"configured":true}}
' "$id"
      ;;
  esac
done
"#,
    )?;
    fs::set_permissions(&plugin, fs::Permissions::from_mode(0o700))?;
    tokio::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": CHAPTER_DETECTOR_PLUGIN_ID,
            "name": "Intro/outro detector",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "chapter_detector",
            "category": "MEDIA",
            "supportedMediaSourceKinds": ["LOCAL_FILE"],
            "supportedItemTypes": ["Episode"],
            "capabilities": ["chapters.detect"],
            "permissions": {"network": [], "filesystem": []},
            "files": []
        }))?,
    )
    .await?;

    let fake_ffmpeg = temp_dir.path().join("ffmpeg");
    fs::write(
        &fake_ffmpeg,
        r#"#!/bin/sh
case "$*" in
  *"-map 0:a:0? -vn -sn -dn -ar 11025 -ac 1 -f chromaprint -fp_format raw -"*) ;;
  *) exit 2 ;;
esac
case "$*" in
  *"-ss 0.000"*) printf '\001\000\000\000\002\000\000\000\003\000\000\000\004\000\000\000\005\000\000\000\006\000\000\000\007\000\000\000\010\000\000\000' ;;
  *) printf '\101\000\000\000\102\000\000\000\103\000\000\000\104\000\000\000\105\000\000\000\106\000\000\000\107\000\000\000\110\000\000\000' ;;
esac
"#,
    )?;
    fs::set_permissions(&fake_ffmpeg, fs::Permissions::from_mode(0o700))?;

    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let media_root = temp_dir.path().join("Shows");
    let season_root = media_root.join("Example Show").join("Season 01");
    tokio::fs::create_dir_all(&season_root).await?;
    for episode in 1..=3 {
        tokio::fs::write(
            season_root.join(format!("Example.Show.S01E0{episode}.mkv")),
            format!("episode-{episode}"),
        )
        .await?;
    }
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;
    sqlx::query("UPDATE media_sources SET duration_ticks = 1_800_000_000, probe_status = 'READY'")
        .execute(database.pool())
        .await?;

    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources ORDER BY id LIMIT 1")
        .fetch_one(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("manual-marker")
    .bind(&source_id)
    .bind(20_000_000_i64)
    .bind("INTRO_START")
    .bind(0_i64)
    .bind("manual")
    .bind(1.0_f64)
    .execute(database.pool())
    .await?;

    let plugins = PluginService::new(database.clone(), config_dir);
    plugins.install(CHAPTER_DETECTOR_PLUGIN_ID).await?;
    libraries
        .update_settings(
            library.id,
            LibrarySettingsPatch {
                chapter_source_id: Some(Some(CHAPTER_DETECTOR_PLUGIN_ID.to_owned())),
                ..Default::default()
            },
        )
        .await?;
    let service = ChapterDetectionService::new(database.clone(), plugins.clone())
        .with_ffmpeg(fake_ffmpeg, Duration::from_secs(5));
    plugins
        .set_enabled(CHAPTER_DETECTOR_PLUGIN_ID, false)
        .await?;
    assert!(matches!(
        service
            .create_library_job(
                library.id,
                CHAPTER_DETECTOR_PLUGIN_ID,
                ChapterDetectionOptions::default(),
            )
            .await,
        Err(luxd::application::chapter_detector::ChapterDetectionError::PluginUnavailable(_))
    ));
    plugins
        .set_enabled(CHAPTER_DETECTOR_PLUGIN_ID, true)
        .await?;
    let job = service
        .create_library_job(
            library.id,
            CHAPTER_DETECTOR_PLUGIN_ID,
            ChapterDetectionOptions::default(),
        )
        .await?;
    sqlx::query(
        "UPDATE chapter_detection_jobs SET status = 'RUNNING', started_at = unixepoch() WHERE id = ?",
    )
    .bind(&job.id)
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE chapter_detection_job_items SET status = 'RUNNING' WHERE job_id = ?")
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    service.run(&job.id).await?;
    let completed = service.get(&job.id).await?;
    assert_eq!(completed.status, "COMPLETED");
    assert!(matches!(
        service.cancel(&job.id).await,
        Err(luxd::application::chapter_detector::ChapterDetectionError::NotCancellable)
    ));

    let detector_markers: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_chapters WHERE provider_id = ?")
            .bind(CHAPTER_DETECTOR_PLUGIN_ID)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(detector_markers, 2);
    let manual_markers: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_chapters WHERE provider_id = 'manual'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(manual_markers, 1);
    let skipped_job = service
        .create_library_job(
            library.id,
            CHAPTER_DETECTOR_PLUGIN_ID,
            ChapterDetectionOptions::default(),
        )
        .await?;
    assert_eq!(skipped_job.total_count, 0);
    service.run(&skipped_job.id).await?;
    let forced_job = service
        .create_library_job(
            library.id,
            CHAPTER_DETECTOR_PLUGIN_ID,
            ChapterDetectionOptions {
                force_refresh: true,
                ..ChapterDetectionOptions::default()
            },
        )
        .await?;
    assert_eq!(forced_job.total_count, 3);
    Ok(())
}

#[tokio::test]
async fn chapter_source_honors_declared_media_source_kinds()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join(format!("plugins/{STRM_ONLY_CHAPTER_SOURCE_ID}"));
    tokio::fs::create_dir_all(plugin_dir.join("binaries")).await?;
    let plugin = plugin_dir.join("binaries/plugin");
    fs::write(
        &plugin,
        r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *chapters.lookup*)
      keys=$(printf '%s' "$line" | grep -o '"key":"[^"]*"' | sed 's/"key":"//;s/"$//')
      first=$(printf '%s\n' "$keys" | sed -n '1p')
      second=$(printf '%s\n' "$keys" | sed -n '2p')
      printf '{"id":"%s","result":{"markers":[{"key":"%s","markerType":"INTRO_START","startPositionTicks":100000000,"confidence":1.0},{"key":"%s","markerType":"INTRO_START","startPositionTicks":100000000,"confidence":1.0}]}}\n' "$id" "$first" "$second"
      ;;
    *)
      printf '{"id":"%s","result":{"available":true,"configured":true}}\n' "$id"
      ;;
  esac
done
"#,
    )?;
    fs::set_permissions(&plugin, fs::Permissions::from_mode(0o700))?;
    tokio::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": STRM_ONLY_CHAPTER_SOURCE_ID,
            "name": "TheIntroDB chapter source",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "chapter_detector",
            "category": "MEDIA",
            "supportedMediaSourceKinds": ["STRM_URL"],
            "supportedItemTypes": ["Episode"],
            "capabilities": ["chapters.lookup"],
            "configFields": [
                {"key": "concurrency", "label": "Concurrency", "type": "number", "defaultValue": 1, "minimum": 1, "maximum": 16},
                {"key": "introWindowSeconds", "label": "Intro", "type": "number", "defaultValue": 180, "minimum": 15, "maximum": 300},
                {"key": "creditsWindowSeconds", "label": "Credits", "type": "number", "defaultValue": 180, "minimum": 15, "maximum": 600},
                {"key": "matchThreshold", "label": "Threshold", "type": "number", "defaultValue": 80, "minimum": 1, "maximum": 100},
                {"key": "schedule", "label": "Schedule", "type": "text", "defaultValue": "0 5 * * *"}
            ],
            "permissions": {"network": ["api.theintrodb.org"], "filesystem": []},
            "files": []
        }))?,
    )
    .await?;
    let config = Config {
        http_addr: "127.0.0.1:8098".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Online shows", LibraryKind::Series, false)
        .await?;
    let media_root = temp_dir.path().join("Online shows");
    let season_root = media_root.join("Example Show").join("Season 01");
    tokio::fs::create_dir_all(&season_root).await?;
    for episode in 1..=2 {
        tokio::fs::write(
            season_root.join(format!("Example.Show.S01E0{episode}.strm")),
            format!("https://media.example/episode-{episode}.mkv"),
        )
        .await?;
    }
    tokio::fs::write(season_root.join("Example.Show.S01E03.mkv"), b"local-media").await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;
    sqlx::query("UPDATE media_sources SET duration_ticks = 1800000000, probe_status = 'READY'")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "UPDATE media_items SET provider_ids_json = '{\"Tmdb\":\"123\"}' WHERE item_type = 'SERIES'",
    )
    .execute(database.pool())
    .await?;
    let plugins = PluginService::new(database.clone(), config_dir);
    plugins.install(STRM_ONLY_CHAPTER_SOURCE_ID).await?;
    libraries
        .update_settings(
            library.id,
            LibrarySettingsPatch {
                chapter_source_id: Some(Some(STRM_ONLY_CHAPTER_SOURCE_ID.to_owned())),
                ..Default::default()
            },
        )
        .await?;
    plugins.sync_chapter_detection_scheduled_tasks().await?;
    let scheduled: (String, i64) = sqlx::query_as(
        "SELECT cron_or_interval, is_enabled FROM scheduled_task_configs
         WHERE owner_type = 'LIBRARY' AND owner_id = ? AND task_type = 'CHAPTER_DETECTION'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(scheduled, ("0 5 * * *".to_owned(), 1));
    let service = ChapterDetectionService::new(database.clone(), plugins);
    let job = service
        .create_library_job(
            library.id,
            STRM_ONLY_CHAPTER_SOURCE_ID,
            ChapterDetectionOptions::default(),
        )
        .await?;
    assert_eq!(job.total_count, 2);
    service.run(&job.id).await?;
    let completed = service.get(&job.id).await?;
    assert_eq!(completed.status, "COMPLETED");
    let marker_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_chapters WHERE provider_id = ?")
            .bind(STRM_ONLY_CHAPTER_SOURCE_ID)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(marker_count, 2);
    let skipped_job = service
        .create_library_job(
            library.id,
            STRM_ONLY_CHAPTER_SOURCE_ID,
            ChapterDetectionOptions::default(),
        )
        .await?;
    assert_eq!(skipped_job.total_count, 0);
    service.run(&skipped_job.id).await?;
    let forced_job = service
        .create_library_job(
            library.id,
            STRM_ONLY_CHAPTER_SOURCE_ID,
            ChapterDetectionOptions {
                force_refresh: true,
                ..ChapterDetectionOptions::default()
            },
        )
        .await?;
    assert_eq!(forced_job.total_count, 2);
    Ok(())
}
