use std::time::Duration;

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    observability::logs::LogStore,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

async fn start_server(
    config: Config,
    database: Database,
    setup: SetupService,
) -> Result<(String, tokio::task::JoinHandle<Result<(), std::io::Error>>), Box<dyn std::error::Error>>
{
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    Ok((
        format!("http://{address}"),
        tokio::spawn(async move { axum::serve(listener, app).await }),
    ))
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

#[tokio::test]
async fn admin_can_filter_and_page_scan_job_events() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
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
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Event Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Event.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    sqlx::query("DROP TABLE scan_job_events")
        .execute(database.pool())
        .await?;

    let log_store = LogStore::new(&config.config_dir);
    let (base_url, server) = start_server(config.clone(), database.clone(), setup).await?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?;
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(login.headers(), "lux_session"),
        cookie_value(login.headers(), "lux_csrf")
    );
    let csrf = cookie_value(login.headers(), "lux_csrf");
    let scan = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/{}/scan",
            library.id
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(scan.status(), reqwest::StatusCode::ACCEPTED);
    let scan_body = scan.json::<Value>().await?;
    let job_id = scan_body["job"]["id"]
        .as_str()
        .ok_or("missing job ID")?
        .to_owned();
    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json::<Value>()
        .await?;
    let scan_started = audit["events"]
        .as_array()
        .and_then(|events| {
            events
                .iter()
                .find(|event| event["eventType"] == "SCAN_STARTED")
        })
        .ok_or("missing SCAN_STARTED")?;
    assert_eq!(
        scan_started["metadata"]["libraryId"],
        json!(library.id.to_string())
    );
    assert_eq!(
        scan_started["metadata"]["jobType"],
        scan_body["job"]["jobType"]
    );
    log_store
        .append_scan_job_event(
            "api-redaction-event",
            &job_id,
            "WARN",
            "SCAN_SOURCE",
            "fixture warning https://example.invalid/private?token=x",
            r#"{"token":"do-not-log","apiKey":"do-not-log-either","sourceUrl":"https://example.invalid/private?token=x"}"#,
        )
        .await?;

    let mut events = Value::Null;
    for _ in 0..40 {
        let response = client
            .get(format!(
                "{base_url}/api/v1/admin/jobs/{job_id}/events?pageSize=100"
            ))
            .header(COOKIE, &cookies)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        events = response.json().await?;
        if events["events"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item["eventCode"] == "POSTPROCESSING_FAILED")
        }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(events["total"].as_i64().unwrap_or_default() >= 1);
    assert!(events["events"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["eventCode"] == "POSTPROCESSING_FAILED")
    }));
    assert!(events["events"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["eventCode"] == "JOB_CREATED" && item["level"] == "INFO")
    }));
    let completed = client
        .get(format!(
            "{base_url}/api/v1/admin/jobs/{job_id}/events?eventCode=POSTPROCESSING_FAILED&pageSize=1"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(completed.status(), reqwest::StatusCode::OK);
    let completed_body: Value = completed.json().await?;
    assert_eq!(completed_body["total"], 1);
    assert_eq!(completed_body["events"][0]["level"], "ERROR");

    let file_event = client
        .get(format!(
            "{base_url}/api/v1/admin/jobs/{job_id}/events?eventCode=SCAN_SOURCE&level=WARN"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(file_event.status(), reqwest::StatusCode::OK);
    let file_event_body: Value = file_event.json().await?;
    assert_eq!(file_event_body["total"], 1);
    assert_eq!(
        file_event_body["events"][0]["details"]["token"],
        "[REDACTED]"
    );
    assert_eq!(
        file_event_body["events"][0]["details"]["sourceUrl"],
        "[REDACTED]"
    );
    assert_eq!(file_event_body["events"][0]["message"], "[REDACTED_URL]");

    let errors = client
        .get(format!(
            "{base_url}/api/v1/admin/jobs/{job_id}/events?level=ERROR"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(errors.status(), reqwest::StatusCode::OK);
    assert_eq!(errors.json::<Value>().await?["total"], 1);

    let invalid = client
        .get(format!(
            "{base_url}/api/v1/admin/jobs/{job_id}/events?level=TRACE"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);

    let too_large = client
        .get(format!(
            "{base_url}/api/v1/admin/jobs/{job_id}/events?pageSize=101"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(too_large.status(), reqwest::StatusCode::BAD_REQUEST);

    let no_matches = client
        .get(format!(
            "{base_url}/api/v1/admin/jobs/{job_id}/events?eventCode=NO_MATCHING_EVENT"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(no_matches.status(), reqwest::StatusCode::OK);
    assert_eq!(no_matches.json::<Value>().await?["total"], 0);

    let cancelled = client
        .post(format!("{base_url}/api/v1/admin/jobs/{job_id}/cancel"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(cancelled.status(), reqwest::StatusCode::ACCEPTED);
    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json::<Value>()
        .await?;
    let scan_cancelled = audit["events"]
        .as_array()
        .and_then(|events| {
            events
                .iter()
                .find(|event| event["eventType"] == "SCAN_CANCELLED")
        })
        .ok_or("missing SCAN_CANCELLED")?;
    assert_eq!(
        scan_cancelled["metadata"]["libraryId"],
        json!(library.id.to_string())
    );
    assert!(scan_cancelled["metadata"]["previousStatus"].is_string());

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_scan_jobs_expose_historical_issues_and_retry_capabilities()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let library = LibraryService::new(database.clone())
        .create_library("Diagnostics", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let cases = [
        ("normal", "COMPLETED", "IDLE", None, false, false),
        (
            "processing",
            "COMPLETED",
            "POSTPROCESSING",
            None,
            false,
            false,
        ),
        ("history", "COMPLETED", "IDLE", None, true, true),
        ("unready", "COMPLETED", "IDLE", None, true, true),
        (
            "failed",
            "FAILED",
            "IDLE",
            Some("WORKER_FAILED"),
            false,
            true,
        ),
        (
            "marker",
            "COMPLETED",
            "IDLE",
            Some("POSTPROCESSING_FAILED"),
            true,
            false,
        ),
    ];
    for (index, (id, status, phase, error, _, _)) in cases.iter().enumerate() {
        sqlx::query("INSERT INTO scan_jobs (id, library_id, job_type, status, scan_phase, error, generation, created_at) VALUES (?, ?, 'RECONCILE_LIBRARY', ?, ?, ?, 'generation', ?)")
            .bind(id).bind(&library_id).bind(status).bind(phase).bind(error)
            .bind(100 - index as i64).execute(database.pool()).await?;
    }
    for (id, state) in [
        ("normal", "DONE"),
        ("processing", "PENDING"),
        ("history", "FAILED"),
    ] {
        sqlx::query("INSERT INTO scan_job_targets (job_id, target_type, target_id, item_id, change_kind, metadata_state) VALUES (?, 'ITEM', 'item', 'item', 'NEW', ?)")
            .bind(id).bind(state).execute(database.pool()).await?;
    }
    for (id, workflow, ready) in [("history", 1_i64, 1_i64), ("unready", 3_i64, 0_i64)] {
        sqlx::query("INSERT INTO scan_manifests (id, job_id, library_id, state, workflow_version, discovery_format_version, postprocessing_targets_ready) VALUES (?, ?, ?, 'POSTPROCESSING', ?, 3, ?)")
            .bind(id).bind(id).bind(&library_id).bind(workflow).bind(ready)
            .execute(database.pool()).await?;
    }
    LogStore::new(&config.config_dir)
        .append_scan_job_event(
            "root-unavailable",
            "unready",
            "WARN",
            "POSTPROCESSING_TARGET_ROOT_UNAVAILABLE",
            "root unavailable",
            "{}",
        )
        .await?;
    let (base_url, server) = start_server(config, database.clone(), setup).await?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()?;
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({"username": "admin", "password": "correct password"}))
        .send()
        .await?;
    let cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(login.headers(), "lux_session"),
        cookie_value(login.headers(), "lux_csrf")
    );
    let csrf = cookie_value(login.headers(), "lux_csrf");
    let jobs: Value = client
        .get(format!("{base_url}/api/v1/admin/jobs?pageSize=100"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json()
        .await?;
    for (id, _, _, _, issues, retryable) in cases {
        let listed = jobs["jobs"]
            .as_array()
            .ok_or("jobs")?
            .iter()
            .find(|job| job["id"] == id)
            .ok_or("job")?;
        assert_eq!(listed["hasPostprocessingIssues"], issues, "{id}");
        assert_eq!(listed["canRetry"], retryable, "{id}");
        let detail: Value = client
            .get(format!("{base_url}/api/v1/admin/jobs/{id}"))
            .header(COOKIE, &cookies)
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(detail["job"], *listed, "list and detail agree for {id}");
    }
    for (page, expected) in [(1, "history"), (2, "unready"), (3, "marker")] {
        let filtered: Value = client
            .get(format!(
                "{base_url}/api/v1/admin/jobs?hasPostprocessingIssues=true&pageSize=1&page={page}"
            ))
            .header(COOKIE, &cookies)
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(filtered["jobs"][0]["id"], expected);
    }
    let warnings: Value = client
        .get(format!(
            "{base_url}/api/v1/admin/jobs/unready/events?level=WARN"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        warnings["events"][0]["eventCode"],
        "POSTPROCESSING_TARGET_ROOT_UNAVAILABLE"
    );
    sqlx::query("INSERT INTO scan_jobs (id, library_id, job_type, status, generation) VALUES ('competing', ?, 'RECONCILE_LIBRARY', 'PENDING', 'generation')")
        .bind(&library_id).execute(database.pool()).await?;
    let blocked: Value = client
        .get(format!("{base_url}/api/v1/admin/jobs/history"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(blocked["job"]["hasPostprocessingIssues"], true);
    assert_eq!(blocked["job"]["canRetry"], false);
    let conflict = client
        .post(format!("{base_url}/api/v1/admin/jobs/history/retry"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(conflict.status(), reqwest::StatusCode::CONFLICT);
    sqlx::query("UPDATE scan_jobs SET status = 'CANCELLED' WHERE id = 'competing'")
        .execute(database.pool())
        .await?;
    let retry = client
        .post(format!("{base_url}/api/v1/admin/jobs/history/retry"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(retry.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(retry.json::<Value>().await?["job"]["id"], "history");
    let completed_retry = client
        .post(format!("{base_url}/api/v1/admin/jobs/normal/retry"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(completed_retry.status(), reqwest::StatusCode::CONFLICT);
    server.abort();
    Ok(())
}
