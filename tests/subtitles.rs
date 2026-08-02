use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        probe::{FfprobeRunner, MediaProbeService},
        scanner::LibraryScanner,
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn external_subtitles_are_indexed_served_and_acl_protected()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Probe Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Probe.Movie.2024.mkv"), b"movie").await?;
    tokio::fs::write(
        movie_dir.join("Probe.Movie.2024.en.srt"),
        b"1\n00:00:00,000 --> 00:00:01,000\nHello\n",
    )
    .await?;
    tokio::fs::write(
        movie_dir.join("Probe.Movie.2024.zh-Hans.forced.vtt"),
        "WEBVTT\n\n00:00.000 --> 00:01.000\n你好\n".as_bytes(),
    )
    .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_movie_library(library.id).await?;
    let script = executable_script(
        temp_dir.path(),
        r#"#!/bin/sh
printf '%s' '{"format":{"format_name":"matroska","duration":"10","bit_rate":"1000"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264"},{"index":1,"codec_type":"audio","codec_name":"aac"}]}'
"#,
    )?;
    MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(script, Duration::from_secs(5)),
    )
    .probe_movie_library(library.id)
    .await?;
    let indexed: Vec<(i64, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT stream_index, external_path, language, title
         FROM media_streams WHERE external_path IS NOT NULL ORDER BY stream_index",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(indexed.len(), 2);
    assert_eq!(indexed[0].0, 2);
    assert_eq!(indexed[0].2.as_deref(), Some("eng"));
    assert_eq!(indexed[1].0, 3);
    assert_eq!(indexed[1].2.as_deref(), Some("zho"));
    assert_eq!(indexed[1].3.as_deref(), Some("zh Hans forced"));
    let flags: Vec<(i64, i64, i64)> = sqlx::query_as(
        "SELECT is_external, is_default, is_forced
         FROM media_streams WHERE external_path IS NOT NULL ORDER BY stream_index",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(flags, [(1, 0, 0), (1, 0, 1)]);

    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE'")
            .fetch_one(database.pool())
            .await?;
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="SubtitleTest", Device="Mac", DeviceId="subtitle-admin", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();
    let item = client
        .get(format!("{base_url}/Items/{item_id}"))
        .header("X-Emby-Token", &token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    let streams = &item["MediaSources"][0]["MediaStreams"];
    assert_eq!(streams[0]["IsExternal"], false);
    assert_eq!(streams[2]["IsExternal"], true);
    assert_eq!(streams[2]["IsForced"], false);
    assert_eq!(streams[3]["IsForced"], true);
    let subtitle = client
        .get(format!(
            "{base_url}/Videos/{item_id}/{source_id}/Subtitles/2/Stream"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(subtitle.status(), reqwest::StatusCode::OK);
    assert_eq!(
        subtitle.headers()["content-type"],
        "text/plain; charset=utf-8"
    );
    assert!(String::from_utf8(subtitle.bytes().await?.to_vec())?.contains("Hello"));

    let viewer_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="SubtitleTest", Device="Mac", DeviceId="subtitle-viewer", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    let viewer_token = viewer_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let denied = client
        .get(format!(
            "{base_url}/Videos/{item_id}/{source_id}/Subtitles/2/Stream"
        ))
        .header("X-Emby-Token", viewer_token)
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    assert_ne!(admin.id, viewer.id);
    Ok(())
}

fn executable_script(
    directory: &Path,
    contents: &str,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let path = directory.join(format!("fake-ffprobe-{}", uuid::Uuid::now_v7()));
    fs::write(&path, contents)?;
    let mut permissions = fs::metadata(&path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions)?;
    Ok(path)
}
