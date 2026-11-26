use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{HeaderMap, Method, Request, StatusCode as AxumStatus},
};
use luxd::network::RemoteAccessPolicy;
use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use std::net::{IpAddr, SocketAddr};
use tokio::net::TcpListener;
use tower::ServiceExt;

/// The policy classifies one already-resolved client address: no forwarded value reaches it.
#[test]
fn remote_policy_classifies_local_and_remote_addresses() {
    let policy = RemoteAccessPolicy;

    for local in [
        // Loopback, both families and the IPv4-mapped form.
        "127.0.0.1",
        "127.9.9.9",
        "::1",
        "::ffff:127.0.0.1",
        // RFC 1918.
        "10.1.2.3",
        "172.16.5.4",
        "172.31.255.254",
        "192.168.1.50",
        // IPv4 link-local.
        "169.254.10.20",
        // IPv6 unique-local and link-local.
        "fc00::1",
        "fd12:3456:789a::1",
        "fe80::1",
        "::ffff:192.168.1.50",
    ] {
        let address: IpAddr = local.parse().expect("valid local address");
        assert!(!policy.is_remote_ip(address), "{local} must stay local");
    }

    for remote in [
        "8.8.8.8",
        "203.0.113.5",
        "192.0.2.10",
        // Tailscale/CGNAT is not a local network.
        "100.64.0.1",
        "100.101.102.103",
        "2001:db8::1",
        "::ffff:8.8.8.8",
    ] {
        let address: IpAddr = remote.parse().expect("valid remote address");
        assert!(policy.is_remote_ip(address), "{remote} must be remote");
    }

    assert!(!policy.is_remote_address(None));
    assert!(!policy.is_remote_address(Some("192.168.1.20".parse().expect("ip"))));
    assert!(policy.is_remote_address(Some("203.0.113.5".parse().expect("ip"))));
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
async fn forwarded_headers_are_only_honoured_for_trusted_proxies()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    // The second half of this test reaches the server through a trusted proxy with a public
    // forwarded address, which is a remote request: the administrator needs the grant.
    sqlx::query("UPDATE users SET can_remote_access = 1 WHERE id = ?")
        .bind(admin.id.to_string())
        .execute(database.pool())
        .await?;
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

    // Without a trusted proxy, forwarded headers must be ignored in favour of the peer.
    //
    // A plain progress tick is coalesced for up to the flush window, so the database row only
    // changes when a callback is written through. Alternating pause and resume makes every
    // callback a state change, which is written before the response, so the assertion below
    // observes exactly the address this request resolved.
    let mut paused = false;
    for (header, value) in [
        ("X-Real-IP", "203.0.113.9"),
        ("Forwarded", "for=198.51.100.17;proto=https"),
        ("X-Forwarded-For", "198.51.100.20"),
    ] {
        paused = !paused;
        let playing = client
            .post(format!("{base_url}/Sessions/Playing/Progress"))
            .header("X-Emby-Token", &token)
            .header(header, value)
            .json(&json!({
                "ItemId": emby_item_id,
                "MediaSourceId": source_id,
                "PlaySessionId": "proxy-session",
                "PositionTicks": 100,
                "RunTimeTicks": 5000,
                "IsPaused": paused,
            }))
            .send()
            .await?;
        assert_eq!(playing.status(), reqwest::StatusCode::NO_CONTENT);
        let stored_ip: Option<String> = sqlx::query_scalar(
            "SELECT remote_ip FROM playback_sessions WHERE play_session_id = 'proxy-session'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(
            stored_ip.as_deref(),
            Some("127.0.0.1"),
            "{header} must not be trusted without a proxy allowlist"
        );
    }

    database
        .set_trusted_proxy_cidrs_json("[\"127.0.0.1/32\"]")
        .await?;

    for prefix in ["", "/emby"] {
        for (header, value, expected) in [
            ("X-Forwarded-For", "203.0.113.9", "203.0.113.9"),
            ("X-Forwarded-For", "2001:db8::9", "2001:db8::9"),
            // X-Real-IP and Forwarded are outside the trusted resolver contract.
            ("X-Real-IP", "203.0.113.9", "127.0.0.1"),
            ("Forwarded", "for=198.51.100.17;proto=https", "127.0.0.1"),
        ] {
            paused = !paused;
            let playing = client
                .post(format!("{base_url}{prefix}/Sessions/Playing/Progress"))
                .header("X-Emby-Token", &token)
                .header(header, value)
                .json(&json!({
                    "ItemId": emby_item_id,
                    "MediaSourceId": source_id,
                    "PlaySessionId": "proxy-session",
                    "PositionTicks": 100,
                    "RunTimeTicks": 5000,
                    "IsPaused": paused,
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

/// What every remote-access scenario needs: one library item, an administrator who may reach the
/// server from anywhere, and a viewer whose `can_remote_access` starts off.
struct Fixture {
    app: Router,
    database: Database,
    admin_id: String,
    viewer_id: String,
    item_id: String,
    source_id: String,
}

async fn fixture(temp_dir: &tempfile::TempDir) -> Result<Fixture, Box<dyn std::error::Error>> {
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    // The administrator drives the policy console in these scenarios, including from remote
    // requests, so the console itself is not the thing under test.
    sqlx::query("UPDATE users SET can_remote_access = 1 WHERE id = ?")
        .bind(admin.id.to_string())
        .execute(database.pool())
        .await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    sqlx::query("UPDATE users SET can_download = 1 WHERE id = ?")
        .bind(viewer.id.to_string())
        .execute(database.pool())
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Remote.Movie.2024.mp4"), b"remote-movie-bytes").await?;
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
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, ?, 1)",
    )
    .bind(viewer.id.to_string())
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE users SET library_access_mode = 'SELECTED' WHERE id = ?")
        .bind(viewer.id.to_string())
        .execute(database.pool())
        .await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let state = AppState::ready(config, database.clone(), setup, web_auth, emby_auth);
    let app = app_with_state(state);
    Ok(Fixture {
        app,
        database,
        admin_id: admin.id.to_string(),
        viewer_id: viewer.id.to_string(),
        item_id,
        source_id,
    })
}

/// A fixture served over a real TCP listener: the peer is loopback, and a trusted-proxy setting
/// decides whether the `X-Forwarded-For` chain is allowed to move the resolved client address.
struct Served {
    fixture: Fixture,
    base_url: String,
    client: reqwest::Client,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn serve(fixture: Fixture) -> Result<Served, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = fixture.app.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    Ok(Served {
        fixture,
        base_url: format!("http://{address}"),
        client: reqwest::Client::builder().no_proxy().build()?,
        server,
    })
}

impl Served {
    async fn login(
        &self,
        username: &str,
        password: &str,
        forwarded_for: Option<&str>,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let mut request = self
            .client
            .post(format!("{}/api/v1/auth/login", self.base_url))
            .json(&json!({ "username": username, "password": password }));
        if let Some(value) = forwarded_for {
            request = request.header("x-forwarded-for", value);
        }
        request.send().await
    }

    async fn emby_login(
        &self,
        username: &str,
        password: &str,
        forwarded_for: Option<&str>,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let mut request = self
            .client
            .post(format!("{}/Users/AuthenticateByName", self.base_url))
            .header(
                "Authorization",
                r#"Emby Client="RemoteTest", Device="Mac", DeviceId="remote-device", Version="1""#,
            )
            .json(&json!({ "Username": username, "Pw": password }));
        if let Some(value) = forwarded_for {
            request = request.header("x-forwarded-for", value);
        }
        request.send().await
    }

    async fn get(
        &self,
        path: &str,
        headers: &[(&str, &str)],
    ) -> Result<reqwest::Response, reqwest::Error> {
        let mut request = self.client.get(format!("{}{path}", self.base_url));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.send().await
    }

    /// Changes one user's remote-access policy through the admin API.
    ///
    /// Policy changes must travel through the API in these tests: the API is what invalidates the
    /// caches the running sessions consult, so a direct SQL update would not model a real
    /// administrator revoking a grant.
    async fn set_remote_access_via_admin(
        &self,
        admin_cookie: &str,
        admin_csrf: &str,
        user_id: &str,
        allowed: bool,
        forwarded_for: Option<&str>,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let mut request = self
            .client
            .patch(format!("{}/api/v1/admin/users/{user_id}", self.base_url))
            .header(COOKIE, admin_cookie)
            .header("x-csrf-token", admin_csrf)
            .json(&json!({ "canRemoteAccess": allowed }));
        if let Some(value) = forwarded_for {
            request = request.header("x-forwarded-for", value);
        }
        request.send().await
    }
}

async fn emby_token(response: reqwest::Response) -> Result<String, Box<dyn std::error::Error>> {
    let body: Value = response.json().await?;
    body["AccessToken"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "missing emby access token".into())
}

async fn create_signed_direct_url(
    served: &Served,
    cookies: &str,
    csrf: &str,
    forwarded_for: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut request = served
        .client
        .post(format!("{}/api/v1/playback/sessions", served.base_url))
        .header(COOKIE, cookies)
        .header("x-csrf-token", csrf)
        .json(&json!({
            "itemId": served.fixture.item_id,
            "sourceId": served.fixture.source_id,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }));
    if let Some(value) = forwarded_for {
        request = request.header("x-forwarded-for", value);
    }
    let response = request.send().await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await?;
    body["plan"]["url"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "missing signed playback url".into())
}

fn websocket_upgrade_headers(origin: &str) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in [
        ("connection", "upgrade"),
        ("upgrade", "websocket"),
        ("sec-websocket-version", "13"),
        ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ("origin", origin),
    ] {
        headers.insert(
            reqwest::header::HeaderName::from_static(name),
            reqwest::header::HeaderValue::from_str(value).expect("header value"),
        );
    }
    headers
}

#[tokio::test]
async fn remote_policy_blocks_login_until_user_is_allowed() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let fixture = fixture(&temp_dir).await?;
    fixture
        .database
        .set_trusted_proxy_cidrs_json("[\"127.0.0.1/32\"]")
        .await?;
    let served = serve(fixture).await?;
    let remote_ip = "203.0.113.40";

    // 1. A local request stays inside the local-network allowance even without the grant.
    let local_login = served.login("viewer", "viewer password", None).await?;
    assert_eq!(local_login.status(), reqwest::StatusCode::OK);
    let local_cookie = cookie_pair(local_login.headers());
    let local_item = served
        .get(
            &format!("/api/v1/items/{}", served.fixture.item_id),
            &[("cookie", &local_cookie)],
        )
        .await?;
    assert_eq!(local_item.status(), reqwest::StatusCode::OK);

    // 2. The same credentials resolved through the trusted proxy to a public address are remote
    //    and are refused for both login surfaces, and no session is handed out.
    let denied_login = served
        .login("viewer", "viewer password", Some(remote_ip))
        .await?;
    assert_eq!(denied_login.status(), reqwest::StatusCode::FORBIDDEN);
    let denied_body: Value = denied_login.json().await?;
    assert_eq!(denied_body["error"]["code"], "PERMISSION_DENIED");
    let denied_emby = served
        .emby_login("viewer", "viewer password", Some(remote_ip))
        .await?;
    assert_eq!(denied_emby.status(), reqwest::StatusCode::FORBIDDEN);

    // The locally issued cookie is refused on remote requests and still accepted locally.
    let remote_item = served
        .get(
            &format!("/api/v1/items/{}", served.fixture.item_id),
            &[("cookie", &local_cookie), ("x-forwarded-for", remote_ip)],
        )
        .await?;
    assert_eq!(remote_item.status(), reqwest::StatusCode::FORBIDDEN);
    let remote_me = served
        .get(
            "/api/v1/auth/me",
            &[("cookie", &local_cookie), ("x-forwarded-for", remote_ip)],
        )
        .await?;
    assert_eq!(remote_me.status(), reqwest::StatusCode::FORBIDDEN);
    let local_me = served
        .get("/api/v1/auth/me", &[("cookie", &local_cookie)])
        .await?;
    assert_eq!(local_me.status(), reqwest::StatusCode::OK);

    // 3. The administrator grants remote access through the admin API.
    let admin_login = served
        .login("Admin", "correct password", Some(remote_ip))
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    let grant = served
        .client
        .patch(format!(
            "{}/api/v1/admin/users/{}",
            served.base_url, served.fixture.viewer_id
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .header("x-forwarded-for", remote_ip)
        .json(&json!({ "canRemoteAccess": true }))
        .send()
        .await?;
    assert_eq!(grant.status(), reqwest::StatusCode::OK);

    // Remote login, media detail, download and the Emby surface all work again.
    let allowed_login = served
        .login("viewer", "viewer password", Some(remote_ip))
        .await?;
    assert_eq!(allowed_login.status(), reqwest::StatusCode::OK);
    let allowed_cookie = cookie_pair(allowed_login.headers());
    let allowed_item = served
        .get(
            &format!("/api/v1/items/{}", served.fixture.item_id),
            &[("cookie", &allowed_cookie), ("x-forwarded-for", remote_ip)],
        )
        .await?;
    assert_eq!(allowed_item.status(), reqwest::StatusCode::OK);
    let allowed_download = served
        .get(
            &format!("/api/v1/items/{}/download", served.fixture.item_id),
            &[("cookie", &allowed_cookie), ("x-forwarded-for", remote_ip)],
        )
        .await?;
    assert_eq!(allowed_download.status(), reqwest::StatusCode::OK);
    let allowed_emby = served
        .emby_login("viewer", "viewer password", Some(remote_ip))
        .await?;
    assert_eq!(allowed_emby.status(), reqwest::StatusCode::OK);
    let viewer_token = emby_token(allowed_emby).await?;
    let allowed_sessions = served
        .get(
            "/emby/Sessions",
            &[
                ("x-emby-token", &viewer_token),
                ("x-forwarded-for", remote_ip),
            ],
        )
        .await?;
    assert_eq!(allowed_sessions.status(), reqwest::StatusCode::OK);

    // 4. Withdrawing the grant through the admin API stops the already issued cookie and token on
    //    remote requests immediately; no re-login is involved and local access is unaffected.
    let revoke = served
        .client
        .patch(format!(
            "{}/api/v1/admin/users/{}",
            served.base_url, served.fixture.viewer_id
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .header("x-forwarded-for", remote_ip)
        .json(&json!({ "canRemoteAccess": false }))
        .send()
        .await?;
    assert_eq!(revoke.status(), reqwest::StatusCode::OK);

    let revoked_item = served
        .get(
            &format!("/api/v1/items/{}", served.fixture.item_id),
            &[("cookie", &allowed_cookie), ("x-forwarded-for", remote_ip)],
        )
        .await?;
    assert_eq!(revoked_item.status(), reqwest::StatusCode::FORBIDDEN);
    let revoked_download = served
        .get(
            &format!("/api/v1/items/{}/download", served.fixture.item_id),
            &[("cookie", &allowed_cookie), ("x-forwarded-for", remote_ip)],
        )
        .await?;
    assert_eq!(revoked_download.status(), reqwest::StatusCode::FORBIDDEN);
    let revoked_emby = served
        .get(
            "/emby/Sessions",
            &[
                ("x-emby-token", &viewer_token),
                ("x-forwarded-for", remote_ip),
            ],
        )
        .await?;
    assert_eq!(revoked_emby.status(), reqwest::StatusCode::FORBIDDEN);
    let still_local = served
        .get(
            &format!("/api/v1/items/{}", served.fixture.item_id),
            &[("cookie", &allowed_cookie)],
        )
        .await?;
    assert_eq!(still_local.status(), reqwest::StatusCode::OK);

    Ok(())
}

#[tokio::test]
async fn remote_policy_blocks_playback_admin_and_websockets_after_revocation()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let fixture = fixture(&temp_dir).await?;
    fixture
        .database
        .set_trusted_proxy_cidrs_json("[\"127.0.0.1/32\"]")
        .await?;
    let viewer_id = fixture.viewer_id.clone();
    let admin_id = fixture.admin_id.clone();
    let served = serve(fixture).await?;
    let remote_ip = "203.0.113.41";

    // The administrator works remotely (their own grant is on) and enables the viewer's grant.
    let admin_login = served
        .login("Admin", "correct password", Some(remote_ip))
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    let grant = served
        .set_remote_access_via_admin(
            &admin_cookie,
            &admin_csrf,
            &viewer_id,
            true,
            Some(remote_ip),
        )
        .await?;
    assert_eq!(grant.status(), reqwest::StatusCode::OK);

    // Remote playback is issued while the grant is on, so the signed URL exists before revocation.
    let login = served
        .login("viewer", "viewer password", Some(remote_ip))
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookies = cookie_pair(login.headers());
    let csrf = cookie_value(login.headers(), "lux_csrf");
    let emby_login = served
        .emby_login("viewer", "viewer password", Some(remote_ip))
        .await?;
    assert_eq!(emby_login.status(), reqwest::StatusCode::OK);
    let viewer_token = emby_token(emby_login).await?;
    let signed_url = create_signed_direct_url(&served, &cookies, &csrf, Some(remote_ip)).await?;
    let signed_remote = served
        .get(&signed_url, &[("x-forwarded-for", remote_ip)])
        .await?;
    assert_eq!(signed_remote.status(), reqwest::StatusCode::OK);

    let revoke = served
        .set_remote_access_via_admin(
            &admin_cookie,
            &admin_csrf,
            &viewer_id,
            false,
            Some(remote_ip),
        )
        .await?;
    assert_eq!(revoke.status(), reqwest::StatusCode::OK);

    // The signed URL carries no credentials of its own: the live user policy stops it remotely.
    let signed_revoked = served
        .get(&signed_url, &[("x-forwarded-for", remote_ip)])
        .await?;
    assert_eq!(signed_revoked.status(), reqwest::StatusCode::FORBIDDEN);
    let signed_local = served.get(&signed_url, &[]).await?;
    assert_eq!(
        signed_local.status(),
        reqwest::StatusCode::OK,
        "a local request keeps the signed resource"
    );

    // Every remaining entry point of the same session is refused remotely.
    let item = served
        .get(
            &format!("/api/v1/items/{}", served.fixture.item_id),
            &[("cookie", &cookies), ("x-forwarded-for", remote_ip)],
        )
        .await?;
    assert_eq!(item.status(), reqwest::StatusCode::FORBIDDEN);
    let playback_create = served
        .client
        .post(format!("{}/api/v1/playback/sessions", served.base_url))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .header("x-forwarded-for", remote_ip)
        .json(&json!({
            "itemId": served.fixture.item_id,
            "sourceId": served.fixture.source_id,
        }))
        .send()
        .await?;
    assert_eq!(
        playback_create.status(),
        reqwest::StatusCode::FORBIDDEN,
        "creating a playback session is a web session entry point"
    );
    let lux_ws = served
        .client
        .get(format!("{}/api/v1/ws", served.base_url))
        .header(COOKIE, &cookies)
        .header("x-forwarded-for", remote_ip)
        .headers(websocket_upgrade_headers(&served.base_url))
        .send()
        .await?;
    assert_eq!(
        lux_ws.status(),
        reqwest::StatusCode::FORBIDDEN,
        "the Lux websocket handshake authenticates remotely"
    );
    let emby_ws = served
        .client
        .get(format!(
            "{}/embywebsocket?api_key={viewer_token}&deviceId=remote-ws",
            served.base_url
        ))
        .header("x-forwarded-for", remote_ip)
        .headers(websocket_upgrade_headers(&served.base_url))
        .send()
        .await?;
    assert_eq!(
        emby_ws.status(),
        reqwest::StatusCode::FORBIDDEN,
        "the Emby websocket handshake authenticates remotely"
    );

    // The admin console follows the same policy: revoking the administrator's grant stops remote
    // administration while the same session keeps working locally.
    let admin_users_remote = served
        .get(
            "/api/v1/admin/users",
            &[("cookie", &admin_cookie), ("x-forwarded-for", remote_ip)],
        )
        .await?;
    assert_eq!(admin_users_remote.status(), reqwest::StatusCode::OK);
    let self_revoke = served
        .set_remote_access_via_admin(
            &admin_cookie,
            &admin_csrf,
            &admin_id,
            false,
            Some(remote_ip),
        )
        .await?;
    assert_eq!(self_revoke.status(), reqwest::StatusCode::OK);
    let admin_users_revoked = served
        .get(
            "/api/v1/admin/users",
            &[("cookie", &admin_cookie), ("x-forwarded-for", remote_ip)],
        )
        .await?;
    assert_eq!(
        admin_users_revoked.status(),
        reqwest::StatusCode::FORBIDDEN,
        "admin APIs honour can_remote_access"
    );
    let admin_users_local = served
        .get("/api/v1/admin/users", &[("cookie", &admin_cookie)])
        .await?;
    assert_eq!(admin_users_local.status(), reqwest::StatusCode::OK);

    Ok(())
}

/// One in-process request with an explicit connection peer.
struct RawResponse {
    status: AxumStatus,
    headers: HeaderMap,
    body: Vec<u8>,
}

fn json_request(
    method: Method,
    uri: &str,
    peer: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "lux.test");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&value).expect("json body"))
        }
        None => Body::empty(),
    };
    let mut request = builder.body(body).expect("test request");
    request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
        peer.parse::<IpAddr>().expect("peer address"),
        40_000,
    )));
    request
}

