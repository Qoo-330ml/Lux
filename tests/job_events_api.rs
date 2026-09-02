use std::time::Duration;

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
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

    let (base_url, server) = start_server(config, database, setup).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
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
    let job_id = scan.json::<Value>().await?["job"]["id"]
        .as_str()
        .ok_or("missing job ID")?
        .to_owned();

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

    server.abort();
    Ok(())
}
