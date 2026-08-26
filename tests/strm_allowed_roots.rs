use std::{net::SocketAddr, time::Duration};

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, RANGE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn cookie_value(response: &reqwest::Response, name: &str) -> String {
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _) = value.split_once(';')?;
            let (cookie_name, cookie_value) = pair.split_once('=')?;
            (cookie_name == name).then(|| cookie_value.to_owned())
        })
        .expect("expected cookie")
}

#[tokio::test]
async fn configured_strm_root_enables_web_and_emby_local_playback()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let library_root = temp_dir.path().join("library");
    let allowed_root = temp_dir.path().join("allowed");
    tokio::fs::create_dir_all(&library_root).await?;
    tokio::fs::create_dir_all(&allowed_root).await?;
    let external_media = allowed_root.join("External.Movie.2026.mp4");
    tokio::fs::write(&external_media, b"external-media").await?;
    tokio::fs::write(
        library_root.join("External.Movie.2026.strm"),
        external_media.to_string_lossy().as_bytes(),
    )
    .await?;
    libraries
        .add_root(
            library.id,
            library_root.to_str().ok_or("library root is not UTF-8")?,
        )
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let (item_id, source_id): (String, String) = sqlx::query_as(
        "SELECT mi.id, ms.id
         FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         WHERE ms.source_kind = 'STRM_URL'",
    )
    .fetch_one(database.pool())
    .await?;

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let stream_url = format!("{base_url}/Videos/{item_id}/{source_id}/stream");

    let emby_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="StrmAllowedRootTest", Device="Mac", DeviceId="strm-allowed-root", Version="1""#,
        )
        .json(&json!({"Username": "admin", "Pw": "correct password"}))
        .send()
        .await?;
    let emby_token = emby_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing Emby token")?
        .to_owned();
    let denied = client
        .get(&stream_url)
        .header("X-Emby-Token", &emby_token)
        .header(RANGE, "bytes=0-")
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);

    let web_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({"username": "admin", "password": "correct password"}))
        .send()
        .await?;
    let session_cookie = cookie_value(&web_login, "lux_session");
    let csrf_cookie = cookie_value(&web_login, "lux_csrf");
    let cookies = format!("lux_session={session_cookie}; lux_csrf={csrf_cookie}");
    let default_settings = client
        .get(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(default_settings.status(), reqwest::StatusCode::OK);
    assert_eq!(
        default_settings.json::<Value>().await?["strmAllowedRoots"],
        json!([])
    );

    let invalid_settings = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf_cookie)
        .json(&json!({"strmAllowedRoots": ["relative/path"]}))
        .send()
        .await?;
    assert_eq!(invalid_settings.status(), reqwest::StatusCode::BAD_REQUEST);

    let settings = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf_cookie)
        .json(&json!({"strmAllowedRoots": [allowed_root.to_string_lossy()]}))
        .send()
        .await?;
    assert_eq!(settings.status(), reqwest::StatusCode::OK);
    let settings_body = settings.json::<Value>().await?;
    assert_eq!(
        settings_body["strmAllowedRoots"],
        json!([allowed_root.to_string_lossy()])
    );

    let emby_stream = client
        .get(&stream_url)
        .header("X-Emby-Token", &emby_token)
        .header(RANGE, "bytes=0-")
        .send()
        .await?;
    assert_eq!(emby_stream.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(emby_stream.bytes().await?.as_ref(), b"external-media");

    let web_create = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf_cookie)
        .json(&json!({
            "itemId": item_id,
            "sourceId": source_id,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(web_create.status(), reqwest::StatusCode::OK);
    let web_body = web_create.json::<Value>().await?;
    assert_eq!(web_body["plan"]["type"], "DIRECT");
    let web_direct_url = web_body["plan"]["url"]
        .as_str()
        .ok_or("missing Web direct URL")?;
    let web_stream = client
        .get(format!("{base_url}{web_direct_url}"))
        .header(RANGE, "bytes=0-")
        .send()
        .await?;
    assert_eq!(web_stream.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(web_stream.bytes().await?.as_ref(), b"external-media");

    let stored_strm =
        tokio::fs::read_to_string(library_root.join("External.Movie.2026.strm")).await?;
    assert_eq!(stored_strm, external_media.to_string_lossy());

    let cleared = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf_cookie)
        .json(&json!({"strmAllowedRoots": []}))
        .send()
        .await?;
    assert_eq!(cleared.status(), reqwest::StatusCode::OK);
    let denied_after_clear = client
        .get(&stream_url)
        .header("X-Emby-Token", &emby_token)
        .header(RANGE, "bytes=0-")
        .send()
        .await?;
    assert_eq!(denied_after_clear.status(), reqwest::StatusCode::FORBIDDEN);

    server.abort();
    Ok(())
}
