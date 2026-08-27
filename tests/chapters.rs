use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::{LibraryService, LibrarySettingsPatch},
        scanner::LibraryScanner,
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn emby_exposes_source_scoped_intro_outro_markers() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Mixed", LibraryKind::Mixed, false)
        .await?;
    libraries
        .update_settings(
            library.id,
            LibrarySettingsPatch {
                chapter_source_id: Some(Some("org.lux.intro-outro-detector".to_owned())),
                ..LibrarySettingsPatch::default()
            },
        )
        .await?;
    let media_root = temp_dir.path().join("Mixed");
    let episode_root = media_root.join("Marker Show").join("Season 01");
    tokio::fs::create_dir_all(&episode_root).await?;
    tokio::fs::write(episode_root.join("Marker.Show.S01E01.mkv"), b"base").await?;
    tokio::fs::write(episode_root.join("Marker.Show.S01E01.2160p.mkv"), b"high").await?;
    tokio::fs::write(media_root.join("Other.Movie.2024.mkv"), b"movie").await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_mixed_library(library.id)
        .await?;

    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'EPISODE'")
            .fetch_one(database.pool())
            .await?;
    let movie_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE'")
            .fetch_one(database.pool())
            .await?;
    let sources: Vec<(String, String)> = sqlx::query_as(
        "SELECT ms.id, fe.relative_path
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE ms.item_id = ? ORDER BY fe.relative_path",
    )
    .bind(&item_id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(sources.len(), 2);

    sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, name, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("base-intro-start")
    .bind(&sources[0].0)
    .bind(10_000_000_i64)
    .bind(Option::<String>::None)
    .bind("INTRO_START")
    .bind(0_i64)
    .bind("org.lux.intro-outro-detector")
    .bind(0.98_f64)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, name, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("high-credits-start")
    .bind(&sources[1].0)
    .bind(900_000_000_i64)
    .bind("片尾")
    .bind("CREDITS_START")
    .bind(2_i64)
    .bind("org.lux.intro-outro-detector")
    .bind(0.91_f64)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, name, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("other-intro-start")
    .bind(&sources[0].0)
    .bind(20_000_000_i64)
    .bind(Option::<String>::None)
    .bind("INTRO_START")
    .bind(1_i64)
    .bind("another-chapter-source")
    .bind(0.87_f64)
    .execute(database.pool())
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
    let client = reqwest::Client::new();
    let login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="LuxTest", Device="Mac", DeviceId="chapters-admin", Version="1""#,
        )
        .json(&serde_json::json!({"Username": "admin", "Pw": "correct password"}))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing admin token")?
        .to_owned();

    let web_login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({"username": "admin", "password": "correct password"}))
        .send()
        .await?;
    assert_eq!(web_login.status(), reqwest::StatusCode::OK);
    let web_cookie = cookie_pair(web_login.headers());
    let lux_item = client
        .get(format!("http://{address}/api/v1/items/{item_id}"))
        .header(COOKIE, &web_cookie)
        .send()
        .await?;
    assert_eq!(lux_item.status(), reqwest::StatusCode::OK);
    let lux_body = lux_item.json::<Value>().await?;
    let lux_sources = lux_body["mediaSources"]
        .as_array()
        .ok_or("missing Lux media sources")?;
    let lux_base = lux_sources
        .iter()
        .find(|source| source["id"] == sources[0].0)
        .ok_or("missing Lux base source")?;
    assert_eq!(lux_base["chapters"].as_array().map(Vec::len), Some(1));
    assert_eq!(lux_base["chapters"][0]["startPositionTicks"], 10_000_000);
    assert_eq!(lux_base["chapters"][0]["markerType"], "INTRO_START");
    assert!(lux_base["chapters"][0]["name"].is_null());
    assert!(lux_base["chapters"][0].get("providerId").is_none());
    let lux_high = lux_sources
        .iter()
        .find(|source| source["id"] == sources[1].0)
        .ok_or("missing Lux high source")?;
    assert_eq!(lux_high["chapters"].as_array().map(Vec::len), Some(1));
    assert_eq!(lux_high["chapters"][0]["startPositionTicks"], 900_000_000);
    assert_eq!(lux_high["chapters"][0]["name"], "片尾");
    assert_eq!(lux_high["chapters"][0]["chapterIndex"], 2);
    assert!(lux_high["chapters"][0].get("confidence").is_none());

    let item = client
        .get(format!("http://{address}/Items/{item_id}?Fields=Chapters"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    let item_status = item.status();
    let item_text = item.text().await?;
    assert_eq!(item_status, reqwest::StatusCode::OK, "{item_text}");
    let item_body: Value = serde_json::from_str(&item_text)?;
    assert_eq!(item_body["Chapters"].as_array().map(Vec::len), Some(1));
    assert_eq!(item_body["Chapters"][0]["MarkerType"], "IntroStart");
    assert_eq!(item_body["Chapters"][0]["StartPositionTicks"], 10_000_000);
    assert_eq!(item_body["Chapters"][0]["ChapterIndex"], 0);
    assert!(item_body["Chapters"][0].get("Name").is_none());

    let movie = client
        .get(format!("http://{address}/Items/{movie_id}?Fields=Chapters"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(movie.status(), reqwest::StatusCode::OK);
    assert_eq!(
        movie.json::<Value>().await?["Chapters"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    let playback = client
        .get(format!("http://{address}/Items/{item_id}/PlaybackInfo"))
        .query(&[("api_key", token.as_str())])
        .send()
        .await?;
    assert_eq!(playback.status(), reqwest::StatusCode::OK);
    let playback_body = playback.json::<Value>().await?;
    let media_sources = playback_body["MediaSources"]
        .as_array()
        .ok_or("missing media sources")?;
    assert_eq!(media_sources.len(), 2);
    for source in media_sources {
        assert!(source["Chapters"].is_array());
    }
    let base = media_sources
        .iter()
        .find(|source| source["Id"] == sources[0].0)
        .ok_or("missing base source")?;
    assert_eq!(base["Chapters"][0]["MarkerType"], "IntroStart");
    let high = media_sources
        .iter()
        .find(|source| source["Id"] == sources[1].0)
        .ok_or("missing high source")?;
    assert_eq!(high["Chapters"][0]["MarkerType"], "CreditsStart");
    assert_eq!(high["Chapters"][0]["Name"], "片尾");
    assert_eq!(high["Chapters"][0]["ChapterIndex"], 2);

    libraries
        .update_settings(
            library.id,
            LibrarySettingsPatch {
                chapter_source_id: Some(Some("another-chapter-source".to_owned())),
                ..LibrarySettingsPatch::default()
            },
        )
        .await?;
    let switched_item = client
        .get(format!("http://{address}/Items/{item_id}?Fields=Chapters"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(switched_item.status(), reqwest::StatusCode::OK);
    let switched_body = switched_item.json::<Value>().await?;
    assert_eq!(switched_body["Chapters"].as_array().map(Vec::len), Some(1));
    assert_eq!(switched_body["Chapters"][0]["MarkerType"], "IntroStart");
    assert_eq!(
        switched_body["Chapters"][0]["StartPositionTicks"],
        20_000_000
    );

    libraries
        .update_settings(
            library.id,
            LibrarySettingsPatch {
                chapter_source_id: Some(None),
                ..LibrarySettingsPatch::default()
            },
        )
        .await?;
    let cleared_item = client
        .get(format!("http://{address}/Items/{item_id}?Fields=Chapters"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(cleared_item.status(), reqwest::StatusCode::OK);
    assert_eq!(
        cleared_item.json::<Value>().await?["Chapters"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    server.abort();
    database.close().await;
    let _ = admin;
    Ok(())
}

fn cookie_pair(headers: &reqwest::header::HeaderMap) -> String {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .collect::<Vec<_>>()
        .join("; ")
}
