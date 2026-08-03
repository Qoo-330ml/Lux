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
    let index_body = index.text().await?;
    let vite_assets = index_body.contains("id=\"root\"");
    assert!(vite_assets || index_body.contains("id=\"app\""));
    let client_route = client
        .get(format!("{base_url}/libraries/example"))
        .send()
        .await?;
    assert_eq!(client_route.status(), StatusCode::OK);
    assert!(client_route.text().await?.contains(if vite_assets {
        "id=\"root\""
    } else {
        "id=\"app\""
    }));
    let logo = client.get(format!("{base_url}/logo.svg")).send().await?;
    assert_eq!(logo.status(), StatusCode::OK);
    assert_eq!(
        logo.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("image/svg+xml")
    );
    assert!(logo.text().await?.contains("<svg"));
    if vite_assets {
        let script = client
            .get(format!("{base_url}/assets/lux.js"))
            .send()
            .await?;
        assert_eq!(script.status(), StatusCode::OK);
        assert!(script.text().await?.contains("/api/v1/auth/login"));
        let styles = client
            .get(format!("{base_url}/assets/index.css"))
            .send()
            .await?;
        assert_eq!(styles.status(), StatusCode::OK);
        assert!(styles.text().await?.contains("--lux-bg"));
    } else {
        let script = client.get(format!("{base_url}/app.mjs")).send().await?;
        assert_eq!(script.status(), StatusCode::OK);
        let script_body = script.text().await?;
        assert!(script_body.contains("/api/v1/auth/login"));
        assert!(script_body.contains("request-options.mjs"));
        let admin_navigation = client
            .get(format!("{base_url}/admin-navigation.mjs"))
            .send()
            .await?;
        assert_eq!(admin_navigation.status(), StatusCode::OK);
        assert!(admin_navigation.text().await?.contains("ADMIN_NAV_ITEMS"));
        let request_options = client
            .get(format!("{base_url}/request-options.mjs"))
            .send()
            .await?;
        assert_eq!(request_options.status(), StatusCode::OK);
        assert!(request_options.text().await?.contains("Content-Type"));
        let styles = client.get(format!("{base_url}/styles.css")).send().await?;
        assert_eq!(styles.status(), StatusCode::OK);
        let styles_body = styles.text().await?;
        assert!(styles_body.contains("--accent"));
        assert!(styles_body.contains(".brand-logo"));
    }

    server.abort();
    let _ = app_with_state(AppState::default());
    Ok(())
}
