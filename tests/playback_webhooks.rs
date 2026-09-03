use axum::{Router, body::Bytes, extract::State, response::IntoResponse, routing::post};
use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        scanner::LibraryScanner,
        setup::SetupService,
        webhooks::{WebhookEventType, WebhookService},
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::mpsc};

fn emby_public_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
}

#[tokio::test]
async fn playback_webhooks_emit_edges_and_throttled_progress()
-> Result<(), Box<dyn std::error::Error>> {
    let (sender, mut receiver) = mpsc::channel::<Bytes>(8);
    let receiver_app = Router::new()
        .route(
            "/hook",
            post(
                |State(sender): State<mpsc::Sender<Bytes>>, body: Bytes| async move {
                    let _ = sender.send(body).await;
                    ().into_response()
                },
            ),
        )
        .with_state(sender);
    let receiver_listener = TcpListener::bind("127.0.0.1:0").await?;
    let receiver_address = receiver_listener.local_addr()?;
    let receiver_server =
        tokio::spawn(async move { axum::serve(receiver_listener, receiver_app).await });

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Playback Hook Movie 2024.mkv"), b"video").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE'")
            .fetch_one(database.pool())
            .await?;
    let emby_item_id = emby_public_id(&item_id);

    let webhook_service = WebhookService::new(database.clone(), temp_dir.path().join("config"))?;
    webhook_service
        .create_destination(
            "Playback receiver",
            &format!("http://{receiver_address}/hook"),
            true,
            true,
            &[
                "PLAYBACK_STARTED".to_owned(),
                "PLAYBACK_PAUSED".to_owned(),
                "PLAYBACK_PROGRESS".to_owned(),
                "PLAYBACK_STOPPED".to_owned(),
            ],
            Some("playback-webhook-secret"),
        )
        .await?;

    let app_state = AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server =
        tokio::spawn(async move { axum::serve(listener, app_with_state(app_state)).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="PlaybackHookTest", Device="Mac", DeviceId="playback-hook-device", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing admin token")?
        .to_owned();

    let common = json!({
        "ItemId": emby_item_id,
        "PlaySessionId": "playback-hook-session",
        "PositionTicks": 100,
        "RunTimeTicks": 10_000,
        "Client": "PlaybackHookTest",
        "DeviceName": "Mac",
        "DeviceId": "playback-hook-device",
        "ApplicationVersion": "1",
        "DeviceType": "Desktop"
    });
    let playing = client
        .post(format!("{base_url}/Sessions/Playing"))
        .header("X-Emby-Token", &token)
        .json(&common)
        .send()
        .await?;
    assert_eq!(playing.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(webhook_service.process_ready_deliveries().await?, 1);
    let started: Value =
        serde_json::from_slice(&receiver.recv().await.ok_or("missing started event")?)?;
    assert_eq!(started["eventType"], "PLAYBACK_STARTED");
    assert_eq!(started["itemId"], emby_item_id);
    assert!(started.get("userId").is_none());

    sqlx::query(
        "UPDATE playback_sessions SET last_event_at = last_event_at - 31
         WHERE play_session_id = ?",
    )
    .bind("playback-hook-session")
    .execute(database.pool())
    .await?;
    let progress = client
        .post(format!("{base_url}/Sessions/Playing/Progress"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": common["ItemId"],
            "PlaySessionId": common["PlaySessionId"],
            "PositionTicks": 500,
            "RunTimeTicks": 10_000,
            "Client": "PlaybackHookTest",
            "DeviceId": "playback-hook-device"
        }))
        .send()
        .await?;
    assert_eq!(progress.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(webhook_service.process_ready_deliveries().await?, 1);
    let progress_event: Value =
        serde_json::from_slice(&receiver.recv().await.ok_or("missing progress event")?)?;
    assert_eq!(progress_event["eventType"], "PLAYBACK_PROGRESS");
    assert_eq!(progress_event["positionTicks"], 500);

    let repeated = client
        .post(format!("{base_url}/Sessions/Playing/Progress"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": common["ItemId"],
            "PlaySessionId": common["PlaySessionId"],
            "PositionTicks": 600,
            "RunTimeTicks": 10_000
        }))
        .send()
        .await?;
    assert_eq!(repeated.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(webhook_service.process_ready_deliveries().await?, 0);

    let stopped = client
        .post(format!("{base_url}/Sessions/Playing/Stopped"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": common["ItemId"],
            "PlaySessionId": common["PlaySessionId"],
            "PositionTicks": 600
        }))
        .send()
        .await?;
    assert_eq!(stopped.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(webhook_service.process_ready_deliveries().await?, 1);
    let stopped_event: Value =
        serde_json::from_slice(&receiver.recv().await.ok_or("missing stopped event")?)?;
    assert_eq!(stopped_event["eventType"], "PLAYBACK_STOPPED");
    assert_eq!(stopped_event["itemId"], emby_item_id);

    let event_types: Vec<String> =
        sqlx::query_scalar("SELECT event_type FROM notification_events ORDER BY occurred_at, id")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(
        event_types,
        vec![
            WebhookEventType::PlaybackStarted.as_str().to_owned(),
            WebhookEventType::PlaybackProgress.as_str().to_owned(),
            WebhookEventType::PlaybackStopped.as_str().to_owned(),
        ]
    );

    server.abort();
    receiver_server.abort();
    Ok(())
}
