use std::{fs, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signer, SigningKey};
use luxd::{
    api::{AppState, app_with_state},
    application::plugin_protocol::PluginManifest,
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn signed_dynamic_manifest() -> Value {
    let mut value = json!({
        "formatVersion": 1,
        "id": "org.lux.tmdb",
        "name": "TMDb 动态插件",
        "version": "1.0.0",
        "apiVersion": 1,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "metadata",
        "supportedItemTypes": ["Movie"],
        "capabilities": ["metadata.search"],
        "configFields": [{
            "key": "apiKey",
            "label": "TMDb API Key",
            "type": "password",
            "required": false,
            "sensitive": true
        }],
        "permissions": {"network": ["api.themoviedb.org"], "filesystem": ["plugin-cache"]},
        "files": [],
        "signature": {"algorithm": "ed25519", "keyId": "test", "value": "placeholder"}
    });
    let manifest = PluginManifest::from_value(value.clone()).expect("manifest should validate");
    let signature = SigningKey::from_bytes(&[7_u8; 32]).sign(
        &manifest
            .signing_payload()
            .expect("manifest payload should serialize"),
    );
    value["signature"]["value"] = Value::String(BASE64.encode(signature.to_bytes()));
    value
}

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
    assert_eq!(catalog_body["plugins"][0]["configurable"], true);
    assert_eq!(catalog_body["plugins"][0]["configSource"], "BUILT_IN");
    assert_eq!(
        catalog_body["plugins"][0]["configFields"][0]["key"],
        "apiKey"
    );
    assert!(catalog_body["plugins"][0].get("apiKey").is_none());

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
async fn admin_can_configure_tmdb_key_and_reset_to_the_embedded_default()
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

    let custom_key = "custom-api-key-for-test";
    let configured = client
        .put(format!("{base_url}/api/v1/admin/plugins/tmdb/config"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "apiKey": custom_key }))
        .send()
        .await?;
    assert_eq!(configured.status(), reqwest::StatusCode::OK);
    let configured_body: Value = configured.json().await?;
    assert_eq!(configured_body["plugin"]["configSource"], "CUSTOM");
    assert!(!configured_body.to_string().contains(custom_key));
    assert_eq!(
        tokio::fs::read_to_string(temp_dir.path().join("config/tmdb_api_key")).await?,
        format!("{custom_key}\n")
    );

    let reset = client
        .put(format!("{base_url}/api/v1/admin/plugins/tmdb/config"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "apiKey": "" }))
        .send()
        .await?;
    assert_eq!(reset.status(), reqwest::StatusCode::OK);
    assert_eq!(
        reset.json::<Value>().await?["plugin"]["configSource"],
        "BUILT_IN"
    );
    assert!(!temp_dir.path().join("config/tmdb_api_key").exists());

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
    assert_eq!(
        created.json::<Value>().await?["library"]["scraperId"],
        "tmdb"
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_discover_a_dynamic_plugin_package_after_startup()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join("plugins/org.lux.tmdb");
    fs::create_dir_all(plugin_dir.join("binaries"))?;
    fs::write(
        config_dir.join("plugins/trusted_keys.json"),
        serde_json::to_vec(&json!({
            "test": BASE64.encode(
                SigningKey::from_bytes(&[7_u8; 32])
                    .verifying_key()
                    .to_bytes()
            )
        }))?,
    )?;
    fs::write(plugin_dir.join("binaries/plugin"), b"plugin")?;
    fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec(&signed_dynamic_manifest())?,
    )?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir,
    };
    let (base_url, server) = start_server(config).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let (cookies, csrf) = admin_session(&client, &base_url).await?;

    let catalog = client
        .get(format!(
            "{base_url}/api/v1/admin/plugins?page=1&pageSize=20"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert_eq!(catalog["total"], 1);
    assert_eq!(catalog["plugins"][0]["id"], "org.lux.tmdb");
    assert_eq!(catalog["plugins"][0]["version"], "1.0.0");
    assert_eq!(catalog["plugins"][0]["runtime"], "process");

    let installed = client
        .post(format!(
            "{base_url}/api/v1/admin/plugins/org.lux.tmdb/install"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(installed.status(), reqwest::StatusCode::CREATED);
    assert_eq!(
        installed.json::<Value>().await?["plugin"]["installed"],
        true
    );

    server.abort();
    Ok(())
}
