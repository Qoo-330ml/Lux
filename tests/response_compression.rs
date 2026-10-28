//! Response compression contract (W5): JSON bodies may be gzip-encoded when the client asks for it,
//! everything else (media streams, images, the SPA shell) must stay identity-encoded.

use luxd::{
    api::{AppState, app_with_state},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    application::setup::SetupService,
    storage::Database,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn json_responses_are_gzipped_only_when_requested()
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
    let client = reqwest::Client::builder().no_proxy().build()?;

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let session = login
        .headers()
        .get_all("set-cookie")
        .iter()
        .find_map(|value| {
            let value = value.to_str().ok()?;
            value.strip_prefix("lux_session=")?.split(';').next()
        })
        .ok_or("missing session")?
        .to_owned();

    let compressed = client
        .get(format!("{base_url}/api/v1/home"))
        .header("Cookie", format!("lux_session={session}"))
        .header("Accept-Encoding", "gzip")
        .send()
        .await?;
    assert_eq!(compressed.status(), reqwest::StatusCode::OK);
    assert_eq!(
        compressed
            .headers()
            .get("content-encoding")
            .and_then(|value| value.to_str().ok()),
        Some("gzip"),
        "JSON responses must be gzip encoded when the client accepts it"
    );
    assert!(
        compressed.headers().contains_key("vary"),
        "a negotiated encoding must be advertised through Vary"
    );

    let identity = client
        .get(format!("{base_url}/api/v1/home"))
        .header("Cookie", format!("lux_session={session}"))
        .send()
        .await?;
    assert_eq!(identity.status(), reqwest::StatusCode::OK);
    assert!(
        identity.headers().get("content-encoding").is_none(),
        "without Accept-Encoding the body must stay identity encoded"
    );
    let identity_body = identity.bytes().await?;
    let identity_json: Value = serde_json::from_slice(&identity_body)?;
    assert!(identity_json["libraries"].is_array());

    // Non-JSON routes keep their original encoding even when the client offers gzip.
    let logo = client
        .get(format!("{base_url}/logo.svg"))
        .header("Accept-Encoding", "gzip")
        .send()
        .await?;
    assert_eq!(logo.status(), reqwest::StatusCode::OK);
    let content_type = logo
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(!content_type.starts_with("application/json"));
    assert!(
        logo.headers().get("content-encoding").is_none(),
        "static assets are served untouched ({content_type})"
    );

    server.abort();
    Ok(())
}
