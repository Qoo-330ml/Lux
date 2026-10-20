use std::{net::SocketAddr, time::Duration};

use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _) = value.split_once(';')?;
            let (cookie_name, cookie_value) = pair.split_once('=')?;
            (cookie_name == name).then(|| cookie_value.to_owned())
        })
}

#[tokio::test]
async fn admin_invitation_api_enforces_auth_csrf_and_redacts_sensitive_fields()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address: SocketAddr = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .no_proxy()
        .build()?;
    let collection_url = format!("{base_url}/api/v1/admin/user-invitations");

    let unauthenticated = client.get(&collection_url).send().await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session = cookie_value(login.headers(), "lux_session").ok_or("missing session cookie")?;
    let csrf = cookie_value(login.headers(), "lux_csrf").ok_or("missing CSRF cookie")?;
    let cookies = format!("lux_session={session}; lux_csrf={csrf}");
    let request_body = json!({
        "recipientEmail": " Recipient@Example.Test ",
        "presetId": "builtin-user"
    });

    let missing_csrf = client
        .post(&collection_url)
        .header(COOKIE, &cookies)
        .json(&request_body)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);
    let no_side_effects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_invitations")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(no_side_effects, 0);

    let created = client
        .post(&collection_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&request_body)
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let created_text = created.text().await?;
    let created: Value = serde_json::from_str(&created_text)?;
    assert_eq!(
        created["invitation"]["recipientEmail"],
        "recipient@example.test"
    );
    assert_eq!(created["invitation"]["status"], "PENDING");
    assert!(
        created["invitation"]
            .get("permissionSnapshotJson")
            .is_none()
    );
    assert!(created["invitation"].get("tokenHash").is_none());
    let invitation_id = created["invitation"]["id"]
        .as_str()
        .ok_or("missing invitation ID")?;
    let original_expires_at = created["invitation"]["expiresAt"]
        .as_i64()
        .ok_or("missing invitation expiry")?;
    let invitation_url = created["invitationUrl"]
        .as_str()
        .ok_or("missing one-time invitation URL")?;
    let token = invitation_url
        .strip_prefix("/register/invitation#token=")
        .ok_or("invitation token is not in the URL fragment")?;
    assert_eq!(token.len(), 43);

    let (challenge_id, challenge_reference_id, recipient_email, token_hash): (
        String,
        String,
        String,
        Vec<u8>,
    ) = sqlx::query_as(
        "SELECT id, reference_id, recipient_email, token_hash
         FROM mail_challenges WHERE challenge_kind = 'INVITATION'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(challenge_reference_id, invitation_id);
    assert_eq!(recipient_email, "recipient@example.test");
    let expected_token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    assert_eq!(token_hash, expected_token_hash);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mail_outbox")
            .fetch_one(database.pool())
            .await?,
        1
    );

    let resend_url = format!("{collection_url}/{invitation_id}/resend");
    let unauthenticated_resend = client
        .post(&resend_url)
        .header("Idempotency-Key", "admin-resend-1")
        .send()
        .await?;
    assert_eq!(
        unauthenticated_resend.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let resend_missing_csrf = client
        .post(&resend_url)
        .header(COOKIE, &cookies)
        .header("Idempotency-Key", "admin-resend-1")
        .send()
        .await?;
    assert_eq!(resend_missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);
    let resend_missing_key = client
        .post(&resend_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .send()
        .await?;
    assert_eq!(
        resend_missing_key.status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let resend_blank_key = client
        .post(&resend_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .header("Idempotency-Key", "   ")
        .send()
        .await?;
    assert_eq!(resend_blank_key.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mail_outbox")
            .fetch_one(database.pool())
            .await?,
        1
    );
    let first_resend = client
        .post(&resend_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .header("Idempotency-Key", "admin-resend-1")
        .send()
        .await?;
    assert_eq!(first_resend.status(), reqwest::StatusCode::ACCEPTED);
    let first_resend_body = first_resend.text().await?;
    assert!(!first_resend_body.contains(token));
    assert!(!first_resend_body.contains(invitation_url));
    assert_eq!(
        serde_json::from_str::<Value>(&first_resend_body)?["emailQueued"],
        true
    );
    for idempotency_key in ["admin-resend-1", "admin-resend-2", "admin-resend-3"] {
        let response = client
            .post(&resend_url)
            .header(COOKIE, &cookies)
            .header("X-CSRF-Token", &csrf)
            .header("Idempotency-Key", idempotency_key)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    }
    let fourth_resend = client
        .post(&resend_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .header("Idempotency-Key", "admin-resend-4")
        .send()
        .await?;
    assert_eq!(fourth_resend.status(), reqwest::StatusCode::ACCEPTED);
    let limited_resend = client
        .post(&resend_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .header("Idempotency-Key", "admin-resend-5")
        .send()
        .await?;
    assert_eq!(
        limited_resend.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    let limited_resend_body = limited_resend.text().await?;
    assert!(!limited_resend_body.contains("recipient@example.test"));
    assert!(!limited_resend_body.contains(token));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mail_outbox")
            .fetch_one(database.pool())
            .await?,
        5
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mail_outbox WHERE reference_id = ?")
            .bind(&challenge_id)
            .fetch_one(database.pool())
            .await?,
        5
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM mail_challenges
             WHERE challenge_kind = 'INVITATION' AND reference_id = ?",
        )
        .bind(invitation_id)
        .fetch_one(database.pool())
        .await?,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT expires_at FROM user_invitations WHERE id = ?")
            .bind(invitation_id)
            .fetch_one(database.pool())
            .await?,
        original_expires_at
    );

    let default_page = client
        .get(&collection_url)
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(default_page.status(), reqwest::StatusCode::OK);
    let default_page: Value = default_page.json().await?;
    assert_eq!(default_page["pageSize"], 20);

    let duplicate = client
        .post(&collection_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "recipientEmail": "RECIPIENT@example.test",
            "presetId": "builtin-user"
        }))
        .send()
        .await?;
    assert_eq!(duplicate.status(), reqwest::StatusCode::CONFLICT);

    let invalid_filter = client
        .get(format!("{collection_url}?status=unknown"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(invalid_filter.status(), reqwest::StatusCode::BAD_REQUEST);

    let pending = client
        .get(format!(
            "{collection_url}?status=pending&page=1&pageSize=1000"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(pending.status(), reqwest::StatusCode::OK);
    let pending_text = pending.text().await?;
    assert!(!pending_text.contains(token));
    assert!(!pending_text.contains("invitationUrl"));
    assert!(!pending_text.contains("permissionSnapshotJson"));
    assert!(!pending_text.contains("tokenHash"));
    let pending: Value = serde_json::from_str(&pending_text)?;
    assert_eq!(pending["pageSize"], 100);
    assert_eq!(pending["totalCount"], 1);
    assert_eq!(pending["invitations"][0]["id"], invitation_id);
    assert_eq!(pending["invitations"][0]["status"], "PENDING");

    let revoke_url = format!("{collection_url}/{invitation_id}/revoke");
    let missing_revoke_csrf = client
        .post(&revoke_url)
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(missing_revoke_csrf.status(), reqwest::StatusCode::FORBIDDEN);
    let revoked = client
        .post(&revoke_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .send()
        .await?;
    assert_eq!(revoked.status(), reqwest::StatusCode::OK);
    let resend_revoked = client
        .post(&resend_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .header("Idempotency-Key", "admin-resend-revoked")
        .send()
        .await?;
    assert_eq!(resend_revoked.status(), reqwest::StatusCode::CONFLICT);
    let resend_missing = client
        .post(format!("{collection_url}/missing-invitation/resend"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .header("Idempotency-Key", "admin-resend-missing")
        .send()
        .await?;
    assert_eq!(resend_missing.status(), reqwest::StatusCode::NOT_FOUND);

    let accepted_creation = client
        .post(&collection_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "recipientEmail": "accepted@example.test",
            "presetId": "builtin-user"
        }))
        .send()
        .await?;
    assert_eq!(accepted_creation.status(), reqwest::StatusCode::CREATED);
    let accepted_creation: Value = accepted_creation.json().await?;
    let accepted_invitation_id = accepted_creation["invitation"]["id"]
        .as_str()
        .ok_or("missing accepted invitation ID")?;
    let before_conflicting_key_resend: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_outbox")
        .fetch_one(database.pool())
        .await?;
    let conflicting_key_resend = client
        .post(format!("{collection_url}/{accepted_invitation_id}/resend"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .header("Idempotency-Key", "admin-resend-1")
        .send()
        .await?;
    assert_eq!(
        conflicting_key_resend.status(),
        reqwest::StatusCode::CONFLICT
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mail_outbox")
            .fetch_one(database.pool())
            .await?,
        before_conflicting_key_resend
    );
    sqlx::query(
        "UPDATE user_invitations
         SET status = 'ACCEPTED', accepted_at = created_at, updated_at = created_at
         WHERE id = ?",
    )
    .bind(accepted_invitation_id)
    .execute(database.pool())
    .await?;
    let before_accepted_resend: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_outbox")
        .fetch_one(database.pool())
        .await?;
    let resend_accepted = client
        .post(format!("{collection_url}/{accepted_invitation_id}/resend"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .header("Idempotency-Key", "admin-resend-accepted")
        .send()
        .await?;
    assert_eq!(resend_accepted.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mail_outbox")
            .fetch_one(database.pool())
            .await?,
        before_accepted_resend
    );

    let expired_creation = client
        .post(&collection_url)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "recipientEmail": "expired@example.test",
            "presetId": "builtin-user"
        }))
        .send()
        .await?;
    assert_eq!(expired_creation.status(), reqwest::StatusCode::CREATED);
    let expired_creation: Value = expired_creation.json().await?;
    let expired_invitation_id = expired_creation["invitation"]["id"]
        .as_str()
        .ok_or("missing expired invitation ID")?;
    sqlx::query(
        "UPDATE user_invitations
         SET status = 'EXPIRED', expires_at = created_at + 1, updated_at = created_at + 1
         WHERE id = ?",
    )
    .bind(expired_invitation_id)
    .execute(database.pool())
    .await?;
    let before_expired_resend: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_outbox")
        .fetch_one(database.pool())
        .await?;
    let resend_expired = client
        .post(format!("{collection_url}/{expired_invitation_id}/resend"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .header("Idempotency-Key", "admin-resend-expired")
        .send()
        .await?;
    assert_eq!(resend_expired.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mail_outbox")
            .fetch_one(database.pool())
            .await?,
        before_expired_resend
    );

    let revoked_list = client
        .get(format!("{collection_url}?status=revoked"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(revoked_list.status(), reqwest::StatusCode::OK);
    let revoked_list: Value = revoked_list.json().await?;
    assert_eq!(revoked_list["totalCount"], 1);
    assert_eq!(revoked_list["invitations"][0]["status"], "REVOKED");
    let challenge_revoked_at: Option<i64> =
        sqlx::query_scalar("SELECT revoked_at FROM mail_challenges WHERE id = ?")
            .bind(&challenge_id)
            .fetch_one(database.pool())
            .await?;
    assert!(challenge_revoked_at.is_some());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM mail_outbox WHERE reference_id = ? AND status = 'FAILED'",
        )
        .bind(&challenge_id)
        .fetch_one(database.pool())
        .await?,
        5
    );

    let (_, audit_events) = luxd::observability::logs::LogStore::new(&config_dir)
        .list_audit_events(0, 20)
        .await?;
    for event_type in [
        "USER_INVITATION_CREATED",
        "USER_INVITATION_RESEND_QUEUED",
        "USER_INVITATION_REVOKED",
    ] {
        let event = audit_events
            .iter()
            .find(|event| {
                event.event_type == event_type && event.target_id.as_deref() == Some(invitation_id)
            })
            .ok_or("missing invitation audit event")?;
        assert_eq!(event.target_id.as_deref(), Some(invitation_id));
        assert_eq!(event.metadata_json, "{}");
    }
    let audit_text = audit_events
        .iter()
        .map(|event| {
            format!(
                "{}|{}|{}|{}|{}",
                event.event_type,
                event.actor_username.as_deref().unwrap_or_default(),
                event.target_type.as_deref().unwrap_or_default(),
                event.target_id.as_deref().unwrap_or_default(),
                event.metadata_json,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!audit_text.contains("recipient@example.test"));
    assert!(!audit_text.contains(token));
    assert!(!audit_text.contains(invitation_url));
    assert!(!audit_text.contains("admin-resend-"));

    server.abort();
    database.close().await;
    Ok(())
}
