use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::Value;
use tokio::net::TcpListener;

#[tokio::test]
async fn cron_can_enqueue_an_enabled_library_reconciliation_without_csrf()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    sqlx::query(
        "UPDATE scheduled_task_configs
         SET is_enabled = 1
         WHERE owner_type = 'LIBRARY' AND owner_id = ? AND task_type = 'RECONCILIATION_SCAN'",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    let setup = SetupService::new(database.clone())?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(
        AppState::ready(config, database.clone(), setup, auth, emby_auth)
            .with_cron_token("cron-token-for-test"),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::new();
    let endpoint = format!(
        "http://{address}/api/v1/cron/tasks/LIBRARY/{}/RECONCILIATION_SCAN",
        library.id
    );

    let unauthorized = client.post(&endpoint).send().await?;
    assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

    let accepted = client
        .post(&endpoint)
        .header(AUTHORIZATION, "Bearer cron-token-for-test")
        .send()
        .await?;
    assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);
    let body: Value = accepted.json().await?;
    assert_eq!(body["taskType"], "RECONCILIATION_SCAN");
    let job_id = body["job"]["id"].as_str().ok_or("missing scan job id")?;
    sqlx::query("UPDATE scan_jobs SET status = 'RUNNING' WHERE id = ?")
        .bind(job_id)
        .execute(database.pool())
        .await?;

    let duplicate = client
        .post(&endpoint)
        .header(AUTHORIZATION, "Bearer cron-token-for-test")
        .send()
        .await?;
    assert_eq!(duplicate.status(), reqwest::StatusCode::CONFLICT);

    server.abort();
    Ok(())
}
