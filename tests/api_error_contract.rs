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
