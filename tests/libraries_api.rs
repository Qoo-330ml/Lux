use std::time::Duration;

use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
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

    let edited = client
        .patch(format!("{base_url}/api/v1/admin/libraries/{library_id}"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "name": "Shows", "kind": "SERIES" }))
        .send()
        .await?;
    assert_eq!(edited.status(), reqwest::StatusCode::OK);
    let edited_library = edited.json::<Value>().await?["library"].clone();
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
    assert!(cover_body["library"]["coverImageUrl"].is_string());

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
            "realtimeWatchEnabled": true,
            "realtimeMetadataAutoMatchEnabled": true,
            "incrementalSchedule": "interval:30s",
            "reconciliationSchedule": "0 3 * * *",
            "metadataSchedule": "interval:5m",
            "scanConcurrency": 4,
            "probeConcurrency": 3
        }))
        .send()
        .await?;
    assert_eq!(first_update.status(), reqwest::StatusCode::OK);
    let first_body: Value = first_update.json().await?;
    assert_eq!(first_body["library"]["incrementalSchedule"], Value::Null);
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
        false
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
            "disc": true,
            "wallpaper": false,
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
    assert_eq!(first["metadataSchedule"], "interval:5m");
    assert_eq!(first["scanConcurrency"], 4);
    assert_eq!(first["probeConcurrency"], 3);
    assert_eq!(second["reconciliationSchedule"], Value::Null);
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
                    && schedule.as_deref() == Some("interval:5m")
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
async fn admin_can_list_and_update_library_schedules_from_operations_page()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, _) = start_server(config).await?;
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
        .json(&json!({ "reconciliationSchedule": "interval:6h" }))
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
    assert_eq!(reconciliation["schedule"], "interval:6h");
    assert_eq!(reconciliation["isEnabled"], true);

    let missing_csrf = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookies)
        .json(&json!({
            "ownerType": "LIBRARY",
            "ownerId": library_id,
            "taskType": "METADATA_PARSE",
            "schedule": "interval:2h"
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
            "schedule": "interval:2h"
        }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    let updated_body: Value = updated.json().await?;
    assert_eq!(updated_body["scheduledTask"]["schedule"], "interval:2h");
    assert_eq!(updated_body["scheduledTask"]["isEnabled"], true);

    let global_schedule = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "GLOBAL",
            "ownerId": "global",
            "taskType": "RECONCILIATION_SCAN",
            "schedule": "interval:6h"
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
            "schedule": "interval:6h"
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
        "interval:6h"
    );
    let incremental_task = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "LIBRARY",
            "ownerId": library_id,
            "taskType": "INCREMENTAL_SCAN",
            "schedule": "interval:30s"
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
    assert_eq!(unchanged_library["reconciliationSchedule"], "interval:6h");

    let invalid_task = client
        .put(format!("{base_url}/api/v1/admin/scheduled-tasks"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "ownerType": "LIBRARY",
            "ownerId": library_id,
            "taskType": "REBUILD_SEARCH",
            "schedule": "interval:2h"
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

    server.abort();
    Ok(())
}
