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

#[tokio::test]
async fn media_info_plugin_config_api_drives_a_background_run()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_root = config_dir.join("plugins/org.lux.strm-media-info");
    tokio::fs::create_dir_all(plugin_root.join("binaries")).await?;
    fs::write(plugin_root.join("binaries/plugin"), b"placeholder")?;
    fs::write(
        plugin_root.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": "org.lux.strm-media-info",
            "name": "strm媒体信息提取",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "media_probe",
            "category": "MEDIA",
            "capabilities": ["media.probe"],
            "configFields": [
                {"key": "libraryIds", "label": "媒体库", "type": "select", "multiple": true, "required": true, "optionsSource": "media-libraries"},
                {"key": "concurrency", "label": "并发数", "type": "number", "required": true, "defaultValue": 2, "minimum": 1, "maximum": 64},
                {"key": "existingInfoPolicy", "label": "已有媒体信息处理方式", "type": "select", "defaultValue": "SKIP", "options": [{"value": "SKIP", "label": "跳过已有媒体信息"}, {"value": "OVERWRITE", "label": "覆盖已有媒体信息"}]},
                {"key": "writeSidecars", "label": "写入旁车", "type": "toggle", "defaultValue": true},
                {"key": "schedule", "label": "执行计划", "type": "text", "required": true, "defaultValue": "0 3 * * *"}
            ],
            "permissions": {"network": ["media-source"], "filesystem": []},
            "files": []
        }))?,
    )?;

    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir,
    };
    let database = Database::connect(&config).await?;
    let database_for_assertions = database.clone();
    let setup = SetupService::new(database.clone())?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

    client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({"username": "Admin", "displayName": "Admin", "password": "correct password"}))
        .send()
        .await?;
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
        .json(&json!({"name": "Movies", "kind": "MOVIE"}))
        .send()
        .await?;
    let library_id = library.json::<Value>().await?["library"]["id"]
        .as_str()
        .ok_or("missing library id")?
        .to_owned();

    let media_root = temp_dir.path().join("remote-media");
    tokio::fs::create_dir_all(&media_root).await?;
    tokio::fs::write(
        media_root.join("Remote.Movie.2024.strm"),
        "https://example.invalid/movie.mkv",
    )
    .await?;
    let media_root_path = media_root.to_string_lossy().into_owned();
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('auto-strm-root', ?, ?, ?, 1, 1)",
    )
    .bind(&library_id)
    .bind(&media_root_path)
    .bind(&media_root_path)
    .execute(database_for_assertions.pool())
    .await?;
    sqlx::query(
        "INSERT INTO filesystem_entries (
             id, library_root_id, relative_path, entry_kind, size, modified_at,
             last_seen_generation, is_missing
         ) VALUES ('auto-strm-entry', 'auto-strm-root', 'Remote.Movie.2024.strm',
                   'FILE', 48, 1, 'generation', 0)",
    )
    .execute(database_for_assertions.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('auto-strm-item', ?, 'MOVIE', 'Remote Movie', 'remote movie', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database_for_assertions.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_sources (
             id, item_id, source_kind, filesystem_entry_id, external_url,
             strm_target_kind, probe_status
         ) VALUES ('auto-strm-source', 'auto-strm-item', 'STRM_URL', 'auto-strm-entry',
                  'https://example.invalid/movie.mkv', 'URL', 'NOT_CONFIGURED')",
    )
    .execute(database_for_assertions.pool())
    .await?;

    let installed = client
        .post(format!(
            "{base_url}/api/v1/admin/plugins/org.lux.strm-media-info/install"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(installed.status(), reqwest::StatusCode::CREATED);
    let installed_body = installed.json::<Value>().await?;
    assert_eq!(
        installed_body["plugin"]["configFields"][0]["optionsSource"],
        "media-libraries"
    );

    let updated = client
        .put(format!(
            "{base_url}/api/v1/admin/plugins/org.lux.strm-media-info/config"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "libraryIds": [library_id],
            "concurrency": 3,
            "existingInfoPolicy": "OVERWRITE",
            "writeSidecars": false
        }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    assert_eq!(
        updated.json::<Value>().await?["plugin"]["configValues"]["concurrency"],
        3
    );
    let mut auto_queued = false;
    for _ in 0..100 {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM strm_probe_jobs WHERE library_id = ?")
                .bind(&library_id)
                .fetch_one(database_for_assertions.pool())
                .await?;
        if count > 0 {
            auto_queued = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        auto_queued,
        "saving a new library selection should queue a STRM job"
    );

    let scheduled = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "GLOBAL",
            "ownerId": "global",
            "taskType": "STRM_MEDIA_INFO",
            "schedule": "0 4 * * *"
        }))
        .send()
        .await?;
    assert_eq!(scheduled.status(), reqwest::StatusCode::OK);
    assert_eq!(
        scheduled.json::<Value>().await?["scheduledTask"]["schedule"],
        "0 4 * * *"
    );

    let plugins = client
        .get(format!(
            "{base_url}/api/v1/admin/plugins/installed?page=1&pageSize=20"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    let plugin = plugins.json::<Value>().await?["plugins"]
        .as_array()
        .and_then(|plugins| {
            plugins
                .iter()
                .find(|plugin| plugin["id"] == "org.lux.strm-media-info")
        })
        .cloned()
        .ok_or("missing STRM plugin")?;
    assert_eq!(plugin["configValues"]["schedule"], "0 4 * * *");

    for _ in 0..200 {
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM strm_probe_jobs WHERE status IN ('PENDING', 'RUNNING')",
        )
        .fetch_one(database_for_assertions.pool())
        .await?;
        if active == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let run = client
        .post(format!(
            "{base_url}/api/v1/admin/plugins/org.lux.strm-media-info/run"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(run.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        run.json::<Value>().await?["jobs"].as_array().map(Vec::len),
        Some(1)
    );

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
