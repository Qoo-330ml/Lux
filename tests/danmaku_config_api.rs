use std::fs;

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

const PLUGIN_ID: &str = "org.lux.danmaku";

#[tokio::test]
async fn danmaku_scheduled_task_api_updates_plugin_schedule()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join(format!("plugins/{PLUGIN_ID}"));
    tokio::fs::create_dir_all(plugin_dir.join("binaries")).await?;
    fs::write(plugin_dir.join("binaries/plugin"), b"placeholder")?;
    tokio::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": PLUGIN_ID,
            "name": "弹幕匹配",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "danmaku",
            "category": "MEDIA",
            "capabilities": ["danmaku.match"],
            "configFields": [
                {"key": "providerBaseUrl", "label": "弹幕 API 地址", "type": "text", "required": true, "sensitive": true},
                {"key": "libraryIds", "label": "媒体库", "type": "select", "multiple": true, "optionsSource": "media-libraries", "defaultValue": []},
                {"key": "concurrency", "label": "并发数", "type": "number", "defaultValue": 2, "minimum": 0, "maximum": 64},
                {"key": "overwrite", "label": "覆盖已有弹幕文件", "type": "toggle", "defaultValue": false},
                {"key": "schedule", "label": "执行计划", "type": "text", "required": true, "defaultValue": "0 6 * * *"}
            ],
            "scheduledTasks": [{
                "taskType": "DANMAKU_MATCH",
                "ownerType": "GLOBAL",
                "name": "弹幕匹配",
                "description": "按计划为选定媒体库匹配并下载 Bilibili XML 弹幕旁车。",
                "scheduleConfigKey": "schedule",
                "defaultSchedule": "0 6 * * *",
                "requiredConfigKeys": ["providerBaseUrl", "libraryIds"],
                "resourceLimit": {"concurrency": 2, "overwrite": false}
            }],
            "permissions": {"network": ["*"], "filesystem": []},
            "files": []
        }))?,
    )
    .await?;

    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let library = luxd::application::libraries::LibraryService::new(database.clone())
        .create_library("Shows", luxd::library::LibraryKind::Series, false)
        .await?;
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

    let installed = client
        .post(format!(
            "{base_url}/api/v1/admin/plugins/{PLUGIN_ID}/install"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(installed.status(), reqwest::StatusCode::CREATED);

    let configured = client
        .put(format!(
            "{base_url}/api/v1/admin/plugins/{PLUGIN_ID}/config"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "providerBaseUrl": "https://danmu.example/api",
            "libraryIds": [library.id.to_string()],
            "concurrency": 0,
            "overwrite": true,
            "schedule": "0 6 * * *"
        }))
        .send()
        .await?;
    assert_eq!(configured.status(), reqwest::StatusCode::OK);

    let updated = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "GLOBAL",
            "ownerId": "global",
            "taskType": "DANMAKU_MATCH",
            "schedule": "0 2 * * *"
        }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    let updated_body: Value = updated.json().await?;
    assert_eq!(updated_body["scheduledTask"]["schedule"], "0 2 * * *");
    assert_eq!(updated_body["scheduledTask"]["isEnabled"], true);

    let plugins = client
        .get(format!(
            "{base_url}/api/v1/admin/plugins/installed?page=1&pageSize=20"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    let plugin = plugins.json::<Value>().await?["plugins"]
        .as_array()
        .and_then(|plugins| plugins.iter().find(|plugin| plugin["id"] == PLUGIN_ID))
        .cloned()
        .ok_or("missing danmaku plugin")?;
    assert_eq!(plugin["configValues"]["schedule"], "0 2 * * *");
    assert_eq!(plugin["configValues"]["concurrency"], 0);
    assert_eq!(plugin["configValues"]["overwrite"], true);

    let missing_schedule = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "GLOBAL",
            "ownerId": "global",
            "taskType": "DANMAKU_MATCH"
        }))
        .send()
        .await?;
    assert_eq!(missing_schedule.status(), reqwest::StatusCode::BAD_REQUEST);

    let stored_schedule: String = sqlx::query_scalar(
        "SELECT cron_or_interval FROM scheduled_task_configs
         WHERE owner_type = 'GLOBAL' AND owner_id = 'global' AND task_type = 'DANMAKU_MATCH'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored_schedule, "0 2 * * *");

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
        .expect("expected cookie")
}

fn cookie_pair(headers: &reqwest::header::HeaderMap) -> String {
    format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(headers, "lux_session"),
        cookie_value(headers, "lux_csrf")
    )
}
