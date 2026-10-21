use std::fs;

use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        plugins::{MEDIA_INFO_PLUGIN_ID, PluginService},
        scanner::LibraryScanner,
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn only_authorized_playing_callback_triggers_strm_probe()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_root = config_dir.join("plugins/org.lux.strm-media-info");
    tokio::fs::create_dir_all(plugin_root.join("binaries")).await?;
    fs::write(plugin_root.join("binaries/plugin"), b"placeholder")?;
    tokio::fs::write(
        plugin_root.join("manifest.json"),
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
    )
    .await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let library_service = LibraryService::new(database.clone());
    let library = library_service
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    for name in [
        "Playing.Movie.2024.strm",
        "Paused.Movie.2024.strm",
        "Stopped.Movie.2024.strm",
    ] {
        tokio::fs::write(
            media_root.join(name),
            format!("https://media.example.invalid/{name}.mkv"),
        )
        .await?;
    }
    library_service
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-utf8 media root")?,
        )
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let sources: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT mi.title, mi.id, ms.id
         FROM media_items mi JOIN media_sources ms ON ms.item_id = mi.id
         WHERE ms.source_kind = 'STRM_URL'",
    )
    .fetch_all(database.pool())
    .await?;
    let find_source = |title: &str| {
        sources
            .iter()
            .find(|(source_title, _, _)| source_title == title)
            .map(|(_, item_id, source_id)| (item_id.clone(), source_id.clone()))
    };
    let (playing_item, playing_source) =
        find_source("Playing Movie").ok_or("playing STRM source missing")?;
    let (paused_item, paused_source) =
        find_source("Paused Movie").ok_or("paused STRM source missing")?;
    let (stopped_item, stopped_source) =
        find_source("Stopped Movie").ok_or("stopped STRM source missing")?;

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
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="STRMProbeTest", Device="Mac", DeviceId="strm-probe-callback", Version="1""#,
        )
        .json(&json!({"Username": "admin", "Pw": "correct password"}))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing Emby token")?
        .to_owned();

    let playing = client
        .post(format!("{base_url}/Sessions/Playing"))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "ItemId": playing_item,
            "MediaSourceId": playing_source,
            "PlaySessionId": "playing-session",
            "PositionTicks": 0,
            "RunTimeTicks": 120000000,
            "DeviceId": "strm-probe-callback"
        }))
        .send()
        .await?;
    assert_eq!(playing.status(), reqwest::StatusCode::NO_CONTENT);

    let paused = client
        .post(format!("{base_url}/Sessions/Playing/Progress"))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "ItemId": paused_item,
            "MediaSourceId": paused_source,
            "PlaySessionId": "paused-session",
            "PositionTicks": 0,
            "RunTimeTicks": 120000000,
            "IsPaused": true,
            "DeviceId": "strm-probe-callback"
        }))
        .send()
        .await?;
    assert_eq!(paused.status(), reqwest::StatusCode::NO_CONTENT);

    let stopped = client
        .post(format!("{base_url}/Sessions/Playing/Stopped"))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "ItemId": stopped_item,
            "MediaSourceId": stopped_source,
            "PlaySessionId": "stopped-session",
            "PositionTicks": 0,
            "RunTimeTicks": 120000000,
            "DeviceId": "strm-probe-callback"
        }))
        .send()
        .await?;
    assert_eq!(stopped.status(), reqwest::StatusCode::NO_CONTENT);

    let mut target_source = None;
    for _ in 0..100 {
        target_source = sqlx::query_scalar::<_, Option<String>>(
            "SELECT target_source_id FROM strm_probe_jobs ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_optional(database.pool())
        .await?
        .flatten();
        if target_source.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(target_source.as_deref(), Some(playing_source.as_str()));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM strm_probe_jobs")
            .fetch_one(database.pool())
            .await?,
        1
    );

    server.abort();
    Ok(())
}
