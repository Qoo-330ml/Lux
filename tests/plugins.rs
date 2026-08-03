use std::time::Duration;

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

async fn start_server(
    config: Config,
) -> Result<(String, tokio::task::JoinHandle<Result<(), std::io::Error>>), Box<dyn std::error::Error>>
{
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    Ok((format!("http://{address}"), server))
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

async fn admin_session(
    client: &reqwest::Client,
    base_url: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
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

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session = cookie_value(login.headers(), "lux_session");
    let csrf = cookie_value(login.headers(), "lux_csrf");
    Ok((format!("lux_session={session}; lux_csrf={csrf}"), csrf))
}

#[tokio::test]
async fn admin_can_install_tmdb_and_select_it_for_a_library()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    tokio::fs::create_dir_all(&config_dir).await?;
    tokio::fs::write(
        config_dir.join("tmdb_read_access_token"),
        "test-only-configured-value\n",
    )
    .await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir,
    };
    let (base_url, server) = start_server(config).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;

    let unauthenticated = client
        .get(format!(
            "{base_url}/api/v1/admin/plugins?page=1&pageSize=20"
        ))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let (cookies, csrf) = admin_session(&client, &base_url).await?;
    let catalog = client
        .get(format!(
            "{base_url}/api/v1/admin/plugins?page=1&pageSize=20"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(catalog.status(), reqwest::StatusCode::OK);
    let catalog_body: Value = catalog.json().await?;
    assert_eq!(catalog_body["total"], 1);
    assert_eq!(catalog_body["plugins"][0]["id"], "tmdb");
    assert_eq!(catalog_body["plugins"][0]["installed"], false);
    assert_eq!(catalog_body["plugins"][0]["configured"], true);

    let installed = client
        .post(format!("{base_url}/api/v1/admin/plugins/tmdb/install"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(installed.status(), reqwest::StatusCode::CREATED);
    assert_eq!(
        installed.json::<Value>().await?["plugin"]["installed"],
        true
    );

    let created = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "name": "Movies",
            "kind": "MOVIE",
            "scraperId": "tmdb"
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let created_body: Value = created.json().await?;
    assert_eq!(created_body["library"]["scraperId"], "tmdb");
    let library_id = created_body["library"]["id"]
        .as_str()
        .ok_or("missing library ID")?;

    let cleared = client
        .patch(format!("{base_url}/api/v1/admin/libraries/{library_id}"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "scraperId": null }))
        .send()
        .await?;
    assert_eq!(cleared.status(), reqwest::StatusCode::OK);
    assert_eq!(
        cleared.json::<Value>().await?["library"]["scraperId"],
        Value::Null
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn unconfigured_tmdb_cannot_be_selected_even_after_install()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server) = start_server(config).await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = admin_session(&client, &base_url).await?;

    let installed = client
        .post(format!("{base_url}/api/v1/admin/plugins/tmdb/install"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(installed.status(), reqwest::StatusCode::CREATED);

    let created = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "name": "Movies",
            "kind": "MOVIE",
            "scraperId": "tmdb"
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        created.json::<Value>().await?["error"]["code"],
        "PLUGIN_UNAVAILABLE"
    );

    server.abort();
    Ok(())
}
