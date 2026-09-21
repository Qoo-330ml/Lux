use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService, metadata::MetadataEnricher, scanner::LibraryScanner,
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> String {
    headers
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
async fn login_background_uses_recent_posters_only_when_enabled()
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
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Image Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Image.Movie.2020.mkv"), b"movie").await?;
    tokio::fs::write(movie_dir.join("poster.jpg"), b"poster-bytes").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    MetadataEnricher::new(database.clone())
        .enrich_movie_library(library.id)
        .await?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let default_background = client
        .get(format!("{base_url}/api/v1/auth/login-background"))
        .send()
        .await?;
    assert_eq!(default_background.status(), reqwest::StatusCode::OK);
    let default_body: Value = default_background.json().await?;
    assert_eq!(default_body["source"], "STATIC");
    assert_eq!(default_body["images"].as_array().map(Vec::len), Some(0));

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(login.headers(), "lux_session"),
        cookie_value(login.headers(), "lux_csrf")
    );

    let invalid = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", cookie_value(login.headers(), "lux_csrf"))
        .json(&json!({ "loginBackgroundSource": "TMDB_POPULAR" }))
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);

    let updated = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", cookie_value(login.headers(), "lux_csrf"))
        .json(&json!({ "loginBackgroundSource": "RECENTLY_ADDED" }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    assert_eq!(
        updated.json::<Value>().await?["loginBackgroundSource"],
        "RECENTLY_ADDED"
    );

    let background = client
        .get(format!("{base_url}/api/v1/auth/login-background"))
        .send()
        .await?;
    assert_eq!(background.status(), reqwest::StatusCode::OK);
    let body: Value = background.json().await?;
    assert_eq!(body["source"], "RECENTLY_ADDED");
    let image_url = body["images"]
        .as_array()
        .and_then(|images| images.first())
        .and_then(Value::as_str)
        .ok_or("missing login background image")?;
    assert!(image_url.starts_with("/emby/Items/"));
    assert!(image_url.contains("/Images/Primary?tag="));
    assert!(!body.to_string().contains("Image Movie"));
    assert!(!body.to_string().contains("Movies"));
    assert!(!body.to_string().contains("poster.jpg"));

    let poster = client.get(format!("{base_url}{image_url}")).send().await?;
    assert_eq!(poster.status(), reqwest::StatusCode::OK);
    assert_eq!(poster.bytes().await?.as_ref(), b"poster-bytes");

    server.abort();
    Ok(())
}
