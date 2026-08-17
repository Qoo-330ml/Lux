use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::SystemTime,
};

use axum::{
    Router,
    http::{HeaderValue, StatusCode},
    response::IntoResponse,
    routing::post,
};
use luxd::{
    application::webhooks::{
        WebhookEventType, WebhookService, WebhookUrlError, canonical_signature,
        validate_webhook_url,
    },
    config::Config,
    storage::Database,
};
use serde_json::json;
use tokio::net::TcpListener;

#[test]
fn webhook_event_types_have_stable_wire_names() {
    assert_eq!(WebhookEventType::MediaAdded.as_str(), "MEDIA_ADDED");
    assert_eq!(WebhookEventType::ScanFailed.as_str(), "SCAN_FAILED");
    assert!(WebhookEventType::from_wire_name("JOB_FAILED").is_some());
    assert!(WebhookEventType::from_wire_name("UNKNOWN").is_none());
}

#[test]
fn webhook_url_rejects_credentials_queries_and_unsupported_schemes() {
    assert!(matches!(
        validate_webhook_url("https://user:pass@example.com/hooks", false),
        Err(WebhookUrlError::Credentials)
    ));
    assert!(matches!(
        validate_webhook_url("https://example.com/hooks?token=secret", false),
        Err(WebhookUrlError::QueryOrFragment)
    ));
    assert!(matches!(
        validate_webhook_url("ftp://example.com/hooks", false),
        Err(WebhookUrlError::Scheme)
    ));
}

#[test]
fn webhook_url_requires_explicit_private_network_opt_in() {
    assert!(matches!(
        validate_webhook_url("http://127.0.0.1:8080/hooks", false),
        Err(WebhookUrlError::PrivateNetwork)
    ));
    assert!(validate_webhook_url("http://127.0.0.1:8080/hooks", true).is_ok());
    assert!(matches!(
        validate_webhook_url("http://[::ffff:192.168.1.2]:8080/hooks", false),
        Err(WebhookUrlError::PrivateNetwork)
    ));
}

#[test]
fn webhook_signature_is_timestamped_hmac_sha256() {
    assert_eq!(
        canonical_signature("key", "", b"The quick brown fox jumps over the lazy dog"),
        "sha256=ef8bbd77eec96191917c8b888cda1257cbea0d84b48b39264422c355ce8f0286"
    );
}

#[tokio::test]
async fn webhook_delivery_records_retry_after_and_permanent_http_failures()
-> Result<(), Box<dyn std::error::Error>> {
    let attempts = Arc::new(AtomicUsize::new(0));
    let receiver_app = Router::new().route(
        "/hook",
        post({
            let attempts = attempts.clone();
            move || {
                let attempts = attempts.clone();
                async move {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        let mut response = StatusCode::TOO_MANY_REQUESTS.into_response();
                        response
                            .headers_mut()
                            .insert("Retry-After", HeaderValue::from_static("120"));
                        response
                    } else {
                        StatusCode::BAD_REQUEST.into_response()
                    }
                }
            }
        }),
    );
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
    let service = WebhookService::new(database.clone(), config.config_dir.clone())?;
    service
        .create_destination(
            "Retry receiver",
            &format!("http://{receiver_address}/hook"),
            true,
            true,
            &[],
            Some("webhook-test-secret-1234"),
        )
        .await?;
    service
        .publish(
            WebhookEventType::ScanCompleted,
            "scan:retry-test",
            1_700_000_000,
            json!({"libraryId": "library-1"}),
        )
        .await?;

    let before = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_secs();
    assert_eq!(service.process_ready_deliveries().await?, 1);
    let first: (String, i64, Option<i64>, i64, Option<String>) = sqlx::query_as(
        "SELECT status, attempt_count, last_http_status, next_attempt_at, last_error
         FROM notification_deliveries",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(first.0, "PENDING");
    assert_eq!(first.1, 1);
    assert_eq!(first.2, Some(429));
    assert!(first.3 >= i64::try_from(before)?.saturating_add(100));
    assert!(
        first
            .4
            .as_deref()
            .is_some_and(|value| value.contains("429"))
    );

    sqlx::query("UPDATE notification_deliveries SET next_attempt_at = unixepoch()")
        .execute(database.pool())
        .await?;
    assert_eq!(service.process_ready_deliveries().await?, 1);
    let second: (String, i64, Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT status, attempt_count, last_http_status, last_error
         FROM notification_deliveries",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(second.0, "FAILED");
    assert_eq!(second.1, 2);
    assert_eq!(second.2, Some(400));
    assert!(
        second
            .3
            .as_deref()
            .is_some_and(|value| value.contains("400"))
    );

    receiver_server.abort();
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn expired_webhook_delivery_lease_is_reclaimed() -> Result<(), Box<dyn std::error::Error>> {
    let receiver_app = axum::Router::new().route(
        "/hook",
        axum::routing::post(|| async { StatusCode::NO_CONTENT }),
    );
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
    let service = WebhookService::new(database.clone(), config.config_dir.clone())?;
    service
        .create_destination(
            "Lease receiver",
            &format!("http://{receiver_address}/hook"),
            true,
            true,
            &[],
            Some("webhook-test-secret-1234"),
        )
        .await?;
    service
        .publish(
            WebhookEventType::ScanCompleted,
            "scan:lease-test",
            1_700_000_000,
            json!({"libraryId": "library-1"}),
        )
        .await?;
    sqlx::query(
        "UPDATE notification_deliveries
         SET status = 'RUNNING', attempt_count = 1,
             claimed_until = unixepoch() - 1, next_attempt_at = unixepoch()",
    )
    .execute(database.pool())
    .await?;

    assert_eq!(service.process_ready_deliveries().await?, 1);
    let result: (String, i64) =
        sqlx::query_as("SELECT status, attempt_count FROM notification_deliveries")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(result, ("DELIVERED".to_owned(), 2));

    receiver_server.abort();
    database.close().await;
    Ok(())
}
