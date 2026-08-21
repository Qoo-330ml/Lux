#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt};

use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const PLUGIN_ID: &str = "org.lux.intro-outro-detector";

#[tokio::test]
async fn chapter_detection_http_api_enforces_admin_csrf_and_persists_jobs()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join(format!("plugins/{PLUGIN_ID}"));
    tokio::fs::create_dir_all(plugin_dir.join("binaries")).await?;
    let binary = plugin_dir.join("binaries/plugin");
    fs::write(&binary, b"#!/bin/sh\n")?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;
    tokio::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": PLUGIN_ID,
            "name": "Intro/outro detector",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "chapter_detector",
            "category": "MEDIA",
            "supportedItemTypes": ["Episode"],
            "capabilities": ["chapters.detect"],
            "configFields": [
                {"key": "concurrency", "label": "Concurrency", "type": "number", "required": true, "defaultValue": 2, "minimum": 1, "maximum": 16},
                {"key": "introWindowSeconds", "label": "Intro window", "type": "number", "required": true, "defaultValue": 180, "minimum": 15, "maximum": 300},
                {"key": "creditsWindowSeconds", "label": "Credits window", "type": "number", "required": true, "defaultValue": 180, "minimum": 15, "maximum": 600},
                {"key": "matchThreshold", "label": "Threshold", "type": "number", "required": true, "defaultValue": 80, "minimum": 1, "maximum": 100},
                {"key": "schedule", "label": "Schedule", "type": "text", "required": true, "defaultValue": "0 4 * * 0"}
            ],
            "permissions": {"network": [], "filesystem": []},
            "files": []
        }))?,
    )
    .await?;

    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir,
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
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
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({"username": "admin", "password": "correct password"}))
        .send()
        .await?;
    let cookie = cookie_pair(login.headers());
    let csrf = cookie_value(login.headers(), "lux_csrf");

    let library = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({"name": "Shows", "kind": "SERIES"}))
        .send()
        .await?;
    assert_eq!(library.status(), reqwest::StatusCode::CREATED);
    let library_id = library.json::<Value>().await?["library"]["id"]
        .as_str()
        .ok_or("missing library id")?
        .to_owned();

    let installed = client
        .post(format!(
            "{base_url}/api/v1/admin/plugins/{PLUGIN_ID}/install"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(installed.status(), reqwest::StatusCode::CREATED);

    let mixed_library = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "name": "Mixed",
            "kind": "MIXED",
            "chapterSourceId": PLUGIN_ID
        }))
        .send()
        .await?;
    assert_eq!(mixed_library.status(), reqwest::StatusCode::CREATED);
    let mixed_library_id = mixed_library.json::<Value>().await?["library"]["id"]
        .as_str()
        .ok_or("missing mixed library id")?
        .to_owned();

    let movie_with_source = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "name": "Movies",
            "kind": "MOVIE",
            "chapterSourceId": PLUGIN_ID
        }))
        .send()
        .await?;
    assert_eq!(movie_with_source.status(), reqwest::StatusCode::BAD_REQUEST);

    let mixed_selected = client
        .patch(format!(
            "{base_url}/api/v1/admin/libraries/{mixed_library_id}"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({"chapterSourceId": PLUGIN_ID}))
        .send()
        .await?;
    assert_eq!(mixed_selected.status(), reqwest::StatusCode::OK);

    let sources = client
        .get(format!(
            "{base_url}/api/v1/admin/chapter-sources?page=1&pageSize=50"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(sources.status(), reqwest::StatusCode::OK);
    assert_eq!(
        sources.json::<Value>().await?["sources"][0]["id"],
        PLUGIN_ID
    );

    let selected = client
        .patch(format!("{base_url}/api/v1/admin/libraries/{library_id}"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({"chapterSourceId": PLUGIN_ID}))
        .send()
        .await?;
    assert_eq!(selected.status(), reqwest::StatusCode::OK);

    let missing_csrf = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/{library_id}/chapter-detection"
        ))
        .header(COOKIE, &cookie)
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let configured = client
        .put(format!(
            "{base_url}/api/v1/admin/plugins/{PLUGIN_ID}/config"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "concurrency": 3,
            "introWindowSeconds": 120,
            "creditsWindowSeconds": 240,
            "matchThreshold": 90,
            "schedule": "0 4 * * 0"
        }))
        .send()
        .await?;
    assert_eq!(configured.status(), reqwest::StatusCode::OK);

    let scheduled = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "LIBRARY",
            "ownerId": library_id,
            "taskType": "CHAPTER_DETECTION",
            "schedule": "0 2 * * 0"
        }))
        .send()
        .await?;
    assert_eq!(scheduled.status(), reqwest::StatusCode::OK);
    assert_eq!(
        scheduled.json::<Value>().await?["scheduledTask"]["schedule"],
        "0 2 * * 0"
    );

    let manual_run = client
        .post(format!("{base_url}/api/v1/admin/scheduled-tasks/run"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "LIBRARY",
            "ownerId": library_id,
            "taskType": "CHAPTER_DETECTION"
        }))
        .send()
        .await?;
    assert_eq!(manual_run.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        manual_run.json::<Value>().await?["taskType"],
        "CHAPTER_DETECTION"
    );
    for _ in 0..100 {
        let active_jobs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM chapter_detection_jobs
             WHERE library_id = ? AND status IN ('PENDING', 'RUNNING')",
        )
        .bind(&library_id)
        .fetch_one(database.pool())
        .await?;
        if active_jobs == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let active_jobs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chapter_detection_jobs
         WHERE library_id = ? AND status IN ('PENDING', 'RUNNING')",
    )
    .bind(&library_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        active_jobs, 0,
        "scheduled chapter detection should finish first"
    );

    let started = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/{library_id}/chapter-detection"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({"forceRefresh": true}))
        .send()
        .await?;
    assert_eq!(started.status(), reqwest::StatusCode::ACCEPTED);
    let started_body = started.json::<Value>().await?;
    let job_id = started_body["job"]["id"]
        .as_str()
        .ok_or("missing chapter detection job id")?
        .to_owned();
    assert_eq!(started_body["job"]["concurrency"], 3);
    assert_eq!(started_body["job"]["introWindowSeconds"], 120);
    assert_eq!(started_body["job"]["creditsWindowSeconds"], 240);
    assert_eq!(started_body["job"]["matchThreshold"], 90);

    let listed = client
        .get(format!("{base_url}/api/v1/admin/chapter-detection-jobs"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    assert_eq!(listed.json::<Value>().await?["jobs"][0]["id"], job_id);

    let detail = client
        .get(format!(
            "{base_url}/api/v1/admin/chapter-detection-jobs/{job_id}"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(detail.status(), reqwest::StatusCode::OK);
    assert_eq!(detail.json::<Value>().await?["job"]["pluginId"], PLUGIN_ID);

    let task_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scheduled_task_configs WHERE task_type = 'CHAPTER_DETECTION'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(task_count, 2);
    server.abort();
    Ok(())
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
        .expect("login should set cookie")
}

fn cookie_pair(headers: &reqwest::header::HeaderMap) -> String {
    format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(headers, "lux_session"),
        cookie_value(headers, "lux_csrf")
    )
}
