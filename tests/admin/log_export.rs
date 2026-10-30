use std::{
    io::{Cursor, Read, Write},
    time::Duration,
};

use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    observability::logs::{LogDateRange, LogExport, export_logs},
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use zip::ZipArchive;

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

#[tokio::test]
async fn admin_can_export_selected_daily_logs_but_viewer_cannot()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let log_dir = config_dir.join("logs");
    tokio::fs::create_dir_all(&log_dir).await?;
    tokio::fs::write(
        log_dir.join("lux.2026-08-08.log"),
        br#"{"message":"older"}
"#,
    )
    .await?;
    tokio::fs::write(
        log_dir.join("lux.2026-08-09.log"),
        br#"{"message":"current"}
"#,
    )
    .await?;
    tokio::fs::write(log_dir.join("not-a-lux-log.txt"), b"must not export").await?;

    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir,
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?;
    let base_url = format!("http://{address}");

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let admin_cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(admin_login.headers(), "lux_session"),
        cookie_value(admin_login.headers(), "lux_csrf")
    );
    let archive_response = client
        .get(format!(
            "{base_url}/api/v1/admin/logs/export?from=2026-08-08&to=2026-08-09"
        ))
        .header(COOKIE, &admin_cookies)
        .send()
        .await?;
    assert_eq!(archive_response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        archive_response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/zip")
    );
    assert!(
        archive_response
            .headers()
            .get("content-disposition")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("lux-logs-20260808-20260809.zip"))
    );
    let bytes = archive_response.bytes().await?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    assert_eq!(archive.len(), 2);
    let mut contents = String::new();
    archive
        .by_name("lux.2026-08-09.log")?
        .read_to_string(&mut contents)?;
    assert!(contents.contains("current"));
    assert!(archive.by_name("not-a-lux-log.txt").is_err());

    let daily_response = client
        .get(format!(
            "{base_url}/api/v1/admin/logs/export?from=2026-08-09&to=2026-08-09"
        ))
        .header(COOKIE, &admin_cookies)
        .send()
        .await?;
    assert_eq!(daily_response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        daily_response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/x-ndjson")
    );
    assert!(
        daily_response
            .headers()
            .get("content-disposition")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("lux.2026-08-09.log"))
    );
    assert!(
        daily_response
            .text()
            .await?
            .contains(r#"{"message":"current"}"#)
    );

    let invalid = client
        .get(format!(
            "{base_url}/api/v1/admin/logs/export?from=2026-01-01&to=2026-02-01"
        ))
        .header(COOKIE, &admin_cookies)
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid.json::<Value>().await?["error"]["code"],
        "INVALID_REQUEST"
    );

    let viewer_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    let viewer_cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(viewer_login.headers(), "lux_session"),
        cookie_value(viewer_login.headers(), "lux_csrf")
    );
    let denied = client
        .get(format!("{base_url}/api/v1/admin/logs/export"))
        .header(COOKIE, viewer_cookies)
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn daily_log_export_reads_archived_segments() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let log_dir = temp_dir.path().join("config").join("logs");
    let archive_dir = log_dir.join("archive");
    tokio::fs::create_dir_all(&archive_dir).await?;
    let archive_path = archive_dir.join("lux.2026-08-08.part-000001.log.zip");
    let archive_file = std::fs::File::create(archive_path)?;
    let mut archive = zip::ZipWriter::new(archive_file);
    archive.start_file(
        "lux.2026-08-08.part-000001.log",
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated),
    )?;
    archive.write_all(b"{\"message\":\"archived\"}\n")?;
    archive.finish()?;

    let from = time::Date::from_calendar_date(2026, time::Month::August, 8)?;
    let range = LogDateRange::new(from, from)?;
    let export = export_logs(log_dir.parent().ok_or("missing config directory")?, range).await?;
    let LogExport::Daily { contents, filename } = export else {
        return Err("single-day export should remain a raw JSONL file".into());
    };
    assert_eq!(filename, "lux.2026-08-08.log");
    assert_eq!(contents, b"{\"message\":\"archived\"}\n");
    Ok(())
}
