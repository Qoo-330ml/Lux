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
        Some(1)
    );

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
