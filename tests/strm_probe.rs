use std::{fs, path::Path};

use luxd::{
    application::{
        plugins::{MEDIA_INFO_PLUGIN_ID, PluginService},
        probe::{FfprobeRunner, MediaProbeService},
        scanner::{IncrementalScanChange, LibraryScanner, ScanJobService},
        strm_probe::{StrmProbeOptions, StrmProbeService},
        watch::ChangeKind,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use serde_json::json;
use uuid::Uuid;

#[cfg(unix)]
#[tokio::test]
async fn strm_probe_plugin_persists_media_info_and_compatible_sidecar()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join("plugins/org.lux.strm-media-info");
    let binary_dir = plugin_dir.join("binaries");
    tokio::fs::create_dir_all(&binary_dir).await?;

    let fake_ffprobe = temp_dir.path().join("ffprobe");
    fs::write(
        &fake_ffprobe,
        r#"#!/bin/sh
printf '%s' '{"format":{"format_name":"matroska","size":"1234","duration":"12.5","bit_rate":"500000"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080},{"index":1,"codec_type":"audio","codec_name":"aac","tags":{"language":"eng"}}]}'
"#,
    )?;
    make_executable(&fake_ffprobe)?;

    let fake_ffmpeg = temp_dir.path().join("ffmpeg");
    fs::write(
        &fake_ffmpeg,
        "#!/bin/sh\nprintf '\\377\\330\\377fake-thumb\\377\\331'\n",
    )?;
    make_executable(&fake_ffmpeg)?;

    let wrapper = binary_dir.join("plugin");
    write_fake_media_probe_plugin(&wrapper, "h264")?;
    fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": MEDIA_INFO_PLUGIN_ID,
            "name": "strm媒体信息提取",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "media_probe",
            "category": "MEDIA",
            "capabilities": ["media.probe"],
            "permissions": {"network": ["media-source"], "filesystem": []},
            "files": []
        }))?,
    )?;

    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Plugin Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    let strm_path = movie_dir.join("Plugin.Movie.2024.strm");
    let original_strm = b"https://media.example.invalid/video.mkv";
    tokio::fs::write(&strm_path, original_strm).await?;

    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let libraries = luxd::application::libraries::LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let generic_probe = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(&fake_ffprobe, std::time::Duration::from_secs(5)),
    );
    let report = generic_probe.probe_movie_library(library.id).await?;
    assert_eq!(report.ready, 0);
    assert_eq!(report.skipped, 1);
    let plugins = PluginService::new(database.clone(), config_dir);
    plugins.install(MEDIA_INFO_PLUGIN_ID).await?;
    let service = StrmProbeService::new(database.clone(), plugins);
    let jobs = service
        .create_jobs(
            &[library.id],
            StrmProbeOptions {
                concurrency: 2,
                include_ready: false,
                write_sidecars: true,
                media_info_enabled: true,
                thumbnail_enabled: false,
                thumbnail_position_percent: 30,
            },
        )
        .await?;
    assert_eq!(jobs.len(), 1);
    assert!(jobs[0].media_info_enabled);
    assert!(!jobs[0].thumbnail_enabled);
    assert_eq!(jobs[0].thumbnail_position_percent, 30);
    let queued_status: String = sqlx::query_scalar("SELECT probe_status FROM media_sources")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(queued_status, "PENDING");
    service.run(&jobs[0].id).await?;

    let job = service.get(&jobs[0].id).await?;
    assert_eq!(job.status, "COMPLETED");
    let source: (String, Option<String>, Option<i64>, Option<i64>, i64) = sqlx::query_as(
        "SELECT probe_status, container, duration_ticks, bitrate,
                (SELECT COUNT(*) FROM media_streams WHERE media_source_id = media_sources.id)
         FROM media_sources",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        source,
        (
            "READY".to_owned(),
            Some("matroska".to_owned()),
            Some(125_000_000),
            Some(500_000),
            2
        )
    );

    let skip_jobs = service
        .create_jobs(
            &[library.id],
            StrmProbeOptions {
                concurrency: 2,
                include_ready: false,
                write_sidecars: false,
                media_info_enabled: true,
                thumbnail_enabled: false,
                thumbnail_position_percent: 30,
            },
        )
        .await?;
    let queued_status: String = sqlx::query_scalar("SELECT probe_status FROM media_sources")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(queued_status, "PENDING");
    service.run(&skip_jobs[0].id).await?;
    assert_eq!(service.get(&skip_jobs[0].id).await?.status, "COMPLETED");
    let skipped_status: String = sqlx::query_scalar("SELECT probe_status FROM media_sources")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(skipped_status, "READY");

    let sidecar = tokio::fs::read(movie_dir.join("Plugin.Movie.2024-mediainfo.json")).await?;
    let sidecar: serde_json::Value = serde_json::from_slice(&sidecar)?;
    assert_eq!(sidecar[0]["MediaSourceInfo"]["Container"], "matroska");
    assert_eq!(sidecar[0]["MediaSourceInfo"]["Protocol"], "Http");
    assert_eq!(sidecar[0]["MediaSourceInfo"]["Type"], "Default");
    assert_eq!(sidecar[0]["MediaSourceInfo"]["IsRemote"], true);
    assert_eq!(sidecar[0]["MediaSourceInfo"]["SupportsDirectPlay"], true);
    assert_eq!(sidecar[0]["MediaSourceInfo"]["SupportsDirectStream"], true);
    assert_eq!(sidecar[0]["MediaSourceInfo"]["SupportsTranscoding"], true);
    assert!(sidecar[0]["MediaSourceInfo"].get("Id").is_none());
    assert!(sidecar[0]["MediaSourceInfo"].get("ItemId").is_none());
    assert!(sidecar[0]["MediaSourceInfo"].get("Path").is_none());
    assert_eq!(
        sidecar[0]["MediaSourceInfo"]["MediaStreams"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(sidecar[0]["Chapters"].as_array().map(Vec::len), Some(0));
    assert_eq!(tokio::fs::read(&strm_path).await?, original_strm);

    fs::write(
        &fake_ffmpeg,
        "#!/bin/sh\nprintf '\\377\\330\\377fake-thumb\\377\\331'\n",
    )?;
    let thumbnail_jobs = service
        .create_jobs(
            &[library.id],
            StrmProbeOptions {
                concurrency: 2,
                include_ready: false,
                write_sidecars: false,
                media_info_enabled: false,
                thumbnail_enabled: true,
                thumbnail_position_percent: 50,
            },
        )
        .await?;
    assert!(!thumbnail_jobs[0].media_info_enabled);
    assert!(thumbnail_jobs[0].thumbnail_enabled);
    assert_eq!(thumbnail_jobs[0].thumbnail_position_percent, 50);
    service.run(&thumbnail_jobs[0].id).await?;
    assert_eq!(
        service.get(&thumbnail_jobs[0].id).await?.status,
        "COMPLETED"
    );
    let thumbnail_path = movie_dir.join("Plugin.Movie.2024-thumbnail.jpg");
    let thumbnail = fs::read(&thumbnail_path)?;
    assert_eq!(thumbnail, b"\xff\xd8\xfffake-thumb\xff\xd9");
    let images: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT image_type, local_path, source, file_size FROM item_images
         WHERE item_id = (SELECT item_id FROM media_sources)
         ORDER BY image_type",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].0, "POSTER");
    assert_eq!(images[1].0, "THUMB");
    assert_eq!(
        fs::canonicalize(&images[0].1)?,
        fs::canonicalize(&thumbnail_path)?
    );
    assert_eq!(images[0].1, images[1].1);
    assert_eq!(images[0].2, "STRM_FFMPEG");
    assert_eq!(images[1].2, "STRM_FFMPEG");
    assert_eq!(images[0].3, i64::try_from(thumbnail.len())?);
    assert_eq!(images[1].3, i64::try_from(thumbnail.len())?);

    let item_id: String = sqlx::query_scalar("SELECT item_id FROM media_sources")
        .fetch_one(database.pool())
        .await?;
    sqlx::query("DELETE FROM item_images")
        .execute(database.pool())
        .await?;
    let poster_path = movie_dir.join("Plugin.Movie.2024-poster.jpg");
    let scraper_thumbnail = b"\xFF\xD8scraper\xFF\xD9";
    tokio::fs::write(&poster_path, scraper_thumbnail).await?;
    tokio::fs::write(&thumbnail_path, scraper_thumbnail).await?;
    for (image_type, path) in [("POSTER", &poster_path), ("THUMB", &thumbnail_path)] {
        sqlx::query(
            "INSERT INTO item_images (
                id, item_id, image_type, image_index, local_path, file_size, content_tag, source
             ) VALUES (?, ?, ?, 0, ?, ?, 'scraper', 'TMDB')",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&item_id)
        .bind(image_type)
        .bind(path.to_string_lossy().as_ref())
        .bind(i64::try_from(scraper_thumbnail.len())?)
        .execute(database.pool())
        .await?;
    }
    sqlx::query("UPDATE libraries SET media_strategy_json = ? WHERE id = ?")
        .bind(r#"{"images":{"thumbnailScrapingMode":"SCREENSHOT_FIRST"}}"#)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
    let screenshot_first_jobs = service
        .create_jobs(
            &[library.id],
            StrmProbeOptions {
                concurrency: 2,
                include_ready: false,
                write_sidecars: false,
                media_info_enabled: false,
                thumbnail_enabled: true,
                thumbnail_position_percent: 50,
            },
        )
        .await?;
    service.run(&screenshot_first_jobs[0].id).await?;
    assert_eq!(
        tokio::fs::read(&thumbnail_path).await?,
        b"\xFF\xD8\xFFfake-thumb\xFF\xD9"
    );
    let screenshot_first_source: String = sqlx::query_scalar(
        "SELECT source FROM item_images WHERE item_id = ? AND image_type = 'POSTER'",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(screenshot_first_source, "STRM_FFMPEG");

    fs::write(&fake_ffmpeg, "#!/bin/sh\nexit 9\n")?;
    let skip_thumbnail_jobs = service
        .create_jobs(
            &[library.id],
            StrmProbeOptions {
                concurrency: 2,
                include_ready: false,
                write_sidecars: false,
                media_info_enabled: false,
                thumbnail_enabled: true,
                thumbnail_position_percent: 50,
            },
        )
        .await?;
    service.run(&skip_thumbnail_jobs[0].id).await?;
    assert_eq!(
        fs::read(&thumbnail_path)?,
        b"\xff\xd8\xfffake-thumb\xff\xd9"
    );

    tokio::fs::remove_file(&thumbnail_path).await?;
    sqlx::query("DELETE FROM item_images")
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE libraries SET media_strategy_json = ? WHERE id = ?")
        .bind(r#"{"images":{"thumbnailScrapingMode":"NONE"}}"#)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_items SET poster_fallback_required = 1")
        .execute(database.pool())
        .await?;
    let disabled_thumbnail_jobs = service
        .create_jobs(
            &[library.id],
            StrmProbeOptions {
                concurrency: 2,
                include_ready: false,
                write_sidecars: false,
                media_info_enabled: false,
                thumbnail_enabled: true,
                thumbnail_position_percent: 50,
            },
        )
        .await?;
    service.run(&disabled_thumbnail_jobs[0].id).await?;
    assert_eq!(
        service.get(&disabled_thumbnail_jobs[0].id).await?.status,
        "COMPLETED"
    );
    let disabled_image_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item_images")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(disabled_image_count, 0);

    fs::write(&fake_ffprobe, "#!/bin/sh\nexit 9\n")?;
    let skip_jobs = service
        .create_jobs(
            &[library.id],
            StrmProbeOptions {
                concurrency: 2,
                include_ready: false,
                write_sidecars: false,
                media_info_enabled: true,
                thumbnail_enabled: false,
                thumbnail_position_percent: 30,
            },
        )
        .await?;
    service.run(&skip_jobs[0].id).await?;
    let skipped_codec: String = sqlx::query_scalar(
        "SELECT codec FROM media_streams WHERE media_source_id = (SELECT id FROM media_sources)\n         AND stream_type = 'VIDEO'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(skipped_codec, "h264");

    fs::write(
        &fake_ffprobe,
        r#"#!/bin/sh
printf '%s' '{"format":{"format_name":"matroska"},"streams":[{"index":0,"codec_type":"video","codec_name":"vp9"}]}'
"#,
    )?;
    let overwrite_jobs = service
        .create_jobs(
            &[library.id],
            StrmProbeOptions {
                concurrency: 2,
                include_ready: true,
                write_sidecars: false,
                media_info_enabled: true,
                thumbnail_enabled: false,
                thumbnail_position_percent: 30,
            },
        )
        .await?;
    service.run(&overwrite_jobs[0].id).await?;
    let overwritten_codec: String = sqlx::query_scalar(
        "SELECT codec FROM media_streams WHERE media_source_id = (SELECT id FROM media_sources)\n         AND stream_type = 'VIDEO'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(overwritten_codec, "h264");

    let sidecar_path = movie_dir.join("Plugin.Movie.2024-mediainfo.json");
    let malformed_sidecar = b"{not-json";
    tokio::fs::write(&sidecar_path, malformed_sidecar).await?;
    let malformed_jobs = service
        .create_jobs(
            &[library.id],
            StrmProbeOptions {
                concurrency: 2,
                include_ready: true,
                write_sidecars: true,
                media_info_enabled: true,
                thumbnail_enabled: false,
                thumbnail_position_percent: 30,
            },
        )
        .await?;
    service.run(&malformed_jobs[0].id).await?;
    assert_eq!(service.get(&malformed_jobs[0].id).await?.status, "FAILED");
    assert_eq!(tokio::fs::read(&sidecar_path).await?, malformed_sidecar);
    assert_eq!(tokio::fs::read(&strm_path).await?, original_strm);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn incremental_scan_queues_and_runs_targeted_strm_probe()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join("plugins/org.lux.strm-media-info");
    let binary_dir = plugin_dir.join("binaries");
    tokio::fs::create_dir_all(&binary_dir).await?;

    let fake_ffprobe = temp_dir.path().join("ffprobe");
    fs::write(
        &fake_ffprobe,
        r#"#!/bin/sh
printf '%s' '{"format":{"format_name":"matroska","duration":"12.5","bit_rate":"500000"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264"}]}'
"#,
    )?;
    make_executable(&fake_ffprobe)?;
    let wrapper = binary_dir.join("plugin");
    write_fake_media_probe_plugin(&wrapper, "h264")?;
    fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": MEDIA_INFO_PLUGIN_ID,
            "name": "strm媒体信息提取",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "media_probe",
            "category": "MEDIA",
            "capabilities": ["media.probe"],
            "configFields": [
                {"key": "libraryIds", "label": "媒体库", "type": "select", "multiple": true, "required": true, "optionsSource": "media-libraries"},
                {"key": "concurrency", "label": "并发数", "type": "number", "required": true, "defaultValue": 2, "minimum": 1, "maximum": 64},
                {"key": "mediaInfoEnabled", "label": "提取媒体信息", "type": "toggle", "defaultValue": true},
                {"key": "thumbnailEnabled", "label": "补全 STRM 缩略图", "type": "toggle", "defaultValue": false},
                {"key": "thumbnailPositionPercent", "label": "缩略图位置", "type": "number", "required": true, "defaultValue": 30, "minimum": 1, "maximum": 99},
                {"key": "existingInfoPolicy", "label": "已有媒体信息处理方式", "type": "select", "defaultValue": "SKIP", "options": [{"value": "SKIP", "label": "跳过已有媒体信息"}, {"value": "OVERWRITE", "label": "覆盖已有媒体信息"}]},
                {"key": "writeSidecars", "label": "写入旁车", "type": "toggle", "defaultValue": false},
                {"key": "schedule", "label": "执行计划", "type": "text", "required": true, "defaultValue": "0 3 * * *"}
            ],
            "permissions": {"network": ["media-source"], "filesystem": []},
            "files": []
        }))?,
    )?;

    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    tokio::fs::write(
        media_root.join("Existing.Movie.2023.strm"),
        "https://media.example.invalid/existing.mkv",
    )
    .await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let libraries = luxd::application::libraries::LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    sqlx::query("UPDATE libraries SET realtime_metadata_auto_match_enabled = 1 WHERE id = ?")
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
    let root = libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    sqlx::query(
        "UPDATE media_sources
         SET container = 'matroska', duration_ticks = 125000000, bitrate = 500000
         WHERE source_kind = 'STRM_URL'",
    )
    .execute(database.pool())
    .await?;

    let plugins = PluginService::new(database.clone(), config_dir);
    plugins.install(MEDIA_INFO_PLUGIN_ID).await?;
    plugins
        .update_dynamic_config(
            MEDIA_INFO_PLUGIN_ID,
            serde_json::Map::from_iter([
                ("libraryIds".to_owned(), json!([library.id.to_string()])),
                ("concurrency".to_owned(), json!(2)),
                ("mediaInfoEnabled".to_owned(), json!(true)),
                ("thumbnailEnabled".to_owned(), json!(false)),
                ("thumbnailPositionPercent".to_owned(), json!(30)),
                ("existingInfoPolicy".to_owned(), json!("SKIP")),
                ("writeSidecars".to_owned(), json!(false)),
                ("schedule".to_owned(), json!("0 3 * * *")),
            ]),
        )
        .await?;

    let new_path = "New.Movie.2024.strm";
    tokio::fs::write(
        media_root.join(new_path),
        "https://media.example.invalid/new.mkv",
    )
    .await?;
    let strm_probe = StrmProbeService::new(database.clone(), plugins.clone());
    let scan_jobs = ScanJobService::new(database.clone()).with_strm_probe(strm_probe.clone());
    let scan_job = scan_jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root.id.to_string(),
                relative_path: new_path.to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    scan_jobs.run_to_completion(&scan_job.id, 100, None).await?;

    let mut probe_job_id = None;
    for _ in 0..250 {
        let job = sqlx::query_as::<_, (String, i64, Option<String>, String)>(
            "SELECT id, total_count, target_scan_job_id, status
             FROM strm_probe_jobs ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .fetch_optional(database.pool())
        .await?;
        if let Some((id, total_count, target_scan_job_id, _status)) = job {
            assert_eq!(total_count, 1);
            assert_eq!(target_scan_job_id.as_deref(), Some(scan_job.id.as_str()));
            probe_job_id = Some(id);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let probe_job_id = probe_job_id.ok_or("targeted STRM probe job was not created")?;
    let mut probe_job = None;
    for _ in 0..250 {
        let job = strm_probe.get(&probe_job_id).await?;
        if matches!(job.status.as_str(), "COMPLETED" | "FAILED") {
            probe_job = Some(job);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let probe_job = probe_job.ok_or("targeted STRM probe job did not finish")?;
    assert_eq!(probe_job.status, "COMPLETED");
    let new_source: (String, String) = sqlx::query_as(
        "SELECT ms.probe_status, mt.codec
         FROM media_sources ms
         JOIN media_streams mt ON mt.media_source_id = ms.id
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE fe.relative_path = ? AND mt.stream_type = 'VIDEO'",
    )
    .bind(new_path)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(new_source, ("READY".to_owned(), "h264".to_owned()));
    let scheduled: (String, i64) = sqlx::query_as(
        "SELECT cron_or_interval, is_enabled FROM scheduled_task_configs
         WHERE owner_type = 'GLOBAL' AND owner_id = 'global' AND task_type = 'STRM_MEDIA_INFO'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(scheduled, ("0 3 * * *".to_owned(), 1));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn source_scoped_job_only_probes_its_target_source() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join("plugins/org.lux.strm-media-info");
    let binary_dir = plugin_dir.join("binaries");
    tokio::fs::create_dir_all(&binary_dir).await?;
    write_fake_media_probe_plugin(&binary_dir.join("plugin"), "h264")?;
    fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": MEDIA_INFO_PLUGIN_ID,
            "name": "strm媒体信息提取",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "media_probe",
            "category": "MEDIA",
            "capabilities": ["media.probe"],
            "permissions": {"network": ["media-source"], "filesystem": []},
            "files": []
        }))?,
    )?;

    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    tokio::fs::write(
        media_root.join("Target.Movie.2024.strm"),
        "https://media.example.invalid/target.mkv",
    )
    .await?;
    tokio::fs::write(
        media_root.join("Other.Movie.2024.strm"),
        "https://media.example.invalid/other.mkv",
    )
    .await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let libraries = luxd::application::libraries::LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let sources: Vec<(String, String)> = sqlx::query_as(
        "SELECT ms.id, fe.relative_path
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE ms.source_kind = 'STRM_URL'",
    )
    .fetch_all(database.pool())
    .await?;
    let find_source = |path: &str| {
        sources
            .iter()
            .find(|(_, relative_path)| relative_path == path)
            .map(|(source_id, _)| source_id.clone())
    };
    let target_source_id =
        find_source("Target.Movie.2024.strm").ok_or("target source was not scanned")?;
    let other_source_id =
        find_source("Other.Movie.2024.strm").ok_or("other source was not scanned")?;

    let plugins = PluginService::new(database.clone(), config_dir);
    plugins.install(MEDIA_INFO_PLUGIN_ID).await?;
    let service = StrmProbeService::new(database.clone(), plugins);
    let jobs = service
        .create_jobs(
            &[library.id],
            StrmProbeOptions {
                concurrency: 1,
                include_ready: false,
                write_sidecars: false,
                media_info_enabled: true,
                thumbnail_enabled: false,
                thumbnail_position_percent: 30,
            },
        )
        .await?;
    sqlx::query("UPDATE strm_probe_jobs SET target_source_id = ?, total_count = 1 WHERE id = ?")
        .bind(&target_source_id)
        .bind(&jobs[0].id)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_sources SET probe_status = 'NOT_CONFIGURED' WHERE id = ?")
        .bind(&other_source_id)
        .execute(database.pool())
        .await?;

    service.run(&jobs[0].id).await?;

    let results: Vec<(String, i64)> = sqlx::query_as(
        "SELECT ms.id,
                (SELECT COUNT(*) FROM media_streams mt WHERE mt.media_source_id = ms.id)
         FROM media_sources ms WHERE ms.id IN (?, ?)",
    )
    .bind(&target_source_id)
    .bind(&other_source_id)
    .fetch_all(database.pool())
    .await?;
    let streams_by_source = results
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(streams_by_source.get(&target_source_id), Some(&2));
    assert_eq!(streams_by_source.get(&other_source_id), Some(&0));

    sqlx::query(
        "UPDATE strm_probe_jobs SET status = 'FAILED', finished_at = unixepoch() WHERE id = ?",
    )
    .bind(&jobs[0].id)
    .execute(database.pool())
    .await?;
    let retried = service.retry(&jobs[0].id).await?;
    let retried_source_id: Option<String> =
        sqlx::query_scalar("SELECT target_source_id FROM strm_probe_jobs WHERE id = ?")
            .bind(&retried.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        retried_source_id.as_deref(),
        Some(target_source_id.as_str())
    );
    assert_eq!(retried.total_count, 1);

    database.close().await;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn playback_probe_fills_only_missing_info_even_when_policy_overwrites()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join("plugins/org.lux.strm-media-info");
    let binary_dir = plugin_dir.join("binaries");
    tokio::fs::create_dir_all(&binary_dir).await?;
    write_fake_media_probe_plugin(&binary_dir.join("plugin"), "vp9")?;
    fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": MEDIA_INFO_PLUGIN_ID,
            "name": "strm媒体信息提取",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "media_probe",
            "category": "MEDIA",
            "capabilities": ["media.probe"],
            "configFields": [
                {"key": "libraryIds", "label": "媒体库", "type": "select", "multiple": true, "required": true, "optionsSource": "media-libraries"},
                {"key": "concurrency", "label": "并发数", "type": "number", "required": true, "defaultValue": 2, "minimum": 1, "maximum": 64},
                {"key": "mediaInfoEnabled", "label": "提取媒体信息", "type": "toggle", "defaultValue": true},
                {"key": "thumbnailEnabled", "label": "补全 STRM 缩略图", "type": "toggle", "defaultValue": false},
                {"key": "thumbnailPositionPercent", "label": "缩略图位置", "type": "number", "required": true, "defaultValue": 30, "minimum": 1, "maximum": 99},
                {"key": "existingInfoPolicy", "label": "已有媒体信息处理方式", "type": "select", "defaultValue": "SKIP", "options": [{"value": "SKIP", "label": "跳过已有媒体信息"}, {"value": "OVERWRITE", "label": "覆盖已有媒体信息"}]},
                {"key": "writeSidecars", "label": "写入旁车", "type": "toggle", "defaultValue": false},
                {"key": "schedule", "label": "执行计划", "type": "text", "required": true, "defaultValue": "0 3 * * *"}
            ],
            "permissions": {"network": ["media-source"], "filesystem": []},
            "files": []
        }))?,
    )?;

    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    for name in ["Existing.Movie.2023.strm", "Missing.Movie.2024.strm"] {
        tokio::fs::write(
            media_root.join(name),
            format!("https://media.example.invalid/{name}.mkv"),
        )
        .await?;
    }
    let local_media_path = media_root.join("Local.Target.2022.mkv");
    tokio::fs::write(&local_media_path, b"test media").await?;
    let local_media_target = local_media_path
        .to_str()
        .ok_or("non-utf8 local STRM target")?;
    tokio::fs::write(
        media_root.join("Local.Target.2022.strm"),
        local_media_target,
    )
    .await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let libraries = luxd::application::libraries::LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let unselected_library = libraries
        .create_library("Unselected", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let sources: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT ms.id, mi.id, fe.relative_path
         FROM media_sources ms
         JOIN media_items mi ON mi.id = ms.item_id
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE ms.source_kind = 'STRM_URL'",
    )
    .fetch_all(database.pool())
    .await?;
    let (existing_source_id, existing_item_id) = sources
        .iter()
        .find(|(_, _, path)| path == "Existing.Movie.2023.strm")
        .map(|(source_id, item_id, _)| (source_id.clone(), item_id.clone()))
        .ok_or("existing STRM source was not scanned")?;
    let (missing_source_id, missing_item_id) = sources
        .iter()
        .find(|(_, _, path)| path == "Missing.Movie.2024.strm")
        .map(|(source_id, item_id, _)| (source_id.clone(), item_id.clone()))
        .ok_or("missing STRM source was not scanned")?;
    let (local_source_id, local_item_id) = sources
        .iter()
        .find(|(_, _, path)| path == "Local.Target.2022.strm")
        .map(|(source_id, item_id, _)| (source_id.clone(), item_id.clone()))
        .ok_or("local-path STRM source was not scanned")?;
    sqlx::query(
        "UPDATE media_sources
         SET container = 'matroska', duration_ticks = 120000000, bitrate = 500000
         WHERE id = ?",
    )
    .bind(&existing_source_id)
    .execute(database.pool())
    .await?;

    let plugins = PluginService::new(database.clone(), config_dir);
    plugins.install(MEDIA_INFO_PLUGIN_ID).await?;
    plugins
        .update_dynamic_config(
            MEDIA_INFO_PLUGIN_ID,
            serde_json::Map::from_iter([
                ("libraryIds".to_owned(), json!([library.id.to_string()])),
                ("concurrency".to_owned(), json!(2)),
                ("mediaInfoEnabled".to_owned(), json!(true)),
                ("thumbnailEnabled".to_owned(), json!(false)),
                ("thumbnailPositionPercent".to_owned(), json!(30)),
                ("existingInfoPolicy".to_owned(), json!("OVERWRITE")),
                ("writeSidecars".to_owned(), json!(false)),
                ("schedule".to_owned(), json!("0 3 * * *")),
            ]),
        )
        .await?;
    let service = StrmProbeService::new(database.clone(), plugins.clone());

    plugins
        .update_dynamic_config(
            MEDIA_INFO_PLUGIN_ID,
            serde_json::Map::from_iter([
                ("libraryIds".to_owned(), json!([library.id.to_string()])),
                ("concurrency".to_owned(), json!(2)),
                ("mediaInfoEnabled".to_owned(), json!(false)),
                ("thumbnailEnabled".to_owned(), json!(false)),
                ("thumbnailPositionPercent".to_owned(), json!(30)),
                ("existingInfoPolicy".to_owned(), json!("OVERWRITE")),
                ("writeSidecars".to_owned(), json!(false)),
                ("schedule".to_owned(), json!("0 3 * * *")),
            ]),
        )
        .await?;
    assert!(service.schedule_playback_probe(&missing_item_id, &missing_source_id));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM strm_probe_jobs")
            .fetch_one(database.pool())
            .await?,
        0,
        "disabled media-info extraction must not enqueue a playback probe"
    );

    plugins
        .update_dynamic_config(
            MEDIA_INFO_PLUGIN_ID,
            serde_json::Map::from_iter([
                (
                    "libraryIds".to_owned(),
                    json!([unselected_library.id.to_string()]),
                ),
                ("concurrency".to_owned(), json!(2)),
                ("mediaInfoEnabled".to_owned(), json!(true)),
                ("thumbnailEnabled".to_owned(), json!(false)),
                ("thumbnailPositionPercent".to_owned(), json!(30)),
                ("existingInfoPolicy".to_owned(), json!("OVERWRITE")),
                ("writeSidecars".to_owned(), json!(false)),
                ("schedule".to_owned(), json!("0 3 * * *")),
            ]),
        )
        .await?;
    assert!(service.schedule_playback_probe(&missing_item_id, &missing_source_id));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM strm_probe_jobs")
            .fetch_one(database.pool())
            .await?,
        0,
        "an unselected media library must not enqueue a playback probe"
    );

    plugins.set_enabled(MEDIA_INFO_PLUGIN_ID, false).await?;
    assert!(service.schedule_playback_probe(&missing_item_id, &missing_source_id));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM strm_probe_jobs")
            .fetch_one(database.pool())
            .await?,
        0,
        "a disabled plugin must not enqueue a playback probe"
    );
    plugins.set_enabled(MEDIA_INFO_PLUGIN_ID, true).await?;
    plugins
        .update_dynamic_config(
            MEDIA_INFO_PLUGIN_ID,
            serde_json::Map::from_iter([
                ("libraryIds".to_owned(), json!([library.id.to_string()])),
                ("concurrency".to_owned(), json!(2)),
                ("mediaInfoEnabled".to_owned(), json!(true)),
                ("thumbnailEnabled".to_owned(), json!(false)),
                ("thumbnailPositionPercent".to_owned(), json!(30)),
                ("existingInfoPolicy".to_owned(), json!("OVERWRITE")),
                ("writeSidecars".to_owned(), json!(false)),
                ("schedule".to_owned(), json!("0 3 * * *")),
            ]),
        )
        .await?;

    assert!(service.schedule_playback_probe(&existing_item_id, &existing_source_id));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM strm_probe_jobs")
            .fetch_one(database.pool())
            .await?,
        0
    );

    assert!(service.schedule_playback_probe(&local_item_id, &local_source_id));
    let mut local_probe_job = None;
    for _ in 0..250 {
        local_probe_job = sqlx::query_as::<_, (String, String)>(
            "SELECT id, status FROM strm_probe_jobs
             WHERE target_source_id = ? ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&local_source_id)
        .fetch_optional(database.pool())
        .await?;
        if local_probe_job
            .as_ref()
            .is_some_and(|(_, status)| matches!(status.as_str(), "COMPLETED" | "FAILED"))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let (local_probe_job_id, local_probe_status) =
        local_probe_job.ok_or("supported local-path STRM target did not enqueue a probe")?;
    assert_eq!(local_probe_status, "COMPLETED");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM media_streams WHERE media_source_id = ?",
        )
        .bind(&local_source_id)
        .fetch_one(database.pool())
        .await?,
        2,
        "the single-source probe should persist media info for its local-path target"
    );
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT target_source_id FROM strm_probe_jobs WHERE id = ?",
        )
        .bind(local_probe_job_id)
        .fetch_one(database.pool())
        .await?
        .as_deref(),
        Some(local_source_id.as_str())
    );

    let blocker = service
        .create_jobs(
            &[library.id],
            StrmProbeOptions {
                concurrency: 2,
                include_ready: true,
                write_sidecars: false,
                media_info_enabled: true,
                thumbnail_enabled: false,
                thumbnail_position_percent: 30,
            },
        )
        .await?;
    assert!(service.schedule_playback_probe(&missing_item_id, &missing_source_id));
    assert!(!service.schedule_playback_probe(&missing_item_id, &missing_source_id));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM strm_probe_jobs WHERE status IN ('PENDING', 'RUNNING')",
        )
        .fetch_one(database.pool())
        .await?,
        1,
        "playback-triggered work must wait behind the existing job"
    );
    sqlx::query(
        "UPDATE strm_probe_jobs SET status = 'COMPLETED', finished_at = unixepoch() WHERE id = ?",
    )
    .bind(&blocker[0].id)
    .execute(database.pool())
    .await?;
    let mut finished_job = None;
    for _ in 0..250 {
        let job: Option<(String, String, Option<String>, i64, i64)> = sqlx::query_as(
            "SELECT id, status, target_source_id, include_ready, thumbnail_enabled
             FROM strm_probe_jobs WHERE target_source_id = ?
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(&missing_source_id)
        .fetch_optional(database.pool())
        .await?;
        if let Some((id, status, target_source_id, include_ready, thumbnail_enabled)) = job
            && matches!(status.as_str(), "COMPLETED" | "FAILED")
        {
            finished_job = Some((
                id,
                status,
                target_source_id,
                include_ready,
                thumbnail_enabled,
            ));
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let (_, status, target_source_id, include_ready, thumbnail_enabled) =
        finished_job.ok_or("playback-triggered STRM job did not finish")?;
    assert_eq!(status, "COMPLETED");
    assert_eq!(
        target_source_id.as_deref(),
        Some(missing_source_id.as_str())
    );
    assert_eq!(include_ready, 0);
    assert_eq!(thumbnail_enabled, 0);

    let metadata: Vec<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT ms.id, ms.container,
                (SELECT COUNT(*) FROM media_streams mt WHERE mt.media_source_id = ms.id)
         FROM media_sources ms WHERE ms.id IN (?, ?)",
    )
    .bind(&existing_source_id)
    .bind(&missing_source_id)
    .fetch_all(database.pool())
    .await?;
    let metadata_by_source = metadata
        .into_iter()
        .map(|(source_id, container, stream_count)| (source_id, (container, stream_count)))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(
        metadata_by_source.get(&existing_source_id),
        Some(&(Some("matroska".to_owned()), 0))
    );
    assert_eq!(
        metadata_by_source.get(&missing_source_id),
        Some(&(Some("matroska".to_owned()), 2))
    );

    database.close().await;
    Ok(())
}

#[cfg(unix)]
fn write_fake_media_probe_plugin(
    path: &Path,
    codec: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let script = format!(
        r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
  case "$method" in
    plugin.hello) printf '{{"id":"%s","result":{{"id":"org.lux.strm-media-info","name":"strm媒体信息提取","apiVersion":1,"capabilities":["media.probe"],"supportedItemTypes":[]}}}}\n' "$id" ;;
    plugin.health) printf '{{"id":"%s","result":{{"available":true,"configured":true}}}}\n' "$id" ;;
    media.probe) printf '{{"id":"%s","result":{{"container":"matroska","sourceSize":1234,"durationTicks":125000000,"bitrate":500000,"streams":[{{"streamIndex":0,"streamType":"VIDEO","codec":"{}","isDefault":false,"isForced":false,"details":{{}}}},{{"streamIndex":1,"streamType":"AUDIO","codec":"aac","language":"eng","isDefault":false,"isForced":false,"details":{{}}}}],"thumbnailJpegBase64":"/9j/ZmFrZS10aHVtYv/Z"}}}}\n' "$id" ;;
    plugin.shutdown) printf '{{"id":"%s","result":{{"accepted":true}}}}\n' "$id" ;;
    *) printf '{{"id":"%s","error":{{"code":"PLUGIN_INVALID_REQUEST","message":"unsupported method"}}}}\n' "$id" ;;
  esac
done
"#,
        codec
    );
    fs::write(path, script)?;
    make_executable(path)?;
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions)
}
