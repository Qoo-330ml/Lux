use luxd::api::{AppState, app, app_with_state};
use reqwest::StatusCode;
use tokio::net::TcpListener;

#[tokio::test]
async fn same_origin_web_assets_are_served_by_rust() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app()).await });
    let client = reqwest::Client::new();
    let base_url = format!("http://{address}");

    let index = client.get(format!("{base_url}/")).send().await?;
    assert_eq!(index.status(), StatusCode::OK);
    assert!(index.text().await?.contains("id=\"app\""));
    let script = client.get(format!("{base_url}/app.mjs")).send().await?;
    assert_eq!(script.status(), StatusCode::OK);
    assert!(script.text().await?.contains("/api/v1/auth/login"));
    let styles = client.get(format!("{base_url}/styles.css")).send().await?;
    assert_eq!(styles.status(), StatusCode::OK);
    assert!(styles.text().await?.contains("--accent"));

    server.abort();
    let _ = app_with_state(AppState::default());
    Ok(())
}
