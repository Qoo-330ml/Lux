use std::{fs, path::Path, time::Duration};

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

async fn seed_local_tmdb_package(config_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let plugin_dir = config_dir.join("plugins/org.lux.tmdb/binaries");
    tokio::fs::create_dir_all(&plugin_dir).await?;
    tokio::fs::write(plugin_dir.join("plugin"), b"#!/bin/sh\nexit 0\n").await?;
    tokio::fs::write(
        config_dir.join("plugins/org.lux.tmdb/manifest.json"),
        serde_json::to_vec(&json!({
            "formatVersion": 1,
            "id": "org.lux.tmdb",
            "name": "TMDb 元数据插件",
            "description": "从 TMDb 提供 Emby 风格电影、剧集和图片元数据。",
            "version": "0.1.5",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "metadata",
            "category": "SCRAPER",
            "supportedItemTypes": ["Movie"],
            "capabilities": ["metadata.search"],
            "configFields": [{
                "key": "apiKey",
                "label": "TMDb API Key",
                "type": "password",
                "required": false,
                "sensitive": true
            }],
            "permissions": {"network": [], "filesystem": []},
            "files": []
        }))?,
    )
    .await?;
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

fn plugin_by_id<'a>(body: &'a Value, plugin_id: &str) -> &'a Value {
    body["plugins"]
        .as_array()
        .and_then(|plugins| plugins.iter().find(|plugin| plugin["id"] == plugin_id))
        .unwrap_or_else(|| panic!("plugin {plugin_id} is missing from the response"))
}

