use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{
        emby::{EmbyAuthService, EmbyDeviceInfo},
        sessions::WebAuthService,
        users::UserStore,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::COOKIE;
use serde_json::{Value, json};
use sha2::Digest;
use tokio::net::TcpListener;

#[tokio::test]
async fn successful_login_is_written_to_the_user_activity_index()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let user = setup
        .complete("activity-user", "Activity User", "correct password")
        .await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder().no_proxy().build()?;
    let device_id = "00000000-0000-4000-8000-000000000001";
    let response = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .header("X-Lux-Device-Id", device_id)
        .json(&json!({
            "username": "activity-user",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let user_id = user.id.to_string();
    let stored_session_device_id: Option<String> = sqlx::query_scalar(
        "SELECT device_id FROM web_sessions WHERE user_id = ? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&user_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored_session_device_id.as_deref(), Some(device_id));
    let (event_count, actor_user_id, metadata_json): (i64, Option<String>, Option<String>) =
        sqlx::query_as(
            "SELECT COUNT(*), MAX(actor_user_id), MAX(metadata_json)
         FROM user_activity_events WHERE user_id = ? AND event_type = 'AUTH_LOGIN'",
        )
        .bind(&user_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(event_count, 1);
    assert_eq!(actor_user_id.as_deref(), Some(user_id.as_str()));
    let metadata_json = metadata_json.ok_or("missing login activity metadata")?;
    assert!(!metadata_json.contains("correct password"));

    let (stored_device_id, client_name, device_type, first_seen_at, last_seen_at): (
        String,
        String,
        Option<String>,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT device_id, client_name, device_type, first_seen_at, last_seen_at
         FROM user_devices WHERE user_id = ?",
    )
    .bind(&user_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored_device_id, device_id);
    assert_eq!(client_name, "Lux Web");
    assert_eq!(device_type.as_deref(), Some("WEB"));
    assert!(first_seen_at > 0);
    assert!(last_seen_at >= first_seen_at);

    let authenticated_cookie = cookie_pair(response.headers());
    sqlx::query("UPDATE user_devices SET last_seen_at = 1 WHERE user_id = ? AND device_id = ?")
        .bind(&user_id)
        .bind(device_id)
        .execute(database.pool())
        .await?;
    let bound_session_without_device_header = client
        .get(format!("http://{address}/api/v1/admin/audit?pageSize=20"))
        .header(COOKIE, &authenticated_cookie)
        .send()
        .await?;
    assert_eq!(
        bound_session_without_device_header.status(),
        reqwest::StatusCode::OK
    );
    let last_seen_without_header: i64 = sqlx::query_scalar(
        "SELECT last_seen_at FROM user_devices WHERE user_id = ? AND device_id = ?",
    )
    .bind(&user_id)
    .bind(device_id)
    .fetch_one(database.pool())
    .await?;
    assert!(last_seen_without_header > 1);

    sqlx::query("UPDATE user_devices SET last_seen_at = 1 WHERE user_id = ? AND device_id = ?")
        .bind(&user_id)
        .bind(device_id)
        .execute(database.pool())
        .await?;
    let alternate_device_id = "00000000-0000-4000-8000-000000000002";
    let authenticated_access = client
        .get(format!("http://{address}/api/v1/admin/audit?pageSize=20"))
        .header("X-Lux-Device-Id", alternate_device_id)
        .header(COOKIE, &authenticated_cookie)
        .send()
        .await?;
    assert_eq!(authenticated_access.status(), reqwest::StatusCode::OK);
    let (refreshed_first_seen_at, refreshed_last_seen_at): (i64, i64) = sqlx::query_as(
        "SELECT first_seen_at, last_seen_at FROM user_devices WHERE user_id = ? AND device_id = ?",
    )
    .bind(&user_id)
    .bind(device_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(refreshed_first_seen_at, first_seen_at);
    assert!(refreshed_last_seen_at > 1);
    let inventory_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_devices WHERE user_id = ?")
            .bind(&user_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(inventory_count, 1);

    sqlx::query("UPDATE user_devices SET last_seen_at = 1 WHERE user_id = ? AND device_id = ?")
        .bind(&user_id)
        .bind(device_id)
        .execute(database.pool())
        .await?;
    let invalid_device_access = client
        .get(format!("http://{address}/api/v1/admin/audit?pageSize=20"))
        .header("X-Lux-Device-Id", "not-a-uuid")
        .header(COOKIE, &authenticated_cookie)
        .send()
        .await?;
    assert_eq!(invalid_device_access.status(), reqwest::StatusCode::OK);
    let (invalid_device_count, refreshed_last_seen_at): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), MAX(last_seen_at) FROM user_devices WHERE user_id = ?")
            .bind(&user_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(invalid_device_count, 1);
    assert!(refreshed_last_seen_at > 1);

    let invalid_device_login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .header("X-Lux-Device-Id", "not-a-uuid")
        .json(&json!({
            "username": "activity-user",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(invalid_device_login.status(), reqwest::StatusCode::OK);
    let null_device_sessions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM web_sessions WHERE user_id = ? AND device_id IS NULL",
    )
    .bind(&user_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(null_device_sessions, 1);
    let device_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_devices WHERE user_id = ?")
            .bind(&user_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(device_count, 1);

    let missing_device_login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({
            "username": "activity-user",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(missing_device_login.status(), reqwest::StatusCode::OK);
    let null_device_sessions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM web_sessions WHERE user_id = ? AND device_id IS NULL",
    )
    .bind(&user_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(null_device_sessions, 2);
    let historical_session_cookie = cookie_pair(missing_device_login.headers());
    let legacy_device_id = "00000000-0000-4000-8000-000000000003";
    let historical_session_access = client
        .get(format!("http://{address}/api/v1/admin/audit?pageSize=20"))
        .header("X-Lux-Device-Id", legacy_device_id)
        .header(COOKIE, historical_session_cookie)
        .send()
        .await?;
    assert_eq!(historical_session_access.status(), reqwest::StatusCode::OK);
    let historical_device_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_devices WHERE user_id = ?")
            .bind(&user_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(historical_device_count, 2);

    sqlx::query(
        "CREATE TRIGGER fail_user_device_insert
         BEFORE INSERT ON user_devices
         BEGIN SELECT RAISE(ABORT, 'device inventory unavailable'); END",
    )
    .execute(database.pool())
    .await?;
    let storage_failure_login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .header("X-Lux-Device-Id", alternate_device_id)
        .json(&json!({
            "username": "activity-user",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(storage_failure_login.status(), reqwest::StatusCode::OK);

    let audit = client
        .get(format!("http://{address}/api/v1/admin/audit?pageSize=20"))
        .header("X-Lux-Device-Id", device_id)
        .header(COOKIE, authenticated_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    assert!(
        audit.json::<Value>().await?["events"]
            .as_array()
            .is_some_and(|events| events
                .iter()
                .any(|event| event["eventType"] == "AUTH_LOGIN"))
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn failed_web_logins_are_indexed_only_for_known_users_and_not_rate_limited_attempts()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let known_user = setup
        .complete("failure-known", "Failure Known", "correct password")
        .await?;
    let disabled_user = UserStore::new(database.clone())?
        .create_user(
            "failure-disabled",
            "Failure Disabled",
            "disabled password",
            false,
        )
        .await?;
    sqlx::query("UPDATE users SET is_disabled = 1 WHERE id = ?")
        .bind(disabled_user.id.to_string())
        .execute(database.pool())
        .await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder().no_proxy().build()?;
    let admin_login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({
            "username": "failure-known",
            "password": "correct password",
        }))
        .send()
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_cookie = cookie_pair(admin_login.headers());

    for attempt in 0..6 {
        let response = client
            .post(format!("http://{address}/api/v1/auth/login"))
            .json(&json!({
                "username": "failure-known",
                "password": format!("wrong password {attempt}"),
            }))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
    let disabled_response = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({
            "username": "failure-disabled",
            "password": "disabled password",
        }))
        .send()
        .await?;
    assert_eq!(
        disabled_response.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let unknown_response = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({
            "username": "failure-unknown",
            "password": "unknown password",
        }))
        .send()
        .await?;
    assert_eq!(unknown_response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let invalid_username_response = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({ "username": "", "password": "invalid username password" }))
        .send()
        .await?;
    assert_ne!(invalid_username_response.status(), reqwest::StatusCode::OK);

    let events: Vec<(
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    )> = sqlx::query_as(
        "SELECT event_type, user_id, actor_user_id, target_type, target_id, metadata_json
         FROM user_activity_events WHERE event_type = 'AUTH_LOGIN_FAILED'",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        events.len(),
        6,
        "the sixth known-user attempt is rate limited"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.1 == known_user.id.to_string())
            .count(),
        5
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.1 == disabled_user.id.to_string())
            .count(),
        1
    );
    assert!(events.iter().all(|event| {
        event.0 == "AUTH_LOGIN_FAILED"
            && event.2.is_none()
            && event.3.as_deref() == Some("user")
            && event.4.as_deref() == Some(event.1.as_str())
            && event.5.contains("LUX_WEB")
            && !event.5.contains("wrong password")
            && !event.5.contains("correct password")
            && !event.5.contains("disabled password")
            && !event.5.contains("failure-known")
            && !event.5.contains("failure-disabled")
    }));
    let audit = client
        .get(format!("http://{address}/api/v1/admin/audit?pageSize=20"))
        .header(COOKIE, admin_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let audit_body = audit.json::<Value>().await?;
    let audit_events = audit_body["events"]
        .as_array()
        .ok_or("missing audit events")?;
    let failed_audit_events: Vec<_> = audit_events
        .iter()
        .filter(|event| event["eventType"] == "AUTH_LOGIN_FAILED")
        .collect();
    assert_eq!(failed_audit_events.len(), 6);
    let audit_json = serde_json::to_string(&failed_audit_events)?;
    assert!(!audit_json.contains("wrong password"));
    assert!(!audit_json.contains("unknown password"));
    assert!(!audit_json.contains("disabled password"));
    assert!(!audit_json.contains("failure-known"));
    assert!(!audit_json.contains("failure-disabled"));

    server.abort();
    Ok(())
}

#[tokio::test]
async fn emby_authentication_target_lookup_matches_login_name_rules()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup
        .complete("lookup-admin", "Unique Display", "admin password")
        .await?;
    let user_store = UserStore::new(database.clone())?;
    user_store
        .create_user(
            "lookup-duplicate-one",
            "Duplicate Display",
            "password one",
            false,
        )
        .await?;
    user_store
        .create_user(
            "lookup-duplicate-two",
            "Duplicate Display",
            "password two",
            false,
        )
        .await?;
    let auth = EmbyAuthService::new(database)?;

    assert_eq!(
        auth.find_failed_authentication_user_id(" LOOKUP-ADMIN ")
            .await?
            .as_deref(),
        Some(admin.id.to_string().as_str())
    );
    assert_eq!(
        auth.find_failed_authentication_user_id("Unique Display")
            .await?
            .as_deref(),
        Some(admin.id.to_string().as_str())
    );
    assert_eq!(
        auth.find_failed_authentication_user_id("Duplicate Display")
            .await?,
        None
    );
    assert_eq!(
        auth.find_failed_authentication_user_id("unknown-name")
            .await?,
        None
    );
    assert!(
        auth.find_failed_authentication_user_id("   ")
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn failed_emby_logins_are_indexed_only_for_known_users_and_not_rate_limited_attempts()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let known_user = setup
        .complete("emby-fail-known", "Emby Failure Known", "correct password")
        .await?;
    let disabled_user = UserStore::new(database.clone())?
        .create_user(
            "emby-fail-disabled",
            "Emby Failure Disabled",
            "disabled password",
            false,
        )
        .await?;
    sqlx::query("UPDATE users SET is_disabled = 1 WHERE id = ?")
        .bind(disabled_user.id.to_string())
        .execute(database.pool())
        .await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder().no_proxy().build()?;
    let admin_login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({
            "username": "emby-fail-known",
            "password": "correct password",
        }))
        .send()
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_cookie = cookie_pair(admin_login.headers());

    for attempt in 0..6 {
        let response = client
            .post(format!("http://{address}/emby/Users/AuthenticateByName"))
            .json(&json!({
                "Username": "emby-fail-known",
                "Pw": format!("wrong password {attempt}"),
            }))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
    let display_name_response = client
        .post(format!("http://{address}/emby/Users/AuthenticateByName"))
        .json(&json!({
            "Username": "Emby Failure Known",
            "Pw": "wrong display-name password",
        }))
        .send()
        .await?;
    assert_eq!(
        display_name_response.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let disabled_response = client
        .post(format!(
            "http://{address}/emby/Users/{}/Authenticate",
            disabled_user.id
        ))
        .json(&json!({ "Pw": "disabled password" }))
        .send()
        .await?;
    assert_eq!(
        disabled_response.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let unknown_response = client
        .post(format!("http://{address}/emby/Users/AuthenticateByName"))
        .json(&json!({
            "Username": "emby-fail-unknown",
            "Pw": "unknown password",
        }))
        .send()
        .await?;
    assert_eq!(unknown_response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let invalid_id_response = client
        .post(format!(
            "http://{address}/emby/Users/not-a-user-id/Authenticate"
        ))
        .json(&json!({ "Pw": "invalid ID password" }))
        .send()
        .await?;
    assert_ne!(invalid_id_response.status(), reqwest::StatusCode::OK);
    let unknown_id_response = client
        .post(format!(
            "http://{address}/emby/Users/00000000-0000-4000-8000-000000000000/Authenticate"
        ))
        .json(&json!({ "Pw": "unknown ID password" }))
        .send()
        .await?;
    assert_eq!(
        unknown_id_response.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let failed_events: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT event_type, user_id, actor_user_id, metadata_json
         FROM user_activity_events WHERE event_type = 'AUTH_LOGIN_FAILED'",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        failed_events.len(),
        7,
        "the sixth known-user attempt is rate limited"
    );
    assert_eq!(
        failed_events
            .iter()
            .filter(|event| event.1 == known_user.id.to_string())
            .count(),
        6
    );
    assert_eq!(
        failed_events
            .iter()
            .filter(|event| event.1 == disabled_user.id.to_string())
            .count(),
        1
    );
    assert!(failed_events.iter().all(|event| {
        event.0 == "AUTH_LOGIN_FAILED"
            && event.2.is_none()
            && event.3.as_deref().is_some_and(|metadata| {
                metadata.contains("LUX_EMBY")
                    && !metadata.contains("wrong password")
                    && !metadata.contains("disabled password")
                    && !metadata.contains("emby-fail-known")
                    && !metadata.contains("emby-fail-disabled")
            })
    }));
    let audit = client
        .get(format!("http://{address}/api/v1/admin/audit?pageSize=20"))
        .header(COOKIE, admin_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let audit_body = audit.json::<Value>().await?;
    assert_eq!(
        audit_body["events"]
            .as_array()
            .ok_or("missing audit events")?
            .iter()
            .filter(|event| event["eventType"] == "AUTH_LOGIN_FAILED")
            .count(),
        7
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_filter_and_page_user_activity_events() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup
        .complete("activity-admin", "Activity Admin", "correct password")
        .await?;
    let admin_id = admin.id.to_string();
    for (id, user_id, actor_user_id, event_type, occurred_at) in [
        (
            "timeline-login-100",
            admin_id.as_str(),
            Some(admin_id.as_str()),
            "AUTH_LOGIN",
            100_i64,
        ),
        (
            "timeline-pause-200",
            admin_id.as_str(),
            Some(admin_id.as_str()),
            "PLAYBACK_PAUSED",
            200,
        ),
        (
            "timeline-start-200",
            admin_id.as_str(),
            Some(admin_id.as_str()),
            "PLAYBACK_STARTED",
            200,
        ),
        (
            "timeline-other-user-200",
            "activity-unrelated-user",
            None,
            "AUTH_LOGIN",
            200,
        ),
    ] {
        sqlx::query(
            "INSERT INTO user_activity_events (
                 id, user_id, actor_user_id, event_type, metadata_json, occurred_at
             ) VALUES (?, ?, ?, ?, '{\"client\":\"Lux\"}', ?)",
        )
        .bind(id)
        .bind(user_id)
        .bind(actor_user_id)
        .bind(event_type)
        .bind(occurred_at)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO user_activity_events (
             id, user_id, actor_user_id, event_type, metadata_json, occurred_at
         ) VALUES ('timeline-scalar-300', ?, ?, 'TEST_SCALAR', '\"https://private.example\"', 300)",
    )
    .bind(&admin_id)
    .bind(&admin_id)
    .execute(database.pool())
    .await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder().no_proxy().build()?;
    let login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({
            "username": "activity-admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookie = cookie_pair(login.headers());
    let csrf = cookie_value(login.headers(), "lux_csrf");
    let created_reader = client
        .post(format!("http://{address}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "activity-reader",
            "password": "reader password"
        }))
        .send()
        .await?;
    assert_eq!(created_reader.status(), reqwest::StatusCode::CREATED);
    let reader_login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({
            "username": "activity-reader",
            "password": "reader password"
        }))
        .send()
        .await?;
    assert_eq!(reader_login.status(), reqwest::StatusCode::OK);
    let forbidden = client
        .get(format!(
            "http://{address}/api/v1/admin/users/{admin_id}/activity"
        ))
        .header(COOKIE, cookie_pair(reader_login.headers()))
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);
    let unauthorized = client
        .get(format!(
            "http://{address}/api/v1/admin/users/{admin_id}/activity"
        ))
        .send()
        .await?;
    assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);

    let response = client
        .get(format!(
            "http://{address}/api/v1/admin/users/{admin_id}/activity?page=1&pageSize=2&from=100&to=200"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let first_page = response.json::<Value>().await?;
    assert_eq!(first_page["page"], 1);
    assert_eq!(first_page["pageSize"], 2);
    assert_eq!(first_page["totalCount"], 3);
    assert_eq!(first_page["events"][0]["id"], "timeline-start-200");
    assert_eq!(first_page["events"][0]["userId"], admin_id);
    assert_eq!(first_page["events"][0]["actorUserId"], admin_id);
    assert_eq!(first_page["events"][0]["eventType"], "PLAYBACK_STARTED");
    assert_eq!(first_page["events"][0]["metadata"]["client"], "Lux");
    assert_eq!(first_page["events"][0]["occurredAt"], 200);
    assert_eq!(first_page["events"][1]["id"], "timeline-pause-200");

    let second_page = client
        .get(format!(
            "http://{address}/api/v1/admin/users/{admin_id}/activity?page=2&pageSize=2&from=100&to=200"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert_eq!(second_page["totalCount"], 3);
    assert_eq!(second_page["events"][0]["id"], "timeline-login-100");

    let filtered = client
        .get(format!(
            "http://{address}/api/v1/admin/users/{admin_id}/activity?eventType=PLAYBACK_STARTED"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert_eq!(filtered["totalCount"], 1);
    assert_eq!(filtered["page"], 1);
    assert_eq!(filtered["pageSize"], 20);
    assert_eq!(filtered["events"][0]["id"], "timeline-start-200");

    let oversized_page = client
        .get(format!(
            "http://{address}/api/v1/admin/users/{admin_id}/activity?pageSize=1000"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert_eq!(oversized_page["pageSize"], 100);

    let sanitized_scalar = client
        .get(format!(
            "http://{address}/api/v1/admin/users/{admin_id}/activity?eventType=TEST_SCALAR"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert_eq!(sanitized_scalar["totalCount"], 1);
    assert_eq!(
        sanitized_scalar["events"][0]["metadata"]["value"],
        "[REDACTED_URL]"
    );

    let invalid_type = client
        .get(format!(
            "http://{address}/api/v1/admin/users/{admin_id}/activity?eventType=%20"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(invalid_type.status(), reqwest::StatusCode::BAD_REQUEST);
    let negative_time = client
        .get(format!(
            "http://{address}/api/v1/admin/users/{admin_id}/activity?from=-1"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(negative_time.status(), reqwest::StatusCode::BAD_REQUEST);
    let invalid_range = client
        .get(format!(
            "http://{address}/api/v1/admin/users/{admin_id}/activity?from=201&to=200"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(invalid_range.status(), reqwest::StatusCode::BAD_REQUEST);

    let invalid_user_id = client
        .get(format!(
            "http://{address}/api/v1/admin/users/invalid/activity"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(invalid_user_id.status(), reqwest::StatusCode::BAD_REQUEST);

    let missing_user_id = uuid::Uuid::now_v7().to_string();
    let missing_user = client
        .get(format!(
            "http://{address}/api/v1/admin/users/{missing_user_id}/activity"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(missing_user.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_manage_users_and_last_manager_is_protected()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin_user = setup.complete("Admin", "Admin", "correct password").await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    sqlx::query("DROP TABLE audit_events")
        .execute(database.pool())
        .await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cookie = cookie_pair(login.headers());
    let csrf = cookie_value(login.headers(), "lux_csrf");
    let users = client
        .get(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    let users_status = users.status();
    let users_body = users.text().await?;
    assert_eq!(users_status, reqwest::StatusCode::OK, "{users_body}");
    assert_eq!(
        serde_json::from_str::<Value>(&users_body)?["users"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    let preset_list = client
        .get(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(preset_list.status(), reqwest::StatusCode::OK);
    let presets = preset_list.json::<Value>().await?["presets"].clone();
    assert_eq!(presets.as_array().map(Vec::len), Some(2));
    assert!(
        presets
            .as_array()
            .is_some_and(|presets| presets.iter().any(|preset| preset["id"] == "builtin-admin"))
    );
    let preset_input = json!({
        "name": "Family",
        "description": "Family media access",
        "isAdmin": false,
        "canManageServer": false,
        "permissions": {
            "libraryAccessMode": "NONE",
            "libraryIds": [],
            "canDownload": false,
            "canPlay": true,
            "maxSimultaneousPlaybacks": 2,
            "allowRemux": true,
            "allowAudioTranscode": true,
            "allowVideoTranscode": true
        }
    });
    let missing_preset_csrf = client
        .post(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &cookie)
        .json(&preset_input)
        .send()
        .await?;
    assert_eq!(missing_preset_csrf.status(), reqwest::StatusCode::FORBIDDEN);
    let invalid_preset = client
        .post(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "name": "Invalid",
            "isAdmin": false,
            "canManageServer": false,
            "permissions": {
                "libraryAccessMode": "NONE",
                "libraryIds": [],
                "canDownload": false,
                "canPlay": true,
                "maxSimultaneousPlaybacks": -1,
                "allowRemux": true,
                "allowAudioTranscode": true,
                "allowVideoTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(invalid_preset.status(), reqwest::StatusCode::BAD_REQUEST);
    let created_preset = client
        .post(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&preset_input)
        .send()
        .await?;
    assert_eq!(created_preset.status(), reqwest::StatusCode::CREATED);
    let created_preset = created_preset.json::<Value>().await?["preset"].clone();
    assert_eq!(created_preset["version"], 1);
    assert_eq!(created_preset["isBuiltin"], false);
    let preset_id = created_preset["id"]
        .as_str()
        .ok_or("missing permission preset id")?
        .to_owned();
    let duplicate_preset = client
        .post(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "name": " family ",
            "isAdmin": false,
            "canManageServer": false,
            "permissions": preset_input["permissions"].clone()
        }))
        .send()
        .await?;
    assert_eq!(duplicate_preset.status(), reqwest::StatusCode::CONFLICT);

    let updated_preset = client
        .put(format!(
            "{base_url}/api/v1/admin/user-permission-presets/{preset_id}"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "version": 1,
            "name": "Family media",
            "description": "Updated family access",
            "isAdmin": false,
            "canManageServer": false,
            "permissions": preset_input["permissions"].clone()
        }))
        .send()
        .await?;
    assert_eq!(updated_preset.status(), reqwest::StatusCode::OK);
    assert_eq!(
        updated_preset.json::<Value>().await?["preset"]["version"],
        2
    );
    let stale_preset_update = client
        .put(format!(
            "{base_url}/api/v1/admin/user-permission-presets/{preset_id}"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "version": 1,
            "name": "Stale family",
            "isAdmin": false,
            "canManageServer": false,
            "permissions": preset_input["permissions"].clone()
        }))
        .send()
        .await?;
    assert_eq!(stale_preset_update.status(), reqwest::StatusCode::CONFLICT);

    let builtin_update = client
        .put(format!(
            "{base_url}/api/v1/admin/user-permission-presets/builtin-admin"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "version": 1,
            "name": "Admin override",
            "isAdmin": true,
            "canManageServer": true,
            "permissions": preset_input["permissions"].clone()
        }))
        .send()
        .await?;
    assert_eq!(builtin_update.status(), reqwest::StatusCode::CONFLICT);

    let builtin_delete = client
        .delete(format!(
            "{base_url}/api/v1/admin/user-permission-presets/builtin-admin?version=1"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(builtin_delete.status(), reqwest::StatusCode::CONFLICT);
    let missing_delete_version = client
        .delete(format!(
            "{base_url}/api/v1/admin/user-permission-presets/{preset_id}"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(
        missing_delete_version.status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let missing_preset = client
        .delete(format!(
            "{base_url}/api/v1/admin/user-permission-presets/missing?version=1"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(missing_preset.status(), reqwest::StatusCode::NOT_FOUND);

    let deleted_preset = client
        .delete(format!(
            "{base_url}/api/v1/admin/user-permission-presets/{preset_id}?version=2"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(deleted_preset.status(), reqwest::StatusCode::NO_CONTENT);

    let created = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "managed",
            "displayName": "Managed",
            "password": "managed password"
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let created_id = created.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing user id")?
        .to_owned();

    let managed_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "managed", "password": "managed password" }))
        .send()
        .await?;
    let managed_cookie = cookie_pair(managed_login.headers());
    let managed_csrf = cookie_value(managed_login.headers(), "lux_csrf");
    let managed_presets = client
        .get(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &managed_cookie)
        .send()
        .await?;
    assert_eq!(managed_presets.status(), reqwest::StatusCode::FORBIDDEN);
    let managed_preset_create = client
        .post(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &managed_cookie)
        .header("x-csrf-token", &managed_csrf)
        .json(&preset_input)
        .send()
        .await?;
    assert_eq!(
        managed_preset_create.status(),
        reqwest::StatusCode::FORBIDDEN
    );

    let default_playback_policy = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{created_id}/playback-policy"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(default_playback_policy.status(), reqwest::StatusCode::OK);
    let default_policy = default_playback_policy.json::<Value>().await?["policy"].clone();
    assert_eq!(default_policy["canPlay"], true);
    assert_eq!(default_policy["maxSimultaneousPlaybacks"], 2);
    assert_eq!(default_policy["allowRemux"], true);
    assert_eq!(default_policy["allowAudioTranscode"], true);
    assert_eq!(default_policy["allowVideoTranscode"], true);

    let playback_policy = json!({
        "canPlay": false,
        "maxSimultaneousPlaybacks": 1,
        "allowRemux": false,
        "allowAudioTranscode": true,
        "allowVideoTranscode": false
    });
    let updated_playback_policy = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/playback-policy"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&playback_policy)
        .send()
        .await?;
    assert_eq!(updated_playback_policy.status(), reqwest::StatusCode::OK);
    assert_eq!(
        updated_playback_policy.json::<Value>().await?["policy"],
        playback_policy
    );
    let missing_csrf_update = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/playback-policy"
        ))
        .header(COOKIE, &cookie)
        .json(&playback_policy)
        .send()
        .await?;
    assert_eq!(missing_csrf_update.status(), reqwest::StatusCode::FORBIDDEN);
    let invalid_playback_policy = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/playback-policy"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "canPlay": true,
            "maxSimultaneousPlaybacks": -1,
            "allowRemux": true,
            "allowAudioTranscode": true,
            "allowVideoTranscode": true
        }))
        .send()
        .await?;
    assert_eq!(
        invalid_playback_policy.status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let unlimited_playback_policy = json!({
        "canPlay": true,
        "maxSimultaneousPlaybacks": 0,
        "allowRemux": true,
        "allowAudioTranscode": true,
        "allowVideoTranscode": true
    });
    let unlimited_update = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/playback-policy"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&unlimited_playback_policy)
        .send()
        .await?;
    assert_eq!(unlimited_update.status(), reqwest::StatusCode::OK);
    assert_eq!(
        unlimited_update.json::<Value>().await?["policy"],
        unlimited_playback_policy
    );

    let duplicate = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "MANAGED",
            "displayName": "Another Managed",
            "password": "another password"
        }))
        .send()
        .await?;
    assert_eq!(duplicate.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        duplicate.json::<Value>().await?["error"]["message"],
        "用户名或邮箱已存在"
    );

    let library = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "name": "Movies", "kind": "MOVIE" }))
        .send()
        .await?;
    assert_eq!(library.status(), reqwest::StatusCode::CREATED);
    let library_id = library.json::<Value>().await?["library"]["id"]
        .as_str()
        .ok_or("missing library id")?
        .to_owned();
    let scan = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/{library_id}/scan"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(scan.status(), reqwest::StatusCode::ACCEPTED);
    let jobs = client
        .get(format!("{base_url}/api/v1/admin/jobs"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(jobs.status(), reqwest::StatusCode::OK);
    let jobs_body = jobs.json::<Value>().await?;
    assert_eq!(jobs_body["jobs"].as_array().map(Vec::len), Some(1));
    let job_id = jobs_body["jobs"][0]["id"]
        .as_str()
        .ok_or("missing job id")?;
    let job_detail = client
        .get(format!("{base_url}/api/v1/admin/jobs/{job_id}"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(job_detail.status(), reqwest::StatusCode::OK);
    assert_eq!(job_detail.json::<Value>().await?["job"]["id"], job_id);
    let access = client
        .patch(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries/{library_id}"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "canView": true }))
        .send()
        .await?;
    assert_eq!(access.status(), reqwest::StatusCode::OK);
    let access_list = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(access_list.status(), reqwest::StatusCode::OK);
    let access_list_body = access_list.json::<Value>().await?;
    assert_eq!(
        access_list_body["libraryIds"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(access_list_body["libraryAccessMode"], "SELECTED");

    let set_none = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "libraryAccessMode": "NONE" }))
        .send()
        .await?;
    assert_eq!(set_none.status(), reqwest::StatusCode::OK);
    let set_none_body = set_none.json::<Value>().await?;
    assert_eq!(set_none_body["libraryAccessMode"], "NONE");
    assert_eq!(
        set_none_body["libraryIds"].as_array().map(Vec::len),
        Some(0)
    );

    let empty_selected = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "libraryAccessMode": "SELECTED",
            "libraryIds": []
        }))
        .send()
        .await?;
    assert_eq!(empty_selected.status(), reqwest::StatusCode::OK);
    assert_eq!(
        empty_selected.json::<Value>().await?["libraryAccessMode"],
        "NONE"
    );

    let invalid_mode = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "libraryAccessMode": "INVALID" }))
        .send()
        .await?;
    assert_eq!(invalid_mode.status(), reqwest::StatusCode::BAD_REQUEST);
    let duplicate_selection = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "libraryAccessMode": "SELECTED",
            "libraryIds": [library_id, library_id]
        }))
        .send()
        .await?;
    assert_eq!(
        duplicate_selection.status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let missing_csrf = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .json(&json!({ "libraryAccessMode": "ALL" }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let invalid_selection = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "libraryAccessMode": "SELECTED",
            "libraryIds": ["00000000-0000-0000-0000-000000000000"]
        }))
        .send()
        .await?;
    assert_eq!(invalid_selection.status(), reqwest::StatusCode::NOT_FOUND);
    let unchanged_access = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(
        unchanged_access.json::<Value>().await?["libraryAccessMode"],
        "NONE"
    );

    let selected = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "libraryAccessMode": "SELECTED",
            "libraryIds": [library_id]
        }))
        .send()
        .await?;
    assert_eq!(selected.status(), reqwest::StatusCode::OK);
    let selected_body = selected.json::<Value>().await?;
    assert_eq!(selected_body["libraryAccessMode"], "SELECTED");
    assert_eq!(
        selected_body["libraryIds"].as_array().map(Vec::len),
        Some(1)
    );

    let set_all = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "libraryAccessMode": "ALL" }))
        .send()
        .await?;
    assert_eq!(set_all.status(), reqwest::StatusCode::OK);
    assert_eq!(set_all.json::<Value>().await?["libraryAccessMode"], "ALL");
    let missing_access = client
        .get(format!(
            "{base_url}/api/v1/admin/users/00000000-0000-0000-0000-000000000000/libraries"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(missing_access.status(), reqwest::StatusCode::NOT_FOUND);

    let regular = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "regular",
            "password": "regular password"
        }))
        .send()
        .await?;
    assert_eq!(regular.status(), reqwest::StatusCode::CREATED);
    let regular_id = regular.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing regular user id")?
        .to_owned();
    let regular_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({
            "username": "regular",
            "password": "regular password"
        }))
        .send()
        .await?;
    assert_eq!(regular_login.status(), reqwest::StatusCode::OK);
    let regular_cookie = cookie_pair(regular_login.headers());
    let regular_users = client
        .get(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &regular_cookie)
        .send()
        .await?;
    assert_eq!(regular_users.status(), reqwest::StatusCode::FORBIDDEN);
    let regular_playback_policy = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{created_id}/playback-policy"
        ))
        .header(COOKIE, &regular_cookie)
        .send()
        .await?;
    assert_eq!(
        regular_playback_policy.status(),
        reqwest::StatusCode::FORBIDDEN
    );

    let updated = client
        .patch(format!("{base_url}/api/v1/admin/users/{created_id}"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "displayName": "Managed Updated",
            "password": "new managed password",
            "isAdmin": true,
            "canManageServer": true,
            "canRemoteAccess": true,
            "canDownload": true
        }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    let updated_user = updated.json::<Value>().await?["user"].clone();
    assert_eq!(updated_user["displayName"], "Managed Updated");
    assert_eq!(updated_user["canDownload"], true);
    assert_eq!(updated_user["canRemoteAccess"], true);

    let managed_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({
            "username": "managed",
            "password": "new managed password"
        }))
        .send()
        .await?;
    assert_eq!(managed_login.status(), reqwest::StatusCode::OK);
    let manager_cookie = cookie_pair(managed_login.headers());
    let manager_csrf = cookie_value(managed_login.headers(), "lux_csrf");

    let disabled_admin = client
        .patch(format!("{base_url}/api/v1/admin/users/{}", admin_user.id))
        .header(COOKIE, &manager_cookie)
        .header("x-csrf-token", &manager_csrf)
        .json(&json!({ "isDisabled": true }))
        .send()
        .await?;
    assert_eq!(disabled_admin.status(), reqwest::StatusCode::OK);

    let listed_after_disable = client
        .get(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &manager_cookie)
        .send()
        .await?;
    assert_eq!(listed_after_disable.status(), reqwest::StatusCode::OK);
    assert!(
        listed_after_disable.json::<Value>().await?["users"]
            .as_array()
            .is_some_and(|users| {
                users.iter().any(|user| {
                    user["id"] == admin_user.id.to_string() && user["isDisabled"] == true
                })
            })
    );

    let disabled_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({
            "username": "admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(disabled_login.status(), reqwest::StatusCode::UNAUTHORIZED);

    let admin_demotion = client
        .patch(format!("{base_url}/api/v1/admin/users/{created_id}"))
        .header(COOKIE, &manager_cookie)
        .header("x-csrf-token", &manager_csrf)
        .json(&json!({ "canManageServer": false }))
        .send()
        .await?;
    assert_eq!(admin_demotion.status(), reqwest::StatusCode::CONFLICT);

    let deleted_regular = client
        .delete(format!("{base_url}/api/v1/admin/users/{regular_id}"))
        .header(COOKIE, &manager_cookie)
        .header("x-csrf-token", &manager_csrf)
        .send()
        .await?;
    let deleted_regular_status = deleted_regular.status();
    let deleted_regular_body = deleted_regular.text().await?;
    assert_eq!(
        deleted_regular_status,
        reqwest::StatusCode::OK,
        "{deleted_regular_body}"
    );
    let deleted_regular_user =
        serde_json::from_str::<Value>(&deleted_regular_body)?["user"].clone();
    assert!(deleted_regular_user["deletedAt"].as_i64().is_some());
    assert!(deleted_regular_user["deletionPurgeAt"].as_i64().is_some());
    let deleted_policy_update = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{regular_id}/playback-policy"
        ))
        .header(COOKIE, &manager_cookie)
        .header("x-csrf-token", &manager_csrf)
        .json(&playback_policy)
        .send()
        .await?;
    assert_eq!(
        deleted_policy_update.status(),
        reqwest::StatusCode::NOT_FOUND
    );
    let deleted_policy_read = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{regular_id}/playback-policy"
        ))
        .header(COOKIE, &manager_cookie)
        .send()
        .await?;
    assert_eq!(deleted_policy_read.status(), reqwest::StatusCode::NOT_FOUND);

    let reserved_username = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &manager_cookie)
        .header("x-csrf-token", &manager_csrf)
        .json(&json!({
            "username": "regular",
            "password": "regular password"
        }))
        .send()
        .await?;
    assert_eq!(reserved_username.status(), reqwest::StatusCode::CONFLICT);

    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit?pageSize=100"))
        .header(COOKIE, &manager_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let audit_events = audit.json::<Value>().await?["events"]
        .as_array()
        .cloned()
        .ok_or("missing audit events")?;
    assert!(
        audit_events
            .iter()
            .any(|event| event["eventType"] == "USER_CREATED")
    );
    assert!(
        audit_events
            .iter()
            .any(|event| event["eventType"] == "USER_UPDATED")
    );
    assert!(
        audit_events
            .iter()
            .any(|event| { event["eventType"] == "USER_PLAYBACK_POLICY_UPDATED" })
    );
    assert!(
        audit_events
            .iter()
            .any(|event| event["eventType"] == "USER_DISABLED")
    );
    assert!(
        audit_events
            .iter()
            .any(|event| event["eventType"] == "USER_DELETED")
    );

    let compatibility_logs = client
        .get(format!("{base_url}/api/v1/admin/logs?pageSize=100"))
        .header(COOKIE, &manager_cookie)
        .send()
        .await?;
    assert_eq!(compatibility_logs.status(), reqwest::StatusCode::OK);
    let compatibility_events = compatibility_logs.json::<Value>().await?["events"]
        .as_array()
        .cloned()
        .ok_or("missing compatibility audit events")?;
    assert!(
        compatibility_events
            .iter()
            .any(|event| event["eventType"] == "USER_CREATED")
    );
    assert!(compatibility_events.windows(2).all(|pair| {
        let previous_time = pair[0]["createdAt"].as_i64().unwrap_or_default();
        let next_time = pair[1]["createdAt"].as_i64().unwrap_or_default();
        previous_time > next_time
            || (previous_time == next_time
                && pair[0]["id"].as_str().unwrap_or_default()
                    >= pair[1]["id"].as_str().unwrap_or_default())
    }));
    let mut page_ids = Vec::new();
    for page in [1, 2] {
        let response = client
            .get(format!(
                "{base_url}/api/v1/admin/audit?page={page}&pageSize=1"
            ))
            .header(COOKIE, &manager_cookie)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let body: Value = response.json().await?;
        let events = body["events"].as_array().ok_or("missing audit page")?;
        assert_eq!(events.len(), 1);
        page_ids.push(
            events[0]["id"]
                .as_str()
                .ok_or("missing audit event ID")?
                .to_owned(),
        );
    }
    assert_ne!(page_ids[0], page_ids[1]);

    server.abort();
    Ok(())
}

fn cookie_pair(headers: &reqwest::header::HeaderMap) -> String {
    format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(headers, "lux_session"),
        cookie_value(headers, "lux_csrf")
    )
}

fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> String {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            value
                .strip_prefix(&format!("{name}="))
                .and_then(|value| value.split(';').next())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn admin_user_creation_applies_configured_permission_snapshot()
-> Result<(), Box<dyn std::error::Error>> {
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
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookie = cookie_pair(login.headers());
    let csrf = cookie_value(login.headers(), "lux_csrf");
    let defaults = json!({
        "libraryAccessMode": "NONE",
        "libraryIds": [],
        "canDownload": true,
        "canPlay": false,
        "maxSimultaneousPlaybacks": 5,
        "allowRemux": false,
        "allowAudioTranscode": true,
        "allowVideoTranscode": false,
    });
    let settings = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "newUserPermissions": defaults }))
        .send()
        .await?;
    assert_eq!(settings.status(), reqwest::StatusCode::OK);

    let created = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "snapshot-user",
            "password": "snapshot password"
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let user = created.json::<Value>().await?["user"].clone();
    assert_eq!(user["canDownload"], true);
    let user_id = user["id"].as_str().ok_or("missing user id")?;

    let playback = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{user_id}/playback-policy"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(playback.status(), reqwest::StatusCode::OK);
    assert_eq!(
        playback.json::<Value>().await?["policy"],
        json!({
            "canPlay": false,
            "maxSimultaneousPlaybacks": 5,
            "allowRemux": false,
            "allowAudioTranscode": true,
            "allowVideoTranscode": false,
        })
    );

    let libraries = client
        .get(format!("{base_url}/api/v1/admin/users/{user_id}/libraries"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(libraries.status(), reqwest::StatusCode::OK);
    assert_eq!(
        libraries.json::<Value>().await?["libraryAccessMode"],
        "NONE"
    );

    sqlx::query("DROP TABLE server_settings")
        .execute(database.pool())
        .await?;
    let settings_unavailable = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "settings-unavailable",
            "password": "snapshot password"
        }))
        .send()
        .await?;
    assert_eq!(
        settings_unavailable.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    let missing_user_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE username_normalized = 'settings-unavailable'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(missing_user_count, 0);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_apply_permission_preset_to_user_with_audit_and_guards()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin_user = setup.complete("Admin", "Admin", "correct password").await?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let admin_cookie = cookie_pair(login.headers());
    let admin_csrf = cookie_value(login.headers(), "lux_csrf");
    let created_user = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "username": "preset-target", "password": "target password" }))
        .send()
        .await?;
    assert_eq!(created_user.status(), reqwest::StatusCode::CREATED);
    let target_id = created_user.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing target user ID")?
        .to_owned();

    let created_preset = client
        .post(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "name": "Applied snapshot",
            "isAdmin": false,
            "canManageServer": false,
            "permissions": {
                "libraryAccessMode": "NONE",
                "libraryIds": [],
                "canDownload": true,
                "canPlay": true,
                "maxSimultaneousPlaybacks": 3,
                "allowRemux": false,
                "allowAudioTranscode": true,
                "allowVideoTranscode": false
            }
        }))
        .send()
        .await?;
    assert_eq!(created_preset.status(), reqwest::StatusCode::CREATED);
    let preset = created_preset.json::<Value>().await?["preset"].clone();
    let preset_id = preset["id"].as_str().ok_or("missing preset ID")?;
    let apply_url = format!(
        "{base_url}/api/v1/admin/users/{}/permission-preset",
        target_id.to_uppercase()
    );
    let request = json!({ "presetId": preset_id });

    let missing_csrf = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .json(&request)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let applied = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&request)
        .send()
        .await?;
    assert_eq!(applied.status(), reqwest::StatusCode::NO_CONTENT);

    let playback = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/playback-policy"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(playback.status(), reqwest::StatusCode::OK);
    assert_eq!(
        playback.json::<Value>().await?["policy"],
        json!({
            "canPlay": true,
            "maxSimultaneousPlaybacks": 3,
            "allowRemux": false,
            "allowAudioTranscode": true,
            "allowVideoTranscode": false
        })
    );
    let library_access = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/libraries"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(library_access.status(), reqwest::StatusCode::OK);
    assert_eq!(
        library_access.json::<Value>().await?["libraryAccessMode"],
        "NONE"
    );
    let applied_source: (String, String, i64, i64) = sqlx::query_as(
        "SELECT preset_id, preset_name, preset_version, applied_at
         FROM user_permission_preset_sources WHERE user_id = ?",
    )
    .bind(&target_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(applied_source.0, preset_id);
    assert_eq!(applied_source.1, "Applied snapshot");
    assert_eq!(applied_source.2, 1);

    let expected_source_details = json!({
        "id": preset_id,
        "name": "Applied snapshot",
        "version": 1,
        "appliedAt": applied_source.3,
    });
    let list = client
        .get(format!("{base_url}/api/v1/admin/users?page=1&pageSize=100"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(list.status(), reqwest::StatusCode::OK);
    let listed_users = list.json::<Value>().await?["users"]
        .as_array()
        .cloned()
        .ok_or("missing users list")?;
    let listed_target = listed_users
        .iter()
        .find(|user| user["id"] == target_id)
        .ok_or("missing target user in admin list")?;
    assert_eq!(listed_target["presetSource"], "Applied snapshot");
    assert_eq!(
        listed_target["presetSourceDetails"],
        expected_source_details
    );
    let listed_admin = listed_users
        .iter()
        .find(|user| user["id"] == admin_user.id.to_string())
        .ok_or("missing administrator in admin list")?;
    assert!(listed_admin["presetSource"].is_null());
    assert!(listed_admin["presetSourceDetails"].is_null());

    let profile = client
        .get(format!("{base_url}/api/v1/admin/users/{target_id}"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(profile.status(), reqwest::StatusCode::OK);
    let profile_user = profile.json::<Value>().await?["user"].clone();
    assert_eq!(profile_user["presetSource"], "Applied snapshot");
    assert_eq!(profile_user["presetSourceDetails"], expected_source_details);

    let deleted_preset = client
        .delete(format!(
            "{base_url}/api/v1/admin/user-permission-presets/{preset_id}?version=1"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(deleted_preset.status(), reqwest::StatusCode::NO_CONTENT);
    let preserved_policy = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/playback-policy"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(preserved_policy.status(), reqwest::StatusCode::OK);
    assert_eq!(
        preserved_policy.json::<Value>().await?["policy"]["canPlay"],
        true
    );
    let profile_after_deleting_preset = client
        .get(format!("{base_url}/api/v1/admin/users/{target_id}"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(
        profile_after_deleting_preset.status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        profile_after_deleting_preset.json::<Value>().await?["user"]["presetSourceDetails"],
        expected_source_details
    );

    let target_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "preset-target", "password": "target password" }))
        .send()
        .await?;
    assert_eq!(target_login.status(), reqwest::StatusCode::OK);
    let forbidden = client
        .post(&apply_url)
        .header(COOKIE, cookie_pair(target_login.headers()))
        .header(
            "x-csrf-token",
            cookie_value(target_login.headers(), "lux_csrf"),
        )
        .json(&request)
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    let invalid_user = client
        .post(format!(
            "{base_url}/api/v1/admin/users/not-a-user-id/permission-preset"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&request)
        .send()
        .await?;
    assert_eq!(invalid_user.status(), reqwest::StatusCode::BAD_REQUEST);
    let missing_user = client
        .post(format!(
            "{base_url}/api/v1/admin/users/00000000-0000-4000-8000-000000000000/permission-preset"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&request)
        .send()
        .await?;
    assert_eq!(missing_user.status(), reqwest::StatusCode::NOT_FOUND);
    let missing_preset = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "presetId": "missing-preset" }))
        .send()
        .await?;
    assert_eq!(missing_preset.status(), reqwest::StatusCode::NOT_FOUND);
    let last_manager = client
        .post(format!(
            "{base_url}/api/v1/admin/users/{}/permission-preset",
            admin_user.id
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "presetId": "builtin-user" }))
        .send()
        .await?;
    assert_eq!(last_manager.status(), reqwest::StatusCode::CONFLICT);

    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit?pageSize=100"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let events = audit.json::<Value>().await?["events"]
        .as_array()
        .cloned()
        .ok_or("missing audit events")?;
    assert!(events.iter().any(|event| {
        event["eventType"] == "USER_PERMISSION_PRESET_APPLIED"
            && event["targetId"] == target_id
            && event["metadata"]["presetId"] == preset_id
            && event["metadata"]["version"] == 1
    }));

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_batch_apply_permission_presets_with_per_user_results()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin_user = setup.complete("Admin", "Admin", "correct password").await?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let batch_url = format!("{base_url}/api/v1/admin/users/batch/permission-preset");

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let admin_cookie = cookie_pair(login.headers());
    let admin_csrf = cookie_value(login.headers(), "lux_csrf");

    let unauthenticated = client
        .post(&batch_url)
        .json(&json!({ "userIds": [], "presetId": "builtin-user" }))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);
    let missing_csrf = client
        .post(&batch_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({ "userIds": [], "presetId": "builtin-user" }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let create_user = |username: &'static str| {
        client
            .post(format!("{base_url}/api/v1/admin/users"))
            .header(COOKIE, &admin_cookie)
            .header("x-csrf-token", &admin_csrf)
            .json(&json!({ "username": username, "password": "target password" }))
    };
    let failed_target = create_user("batch-failed-target")
        .send()
        .await?
        .json::<Value>()
        .await?["user"]["id"]
        .as_str()
        .ok_or("missing failed target ID")?
        .to_owned();
    let successful_target = create_user("batch-success-target")
        .send()
        .await?
        .json::<Value>()
        .await?["user"]["id"]
        .as_str()
        .ok_or("missing successful target ID")?
        .to_owned();
    sqlx::query(
        "CREATE TRIGGER fail_batch_permission_preset_source
         BEFORE INSERT ON user_permission_preset_sources
         WHEN NEW.user_id = (
             SELECT id FROM users WHERE username_normalized = 'batch-failed-target'
         )
         BEGIN SELECT RAISE(ABORT, 'injected batch target failure'); END",
    )
    .execute(database.pool())
    .await?;

    let missing_user_id = "00000000-0000-4000-8000-000000000000";
    let body = json!({
        "userIds": [
            "not-a-user-id",
            missing_user_id,
            failed_target,
            successful_target,
            admin_user.id.to_string()
        ],
        "presetId": "builtin-user"
    });
    let applied = client
        .post(&batch_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&body)
        .send()
        .await?;
    let applied_status = applied.status();
    let applied_body = applied.text().await?;
    assert_eq!(applied_status, reqwest::StatusCode::OK, "{applied_body}");
    assert!(!applied_body.to_ascii_lowercase().contains("cookie"));
    assert!(!applied_body.to_ascii_lowercase().contains("token"));
    let response = serde_json::from_str::<Value>(&applied_body)?;
    let results = response["results"].as_array().ok_or("missing results")?;
    assert_eq!(results.len(), 5);
    assert_eq!(results[0]["userId"], "not-a-user-id");
    assert_eq!(results[0]["success"], false);
    assert_eq!(results[0]["errorCode"], "INVALID_USER_ID");
    assert_eq!(results[1]["userId"], missing_user_id);
    assert_eq!(results[1]["errorCode"], "USER_NOT_FOUND");
    assert_eq!(results[2]["userId"], failed_target);
    assert_eq!(results[2]["errorCode"], "STORAGE_UNAVAILABLE");
    assert_eq!(results[3]["userId"], successful_target);
    assert_eq!(results[3]["success"], true);
    assert!(results[3]["errorCode"].is_null());
    assert_eq!(results[4]["userId"], admin_user.id.to_string());
    assert_eq!(results[4]["errorCode"], "LAST_MANAGER");
    assert_eq!(response["preset"]["id"], "builtin-user");

    let applied_source_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_permission_preset_sources WHERE user_id = ?")
            .bind(&successful_target)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(applied_source_count, 1);
    let failed_source_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_permission_preset_sources WHERE user_id = ?")
            .bind(&failed_target)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(failed_source_count, 0);

    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit?pageSize=100"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let events = audit.json::<Value>().await?["events"]
        .as_array()
        .cloned()
        .ok_or("missing audit events")?;
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                event["eventType"] == "USER_PERMISSION_PRESET_APPLIED"
                    && event["targetId"] == successful_target
            })
            .count(),
        1
    );
    assert!(!events.iter().any(|event| {
        event["eventType"] == "USER_PERMISSION_PRESET_APPLIED"
            && (event["targetId"] == failed_target
                || event["targetId"] == admin_user.id.to_string())
    }));

    for invalid_body in [
        json!({ "userIds": [], "presetId": "builtin-user" }),
        json!({ "userIds": [successful_target, successful_target], "presetId": "builtin-user" }),
        json!({
            "userIds": (0..101).map(|index| format!("invalid-{index}")).collect::<Vec<_>>(),
            "presetId": "builtin-user"
        }),
    ] {
        let invalid = client
            .post(&batch_url)
            .header(COOKIE, &admin_cookie)
            .header("x-csrf-token", &admin_csrf)
            .json(&invalid_body)
            .send()
            .await?;
        assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
    }
    let unknown_preset = client
        .post(&batch_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "userIds": [successful_target], "presetId": "missing-preset" }))
        .send()
        .await?;
    assert_eq!(unknown_preset.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_batch_enable_and_disable_users_with_per_user_results()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin_user = setup.complete("Admin", "Admin", "correct password").await?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let batch_url = format!("{base_url}/api/v1/admin/users/batch/status");

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");

    let unauthenticated = client
        .post(&batch_url)
        .json(&json!({ "userIds": [], "isDisabled": true }))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);
    let missing_csrf = client
        .post(&batch_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({ "userIds": [], "isDisabled": true }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let target_created = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "username": "batch-status-target", "password": "target password" }))
        .send()
        .await?;
    let target_status = target_created.status();
    let target_body = target_created.json::<Value>().await?;
    assert_eq!(target_status, reqwest::StatusCode::CREATED, "{target_body}");
    let target_id = target_body["user"]["id"]
        .as_str()
        .ok_or("missing batch status target ID")?
        .to_owned();
    let failed_target_created = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "username": "batch-status-failure",
            "password": "target password"
        }))
        .send()
        .await?;
    let failed_target_body = failed_target_created.json::<Value>().await?;
    let failed_target_id = failed_target_body["user"]["id"]
        .as_str()
        .ok_or("missing failed batch status target ID")?
        .to_owned();
    sqlx::query(
        "CREATE TRIGGER fail_batch_user_status_update
         BEFORE UPDATE OF is_disabled ON users
         WHEN OLD.username_normalized = 'batch-status-failure'
         BEGIN SELECT RAISE(ABORT, 'injected batch status failure'); END",
    )
    .execute(database.pool())
    .await?;

    let target_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "batch-status-target", "password": "target password" }))
        .send()
        .await?;
    assert_eq!(target_login.status(), reqwest::StatusCode::OK);
    let target_cookie = cookie_pair(target_login.headers());

    for invalid_ids in [
        Vec::<String>::new(),
        vec![target_id.clone(), target_id.clone()],
        (0..101).map(|index| format!("invalid-{index}")).collect(),
    ] {
        let invalid = client
            .post(&batch_url)
            .header(COOKIE, &admin_cookie)
            .header("x-csrf-token", &admin_csrf)
            .json(&json!({ "userIds": invalid_ids, "isDisabled": true }))
            .send()
            .await?;
        assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    let missing_user_id = "00000000-0000-4000-8000-000000000000";
    let disabled = client
        .post(&batch_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "userIds": [
                "not-a-user-id",
                missing_user_id,
                failed_target_id,
                target_id,
                admin_user.id.to_string()
            ],
            "isDisabled": true
        }))
        .send()
        .await?;
    let disabled_status = disabled.status();
    let disabled_body = disabled.text().await?;
    assert_eq!(disabled_status, reqwest::StatusCode::OK, "{disabled_body}");
    assert!(!disabled_body.to_ascii_lowercase().contains("cookie"));
    assert!(!disabled_body.to_ascii_lowercase().contains("token"));
    let disabled_results = serde_json::from_str::<Value>(&disabled_body)?["results"]
        .as_array()
        .cloned()
        .ok_or("missing batch status results")?;
    assert_eq!(disabled_results.len(), 5);
    assert_eq!(disabled_results[0]["errorCode"], "INVALID_USER_ID");
    assert_eq!(disabled_results[1]["errorCode"], "USER_NOT_FOUND");
    assert_eq!(disabled_results[2]["userId"], failed_target_id);
    assert_eq!(disabled_results[2]["errorCode"], "STORAGE_UNAVAILABLE");
    assert_eq!(disabled_results[3]["userId"], target_id);
    assert_eq!(disabled_results[3]["success"], true);
    assert_eq!(disabled_results[4]["userId"], admin_user.id.to_string());
    assert_eq!(disabled_results[4]["errorCode"], "LAST_MANAGER");

    let revoked_session = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &target_cookie)
        .send()
        .await?;
    assert_eq!(revoked_session.status(), reqwest::StatusCode::UNAUTHORIZED);
    let disabled_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "batch-status-target", "password": "target password" }))
        .send()
        .await?;
    assert_eq!(disabled_login.status(), reqwest::StatusCode::UNAUTHORIZED);

    let enabled = client
        .post(&batch_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "userIds": [target_id], "isDisabled": false }))
        .send()
        .await?;
    assert_eq!(enabled.status(), reqwest::StatusCode::OK);
    assert_eq!(
        enabled.json::<Value>().await?["results"][0]["success"],
        true
    );
    let enabled_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "batch-status-target", "password": "target password" }))
        .send()
        .await?;
    assert_eq!(enabled_login.status(), reqwest::StatusCode::OK);

    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit?pageSize=100"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let events = audit.json::<Value>().await?["events"]
        .as_array()
        .cloned()
        .ok_or("missing audit events")?;
    for event_type in ["USER_DISABLED", "USER_ENABLED"] {
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event["eventType"] == event_type && event["targetId"] == target_id
                })
                .count(),
            1
        );
    }
    assert!(!events.iter().any(|event| {
        event["eventType"] == "USER_DISABLED" && event["targetId"] == admin_user.id.to_string()
    }));
    assert!(!events.iter().any(|event| {
        event["eventType"] == "USER_DISABLED" && event["targetId"] == failed_target_id
    }));

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_preview_and_copy_account_preferences() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let preview_url = format!("{base_url}/api/v1/admin/users/account-preferences/copy/preview");

    let preview_request = json!({
        "sourceUserId": "00000000-0000-4000-8000-000000000001",
        "targetUserIds": ["00000000-0000-4000-8000-000000000002"],
        "groups": { "appearance": true, "home": false, "playback": false }
    });
    let unauthenticated = client
        .post(&preview_url)
        .json(&preview_request)
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let admin_cookie = cookie_pair(login.headers());
    let admin_csrf = cookie_value(login.headers(), "lux_csrf");
    let create_user = |username: &'static str| {
        client
            .post(format!("{base_url}/api/v1/admin/users"))
            .header(COOKIE, &admin_cookie)
            .header("x-csrf-token", &admin_csrf)
            .json(&json!({ "username": username, "password": "target password" }))
    };
    let source = create_user("preferences-copy-source")
        .send()
        .await?
        .json::<Value>()
        .await?["user"]["id"]
        .as_str()
        .ok_or("missing source user ID")?
        .to_owned();
    let target = create_user("preferences-copy-target")
        .send()
        .await?
        .json::<Value>()
        .await?["user"]["id"]
        .as_str()
        .ok_or("missing target user ID")?
        .to_owned();
    let conflict_target = create_user("preferences-copy-conflict")
        .send()
        .await?
        .json::<Value>()
        .await?["user"]["id"]
        .as_str()
        .ok_or("missing conflict target ID")?
        .to_owned();
    create_user("preferences-copy-non-admin").send().await?;
    sqlx::query(
        "INSERT INTO user_account_preferences
         (user_id, theme, accent_color, show_media_libraries, show_continue_watching,
          audio_language, subtitle_language, auto_play_next_episode, version)
         VALUES (?, 'light', 'berry', 0, 1, 'Japanese', 'English', 0, 3),
                (?, 'dark', 'ocean', 1, 0, 'Chinese', 'Chinese', 1, 4),
                (?, 'dark', 'mint', 1, 1, 'French', 'French', 0, 2)",
    )
    .bind(&source)
    .bind(&target)
    .bind(&conflict_target)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind)
         VALUES ('copy-library-shared', 'Shared Library', 'MOVIE'),
                ('copy-library-private', 'Private Library', 'MOVIE')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE users SET library_access_mode = 'SELECTED' WHERE id = ?")
        .bind(&target)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE users SET library_access_mode = 'ALL' WHERE id = ?")
        .bind(&conflict_target)
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, 'copy-library-shared', 1)",
    )
    .bind(&target)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_library_order (user_id, library_id, position)
         VALUES (?, 'copy-library-shared', 0), (?, 'copy-library-private', 1),
                (?, 'copy-library-private', 0)",
    )
    .bind(&source)
    .bind(&source)
    .bind(&target)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_playback_settings (user_id, played_percent)
         VALUES (?, 67), (?, 42)",
    )
    .bind(&source)
    .bind(&target)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_library_order_preferences (user_id, use_admin_library_order)
         VALUES (?, 1)",
    )
    .bind(&target)
    .execute(database.pool())
    .await?;

    let unavailable_target = client
        .post(&preview_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({
            "sourceUserId": source,
            "targetUserIds": ["00000000-0000-4000-8000-000000000099"],
            "groups": { "appearance": true, "home": false, "playback": false }
        }))
        .send()
        .await?;
    assert_eq!(unavailable_target.status(), reqwest::StatusCode::NOT_FOUND);

    let non_admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({
            "username": "preferences-copy-non-admin",
            "password": "target password"
        }))
        .send()
        .await?;
    assert_eq!(non_admin_login.status(), reqwest::StatusCode::OK);
    let non_admin = client
        .post(&preview_url)
        .header(COOKIE, cookie_pair(non_admin_login.headers()))
        .json(&json!({
            "sourceUserId": source,
            "targetUserIds": [target],
            "groups": { "appearance": true, "home": false, "playback": false }
        }))
        .send()
        .await?;
    assert_eq!(non_admin.status(), reqwest::StatusCode::FORBIDDEN);

    let empty_groups = client
        .post(&preview_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({
            "sourceUserId": source,
            "targetUserIds": [target],
            "groups": { "appearance": false, "home": false, "playback": false }
        }))
        .send()
        .await?;
    assert_eq!(empty_groups.status(), reqwest::StatusCode::BAD_REQUEST);

    let duplicate_targets = client
        .post(&preview_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({
            "sourceUserId": source,
            "targetUserIds": [target, target],
            "groups": { "appearance": true, "home": false, "playback": false }
        }))
        .send()
        .await?;
    assert_eq!(duplicate_targets.status(), reqwest::StatusCode::BAD_REQUEST);

    let invalid_user_id = client
        .post(&preview_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({
            "sourceUserId": "not-a-user-id",
            "targetUserIds": [target],
            "groups": { "appearance": true, "home": false, "playback": false }
        }))
        .send()
        .await?;
    assert_eq!(invalid_user_id.status(), reqwest::StatusCode::BAD_REQUEST);

    let preview = client
        .post(&preview_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({
            "sourceUserId": source,
            "targetUserIds": [target, conflict_target],
            "groups": { "appearance": true, "home": false, "playback": true, "libraryOrder": true }
        }))
        .send()
        .await?;
    let preview_status = preview.status();
    let preview_body = preview.text().await?;
    assert_eq!(preview_status, reqwest::StatusCode::OK, "{preview_body}");
    let preview = serde_json::from_str::<Value>(&preview_body)?;
    assert_eq!(preview["sourceVersion"], 3);
    assert_eq!(preview["sourceSnapshot"]["appearance"]["theme"], "light");
    assert!(preview["sourceSnapshot"].get("home").is_none());
    assert_eq!(preview["sourceSnapshot"]["playback"]["playedPercent"], 67);
    assert_eq!(
        preview["sourceSnapshot"]["libraryOrder"],
        json!(["copy-library-shared", "copy-library-private"])
    );
    assert_eq!(preview["targets"][0]["expectedVersion"], 4);
    assert_eq!(preview["targets"][1]["expectedVersion"], 2);
    assert_eq!(
        preview["targets"][0]["libraryOrder"],
        json!(["copy-library-shared"])
    );
    assert_eq!(
        preview["targets"][1]["libraryOrder"],
        json!(["copy-library-shared", "copy-library-private"])
    );
    sqlx::query("UPDATE user_account_preferences SET version = version + 1 WHERE user_id = ?")
        .bind(&conflict_target)
        .execute(database.pool())
        .await?;

    let apply_url = format!("{base_url}/api/v1/admin/users/account-preferences/copy");
    let apply_targets = preview["targets"]
        .as_array()
        .ok_or("missing preview targets")?
        .iter()
        .map(|target| {
            json!({
                "userId": target["userId"],
                "expectedVersion": target["expectedVersion"]
            })
        })
        .collect::<Vec<_>>();
    let apply_request = json!({
        "sourceUserId": source,
        "sourceExpectedVersion": preview["sourceVersion"],
        "targets": apply_targets,
        "groups": { "appearance": true, "home": false, "playback": true, "libraryOrder": true }
    });
    let missing_csrf = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .json(&apply_request)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let applied = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&apply_request)
        .send()
        .await?;
    let applied_status = applied.status();
    let applied_body = applied.text().await?;
    assert_eq!(applied_status, reqwest::StatusCode::OK, "{applied_body}");
    let applied = serde_json::from_str::<Value>(&applied_body)?;
    assert_eq!(applied["results"][0]["success"], true);
    assert_eq!(applied["results"][1]["success"], false);
    assert_eq!(applied["results"][1]["errorCode"], "VERSION_CONFLICT");
    assert!(!applied_body.to_ascii_lowercase().contains("cookie"));
    assert!(!applied_body.to_ascii_lowercase().contains("token"));

    let stored: (String, String, i64, String) = sqlx::query_as(
        "SELECT theme, accent_color, show_media_libraries, audio_language
         FROM user_account_preferences WHERE user_id = ?",
    )
    .bind(&target)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored.0, "light");
    assert_eq!(stored.1, "berry");
    assert_eq!(stored.2, 1);
    assert_eq!(stored.3, "Japanese");
    let copied_library_order: Vec<String> = sqlx::query_scalar(
        "SELECT library_id FROM user_library_order WHERE user_id = ? ORDER BY position",
    )
    .bind(&target)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(copied_library_order, ["copy-library-shared"]);
    let copied_played_percent: i64 =
        sqlx::query_scalar("SELECT played_percent FROM user_playback_settings WHERE user_id = ?")
            .bind(&target)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(copied_played_percent, 67);
    let use_admin_library_order: i64 = sqlx::query_scalar(
        "SELECT use_admin_library_order FROM user_library_order_preferences WHERE user_id = ?",
    )
    .bind(&target)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(use_admin_library_order, 1);

    sqlx::query("UPDATE user_account_preferences SET version = version + 1 WHERE user_id = ?")
        .bind(&source)
        .execute(database.pool())
        .await?;
    let stale_source = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&apply_request)
        .send()
        .await?;
    assert_eq!(stale_source.status(), reqwest::StatusCode::CONFLICT);

    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit?pageSize=100"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let events = audit.json::<Value>().await?["events"]
        .as_array()
        .cloned()
        .ok_or("missing audit events")?;
    let copy_event = events
        .iter()
        .find(|event| {
            event["eventType"] == "USER_ACCOUNT_PREFERENCES_COPIED" && event["targetId"] == target
        })
        .ok_or("missing account preferences copy audit event")?;
    assert_eq!(copy_event["metadata"]["sourceUserId"], source);
    assert_eq!(copy_event["metadata"]["groups"]["appearance"], true);
    assert_eq!(copy_event["metadata"]["groups"]["home"], false);
    assert_eq!(copy_event["metadata"]["groups"]["playback"], true);
    assert_eq!(copy_event["metadata"]["groups"]["libraryOrder"], true);
    assert!(!copy_event["metadata"].to_string().contains("light"));
    assert!(
        !copy_event["metadata"]
            .to_string()
            .to_ascii_lowercase()
            .contains("token")
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_user_profile_api_normalizes_email_and_preserves_verification_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin_user = setup.complete("Admin", "Admin", "correct password").await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookie = cookie_pair(login.headers());
    let csrf = cookie_value(login.headers(), "lux_csrf");

    let legacy_profile = client
        .get(format!("{base_url}/api/v1/admin/users/{}", admin_user.id))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(legacy_profile.status(), reqwest::StatusCode::OK);
    let legacy_user = legacy_profile.json::<Value>().await?["user"].clone();
    assert_eq!(legacy_user["email"], Value::Null);
    assert_eq!(legacy_user["emailVerifiedAt"], Value::Null);
    assert_eq!(legacy_user["isDisabled"], false);

    let created = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "profile-managed",
            "displayName": "Profile Managed",
            "password": "managed password",
            "email": " Managed@Example.COM "
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let user_id = created.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing created user id")?
        .to_owned();
    let profile_url = format!("{base_url}/api/v1/admin/users/{user_id}/profile");

    let initial_profile = client
        .get(format!("{base_url}/api/v1/admin/users/{user_id}"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(initial_profile.status(), reqwest::StatusCode::OK);
    let initial_user = initial_profile.json::<Value>().await?["user"].clone();
    assert_eq!(initial_user["email"], "managed@example.com");
    assert_eq!(initial_user["emailVerifiedAt"], Value::Null);

    sqlx::query("UPDATE users SET email_verified_at = 123 WHERE id = ?")
        .bind(&user_id)
        .execute(database.pool())
        .await?;
    let missing_csrf = client
        .patch(&profile_url)
        .header(COOKIE, &cookie)
        .json(&json!({ "displayName": "Must Not Save" }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let display_update = client
        .patch(&profile_url)
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "displayName": "Managed Display" }))
        .send()
        .await?;
    assert_eq!(display_update.status(), reqwest::StatusCode::OK);
    let display_user = display_update.json::<Value>().await?["user"].clone();
    assert_eq!(display_user["email"], "managed@example.com");
    assert_eq!(display_user["emailVerifiedAt"], 123);

    let email_update = client
        .patch(&profile_url)
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "email": " New@Example.COM " }))
        .send()
        .await?;
    assert_eq!(email_update.status(), reqwest::StatusCode::OK);
    let updated_user = email_update.json::<Value>().await?["user"].clone();
    assert_eq!(updated_user["email"], "new@example.com");
    assert_eq!(updated_user["emailVerifiedAt"], Value::Null);

    sqlx::query("UPDATE users SET email_verified_at = 456 WHERE id = ?")
        .bind(&user_id)
        .execute(database.pool())
        .await?;
    let same_email_update = client
        .patch(&profile_url)
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "email": "NEW@example.com" }))
        .send()
        .await?;
    assert_eq!(same_email_update.status(), reqwest::StatusCode::OK);
    assert_eq!(
        same_email_update.json::<Value>().await?["user"]["emailVerifiedAt"],
        456
    );

    let duplicate_email = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "profile-duplicate",
            "displayName": "Duplicate Email",
            "password": "duplicate password",
            "email": "new@EXAMPLE.com"
        }))
        .send()
        .await?;
    assert_eq!(duplicate_email.status(), reqwest::StatusCode::CONFLICT);

    let invalid_email = client
        .patch(&profile_url)
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "email": "not-an-email" }))
        .send()
        .await?;
    assert_eq!(invalid_email.status(), reqwest::StatusCode::BAD_REQUEST);
    let invalid_body = invalid_email.text().await?;
    assert!(!invalid_body.contains("sqlx"));
    assert!(!invalid_body.contains("database"));
    let unchanged_profile = client
        .get(format!("{base_url}/api/v1/admin/users/{user_id}"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    let unchanged_user = unchanged_profile.json::<Value>().await?["user"].clone();
    assert_eq!(unchanged_user["email"], "new@example.com");
    assert_eq!(unchanged_user["emailVerifiedAt"], 456);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_read_content_restriction_index_readiness()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "admin password").await?;
    let users = UserStore::new(database.clone())?;
    users
        .create_user("reader", "Reader", "reader password", false)
        .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Restriction Readiness", LibraryKind::Movie, false)
        .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('restriction-readiness-item', ?, 'MOVIE', 'Readiness', 'Readiness', 'LOCAL_CONFIRMED')",
    )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config, database, setup, web_auth, emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let url = format!("{base_url}/api/v1/admin/content-restriction-index");

    let anonymous = client.get(&url).send().await?;
    assert_eq!(anonymous.status(), reqwest::StatusCode::UNAUTHORIZED);

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "admin password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_response = client.get(&url).header(COOKIE, admin_cookie).send().await?;
    assert_eq!(admin_response.status(), reqwest::StatusCode::OK);
    let readiness = admin_response.json::<Value>().await?;
    assert_eq!(readiness["totalItems"], 1);
    assert_eq!(readiness["indexedItems"], 0);
    assert_eq!(readiness["pendingItems"], 1);
    assert_eq!(readiness.as_object().map(|body| body.len()), Some(3));

    let reader_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "reader", "password": "reader password" }))
        .send()
        .await?;
    let reader_cookie = cookie_pair(reader_login.headers());
    let reader_response = client
        .get(&url)
        .header(COOKIE, reader_cookie)
        .send()
        .await?;
    assert_eq!(reader_response.status(), reqwest::StatusCode::FORBIDDEN);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_preview_and_apply_user_permission_groups()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let users = UserStore::new(database.clone())?;
    let source_id = users
        .create_user(
            "permission-source",
            "Permission Source",
            "user password",
            false,
        )
        .await?
        .id
        .to_string();
    let success_target_id = users
        .create_user(
            "permission-target",
            "Permission Target",
            "user password",
            false,
        )
        .await?
        .id
        .to_string();
    let stale_target_id = users
        .create_user(
            "permission-stale-target",
            "Permission Stale Target",
            "user password",
            false,
        )
        .await?
        .id
        .to_string();
    let source_stale_target_id = users
        .create_user(
            "permission-source-stale-target",
            "Permission Source Stale Target",
            "user password",
            false,
        )
        .await?
        .id
        .to_string();
    let _reader_id = users
        .create_user(
            "permission-reader",
            "Permission Reader",
            "user password",
            false,
        )
        .await?
        .id
        .to_string();

    sqlx::query("UPDATE users SET library_access_mode = 'NONE' WHERE id = ?")
        .bind(&source_id)
        .execute(database.pool())
        .await?;
    for target_id in [
        &success_target_id,
        &stale_target_id,
        &source_stale_target_id,
    ] {
        sqlx::query("UPDATE users SET library_access_mode = 'ALL' WHERE id = ?")
            .bind(target_id)
            .execute(database.pool())
            .await?;
    }

    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let preview_url = format!("{base_url}/api/v1/admin/users/permission-copy/preview");
    let apply_url = format!("{base_url}/api/v1/admin/users/permission-copy");
    let preview_request = json!({
        "sourceUserId": source_id,
        "targetUserIds": [&success_target_id, &stale_target_id],
        "groups": { "library": true, "content": false, "playback": false }
    });

    let anonymous_preview = client
        .post(&preview_url)
        .json(&preview_request)
        .send()
        .await?;
    assert_eq!(
        anonymous_preview.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    let reader_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "permission-reader", "password": "user password" }))
        .send()
        .await?;
    assert_eq!(reader_login.status(), reqwest::StatusCode::OK);
    let non_admin_preview = client
        .post(&preview_url)
        .header(COOKIE, cookie_pair(reader_login.headers()))
        .json(&preview_request)
        .send()
        .await?;
    assert_eq!(non_admin_preview.status(), reqwest::StatusCode::FORBIDDEN);

    let invalid_preview = client
        .post(&preview_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({
            "sourceUserId": source_id,
            "targetUserIds": [&success_target_id],
            "groups": { "library": false, "content": false, "playback": false }
        }))
        .send()
        .await?;
    assert_eq!(invalid_preview.status(), reqwest::StatusCode::BAD_REQUEST);

    let mut unknown_field_request = preview_request.clone();
    unknown_field_request["unexpected"] = json!(true);
    let unknown_field_preview = client
        .post(&preview_url)
        .header(COOKIE, &admin_cookie)
        .json(&unknown_field_request)
        .send()
        .await?;
    assert_eq!(
        unknown_field_preview.status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    assert_eq!(
        unknown_field_preview.json::<Value>().await?["error"]["code"],
        "INVALID_REQUEST"
    );

    let preview_response = client
        .post(&preview_url)
        .header(COOKIE, &admin_cookie)
        .json(&preview_request)
        .send()
        .await?;
    let preview_status = preview_response.status();
    let preview_body = preview_response.text().await?;
    assert_eq!(preview_status, reqwest::StatusCode::OK, "{preview_body}");
    let preview = serde_json::from_str::<Value>(&preview_body)?;
    assert_eq!(preview["groups"]["library"], true);
    assert_eq!(preview["sourceSnapshot"]["library"]["accessMode"], "NONE");
    assert!(preview["sourceSnapshot"].get("content").is_none());
    assert!(preview["sourceSnapshot"].get("playback").is_none());
    assert!(preview["sourceSnapshot"].get("isAdmin").is_none());

    let apply_request = json!({
        "sourceUserId": source_id,
        "sourceVersion": preview["sourceVersion"],
        "targets": preview["targets"],
        "groups": preview["groups"]
    });
    let missing_csrf = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .json(&apply_request)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    sqlx::query("UPDATE users SET library_access_mode = 'NONE' WHERE id = ?")
        .bind(&stale_target_id)
        .execute(database.pool())
        .await?;
    let applied = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&apply_request)
        .send()
        .await?;
    let applied_status = applied.status();
    let applied_body = applied.text().await?;
    assert_eq!(applied_status, reqwest::StatusCode::OK, "{applied_body}");
    let applied = serde_json::from_str::<Value>(&applied_body)?;
    assert_eq!(applied["results"][0]["success"], true);
    assert!(applied["results"][0]["errorCode"].is_null());
    assert_eq!(applied["results"][1]["success"], false);
    assert_eq!(applied["results"][1]["errorCode"], "VERSION_CONFLICT");

    let successful_target_mode: String =
        sqlx::query_scalar("SELECT library_access_mode FROM users WHERE id = ?")
            .bind(&success_target_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(successful_target_mode, "NONE");
    assert!(!applied_body.to_ascii_lowercase().contains("cookie"));
    assert!(!applied_body.to_ascii_lowercase().contains("token"));

    let source_stale_preview = client
        .post(&preview_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({
            "sourceUserId": source_id,
            "targetUserIds": [&source_stale_target_id],
            "groups": { "library": true, "content": false, "playback": false }
        }))
        .send()
        .await?
        .json::<Value>()
        .await?;
    let source_stale_apply_request = json!({
        "sourceUserId": source_id,
        "sourceVersion": source_stale_preview["sourceVersion"],
        "targets": source_stale_preview["targets"],
        "groups": source_stale_preview["groups"]
    });
    sqlx::query("UPDATE users SET library_access_mode = 'ALL' WHERE id = ?")
        .bind(&source_id)
        .execute(database.pool())
        .await?;
    let source_stale_apply = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&source_stale_apply_request)
        .send()
        .await?;
    assert_eq!(source_stale_apply.status(), reqwest::StatusCode::CONFLICT);
    let unchanged_source_stale_target_mode: String =
        sqlx::query_scalar("SELECT library_access_mode FROM users WHERE id = ?")
            .bind(&source_stale_target_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(unchanged_source_stale_target_mode, "ALL");

    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit?pageSize=100"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let events = audit.json::<Value>().await?["events"]
        .as_array()
        .cloned()
        .ok_or("missing audit events")?;
    let copy_event = events
        .iter()
        .find(|event| {
            event["eventType"] == "USER_PERMISSION_GROUPS_COPIED"
                && event["targetId"] == success_target_id
        })
        .ok_or("missing permission copy audit event")?;
    assert_eq!(copy_event["metadata"]["sourceUserId"], source_id);
    assert_eq!(copy_event["metadata"]["groups"]["library"], true);
    assert_eq!(copy_event["metadata"]["groups"]["content"], false);
    assert_eq!(copy_event["metadata"]["groups"]["playback"], false);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_manage_user_content_restrictions() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "admin password").await?;
    let users = UserStore::new(database.clone())?;
    let reader = users
        .create_user("reader", "Reader", "reader password", false)
        .await?;
    let reader_id = reader.id.to_string();
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config, database, setup, web_auth, emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let url = format!("{base_url}/api/v1/admin/users/{reader_id}/content-restrictions");

    let anonymous = client.get(&url).send().await?;
    assert_eq!(anonymous.status(), reqwest::StatusCode::UNAUTHORIZED);

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "admin password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    let initial = client
        .get(&url)
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(initial.status(), reqwest::StatusCode::OK);
    assert_eq!(initial.json::<Value>().await?["policy"], Value::Null);

    let reader_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "reader", "password": "reader password" }))
        .send()
        .await?;
    let reader_cookie = cookie_pair(reader_login.headers());
    let reader_read = client
        .get(&url)
        .header(COOKIE, &reader_cookie)
        .send()
        .await?;
    assert_eq!(reader_read.status(), reqwest::StatusCode::FORBIDDEN);
    let reader_write = client
        .put(&url)
        .header(COOKIE, &reader_cookie)
        .header(
            "x-csrf-token",
            cookie_value(reader_login.headers(), "lux_csrf"),
        )
        .json(&json!({
            "expectedVersion": 0,
            "ratingSystem": "MPA",
            "maximumRating": "PG-13",
            "allowUnrated": false,
            "allowedTags": [],
            "blockedTags": []
        }))
        .send()
        .await?;
    assert_eq!(reader_write.status(), reqwest::StatusCode::FORBIDDEN);

    let policy = json!({
        "expectedVersion": 0,
        "ratingSystem": "mpa",
        "maximumRating": "pg-13",
        "allowUnrated": false,
        "allowedTags": ["  Action  ", "Drama"],
        "blockedTags": ["Drama"]
    });
    let missing_csrf = client
        .put(&url)
        .header(COOKIE, &admin_cookie)
        .json(&policy)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let invalid_rating = client
        .put(&url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "expectedVersion": 0,
            "ratingSystem": "MPA",
            "maximumRating": "TV-MA",
            "allowUnrated": false,
            "allowedTags": [],
            "blockedTags": []
        }))
        .send()
        .await?;
    assert_eq!(invalid_rating.status(), reqwest::StatusCode::BAD_REQUEST);

    let duplicate_tags = client
        .put(&url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "expectedVersion": 0,
            "ratingSystem": "MPA",
            "maximumRating": null,
            "allowUnrated": true,
            "allowedTags": ["Drama", " drama "],
            "blockedTags": []
        }))
        .send()
        .await?;
    assert_eq!(duplicate_tags.status(), reqwest::StatusCode::BAD_REQUEST);

    let too_many_tags = client
        .put(&url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "expectedVersion": 0,
            "ratingSystem": "MPA",
            "maximumRating": null,
            "allowUnrated": true,
            "allowedTags": (0..101).map(|index| format!("tag-{index}")).collect::<Vec<_>>(),
            "blockedTags": []
        }))
        .send()
        .await?;
    assert_eq!(too_many_tags.status(), reqwest::StatusCode::BAD_REQUEST);

    let created = client
        .put(&url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&policy)
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::OK);
    let created_policy = created.json::<Value>().await?["policy"].clone();
    assert_eq!(created_policy["ratingSystem"], "MPA");
    assert_eq!(created_policy["maximumRating"], "PG-13");
    assert_eq!(created_policy["version"], 1);
    assert_eq!(created_policy["allowedTags"], json!(["action", "drama"]));
    assert_eq!(created_policy["blockedTags"], json!(["drama"]));

    let stale = client
        .put(&url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&policy)
        .send()
        .await?;
    assert_eq!(stale.status(), reqwest::StatusCode::CONFLICT);

    let update = client
        .put(&url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "expectedVersion": 1,
            "ratingSystem": "BBFC",
            "maximumRating": "12A",
            "allowUnrated": true,
            "allowedTags": [],
            "blockedTags": ["Horror"]
        }))
        .send()
        .await?;
    assert_eq!(update.status(), reqwest::StatusCode::OK);
    let updated_policy = update.json::<Value>().await?["policy"].clone();
    assert_eq!(updated_policy["version"], 2);
    assert_eq!(updated_policy["ratingSystem"], "BBFC");
    assert_eq!(updated_policy["blockedTags"], json!(["horror"]));

    let readback = client
        .get(&url)
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(readback.status(), reqwest::StatusCode::OK);
    assert_eq!(readback.json::<Value>().await?["policy"], updated_policy);

    let missing_user_url = format!(
        "{base_url}/api/v1/admin/users/{}/content-restrictions",
        uuid::Uuid::now_v7()
    );
    let missing_user = client
        .put(&missing_user_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "expectedVersion": 0,
            "ratingSystem": "MPA",
            "maximumRating": null,
            "allowUnrated": true,
            "allowedTags": [],
            "blockedTags": []
        }))
        .send()
        .await?;
    assert_eq!(missing_user.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_view_and_stop_only_user_server_controlled_playback()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "admin password").await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder().no_proxy().build()?;

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "admin password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    let created = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "username": "playback-target", "password": "target password" }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let user_id = created.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing playback target user id")?
        .to_owned();
    let target_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "playback-target", "password": "target password" }))
        .send()
        .await?;
    let target_cookie = cookie_pair(target_login.headers());

    let library = LibraryService::new(database.clone())
        .create_library("User playback", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Playback.Item.2024.mkv"), b"fixture").await?;
    LibraryService::new(database.clone())
        .add_root(library.id, root.to_str().ok_or("non-UTF-8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE title = 'Playback Item' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let hls_playback_id = uuid::Uuid::now_v7().to_string();
    let direct_playback_id = uuid::Uuid::now_v7().to_string();
    let other_user_playback_id = uuid::Uuid::now_v7().to_string();
    for (id, play_session_id, client_name, last_event_age) in [
        (
            hls_playback_id.as_str(),
            "lux-web:web-hls-session",
            "Web Player",
            1_i64,
        ),
        (
            direct_playback_id.as_str(),
            "direct-play-session",
            "External Player",
            2_i64,
        ),
    ] {
        sqlx::query(
            "INSERT INTO playback_sessions (
                    id, user_id, item_id, play_session_id, device_id, client,
                    device_name, state, position_ticks, duration_ticks, last_event_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, 'PLAYING', 10000000, 100000000,
                           unixepoch() - ?)",
        )
        .bind(id)
        .bind(&user_id)
        .bind(&item_id)
        .bind(play_session_id)
        .bind(format!("device-{id}"))
        .bind(client_name)
        .bind(format!("{client_name} device"))
        .bind(last_event_age)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO web_playback_sessions (
                id, user_id, item_id, play_session_id, tier, plan, state, is_admin,
                expires_at, last_heartbeat_at
             ) VALUES (
                'web-hls-session', ?, ?, 'lux-web:web-hls-session', 4,
                'SERVER_HLS', 'ACTIVE', 0, unixepoch() + 900, unixepoch()
             )",
    )
    .bind(&user_id)
    .bind(&item_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO playback_admissions (
                user_id, play_session_id, state, admitted_at, last_seen_at
             ) VALUES (?, 'lux-web:web-hls-session', 'ACTIVE', unixepoch() - 5, unixepoch())",
    )
    .bind(&user_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO playback_sessions (
                id, user_id, item_id, play_session_id, device_id, state, last_event_at
             ) VALUES (?, ?, ?, 'other-user-session', 'admin-device', 'PLAYING', unixepoch())",
    )
    .bind(&other_user_playback_id)
    .bind(admin.id.to_string())
    .bind(&item_id)
    .execute(database.pool())
    .await?;

    let unauthenticated = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{user_id}/playback-sessions"
        ))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);
    let forbidden = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{user_id}/playback-sessions"
        ))
        .header(COOKIE, &target_cookie)
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    let first_page = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{user_id}/playback-sessions?page=1&pageSize=1"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(first_page.status(), reqwest::StatusCode::OK);
    let first_body = first_page.json::<Value>().await?;
    assert_eq!(first_body["totalCount"], 2);
    assert_eq!(first_body["pageSize"], 1);
    assert_eq!(first_body["sessions"][0]["id"], hls_playback_id);
    assert_eq!(first_body["sessions"][0]["title"], "Playback Item");
    assert_eq!(first_body["sessions"][0]["canStop"], true);
    let second_page = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{user_id}/playback-sessions?page=2&pageSize=1"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    let second_body = second_page.json::<Value>().await?;
    assert_eq!(second_body["totalCount"], 2);
    assert_eq!(second_body["sessions"][0]["id"], direct_playback_id);
    assert_eq!(second_body["sessions"][0]["canStop"], false);

    let other_user_stop = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{user_id}/playback-sessions/{other_user_playback_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(other_user_stop.status(), reqwest::StatusCode::NOT_FOUND);
    let other_user_state: String =
        sqlx::query_scalar("SELECT state FROM playback_sessions WHERE id = ?")
            .bind(&other_user_playback_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(other_user_state, "PLAYING");

    let hls_url =
        format!("{base_url}/api/v1/admin/users/{user_id}/playback-sessions/{hls_playback_id}");
    let missing_csrf = client
        .delete(&hls_url)
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);
    let direct_stop = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{user_id}/playback-sessions/{direct_playback_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(direct_stop.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        direct_stop.json::<Value>().await?["error"]["code"],
        "INVALID_REQUEST"
    );
    let direct_state: String =
        sqlx::query_scalar("SELECT state FROM playback_sessions WHERE id = ?")
            .bind(&direct_playback_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(direct_state, "PLAYING");

    let stopped = client
        .delete(&hls_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(stopped.status(), reqwest::StatusCode::NO_CONTENT);
    let hls_state: String = sqlx::query_scalar("SELECT state FROM playback_sessions WHERE id = ?")
        .bind(&hls_playback_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(hls_state, "STOPPED");
    let admission_state: String = sqlx::query_scalar(
        "SELECT state FROM playback_admissions
         WHERE user_id = ? AND play_session_id = 'lux-web:web-hls-session'",
    )
    .bind(&user_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(admission_state, "RELEASED");
    let web_hls_state: String =
        sqlx::query_scalar("SELECT state FROM web_playback_sessions WHERE id = 'web-hls-session'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(web_hls_state, "STOPPED");
    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit?pageSize=50"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    assert!(
        audit.json::<Value>().await?["events"]
            .as_array()
            .is_some_and(|events| events.iter().any(|event| {
                event["eventType"] == "USER_PLAYBACK_STOPPED"
                    && event["targetId"] == hls_playback_id
            }))
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_list_user_devices_with_pagination_and_auth()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "admin password").await?;
    let target = UserStore::new(database.clone())?
        .create_user("device-target", "Device Target", "target password", false)
        .await?;
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder().no_proxy().build()?;
    let path = format!("{base_url}/api/v1/admin/users/{}/devices", target.id);

    let unauthenticated = client.get(&path).send().await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "admin password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let target_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({
            "username": "device-target",
            "password": "target password"
        }))
        .send()
        .await?;
    let target_cookie = cookie_pair(target_login.headers());
    let forbidden = client
        .get(&path)
        .header(COOKIE, &target_cookie)
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    for (device_id, name, client_name, seen_at) in [
        ("device-b", "Phone B", "Infuse", 300_i64),
        ("device-a", "Phone A", "Lux Web", 300_i64),
        ("device-c", "Tablet C", "Kodi", 200_i64),
    ] {
        sqlx::query(
            "INSERT INTO user_devices (
                 user_id, device_id, device_name, device_type, client_name,
                 client_version, first_seen_at, last_seen_at, last_remote_ip
             ) VALUES (?, ?, ?, 'TEST', ?, '1.2.3', ?, ?, '203.0.113.10')",
        )
        .bind(target.id.to_string())
        .bind(device_id)
        .bind(name)
        .bind(client_name)
        .bind(seen_at - 10)
        .bind(seen_at)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO web_sessions (
             id, user_id, session_token_hash, csrf_token_hash, expires_at, device_id
         ) VALUES ('device-a-web-session', ?, X'31', X'41', unixepoch() + 3600, 'device-a'),
                  ('device-c-expired-web-session', ?, X'32', X'42', unixepoch() - 1, 'device-c')",
    )
    .bind(target.id.to_string())
    .bind(target.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO access_tokens (
             id, token_hash, user_id, device_id, client_name, device_name,
             client_version, created_at, updated_at, revoked_at
         ) VALUES ('device-b-emby-token', X'51', ?, 'device-b', 'Emby', 'Phone B', '1.0', unixepoch(), unixepoch(), NULL),
                  ('device-c-revoked-emby-token', X'52', ?, 'device-c', 'Emby', 'Tablet C', '1.0', unixepoch(), unixepoch(), unixepoch())",
    )
    .bind(target.id.to_string())
    .bind(target.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_devices (
             user_id, device_id, device_name, device_type, client_name,
             client_version, first_seen_at, last_seen_at
         ) VALUES (?, 'admin-private-device', 'Admin Device', 'WEB', 'Lux Web', '1.0', 1, 999)",
    )
    .bind(admin.id.to_string())
    .execute(database.pool())
    .await?;

    let first_page = client
        .get(format!("{path}?page=1&pageSize=1"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(first_page.status(), reqwest::StatusCode::OK);
    let first_body = first_page.json::<Value>().await?;
    assert_eq!(first_body["page"], 1);
    assert_eq!(first_body["pageSize"], 1);
    assert_eq!(first_body["totalCount"], 3);
    assert_eq!(first_body["devices"].as_array().map(Vec::len), Some(1));
    let device = &first_body["devices"][0];
    assert_eq!(device["deviceId"], "device-a");
    assert_eq!(device["hasActiveLogin"], true);
    assert_eq!(device["deviceName"], "Phone A");
    assert_eq!(device["deviceType"], "TEST");
    assert_eq!(device["clientName"], "Lux Web");
    assert_eq!(device["clientVersion"], "1.2.3");
    assert_eq!(device["firstSeenAt"], 290);
    assert_eq!(device["lastSeenAt"], 300);
    assert_eq!(device["lastRemoteIp"], "203.0.113.10");
    assert!(device.get("token").is_none());
    assert!(device.get("userId").is_none());

    let second_page = client
        .get(format!("{path}?page=2&pageSize=1"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    let second_body = second_page.json::<Value>().await?;
    assert_eq!(second_body["totalCount"], 3);
    assert_eq!(second_body["devices"][0]["deviceId"], "device-b");
    assert_eq!(second_body["devices"][0]["hasActiveLogin"], true);

    let third_page = client
        .get(format!("{path}?page=3&pageSize=1"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    let third_body = third_page.json::<Value>().await?;
    assert_eq!(third_body["devices"][0]["deviceId"], "device-c");
    assert_eq!(third_body["devices"][0]["hasActiveLogin"], false);

    let clamped = client
        .get(format!("{path}?page=0&pageSize=101"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(clamped.status(), reqwest::StatusCode::OK);
    let clamped_body = clamped.json::<Value>().await?;
    assert_eq!(clamped_body["page"], 1);
    assert_eq!(clamped_body["pageSize"], 100);

    let invalid_id = client
        .get(format!("{base_url}/api/v1/admin/users/not-a-user/devices"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(invalid_id.status(), reqwest::StatusCode::BAD_REQUEST);
    let missing_id = uuid::Uuid::now_v7().to_string();
    let missing_user = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{missing_id}/devices"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(missing_user.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_manage_scoped_user_access_blacklist_rules()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "admin password").await?;
    let target_a = UserStore::new(database.clone())?
        .create_user("blacklist-a", "Blacklist A", "target password", false)
        .await?;
    let target_b = UserStore::new(database.clone())?
        .create_user("blacklist-b", "Blacklist B", "target password", false)
        .await?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder().no_proxy().build()?;
    let global_path = format!("{base_url}/api/v1/admin/access-blacklist/global");

    let unauthenticated = client.get(&global_path).send().await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "admin password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    let target_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({
            "username": "blacklist-a",
            "password": "target password"
        }))
        .send()
        .await?;
    let target_cookie = cookie_pair(target_login.headers());

    let forbidden_member = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{}/access-blacklist",
            target_a.id
        ))
        .header(COOKIE, &target_cookie)
        .send()
        .await?;
    assert_eq!(forbidden_member.status(), reqwest::StatusCode::FORBIDDEN);

    let missing_csrf = client
        .post(&global_path)
        .header(COOKIE, &admin_cookie)
        .json(&json!({ "ruleType": "CLIENT", "matchValue": "Kodi" }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let invalid_type = client
        .post(&global_path)
        .header(COOKIE, &admin_cookie)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({ "ruleType": "OPERATING_SYSTEM", "matchValue": "iOS" }))
        .send()
        .await?;
    assert_eq!(invalid_type.status(), reqwest::StatusCode::BAD_REQUEST);

    let global_client_rule = client
        .post(&global_path)
        .header(COOKIE, &admin_cookie)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({
            "ruleType": "CLIENT",
            "matchValue": "  Jellyfin   Web  "
        }))
        .send()
        .await?;
    assert_eq!(global_client_rule.status(), reqwest::StatusCode::CREATED);
    let global_client_rule = global_client_rule.json::<Value>().await?;
    let global_client_id = global_client_rule["id"].as_str().unwrap().to_owned();
    assert_eq!(global_client_rule["ruleType"], "CLIENT");
    assert_eq!(global_client_rule["matchValue"], "jellyfin web");
    assert_eq!(global_client_rule["userId"], Value::Null);
    assert_eq!(global_client_rule["createdBy"], admin.id.to_string());
    assert_eq!(global_client_rule["isEnabled"], true);
    assert!(global_client_rule.get("token").is_none());

    let duplicate_global_rule = client
        .post(&global_path)
        .header(COOKIE, &admin_cookie)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({ "ruleType": "CLIENT", "matchValue": "JELLYFIN WEB" }))
        .send()
        .await?;
    assert_eq!(
        duplicate_global_rule.status(),
        reqwest::StatusCode::CONFLICT
    );

    let invalid_cidr = client
        .post(&global_path)
        .header(COOKIE, &admin_cookie)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({ "ruleType": "IP_CIDR", "matchValue": "192.0.2.1/44" }))
        .send()
        .await?;
    assert_eq!(invalid_cidr.status(), reqwest::StatusCode::BAD_REQUEST);

    let global_ip_rule = client
        .post(&global_path)
        .header(COOKIE, &admin_cookie)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({ "ruleType": "IP_CIDR", "matchValue": "192.0.2.99/24" }))
        .send()
        .await?;
    assert_eq!(global_ip_rule.status(), reqwest::StatusCode::CREATED);
    let global_ip_rule = global_ip_rule.json::<Value>().await?;
    let global_ip_id = global_ip_rule["id"].as_str().unwrap().to_owned();
    assert_eq!(global_ip_rule["matchValue"], "192.0.2.0/24");

    let user_path = format!(
        "{base_url}/api/v1/admin/users/{}/access-blacklist",
        target_a.id
    );
    let user_client_rule = client
        .post(&user_path)
        .header(COOKIE, &admin_cookie)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({ "ruleType": "CLIENT", "matchValue": "jellyfin web" }))
        .send()
        .await?;
    assert_eq!(user_client_rule.status(), reqwest::StatusCode::CREATED);
    let user_client_rule = user_client_rule.json::<Value>().await?;
    let user_client_id = user_client_rule["id"].as_str().unwrap().to_owned();
    assert_eq!(user_client_rule["userId"], target_a.id.to_string());

    let user_a_rules = client
        .get(&user_path)
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(user_a_rules.status(), reqwest::StatusCode::OK);
    let user_a_rules = user_a_rules.json::<Value>().await?;
    assert_eq!(user_a_rules["totalCount"], 1);
    assert_eq!(user_a_rules["rules"][0]["id"], user_client_id);

    let user_b_rules = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{}/access-blacklist",
            target_b.id
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(user_b_rules.status(), reqwest::StatusCode::OK);
    assert_eq!(user_b_rules.json::<Value>().await?["totalCount"], 0);

    let global_page = client
        .get(format!("{global_path}?page=0&pageSize=101"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(global_page.status(), reqwest::StatusCode::OK);
    let global_page = global_page.json::<Value>().await?;
    assert_eq!(global_page["page"], 1);
    assert_eq!(global_page["pageSize"], 100);
    assert_eq!(global_page["totalCount"], 2);
    assert_eq!(global_page["rules"].as_array().map(Vec::len), Some(2));

    let wrong_scope_update = client
        .patch(format!("{global_path}/{user_client_id}"))
        .header(COOKIE, &admin_cookie)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({ "isEnabled": false }))
        .send()
        .await?;
    assert_eq!(wrong_scope_update.status(), reqwest::StatusCode::NOT_FOUND);

    let missing_update_csrf = client
        .patch(format!("{global_path}/{global_client_id}"))
        .header(COOKIE, &admin_cookie)
        .json(&json!({ "isEnabled": false }))
        .send()
        .await?;
    assert_eq!(missing_update_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let disable_global_rule = client
        .patch(format!("{global_path}/{global_client_id}"))
        .header(COOKIE, &admin_cookie)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({ "isEnabled": false }))
        .send()
        .await?;
    assert_eq!(disable_global_rule.status(), reqwest::StatusCode::OK);
    assert_eq!(
        disable_global_rule.json::<Value>().await?["isEnabled"],
        false
    );

    let wrong_scope_delete = client
        .delete(format!("{user_path}/{global_ip_id}"))
        .header(COOKIE, &admin_cookie)
        .header("X-CSRF-Token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(wrong_scope_delete.status(), reqwest::StatusCode::NOT_FOUND);

    let delete_user_rule = client
        .delete(format!("{user_path}/{user_client_id}"))
        .header(COOKIE, &admin_cookie)
        .header("X-CSRF-Token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(delete_user_rule.status(), reqwest::StatusCode::NO_CONTENT);

    let invalid_user_id = client
        .get(format!(
            "{base_url}/api/v1/admin/users/not-a-user/access-blacklist"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(invalid_user_id.status(), reqwest::StatusCode::BAD_REQUEST);
    let missing_user_id = uuid::Uuid::now_v7();
    let missing_user = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{missing_user_id}/access-blacklist"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(missing_user.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn web_login_and_sessions_enforce_device_client_blacklists()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "admin password").await?;
    let target = UserStore::new(database.clone())?
        .create_user(
            "web-blacklist-target",
            "Web Blacklist Target",
            "target password",
            false,
        )
        .await?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder().no_proxy().build()?;
    let device_a = uuid::Uuid::new_v4().to_string();
    let device_b = uuid::Uuid::new_v4().to_string();
    let login = |device_id: &str| {
        client
            .post(format!("{base_url}/api/v1/auth/login"))
            .header("X-Lux-Device-Id", device_id.to_owned())
            .json(&json!({
                "username": "web-blacklist-target",
                "password": "target password"
            }))
    };

    let first_login = login(&device_a).send().await?;
    assert_eq!(first_login.status(), reqwest::StatusCode::OK);
    let first_cookie = cookie_pair(first_login.headers());

    sqlx::query(
        "INSERT INTO user_access_blacklist_rules (
             id, user_id, rule_type, match_value, is_enabled, created_by, created_at, updated_at
         ) VALUES (?, ?, 'DEVICE', ?, 1, ?, unixepoch(), unixepoch())",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(target.id.to_string())
    .bind(&device_a)
    .bind(admin.id.to_string())
    .execute(database.pool())
    .await?;

    let denied_existing = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &first_cookie)
        .send()
        .await?;
    assert_eq!(denied_existing.status(), reqwest::StatusCode::FORBIDDEN);
    let revoked_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM web_sessions
         WHERE user_id = ? AND device_id = ? AND revoked_at IS NOT NULL",
    )
    .bind(target.id.to_string())
    .bind(&device_a)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(revoked_count, 1);

    let denied_device_login = login(&device_a).send().await?;
    assert_eq!(denied_device_login.status(), reqwest::StatusCode::FORBIDDEN);
    let active_after_denied_login: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM web_sessions
         WHERE user_id = ? AND device_id = ? AND revoked_at IS NULL",
    )
    .bind(target.id.to_string())
    .bind(&device_a)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(active_after_denied_login, 0);

    let other_device_login = login(&device_b).send().await?;
    assert_eq!(other_device_login.status(), reqwest::StatusCode::OK);
    let other_device_cookie = cookie_pair(other_device_login.headers());
    let other_device_access = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &other_device_cookie)
        .send()
        .await?;
    assert_eq!(other_device_access.status(), reqwest::StatusCode::OK);

    sqlx::query(
        "INSERT INTO user_access_blacklist_rules (
             id, user_id, rule_type, match_value, is_enabled, created_by, created_at, updated_at
         ) VALUES (?, NULL, 'CLIENT', 'lux web', 1, ?, unixepoch(), unixepoch())",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(admin.id.to_string())
    .execute(database.pool())
    .await?;
    let denied_by_client = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &other_device_cookie)
        .send()
        .await?;
    assert_eq!(denied_by_client.status(), reqwest::StatusCode::FORBIDDEN);
    let denied_client_login = login(&device_b).send().await?;
    assert_eq!(denied_client_login.status(), reqwest::StatusCode::FORBIDDEN);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn web_login_and_existing_sessions_enforce_ip_blacklists_through_trusted_proxies()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "admin password").await?;
    let target = UserStore::new(database.clone())?
        .create_user("web-ip-target", "Web IP Target", "target password", false)
        .await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        web_auth.clone(),
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder().no_proxy().build()?;

    for (id, value) in [
        ("web-ip-rule-a", "198.51.100.0/24"),
        ("web-ip-rule-b", "203.0.113.0/24"),
    ] {
        sqlx::query(
            "INSERT INTO user_access_blacklist_rules (
                 id, user_id, rule_type, match_value, is_enabled, created_by,
                 created_at, updated_at
             ) VALUES (?, ?, 'IP_CIDR', ?, 1, ?, unixepoch(), unixepoch())",
        )
        .bind(id)
        .bind(target.id.to_string())
        .bind(value)
        .bind(admin.id.to_string())
        .execute(database.pool())
        .await?;
    }

    let untrusted_header_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .header("X-Forwarded-For", "198.51.100.7")
        .json(&json!({
            "username": "web-ip-target",
            "password": "target password"
        }))
        .send()
        .await?;
    assert_eq!(untrusted_header_login.status(), reqwest::StatusCode::OK);
    let untrusted_session_cookie = cookie_pair(untrusted_header_login.headers());

    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES ('trusted_proxy_cidrs', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind("[\"127.0.0.1/32\"]")
    .execute(database.pool())
    .await?;

    assert!(matches!(
        web_auth
            .login_with_device_and_ip("web-ip-target", "target password", None, None)
            .await,
        Err(luxd::auth::sessions::WebAuthError::AccessDenied)
    ));

    let denied_ip_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .header("X-Forwarded-For", "198.51.100.7")
        .json(&json!({
            "username": "web-ip-target",
            "password": "target password"
        }))
        .send()
        .await?;
    assert_eq!(denied_ip_login.status(), reqwest::StatusCode::FORBIDDEN);

    let allowed_ip_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .header("X-Forwarded-For", "192.0.2.7")
        .json(&json!({
            "username": "web-ip-target",
            "password": "target password"
        }))
        .send()
        .await?;
    assert_eq!(allowed_ip_login.status(), reqwest::StatusCode::OK);
    let allowed_session_cookie = cookie_pair(allowed_ip_login.headers());

    let denied_existing_session = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &allowed_session_cookie)
        .header("X-Forwarded-For", "203.0.113.9")
        .send()
        .await?;
    assert_eq!(
        denied_existing_session.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let revoked_sessions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM web_sessions WHERE user_id = ? AND revoked_at IS NOT NULL",
    )
    .bind(target.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(revoked_sessions, 1);

    let unblocked_existing_session = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &untrusted_session_cookie)
        .header("X-Forwarded-For", "192.0.2.8")
        .send()
        .await?;
    assert_eq!(unblocked_existing_session.status(), reqwest::StatusCode::OK);

    for index in 0..199 {
        let id = format!("web-ip-overflow-{index}");
        let match_value = format!("10.0.0.{index}/32");
        sqlx::query(
            "INSERT INTO user_access_blacklist_rules (
                 id, user_id, rule_type, match_value, is_enabled, created_by,
                 created_at, updated_at
             ) VALUES (?, ?, 'IP_CIDR', ?, 1, ?, unixepoch(), unixepoch())",
        )
        .bind(id)
        .bind(target.id.to_string())
        .bind(match_value)
        .bind(admin.id.to_string())
        .execute(database.pool())
        .await?;
    }
    let overflow_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .header("X-Forwarded-For", "192.0.2.9")
        .json(&json!({
            "username": "web-ip-target",
            "password": "target password"
        }))
        .send()
        .await?;
    assert_eq!(
        overflow_login.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn emby_tokens_enforce_ip_blacklists_in_lux_api_routes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "admin password").await?;
    let target = UserStore::new(database.clone())?
        .create_user(
            "lux-api-emby-ip",
            "Lux API Emby IP",
            "target password",
            false,
        )
        .await?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });
    let client = reqwest::Client::builder().no_proxy().build()?;
    let base_url = format!("http://{address}");
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .json(&json!({
            "Username": "lux-api-emby-ip",
            "Pw": "target password"
        }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing Emby token")?
        .to_owned();

    sqlx::query(
        "INSERT INTO user_access_blacklist_rules (
             id, user_id, rule_type, match_value, is_enabled, created_by, created_at, updated_at
         ) VALUES ('lux-api-emby-ip-rule', ?, 'IP_CIDR', '198.51.100.0/24', 1, ?, unixepoch(), unixepoch())",
    )
    .bind(target.id.to_string())
    .bind(admin.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES ('trusted_proxy_cidrs', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind("[\"127.0.0.1/32\"]")
    .execute(database.pool())
    .await?;

    let allowed_address = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header("X-Emby-Token", &token)
        .header("X-Forwarded-For", "192.0.2.31")
        .send()
        .await?;
    assert_eq!(allowed_address.status(), reqwest::StatusCode::OK);

    let blocked_address = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header("X-Emby-Token", &token)
        .header("X-Forwarded-For", "198.51.100.31")
        .send()
        .await?;
    assert_eq!(blocked_address.status(), reqwest::StatusCode::UNAUTHORIZED);
    let token_hash = sha2::Sha256::digest(token.as_bytes()).to_vec();
    let revoked_at: Option<i64> =
        sqlx::query_scalar("SELECT revoked_at FROM access_tokens WHERE token_hash = ?")
            .bind(token_hash)
            .fetch_one(database.pool())
            .await?;
    assert!(revoked_at.is_some());

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_list_user_web_sessions_without_exposing_tokens()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "admin password").await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config, database, setup, web_auth, emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let unauthenticated = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{}/sessions",
            admin.id
        ))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "admin password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    let created = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "username": "session-target",
            "password": "target password"
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let target_id = created.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing target user id")?
        .to_owned();
    let first_target_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "session-target", "password": "target password" }))
        .send()
        .await?;
    let _second_target_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "session-target", "password": "target password" }))
        .send()
        .await?;
    let target_cookie = cookie_pair(first_target_login.headers());

    let denied = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions"
        ))
        .header(COOKIE, &target_cookie)
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);

    let first_page = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions?page=1&pageSize=1"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(first_page.status(), reqwest::StatusCode::OK);
    let first_body = first_page.json::<Value>().await?;
    assert_eq!(first_body["page"], 1);
    assert_eq!(first_body["pageSize"], 1);
    assert_eq!(first_body["totalCount"], 2);
    assert_eq!(first_body["sessions"].as_array().map(Vec::len), Some(1));
    let first_id = first_body["sessions"][0]["id"]
        .as_str()
        .ok_or("missing first id")?;
    assert!(first_body["sessions"][0]["createdAt"].as_i64().is_some());
    assert!(first_body["sessions"][0].get("sessionToken").is_none());
    assert!(first_body["sessions"][0].get("csrfToken").is_none());

    let second_page = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions?page=2&pageSize=1"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(second_page.status(), reqwest::StatusCode::OK);
    let second_body = second_page.json::<Value>().await?;
    assert_eq!(second_body["totalCount"], 2);
    assert_ne!(
        first_id,
        second_body["sessions"][0]["id"]
            .as_str()
            .unwrap_or_default()
    );

    let capped_page = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions?page=1&pageSize=500"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(capped_page.status(), reqwest::StatusCode::OK);
    let capped_body = capped_page.json::<Value>().await?;
    assert_eq!(capped_body["pageSize"], 100);
    assert_eq!(capped_body["sessions"].as_array().map(Vec::len), Some(2));

    let admin_sessions = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{}/sessions",
            admin.id
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(admin_sessions.status(), reqwest::StatusCode::OK);
    assert_eq!(
        admin_sessions.json::<Value>().await?["sessions"][0]["isCurrent"],
        true
    );

    let invalid_id = client
        .get(format!(
            "{base_url}/api/v1/admin/users/not-a-user-id/sessions"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(invalid_id.status(), reqwest::StatusCode::BAD_REQUEST);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_revoke_user_web_sessions_without_affecting_other_sessions()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "admin password").await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "admin password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    let admin_sessions = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{}/sessions",
            admin.id
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?
        .json::<Value>()
        .await?;
    let admin_session_id = admin_sessions["sessions"]
        .as_array()
        .and_then(|sessions| sessions.iter().find(|session| session["isCurrent"] == true))
        .and_then(|session| session["id"].as_str())
        .ok_or("missing current admin session")?
        .to_owned();

    let created = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "username": "revoke-target",
            "password": "target password"
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let target_id = created.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing target user id")?
        .to_owned();

    let mut target_cookies = Vec::new();
    for _ in 0..3 {
        let login = client
            .post(format!("{base_url}/api/v1/auth/login"))
            .json(&json!({ "username": "revoke-target", "password": "target password" }))
            .send()
            .await?;
        assert_eq!(login.status(), reqwest::StatusCode::OK);
        target_cookies.push(cookie_pair(login.headers()));
    }

    let mut target_session_ids = Vec::new();
    for target_cookie in &target_cookies {
        let sessions = client
            .get(format!("{base_url}/api/v1/auth/sessions"))
            .header(COOKIE, target_cookie)
            .send()
            .await?;
        assert_eq!(sessions.status(), reqwest::StatusCode::OK);
        let sessions_body = sessions.json::<Value>().await?;
        let current_id = sessions_body["sessions"]
            .as_array()
            .and_then(|sessions| sessions.iter().find(|session| session["isCurrent"] == true))
            .and_then(|session| session["id"].as_str())
            .ok_or("missing current target session")?;
        target_session_ids.push(current_id.to_owned());
    }

    let list_url = format!("{base_url}/api/v1/admin/users/{target_id}/sessions?pageSize=100");
    let active = client
        .get(&list_url)
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(active.status(), reqwest::StatusCode::OK);
    let active_sessions = active.json::<Value>().await?["sessions"]
        .as_array()
        .cloned()
        .ok_or("missing active target sessions")?;
    assert_eq!(active_sessions.len(), 3);
    let expired_session_id = &target_session_ids[0];
    let revoke_session_id = &target_session_ids[1];

    let unauthenticated = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions/{revoke_session_id}"
        ))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let missing_csrf = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions/{revoke_session_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let wrong_owner = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions/{admin_session_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(wrong_owner.status(), reqwest::StatusCode::NOT_FOUND);

    sqlx::query("UPDATE web_sessions SET expires_at = unixepoch() - 1 WHERE id = ?")
        .bind(expired_session_id)
        .execute(database.pool())
        .await?;
    let expired = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions/{expired_session_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(expired.status(), reqwest::StatusCode::NOT_FOUND);

    let current_admin = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{}/sessions/{admin_session_id}",
            admin.id
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(current_admin.status(), reqwest::StatusCode::BAD_REQUEST);
    let admin_still_authenticated = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(admin_still_authenticated.status(), reqwest::StatusCode::OK);

    let revoked = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions/{revoke_session_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(revoked.status(), reqwest::StatusCode::NO_CONTENT);
    let revoked_login = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &target_cookies[1])
        .send()
        .await?;
    assert_eq!(revoked_login.status(), reqwest::StatusCode::UNAUTHORIZED);
    let other_login = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &target_cookies[2])
        .send()
        .await?;
    assert_eq!(other_login.status(), reqwest::StatusCode::OK);

    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit?pageSize=100"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let audit_body = audit.json::<Value>().await?;
    let audit_events = audit_body["events"]
        .as_array()
        .ok_or("missing audit events")?;
    assert!(audit_events.iter().any(|event| {
        event["eventType"] == "USER_SESSION_REVOKED"
            && event["targetType"] == "web_session"
            && event["targetId"].as_str() == Some(revoke_session_id.as_str())
            && event["metadata"]["userId"].as_str() == Some(target_id.as_str())
    }));
    let audit_json = audit_body.to_string();
    assert!(!audit_json.contains(&target_cookies[1]));
    assert!(!audit_json.contains(&admin_cookie));

    let repeated = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions/{revoke_session_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(repeated.status(), reqwest::StatusCode::NOT_FOUND);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_manage_emby_login_sessions() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "admin password").await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        web_auth,
        emby_auth.clone(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "admin password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    let create_user = |username: &str| {
        client
            .post(format!("{base_url}/api/v1/admin/users"))
            .header(COOKIE, &admin_cookie)
            .header("x-csrf-token", &admin_csrf)
            .json(&json!({ "username": username, "password": "target password" }))
            .send()
    };
    let target_response = create_user("emby-revoke-target").await?;
    assert_eq!(target_response.status(), reqwest::StatusCode::CREATED);
    let target_id = target_response.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing target user id")?
        .to_owned();
    let other_response = create_user("emby-revoke-other").await?;
    assert_eq!(other_response.status(), reqwest::StatusCode::CREATED);
    let other_id = other_response.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing other user id")?
        .to_owned();

    let target_sessions = [
        emby_auth
            .authenticate_user_id(
                &target_id,
                "target password",
                &EmbyDeviceInfo {
                    client: "Emby Mobile".to_owned(),
                    device: "Phone".to_owned(),
                    device_id: "phone-1".to_owned(),
                    version: "1.0".to_owned(),
                    user_id: None,
                },
            )
            .await?
            .ok_or("target login failed")?,
        emby_auth
            .authenticate_user_id(
                &target_id,
                "target password",
                &EmbyDeviceInfo {
                    client: "Emby TV".to_owned(),
                    device: "Living Room".to_owned(),
                    device_id: "tv-1".to_owned(),
                    version: "2.0".to_owned(),
                    user_id: None,
                },
            )
            .await?
            .ok_or("target login failed")?,
        emby_auth
            .authenticate_user_id(
                &target_id,
                "target password",
                &EmbyDeviceInfo {
                    client: "Emby Web".to_owned(),
                    device: "Browser".to_owned(),
                    device_id: "browser-1".to_owned(),
                    version: "3.0".to_owned(),
                    user_id: None,
                },
            )
            .await?
            .ok_or("target login failed")?,
    ];
    let other_session = emby_auth
        .authenticate_user_id(
            &other_id,
            "target password",
            &EmbyDeviceInfo {
                client: "Emby Other".to_owned(),
                device: "Other Device".to_owned(),
                device_id: "other-device".to_owned(),
                version: "1.0".to_owned(),
                user_id: None,
            },
        )
        .await?
        .ok_or("other login failed")?;

    let unauthenticated = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/emby-sessions"
        ))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let page = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/emby-sessions?page=2&pageSize=1"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(page.status(), reqwest::StatusCode::OK);
    let page_body = page.json::<Value>().await?;
    assert_eq!(page_body["page"], 2);
    assert_eq!(page_body["pageSize"], 1);
    assert_eq!(page_body["totalCount"], 3);
    assert_eq!(page_body["sessions"].as_array().map(Vec::len), Some(1));
    let page_json = page_body.to_string();
    for session in &target_sessions {
        assert!(!page_json.contains(&session.token));
    }
    assert!(!page_json.contains("tokenHash"));
    assert!(!page_json.contains("token_hash"));

    let bounded_page = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/emby-sessions?pageSize=1000"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert_eq!(bounded_page["pageSize"], 100);
    assert_eq!(bounded_page["totalCount"], 3);
    let invalid_user = client
        .get(format!(
            "{base_url}/api/v1/admin/users/not-a-user-id/emby-sessions"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(invalid_user.status(), reqwest::StatusCode::BAD_REQUEST);

    let first_session_id = &target_sessions[0].session_id;
    let unauthenticated_delete = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/emby-sessions/{first_session_id}"
        ))
        .send()
        .await?;
    assert_eq!(
        unauthenticated_delete.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let missing_csrf = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/emby-sessions/{first_session_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);
    let wrong_owner = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/emby-sessions/{}",
            other_session.session_id
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(wrong_owner.status(), reqwest::StatusCode::NOT_FOUND);

    let revoked_one = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/emby-sessions/{first_session_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(revoked_one.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(!emby_auth.verify_token(&target_sessions[0].token).await?);
    assert!(emby_auth.verify_token(&target_sessions[1].token).await?);

    let missing_all_csrf = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/emby-sessions"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(missing_all_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let revoked_all = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/emby-sessions"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(revoked_all.status(), reqwest::StatusCode::OK);
    assert_eq!(revoked_all.json::<Value>().await?["revokedCount"], 2);
    for session in &target_sessions {
        assert!(!emby_auth.verify_token(&session.token).await?);
    }
    assert!(emby_auth.verify_token(&other_session.token).await?);
    let repeated = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/emby-sessions"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(repeated.json::<Value>().await?["revokedCount"], 0);

    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit?pageSize=100"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let audit_body = audit.json::<Value>().await?;
    let audit_events = audit_body["events"]
        .as_array()
        .ok_or("missing audit events")?;
    assert!(
        audit_events
            .iter()
            .any(|event| event["eventType"] == "USER_EMBY_LOGIN_SESSION_REVOKED")
    );
    assert!(
        audit_events
            .iter()
            .any(|event| event["eventType"] == "USER_EMBY_LOGIN_SESSIONS_REVOKED")
    );
    let audit_json = audit_body.to_string();
    for session in &target_sessions {
        assert!(!audit_json.contains(&session.token));
    }
    assert!(!audit_json.contains(&other_session.token));
    server.abort();
    Ok(())
}
