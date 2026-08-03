use std::time::Duration;

use axum::{Json, Router, routing::get};
use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        scanner::LibraryScanner,
        setup::SetupService,
        tmdb::{TmdbClient, TmdbClientConfig},
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

async fn tmdb_movie() -> Json<Value> {
    Json(json!({
        "id": 7,
        "title": "Movie A",
        "belongs_to_collection": { "id": 10, "name": "Stub Collection" }
    }))
}

async fn tmdb_collection() -> Json<Value> {
    Json(json!({
        "id": 10,
        "name": "Stub Collection",
        "overview": "Collection overview",
        "parts": [
            { "id": 7, "title": "Movie A", "release_date": "2020-01-01" },
            { "id": 8, "title": "Movie B", "release_date": "2021-01-01" }
        ]
    }))
}

#[tokio::test]
async fn tmdb_collection_refresh_is_idempotent_and_filters_members_by_acl()
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
    let first = libraries
        .create_library("Movies A", LibraryKind::Movie, false)
        .await?;
    let second = libraries
        .create_library("Movies B", LibraryKind::Movie, false)
        .await?;
    let first_root = temp_dir.path().join("MoviesA");
    let second_root = temp_dir.path().join("MoviesB");
    tokio::fs::create_dir_all(&first_root).await?;
    tokio::fs::create_dir_all(&second_root).await?;
    tokio::fs::write(first_root.join("Movie.A.2020.mkv"), b"a").await?;
    tokio::fs::write(second_root.join("Movie.B.2021.mkv"), b"b").await?;
    libraries
        .add_root(first.id, first_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    libraries
        .add_root(second.id, second_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_movie_library(first.id).await?;
    scanner.scan_movie_library(second.id).await?;

    let first_item: String = sqlx::query_scalar("SELECT id FROM media_items WHERE library_id = ?")
        .bind(first.id.to_string())
        .fetch_one(database.pool())
        .await?;
    let second_item: String = sqlx::query_scalar("SELECT id FROM media_items WHERE library_id = ?")
        .bind(second.id.to_string())
        .fetch_one(database.pool())
        .await?;
    sqlx::query("UPDATE media_items SET provider_ids_json = ? WHERE id = ?")
        .bind(r#"{"tmdb":"7"}"#)
        .bind(&first_item)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_items SET provider_ids_json = ? WHERE id = ?")
        .bind(r#"{"tmdb":"8"}"#)
        .bind(&second_item)
        .execute(database.pool())
        .await?;

    let tmdb_app = Router::new()
        .route("/3/movie/7", get(tmdb_movie))
        .route("/3/collection/10", get(tmdb_collection));
    let tmdb_listener = TcpListener::bind("127.0.0.1:0").await?;
    let tmdb_address = tmdb_listener.local_addr()?;
    let tmdb_server = tokio::spawn(async move { axum::serve(tmdb_listener, tmdb_app).await });
    let tmdb = TmdbClient::new(TmdbClientConfig {
        base_url: format!("http://{tmdb_address}"),
        api_key: None,
        read_access_token: Some("stub-token".to_owned()),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let state = AppState::ready(config, database.clone(), setup, web_auth, emby_auth)
        .with_tmdb_client(tmdb);
    let app = app_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    let refreshed = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{first_item}/collection/refresh"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(refreshed.status(), reqwest::StatusCode::OK);
    let refresh_body = refreshed.json::<Value>().await?;
    let collection_item_id = refresh_body["collectionItemId"]
        .as_str()
        .ok_or("missing collection item")?
        .to_owned();
    assert_eq!(refresh_body["memberCount"], 1);

    let refreshed_again = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{first_item}/collection/refresh"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(refreshed_again.status(), reqwest::StatusCode::OK);
    let refreshed_again_body = refreshed_again.json::<Value>().await?;
    assert_eq!(refreshed_again_body["collectionItemId"], collection_item_id);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM collections")
            .fetch_one(database.pool())
            .await?,
        1
    );
    sqlx::query(
        "INSERT INTO collection_items (collection_id, item_id, sort_order)
         SELECT c.id, ?, 1 FROM collections c WHERE c.item_id = ?",
    )
    .bind(&second_item)
    .bind(&collection_item_id)
    .execute(database.pool())
    .await?;

    let admin_collection = client
        .get(format!(
            "{base_url}/api/v1/collections/{collection_item_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(admin_collection.status(), reqwest::StatusCode::OK);
    assert_eq!(
        admin_collection.json::<Value>().await?["items"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );

    let grant = client
        .patch(format!(
            "{base_url}/api/v1/admin/users/{}/libraries/{}",
            viewer.id, first.id
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "canView": true }))
        .send()
        .await?;
    assert_eq!(grant.status(), reqwest::StatusCode::OK);
    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, is_favorite)
         VALUES (?, ?, 1)
         ON CONFLICT(user_id, item_id) DO UPDATE SET is_favorite = 1",
    )
    .bind(viewer.id.to_string())
    .bind(&first_item)
    .execute(database.pool())
    .await?;
    let viewer_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    let viewer_cookie = cookie_pair(viewer_login.headers());
    let collection = client
        .get(format!(
            "{base_url}/api/v1/collections/{collection_item_id}"
        ))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(collection.status(), reqwest::StatusCode::OK);
    let collection_body = collection.json::<Value>().await?;
    assert_eq!(collection_body["collection"]["itemType"], "BOX_SET");
    assert_eq!(collection_body["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(collection_body["items"][0]["id"], first_item);
    assert_eq!(collection_body["items"][0]["userData"]["isFavorite"], true);

    let viewer_emby_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="CollectionTest", Device="Mac", DeviceId="collection-viewer", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    let viewer_token = viewer_emby_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let children = client
        .get(format!("{base_url}/Items/{collection_item_id}/Children"))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(children.status(), reqwest::StatusCode::OK);
    let children_body = children.json::<Value>().await?;
    assert_eq!(children_body["Items"].as_array().map(Vec::len), Some(1));

    server.abort();
    tmdb_server.abort();
    assert_ne!(admin.id, viewer.id);
    Ok(())
}

fn cookie_pair(headers: &reqwest::header::HeaderMap) -> String {
    format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(headers, "lux_session"),
        cookie_value(headers, "lux_csrf")
    )
}

fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> String {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            value
                .strip_prefix(&format!("{name}="))
                .and_then(|value| value.split(';').next())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}
