use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService, metadata::MetadataEnricher, scanner::LibraryScanner,
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn library_acl_is_consistent_for_lists_details_and_images()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let users = UserStore::new(database.clone())?;
    let viewer = users
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let first = libraries
        .create_library("Movies A", LibraryKind::Movie, false)
        .await?;
    let second = libraries
        .create_library("Movies B", LibraryKind::Movie, false)
        .await?;
    let (first_item, _) = create_fixture(
        &database,
        &libraries,
        first.id,
        temp_dir.path().join("MoviesA"),
        "Allowed Movie",
        2020,
    )
    .await?;
    let (second_item, _) = create_fixture(
        &database,
        &libraries,
        second.id,
        temp_dir.path().join("MoviesB"),
        "Denied Movie",
        2021,
    )
    .await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config, database, setup, web_auth, emby_auth,
    ));
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
    let grant_first = client
        .patch(format!(
            "{base_url}/api/v1/admin/users/{}/libraries/{}",
            viewer.id, first.id
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "canView": true }))
        .send()
        .await?;
    assert_eq!(grant_first.status(), reqwest::StatusCode::OK);

    let viewer_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    let viewer_cookie = cookie_pair(viewer_login.headers());
    let visible_libraries = client
        .get(format!("{base_url}/api/v1/libraries"))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    let visible_body: Value = visible_libraries.json().await?;
    assert_eq!(visible_body["libraries"].as_array().map(Vec::len), Some(1));
    assert_eq!(visible_body["libraries"][0]["id"], first.id.to_string());

    let allowed_items = client
        .get(format!("{base_url}/api/v1/libraries/{}/items", first.id))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(allowed_items.status(), reqwest::StatusCode::OK);
    let denied_items = client
        .get(format!("{base_url}/api/v1/libraries/{}/items", second.id))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(denied_items.status(), reqwest::StatusCode::FORBIDDEN);
    let denied_detail = client
        .get(format!("{base_url}/api/v1/items/{second_item}"))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(denied_detail.status(), reqwest::StatusCode::NOT_FOUND);
    let denied_image = client
        .get(format!(
            "{base_url}/api/v1/items/{second_item}/images/poster"
        ))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(denied_image.status(), reqwest::StatusCode::NOT_FOUND);

    let viewer_emby_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="ACLTest", Device="Mac", DeviceId="acl-viewer", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    let viewer_token = viewer_emby_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let emby_views = client
        .get(format!("{base_url}/Users/{}/Views", viewer.id))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    let emby_views_body: Value = emby_views.json().await?;
    assert_eq!(emby_views_body["Items"].as_array().map(Vec::len), Some(1));
    let emby_denied_items = client
        .get(format!(
            "{base_url}/Users/{}/Items?ParentId={}",
            viewer.id, second.id
        ))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(emby_denied_items.status(), reqwest::StatusCode::FORBIDDEN);
    let emby_denied_detail = client
        .get(format!("{base_url}/Items/{second_item}"))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(emby_denied_detail.status(), reqwest::StatusCode::NOT_FOUND);

    let grant_second = client
        .patch(format!(
            "{base_url}/api/v1/admin/users/{}/libraries/{}",
            viewer.id, second.id
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "canView": true }))
        .send()
        .await?;
    assert_eq!(grant_second.status(), reqwest::StatusCode::OK);
    let now_allowed = client
        .get(format!("{base_url}/api/v1/items/{second_item}"))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(now_allowed.status(), reqwest::StatusCode::OK);

    server.abort();
    assert_ne!(admin.id, viewer.id);
    assert_ne!(first_item, second_item);
    Ok(())
}

async fn create_fixture(
    database: &Database,
    libraries: &LibraryService,
    library_id: luxd::domain::ids::LibraryId,
    root: std::path::PathBuf,
    title: &str,
    year: i32,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let movie_dir = root.join(format!("{title} ({year})"));
    tokio::fs::create_dir_all(&movie_dir).await?;
    let stem = title.replace(' ', ".");
    tokio::fs::write(movie_dir.join(format!("{stem}.{year}.mkv")), b"movie").await?;
    tokio::fs::write(movie_dir.join("poster.jpg"), b"poster").await?;
    libraries
        .add_root(library_id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library_id)
        .await?;
    MetadataEnricher::new(database.clone())
        .enrich_movie_library(library_id)
        .await?;
    let item: (String, String) = sqlx::query_as(
        "SELECT mi.id, ii.id
         FROM media_items mi
         JOIN item_images ii ON ii.item_id = mi.id AND ii.image_type = 'POSTER'
         WHERE mi.library_id = ?",
    )
    .bind(library_id.to_string())
    .fetch_one(database.pool())
    .await?;
    Ok(item)
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
