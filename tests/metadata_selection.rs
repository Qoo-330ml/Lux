use axum::{Router, body::Body, http::StatusCode, response::Response, routing::get};
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
use reqwest::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use uuid::Uuid;

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

#[tokio::test]
async fn admin_selection_fills_missing_fields_and_writes_nfo_and_images()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(true).await?;
    let (image_url, image_server) = start_image_stub().await?;
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "Online Title",
            "overview": "Online Overview",
            "productionYear": 2025,
            "posterUrl": format!("{image_url}/poster"),
            "fanartUrl": format!("{image_url}/fanart")
        }),
    )
    .await?;
    let item_id = fixture.item_id.clone();
    let (base_url, lux_server) = start_lux(&fixture).await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = login(&client, &base_url).await?;

    let response = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{item_id}/identify/candidates/{candidate_id}/select"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "mode": "fillMissing" }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await?;
    assert_eq!(body["status"], "ONLINE_CONFIRMED");
    assert_eq!(body["mode"], "fillMissing");
    assert_eq!(body["imageTypes"], json!(["POSTER", "FANART"]));

    let nfo = tokio::fs::read_to_string(&fixture.movie_dir.join("movie.nfo")).await?;
    assert!(nfo.contains("<title>本地标题</title>"));
    assert!(nfo.contains("<plot>Online Overview</plot>"));
    assert!(fixture.movie_dir.join("poster.png").exists());
    assert!(fixture.movie_dir.join("fanart.webp").exists());
    let status: String =
        sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(status, "ONLINE_CONFIRMED");
    let candidate_status: String =
        sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
            .bind(&candidate_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(candidate_status, "SELECTED");
    let image_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM item_images WHERE item_id = ? AND source = 'TMDB'",
    )
    .bind(&item_id)
    .fetch_one(fixture.database.pool())
    .await?;
    assert_eq!(image_count, 2);

    lux_server.abort();
    image_server.abort();
    Ok(())
}

#[tokio::test]
async fn failed_selection_stays_pending_and_can_be_retried()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    let (image_url, image_server) = start_image_stub().await?;
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "Retry Title",
            "posterUrl": format!("{image_url}/bad")
        }),
    )
    .await?;
    let item_id = fixture.item_id.clone();
    let (base_url, lux_server) = start_lux(&fixture).await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = login(&client, &base_url).await?;
    let path = format!(
        "{base_url}/api/v1/admin/items/{item_id}/identify/candidates/{candidate_id}/select"
    );

    let failed = client
        .post(&path)
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "mode": "refreshUnlocked" }))
        .send()
        .await?;
    assert!(failed.status().is_server_error());
    let status: String =
        sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(status, "LOCAL_CONFIRMED");
    let candidate_status: String =
        sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
            .bind(&candidate_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(candidate_status, "PENDING");

    sqlx::query("UPDATE metadata_candidates SET candidate_json = ? WHERE id = ?")
        .bind(
            json!({
                "title": "Retry Title",
                "posterUrl": format!("{image_url}/poster")
            })
            .to_string(),
        )
        .bind(&candidate_id)
        .execute(fixture.database.pool())
        .await?;
    let retried = client
        .post(&path)
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "mode": "refreshUnlocked" }))
        .send()
        .await?;
    assert_eq!(retried.status(), StatusCode::OK);
    let status: String =
        sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(status, "ONLINE_CONFIRMED");
    let candidate_status: String =
        sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
            .bind(&candidate_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(candidate_status, "SELECTED");

    lux_server.abort();
    image_server.abort();
    Ok(())
}

struct Fixture {
    _temp_dir: TempDir,
    config: Config,
    database: Database,
    setup: SetupService,
    item_id: String,
    movie_dir: std::path::PathBuf,
}

async fn prepare_fixture(with_local_nfo: bool) -> Result<Fixture, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Example Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2020.mkv"), b"fixture").await?;
    if with_local_nfo {
        tokio::fs::write(
            movie_dir.join("movie.nfo"),
            "<movie><title>本地标题</title><custom>keep</custom></movie>",
        )
        .await?;
    }
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
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    if with_local_nfo {
        MetadataEnricher::new(database.clone())
            .enrich_movie_library(library.id)
            .await?;
    }
    let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
        .fetch_one(database.pool())
        .await?;
    Ok(Fixture {
        _temp_dir: temp_dir,
        config,
        database,
        setup,
        item_id,
        movie_dir,
    })
}

async fn insert_candidate(
    database: &Database,
    item_id: &str,
    candidate: Value,
) -> Result<String, Box<dyn std::error::Error>> {
    let candidate_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO metadata_candidates
         (id, item_id, provider, provider_id, candidate_json, score, status)
         VALUES (?, ?, 'TMDB', '603', ?, 100, 'PENDING')",
    )
    .bind(&candidate_id)
    .bind(item_id)
    .bind(candidate.to_string())
    .execute(database.pool())
    .await?;
    Ok(candidate_id)
}

async fn start_image_stub()
-> Result<(String, tokio::task::JoinHandle<Result<(), std::io::Error>>), Box<dyn std::error::Error>>
{
    let app = Router::new().route(
        "/{name}",
        get(|path: axum::extract::Path<String>| async move {
            match path.0.as_str() {
                "poster" => Response::builder()
                    .header(CONTENT_TYPE, "image/png")
                    .body(Body::from(PNG_1X1.to_vec()))
                    .unwrap(),
                "fanart" => Response::builder()
                    .header(CONTENT_TYPE, "image/webp")
                    .body(Body::from(b"RIFF\x04\x00\x00\x00WEBP".to_vec()))
                    .unwrap(),
                "bad" => Response::builder()
                    .header(CONTENT_TYPE, "image/png")
                    .body(Body::from(b"broken".to_vec()))
                    .unwrap(),
                _ => Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::empty())
                    .unwrap(),
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    Ok((
        format!("http://{address}"),
        tokio::spawn(async move { axum::serve(listener, app).await }),
    ))
}

async fn start_lux(
    fixture: &Fixture,
) -> Result<(String, tokio::task::JoinHandle<Result<(), std::io::Error>>), Box<dyn std::error::Error>>
{
    let auth = WebAuthService::new(fixture.database.clone())?;
    let emby_auth = EmbyAuthService::new(fixture.database.clone())?;
    let app = app_with_state(AppState::ready(
        fixture.config.clone(),
        fixture.database.clone(),
        fixture.setup.clone(),
        auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    Ok((
        format!("http://{address}"),
        tokio::spawn(async move { axum::serve(listener, app).await }),
    ))
}

async fn login(
    client: &reqwest::Client,
    base_url: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let session = cookie_value(response.headers(), "lux_session");
    let csrf = cookie_value(response.headers(), "lux_csrf");
    Ok((format!("lux_session={session}; lux_csrf={csrf}"), csrf))
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
