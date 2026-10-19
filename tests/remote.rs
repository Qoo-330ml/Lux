use luxd::network::RemoteAccessPolicy;
use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::COOKIE;
use serde_json::{Value, json};
use std::net::SocketAddr;
use tokio::net::TcpListener;

#[test]
fn remote_policy_uses_forwarded_client_without_proxy_allowlist() {
    let policy = RemoteAccessPolicy;

    assert_eq!(
        policy.client_ip(Some("192.168.1.2"), Some("8.8.8.8")),
        Some("8.8.8.8".parse().expect("valid client IP"))
    );
    assert!(!policy.is_remote(Some("192.168.1.2"), Some("8.8.8.8")));
}

#[test]
fn remote_policy_reports_forwarded_client_without_proxy_allowlist() {
    let policy = RemoteAccessPolicy;

    assert_eq!(
        policy.reported_client_ip(Some("192.168.1.2"), Some("8.8.8.8"), None, None),
        Some("8.8.8.8".parse().expect("valid client IP"))
    );
    assert_eq!(
        policy.reported_client_ip(Some("192.168.1.20"), None, None, None),
        None
    );
}

#[test]
fn reported_client_ip_uses_valid_headers_in_priority_order() {
    let policy = RemoteAccessPolicy;
    for (forwarded_for, real_ip, forwarded, expected) in [
        (
            Some("203.0.113.1, 192.0.2.1"),
            Some("203.0.113.2"),
            Some("for=203.0.113.3"),
            "203.0.113.1",
        ),
        (
            None,
            Some(" 203.0.113.2 "),
            Some("for=203.0.113.3"),
            "203.0.113.2",
        ),
        (
            Some("invalid"),
            Some("203.0.113.2"),
            Some("for=203.0.113.3"),
            "203.0.113.2",
        ),
        (
            Some("unknown, 192.0.2.1"),
            Some("invalid"),
            Some("for=203.0.113.3"),
            "203.0.113.3",
        ),
        (Some(""), Some(""), Some("for=203.0.113.3"), "203.0.113.3"),
    ] {
        assert_eq!(
            policy.reported_client_ip(Some("127.0.0.1"), forwarded_for, real_ip, forwarded),
            Some(expected.parse().expect("valid expected IP"))
        );
    }
    assert_eq!(
        policy.reported_client_ip(None, None, Some("203.0.113.2"), None),
        None
    );
}

#[test]
fn reported_real_ip_requires_a_single_ip_address() {
    let policy = RemoteAccessPolicy;
    assert_eq!(
        policy.reported_client_ip(Some("127.0.0.1"), None, Some("2001:db8::9"), None),
        Some("2001:db8::9".parse().expect("valid IPv6"))
    );
    for value in [
        "",
        "unknown",
        "203.0.113.9:80",
        "[2001:db8::9]",
        "203.0.113.9, 192.0.2.1",
    ] {
        assert_eq!(
            policy.reported_client_ip(Some("127.0.0.1"), None, Some(value), None),
            None,
            "{value}"
        );
    }
}

