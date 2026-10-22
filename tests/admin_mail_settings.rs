use std::{net::SocketAddr, time::Duration};

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

fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _) = value.split_once(';')?;
            let (cookie_name, cookie_value) = pair.split_once('=')?;
            (cookie_name == name).then(|| cookie_value.to_owned())
        })
}

#[tokio::test]
async fn admin_mail_settings_keep_credentials_private_and_check_connection_safely()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address: SocketAddr = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()?;

    let unauthorized = client
        .get(format!("{base_url}/api/v1/admin/settings/mail"))
        .send()
        .await?;
    assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({"username": "admin", "password": "correct password"}))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session = cookie_value(login.headers(), "lux_session").ok_or("missing session cookie")?;
    let csrf = cookie_value(login.headers(), "lux_csrf").ok_or("missing CSRF cookie")?;
    let cookies = format!("lux_session={session}; lux_csrf={csrf}");
    let smtp_listener = TcpListener::bind("127.0.0.1:0").await?;
    let smtp_address = smtp_listener.local_addr()?;
    let smtp_stall = tokio::spawn(async move {
        if let Ok((_connection, _peer)) = smtp_listener.accept().await {
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });

    let initial = client
        .get(format!("{base_url}/api/v1/admin/settings/mail"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(initial.status(), reqwest::StatusCode::OK);
    let initial: Value = initial.json().await?;
    assert_eq!(initial["configured"], false);
    assert_eq!(initial["credentialsConfigured"], false);
    assert!(initial["settings"].is_null());

    let settings = json!({
        "host": "127.0.0.1",
        "port": smtp_address.port(),
        "tlsMode": "SMTPS",
        "senderName": "Lux",
        "senderEmail": "noreply@example.test",
        "siteUrl": "https://lux.example.test"
    });
    let save_settings = client
        .put(format!("{base_url}/api/v1/admin/settings/mail"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&settings)
        .send()
        .await?;
    assert_eq!(save_settings.status(), reqwest::StatusCode::NO_CONTENT);

    let missing_csrf = client
        .put(format!("{base_url}/api/v1/admin/settings/mail/credentials"))
        .header(COOKIE, &cookies)
        .json(&json!({"username": "smtp-user", "password": "private-secret"}))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let save_credentials = client
        .put(format!("{base_url}/api/v1/admin/settings/mail/credentials"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({"username": "smtp-user", "password": "private-secret"}))
        .send()
        .await?;
    assert_eq!(save_credentials.status(), reqwest::StatusCode::NO_CONTENT);

    let update_settings_again = client
        .put(format!("{base_url}/api/v1/admin/settings/mail"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&settings)
        .send()
        .await?;
    assert_eq!(
        update_settings_again.status(),
        reqwest::StatusCode::NO_CONTENT
    );

    let configured = client
        .get(format!("{base_url}/api/v1/admin/settings/mail"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(configured.status(), reqwest::StatusCode::OK);
    let configured_text = configured.text().await?;
    assert!(!configured_text.contains("smtp-user"));
    assert!(!configured_text.contains("private-secret"));
    let configured: Value = serde_json::from_str(&configured_text)?;
    assert_eq!(configured["configured"], true);
    assert_eq!(configured["credentialsConfigured"], true);
    assert_eq!(configured["settings"], settings);

    let settings_file = tokio::fs::read_to_string(config_dir.join("smtp_settings.json")).await?;
    assert!(!settings_file.contains("smtp-user"));
    assert!(!settings_file.contains("private-secret"));
    let credentials_file = config_dir.join("smtp_credentials.json");
    let credentials_json = tokio::fs::read_to_string(&credentials_file).await?;
    assert!(credentials_json.contains("smtp-user"));
    assert!(credentials_json.contains("private-secret"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            tokio::fs::metadata(&credentials_file)
                .await?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    let invalid_settings = client
        .put(format!("{base_url}/api/v1/admin/settings/mail"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "host": "",
            "port": 0,
            "tlsMode": "SMTPS",
            "senderName": "Lux",
            "senderEmail": "noreply@example.test",
            "siteUrl": "https://lux.example.test"
        }))
        .send()
        .await?;
    assert_eq!(invalid_settings.status(), reqwest::StatusCode::BAD_REQUEST);

    let connection_test_started = tokio::time::Instant::now();
    let connection_test = client
        .post(format!("{base_url}/api/v1/admin/settings/mail/test"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .send()
        .await?;
    assert_eq!(connection_test.status(), reqwest::StatusCode::BAD_GATEWAY);
    assert!(connection_test_started.elapsed() < Duration::from_secs(10));
    let connection_error: Value = connection_test.json().await?;
    assert_eq!(connection_error["error"]["message"], "SMTP 连接失败");
    assert_eq!(connection_error["error"]["code"], "INTERNAL");
    let connection_error_text = connection_error.to_string();
    assert!(!connection_error_text.contains("smtp-user"));
    assert!(!connection_error_text.contains("private-secret"));

    let delete_missing_csrf = client
        .delete(format!("{base_url}/api/v1/admin/settings/mail/credentials"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(delete_missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);
    let delete_credentials = client
        .delete(format!("{base_url}/api/v1/admin/settings/mail/credentials"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .send()
        .await?;
    assert_eq!(delete_credentials.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(!tokio::fs::try_exists(credentials_file).await?);

    smtp_stall.abort();
    server.abort();
    Ok(())
}
