use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::json;
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
    assert!(session_set_cookie.contains("Secure"));
    assert!(session_set_cookie.contains("SameSite=Lax"));
    let cookie_header = format!("lux_session={session_cookie}; lux_csrf={csrf_cookie}");

    let me = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &cookie_header)
        .send()
        .await?;
    assert_eq!(me.status(), reqwest::StatusCode::OK);
    assert_eq!(
        me.json::<serde_json::Value>().await?["user"]["isAdmin"],
        true
    );

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
