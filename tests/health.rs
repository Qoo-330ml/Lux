use std::time::Duration;

use luxd::app;
use tokio::net::TcpListener;

#[tokio::test]
async fn live_health_endpoint_returns_json_and_request_id() -> Result<(), Box<dyn std::error::Error>>
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app()).await });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let response = client
        .get(format!("http://{address}/health/live"))
        .send()
        .await?;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(response.headers().get("x-request-id").is_some());
    assert_eq!(response.text().await?, r#"{"status":"ok"}"#);

    server.abort();
    Ok(())
}
