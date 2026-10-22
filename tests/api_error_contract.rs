use std::time::Duration;

use luxd::app;
use serde_json::Value;
use tokio::net::TcpListener;

#[tokio::test]
async fn empty_lux_api_service_unavailable_responses_use_the_error_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app()).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?;

    let response = client
        .get(format!("http://{address}/api/v1/auth/sessions"))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await?;
    assert_eq!(body["error"]["code"], "DATABASE_UNAVAILABLE");
    assert!(
        body["error"]["requestId"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );

    let structured = client
        .get(format!("http://{address}/api/v1/auth/me"))
        .send()
        .await?;
    assert_eq!(
        structured.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    let structured_body: Value = structured.json().await?;
    assert_eq!(structured_body["error"]["message"], "服务尚未就绪");

    let emby = client
        .get(format!("http://{address}/System/Info"))
        .send()
        .await?;
    assert_eq!(emby.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert!(emby.bytes().await?.is_empty());

    server.abort();
    Ok(())
}

#[tokio::test]
async fn client_request_ids_are_not_adopted_by_the_server() -> Result<(), Box<dyn std::error::Error>>
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app()).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?;

    let response = client
        .get(format!("http://{address}/api/v1/auth/sessions"))
        .header("x-request-id", "client-supplied-id")
        .send()
        .await?;
    let server_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(!server_id.is_empty());
    assert_ne!(
        server_id, "client-supplied-id",
        "the server must mint its own correlation id"
    );
    let body: Value = response.json().await?;
    assert_eq!(body["error"]["requestId"], server_id.as_str());

    server.abort();
    Ok(())
}

#[tokio::test]
async fn responses_carry_security_headers_and_hsts_only_when_forwarded_https()
-> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app()).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?;

    let plain = client
        .get(format!("http://{address}/api/v1/auth/sessions"))
        .send()
        .await?;
    let headers = plain.headers();
    assert_eq!(
        headers
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
    assert_eq!(
        headers.get("x-frame-options").and_then(|v| v.to_str().ok()),
        Some("DENY")
    );
    assert_eq!(
        headers.get("referrer-policy").and_then(|v| v.to_str().ok()),
        Some("no-referrer")
    );
    assert!(
        headers
            .get("content-security-policy")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|value| value.contains("frame-ancestors 'none'"))
    );
    assert!(
        headers.get("strict-transport-security").is_none(),
        "HSTS must not be sent over plain HTTP"
    );

    let forwarded = client
        .get(format!("http://{address}/api/v1/auth/sessions"))
        .header("x-forwarded-proto", "https")
        .send()
        .await?;
    assert_eq!(
        forwarded
            .headers()
            .get("strict-transport-security")
            .and_then(|v| v.to_str().ok()),
        Some("max-age=31536000; includeSubDomains")
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn request_bursts_from_one_peer_are_rate_limited() -> Result<(), Box<dyn std::error::Error>> {
    use luxd::{
        api::AppState,
        application::setup::SetupService,
        auth::{emby::EmbyAuthService, sessions::WebAuthService},
        config::Config,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let app = luxd::api::app_with_state(
        AppState::ready(
            config,
            database.clone(),
            setup,
            WebAuthService::new(database.clone())?,
            EmbyAuthService::new(database.clone())?,
        )
        .with_request_rate_limit(3),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;

    for _ in 0..3 {
        let allowed = client
            .get(format!("http://{address}/api/v1/version"))
            .send()
            .await?;
        assert_eq!(allowed.status(), reqwest::StatusCode::OK);
    }
    let limited = client
        .get(format!("http://{address}/api/v1/version"))
        .send()
        .await?;
    assert_eq!(limited.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        limited
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok()),
        Some("60")
    );

    server.abort();
    Ok(())
}
