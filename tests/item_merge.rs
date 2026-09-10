use std::time::Duration;

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::{
    StatusCode,
    header::{COOKIE, SET_COOKIE},
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use uuid::Uuid;

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

async fn insert_item(
    database: &Database,
    id: &str,
    library_id: &str,
    item_type: &str,
    title: &str,
    parent_id: Option<&str>,
    series_id: Option<&str>,
    season_number: Option<i64>,
    episode_number: Option<i64>,
    has_available_source: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, parent_id, series_id,
             season_number, episode_number, title, sort_title,
             identification_status, has_available_source
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', ?)",
    )
    .bind(id)
    .bind(library_id)
    .bind(item_type)
    .bind(parent_id)
    .bind(series_id)
    .bind(season_number)
    .bind(episode_number)
    .bind(title)
    .bind(title.to_lowercase())
    .bind(i64::from(has_available_source))
    .execute(database.pool())
    .await
    .map(|_| ())
}

async fn insert_source(database: &Database, id: &str, item_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO media_sources (id, item_id, source_kind, is_default, probe_status)
         VALUES (?, ?, 'LOCAL_FILE', 1, 'PENDING')",
    )
    .bind(id)
    .bind(item_id)
    .execute(database.pool())
    .await
    .map(|_| ())
}

async fn start_app(
    config: Config,
    database: Database,
) -> Result<
    (
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
        String,
        String,
        String,
    ),
    Box<dyn std::error::Error>,
> {
    let setup = SetupService::new(database.clone())?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config, database, setup, web_auth, emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let session = cookie_value(login.headers(), "lux_session");
    let csrf = cookie_value(login.headers(), "lux_csrf");
    Ok((
        server,
        format!("http://{address}"),
        format!("lux_session={session}; lux_csrf={csrf}"),
        csrf,
    ))
}