#[tokio::test]
async fn admin_can_install_tmdb_and_select_it_for_a_library()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    tokio::fs::create_dir_all(&config_dir).await?;
    seed_local_tmdb_package(&config_dir).await?;
    tokio::fs::create_dir_all(config_dir.join("plugin-config")).await?;
    tokio::fs::write(
        config_dir.join("plugin-config/org.lux.tmdb.json"),
        br#"{"preferredLanguage":"zh-CN"}"#,
    )
    .await?;
    tokio::fs::write(config_dir.join("tmdb_api_key"), "test-key").await?;
    tokio::fs::write(config_dir.join("tmdb_settings.json"), "{}").await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir,
    };
    let (base_url, server) = start_server(config).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
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
    let tmdb = plugin_by_id(&catalog_body, "org.lux.tmdb");
    assert_eq!(catalog_body["total"], 6);
    assert_eq!(tmdb["category"], "SCRAPER");
    assert_eq!(tmdb["version"], "0.1.5");
    assert_eq!(tmdb["installed"], false);
    assert_eq!(tmdb["configured"], true);
    assert_eq!(tmdb["configurable"], true);
    assert_eq!(tmdb["configSource"], "CUSTOM");
    assert_eq!(tmdb["configFields"][0]["key"], "apiKey");
    assert_eq!(tmdb["configFields"][1]["key"], "preferredLanguage");
    assert_eq!(tmdb["configFields"][1]["options"][0]["value"], "zh-CN");
    assert_eq!(tmdb["configFields"][3]["multiple"], true);
    assert_eq!(tmdb["configValues"]["preferredLanguage"], "zh-CN");
    assert_eq!(
        tmdb["configValues"]["fallbackLanguages"],
        json!(["zh-SG", "zh-HK", "zh-TW"])
    );
    assert_eq!(tmdb["configValues"]["alternateApiEnabled"], false);
    assert_eq!(tmdb["configValues"]["titleAliasReplacementEnabled"], false);
    assert_eq!(
        tmdb["configValues"]["apiBaseUrl"],
        "https://api.themoviedb.org"
    );
    assert_eq!(tmdb["configFields"][4]["key"], "alternateApiEnabled");
    assert_eq!(
        tmdb["configFields"][5]["options"][1]["label"],
        "https://api.tmdb.org"
    );
    assert_eq!(
        tmdb["configFields"][6]["key"],
        "titleAliasReplacementEnabled"
    );
    assert!(tmdb.get("apiKey").is_none());

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

    let managed = client
        .get(format!(
            "{base_url}/api/v1/admin/plugins/installed?page=1&pageSize=20"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(managed.status(), reqwest::StatusCode::OK);
    let managed_body: Value = managed.json().await?;
    let managed_tmdb = plugin_by_id(&managed_body, "org.lux.tmdb");
    assert_eq!(managed_body["total"], 1);
    assert_eq!(managed_tmdb["id"], "org.lux.tmdb");

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
async fn admin_can_read_and_update_plugin_store_source() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server) = start_server(config).await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = admin_session(&client, &base_url).await?;

    let initial = client
        .get(format!("{base_url}/api/v1/admin/plugin-store"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(initial.status(), reqwest::StatusCode::OK);
    let initial_body: Value = initial.json().await?;
    assert_eq!(
        initial_body["url"],
        "https://github.com/Qoo-330ml/Lux-plugins"
    );
    assert_eq!(initial_body["defaultUrl"], initial_body["url"]);

    let invalid = client
        .put(format!("{base_url}/api/v1/admin/plugin-store"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "url": "http://example.com/index.json" }))
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);

    let updated = client
        .put(format!("{base_url}/api/v1/admin/plugin-store"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "url": "https://example.com/lux/index.json" }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    assert_eq!(
        updated.json::<Value>().await?["url"],
        "https://example.com/lux/index.json"
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_disable_an_installed_plugin_without_removing_it()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    seed_local_tmdb_package(&config.config_dir).await?;
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

    let disabled = client
        .patch(format!("{base_url}/api/v1/admin/plugins/tmdb/enabled"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "enabled": false }))
        .send()
        .await?;
    assert_eq!(disabled.status(), reqwest::StatusCode::OK);
    let disabled_body: Value = disabled.json().await?;
    assert_eq!(disabled_body["plugin"]["installed"], true);
    assert_eq!(disabled_body["plugin"]["enabled"], false);
    assert_eq!(disabled_body["plugin"]["available"], false);

    let managed = client
        .get(format!(
            "{base_url}/api/v1/admin/plugins/installed?page=1&pageSize=20"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(managed.status(), reqwest::StatusCode::OK);
    let managed_body: Value = managed.json().await?;
    assert_eq!(managed_body["total"], 1);
    assert_eq!(managed_body["plugins"][0]["installed"], true);
    assert_eq!(managed_body["plugins"][0]["enabled"], false);

    let rejected = client
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
    assert_eq!(rejected.status(), reqwest::StatusCode::CONFLICT);

    let enabled = client
        .patch(format!("{base_url}/api/v1/admin/plugins/tmdb/enabled"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "enabled": true }))
        .send()
        .await?;
    assert_eq!(enabled.status(), reqwest::StatusCode::OK);
    assert_eq!(enabled.json::<Value>().await?["plugin"]["enabled"], true);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_uninstall_an_installed_plugin_and_remove_its_package()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    tokio::fs::create_dir_all(&config_dir).await?;
    seed_local_tmdb_package(&config_dir).await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
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

    let uninstalled = client
        .delete(format!("{base_url}/api/v1/admin/plugins/tmdb"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(uninstalled.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(!config_dir.join("plugins/org.lux.tmdb").exists());
    assert!(!config_dir.join("plugin-config/org.lux.tmdb.json").exists());
    assert!(!config_dir.join("tmdb_api_key").exists());
    assert!(!config_dir.join("tmdb_settings.json").exists());

    let managed = client
        .get(format!(
            "{base_url}/api/v1/admin/plugins/installed?page=1&pageSize=20"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(managed.status(), reqwest::StatusCode::OK);
    let managed_body: Value = managed.json().await?;
    assert_eq!(managed_body["total"], 0);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_configure_tmdb_key_and_reset_to_the_plugin_default()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    seed_local_tmdb_package(&config.config_dir).await?;
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
        .json(&json!({
            "apiKey": custom_key,
            "preferredLanguage": "zh-SG",
            "languageFallbackEnabled": true,
            "titleAliasReplacementEnabled": true,
            "fallbackLanguages": ["zh-HK", "zh-TW"],
            "alternateApiEnabled": true,
            "apiBaseUrl": "https://api.tmdb.org"
        }))
        .send()
        .await?;
    assert_eq!(configured.status(), reqwest::StatusCode::OK);
    let configured_body: Value = configured.json().await?;
    assert_eq!(configured_body["plugin"]["configSource"], "CUSTOM");
    assert_eq!(
        configured_body["plugin"]["configValues"]["preferredLanguage"],
        "zh-SG"
    );
    assert_eq!(
        configured_body["plugin"]["configValues"]["languageFallbackEnabled"],
        true
    );
    assert_eq!(
        configured_body["plugin"]["configValues"]["titleAliasReplacementEnabled"],
        true
    );
    assert_eq!(
        configured_body["plugin"]["configValues"]["fallbackLanguages"],
        json!(["zh-HK", "zh-TW"])
    );
    assert_eq!(
        configured_body["plugin"]["configValues"]["alternateApiEnabled"],
        true
    );
    assert_eq!(
        configured_body["plugin"]["configValues"]["apiBaseUrl"],
        "https://api.tmdb.org"
    );
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
        "PLUGIN_DEFAULT"
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
    let tmdb = plugin_by_id(&catalog, "org.lux.tmdb");
    assert_eq!(catalog["total"], 6);
    assert_eq!(tmdb["category"], "SCRAPER");
    assert_eq!(tmdb["version"], "1.0.0");
    assert_eq!(tmdb["runtime"], "process");
    assert_eq!(tmdb["installed"], false);
    assert_eq!(tmdb["enabled"], false);
    assert_eq!(tmdb["available"], false);

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

#[tokio::test]
async fn admin_cannot_select_a_media_probe_plugin_as_a_library_scraper()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join("plugins/org.lux.strm-media-info");
    fs::create_dir_all(plugin_dir.join("binaries"))?;
    fs::write(plugin_dir.join("binaries/plugin"), b"plugin")?;
    fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec(&json!({
            "formatVersion": 1,
            "id": "org.lux.strm-media-info",
            "name": "strm媒体信息提取",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "media_probe",
            "category": "MEDIA",
            "capabilities": ["media.probe"],
            "permissions": {"network": ["media-source"], "filesystem": []},
            "files": []
        }))?,
    )?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir,
    };
    let (base_url, server) = start_server(config).await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = admin_session(&client, &base_url).await?;

    let installed = client
        .post(format!(
            "{base_url}/api/v1/admin/plugins/org.lux.strm-media-info/install"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(installed.status(), reqwest::StatusCode::CREATED);

    let response = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "name": "STRM",
            "kind": "MOVIE",
            "scraperId": "org.lux.strm-media-info"
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        response.json::<Value>().await?["error"]["code"],
        "PLUGIN_UNAVAILABLE"
    );

    server.abort();
    Ok(())
}
