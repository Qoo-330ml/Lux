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
async fn user_playback_threshold_defaults_to_95_and_is_csrf_protected()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session = cookie_value(&login, "lux_session")?;
    let csrf = cookie_value(&login, "lux_csrf")?;
    let cookies = format!("lux_session={session}; lux_csrf={csrf}");

    let defaults = client
        .get(format!("{base_url}/api/v1/auth/settings"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(defaults.status(), reqwest::StatusCode::OK);
    let defaults = defaults.json::<Value>().await?;
    assert_eq!(defaults["playedPercent"], 95);
    assert_eq!(defaults["useAdminLibraryOrder"], true);
    assert_eq!(defaults["libraryOrderForced"], false);

    let missing_csrf = client
        .patch(format!("{base_url}/api/v1/auth/settings"))
        .header(COOKIE, &cookies)
        .json(&json!({ "playedPercent": 80 }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let updated = client
        .patch(format!("{base_url}/api/v1/auth/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "playedPercent": 80 }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    assert_eq!(updated.json::<Value>().await?["playedPercent"], 80);

    let invalid = client
        .patch(format!("{base_url}/api/v1/auth/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "playedPercent": 101 }))
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn account_preferences_api_is_authenticated_versioned_and_csrf_protected()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
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
    let client = reqwest::Client::new();

    let unauthenticated = client
        .get(format!("{base_url}/api/v1/auth/account-preferences"))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session = cookie_value(&login, "lux_session")?;
    let csrf = cookie_value(&login, "lux_csrf")?;
    let cookies = format!("lux_session={session}; lux_csrf={csrf}");

    let defaults = client
        .get(format!("{base_url}/api/v1/auth/account-preferences"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(defaults.status(), reqwest::StatusCode::OK);
    let defaults = defaults.json::<Value>().await?;
    assert_eq!(defaults["theme"], "dark");
    assert_eq!(defaults["accentColor"], "silver");
    assert_eq!(defaults["showMediaLibraries"], true);
    assert_eq!(defaults["showContinueWatching"], true);
    assert_eq!(defaults["audioLanguage"], "原始音轨");
    assert_eq!(defaults["subtitleLanguage"], "简体中文");
    assert_eq!(defaults["autoPlayNextEpisode"], true);
    assert_eq!(defaults["version"], 0);
    assert!(defaults["updatedAt"].is_null());

    let preferences = json!({
        "expectedVersion": 0,
        "theme": "light",
        "accentColor": "berry",
        "showMediaLibraries": false,
        "showContinueWatching": true,
        "audioLanguage": "日本語",
        "subtitleLanguage": "关闭",
        "autoPlayNextEpisode": false
    });
    let missing_csrf = client
        .patch(format!("{base_url}/api/v1/auth/account-preferences"))
        .header(COOKIE, &cookies)
        .json(&preferences)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let created = client
        .patch(format!("{base_url}/api/v1/auth/account-preferences"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&preferences)
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::OK);
    let created = created.json::<Value>().await?;
    assert_eq!(created["theme"], "light");
    assert_eq!(created["accentColor"], "berry");
    assert_eq!(created["version"], 1);
    assert!(created["updatedAt"].as_i64().is_some());

    let stale = client
        .patch(format!("{base_url}/api/v1/auth/account-preferences"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&preferences)
        .send()
        .await?;
    assert_eq!(stale.status(), reqwest::StatusCode::CONFLICT);
    let stale = stale.json::<Value>().await?;
    assert_eq!(stale["error"]["code"], "VERSION_CONFLICT");

    let invalid = client
        .patch(format!("{base_url}/api/v1/auth/account-preferences"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "expectedVersion": 1,
            "theme": "sepia",
            "accentColor": "silver",
            "showMediaLibraries": true,
            "showContinueWatching": true,
            "audioLanguage": "原始音轨",
            "subtitleLanguage": "简体中文",
            "autoPlayNextEpisode": true
        }))
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);

    sqlx::query("DROP TABLE user_account_preferences")
        .execute(database.pool())
        .await?;
    let unavailable = client
        .patch(format!("{base_url}/api/v1/auth/account-preferences"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "expectedVersion": 1,
            "theme": "dark",
            "accentColor": "silver",
            "showMediaLibraries": true,
            "showContinueWatching": true,
            "audioLanguage": "原始音轨",
            "subtitleLanguage": "简体中文",
            "autoPlayNextEpisode": true
        }))
        .send()
        .await?;
    assert_eq!(
        unavailable.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );

    server.abort();
    Ok(())
}

fn cookie_value(
    response: &reqwest::Response,
    name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .find_map(|value| {
            let value = value.to_str().ok()?;
            let value = value.strip_prefix(&format!("{name}="))?;
            Some(value.split(';').next()?.to_owned())
        })
        .ok_or_else(|| format!("missing {name} cookie").into())
}
