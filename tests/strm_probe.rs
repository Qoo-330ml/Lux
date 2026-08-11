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
    tokio::fs::write(&strm_path, "https://media.example.invalid/video.mkv").await?;

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
    sqlx::query(
        "UPDATE media_sources
         SET probe_status = 'READY', container = 'strm', duration_ticks = NULL,
             bitrate = NULL, probe_error = NULL",
    )
    .execute(database.pool())
    .await?;

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

    let sidecar = tokio::fs::read(movie_dir.join("Plugin.Movie.2024-mediainfo.json")).await?;
    let sidecar: serde_json::Value = serde_json::from_slice(&sidecar)?;
    assert_eq!(sidecar[0]["MediaSourceInfo"]["Container"], "matroska");
    assert_eq!(
        sidecar[0]["MediaSourceInfo"]["MediaStreams"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );

    fs::write(&fake_ffmpeg, "#!/bin/sh\nexit 9\n")?;
    let unmarked_thumbnail_jobs = service
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
    service.run(&unmarked_thumbnail_jobs[0].id).await?;
    assert_eq!(
        service.get(&unmarked_thumbnail_jobs[0].id).await?.status,
        "COMPLETED"
    );
    assert!(!movie_dir.join("Plugin.Movie.2024-thumb.jpg").exists());

    sqlx::query(
        "UPDATE media_items
         SET thumbnail_fallback_required = 1
         WHERE id = (SELECT item_id FROM media_sources)",
    )
    .execute(database.pool())
    .await?;
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
    let thumbnail_path = movie_dir.join("Plugin.Movie.2024-thumb.jpg");
    let thumbnail = fs::read(&thumbnail_path)?;
    assert_eq!(thumbnail, b"\xff\xd8\xfffake-thumb\xff\xd9");
    let image: (String, String, i64) = sqlx::query_as(
        "SELECT local_path, source, file_size FROM item_images
         WHERE item_id = (SELECT item_id FROM media_sources) AND image_type = 'THUMB'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        fs::canonicalize(&image.0)?,
        fs::canonicalize(&thumbnail_path)?
    );
    assert_eq!(image.1, "STRM_FFMPEG");
    assert_eq!(image.2, i64::try_from(thumbnail.len())?);

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
