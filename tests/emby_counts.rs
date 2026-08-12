use luxd::{
    api::{AppState, app_with_state},
    application::libraries::LibraryService,
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::net::TcpListener;

async fn insert_count_item(
    database: &Database,
    id: &str,
    library_id: &str,
    item_type: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO media_items
         (id, library_id, item_type, title, sort_title, identification_status,
          identity_key, has_available_source)
         VALUES (?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', ?, 1)",
    )
    .bind(id)
    .bind(library_id)
    .bind(item_type)
    .bind(id)
    .bind(id)
    .bind(format!("counts:{id}"))
    .execute(database.pool())
    .await
    .map(|_| ())
}

async fn emby_login(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base_url}/emby/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="CountsTest", Device="Mac", DeviceId="counts-device", Version="1""#,
        )
        .json(&json!({ "Username": username, "Pw": password }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await?;
    Ok(body["AccessToken"]
        .as_str()
        .ok_or("missing access token")?
        .to_owned())
}

#[tokio::test]
async fn emby_item_counts_respects_auth_user_scope_and_favorites()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let _admin = setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let movie_library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let series_library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;

    for (id, item_type) in [("count-movie-1", "MOVIE"), ("count-movie-2", "MOVIE")] {
        insert_count_item(&database, id, &movie_library.id.to_string(), item_type).await?;
    }
    for (id, item_type) in [
        ("count-series-1", "SERIES"),
        ("count-season-1", "SEASON"),
        ("count-episode-1", "EPISODE"),
        ("count-episode-2", "EPISODE"),
        ("count-box-set-1", "BOX_SET"),
    ] {
        insert_count_item(&database, id, &series_library.id.to_string(), item_type).await?;
    }
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, ?, 1)",
    )
    .bind(viewer.id.to_string())
    .bind(movie_library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, is_favorite)
         VALUES (?, ?, 1)",
    )
    .bind(viewer.id.to_string())
    .bind("count-movie-1")
    .execute(database.pool())
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

    let unauthenticated = client
        .get(format!("{base_url}/emby/Items/Counts"))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let admin_token = emby_login(&client, &base_url, "admin", "correct password").await?;
    let admin_counts = client
        .get(format!("{base_url}/emby/Items/Counts"))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(admin_counts.status(), reqwest::StatusCode::OK);
    let admin_body: Value = admin_counts.json().await?;
    assert_eq!(admin_body["MovieCount"], 2);
    assert_eq!(admin_body["SeriesCount"], 1);
    assert_eq!(admin_body["EpisodeCount"], 2);
    assert_eq!(admin_body["BoxSetCount"], 1);
    assert_eq!(admin_body["ItemCount"], 7);
    assert_eq!(admin_body["SongCount"], 0);

    let root_counts = client
        .get(format!("{base_url}/Items/Counts"))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(root_counts.status(), reqwest::StatusCode::OK);
    assert_eq!(root_counts.json::<Value>().await?["ItemCount"], 7);

    let viewer_counts = client
        .get(format!("{base_url}/emby/Items/Counts?UserId={}", viewer.id))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(viewer_counts.status(), reqwest::StatusCode::OK);
    let viewer_body: Value = viewer_counts.json().await?;
    assert_eq!(viewer_body["MovieCount"], 2);
    assert_eq!(viewer_body["SeriesCount"], 0);
    assert_eq!(viewer_body["EpisodeCount"], 0);
    assert_eq!(viewer_body["ItemCount"], 2);

    let viewer_favorites = client
        .get(format!(
            "{base_url}/emby/Items/Counts?UserId={}&IsFavorite=true",
            viewer.id
        ))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(viewer_favorites.status(), reqwest::StatusCode::OK);
    let favorites_body: Value = viewer_favorites.json().await?;
    assert_eq!(favorites_body["MovieCount"], 1);
    assert_eq!(favorites_body["ItemCount"], 1);

    server.abort();
    Ok(())
}
