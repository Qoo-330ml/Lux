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

async fn insert_movie(
    database: &Database,
    library_id: &str,
    item_id: &str,
    title: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title,
             identification_status, has_available_source
         ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1)",
    )
    .bind(item_id)
    .bind(library_id)
    .bind(title)
    .bind(title.to_lowercase())
    .execute(database.pool())
    .await
    .map(|_| ())
}

async fn insert_source(database: &Database, id: &str, item_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO media_sources (id, item_id, source_kind, is_default, probe_status)
         VALUES (?, ?, 'LOCAL_FILE', 1, 'READY')",
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
        .no_proxy()
        .timeout(Duration::from_secs(5))
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
async fn admin_can_list_merge_records_and_unmerge_items() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:0".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Mixed, false)
        .await?;
    let library_id = library.id.to_string();
    let primary_id = uuid::Uuid::now_v7().to_string();
    let secondary_id = uuid::Uuid::now_v7().to_string();
    insert_movie(&database, &library_id, &primary_id, "Primary Movie").await?;
    insert_movie(&database, &library_id, &secondary_id, "Secondary Movie").await?;
    insert_source(&database, "source-primary", &primary_id).await?;
    insert_source(&database, "source-secondary", &secondary_id).await?;
    let _ = admin;

    let (server, base_url, cookies, csrf) = start_app(config, database.clone()).await?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;

    let merge = client
        .post(format!("{base_url}/api/v1/admin/items/merge"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "itemIds": [&primary_id, &secondary_id],
            "primaryItemId": &primary_id,
        }))
        .send()
        .await?;
    let merge_status = merge.status();
    let merge_text = merge.text().await?;
    assert_eq!(merge_status, StatusCode::OK, "merge response: {merge_text}");

    let records = client
        .get(format!("{base_url}/api/v1/admin/items/{primary_id}/merges"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    let records_status = records.status();
    let records_text = records.text().await?;
    assert_eq!(
        records_status,
        StatusCode::OK,
        "records response: {records_text}"
    );
    let records_body: Value = serde_json::from_str(&records_text)?;
    let record = &records_body["records"][0];
    assert_eq!(record["mergedItemId"], secondary_id);
    assert_eq!(record["mergedItemTitle"], "Secondary Movie");
    assert_eq!(record["itemType"], "MOVIE");
    assert_eq!(record["origin"], "MANUAL");
    assert_eq!(record["sourceCount"], 1);
    assert!(record["undoneAt"].is_null());

    let missing_csrf = client
        .post(format!("{base_url}/api/v1/admin/items/unmerge"))
        .header(COOKIE, &cookies)
        .json(&json!({
            "primaryItemId": &primary_id,
            "mergedItemId": &secondary_id,
        }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), StatusCode::FORBIDDEN);

    let unmerge = client
        .post(format!("{base_url}/api/v1/admin/items/unmerge"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "primaryItemId": &primary_id,
            "mergedItemId": &secondary_id,
        }))
        .send()
        .await?;
    let unmerge_status = unmerge.status();
    let unmerge_text = unmerge.text().await?;
    assert_eq!(
        unmerge_status,
        StatusCode::OK,
        "unmerge response: {unmerge_text}"
    );
    let unmerge_body: Value = serde_json::from_str(&unmerge_text)?;
    assert_eq!(unmerge_body["primaryItemId"], primary_id);
    assert_eq!(unmerge_body["mergedItemId"], secondary_id);
    assert_eq!(unmerge_body["libraryId"], library_id);
    assert_eq!(unmerge_body["itemType"], "MOVIE");
    assert_eq!(unmerge_body["restoredSourceCount"], 1);
    assert_eq!(unmerge_body["restoredItemIds"], json!([secondary_id]));
    assert_eq!(unmerge_body["userStateRestored"], false);

    let secondary_sources: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_sources WHERE item_id = ?")
            .bind(&secondary_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(secondary_sources, 1);
    let primary_sources: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_sources WHERE item_id = ?")
            .bind(&primary_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(primary_sources, 1);

    let repeated = client
        .post(format!("{base_url}/api/v1/admin/items/unmerge"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "primaryItemId": &primary_id,
            "mergedItemId": &secondary_id,
        }))
        .send()
        .await?;
    assert_eq!(repeated.status(), StatusCode::CONFLICT);

    let records_after = client
        .get(format!("{base_url}/api/v1/admin/items/{primary_id}/merges"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    let records_after_body: Value = serde_json::from_str(&records_after.text().await?)?;
    assert!(records_after_body["records"][0]["undoneAt"].is_number());

    server.abort();
    Ok(())
}