#[test]
fn reported_forwarded_ip_parses_standard_node_formats_and_quoted_delimiters() {
    let policy = RemoteAccessPolicy;
    for (value, expected) in [
        ("for=192.0.2.43;proto=https;by=198.51.100.17", "192.0.2.43"),
        (
            "proto=https; For = 192.0.2.43 , for=198.51.100.17",
            "192.0.2.43",
        ),
        (r#"for="192.0.2.43""#, "192.0.2.43"),
        (r#"for="192.0.2.43:47011""#, "192.0.2.43"),
        (r#"for="[2001:db8:cafe::17]""#, "2001:db8:cafe::17"),
        (r#"For="[2001:db8:cafe::17]:4711""#, "2001:db8:cafe::17"),
        (r#"for="[2001:db8::17]:_hidden""#, "2001:db8::17"),
        (
            r#"ext="comma,semi;quote\"value"; for=192.0.2.43"#,
            "192.0.2.43",
        ),
        (r#"for="192\.0.2.43""#, "192.0.2.43"),
    ] {
        assert_eq!(
            policy.reported_client_ip(Some("127.0.0.1"), None, None, Some(value)),
            Some(expected.parse().expect("valid expected IP")),
            "{value}"
        );
    }
}

#[test]
fn reported_forwarded_ip_rejects_unknown_malformed_and_later_proxy_nodes() {
    let policy = RemoteAccessPolicy;
    for value in [
        "",
        "for=unknown",
        "for=_hidden",
        "for=unknown, for=192.0.2.43",
        "proto=https, for=192.0.2.43",
        r#"ext="fake;for=192.0.2.43""#,
        "for=192.0.2.43;for=198.51.100.17",
        "for=2001:db8::17",
        "for=192.0.2.43:47011",
        r#"for="[2001:db8::17]"garbage"#,
        r#"for="[2001:db8::17]:bad""#,
        r#"for="[2001:db8::17]:65536""#,
        r#"for="192.0.2.43:""#,
        r#"for="192.0.2.43:_""#,
        r#"for="[2001:db8::17]";ext="unterminated"#,
    ] {
        assert_eq!(
            policy.reported_client_ip(Some("127.0.0.1"), None, None, Some(value)),
            None,
            "{value}"
        );
    }
}

#[tokio::test]
async fn alternate_forwarded_headers_are_reported_in_playback_sessions()
-> Result<(), Box<dyn std::error::Error>> {
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
    tokio::fs::write(root.join("Proxy.Movie.2024.mkv"), b"video").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
        .fetch_one(database.pool())
        .await?;
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    let emby_item_id = uuid::Uuid::parse_str(&item_id)?.as_u128().to_string();
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
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder().no_proxy().build()?;
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            "Authorization",
            r#"Emby Client="ProxyTest", Device="Mac", DeviceId="proxy-device", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();

    for prefix in ["", "/emby"] {
        for (header, value, expected) in [
            ("X-Real-IP", "203.0.113.9", "203.0.113.9"),
            ("X-Real-IP", "2001:db8::9", "2001:db8::9"),
            (
                "Forwarded",
                "for=198.51.100.17;proto=https",
                "198.51.100.17",
            ),
            ("Forwarded", r#"For="[2001:db8::17]:4711""#, "2001:db8::17"),
        ] {
            let playing = client
                .post(format!("{base_url}{prefix}/Sessions/Playing"))
                .header("X-Emby-Token", &token)
                .header(header, value)
                .json(&json!({
                    "ItemId": emby_item_id,
                    "MediaSourceId": source_id,
                    "PlaySessionId": "proxy-session",
                    "PositionTicks": 100,
                    "RunTimeTicks": 5000,
                }))
                .send()
                .await?;
            assert_eq!(playing.status(), reqwest::StatusCode::NO_CONTENT);
            let stored_ip: Option<String> = sqlx::query_scalar(
                "SELECT remote_ip FROM playback_sessions WHERE play_session_id = 'proxy-session'",
            )
            .fetch_one(database.pool())
            .await?;
            assert_eq!(stored_ip.as_deref(), Some(expected), "{prefix}: {header}");
            let sessions = client
                .get(format!("{base_url}{prefix}/Sessions"))
                .header("X-Emby-Token", &token)
                .send()
                .await?;
            assert_eq!(sessions.status(), reqwest::StatusCode::OK);
            let sessions = sessions.json::<Value>().await?;
            assert_eq!(
                sessions[0]["RemoteEndPoint"], expected,
                "{prefix}: {header}"
            );
        }
    }
    server.abort();
    Ok(())
}

#[tokio::test]
async fn remote_policy_blocks_auth_and_media_until_user_is_allowed()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Remote.Movie.2024.mkv"), b"movie").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
        .fetch_one(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, ?, 1)",
    )
    .bind(viewer.id.to_string())
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let state = AppState::ready(config, database.clone(), setup, web_auth, emby_auth);
    let app = app_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let denied_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .header("x-forwarded-for", "8.8.8.8")
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    assert_eq!(denied_login.status(), reqwest::StatusCode::OK);

    sqlx::query("UPDATE users SET can_remote_access = 1 WHERE id = ?")
        .bind(viewer.id.to_string())
        .execute(database.pool())
        .await?;
    let allowed_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .header("x-forwarded-for", "8.8.8.8")
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    assert_eq!(allowed_login.status(), reqwest::StatusCode::OK);
    let cookie = cookie_pair(allowed_login.headers());
    let visible_item = client
        .get(format!("{base_url}/api/v1/items/{item_id}"))
        .header(COOKIE, &cookie)
        .header("x-forwarded-for", "8.8.8.8")
        .send()
        .await?;
    assert_eq!(visible_item.status(), reqwest::StatusCode::OK);

    sqlx::query("UPDATE users SET can_remote_access = 0 WHERE id = ?")
        .bind(viewer.id.to_string())
        .execute(database.pool())
        .await?;
    let blocked_item = client
        .get(format!("{base_url}/api/v1/items/{item_id}"))
        .header(COOKIE, &cookie)
        .header("x-forwarded-for", "8.8.8.8")
        .send()
        .await?;
    assert_eq!(blocked_item.status(), reqwest::StatusCode::OK);

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
