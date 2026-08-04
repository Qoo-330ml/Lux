use std::io::Cursor;

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::json;
use tokio::net::TcpListener;
use zip::ZipArchive;

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
async fn download_archives_only_the_selected_source_and_its_sidecars()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let download_dir = config.config_dir.join("downloads");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("二毛 (2019)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    let selected = "二毛 (2019) - 2160p - H.265 - AAC - test.mkv";
    tokio::fs::write(movie_dir.join(selected), b"selected video").await?;
    tokio::fs::write(
        movie_dir.join("二毛 (2019) - 2160p - H.265 - AAC - test.zh.ass"),
        b"subtitle",
    )
    .await?;
    tokio::fs::write(
        movie_dir.join("二毛 (2019) - 1080p - H.264 - AAC.mkv"),
        b"other video",
    )
    .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
        .fetch_one(database.pool())
        .await?;
    sqlx::query("UPDATE users SET can_download = 1 WHERE username_normalized = 'admin'")
        .execute(database.pool())
        .await?;
    let source_id: String = sqlx::query_scalar(
        "SELECT ms.id
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE ms.item_id = ? AND fe.relative_path = ?",
    )
    .bind(&item_id)
    .bind(format!("二毛 (2019)/{selected}"))
    .fetch_one(database.pool())
    .await?;

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cookie = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(login.headers(), "lux_session"),
        cookie_value(login.headers(), "lux_csrf")
    );

    let response = client
        .head(format!(
            "{base_url}/api/v1/items/{item_id}/download?sourceId={source_id}"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let mut entries = tokio::fs::read_dir(&download_dir).await?;
    assert!(entries.next_entry().await?.is_none());

    let response = client
        .get(format!(
            "{base_url}/api/v1/items/{item_id}/download?sourceId={source_id}"
        ))
        .header(COOKIE, cookie)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/zip");
    let bytes = response.bytes().await?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut names = (0..archive.len())
        .map(|index| archive.by_index(index).map(|file| file.name().to_owned()))
        .collect::<Result<Vec<_>, _>>()?;
    names.sort();
    assert_eq!(
        names,
        vec![
            "二毛 (2019) - 2160p - H.265 - AAC - test.mkv",
            "二毛 (2019) - 2160p - H.265 - AAC - test.zh.ass",
        ]
    );

    server.abort();
    Ok(())
}
