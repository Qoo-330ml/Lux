use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{
        admin_api_key::AdminApiKeyService, emby::EmbyAuthService, sessions::WebAuthService,
        users::UserStore,
    },
    config::Config,
    storage::Database,
};
use tokio::net::TcpListener;

#[tokio::test]
async fn performance_metrics_are_admin_only_and_grouped_by_route_template()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    UserStore::new(database.clone())?
        .create_initial_admin("Admin", "Administrator", "correct horse battery staple")
        .await?;
    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        SetupService::new(database.clone())?,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

    let metrics_url = format!("http://{address}/api/v1/admin/performance-metrics");
    assert_eq!(
        client.get(&metrics_url).send().await?.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    // Any authenticated route is enough to create a template-keyed entry; this one answers 401,
    // which must not count as an error.
    let home_status = client
        .get(format!("http://{address}/api/v1/home"))
        .send()
        .await?
        .status();
    assert!(home_status.is_client_error());

    let response = client
        .get(&metrics_url)
        .header("X-Lux-Api-Key", &key)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.headers()[reqwest::header::CACHE_CONTROL],
        "no-store, max-age=0"
    );
    let body = response.text().await?;
    assert!(!body.contains(&key), "metrics must not echo credentials");
    assert!(!body.contains('?'), "metrics must not echo query strings");
    let parsed: serde_json::Value = serde_json::from_str(&body)?;
    assert!(parsed["totalRequests"].as_u64().unwrap_or_default() >= 1);
    let routes = parsed["routes"].as_array().expect("routes array");
    let home = routes
        .iter()
        .find(|route| route["route"] == "/api/v1/home")
        .expect("home route is tracked by template");
    assert_eq!(home["method"], "GET");
    assert_eq!(home["errors"], 0);
    assert!(home["p95Ms"].is_number());

    server.abort();
    Ok(())
}
