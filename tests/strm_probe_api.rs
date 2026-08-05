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
async fn admin_can_start_and_list_strm_probe_jobs() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cookie = cookie_pair(login.headers());
    let csrf = cookie_value(login.headers(), "lux_csrf");

    let library = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "name": "Movies", "kind": "MOVIE" }))
        .send()
        .await?;
    assert_eq!(library.status(), reqwest::StatusCode::CREATED);
    let library_id = library.json::<Value>().await?["library"]["id"]
        .as_str()
        .ok_or("missing library id")?
        .to_owned();

    let invalid = client
        .post(format!("{base_url}/api/v1/admin/strm-probe-jobs"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "libraryIds": [library_id], "concurrency": 0 }))
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);

    let started = client
        .post(format!("{base_url}/api/v1/admin/strm-probe-jobs"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "libraryIds": [library_id], "concurrency": 2 }))
        .send()
        .await?;
    assert_eq!(started.status(), reqwest::StatusCode::ACCEPTED);
    let started_body = started.json::<Value>().await?;
    let job_id = started_body["jobs"][0]["id"]
        .as_str()
        .ok_or("missing STRM probe job id")?
        .to_owned();
    assert!(started_body["operationId"].is_string());

    let listed = client
        .get(format!("{base_url}/api/v1/admin/strm-probe-jobs"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    assert_eq!(
        listed.json::<Value>().await?["jobs"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    let detail = client
        .get(format!("{base_url}/api/v1/admin/strm-probe-jobs/{job_id}"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(detail.status(), reqwest::StatusCode::OK);
    assert_eq!(detail.json::<Value>().await?["job"]["id"], job_id);

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
        .ok_or_else(|| format!("missing cookie {name}"))
        .expect("login should set cookie")
}

fn cookie_pair(headers: &reqwest::header::HeaderMap) -> String {
    format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(headers, "lux_session"),
        cookie_value(headers, "lux_csrf")
    )
}