async fn send(app: &Router, request: Request<Body>) -> RawResponse {
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 128 * 1024)
        .await
        .expect("response body")
        .to_vec();
    RawResponse {
        status,
        headers,
        body,
    }
}

fn raw_cookie_pair(headers: &HeaderMap) -> String {
    let value = |name: &str| {
        headers
            .get_all(SET_COOKIE.as_str())
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find_map(|value| {
                value
                    .strip_prefix(&format!("{name}="))
                    .and_then(|value| value.split(';').next())
                    .map(str::to_owned)
            })
            .unwrap_or_default()
    };
    format!(
        "lux_session={}; lux_csrf={}",
        value("lux_session"),
        value("lux_csrf")
    )
}

#[tokio::test]
async fn forwarded_headers_cannot_bypass_remote_access_policy()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let fixture = fixture(&temp_dir).await?;
    let login_body = json!({ "username": "viewer", "password": "viewer password" });

    // A loopback login keeps a session cookie that this test then replays from other peers.
    let local_login = send(
        &fixture.app,
        json_request(
            Method::POST,
            "/api/v1/auth/login",
            "127.0.0.1",
            &[],
            Some(login_body.clone()),
        ),
    )
    .await;
    assert_eq!(local_login.status, AxumStatus::OK);
    let cookies = raw_cookie_pair(&local_login.headers);

    // An untrusted public peer cannot smuggle itself into the local network with a header: the
    // forwarded chain is ignored unless the connection peer is a configured trusted proxy.
    for forged in ["127.0.0.1", "192.168.1.10"] {
        let login = send(
            &fixture.app,
            json_request(
                Method::POST,
                "/api/v1/auth/login",
                "203.0.113.77",
                &[("x-forwarded-for", forged)],
                Some(login_body.clone()),
            ),
        )
        .await;
        assert_eq!(
            login.status,
            AxumStatus::FORBIDDEN,
            "a public peer forging {forged} must stay remote"
        );
        let item = send(
            &fixture.app,
            json_request(
                Method::GET,
                &format!("/api/v1/items/{}", fixture.item_id),
                "203.0.113.77",
                &[("cookie", &cookies), ("x-forwarded-for", forged)],
                None,
            ),
        )
        .await;
        assert_eq!(
            item.status,
            AxumStatus::FORBIDDEN,
            "a public peer forging {forged} must not read media"
        );
    }

    // A local peer cannot turn itself remote either: without a trusted-proxy configuration its
    // forwarded header is noise, so the request stays local and is allowed.
    let forged_public = send(
        &fixture.app,
        json_request(
            Method::POST,
            "/api/v1/auth/login",
            "127.0.0.1",
            &[("x-forwarded-for", "8.8.8.8")],
            Some(login_body.clone()),
        ),
    )
    .await;
    assert_eq!(
        forged_public.status,
        AxumStatus::OK,
        "an untrusted forwarded header must not lock a local user out"
    );

    // A trusted proxy whose chain is malformed fails closed instead of falling back to allow.
    fixture
        .database
        .set_trusted_proxy_cidrs_json("[\"127.0.0.1/32\"]")
        .await?;
    let malformed = send(
        &fixture.app,
        json_request(
            Method::POST,
            "/api/v1/auth/login",
            "127.0.0.1",
            &[("x-forwarded-for", "not-an-ip")],
            Some(login_body.clone()),
        ),
    )
    .await;
    assert_eq!(malformed.status, AxumStatus::SERVICE_UNAVAILABLE);
    let empty_hop = send(
        &fixture.app,
        json_request(
            Method::POST,
            "/api/v1/auth/login",
            "127.0.0.1",
            &[("x-forwarded-for", "203.0.113.9, ")],
            Some(login_body.clone()),
        ),
    )
    .await;
    assert_eq!(empty_hop.status, AxumStatus::SERVICE_UNAVAILABLE);

    // An unreadable trusted-proxy setting also fails closed.
    fixture
        .database
        .set_trusted_proxy_cidrs_json("invalid-json")
        .await?;
    let broken_settings = send(
        &fixture.app,
        json_request(
            Method::POST,
            "/api/v1/auth/login",
            "127.0.0.1",
            &[("x-forwarded-for", "203.0.113.9")],
            Some(login_body.clone()),
        ),
    )
    .await;
    assert_eq!(broken_settings.status, AxumStatus::SERVICE_UNAVAILABLE);

    // With a working trusted proxy the chain decides: a private hop stays local and is allowed,
    // while a public hop is remote and is refused.
    fixture
        .database
        .set_trusted_proxy_cidrs_json("[\"127.0.0.1/32\"]")
        .await?;
    let trusted_lan = send(
        &fixture.app,
        json_request(
            Method::POST,
            "/api/v1/auth/login",
            "127.0.0.1",
            &[("x-forwarded-for", "192.168.1.50")],
            Some(login_body.clone()),
        ),
    )
    .await;
    assert_eq!(
        trusted_lan.status,
        AxumStatus::OK,
        "a trusted proxy may report a LAN client"
    );
    let trusted_lan_item = send(
        &fixture.app,
        json_request(
            Method::GET,
            &format!("/api/v1/items/{}", fixture.item_id),
            "127.0.0.1",
            &[("cookie", &cookies), ("x-forwarded-for", "192.168.1.50")],
            None,
        ),
    )
    .await;
    assert_eq!(trusted_lan_item.status, AxumStatus::OK);

    let trusted_remote = send(
        &fixture.app,
        json_request(
            Method::POST,
            "/api/v1/auth/login",
            "127.0.0.1",
            &[("x-forwarded-for", "203.0.113.9")],
            Some(login_body),
        ),
    )
    .await;
    assert_eq!(trusted_remote.status, AxumStatus::FORBIDDEN);

    let body: Value = serde_json::from_slice(&trusted_remote.body)?;
    assert_eq!(body["error"]["code"], "PERMISSION_DENIED");

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
