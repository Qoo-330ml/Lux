use luxd::api::{AppState, app, app_with_state};
use reqwest::StatusCode;
use tokio::net::TcpListener;

#[tokio::test]
async fn same_origin_web_assets_are_served_by_rust() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app()).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
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
    let lux_missing = client
        .get(format!("{base_url}/api/v1/not-implemented"))
        .send()
        .await?;
    assert_eq!(lux_missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        lux_missing
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next()),
        Some("application/json")
    );
    assert!(lux_missing.json::<serde_json::Value>().await?.is_object());
    // Emby clients expect a bare status; only the Lux API uses the JSON error envelope.
    for path in ["/emby/not-implemented", "/Sessions/not-implemented"] {
        let response = client.get(format!("{base_url}{path}")).send().await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .is_none(),
            "{path} must not advertise JSON"
        );
        assert!(response.bytes().await?.is_empty(), "{path} has a body");
    }
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
        // Vite emits content-hashed entry names, so read them from the served index instead of
        // assuming a fixed filename.
        let script_path = index_body
            .split("src=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .filter(|path| path.ends_with(".js"))
            .ok_or("entry script missing from index.html")?;
        let script = client
            .get(format!("{base_url}{script_path}"))
            .send()
            .await?;
        assert_eq!(script.status(), StatusCode::OK, "{script_path}");
        assert!(script.text().await?.contains("/api/v1/auth/login"));
        let style_path = index_body
            .split("href=\"")
            .filter_map(|rest| rest.split('"').next())
            .find(|path| path.ends_with(".css"))
            .ok_or("entry stylesheet missing from index.html")?;
        let styles = client.get(format!("{base_url}{style_path}")).send().await?;
        assert_eq!(styles.status(), StatusCode::OK, "{style_path}");
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

#[tokio::test]
async fn media_detail_navigation_is_served_as_html_while_json_uses_emby()
-> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app()).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let base_url = format!("http://{address}");

    let media_detail_page = client
        .get(format!("{base_url}/items/example-item"))
        .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml")
        .send()
        .await?;
    assert_eq!(media_detail_page.status(), StatusCode::OK);
    assert!(
        media_detail_page
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/html"))
    );

    let emby_item_api = client
        .get(format!("{base_url}/items/example-item"))
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await?;
    assert_ne!(emby_item_api.status(), StatusCode::OK);
    assert!(
        !emby_item_api
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/html"))
    );

    server.abort();
    Ok(())
}
