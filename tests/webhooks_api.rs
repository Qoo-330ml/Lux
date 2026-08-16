use axum::{
    Router, body::Bytes, extract::State, http::HeaderMap, response::IntoResponse, routing::post,
};
use luxd::{
    api::{AppState, app_with_state},
    application::{
        setup::SetupService,
        webhooks::{WebhookEventType, WebhookService},
    },
    auth::emby::EmbyAuthService,
    auth::sessions::WebAuthService,
    config::Config,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::mpsc};

#[derive(Debug)]
struct ReceivedWebhook {
    body: Bytes,
    signature: Option<String>,
}

fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> String {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _) = value.split_once(';')?;
            let (cookie_name, cookie_value) = pair.split_once('=')?;
            (cookie_name == name).then(|| cookie_value.to_owned())
        })
        .expect("expected cookie")
}

#[tokio::test]
async fn webhook_destination_api_publishes_signed_events_and_hides_secret()
-> Result<(), Box<dyn std::error::Error>> {
    let (received_sender, mut received_receiver) = mpsc::channel::<ReceivedWebhook>(1);
    let receiver_app = Router::new()
        .route(
            "/hook",
            post(
                |State(sender): State<mpsc::Sender<ReceivedWebhook>>,
                 headers: HeaderMap,
                 body: Bytes| async move {
                    let signature = headers
                        .get("X-Lux-Signature")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned);
                    let _ = sender.send(ReceivedWebhook { body, signature }).await;
                    ().into_response()
                },
            ),
        )
        .with_state(received_sender);
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
    setup
        .complete("Admin", "Administrator", "correct password")
        .await?;
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let lux_listener = TcpListener::bind("127.0.0.1:0").await?;
    let lux_address = lux_listener.local_addr()?;
    let lux_server = tokio::spawn(async move { axum::serve(lux_listener, app).await });
    let client = reqwest::Client::new();

    let login = client
        .post(format!("http://{lux_address}/api/v1/auth/login"))
        .json(&json!({
            "username": "admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(login.headers(), "lux_session"),
        cookie_value(login.headers(), "lux_csrf")
    );
    let csrf = cookie_value(login.headers(), "lux_csrf");
    let secret = "webhook-test-secret-1234";

    let missing_csrf = client
        .post(format!(
            "http://{lux_address}/api/v1/admin/notification-destinations"
        ))
        .header(COOKIE, &cookies)
        .json(&json!({
            "name": "CSRF receiver",
            "url": "https://example.com/lux-hook",
            "secret": secret
        }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let created = client
        .post(format!(
            "http://{lux_address}/api/v1/admin/notification-destinations"
        ))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "name": "Local receiver",
            "url": format!("http://{receiver_address}/hook"),
            "allowPrivateNetwork": true,
            "eventTypes": ["SCAN_COMPLETED"],
            "secret": secret
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let created_body: Value = created.json().await?;
    let destination_id = created_body["destination"]["id"]
        .as_str()
        .ok_or("missing destination ID")?
        .to_owned();
    assert_eq!(created_body["secret"], secret);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = tokio::fs::metadata(config.config_dir.join("lux_webhook_secrets.json"))
            .await?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    let listed = client
        .get(format!(
            "http://{lux_address}/api/v1/admin/notification-destinations"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let listed_body: Value = listed.json().await?;
    assert_eq!(listed_body["destinations"][0]["secretConfigured"], true);
    assert!(!listed_body.to_string().contains(secret));

    let regular = client
        .post(format!("http://{lux_address}/api/v1/admin/users"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "username": "webhook-viewer",
            "password": "webhook-viewer-password"
        }))
        .send()
        .await?;
    assert_eq!(regular.status(), reqwest::StatusCode::CREATED);
    let regular_login = client
        .post(format!("http://{lux_address}/api/v1/auth/login"))
        .json(&json!({
            "username": "webhook-viewer",
            "password": "webhook-viewer-password"
        }))
        .send()
        .await?;
    assert_eq!(regular_login.status(), reqwest::StatusCode::OK);
    let regular_cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(regular_login.headers(), "lux_session"),
        cookie_value(regular_login.headers(), "lux_csrf")
    );
    let denied = client
        .get(format!(
            "http://{lux_address}/api/v1/admin/notification-destinations"
        ))
        .header(COOKIE, regular_cookies)
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);

    let service = WebhookService::new(database.clone(), config.config_dir.clone())?;
    let event_id = service
        .publish(
            WebhookEventType::ScanCompleted,
            "scan:test:completed",
            1_700_000_000,
            json!({ "libraryId": "library-1", "processed": 3 }),
        )
        .await?
        .ok_or("event was not inserted")?;
    assert!(event_id.len() > 10);
    assert_eq!(service.process_ready_deliveries().await?, 1);

    let received = received_receiver
        .recv()
        .await
        .ok_or("missing webhook request")?;
    let received_body: Value = serde_json::from_slice(&received.body)?;
    assert_eq!(received_body["eventType"], "SCAN_COMPLETED");
    assert_eq!(received_body["eventId"], event_id);
    assert!(
        received
            .signature
            .as_deref()
            .is_some_and(|value| value.starts_with("sha256="))
    );

    let deliveries = client
        .get(format!(
            "http://{lux_address}/api/v1/admin/notification-deliveries"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(deliveries.status(), reqwest::StatusCode::OK);
    let deliveries_body: Value = deliveries.json().await?;
    assert_eq!(deliveries_body["deliveries"][0]["status"], "DELIVERED");

    let updated = client
        .patch(format!(
            "http://{lux_address}/api/v1/admin/notification-destinations/{destination_id}"
        ))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "name": "Updated receiver",
            "enabled": false,
            "eventTypes": []
        }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    assert_eq!(
        updated.json::<Value>().await?["destination"]["enabled"],
        false
    );
    let fetched = client
        .get(format!(
            "http://{lux_address}/api/v1/admin/notification-destinations/{destination_id}"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(fetched.status(), reqwest::StatusCode::OK);
    assert_eq!(
        fetched.json::<Value>().await?["destination"]["name"],
        "Updated receiver"
    );

    let rotated = client
        .post(format!(
            "http://{lux_address}/api/v1/admin/notification-destinations/{destination_id}/rotate-secret"
        ))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .send()
        .await?;
    assert_eq!(rotated.status(), reqwest::StatusCode::OK);
    let rotated_body: Value = rotated.json().await?;
    assert_ne!(rotated_body["secret"], secret);

    let deleted = client
        .delete(format!(
            "http://{lux_address}/api/v1/admin/notification-destinations/{destination_id}"
        ))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .send()
        .await?;
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);

    lux_server.abort();
    receiver_server.abort();
    database.close().await;
    Ok(())
}
