use std::time::Duration;

use luxd::{
    api::{AppState, app_with_state},
    application::libraries::LibraryService,
    application::scanner::ScanJobService,
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

async fn start_server(
    config: Config,
) -> Result<
    (
        String,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
        Database,
    ),
    Box<dyn std::error::Error>,
> {
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
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
    Ok((format!("http://{address}"), server, database))
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

async fn login(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": username, "password": password }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let session = cookie_value(response.headers(), "lux_session");
    let csrf = cookie_value(response.headers(), "lux_csrf");
    Ok((format!("lux_session={session}; lux_csrf={csrf}"), csrf))
}

#[tokio::test]
async fn admin_can_create_list_and_add_library_root_with_csrf()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, _) = start_server(config).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;

    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(
            &json!({ "username": "Admin", "displayName": "Admin", "password": "correct password" }),
        )
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);
    let (cookies, csrf) = login(&client, &base_url, "admin", "correct password").await?;

    let unauthenticated = client
        .get(format!("{base_url}/api/v1/admin/libraries"))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let missing_csrf = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .json(&json!({ "name": "Movies", "kind": "MOVIE" }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let created = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "name": "  Movies ", "kind": "MOVIE" }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let created_body: Value = created.json().await?;
    assert_eq!(created_body["library"]["name"], "Movies");
    assert_eq!(created_body["library"]["kind"], "MOVIE");
    assert_eq!(
        created_body["library"]["realtimeMetadataAutoMatchEnabled"],
        true
    );
    let library_id = created_body["library"]["id"]
        .as_str()
        .ok_or("missing library ID")?;

    let media_dir = temp_dir.path().join("Movies");
    tokio::fs::create_dir(&media_dir).await?;
    let root = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/{library_id}/roots"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "path": media_dir }))
        .send()
        .await?;
    assert_eq!(root.status(), reqwest::StatusCode::CREATED);
    let root_body: Value = root.json().await?;
    assert_eq!(root_body["root"]["isAvailable"], true);
    assert_eq!(root_body["root"]["isWritable"], true);
    assert_eq!(root_body["warnings"], json!([]));
    assert!(root_body["scanJob"]["id"].is_string());
    let scan_job_id = root_body["scanJob"]["id"]
        .as_str()
        .ok_or("missing scan job ID")?
        .to_owned();
    let root_id = root_body["root"]["id"].as_str().ok_or("missing root ID")?;
    for _ in 0..80 {
        let jobs: Value = client
            .get(format!("{base_url}/api/v1/admin/jobs?page=1&pageSize=50"))
            .header(COOKIE, &cookies)
            .send()
            .await?
            .json()
            .await?;
        let running = jobs["jobs"].as_array().is_some_and(|jobs| {
            jobs.iter()
                .any(|job| job["status"] == "PENDING" || job["status"] == "RUNNING")
        });
        if !running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let completed_jobs: Value = client
        .get(format!("{base_url}/api/v1/admin/jobs?page=1&pageSize=50"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json()
        .await?;
    let completed_scan = completed_jobs["jobs"]
        .as_array()
        .and_then(|jobs| jobs.iter().find(|job| job["id"] == scan_job_id))
        .ok_or("missing completed scan job")?;
    assert!(completed_scan["startedAt"].is_number());
    assert!(completed_scan["finishedAt"].is_number());

    let edited = client
        .patch(format!("{base_url}/api/v1/admin/libraries/{library_id}"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "name": "Shows", "kind": "SERIES" }))
        .send()
        .await?;
    let edited_status = edited.status();
    let edited_body = edited.text().await?;
    assert_eq!(
        edited_status,
        reqwest::StatusCode::OK,
        "library update response: {edited_body}"
    );
    let edited_body: Value = serde_json::from_str(&edited_body)?;
    let edited_library = edited_body["library"].clone();
    assert_eq!(edited_library["name"], "Shows");
    assert_eq!(edited_library["kind"], "SERIES");

    let cover = client
        .put(format!(
            "{base_url}/api/v1/admin/libraries/{library_id}/cover"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .header("content-type", "image/png")
        .body(PNG_1X1)
        .send()
        .await?;
    assert_eq!(cover.status(), reqwest::StatusCode::OK);
    let cover_body = cover.json::<Value>().await?;
    let cover_url = cover_body["library"]["coverImageUrl"]
        .as_str()
        .ok_or("missing versioned cover URL")?;
    assert!(cover_url.contains("/cover?v="));
    let listed_libraries: Value = client
        .get(format!("{base_url}/api/v1/libraries"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json()
        .await?;
    let listed_cover_url = listed_libraries["libraries"]
        .as_array()
        .and_then(|libraries| libraries.iter().find(|library| library["id"] == library_id))
        .and_then(|library| library["coverImageUrl"].as_str())
        .ok_or("missing versioned cover URL in library list")?;
    assert_eq!(listed_cover_url, cover_url);

    let public_cover = client
        .get(format!("{base_url}/api/v1/libraries/{library_id}/cover"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(public_cover.status(), reqwest::StatusCode::OK);
    assert_eq!(
        public_cover.headers().get("content-type").unwrap(),
        "image/png"
    );
    assert_eq!(public_cover.bytes().await?.as_ref(), PNG_1X1);

    let invalid_cover = client
        .put(format!(
            "{base_url}/api/v1/admin/libraries/{library_id}/cover"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .header("content-type", "image/png")
        .body("not an image")
        .send()
        .await?;
    assert_eq!(invalid_cover.status(), reqwest::StatusCode::BAD_REQUEST);

    let deleted_root = client
        .delete(format!(
            "{base_url}/api/v1/admin/libraries/{library_id}/roots/{root_id}"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(deleted_root.status(), reqwest::StatusCode::NO_CONTENT);

    let listed = client
        .get(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let listed_body: Value = listed.json().await?;
    assert_eq!(listed_body["libraries"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        listed_body["libraries"][0]["roots"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    let disabled = client
        .patch(format!("{base_url}/api/v1/admin/libraries/{library_id}"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "isEnabled": false }))
        .send()
        .await?;
    assert_eq!(disabled.status(), reqwest::StatusCode::OK);
    assert_eq!(
        disabled.json::<Value>().await?["library"]["isEnabled"],
        false
    );

    let disabled_scan = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/{library_id}/scan"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(disabled_scan.status(), reqwest::StatusCode::NOT_FOUND);

    let disabled_reconcile = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/{library_id}/reconcile"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(disabled_reconcile.status(), reqwest::StatusCode::NOT_FOUND);

    let deleted_library = client
        .delete(format!("{base_url}/api/v1/admin/libraries/{library_id}"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(deleted_library.status(), reqwest::StatusCode::NO_CONTENT);
    let after_delete = client
        .get(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(after_delete.status(), reqwest::StatusCode::OK);
    assert_eq!(
        after_delete.json::<Value>().await?["libraries"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_library_pipeline_activity_is_authorized_bounded_and_sanitized()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, database) = start_server(config).await?;
    let client = reqwest::Client::new();
    let endpoint = format!("{base_url}/api/v1/admin/library-pipeline-activity");

    let unauthorized = client.get(&endpoint).send().await?;
    assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);
    let (cookies, _) = login(&client, &base_url, "admin", "correct password").await?;

    let libraries = LibraryService::new(database.clone());
    let first_library = libraries
        .create_library("A Pipeline", LibraryKind::Movie, false)
        .await?;
    let second_library = libraries
        .create_library("Library 02", LibraryKind::Movie, false)
        .await?;
    for index in 3..=51 {
        libraries
            .create_library(&format!("Library {index:02}"), LibraryKind::Movie, false)
            .await?;
    }
    let disabled = libraries
        .create_library("Z Disabled", LibraryKind::Movie, false)
        .await?;
    sqlx::query("UPDATE libraries SET is_enabled = 0 WHERE id = ?")
        .bind(disabled.id.to_string())
        .execute(database.pool())
        .await?;
    let removed = libraries
        .create_library("Y Removed", LibraryKind::Movie, false)
        .await?;
    libraries.delete_library(removed.id).await?;

    let completed_scan = ScanJobService::new(database.clone())
        .create_movie_scan_job(first_library.id)
        .await?;
    sqlx::query(
        "UPDATE scan_jobs
         SET status = 'COMPLETED', scan_phase = 'POSTPROCESSING', current_item = '媒体探测',
             processed_count = 1, total_count = 1, updated_at = 900,
             error = 'DO_NOT_LEAK_PIPELINE_ERROR'
         WHERE id = ?",
    )
    .bind(&completed_scan.id)
    .execute(database.pool())
    .await?;
    let cancelled_scan = ScanJobService::new(database.clone())
        .create_movie_scan_job(first_library.id)
        .await?;
    sqlx::query(
        "UPDATE scan_jobs
         SET status = 'CANCELLED', scan_phase = 'IDLE', updated_at = 1000
         WHERE id = ?",
    )
    .bind(&cancelled_scan.id)
    .execute(database.pool())
    .await?;

    sqlx::query(
        "INSERT INTO scan_job_targets (
             job_id, target_type, target_id, item_id, source_id, change_kind,
             probe_state, updated_at
         ) VALUES (?, 'SOURCE', 'pipeline-probe-failed', 'pipeline-secret-item',
                  'pipeline-probe-failed', 'NEW', 'FAILED', 910),
                  (?, 'SOURCE', 'pipeline-probe-cancelled', 'pipeline-secret-item',
                   'pipeline-probe-cancelled', 'NEW', 'PENDING', 1000)",
    )
    .bind(&completed_scan.id)
    .bind(&cancelled_scan.id)
    .execute(database.pool())
    .await?;

    let root_path = temp_dir.path().join("DO_NOT_LEAK_PIPELINE_ROOT");
    std::fs::create_dir(&root_path)?;
    let root = libraries
        .add_root(
            first_library.id,
            root_path.to_str().ok_or("media root is not UTF-8")?,
        )
        .await?
        .root;
    sqlx::query(
        "INSERT INTO scan_local_metadata_backfills (library_root_id, status, updated_at)
         VALUES (?, 'FAILED', 930)",
    )
    .bind(root.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_job_targets (
             job_id, target_type, target_id, item_id, change_kind, thumbnail_state, updated_at
         ) VALUES (?, 'ITEM', 'pipeline-thumbnail-done', 'pipeline-secret-item', 'NEW', 'DONE', 920)",
    )
    .bind(&completed_scan.id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('pipeline-secret-item', ?, 'MOVIE', 'DO_NOT_LEAK_PIPELINE_TITLE',
                  'pipeline-secret-title', 'LOCAL_CONFIRMED')",
    )
    .bind(first_library.id.to_string())
    .execute(database.pool())
    .await?;
    for (job_id, status, source) in [
        ("pipeline-auto-queued", "QUEUED", "AUTOMATIC"),
        ("pipeline-auto-failed", "FAILED", "AUTOMATIC"),
        ("pipeline-auto-cancelled", "CANCELLED", "AUTOMATIC"),
        ("pipeline-explicit", "COMPLETED", "EXPLICIT"),
    ] {
        sqlx::query(
            "INSERT INTO metadata_reidentify_jobs (
                 id, status, total_count, mode, library_id, dispatch_source
             ) VALUES (?, ?, 1, 'FILL_MISSING', ?, ?)",
        )
        .bind(job_id)
        .bind(status)
        .bind(first_library.id.to_string())
        .bind(source)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO metadata_reidentify_jobs (
             id, status, total_count, mode, library_id, dispatch_source
         ) VALUES ('pipeline-second-library-running', 'RUNNING', 1, 'FILL_MISSING', ?, 'AUTOMATIC')",
    )
    .bind(second_library.id.to_string())
    .execute(database.pool())
    .await?;

    let first_page = client
        .get(&endpoint)
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(first_page.status(), reqwest::StatusCode::OK);
    let first_page: Value = first_page.json().await?;
    assert_eq!(first_page["page"], 1);
    assert_eq!(first_page["pageSize"], 20);
    assert_eq!(first_page["totalLibraries"], 51);
    assert_eq!(first_page["libraries"].as_array().map(Vec::len), Some(20));
    let first = &first_page["libraries"][0];
    assert_eq!(first["libraryId"], first_library.id.to_string());
    assert_eq!(first["libraryName"], "A Pipeline");
    let stages = first["stages"].as_array().ok_or("missing stages")?;
    assert_eq!(stages.len(), 5);
    assert_eq!(stages[0]["stage"], "INDEX");
    assert_eq!(stages[0]["planned"], true);
    assert_eq!(stages[0]["statusCounts"]["completed"], 1);
    assert_eq!(stages[0]["statusCounts"]["cancelled"], 1);
    assert_eq!(stages[0]["recentActivityAt"], 1000);
    assert_eq!(stages[1]["stage"], "LOCAL_METADATA");
    assert_eq!(stages[1]["statusCounts"]["failed"], 1);
    assert_eq!(stages[1]["recentActivityAt"], 930);
    assert_eq!(stages[2]["stage"], "PROBE");
    assert_eq!(stages[2]["statusCounts"]["running"], 1);
    assert_eq!(stages[2]["statusCounts"]["failed"], 1);
    assert_eq!(stages[2]["statusCounts"]["cancelled"], 1);
    assert_eq!(stages[2]["recentActivityAt"], 1000);
    assert_eq!(stages[3]["stage"], "THUMBNAIL");
    assert_eq!(stages[3]["statusCounts"]["completed"], 1);
    assert_eq!(stages[3]["recentActivityAt"], 920);
    assert_eq!(stages[4]["stage"], "AUTOMATIC_METADATA");
    assert_eq!(stages[4]["statusCounts"]["queued"], 1);
    assert_eq!(stages[4]["statusCounts"]["failed"], 1);
    assert_eq!(stages[4]["statusCounts"]["cancelled"], 1);
    assert_eq!(
        first_page["libraries"][1]["libraryId"],
        second_library.id.to_string()
    );
    assert_eq!(
        first_page["libraries"][1]["stages"][4]["statusCounts"]["running"],
        1
    );
    let serialized = first_page.to_string();
    for private_value in [
        "DO_NOT_LEAK_PIPELINE_TITLE",
        "DO_NOT_LEAK_PIPELINE_ROOT",
        "DO_NOT_LEAK_PIPELINE_ERROR",
        "媒体探测",
    ] {
        assert!(!serialized.contains(private_value));
    }

    let second_page = client
        .get(format!("{endpoint}?page=2&pageSize=50"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(second_page.status(), reqwest::StatusCode::OK);
    let second_page: Value = second_page.json().await?;
    assert_eq!(second_page["page"], 2);
    assert_eq!(second_page["pageSize"], 50);
    assert_eq!(second_page["totalLibraries"], 51);
    assert_eq!(second_page["libraries"].as_array().map(Vec::len), Some(1));
    assert_eq!(second_page["libraries"][0]["stages"][0]["planned"], false);
    assert_eq!(
        second_page["libraries"][0]["stages"][0]["recentActivityAt"],
        Value::Null
    );
    let empty_page = client
        .get(format!("{endpoint}?page=3&pageSize=50"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(empty_page.status(), reqwest::StatusCode::OK);
    assert_eq!(empty_page.json::<Value>().await?["libraries"], json!([]));

    for query in [
        "page=0",
        "pageSize=51",
        "page=9223372036854775807&pageSize=50",
    ] {
        let invalid = client
            .get(format!("{endpoint}?{query}"))
            .header(COOKIE, &cookies)
            .send()
            .await?;
        assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_delete_disabled_library_with_active_scan_and_probe_jobs()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, database) = start_server(config).await?;
    let client = reqwest::Client::new();

    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(
            &json!({ "username": "Admin", "displayName": "Admin", "password": "correct password" }),
        )
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);
    let (cookies, csrf) = login(&client, &base_url, "admin", "correct password").await?;

    let created = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "name": "Busy Movies", "kind": "MOVIE" }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let library_id = created.json::<Value>().await?["library"]["id"]
        .as_str()
        .ok_or("missing library ID")?
        .to_owned();

    let disabled = client
        .patch(format!("{base_url}/api/v1/admin/libraries/{library_id}"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "isEnabled": false }))
        .send()
        .await?;
    assert_eq!(disabled.status(), reqwest::StatusCode::OK);

    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES (?, ?, 'RECONCILE_LIBRARY', 'RUNNING', ?)",
    )
    .bind("active-scan-job")
    .bind(&library_id)
    .bind("scan-generation")
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO strm_probe_jobs
            (id, operation_id, library_id, status, concurrency, target_scan_job_id)
         VALUES (?, ?, ?, 'RUNNING', 1, ?)",
    )
    .bind("active-probe-job")
    .bind("probe-operation")
    .bind(&library_id)
    .bind("active-scan-job")
    .execute(database.pool())
    .await?;

    let deleted = client
        .delete(format!("{base_url}/api/v1/admin/libraries/{library_id}"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);

    let remaining_libraries: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM libraries WHERE id = ?")
            .bind(&library_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_libraries, 0);
    let remaining_scan_jobs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_jobs WHERE id = 'active-scan-job'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_scan_jobs, 0);
    let remaining_probe_jobs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM strm_probe_jobs WHERE id = 'active-probe-job'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_probe_jobs, 0);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn deleting_library_removes_it_from_strm_media_info_plugin_config()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let (base_url, server, _) = start_server(config).await?;
    let client = reqwest::Client::new();

    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(
            &json!({ "username": "Admin", "displayName": "Admin", "password": "correct password" }),
        )
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);
    let (cookies, csrf) = login(&client, &base_url, "admin", "correct password").await?;

    let created = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "name": "STRM", "kind": "MIXED" }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let library_id = created.json::<Value>().await?["library"]["id"]
        .as_str()
        .ok_or("missing library ID")?
        .to_owned();

    let remaining = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "name": "STRM 2", "kind": "MIXED" }))
        .send()
        .await?;
    assert_eq!(remaining.status(), reqwest::StatusCode::CREATED);
    let remaining_library_id = remaining.json::<Value>().await?["library"]["id"]
        .as_str()
        .ok_or("missing remaining library ID")?
        .to_owned();

    let plugin_config_dir = config_dir.join("plugin-config");
    tokio::fs::create_dir_all(&plugin_config_dir).await?;
    tokio::fs::write(
        plugin_config_dir.join("org.lux.strm-media-info.json"),
        serde_json::to_vec(&json!({
            "libraryIds": [library_id, remaining_library_id],
            "concurrency": 1,
            "mediaInfoEnabled": true,
            "thumbnailEnabled": false,
            "thumbnailPositionPercent": 30,
            "existingInfoPolicy": "SKIP",
            "writeSidecars": false,
            "schedule": "0 3 * * *"
        }))?,
    )
    .await?;

    let deleted = client
        .delete(format!("{base_url}/api/v1/admin/libraries/{library_id}"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);

    let config_values: Value = serde_json::from_slice(
        &tokio::fs::read(plugin_config_dir.join("org.lux.strm-media-info.json")).await?,
    )?;
    assert_eq!(config_values["libraryIds"], json!([remaining_library_id]));

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_browse_server_directories_with_bounded_pagination()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let browse_root = temp_dir.path().join("browse-root");
    tokio::fs::create_dir_all(browse_root.join("Alpha")).await?;
    tokio::fs::create_dir_all(browse_root.join("Beta")).await?;
    tokio::fs::write(browse_root.join("not-a-directory.mkv"), b"media").await?;

    let (base_url, server, _) = start_server(config).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(
            &json!({ "username": "Admin", "displayName": "Admin", "password": "correct password" }),
        )
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);
    let (cookies, _) = login(&client, &base_url, "admin", "correct password").await?;
    let browse_path = browse_root.to_string_lossy().into_owned();

    let unauthenticated = client
        .get(format!("{base_url}/api/v1/admin/directories"))
        .query(&[("path", browse_path.as_str())])
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let first_page = client
        .get(format!("{base_url}/api/v1/admin/directories"))
        .header(COOKIE, &cookies)
        .query(&[
            ("path", browse_path.as_str()),
            ("page", "1"),
            ("pageSize", "1"),
        ])
        .send()
        .await?;
    assert_eq!(first_page.status(), reqwest::StatusCode::OK);
    let first_body = first_page.json::<Value>().await?;
    assert_eq!(
        first_body["path"],
        std::fs::canonicalize(&browse_root)?
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(first_body["directories"].as_array().map(Vec::len), Some(1));
    assert_eq!(first_body["page"], 1);
    assert_eq!(first_body["pageSize"], 1);
    assert_eq!(first_body["hasMore"], true);
    assert!(first_body["parentPath"].is_string());

    let second_page = client
        .get(format!("{base_url}/api/v1/admin/directories"))
        .header(COOKIE, &cookies)
        .query(&[
            ("path", browse_path.as_str()),
            ("page", "2"),
            ("pageSize", "1"),
        ])
        .send()
        .await?;
    assert_eq!(second_page.status(), reqwest::StatusCode::OK);
    let second_body = second_page.json::<Value>().await?;
    assert_eq!(second_body["directories"].as_array().map(Vec::len), Some(1));
    assert_eq!(second_body["hasMore"], false);
    assert_ne!(
        first_body["directories"][0]["path"],
        second_body["directories"][0]["path"]
    );
    assert!(
        first_body["directories"][0]["name"] == "Alpha"
            || first_body["directories"][0]["name"] == "Beta"
    );
    assert!(
        second_body["directories"][0]["name"] == "Alpha"
            || second_body["directories"][0]["name"] == "Beta"
    );

    let relative_path = client
        .get(format!("{base_url}/api/v1/admin/directories"))
        .header(COOKIE, &cookies)
        .query(&[("path", "relative/path")])
        .send()
        .await?;
    assert_eq!(relative_path.status(), reqwest::StatusCode::BAD_REQUEST);

    let file_path = browse_root.join("not-a-directory.mkv");
    let file_path = file_path.to_string_lossy().into_owned();
    let file_request = client
        .get(format!("{base_url}/api/v1/admin/directories"))
        .header(COOKIE, &cookies)
        .query(&[("path", file_path.as_str())])
        .send()
        .await?;
    assert_eq!(file_request.status(), reqwest::StatusCode::BAD_REQUEST);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn library_cover_survives_server_restart() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, database) = start_server(config.clone()).await?;
    let client = reqwest::Client::new();

    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);
    let (cookies, csrf) = login(&client, &base_url, "admin", "correct password").await?;
    let created = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "name": "Movies", "kind": "MOVIE" }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let library_id = created.json::<Value>().await?["library"]["id"]
        .as_str()
        .ok_or("missing library ID")?
        .to_owned();

    let uploaded = client
        .put(format!(
            "{base_url}/api/v1/admin/libraries/{library_id}/cover"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .header("content-type", "image/png")
        .body(PNG_1X1)
        .send()
        .await?;
    assert_eq!(uploaded.status(), reqwest::StatusCode::OK);

    server.abort();
    let _ = server.await;
    database.close().await;

    let (base_url, server, database) = start_server(config).await?;
    let (cookies, _) = login(&client, &base_url, "admin", "correct password").await?;
    let cover = client
        .get(format!("{base_url}/api/v1/libraries/{library_id}/cover"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(cover.status(), reqwest::StatusCode::OK);
    assert_eq!(cover.headers()["content-type"], "image/png");
    assert_eq!(cover.bytes().await?.as_ref(), PNG_1X1);

    server.abort();
    let _ = server.await;
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn admin_can_run_registered_auto_library_cover_task_manually()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, database) = start_server(config).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);
    let (cookies, csrf) = login(&client, &base_url, "admin", "correct password").await?;

    let unregistered_missing_library = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/00000000-0000-0000-0000-000000000000/cover/auto"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(
        unregistered_missing_library.status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert_eq!(
        unregistered_missing_library.json::<Value>().await?["error"]["message"],
        "自动媒体库封面任务尚未注册"
    );

    let created = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "name": "Movies", "kind": "MOVIE" }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let library_id = created.json::<Value>().await?["library"]["id"]
        .as_str()
        .ok_or("missing library ID")?
        .to_owned();
    sqlx::query(
        "INSERT INTO scheduled_task_configs (
            owner_type, owner_id, task_type, task_name, task_description,
            source_type, is_enabled, resource_limit_json
         ) VALUES ('LIBRARY', ?, 'AUTO_LIBRARY_COVER', '自动生成媒体库封面',
                   '手动测试任务', 'SYSTEM', 0, '{\"oneShot\":true}')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;

    let response = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/{library_id}/cover/auto"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    let body: Value = response.json().await?;
    assert_eq!(body["status"], "QUEUED");
    assert_eq!(body["taskType"], "AUTO_LIBRARY_COVER");
    server.abort();
    Ok(())
}

#[tokio::test]
async fn non_admin_cannot_manage_libraries() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, database) = start_server(config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let users = UserStore::new(database)?;
    users
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = login(&client, &base_url, "viewer", "viewer password").await?;

    let response = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, cookies)
        .header("x-csrf-token", csrf)
        .json(&json!({ "name": "Movies", "kind": "MOVIE" }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_update_independent_library_schedules_without_restart()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, database) = start_server(config).await?;
    let client = reqwest::Client::new();

    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);
    let (cookies, csrf) = login(&client, &base_url, "admin", "correct password").await?;

    let mut library_ids = Vec::new();
    for name in ["Movies", "Series"] {
        let response = client
            .post(format!("{base_url}/api/v1/admin/libraries"))
            .header(COOKIE, &cookies)
            .header("x-csrf-token", &csrf)
            .json(&json!({ "name": name, "kind": "MIXED" }))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::CREATED);
        let body: Value = response.json().await?;
        library_ids.push(
            body["library"]["id"]
                .as_str()
                .ok_or("missing library ID")?
                .to_owned(),
        );
    }

    let first_update = client
        .patch(format!(
            "{base_url}/api/v1/admin/libraries/{}",
            library_ids[0]
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "realtimeWatchEnabled": false,
            "realtimeMetadataAutoMatchEnabled": true,
            "incrementalSchedule": "interval:30s",
            "reconciliationSchedule": "0 3 * * *",
            "metadataSchedule": "*/5 * * * *",
            "scanConcurrency": 4,
            "probeConcurrency": 3
        }))
        .send()
        .await?;
    assert_eq!(first_update.status(), reqwest::StatusCode::OK);
    let first_body: Value = first_update.json().await?;
    assert_eq!(first_body["library"]["incrementalSchedule"], Value::Null);
    assert_eq!(first_body["library"]["realtimeWatchEnabled"], false);
    assert_eq!(first_body["library"]["scanConcurrency"], 4);
    assert_eq!(
        first_body["library"]["realtimeMetadataAutoMatchEnabled"],
        true
    );

    let second_update = client
        .patch(format!(
            "{base_url}/api/v1/admin/libraries/{}",
            library_ids[1]
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "scanConcurrency": 7
        }))
        .send()
        .await?;
    assert_eq!(second_update.status(), reqwest::StatusCode::OK);

    let second_body: Value = second_update.json().await?;
    assert_eq!(
        second_body["library"]["realtimeMetadataAutoMatchEnabled"],
        true
    );

    let library_strategy = json!({
        "metadataLanguage": "ja-JP",
        "imageLanguage": "ja",
        "region": "JP",
        "scraperId": null,
        "applyScope": "NEW_CONTENT",
        "images": {
            "poster": true,
            "artwork": true,
            "banner": false,
            "logo": true,
            "thumbnail": false,
            "thumbnailScrapingMode": "SCREENSHOT_FIRST",
            "disc": true,
            "wallpaper": false,
            "writeToMetadata": true,
            "maxBackdropCount": 3,
            "minDownloadWidth": 1920
        },
        "subtitles": {
            "autoDownload": true,
            "languages": ["ja", "zh-CN"],
            "forcedOnly": false,
            "hearingImpaired": false
        }
    });
    let strategy_update = client
        .patch(format!(
            "{base_url}/api/v1/admin/libraries/{}",
            library_ids[0]
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "mediaStrategy": library_strategy }))
        .send()
        .await?;
    assert_eq!(strategy_update.status(), reqwest::StatusCode::OK);
    let strategy_body: Value = strategy_update.json().await?;
    assert_eq!(strategy_body["library"]["mediaStrategy"]["region"], "JP");
    assert_eq!(
        strategy_body["library"]["mediaStrategy"]["images"]["maxBackdropCount"],
        3
    );
    assert_eq!(
        strategy_body["library"]["mediaStrategy"]["images"]["disc"],
        true
    );
    assert_eq!(
        strategy_body["library"]["mediaStrategy"]["images"]["writeToMetadata"],
        true
    );
    assert_eq!(
        strategy_body["library"]["mediaStrategy"]["images"]["thumbnailScrapingMode"],
        "SCREENSHOT_FIRST"
    );

    let listed = client
        .get(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let listed_body: Value = listed.json().await?;
    let libraries = listed_body["libraries"]
        .as_array()
        .ok_or("missing libraries")?;
    let first = libraries
        .iter()
        .find(|library| library["id"] == library_ids[0])
        .ok_or("missing first library")?;
    let second = libraries
        .iter()
        .find(|library| library["id"] == library_ids[1])
        .ok_or("missing second library")?;
    assert_eq!(first["reconciliationSchedule"], "0 3 * * *");
    assert_eq!(first["metadataSchedule"], "*/5 * * * *");
    assert_eq!(first["scanConcurrency"], 4);
    assert_eq!(first["probeConcurrency"], 3);
    assert_eq!(second["reconciliationSchedule"], "0 3 * * 0");
    assert_eq!(second["scanConcurrency"], 7);
    assert_eq!(first["mediaStrategy"]["imageLanguage"], "ja");

    let inherited = client
        .patch(format!(
            "{base_url}/api/v1/admin/libraries/{}",
            library_ids[0]
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "mediaStrategy": null }))
        .send()
        .await?;
    assert_eq!(inherited.status(), reqwest::StatusCode::OK);
    assert_eq!(
        inherited.json::<Value>().await?["library"]["mediaStrategy"],
        Value::Null
    );

    let scheduled_tasks: Vec<(String, String, Option<String>, i64, String)> = sqlx::query_as(
        "SELECT owner_id, task_type, cron_or_interval, is_enabled, resource_limit_json
         FROM scheduled_task_configs ORDER BY owner_id, task_type",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(scheduled_tasks.len(), 4);
    assert!(
        scheduled_tasks
            .iter()
            .any(|(owner, task, schedule, enabled, _)| {
                owner == &library_ids[0]
                    && task == "METADATA_PARSE"
                    && schedule.as_deref() == Some("*/5 * * * *")
                    && *enabled == 1
            })
    );
    assert!(
        scheduled_tasks
            .iter()
            .any(|(owner, task, schedule, enabled, _)| {
                owner == &library_ids[0]
                    && task == "RECONCILIATION_SCAN"
                    && schedule.as_deref() == Some("0 3 * * *")
                    && *enabled == 1
            })
    );
    assert!(scheduled_tasks
        .iter()
        .any(|(owner, task, _, _, _)| owner == &library_ids[1] && task == "RECONCILIATION_SCAN"));
    assert!(
        !scheduled_tasks
            .iter()
            .any(|(_, task, _, _, _)| task == "INCREMENTAL_SCAN")
    );

    let cleared = client
        .patch(format!(
            "{base_url}/api/v1/admin/libraries/{}",
            library_ids[0]
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "metadataSchedule": null }))
        .send()
        .await?;
    assert_eq!(cleared.status(), reqwest::StatusCode::OK);
    let cleared_body: Value = cleared.json().await?;
    assert_eq!(cleared_body["library"]["metadataSchedule"], Value::Null);

    let invalid = client
        .patch(format!(
            "{base_url}/api/v1/admin/libraries/{}",
            library_ids[0]
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "scanConcurrency": 0 }))
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);

    let deleted = client
        .delete(format!(
            "{base_url}/api/v1/admin/libraries/{}",
            library_ids[0]
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);
    let orphaned_tasks: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scheduled_task_configs WHERE owner_type = 'LIBRARY' AND owner_id = ?",
    )
    .bind(&library_ids[0])
    .fetch_one(database.pool())
    .await?;
    assert_eq!(orphaned_tasks, 0);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_task_activity_includes_scan_postprocessing() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, database) = start_server(config).await?;
    let client = reqwest::Client::new();

    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);
    let (cookies, _csrf) = login(&client, &base_url, "admin", "correct password").await?;

    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let job_id = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await?
        .id;

    sqlx::query(
        "UPDATE scan_jobs
         SET status = 'COMPLETED', scan_phase = 'POSTPROCESSING', current_item = '媒体探测'
         WHERE id = ?",
    )
    .bind(&job_id)
    .execute(database.pool())
    .await?;

    let activity = client
        .get(format!("{base_url}/api/v1/admin/task-activity"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(activity.status(), reqwest::StatusCode::OK);
    let activities = activity.json::<Value>().await?["activities"]
        .as_array()
        .cloned()
        .ok_or("missing activities")?;
    let scan = activities
        .iter()
        .find(|activity| activity["id"] == job_id)
        .ok_or("postprocessing scan missing from activity")?;
    assert_eq!(scan["status"], "COMPLETED");
    assert_eq!(scan["scanPhase"], "POSTPROCESSING");
    assert_eq!(scan["discoveryCompleted"], false);
    assert_eq!(scan["currentItem"], "媒体探测");

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_task_activity_includes_current_media_title_for_chapter_jobs()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, database) = start_server(config).await?;
    let client = reqwest::Client::new();

    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);
    let (cookies, _csrf) = login(&client, &base_url, "admin", "correct password").await?;

    let library = LibraryService::new(database.clone())
        .create_library("Series", LibraryKind::Series, false)
        .await?;
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status, identity_key
         ) VALUES ('activity-series', ?, 'SERIES', '动漫', '动漫', 'LOCAL_CONFIRMED', 'activity:series')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, parent_id, series_id, season_number,
             title, sort_title, identification_status, identity_key
         ) VALUES (
             'activity-season', ?, 'SEASON', 'activity-series', 'activity-series', 1,
             'Season 01', 'season 01', 'LOCAL_CONFIRMED', 'activity:season'
         )",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, parent_id, series_id, season_number, episode_number,
             title, sort_title, identification_status, identity_key
         ) VALUES (
             'activity-episode', ?, 'EPISODE', 'activity-season', 'activity-series', 1, 8,
             '第八集', '第八集', 'LOCAL_CONFIRMED', 'activity:episode'
         )",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_sources (id, item_id, source_kind)
         VALUES ('activity-source', 'activity-episode', 'LOCAL_FILE')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO chapter_detection_jobs (
             id, library_id, plugin_id, status, concurrency,
             intro_window_seconds, credits_window_seconds, match_threshold, total_count
         ) VALUES ('activity-chapter-job', ?, 'activity.plugin', 'RUNNING', 1, 30, 60, 0.8, 1)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO chapter_detection_job_items (
             job_id, source_id, item_id, season_id, source_fingerprint, status
         ) VALUES ('activity-chapter-job', 'activity-source', 'activity-episode', 'activity-season', ?, 'RUNNING')",
    )
    .bind(Vec::<u8>::new())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO danmaku_match_jobs (
             id, library_id, status, concurrency, total_count
         ) VALUES ('activity-danmaku-job', ?, 'RUNNING', 1, 1)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO danmaku_match_job_items (id, job_id, media_source_id, status)
         VALUES ('activity-danmaku-item', 'activity-danmaku-job', 'activity-source', 'RUNNING')",
    )
    .execute(database.pool())
    .await?;

    let activity = client
        .get(format!("{base_url}/api/v1/admin/task-activity"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(activity.status(), reqwest::StatusCode::OK);
    let activities = activity.json::<Value>().await?["activities"]
        .as_array()
        .cloned()
        .ok_or("missing activities")?;
    let chapter = activities
        .iter()
        .find(|activity| activity["id"] == "activity-chapter-job")
        .ok_or("chapter activity missing")?;
    assert_eq!(chapter["currentItem"], "动漫 · S01E08 · 第八集");
    let danmaku = activities
        .iter()
        .find(|activity| activity["id"] == "activity-danmaku-job")
        .ok_or("danmaku activity missing")?;
    assert_eq!(danmaku["currentItem"], "动漫 · S01E08 · 第八集");

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_list_and_update_library_schedules_from_operations_page()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, database) = start_server(config).await?;
    let client = reqwest::Client::new();

    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);
    let (cookies, csrf) = login(&client, &base_url, "admin", "correct password").await?;

    let initial_tasks = client
        .get(format!(
            "{base_url}/api/v1/admin/scheduled-tasks?page=1&pageSize=10"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(initial_tasks.status(), reqwest::StatusCode::OK);
    assert_eq!(initial_tasks.json::<Value>().await?["total"], 0);

    let created = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "name": "Movies", "kind": "MOVIE" }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let library_id = created.json::<Value>().await?["library"]["id"]
        .as_str()
        .ok_or("missing library ID")?
        .to_owned();

    let registered = client
        .get(format!(
            "{base_url}/api/v1/admin/scheduled-tasks?page=1&pageSize=10"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(registered.status(), reqwest::StatusCode::OK);
    let registered_body: Value = registered.json().await?;
    assert_eq!(registered_body["total"], 2);
    assert_eq!(
        registered_body["scheduledTasks"]
            .as_array()
            .and_then(|tasks| tasks
                .iter()
                .find(|task| task["taskType"] == "RECONCILIATION_SCAN"))
            .and_then(|task| task["name"].as_str()),
        Some("全量校验媒体库")
    );
    assert_eq!(
        registered_body["scheduledTasks"]
            .as_array()
            .and_then(|tasks| tasks
                .iter()
                .find(|task| task["taskType"] == "METADATA_PARSE"))
            .and_then(|task| task["sourceType"].as_str()),
        Some("SYSTEM")
    );

    let seeded = client
        .patch(format!("{base_url}/api/v1/admin/libraries/{library_id}"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "reconciliationSchedule": "0 */6 * * *" }))
        .send()
        .await?;
    assert_eq!(seeded.status(), reqwest::StatusCode::OK);

    let listed = client
        .get(format!(
            "{base_url}/api/v1/admin/scheduled-tasks?page=1&pageSize=10"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let listed_body: Value = listed.json().await?;
    assert_eq!(listed_body["total"], 2);
    let tasks = listed_body["scheduledTasks"]
        .as_array()
        .ok_or("missing scheduled tasks")?;
    let reconciliation = tasks
        .iter()
        .find(|task| task["taskType"] == "RECONCILIATION_SCAN")
        .ok_or("missing reconciliation schedule")?;
    assert_eq!(reconciliation["ownerType"], "LIBRARY");
    assert_eq!(reconciliation["ownerName"], "Movies");
    assert_eq!(reconciliation["schedule"], "0 */6 * * *");
    assert_eq!(reconciliation["isEnabled"], true);

    let missing_csrf = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookies)
        .json(&json!({
            "ownerType": "LIBRARY",
            "ownerId": library_id,
            "taskType": "METADATA_PARSE",
            "schedule": "0 */2 * * *"
        }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let updated = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "LIBRARY",
            "ownerId": library_id,
            "taskType": "METADATA_PARSE",
            "schedule": "0 */2 * * *"
        }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    let updated_body: Value = updated.json().await?;
    assert_eq!(updated_body["scheduledTask"]["schedule"], "0 */2 * * *");
    assert_eq!(updated_body["scheduledTask"]["isEnabled"], true);

    let global_schedule = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "GLOBAL",
            "ownerId": "global",
            "taskType": "RECONCILIATION_SCAN",
            "schedule": "0 */6 * * *"
        }))
        .send()
        .await?;
    assert_eq!(global_schedule.status(), reqwest::StatusCode::NOT_FOUND);

    let registered_reconciliation_task = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "LIBRARY",
            "ownerId": library_id,
            "taskType": "RECONCILIATION_SCAN",
            "schedule": "0 */6 * * *"
        }))
        .send()
        .await?;
    assert_eq!(
        registered_reconciliation_task.status(),
        reqwest::StatusCode::OK
    );
    let updated_reconciliation = registered_reconciliation_task.json::<Value>().await?;
    assert_eq!(
        updated_reconciliation["scheduledTask"]["schedule"],
        "0 */6 * * *"
    );
    let incremental_task = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "LIBRARY",
            "ownerId": library_id,
            "taskType": "INCREMENTAL_SCAN",
            "schedule": "0 0 * * *"
        }))
        .send()
        .await?;
    assert_eq!(incremental_task.status(), reqwest::StatusCode::BAD_REQUEST);
    let unchanged_library = client
        .get(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json::<Value>()
        .await?;
    let unchanged_library = unchanged_library["libraries"]
        .as_array()
        .and_then(|libraries| libraries.iter().find(|library| library["id"] == library_id))
        .ok_or("missing library after scheduled task update")?;
    assert_eq!(unchanged_library["reconciliationSchedule"], "0 */6 * * *");

    let invalid_task = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "LIBRARY",
            "ownerId": library_id,
            "taskType": "REBUILD_SEARCH",
            "schedule": "0 */2 * * *"
        }))
        .send()
        .await?;
    assert_eq!(invalid_task.status(), reqwest::StatusCode::BAD_REQUEST);

    let cleared = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "LIBRARY",
            "ownerId": library_id,
            "taskType": "METADATA_PARSE",
            "schedule": null,
            "isEnabled": false
        }))
        .send()
        .await?;
    assert_eq!(cleared.status(), reqwest::StatusCode::OK);
    let cleared_body: Value = cleared.json().await?;
    assert_eq!(cleared_body["scheduledTask"]["schedule"], Value::Null);
    assert_eq!(cleared_body["scheduledTask"]["isEnabled"], false);

    let libraries = client
        .get(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    let libraries_body: Value = libraries.json().await?;
    let library = libraries_body["libraries"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == library_id))
        .ok_or("missing library after schedule update")?;
    assert_eq!(library["metadataSchedule"], Value::Null);

    let manual_run = client
        .post(format!("{base_url}/api/v1/admin/scheduled-tasks/run"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "LIBRARY",
            "ownerId": library_id,
            "taskType": "RECONCILIATION_SCAN"
        }))
        .send()
        .await?;
    assert_eq!(manual_run.status(), reqwest::StatusCode::ACCEPTED);
    let manual_run_body = manual_run.json::<Value>().await?;
    assert_eq!(manual_run_body["taskType"], "RECONCILIATION_SCAN");
    let scan_job_id = manual_run_body["run"]["jobId"]
        .as_str()
        .ok_or("missing scheduled scan job ID")?;
    let mut scan_status = (String::new(), String::new());
    for _ in 0..200 {
        scan_status = sqlx::query_as("SELECT status, scan_phase FROM scan_jobs WHERE id = ?")
            .bind(scan_job_id)
            .fetch_one(database.pool())
            .await?;
        if scan_status.0 == "COMPLETED" && scan_status.1 == "IDLE" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(scan_status, ("COMPLETED".to_owned(), "IDLE".to_owned()));

    let activity = client
        .get(format!("{base_url}/api/v1/admin/task-activity"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(activity.status(), reqwest::StatusCode::OK);
    assert!(activity.json::<Value>().await?["activities"].is_array());

    sqlx::query(
        "INSERT INTO scheduled_task_configs (
            owner_type, owner_id, task_type, task_name, task_description,
            source_type, plugin_id, cron_or_interval, is_enabled, resource_limit_json
         ) VALUES ('LIBRARY', ?, 'AUTO_LIBRARY_COVER', '自动生成媒体库封面',
            '生成媒体库自动封面。', 'SYSTEM', NULL, NULL, 0, '{}')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    let cover_schedule = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "LIBRARY",
            "ownerId": library_id,
            "taskType": "AUTO_LIBRARY_COVER",
            "schedule": "0 1 * * *"
        }))
        .send()
        .await?;
    assert_eq!(cover_schedule.status(), reqwest::StatusCode::OK);
    assert_eq!(
        cover_schedule.json::<Value>().await?["scheduledTask"]["schedule"],
        "0 1 * * *"
    );
    let cover_plan: (Option<String>, i64, i64) = sqlx::query_as(
        "SELECT p.cron_or_interval, p.is_enabled,
                       (SELECT COUNT(*) FROM scheduled_task_plan_libraries l
                        WHERE l.plan_id = p.id)
         FROM scheduled_task_plans p
         JOIN scheduled_task_configs c ON c.plan_id = p.id
         WHERE c.owner_type = 'LIBRARY' AND c.owner_id = ?
           AND c.task_type = 'AUTO_LIBRARY_COVER'",
    )
    .bind(&library_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(cover_plan.0.as_deref(), Some("0 1 * * *"));
    assert_eq!(cover_plan.1, 1);
    assert_eq!(cover_plan.2, 1);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_group_library_schedules_into_plans() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, database) = start_server(config).await?;
    let client = reqwest::Client::new();
    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);
    let (cookies, csrf) = login(&client, &base_url, "admin", "correct password").await?;
    let mut library_ids = Vec::new();
    for name in ["Movies", "Series"] {
        let response = client
            .post(format!("{base_url}/api/v1/admin/libraries"))
            .header(COOKIE, &cookies)
            .header("x-csrf-token", &csrf)
            .json(&json!({ "name": name, "kind": "MOVIE" }))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::CREATED);
        library_ids.push(
            response.json::<Value>().await?["library"]["id"]
                .as_str()
                .ok_or("missing library id")?
                .to_owned(),
        );
    }
    let created = client
        .post(format!("{base_url}/api/v1/admin/scheduled-task-plans"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "taskType": "RECONCILIATION_SCAN",
            "name": "电影与剧集夜间校验",
            "schedule": "0 2 * * 1",
            "isEnabled": true,
            "libraryIds": library_ids
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let created_body: Value = created.json().await?;
    assert_eq!(created_body["plan"]["libraryCount"], 2);
    assert_eq!(created_body["plan"]["schedule"], "0 2 * * 1");

    let listed = client
        .get(format!(
            "{base_url}/api/v1/admin/scheduled-task-plans?page=1&pageSize=50&taskType=RECONCILIATION_SCAN"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let listed_body: Value = listed.json().await?;
    assert_eq!(listed_body["total"], 2);
    assert!(listed_body["plans"].as_array().is_some_and(|plans| {
        plans
            .iter()
            .any(|plan| plan["name"] == "电影与剧集夜间校验" && plan["libraryCount"] == 2)
    }));

    let searched = client
        .get(format!(
            "{base_url}/api/v1/admin/scheduled-task-plans?page=1&pageSize=50&search=Series"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(searched.status(), reqwest::StatusCode::OK);
    let searched_body: Value = searched.json().await?;
    assert_eq!(
        searched_body["total"], 2,
        "unexpected plan search response: {searched_body}"
    );
    assert!(searched_body["plans"].as_array().is_some_and(|plans| {
        plans
            .iter()
            .any(|plan| plan["name"] == "电影与剧集夜间校验")
            && plans.iter().any(|plan| plan["name"] == "元数据刮削")
    }));

    let config_plan_ids: Vec<String> = sqlx::query_scalar(
        "SELECT plan_id FROM scheduled_task_configs
         WHERE task_type = 'RECONCILIATION_SCAN' ORDER BY owner_id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(config_plan_ids.len(), 2);
    assert_eq!(config_plan_ids[0], config_plan_ids[1]);

    let original_plan_id = created_body["plan"]["id"]
        .as_str()
        .ok_or("missing created plan id")?
        .to_owned();
    let moved = client
        .post(format!("{base_url}/api/v1/admin/scheduled-task-plans"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "taskType": "RECONCILIATION_SCAN",
            "name": "临时校验",
            "schedule": "0 5 * * 1",
            "isEnabled": true,
            "libraryIds": library_ids
        }))
        .send()
        .await?;
    assert_eq!(moved.status(), reqwest::StatusCode::CREATED);
    let moved_body: Value = moved.json().await?;
    let moved_plan_id = moved_body["plan"]["id"]
        .as_str()
        .ok_or("missing moved plan id")?
        .to_owned();

    let restored = client
        .patch(format!(
            "{base_url}/api/v1/admin/scheduled-task-plans/{original_plan_id}"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "libraryIds": [library_ids[0]] }))
        .send()
        .await?;
    assert_eq!(restored.status(), reqwest::StatusCode::OK);
    assert_eq!(restored.json::<Value>().await?["plan"]["libraryCount"], 1);

    let default_plan_id: String = sqlx::query_scalar(
        "SELECT id FROM scheduled_task_plans
         WHERE task_type = 'RECONCILIATION_SCAN' AND scope_type = 'LIBRARY'
           AND is_default = 1",
    )
    .fetch_one(database.pool())
    .await?;
    let default_delete = client
        .delete(format!(
            "{base_url}/api/v1/admin/scheduled-task-plans/{default_plan_id}"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(default_delete.status(), reqwest::StatusCode::CONFLICT);

    let deleted = client
        .delete(format!(
            "{base_url}/api/v1/admin/scheduled-task-plans/{moved_plan_id}"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);
    let deleted_plan_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scheduled_task_plans WHERE id = ?")
            .bind(&moved_plan_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(deleted_plan_count, 0);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_plan_run_continues_after_one_library_rejection()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, database) = start_server(config).await?;
    let client = reqwest::Client::new();
    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);
    let (cookies, csrf) = login(&client, &base_url, "admin", "correct password").await?;

    let mut library_ids = Vec::new();
    for name in ["A Missing", "B Ready"] {
        let response = client
            .post(format!("{base_url}/api/v1/admin/libraries"))
            .header(COOKIE, &cookies)
            .header("x-csrf-token", &csrf)
            .json(&json!({ "name": name, "kind": "MOVIE" }))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::CREATED);
        library_ids.push(
            response.json::<Value>().await?["library"]["id"]
                .as_str()
                .ok_or("missing library id")?
                .to_owned(),
        );
    }

    let created = client
        .post(format!("{base_url}/api/v1/admin/scheduled-task-plans"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "taskType": "RECONCILIATION_SCAN",
            "name": "验证逐库派发",
            "isEnabled": true,
            "libraryIds": library_ids
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let plan_id = created.json::<Value>().await?["plan"]["id"]
        .as_str()
        .ok_or("missing plan id")?
        .to_owned();

    sqlx::query(
        "DELETE FROM scheduled_task_configs
         WHERE owner_type = 'LIBRARY' AND owner_id = ?
           AND task_type = 'RECONCILIATION_SCAN'",
    )
    .bind(&library_ids[0])
    .execute(database.pool())
    .await?;

    let run = client
        .post(format!(
            "{base_url}/api/v1/admin/scheduled-task-plans/{plan_id}/run"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(run.status(), reqwest::StatusCode::ACCEPTED);
    let run_body: Value = run.json().await?;
    assert_eq!(run_body["runs"].as_array().map(Vec::len), Some(1));
    assert_eq!(run_body["runs"][0]["libraryId"], library_ids[1]);
    assert_eq!(run_body["skippedLibraryIds"], json!([library_ids[0]]));

    server.abort();
    Ok(())
}