#[tokio::test]
async fn admin_can_merge_movie_and_series_items_without_losing_sources_or_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let library = LibraryService::new(database.clone())
        .create_library("Mixed", LibraryKind::Mixed, false)
        .await?;
    let library_id = library.id.to_string();

    let movie_primary = Uuid::now_v7().to_string();
    let movie_secondary = Uuid::now_v7().to_string();
    insert_item(
        &database,
        &movie_primary,
        &library_id,
        "MOVIE",
        "电影主条目",
        None,
        None,
        None,
        None,
        true,
    )
    .await?;
    insert_item(
        &database,
        &movie_secondary,
        &library_id,
        "MOVIE",
        "电影其他版本",
        None,
        None,
        None,
        None,
        true,
    )
    .await?;
    insert_source(&database, "movie-source-primary", &movie_primary).await?;
    insert_source(&database, "movie-source-secondary", &movie_secondary).await?;
    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, position_ticks, is_favorite)
         VALUES (?, ?, 123, 1)",
    )
    .bind(admin.id.to_string())
    .bind(&movie_secondary)
    .execute(database.pool())
    .await?;

    let series_primary = Uuid::now_v7().to_string();
    let series_secondary = Uuid::now_v7().to_string();
    let primary_season = Uuid::now_v7().to_string();
    let secondary_season_overlap = Uuid::now_v7().to_string();
    let secondary_season_extra = Uuid::now_v7().to_string();
    let primary_episode = Uuid::now_v7().to_string();
    let secondary_episode_overlap = Uuid::now_v7().to_string();
    let secondary_episode_extra = Uuid::now_v7().to_string();
    insert_item(
        &database,
        &series_primary,
        &library_id,
        "SERIES",
        "剧集主条目",
        None,
        None,
        None,
        None,
        false,
    )
    .await?;
    insert_item(
        &database,
        &series_secondary,
        &library_id,
        "SERIES",
        "剧集其他版本",
        None,
        None,
        None,
        None,
        false,
    )
    .await?;
    insert_item(
        &database,
        &primary_season,
        &library_id,
        "SEASON",
        "Season 01",
        Some(&series_primary),
        Some(&series_primary),
        Some(1),
        None,
        false,
    )
    .await?;
    insert_item(
        &database,
        &secondary_season_overlap,
        &library_id,
        "SEASON",
        "Season 01",
        Some(&series_secondary),
        Some(&series_secondary),
        Some(1),
        None,
        false,
    )
    .await?;
    insert_item(
        &database,
        &secondary_season_extra,
        &library_id,
        "SEASON",
        "Season 02",
        Some(&series_secondary),
        Some(&series_secondary),
        Some(2),
        None,
        false,
    )
    .await?;
    insert_item(
        &database,
        &primary_episode,
        &library_id,
        "EPISODE",
        "第一集",
        Some(&primary_season),
        Some(&series_primary),
        Some(1),
        Some(1),
        true,
    )
    .await?;
    insert_item(
        &database,
        &secondary_episode_overlap,
        &library_id,
        "EPISODE",
        "第一集其他版本",
        Some(&secondary_season_overlap),
        Some(&series_secondary),
        Some(1),
        Some(1),
        true,
    )
    .await?;
    insert_item(
        &database,
        &secondary_episode_extra,
        &library_id,
        "EPISODE",
        "第二季第一集",
        Some(&secondary_season_extra),
        Some(&series_secondary),
        Some(2),
        Some(1),
        true,
    )
    .await?;
    insert_source(&database, "episode-source-primary", &primary_episode).await?;
    insert_source(
        &database,
        "episode-source-overlap",
        &secondary_episode_overlap,
    )
    .await?;
    insert_source(&database, "episode-source-extra", &secondary_episode_extra).await?;

    let (server, base_url, cookies, csrf) = start_app(config, database.clone()).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let merge = client
        .post(format!("{base_url}/api/v1/admin/items/merge"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "itemIds": [&movie_secondary, &movie_primary],
            "primaryItemId": &movie_primary,
        }))
        .send()
        .await?;
    let merge_status = merge.status();
    let merge_text = merge.text().await?;
    assert_eq!(merge_status, StatusCode::OK, "merge response: {merge_text}");
    let merge_body: Value = serde_json::from_str(&merge_text)?;
    assert_eq!(merge_body["primaryItemId"], movie_primary);
    assert_eq!(merge_body["mergedItemIds"], json!([movie_secondary]));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_sources WHERE item_id = ?")
            .bind(&movie_primary)
            .fetch_one(database.pool())
            .await?,
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT position_ticks FROM user_item_state WHERE user_id = ? AND item_id = ?"
        )
        .bind(admin.id.to_string())
        .bind(&movie_primary)
        .fetch_one(database.pool())
        .await?,
        123
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT is_favorite FROM user_item_state WHERE user_id = ? AND item_id = ?"
        )
        .bind(admin.id.to_string())
        .bind(&movie_primary)
        .fetch_one(database.pool())
        .await?,
        1
    );

    let series_merge = client
        .post(format!("{base_url}/api/v1/admin/items/merge"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "itemIds": [&series_primary, &series_secondary],
            "primaryItemId": &series_primary,
        }))
        .send()
        .await?;
    assert_eq!(series_merge.status(), StatusCode::OK);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_sources WHERE item_id = ?")
            .bind(&primary_episode)
            .fetch_one(database.pool())
            .await?,
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT parent_id FROM media_items WHERE id = ?")
            .bind(&secondary_season_extra)
            .fetch_one(database.pool())
            .await?,
        series_primary
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT series_id FROM media_items WHERE id = ?")
            .bind(&secondary_episode_extra)
            .fetch_one(database.pool())
            .await?,
        series_primary
    );
    sqlx::query("UPDATE media_items SET has_available_source = 1 WHERE id IN (?, ?)")
        .bind(&movie_primary)
        .bind(&movie_secondary)
        .execute(database.pool())
        .await?;

    let catalog = client
        .get(format!(
            "{base_url}/api/v1/libraries/{library_id}/items?itemType=MOVIE,SERIES"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(catalog.status(), StatusCode::OK);
    let catalog_body: Value = catalog.json().await?;
    assert_eq!(catalog_body["total"], 2, "catalog response: {catalog_body}");
    let catalog_ids = catalog_body["items"]
        .as_array()
        .ok_or("catalog items missing")?
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect::<Vec<_>>();
    assert!(catalog_ids.contains(&movie_primary.as_str()));
    assert!(catalog_ids.contains(&series_primary.as_str()));
    assert!(!catalog_ids.contains(&movie_secondary.as_str()));
    assert!(!catalog_ids.contains(&series_secondary.as_str()));
    server.abort();
    Ok(())
}
