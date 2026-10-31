use image::{DynamicImage, ImageFormat, Rgba};
use luxd::{
    api::{AppState, app_with_state},
    application::settings::{is_valid_login_background_source, login_background_plugin_id},
    application::{
        libraries::LibraryService, metadata::MetadataEnricher, scanner::LibraryScanner,
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::Cursor,
    os::unix::fs::PermissionsExt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;
use uuid::Uuid;

fn emby_public_id(id: &str) -> String {
    Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
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

#[test]
fn login_background_plugin_source_requires_a_valid_plugin_id() {
    assert!(is_valid_login_background_source(
        "PLUGIN:org.lux.bing-daily_1"
    ));
    assert_eq!(
        login_background_plugin_id("PLUGIN:org.lux.bing-daily_1"),
        Some("org.lux.bing-daily_1")
    );
    for invalid in [
        "PLUGIN:",
        "plugin:org.lux.provider",
        "PLUGIN:org/lux/provider",
        "PLUGIN:org.lux.provider?secret",
    ] {
        assert!(!is_valid_login_background_source(invalid), "{invalid}");
        assert_eq!(login_background_plugin_id(invalid), None);
    }
    assert!(!is_valid_login_background_source(&format!(
        "PLUGIN:{}",
        "a".repeat(129)
    )));
}

#[tokio::test]
async fn login_background_uses_recent_posters_only_when_enabled()
-> Result<(), Box<dyn std::error::Error>> {
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
    let movie_dir = root.join("Image Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Image.Movie.2020.mkv"), b"movie").await?;
    tokio::fs::write(movie_dir.join("poster.jpg"), b"poster-bytes").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    MetadataEnricher::new(database.clone())
        .enrich_movie_library(library.id)
        .await?;

    let shows_library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let shows_root = temp_dir.path().join("Shows");
    let episode_dir = shows_root.join("Example Show (2024)").join("Season 01");
    tokio::fs::create_dir_all(&episode_dir).await?;
    tokio::fs::write(
        shows_root.join("Example Show (2024)").join("poster.jpg"),
        b"series-poster",
    )
    .await?;
    tokio::fs::write(episode_dir.join("poster.jpg"), b"season-poster").await?;
    tokio::fs::write(episode_dir.join("Example.Show.S01E01.mkv"), b"episode").await?;
    tokio::fs::write(
        episode_dir.join("Example.Show.S01E01-poster.jpg"),
        b"episode-poster",
    )
    .await?;
    libraries
        .add_root(
            shows_library.id,
            shows_root.to_str().ok_or("non-utf8 root")?,
        )
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(shows_library.id)
        .await?;
    MetadataEnricher::new(database.clone())
        .enrich_series_library(shows_library.id)
        .await?;
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
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

    let default_background = client
        .get(format!("{base_url}/api/v1/auth/login-background"))
        .send()
        .await?;
    assert_eq!(default_background.status(), reqwest::StatusCode::OK);
    let default_body: Value = default_background.json().await?;
    assert_eq!(default_body["source"], "STATIC");
    assert_eq!(default_body["images"].as_array().map(Vec::len), Some(0));

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

    let invalid = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", cookie_value(login.headers(), "lux_csrf"))
        .json(&json!({ "loginBackgroundSource": "TMDB_POPULAR" }))
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);

    let unavailable_provider = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", cookie_value(login.headers(), "lux_csrf"))
        .json(&json!({ "loginBackgroundSource": "PLUGIN:org.lux.not-installed" }))
        .send()
        .await?;
    assert_eq!(
        unavailable_provider.status(),
        reqwest::StatusCode::BAD_REQUEST
    );

    let updated = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", cookie_value(login.headers(), "lux_csrf"))
        .json(&json!({ "loginBackgroundSource": "RECENTLY_ADDED" }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    assert_eq!(
        updated.json::<Value>().await?["loginBackgroundSource"],
        "RECENTLY_ADDED"
    );

    let background = client
        .get(format!("{base_url}/api/v1/auth/login-background"))
        .send()
        .await?;
    assert_eq!(background.status(), reqwest::StatusCode::OK);
    let body: Value = background.json().await?;
    assert_eq!(body["source"], "RECENTLY_ADDED");
    let allowed_public_ids = sqlx::query_scalar::<_, String>(
        "SELECT id FROM media_items WHERE item_type IN ('MOVIE', 'SERIES')",
    )
    .fetch_all(database.pool())
    .await?
    .into_iter()
    .map(|id| emby_public_id(&id))
    .collect::<BTreeSet<_>>();
    let returned_public_ids = body["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|url| url.split('/').nth(3))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    assert!(!returned_public_ids.is_empty());
    assert!(returned_public_ids.is_subset(&allowed_public_ids));
    let movie_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE'")
            .fetch_one(database.pool())
            .await?;
    let movie_public_id = emby_public_id(&movie_id);
    let image_url = body["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|url| url.contains(&format!("/Items/{movie_public_id}/")))
        .map(str::to_owned)
        .ok_or("missing login background image")?;
    assert!(image_url.starts_with("/emby/Items/"));
    assert!(image_url.contains("/Images/Primary?tag="));
    assert!(!body.to_string().contains("Image Movie"));
    assert!(!body.to_string().contains("Movies"));
    assert!(!body.to_string().contains("poster.jpg"));

    let poster = client.get(format!("{base_url}{image_url}")).send().await?;
    assert_eq!(poster.status(), reqwest::StatusCode::OK);
    assert_eq!(poster.bytes().await?.as_ref(), b"poster-bytes");

    server.abort();
    Ok(())
}

#[tokio::test]
async fn login_background_plugin_is_refreshed_in_worker_and_served_from_cache()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let plugin_id = "org.lux.login-background-test";
    let plugin_root = config.config_dir.join("plugins").join(plugin_id);
    let entrypoint = plugin_root.join("binaries/plugin");
    tokio::fs::create_dir_all(
        entrypoint
            .parent()
            .ok_or("missing plugin binary directory")?,
    )
    .await?;
    let call_count_path = temp_dir.path().join("plugin-call-count");
    let call_count_literal = serde_json::to_string(
        call_count_path
            .to_str()
            .ok_or("call count path is not UTF-8")?,
    )?;
    let dedicated_config_literal = serde_json::to_string(
        config
            .config_dir
            .join("plugin-config")
            .join(format!("{plugin_id}.json"))
            .to_str()
            .ok_or("plugin config path is not UTF-8")?,
    )?;
    let script = format!(
        r##"#!/usr/bin/env python3
import json
import os
import pathlib
import sys

count_path = pathlib.Path({call_count_literal})
for request_line in sys.stdin:
    request = json.loads(request_line)
    count = int(count_path.read_text()) if count_path.exists() else 0
    count_path.write_text(str(count + 1))
    safe_config = (
        "Dedicated config"
        if os.environ.get("LUX_PLUGIN_CONFIG_PATH") == {dedicated_config_literal}
        and "LUX_CONFIG_DIR" not in os.environ
        else "Shared config leaked"
    )
    print(json.dumps({{
        "id": request["id"],
        "result": {{
            "contentKind": "HERO_IMAGE",
            "sourceName": safe_config,
            "items": [{{"imageUrl": "https://images.example.com/today.jpg"}}]
        }}
    }}), flush=True)
"##
    );
    tokio::fs::write(&entrypoint, script).await?;
    let mut permissions = tokio::fs::metadata(&entrypoint).await?.permissions();
    permissions.set_mode(0o755);
    tokio::fs::set_permissions(&entrypoint, permissions).await?;
    tokio::fs::write(
        plugin_root.join("manifest.json"),
        serde_json::to_vec(&json!({
            "formatVersion": 1,
            "id": plugin_id,
            "name": "Login background test",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "login_background",
            "category": "UTILITY",
            "capabilities": ["login_background.get"],
            "permissions": {"imageHosts": ["images.example.com"]},
            "files": []
        }))?,
    )
    .await?;

    let database = Database::connect(&config).await?;
    sqlx::query("INSERT INTO installed_plugins (plugin_id, is_enabled) VALUES (?, 1)")
        .bind(plugin_id)
        .execute(database.pool())
        .await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
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
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

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
    let admin_settings = client
        .get(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(admin_settings.status(), reqwest::StatusCode::OK);
    assert_eq!(
        admin_settings.json::<Value>().await?["loginBackgroundSourceStatus"],
        "READY"
    );
    let selected = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", cookie_value(login.headers(), "lux_csrf"))
        .json(&json!({ "loginBackgroundSource": format!("PLUGIN:{plugin_id}") }))
        .send()
        .await?;
    assert_eq!(selected.status(), reqwest::StatusCode::OK);

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if tokio::fs::try_exists(&call_count_path)
                .await
                .unwrap_or(false)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;

    let background = client
        .get(format!("{base_url}/api/v1/auth/login-background"))
        .send()
        .await?;
    assert_eq!(background.status(), reqwest::StatusCode::OK);
    let body: Value = background.json().await?;
    assert_eq!(body["source"], format!("PLUGIN:{plugin_id}"));
    assert_eq!(body["contentKind"], "HERO_IMAGE");
    assert_eq!(body["sourceName"], "Dedicated config");
    assert_eq!(
        body["items"][0]["imageUrl"],
        "https://images.example.com/today.jpg"
    );
    assert_eq!(tokio::fs::read_to_string(&call_count_path).await?, "1");

    let stale_timestamp =
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64 - 49 * 60 * 60;
    sqlx::query("UPDATE login_background_plugin_cache SET refreshed_at = ? WHERE plugin_id = ?")
        .bind(stale_timestamp)
        .bind(plugin_id)
        .execute(database.pool())
        .await?;
    let stale_background = client
        .get(format!("{base_url}/api/v1/auth/login-background"))
        .send()
        .await?;
    let stale_body: Value = stale_background.json().await?;
    assert_eq!(stale_body["source"], "STATIC");

    let fresh_timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    sqlx::query("UPDATE login_background_plugin_cache SET refreshed_at = ? WHERE plugin_id = ?")
        .bind(fresh_timestamp)
        .bind(plugin_id)
        .execute(database.pool())
        .await?;

    sqlx::query("UPDATE installed_plugins SET is_enabled = 0 WHERE plugin_id = ?")
        .bind(plugin_id)
        .execute(database.pool())
        .await?;
    let disabled_background = client
        .get(format!("{base_url}/api/v1/auth/login-background"))
        .send()
        .await?;
    let disabled_body: Value = disabled_background.json().await?;
    assert_eq!(disabled_body["source"], "STATIC");
    let selected_source: String = sqlx::query_scalar(
        "SELECT value FROM server_settings WHERE key = 'login_background_source'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(selected_source, format!("PLUGIN:{plugin_id}"));
    assert_eq!(tokio::fs::read_to_string(&call_count_path).await?, "1");

    let unrelated_update = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", cookie_value(login.headers(), "lux_csrf"))
        .json(&json!({ "resumePlayedPercent": 90 }))
        .send()
        .await?;
    assert_eq!(unrelated_update.status(), reqwest::StatusCode::OK);
    let unrelated_body: Value = unrelated_update.json().await?;
    assert_eq!(
        unrelated_body["loginBackgroundSource"],
        format!("PLUGIN:{plugin_id}")
    );
    assert_eq!(
        unrelated_body["loginBackgroundSourceStatus"],
        "PLUGIN_DISABLED"
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn unified_plugin_custom_image_upload_is_admin_only_and_served_only_when_selected()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let plugin_id = "org.lux.login-background";
    let plugin_root = config.config_dir.join("plugins").join(plugin_id);
    let entrypoint = plugin_root.join("binaries/plugin");
    tokio::fs::create_dir_all(
        entrypoint
            .parent()
            .ok_or("missing plugin binary directory")?,
    )
    .await?;
    tokio::fs::write(
        &entrypoint,
        r##"#!/usr/bin/env python3
import json
import os
import sys

config_path = os.environ["LUX_PLUGIN_CONFIG_PATH"]
for request_line in sys.stdin:
    request = json.loads(request_line)
    with open(config_path, encoding="utf-8") as config_file:
        config = json.load(config_file)
    source = config.get("source", "BING_DAILY")
    image_url = (
        "/api/v1/auth/login-background/custom-image"
        if source == "CUSTOM_IMAGE" and config.get("customImage")
        else "https://images.example.com/today.jpg"
    )
    print(json.dumps({
        "id": request["id"],
        "result": {
            "contentKind": "HERO_IMAGE",
            "sourceName": "Custom image fixture",
            "items": [{"imageUrl": image_url}]
        }
    }), flush=True)
"##,
    )
    .await?;
    let mut permissions = tokio::fs::metadata(&entrypoint).await?.permissions();
    permissions.set_mode(0o755);
    tokio::fs::set_permissions(&entrypoint, permissions).await?;
    tokio::fs::write(
        plugin_root.join("manifest.json"),
        serde_json::to_vec(&json!({
            "formatVersion": 1,
            "id": plugin_id,
            "name": "Unified login background",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "login_background",
            "category": "UTILITY",
            "capabilities": ["login_background.get"],
            "configFields": [
                {
                    "key": "source",
                    "label": "Source",
                    "type": "select",
                    "required": true,
                    "defaultValue": "CUSTOM_IMAGE",
                    "options": [
                        {"value": "BING_DAILY", "label": "Bing"},
                        {"value": "TMDB_TRENDING", "label": "TMDb"},
                        {"value": "CUSTOM_IMAGE", "label": "Custom image"}
                    ]
                },
                {"key": "customImage", "label": "Custom image", "type": "image"},
                {
                    "key": "customImageRightsConfirmed",
                    "label": "I may display this image publicly",
                    "type": "toggle",
                    "defaultValue": false
                }
            ],
            "permissions": {"imageHosts": ["images.example.com"]},
            "files": []
        }))?,
    )
    .await?;

    let database = Database::connect(&config).await?;
    sqlx::query("INSERT INTO installed_plugins (plugin_id, is_enabled) VALUES (?, 1)")
        .bind(plugin_id)
        .execute(database.pool())
        .await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let upload_url =
        format!("{base_url}/api/v1/admin/plugins/{plugin_id}/config/image/customImage");
    let image_bytes = |pixel| {
        let mut output = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, Rgba(pixel)))
            .write_to(&mut output, ImageFormat::Png)
            .expect("PNG fixture should encode");
        output.into_inner()
    };

    let unauthenticated = client
        .put(&upload_url)
        .header("Content-Type", "image/png")
        .body(image_bytes([1, 2, 3, 255]))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let csrf = cookie_value(login.headers(), "lux_csrf");
    let cookies = format!(
        "lux_session={}; lux_csrf={csrf}",
        cookie_value(login.headers(), "lux_session"),
    );

    let missing_csrf = client
        .put(&upload_url)
        .header(COOKIE, &cookies)
        .header("Content-Type", "image/png")
        .body(image_bytes([1, 2, 3, 255]))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let first_bytes = image_bytes([1, 2, 3, 255]);
    let first_upload = client
        .put(&upload_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .header("Content-Type", "image/png")
        .body(first_bytes.clone())
        .send()
        .await?;
    assert_eq!(first_upload.status(), reqwest::StatusCode::OK);
    let first_body: Value = first_upload.json().await?;
    let first_asset_id = first_body["assetId"]
        .as_str()
        .ok_or("upload response omitted assetId")?;
    assert!(first_asset_id.starts_with("sha256:"));
    assert_eq!(first_body["contentType"], "image/png");
    let plugin_config: Value = serde_json::from_slice(
        &tokio::fs::read(
            config
                .config_dir
                .join("plugin-config")
                .join(format!("{plugin_id}.json")),
        )
        .await?,
    )?;
    assert_eq!(plugin_config["customImage"], first_asset_id);
    assert!(!plugin_config.to_string().contains("/config/"));
    assert!(!plugin_config.to_string().contains("data:image"));

    let custom_image_url = format!("{base_url}/api/v1/auth/login-background/custom-image");
    let hidden_before_source_selection = client.get(&custom_image_url).send().await?;
    assert_eq!(
        hidden_before_source_selection.status(),
        reqwest::StatusCode::NOT_FOUND
    );

    let select_plugin = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "loginBackgroundSource": "PLUGIN:org.lux.login-background" }))
        .send()
        .await?;
    assert_eq!(select_plugin.status(), reqwest::StatusCode::OK);

    let login_background_url = format!("{base_url}/api/v1/auth/login-background");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(response) = client.get(&login_background_url).send().await
                && let Ok(body) = response.json::<Value>().await
                && body["items"][0]["imageUrl"] == "/api/v1/auth/login-background/custom-image"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    assert_eq!(
        client.get(&custom_image_url).send().await?.status(),
        reqwest::StatusCode::NOT_FOUND,
        "uploading and selecting a custom image is not enough without the rights confirmation"
    );

    let confirm_custom_image_rights = client
        .put(format!(
            "{base_url}/api/v1/admin/plugins/{plugin_id}/config"
        ))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "customImageRightsConfirmed": true }))
        .send()
        .await?;
    assert_eq!(
        confirm_custom_image_rights.status(),
        reqwest::StatusCode::OK
    );

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = client.get(&custom_image_url).send().await;
            if response
                .as_ref()
                .is_ok_and(|response| response.status() == reqwest::StatusCode::OK)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;

    let first_response = client.get(&custom_image_url).send().await?;
    assert_eq!(first_response.status(), reqwest::StatusCode::OK);
    assert_eq!(first_response.headers()["content-type"], "image/png");
    assert_eq!(
        first_response.headers()["x-content-type-options"],
        "nosniff"
    );
    assert_eq!(
        first_response.headers()["cache-control"],
        "no-cache, must-revalidate"
    );
    let first_etag = first_response.headers()["etag"].to_str()?.to_owned();
    assert_eq!(first_response.bytes().await?.as_ref(), first_bytes);

    let direct_asset_config = client
        .put(format!(
            "{base_url}/api/v1/admin/plugins/{plugin_id}/config"
        ))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "customImage": first_asset_id }))
        .send()
        .await?;
    assert_eq!(
        direct_asset_config.status(),
        reqwest::StatusCode::BAD_REQUEST
    );

    let not_modified = client
        .get(&custom_image_url)
        .header("If-None-Match", &first_etag)
        .send()
        .await?;
    assert_eq!(not_modified.status(), reqwest::StatusCode::NOT_MODIFIED);

    let head = client.head(&custom_image_url).send().await?;
    assert_eq!(head.status(), reqwest::StatusCode::OK);
    assert_eq!(head.headers()["etag"], first_etag);
    assert!(head.bytes().await?.is_empty());

    let select_bing_mode = client
        .put(format!(
            "{base_url}/api/v1/admin/plugins/{plugin_id}/config"
        ))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "source": "BING_DAILY" }))
        .send()
        .await?;
    assert_eq!(select_bing_mode.status(), reqwest::StatusCode::OK);
    assert_eq!(
        client.get(&custom_image_url).send().await?.status(),
        reqwest::StatusCode::NOT_FOUND
    );

    let restore_custom_mode = client
        .put(format!(
            "{base_url}/api/v1/admin/plugins/{plugin_id}/config"
        ))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "source": "CUSTOM_IMAGE" }))
        .send()
        .await?;
    assert_eq!(restore_custom_mode.status(), reqwest::StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(response) = client.get(&login_background_url).send().await
                && let Ok(body) = response.json::<Value>().await
                && body["items"][0]["imageUrl"] == "/api/v1/auth/login-background/custom-image"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    let restored_asset = client.get(&custom_image_url).send().await?;
    assert_eq!(restored_asset.status(), reqwest::StatusCode::OK);
    assert_eq!(restored_asset.bytes().await?.as_ref(), first_bytes);

    let bad_upload = client
        .put(&upload_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .header("Content-Type", "image/svg+xml")
        .body(b"<svg></svg>".to_vec())
        .send()
        .await?;
    assert_eq!(bad_upload.status(), reqwest::StatusCode::BAD_REQUEST);
    let after_bad_upload = client.get(&custom_image_url).send().await?;
    assert_eq!(after_bad_upload.status(), reqwest::StatusCode::OK);
    assert_eq!(after_bad_upload.bytes().await?.as_ref(), first_bytes);

    let oversized_upload = client
        .put(&upload_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .header("Content-Type", "image/png")
        .body(vec![0; 5 * 1024 * 1024 + 1])
        .send()
        .await?;
    assert_eq!(
        oversized_upload.status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE
    );
    let wrong_field = client
        .put(format!(
            "{base_url}/api/v1/admin/plugins/{plugin_id}/config/image/other"
        ))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .header("Content-Type", "image/png")
        .body(image_bytes([4, 5, 6, 255]))
        .send()
        .await?;
    assert_eq!(wrong_field.status(), reqwest::StatusCode::BAD_REQUEST);

    let second_bytes = image_bytes([9, 8, 7, 255]);
    let second_upload = client
        .put(&upload_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .header("Content-Type", "image/png")
        .body(second_bytes.clone())
        .send()
        .await?;
    assert_eq!(second_upload.status(), reqwest::StatusCode::OK);
    let second_body: Value = second_upload.json().await?;
    assert_ne!(second_body["assetId"], first_asset_id);
    let after_replace = client.get(&custom_image_url).send().await?;
    assert_eq!(after_replace.status(), reqwest::StatusCode::OK);
    assert_ne!(after_replace.headers()["etag"], first_etag);
    assert_eq!(after_replace.bytes().await?.as_ref(), second_bytes);

    let assets_directory = config.config_dir.join("login-background-assets");
    let assets = tokio::fs::read_dir(&assets_directory).await?;
    let mut asset_count = 0;
    let mut assets = assets;
    while assets.next_entry().await?.is_some() {
        asset_count += 1;
    }
    assert_eq!(
        asset_count, 1,
        "replaced upload should not retain the old asset"
    );
    sqlx::query("UPDATE installed_plugins SET is_enabled = 0 WHERE plugin_id = ?")
        .bind(plugin_id)
        .execute(database.pool())
        .await?;
    assert_eq!(
        client.get(&custom_image_url).send().await?.status(),
        reqwest::StatusCode::NOT_FOUND
    );

    server.abort();
    Ok(())
}
