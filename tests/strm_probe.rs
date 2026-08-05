use std::{fs, path::Path};

use luxd::{
    application::{
        plugins::{MEDIA_INFO_PLUGIN_ID, PluginService},
        scanner::LibraryScanner,
        strm_probe::StrmProbeService,
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
    let plugin_dir = config_dir.join("plugins/org.lux.media-info");
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

    let plugin_binary = std::env::var_os("CARGO_BIN_EXE_lux-plugin-media-info")
        .or_else(|| std::env::var_os("CARGO_BIN_EXE_lux_plugin_media_info"))
        .ok_or("media-info plugin binary path is missing")?;
    let wrapper = binary_dir.join("plugin");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nLUX_FFPROBE_BINARY={} exec {} \"$@\"\n",
            shell_quote(&fake_ffprobe),
            shell_quote(Path::new(&plugin_binary)),
        ),
    )?;
    make_executable(&wrapper)?;
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

    let plugins = PluginService::new(database.clone(), config_dir);
    plugins.install(MEDIA_INFO_PLUGIN_ID).await?;
    let service = StrmProbeService::new(database.clone(), plugins);
    let jobs = service.create_jobs(&[library.id], 2, false, true).await?;
    assert_eq!(jobs.len(), 1);
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

    fs::write(&fake_ffprobe, "#!/bin/sh\nexit 9\n")?;
    let skip_jobs = service.create_jobs(&[library.id], 2, false, false).await?;
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
    let overwrite_jobs = service.create_jobs(&[library.id], 2, true, false).await?;
    service.run(&overwrite_jobs[0].id).await?;
    let overwritten_codec: String = sqlx::query_scalar(
        "SELECT codec FROM media_streams WHERE media_source_id = (SELECT id FROM media_sources)\n         AND stream_type = 'VIDEO'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(overwritten_codec, "vp9");
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions)
}

#[cfg(unix)]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}
