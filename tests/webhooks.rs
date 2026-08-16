use luxd::application::webhooks::{
    WebhookEventType, WebhookUrlError, canonical_signature, validate_webhook_url,
};

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
