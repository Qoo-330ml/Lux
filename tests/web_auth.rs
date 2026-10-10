use std::io::Cursor;

use image::{DynamicImage, ImageFormat};
use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    storage::Database,
};
use reqwest::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

async fn test_server(
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
        .expect("expected cookie in test response")
}

#[tokio::test]
async fn login_me_logout_and_csrf_are_session_backed() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server) = test_server(config).await?;
    let client = reqwest::Client::new();

    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(
            &json!({ "username": "Admin", "displayName": "Admin", "password": "correct password" }),
        )
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "ADMIN", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session_cookie = cookie_value(login.headers(), "lux_session");
    let csrf_cookie = cookie_value(login.headers(), "lux_csrf");
    let session_set_cookie = login
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("lux_session="))
        .ok_or("missing session set-cookie")?;
    assert!(session_set_cookie.contains("HttpOnly"));
    assert!(!session_set_cookie.contains("Secure"));
    assert!(session_set_cookie.contains("SameSite=Lax"));
    let cookie_header = format!("lux_session={session_cookie}; lux_csrf={csrf_cookie}");

    let second_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(second_login.status(), reqwest::StatusCode::OK);
    let sessions = client
        .get(format!("{base_url}/api/v1/auth/sessions"))
        .header(COOKIE, &cookie_header)
        .send()
        .await?;
    assert_eq!(sessions.status(), reqwest::StatusCode::OK);
    let sessions_body = sessions.json::<serde_json::Value>().await?;
    assert_eq!(sessions_body["sessions"].as_array().map(Vec::len), Some(2));
    let other_session_id = sessions_body["sessions"]
        .as_array()
        .and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["isCurrent"] == false)
        })
        .and_then(|session| session["id"].as_str())
        .ok_or("missing other session")?;
    let revoke_other = client
        .delete(format!(
            "{base_url}/api/v1/auth/sessions/{other_session_id}"
        ))
        .header(COOKIE, &cookie_header)
        .header("x-csrf-token", &csrf_cookie)
        .send()
        .await?;
    assert_eq!(revoke_other.status(), reqwest::StatusCode::NO_CONTENT);
    let remaining = client
        .get(format!("{base_url}/api/v1/auth/sessions"))
        .header(COOKIE, &cookie_header)
        .send()
        .await?;
    assert_eq!(
        remaining.json::<serde_json::Value>().await?["sessions"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    let me = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &cookie_header)
        .send()
        .await?;
    assert_eq!(me.status(), reqwest::StatusCode::OK);
    let me_body = me.json::<serde_json::Value>().await?;
    assert_eq!(me_body["user"]["isAdmin"], true);
    assert_eq!(me_body["serverName"], "Lux Server");

    let missing_csrf = client
        .post(format!("{base_url}/api/v1/auth/logout"))
        .header(COOKIE, &cookie_header)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let wrong_csrf = client
        .post(format!("{base_url}/api/v1/auth/logout"))
        .header(COOKIE, &cookie_header)
        .header("x-csrf-token", "wrong")
        .send()
        .await?;
    assert_eq!(wrong_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let logout = client
        .post(format!("{base_url}/api/v1/auth/logout"))
        .header(COOKIE, &cookie_header)
        .header("x-csrf-token", &csrf_cookie)
        .send()
        .await?;
    assert_eq!(logout.status(), reqwest::StatusCode::NO_CONTENT);

    let me_after_logout = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &cookie_header)
        .send()
        .await?;
    assert_eq!(me_after_logout.status(), reqwest::StatusCode::UNAUTHORIZED);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn expired_web_sessions_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let auth = WebAuthService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;

    let session = auth
        .login("admin", "correct password")
        .await?
        .ok_or("expected session")?;
    let session_hash = Sha256::digest(session.session_token.as_bytes()).to_vec();
    sqlx::query(
        "UPDATE web_sessions SET expires_at = unixepoch() - 1
         WHERE session_token_hash = ?",
    )
    .bind(session_hash)
    .execute(database.pool())
    .await?;

    assert!(auth.resolve(&session.session_token).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn authenticated_user_can_change_own_password() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server) = test_server(config).await?;
    let client = reqwest::Client::new();

    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Admin",
            "password": "admin password"
        }))
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "admin password" }))
        .send()
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_session = cookie_value(admin_login.headers(), "lux_session");
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    let admin_cookie = format!("lux_session={admin_session}; lux_csrf={admin_csrf}");

    let create_user = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "username": "regular",
            "displayName": "Regular",
            "password": "old password"
        }))
        .send()
        .await?;
    assert_eq!(create_user.status(), reqwest::StatusCode::CREATED);

    let regular_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "regular", "password": "old password" }))
        .send()
        .await?;
    assert_eq!(regular_login.status(), reqwest::StatusCode::OK);
    let regular_session = cookie_value(regular_login.headers(), "lux_session");
    let regular_csrf = cookie_value(regular_login.headers(), "lux_csrf");
    let regular_cookie = format!("lux_session={regular_session}; lux_csrf={regular_csrf}");

    let missing_csrf = client
        .patch(format!("{base_url}/api/v1/auth/password"))
        .header(COOKIE, &regular_cookie)
        .json(&json!({
            "currentPassword": "old password",
            "newPassword": "new password"
        }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let wrong_current = client
        .patch(format!("{base_url}/api/v1/auth/password"))
        .header(COOKIE, &regular_cookie)
        .header("x-csrf-token", &regular_csrf)
        .json(&json!({
            "currentPassword": "wrong password",
            "newPassword": "new password"
        }))
        .send()
        .await?;
    assert_eq!(wrong_current.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(
        wrong_current.json::<serde_json::Value>().await?["error"]["code"],
        "INVALID_CREDENTIALS"
    );

    let empty_new_password = client
        .patch(format!("{base_url}/api/v1/auth/password"))
        .header(COOKIE, &regular_cookie)
        .header("x-csrf-token", &regular_csrf)
        .json(&json!({
            "currentPassword": "old password",
            "newPassword": ""
        }))
        .send()
        .await?;
    assert_eq!(
        empty_new_password.status(),
        reqwest::StatusCode::BAD_REQUEST
    );

    let changed = client
        .patch(format!("{base_url}/api/v1/auth/password"))
        .header(COOKIE, &regular_cookie)
        .header("x-csrf-token", &regular_csrf)
        .json(&json!({
            "currentPassword": "old password",
            "newPassword": "new password"
        }))
        .send()
        .await?;
    assert_eq!(changed.status(), reqwest::StatusCode::NO_CONTENT);

    let current_session = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &regular_cookie)
        .send()
        .await?;
    assert_eq!(current_session.status(), reqwest::StatusCode::OK);

    let old_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "regular", "password": "old password" }))
        .send()
        .await?;
    assert_eq!(old_login.status(), reqwest::StatusCode::UNAUTHORIZED);

    let new_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "regular", "password": "new password" }))
        .send()
        .await?;
    assert_eq!(new_login.status(), reqwest::StatusCode::OK);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn avatar_upload_requires_csrf_and_survives_a_second_login()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server) = test_server(config).await?;
    let client = reqwest::Client::new();
    let avatar = png_fixture()?;

    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(
            &json!({ "username": "Admin", "displayName": "Admin", "password": "correct password" }),
        )
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let session_cookie = cookie_value(login.headers(), "lux_session");
    let csrf_cookie = cookie_value(login.headers(), "lux_csrf");
    let cookie_header = format!("lux_session={session_cookie}; lux_csrf={csrf_cookie}");

    let no_avatar = client
        .get(format!("{base_url}/api/v1/auth/avatar"))
        .header(COOKIE, &cookie_header)
        .send()
        .await?;
    assert_eq!(no_avatar.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        no_avatar
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("private, no-cache")
    );

    let missing_csrf = client
        .put(format!("{base_url}/api/v1/auth/avatar"))
        .header(COOKIE, &cookie_header)
        .header(CONTENT_TYPE, "image/png")
        .body(avatar.clone())
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let upload = client
        .put(format!("{base_url}/api/v1/auth/avatar"))
        .header(COOKIE, &cookie_header)
        .header("x-csrf-token", &csrf_cookie)
        .header(CONTENT_TYPE, "image/png")
        .body(avatar.clone())
        .send()
        .await?;
    assert_eq!(upload.status(), reqwest::StatusCode::OK);

    let first_read = client
        .get(format!("{base_url}/api/v1/auth/avatar"))
        .header(COOKIE, &cookie_header)
        .send()
        .await?;
    assert_eq!(first_read.status(), reqwest::StatusCode::OK);
    assert_eq!(
        first_read
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("image/png")
    );
    assert_eq!(first_read.bytes().await?.as_ref(), avatar.as_slice());

    let second_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let second_cookie = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(second_login.headers(), "lux_session"),
        cookie_value(second_login.headers(), "lux_csrf")
    );
    let second_read = client
        .get(format!("{base_url}/api/v1/auth/avatar"))
        .header(COOKIE, second_cookie)
        .send()
        .await?;
    assert_eq!(second_read.status(), reqwest::StatusCode::OK);
    assert_eq!(second_read.bytes().await?.as_ref(), avatar.as_slice());

    server.abort();
    Ok(())
}

fn png_fixture() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut output = Cursor::new(Vec::new());
    DynamicImage::new_rgba8(1, 1).write_to(&mut output, ImageFormat::Png)?;
    Ok(output.into_inner())
}
