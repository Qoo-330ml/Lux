//! `GET /api/v1/ws` (LUX-894).
//!
//! The Lux realtime channel is pinned over a real socket, the way a browser drives it: the
//! handshake decides 426 / 400 / 401 / 403 / 429 before any upgrade, the two channels follow the
//! handshake principal, the envelope is frozen (`ready`, then `invalidate` with an optional
//! `itemId`), the inbound surface only takes `{"type":"ping"}`, and every budget, timer, demotion
//! and shutdown rule ends the connection with the documented close code.
//!
//! These tests talk to the real router (`app_with_state`) and the production services; only the
//! limits are injected so a budget or a timer is observable without waiting for 45 s or filling
//! 128 slots. The `admin` group owns this module (`tests/admin/main.rs`).

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use luxd::{
    api::{AppState, LuxWebsocketLimits, WebsocketLimits, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{
        Error as WsError, Message,
        client::IntoClientRequest,
        http::{HeaderValue, Request},
    },
};

type ClientSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// One logged-in account: the Web session cookies the browser sends and the Emby client token a
/// non-browser client sends.
#[derive(Clone)]
struct Account {
    id: String,
    cookies: String,
    csrf: String,
    token: String,
}

impl Account {
    fn session_cookie(&self) -> String {
        self.cookies
            .split(';')
            .map(str::trim)
            .find(|pair| pair.starts_with("lux_session="))
            .unwrap_or_default()
            .to_owned()
    }
}

struct TestServer {
    origin: String,
    ws_base: String,
    address: std::net::SocketAddr,
    client: reqwest::Client,
    state: AppState,
    database: Database,
    admin: Account,
    operator: Account,
    viewer: Account,
    viewer_two: Account,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    server: tokio::task::JoinHandle<Result<(), std::io::Error>>,
    _temp: tempfile::TempDir,
}

/// What one handshake attempt produced.
struct Attempt {
    socket: Option<ClientSocket>,
    status: u16,
    retry_after: Option<String>,
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
        .unwrap_or_default()
}

async fn login(
    client: &reqwest::Client,
    base: &str,
    username: &str,
    password: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({ "username": username, "password": password }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(response.headers(), "lux_session"),
        cookie_value(response.headers(), "lux_csrf")
    );
    let csrf = cookie_value(response.headers(), "lux_csrf");
    Ok((cookies, csrf))
}

/// An Emby access token, which `require_web_user` accepts through `X-Emby-Token` (ADR-054 D6).
async fn emby_token(
    client: &reqwest::Client,
    base: &str,
    username: &str,
    password: &str,
    device_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            format!(
                r#"Emby Client="LuxWsTest", Device="Mac", DeviceId="{device_id}", Version="1""#
            ),
        )
        .json(&json!({ "Username": username, "Pw": password }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await?;
    Ok(body["AccessToken"]
        .as_str()
        .ok_or("missing Emby access token")?
        .to_owned())
}

/// Starts one isolated Lux instance with an administrator, a second administrator (`operator`),
/// two regular accounts and the production route table.
async fn spawn_server(
    limits: LuxWebsocketLimits,
) -> Result<TestServer, Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:0".parse()?,
        config_dir: temp.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let users = UserStore::new(database.clone())?;
    let operator = users
        .create_user("operator", "Operator", "operator password", true)
        .await?;
    let viewer = users
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let viewer_two = users
        .create_user("viewer2", "Viewer Two", "viewer two password", false)
        .await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let state = AppState::ready(config, database.clone(), setup, web_auth, emby_auth)
        .with_lux_websocket_limits(limits);
    let app = app_with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_state = state.clone();
    let server = tokio::spawn(async move {
        // Production serves with the connection peer attached, which the access-IP resolver
        // turns into `x-lux-peer-ip`; a session with IP rules cannot be evaluated without it.
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
            // Mirrors `src/main.rs`: both realtime endpoints are told to close first.
            shutdown_state.signal_websocket_shutdown();
        })
        .await
    });
    let base = format!("http://{address}");
    let client = reqwest::Client::builder().no_proxy().build()?;
    let (admin_cookies, admin_csrf) = login(&client, &base, "admin", "correct password").await?;
    let (operator_cookies, operator_csrf) =
        login(&client, &base, "operator", "operator password").await?;
    let (viewer_cookies, viewer_csrf) = login(&client, &base, "viewer", "viewer password").await?;
    let (viewer_two_cookies, viewer_two_csrf) =
        login(&client, &base, "viewer2", "viewer two password").await?;
    let admin_token =
        emby_token(&client, &base, "Admin", "correct password", "lux-ws-admin").await?;
    let operator_token = emby_token(
        &client,
        &base,
        "Operator",
        "operator password",
        "lux-ws-operator",
    )
    .await?;
    let viewer_token =
        emby_token(&client, &base, "Viewer", "viewer password", "lux-ws-viewer").await?;
    let viewer_two_token = emby_token(
        &client,
        &base,
        "Viewer Two",
        "viewer two password",
        "lux-ws-viewer-2",
    )
    .await?;
    Ok(TestServer {
        origin: format!("http://{address}"),
        ws_base: format!("ws://{address}"),
        address,
        client,
        state,
        database,
        admin: Account {
            id: admin.id.to_string(),
            cookies: admin_cookies,
            csrf: admin_csrf,
            token: admin_token,
        },
        operator: Account {
            id: operator.id.to_string(),
            cookies: operator_cookies,
            csrf: operator_csrf,
            token: operator_token,
        },
        viewer: Account {
            id: viewer.id.to_string(),
            cookies: viewer_cookies,
            csrf: viewer_csrf,
            token: viewer_token,
        },
        viewer_two: Account {
            id: viewer_two.id.to_string(),
            cookies: viewer_two_cookies,
            csrf: viewer_two_csrf,
            token: viewer_two_token,
        },
        shutdown: Some(shutdown),
        server,
        _temp: temp,
    })
}

fn limits() -> LuxWebsocketLimits {
    LuxWebsocketLimits::default()
}

/// Timers a test would otherwise have to wait 45 s for.
fn fast_timers() -> LuxWebsocketLimits {
    LuxWebsocketLimits {
        protocol: WebsocketLimits {
            revalidate_interval: Duration::from_millis(150),
            auth_warn_interval: Duration::from_millis(100),
            ..WebsocketLimits::default()
        },
        ..LuxWebsocketLimits::default()
    }
}

impl TestServer {
    fn request(&self) -> Request<()> {
        self.request_with_path("/api/v1/ws")
    }

    fn request_with_path(&self, path: &str) -> Request<()> {
        let mut request = format!("{}{path}", self.ws_base)
            .as_str()
            .into_client_request()
            .expect("valid ws request");
        request.headers_mut().insert(
            "origin",
            HeaderValue::from_str(&self.origin).expect("valid origin"),
        );
        request
    }

    async fn handshake(&self, request: Request<()>) -> Result<Attempt, Box<dyn std::error::Error>> {
        match tokio_tungstenite::connect_async(request).await {
            Ok((socket, response)) => Ok(Attempt {
                socket: Some(socket),
                status: response.status().as_u16(),
                retry_after: None,
            }),
            Err(WsError::Http(response)) => Ok(Attempt {
                socket: None,
                status: response.status().as_u16(),
                retry_after: response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
            }),
            Err(error) => Err(format!("unexpected handshake error: {error}").into()),
        }
    }

    /// Completes the handshake and consumes the `ready` frame.
    async fn connect(
        &self,
        request: Request<()>,
    ) -> Result<ClientSocket, Box<dyn std::error::Error>> {
        let attempt = self.handshake(request).await?;
        assert_eq!(
            attempt.status, 101,
            "the handshake must upgrade, not answer {}",
            attempt.status
        );
        let mut socket = attempt.socket.ok_or("a 101 response without a socket")?;
        let ready = wait_for_frame(&mut socket, CONDITION_DEADLINE, |_| true)
            .await?
            .ok_or("the server sent no ready frame")?;
        assert_eq!(
            ready,
            json!({"type": "ready", "version": 1}),
            "the first frame is the frozen ready frame"
        );
        Ok(socket)
    }

    async fn connect_account(
        &self,
        account: &Account,
    ) -> Result<ClientSocket, Box<dyn std::error::Error>> {
        let request = with_cookie(self.request(), &account.session_cookie());
        self.connect(request).await
    }

    /// The status of a handshake that is expected to be refused.
    async fn refusal(&self, request: Request<()>) -> Result<u16, Box<dyn std::error::Error>> {
        Ok(self.handshake(request).await?.status)
    }

    async fn patch_settings(
        &self,
        account: &Account,
        server_name: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = self
            .client
            .patch(format!("http://{}/api/v1/admin/settings", self.address))
            .header(COOKIE, &account.cookies)
            .header("x-csrf-token", &account.csrf)
            .json(&json!({ "serverName": server_name }))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        Ok(())
    }

    /// Changes one administrator's `canManageServer` through the real admin API.
    async fn set_can_manage_server(
        &self,
        actor: &Account,
        target: &Account,
        can_manage_server: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = self
            .client
            .patch(format!(
                "http://{}/api/v1/admin/users/{}",
                self.address, target.id
            ))
            .header(COOKIE, &actor.cookies)
            .header("x-csrf-token", &actor.csrf)
            .json(&json!({ "canManageServer": can_manage_server }))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        Ok(())
    }

    /// Creates one scanned movie and returns `(itemId, sourceId)` for the playback API.
    async fn create_movie(&self) -> Result<(String, String), Box<dyn std::error::Error>> {
        let libraries = LibraryService::new(self.database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let root = self._temp.path().join("Movies");
        tokio::fs::create_dir_all(&root).await?;
        tokio::fs::write(root.join("Realtime.Fixture.2024.mp4"), b"fixture").await?;
        libraries
            .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
            .await?;
        LibraryScanner::new(self.database.clone())
            .scan_movie_library(library.id)
            .await?;
        let item_id: String =
            sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
                .fetch_one(self.database.pool())
                .await?;
        let source_id: String =
            sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
                .bind(&item_id)
                .fetch_one(self.database.pool())
                .await?;
        Ok((item_id, source_id))
    }

    /// Polls a fresh handshake until the connection budget has a free slot, or the deadline passes.
    ///
    /// This is how a test observes "the previous connection is gone": the slot is only returned
    /// after the connection task wrote its close frame, so the success is a positive signal that
    /// does not depend on how long the close happened to take.
    async fn wait_for_capacity(&self, account: &Account, deadline: Duration) -> Result<(), String> {
        let until = tokio::time::Instant::now() + deadline;
        loop {
            let attempt = self
                .handshake(with_cookie(self.request(), &account.session_cookie()))
                .await
                .map_err(|error| error.to_string())?;
            match attempt.status {
                // The probe socket is dropped immediately, which returns the slot again.
                101 => return Ok(()),
                429 => {}
                status => {
                    return Err(format!(
                        "unexpected handshake status {status} while waiting for a free slot"
                    ));
                }
            }
            if tokio::time::Instant::now() >= until {
                return Err(format!(
                    "the connection slot was still taken after {deadline:?}"
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Signals both realtime endpoints and waits for the bounded drain, exactly like `src/main.rs`.
    async fn shutdown_websocket_connections(&self) -> bool {
        tokio::time::timeout(
            Duration::from_secs(8),
            self.state.shutdown_websocket_connections(),
        )
        .await
        .expect("the shutdown drain must not block")
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.server.abort();
        let _ = self.shutdown.take();
    }
}

fn with_cookie(mut request: Request<()>, cookies: &str) -> Request<()> {
    request.headers_mut().insert(
        "cookie",
        HeaderValue::from_str(cookies).expect("valid cookie header"),
    );
    request
}

fn with_token(mut request: Request<()>, token: &str) -> Request<()> {
    request.headers_mut().insert(
        "x-emby-token",
        HeaderValue::from_str(token).expect("valid token header"),
    );
    request
}

fn with_header(mut request: Request<()>, name: &'static str, value: &str) -> Request<()> {
    request.headers_mut().insert(
        name,
        HeaderValue::from_str(value).expect("valid header value"),
    );
    request
}

async fn send_text(
    socket: &mut ClientSocket,
    payload: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    socket
        .send(Message::Text(payload.into()))
        .await
        .map_err(Into::into)
}

/// Every positive wait in this module polls against this deadline instead of a fixed window, so a
/// loaded machine only makes a test slower. It is far above anything a passing test can need: the
/// longest legitimate wait is one idle timeout plus a frame round trip, and even the server's own
/// send bound is 10 s.
const CONDITION_DEADLINE: Duration = Duration::from_secs(20);

/// Window of a negative assertion ("nothing must arrive"). Kept deliberately short: the absence is
/// checked right after the positive frame of the same publish, and a longer window would only make
/// a passing test slower without making the observation stronger.
const QUIET_WINDOW: Duration = Duration::from_millis(400);

/// What the client saw when the server closed the connection.
struct CloseInfo {
    code: Option<u16>,
    reason: Option<String>,
}

impl CloseInfo {
    fn describe(&self, expected: &str) -> String {
        match self.code {
            Some(code) => format!(
                "connection closed with {code} ({:?}) while waiting for {expected}",
                self.reason
            ),
            None => format!("transport ended without a close frame while waiting for {expected}"),
        }
    }
}

/// One inbound event, or `None` once the deadline passed.
enum Inbound {
    Frame(Value),
    Closed(CloseInfo),
}

/// Reads the next inbound event without assuming the clock: every call carries the shared deadline,
/// so a slow machine delays a test instead of changing its outcome.
async fn next_inbound(
    socket: &mut ClientSocket,
    until: tokio::time::Instant,
) -> Result<Option<Inbound>, String> {
    loop {
        let remaining = until.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        match tokio::time::timeout(remaining, socket.next()).await {
            Err(_) => return Ok(None),
            Ok(None) => return Ok(Some(Inbound::Closed(close_without_frame()))),
            Ok(Some(Ok(Message::Text(text)))) => {
                return match serde_json::from_str::<Value>(text.as_ref()) {
                    Ok(frame) => Ok(Some(Inbound::Frame(frame))),
                    Err(error) => Err(format!("server sent a non-JSON text frame: {error}")),
                };
            }
            Ok(Some(Ok(
                Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_),
            ))) => {}
            Ok(Some(Ok(Message::Close(frame)))) => {
                return Ok(Some(Inbound::Closed(CloseInfo {
                    code: frame.as_ref().map(|frame| u16::from(frame.code)),
                    reason: frame.as_ref().map(|frame| frame.reason.as_str().to_owned()),
                })));
            }
            Ok(Some(Err(WsError::ConnectionClosed | WsError::Protocol(_) | WsError::Io(_)))) => {
                return Ok(Some(Inbound::Closed(close_without_frame())));
            }
            Ok(Some(Err(error))) => return Err(format!("websocket error: {error}")),
        }
    }
}

fn close_without_frame() -> CloseInfo {
    CloseInfo {
        code: None,
        reason: None,
    }
}

/// Polls until one frame satisfies `predicate`. `Ok(None)` means the deadline passed without the
/// frame (the caller's assertion then fails with its own message); a close is an error, because no
/// later frame can arrive.
async fn wait_for_frame(
    socket: &mut ClientSocket,
    deadline: Duration,
    mut predicate: impl FnMut(&Value) -> bool,
) -> Result<Option<Value>, String> {
    let until = tokio::time::Instant::now() + deadline;
    loop {
        match next_inbound(socket, until).await? {
            Some(Inbound::Frame(frame)) if predicate(&frame) => return Ok(Some(frame)),
            Some(Inbound::Frame(_)) => continue,
            Some(Inbound::Closed(close)) => {
                return Err(close.describe("a matching invalidation frame"));
            }
            None => return Ok(None),
        }
    }
}

/// Polls until `done` accepts the frames collected so far, and answers them either way so the
/// caller's assertion can print what actually arrived. A close before that is an error.
async fn wait_for_frames(
    socket: &mut ClientSocket,
    deadline: Duration,
    mut done: impl FnMut(&[Value]) -> bool,
) -> Result<Vec<Value>, String> {
    let until = tokio::time::Instant::now() + deadline;
    let mut frames = Vec::new();
    loop {
        if done(&frames) {
            return Ok(frames);
        }
        match next_inbound(socket, until).await? {
            Some(Inbound::Frame(frame)) => frames.push(frame),
            Some(Inbound::Closed(close)) => {
                return Err(close.describe("the expected invalidation frames"));
            }
            None => return Ok(frames),
        }
    }
}

/// The server's close frame, waiting until it arrives or the deadline passes.
async fn next_close(socket: &mut ClientSocket, deadline: Duration) -> Result<CloseInfo, String> {
    let until = tokio::time::Instant::now() + deadline;
    loop {
        match next_inbound(socket, until).await? {
            Some(Inbound::Closed(close)) => return Ok(close),
            Some(Inbound::Frame(_)) => continue,
            None => {
                return Err(format!(
                    "timed out after {deadline:?} waiting for the server to close the websocket"
                ));
            }
        }
    }
}

/// Text frames received inside `window` (bounded, no assertion). Used for short quiet windows and
/// for draining the invalidations a setup step publishes.
async fn collect_frames(socket: &mut ClientSocket, window: Duration) -> Vec<Value> {
    let until = tokio::time::Instant::now() + window;
    let mut frames = Vec::new();
    while let Ok(Some(event)) = next_inbound(socket, until).await {
        match event {
            Inbound::Frame(frame) => frames.push(frame),
            Inbound::Closed(_) => break,
        }
    }
    frames
}

/// Counts the protocol pings the server sends while this task keeps reading (which is what makes
/// tungstenite answer them with a pong), and only stops once `min_pings` arrived *and* `min_elapsed`
/// passed. A close is an error: the connection was supposed to stay alive.
async fn observe_protocol_pings(
    socket: &mut ClientSocket,
    deadline: Duration,
    min_elapsed: Duration,
    min_pings: usize,
) -> Result<usize, String> {
    let started = tokio::time::Instant::now();
    let until = started + deadline;
    let mut pings = 0;
    loop {
        if pings >= min_pings && started.elapsed() >= min_elapsed {
            return Ok(pings);
        }
        let remaining = until.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(pings);
        }
        match tokio::time::timeout(remaining, socket.next()).await {
            Err(_) => return Ok(pings),
            Ok(None) => return Err("transport ended while observing a live connection".to_owned()),
            Ok(Some(Ok(Message::Ping(_)))) => pings += 1,
            Ok(Some(Ok(
                Message::Pong(_) | Message::Text(_) | Message::Binary(_) | Message::Frame(_),
            ))) => {}
            Ok(Some(Ok(Message::Close(frame)))) => {
                return Err(format!(
                    "connection closed (code {:?}) while observing a live connection",
                    frame.map(|frame| u16::from(frame.code))
                ));
            }
            Ok(Some(Err(error))) => return Err(format!("websocket error: {error}")),
        }
    }
}

fn is_channel(frame: &Value, channel: &str) -> bool {
    frame["type"] == "invalidate" && frame["channel"] == channel
}

fn is_scope(frame: &Value, channel: &str, scope: &str) -> bool {
    is_channel(frame, channel) && frame["scope"] == scope
}

// ---------------------------------------------------------------------------------------------
// Upgrade required
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_plain_get_answers_426_upgrade_required() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    // Without any credential the answer is still 426: the upgrade check runs first.
    let anonymous = server
        .client
        .get(format!("http://{}/api/v1/ws", server.address))
        .send()
        .await?;
    assert_eq!(anonymous.status(), reqwest::StatusCode::UPGRADE_REQUIRED);
    // A cookie is present on purpose here: the 426 must come from the upgrade check, not auth.
    let response = server
        .client
        .get(format!("http://{}/api/v1/ws", server.address))
        .header(COOKIE, server.admin.session_cookie())
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::UPGRADE_REQUIRED);
    assert_eq!(
        response
            .headers()
            .get("upgrade")
            .and_then(|value| value.to_str().ok()),
        Some("websocket")
    );
    // Not a `200` empty body that a health probe could mistake for a working endpoint.
    assert_ne!(response.status(), reqwest::StatusCode::OK);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Origin gate (ADR-054 "Origin 校验", rules 1-7)
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_same_origin_cookie_handshake_upgrades() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let mut socket = server.connect_account(&server.viewer).await?;
    // The connection really works: one user invalidation must arrive.
    server.patch_settings(&server.admin, "Same Origin").await?;
    let frame = wait_for_frame(&mut socket, CONDITION_DEADLINE, |frame| {
        is_scope(frame, "user", "home")
    })
    .await?
    .ok_or("a same-origin cookie connection must receive user frames")?;
    assert_eq!(frame["scope"], "home");
    Ok(())
}

#[tokio::test]
async fn a_cookie_handshake_without_an_origin_is_refused() -> Result<(), Box<dyn std::error::Error>>
{
    let server = spawn_server(limits()).await?;
    let request = with_cookie(
        server.request_with_path("/api/v1/ws"),
        &server.viewer.session_cookie(),
    );
    let request = {
        let mut request = request;
        request.headers_mut().remove("origin");
        request
    };
    assert_eq!(server.refusal(request).await?, 401);
    Ok(())
}

#[tokio::test]
async fn a_null_or_unparsable_origin_is_treated_as_missing()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    for origin in [
        "null",
        "http://example.com/path",
        "http://example.com, http://evil.example",
        "example.com",
        "ws://example.com",
    ] {
        let request = with_cookie(server.request(), &server.viewer.session_cookie());
        let request = with_header(request, "origin", origin);
        assert_eq!(
            server.refusal(request).await?,
            401,
            "`{origin}` must be treated as a missing origin for a cookie handshake"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_client_token_may_omit_the_origin() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let request = with_token(server.request(), &server.viewer.token);
    let mut request = request;
    request.headers_mut().remove("origin");
    assert_eq!(server.handshake(request).await?.status, 101);

    // A literal `null` Origin (a sandboxed or file:// page) is "missing" too, so only a token is
    // accepted.
    let request = with_token(server.request(), &server.viewer.token);
    let request = with_header(request, "origin", "null");
    assert_eq!(server.handshake(request).await?.status, 101);
    Ok(())
}

#[tokio::test]
async fn host_case_and_default_ports_are_normalized() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    // Host and Origin differ in case and in the default port; both normalize to `example.com`.
    let request = with_token(server.request(), &server.viewer.token);
    let request = with_header(request, "host", "Example.COM");
    let request = with_header(request, "origin", "http://example.com:80");
    assert_eq!(server.handshake(request).await?.status, 101);

    // The scheme is not part of the comparison, and 443 is the default of the other scheme.
    let request = with_token(server.request(), &server.viewer.token);
    let request = with_header(request, "host", "example.com:443");
    let request = with_header(request, "origin", "https://EXAMPLE.com");
    assert_eq!(server.handshake(request).await?.status, 101);

    // A non-default port must be present on both sides.
    let request = with_token(server.request(), &server.viewer.token);
    let request = with_header(request, "host", "example.com:8097");
    let request = with_header(request, "origin", "https://example.com:8097");
    assert_eq!(server.handshake(request).await?.status, 101);

    let request = with_token(server.request(), &server.viewer.token);
    let request = with_header(request, "host", "example.com:8097");
    let request = with_header(request, "origin", "https://example.com");
    assert_eq!(server.refusal(request).await?, 400);
    Ok(())
}

#[tokio::test]
async fn a_mismatched_origin_is_refused_before_the_upgrade()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let request = with_cookie(server.request(), &server.viewer.session_cookie());
    let request = with_header(request, "origin", "https://evil.example");
    assert_eq!(server.refusal(request).await?, 400);
    // A client token does not bypass the comparison either.
    let request = with_token(server.request(), &server.viewer.token);
    let request = with_header(request, "origin", "https://evil.example");
    assert_eq!(server.refusal(request).await?, 400);

    // The request authority can be overridden independently, so the comparison is real.
    let request = with_cookie(server.request(), &server.viewer.session_cookie());
    let request = with_header(request, "host", "evil.example");
    let request = with_header(request, "origin", &server.origin);
    assert_eq!(server.refusal(request).await?, 400);
    Ok(())
}

#[tokio::test]
async fn cross_site_is_refused_even_with_a_matching_origin()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let request = with_cookie(server.request(), &server.viewer.session_cookie());
    let request = with_header(request, "sec-fetch-site", "cross-site");
    assert_eq!(server.refusal(request).await?, 400);

    // A missing `Sec-Fetch-Site` is not a rejection, and `same-origin` is fine.
    let request = with_cookie(server.request(), &server.viewer.session_cookie());
    let request = with_header(request, "sec-fetch-site", "same-origin");
    assert_eq!(server.handshake(request).await?.status, 101);
    Ok(())
}

#[tokio::test]
async fn forwarded_headers_never_widen_the_origin_gate() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let request = with_cookie(server.request(), &server.viewer.session_cookie());
    let request = with_header(request, "origin", "https://evil.example");
    let request = with_header(request, "x-forwarded-proto", "https");
    let request = with_header(request, "x-forwarded-host", "evil.example");
    let request = with_header(request, "forwarded", "host=evil.example;proto=https");
    let request = with_header(request, "x-real-ip", "203.0.113.9");
    assert_eq!(
        server.refusal(request).await?,
        400,
        "a forwarded header must not make a foreign Origin match"
    );
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_unauthenticated_handshake_answers_401() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    assert_eq!(server.refusal(server.request()).await?, 401);
    Ok(())
}

#[tokio::test]
async fn a_query_credential_is_never_read() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    // A valid token in the query string (the Emby habit) must not authenticate this endpoint.
    let request = server.request_with_path(&format!("/api/v1/ws?token={}", server.viewer.token));
    assert_eq!(server.refusal(request).await?, 401);
    // Query parameters are otherwise ignored, not rejected.
    let request = with_token(
        server.request_with_path("/api/v1/ws?foo=bar&deviceId=test"),
        &server.viewer.token,
    );
    assert_eq!(server.handshake(request).await?.status, 101);
    Ok(())
}

#[tokio::test]
async fn a_client_token_authenticates_the_handshake() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let request = with_token(server.request(), &server.viewer.token);
    let mut socket = server.connect(request).await?;
    server
        .patch_settings(&server.admin, "Token Channel")
        .await?;
    let frame = wait_for_frame(&mut socket, CONDITION_DEADLINE, |frame| {
        is_scope(frame, "user", "home")
    })
    .await?
    .ok_or("a token connection must receive user frames")?;
    assert_eq!(frame["channel"], "user");
    Ok(())
}

#[tokio::test]
async fn a_blocked_client_is_refused_with_403() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    // A client rule matching the Web client turns the principal resolution into `AccessDenied`.
    sqlx::query(
        "INSERT INTO user_access_blacklist_rules (
             id, user_id, rule_type, match_value, is_enabled, created_by, created_at, updated_at
         ) VALUES ('lux-ws-blocked-client', ?, 'CLIENT', 'lux web', 1, ?, unixepoch(), unixepoch())",
    )
    .bind(&server.viewer.id)
    .bind(&server.admin.id)
    .execute(server.database.pool())
    .await?;
    let request = with_cookie(server.request(), &server.viewer.session_cookie());
    assert_eq!(server.refusal(request).await?, 403);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_administrator_receives_both_channels_a_viewer_only_the_user_channel()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let mut operator = server.connect_account(&server.operator).await?;
    let mut viewer = server.connect_account(&server.viewer).await?;

    server.patch_settings(&server.admin, "Two Channels").await?;

    // The administrator's one connection carries both channels; their relative order is not part
    // of the contract, so the test polls until both arrived instead of assuming an order.
    let frames = wait_for_frames(&mut operator, CONDITION_DEADLINE, |frames| {
        frames
            .iter()
            .any(|frame| is_scope(frame, "admin", "settings"))
            && frames.iter().any(|frame| is_scope(frame, "user", "home"))
    })
    .await?;
    assert!(
        frames
            .iter()
            .any(|frame| is_scope(frame, "admin", "settings")),
        "an administrator must receive the admin channel, got {frames:?}"
    );
    // `publish_user_invalidation(Settings)` maps to `home`; that mapping must not change.
    assert!(
        frames.iter().any(|frame| is_scope(frame, "user", "home")),
        "the admin publish must still reach the user scope, got {frames:?}"
    );
    assert!(
        frames
            .iter()
            .filter(|frame| is_channel(frame, "admin"))
            .all(|frame| frame["type"] == "invalidate"),
        "the admin channel only carries invalidations"
    );

    // A regular user never sees the admin channel: wait for the positive user frame first, then
    // look for an admin frame only in a short quiet window (a longer one would not be stronger).
    let frames = wait_for_frames(&mut viewer, CONDITION_DEADLINE, |frames| {
        frames.iter().any(|frame| is_scope(frame, "user", "home"))
    })
    .await?;
    assert!(
        frames.iter().any(|frame| is_scope(frame, "user", "home")),
        "a viewer must receive the broadcast user invalidation, got {frames:?}"
    );
    let extra = collect_frames(&mut viewer, QUIET_WINDOW).await;
    assert!(
        !frames
            .iter()
            .chain(extra.iter())
            .any(|frame| is_channel(frame, "admin")),
        "a viewer must never receive admin frames, got {frames:?} {extra:?}"
    );
    // The envelope never carries a library id (ADR-054 D14) or any business payload.
    assert!(
        frames.iter().all(|frame| frame.get("libraryId").is_none()),
        "the envelope must not carry libraryId"
    );
    Ok(())
}

#[tokio::test]
async fn a_progress_event_carries_the_item_id() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let (item_id, source_id) = server.create_movie().await?;
    let mut operator = server.connect_account(&server.operator).await?;

    let session = server
        .client
        .post(format!(
            "http://{}/api/v1/playback/sessions",
            server.address
        ))
        .header(COOKIE, &server.operator.cookies)
        .header("x-csrf-token", &server.operator.csrf)
        .json(&json!({
            "itemId": item_id,
            "sourceId": source_id,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(session.status(), reqwest::StatusCode::OK);
    let session: Value = session.json().await?;
    let session_id = session["sessionId"].as_str().ok_or("missing sessionId")?;

    let reported = server
        .client
        .post(format!(
            "http://{}/api/v1/playback/sessions/{session_id}/events",
            server.address
        ))
        .header(COOKIE, &server.operator.cookies)
        .header("x-csrf-token", &server.operator.csrf)
        .json(&json!({
            "eventId": "lux-ws-progress-1",
            "sequence": 1,
            "state": "PLAYING",
            "positionTicks": 1_000_000_000_i64,
            "durationTicks": 2_000_000_000_i64
        }))
        .send()
        .await?;
    assert_eq!(reported.status(), reqwest::StatusCode::OK);

    let frame = wait_for_frame(&mut operator, CONDITION_DEADLINE, |frame| {
        is_scope(frame, "user", "progress")
    })
    .await?
    .ok_or("a playback progress tick must produce a progress invalidation")?;
    assert_eq!(frame["itemId"], item_id);
    assert_eq!(frame["channel"], "user");
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Inbound frames
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_binary_frame_closes_with_1003() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let mut socket = server.connect_account(&server.viewer).await?;
    socket
        .send(Message::Binary(vec![1_u8, 2, 3].into()))
        .await?;
    let close = next_close(&mut socket, CONDITION_DEADLINE).await?;
    assert_eq!(close.code, Some(1003), "{}", close.describe("1003"));
    Ok(())
}

#[tokio::test]
async fn a_message_above_4_kib_closes_with_1009() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let mut socket = server.connect_account(&server.viewer).await?;
    // 4097 bytes: one over the documented contract limit (the transport buffer is larger on
    // purpose, so the handler can write the close frame itself).
    send_text(&mut socket, &"x".repeat(4 * 1024 + 1)).await?;
    let close = next_close(&mut socket, CONDITION_DEADLINE).await?;
    assert_eq!(close.code, Some(1009), "{}", close.describe("1009"));
    Ok(())
}

#[tokio::test]
async fn unknown_message_types_are_ignored_and_do_not_close_the_connection()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let mut socket = server.connect_account(&server.viewer).await?;
    for payload in [
        r#"{"type":"subscribe","channels":["admin"]}"#,
        r#"{"type":"pong"}"#,
        r#"{"MessageType":"SessionsStart"}"#,
        "not json at all",
        "[]",
        r#"{"type":"ping"}"#,
    ] {
        send_text(&mut socket, payload).await?;
    }
    server
        .patch_settings(&server.admin, "Unknown Types")
        .await?;
    let frame = wait_for_frame(&mut socket, CONDITION_DEADLINE, |frame| {
        is_scope(frame, "user", "home")
    })
    .await?
    .ok_or("the connection must stay usable after unknown inbound frames")?;
    assert_eq!(frame["channel"], "user");
    Ok(())
}

#[tokio::test]
async fn the_application_ping_is_never_answered_with_a_pong()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let mut socket = server.connect_account(&server.viewer).await?;
    send_text(&mut socket, r#"{"type":"ping"}"#).await?;
    // Negative assertion, so the window stays short (a longer one would not be stronger).
    let frames = collect_frames(&mut socket, QUIET_WINDOW).await;
    assert!(
        frames.is_empty(),
        "the envelope has no pong frame, received {frames:?}"
    );
    Ok(())
}

#[tokio::test]
async fn protocol_pings_keep_a_silent_connection_alive() -> Result<(), Box<dyn std::error::Error>> {
    // Ratio of the two clocks: the idle window (2.5 s) is 10x the ping interval (250 ms) and the
    // interval is 50x a typical 5 ms scheduling delay, so a preemption cannot turn a live
    // connection into an idle one. The observation below then lasts 1.6 idle windows.
    let server = spawn_server(LuxWebsocketLimits {
        protocol: WebsocketLimits {
            heartbeat_interval: Duration::from_millis(250),
            idle_timeout: Duration::from_millis(2500),
            ..WebsocketLimits::default()
        },
        ..LuxWebsocketLimits::default()
    })
    .await?;
    let mut socket = server.connect_account(&server.viewer).await?;
    // Reading is what answers the protocol pings with a pong, which is the only proof of life a
    // browser sends while the user does nothing. Counting the pings is the positive form of "the
    // server kept the heartbeat running".
    let pings = observe_protocol_pings(
        &mut socket,
        CONDITION_DEADLINE,
        Duration::from_millis(4000),
        4,
    )
    .await?;
    assert!(
        pings >= 4,
        "a silent connection must keep receiving protocol pings, saw {pings}"
    );
    // The positive assertion: after more than one idle window of ponging only, the connection
    // still delivers a frame.
    server
        .patch_settings(&server.admin, "Ping Keeps Alive")
        .await?;
    let frame = wait_for_frame(&mut socket, CONDITION_DEADLINE, |frame| {
        is_scope(frame, "user", "home")
    })
    .await?
    .ok_or("a connection that answered the protocol pings must still be open")?;
    assert_eq!(frame["channel"], "user");
    Ok(())
}

#[tokio::test]
async fn an_idle_connection_is_closed_with_1001_and_the_slot_is_released()
-> Result<(), Box<dyn std::error::Error>> {
    // The idle window (1 s) is 200x a typical 5 ms scheduling delay, and the heartbeat is set to
    // the same value on purpose: `normalized()` keeps idle >= heartbeat and the loop checks the
    // idle deadline first, so this connection is closed before any ping is sent. A ping would be
    // queued in the test client, which answers it while reading after the server already dropped
    // the socket; that write can reset the connection and hide the close frame. The ping/idle
    // ratio is the subject of `protocol_pings_keep_a_silent_connection_alive` (250 ms / 2.5 s).
    let server = spawn_server(LuxWebsocketLimits {
        user_max_connections: 1,
        user_max_connections_per_user: 1,
        protocol: WebsocketLimits {
            heartbeat_interval: Duration::from_millis(1000),
            idle_timeout: Duration::from_millis(1000),
            ..WebsocketLimits::default()
        },
        ..LuxWebsocketLimits::default()
    })
    .await?;
    let mut socket = server.connect_account(&server.viewer).await?;
    // This socket is deliberately never polled again, so no inbound frame can ever arrive and the
    // idle deadline fires. Instead of sleeping, poll the budget: the slot is returned only after
    // the connection task wrote its close frame, so a free slot is a positive signal that the close
    // already happened and reading it cannot block.
    server
        .wait_for_capacity(&server.viewer_two, CONDITION_DEADLINE)
        .await?;
    let close = next_close(&mut socket, CONDITION_DEADLINE).await?;
    assert_eq!(
        close.code,
        Some(1001),
        "{}",
        close.describe("an idle close")
    );
    assert_eq!(
        close.reason.as_deref(),
        Some("idle timeout"),
        "the close must come from the idle timer, not from another path"
    );
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Budgets
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_per_user_budget_answers_429_with_retry_after() -> Result<(), Box<dyn std::error::Error>>
{
    let server = spawn_server(LuxWebsocketLimits {
        user_max_connections: 8,
        user_max_connections_per_user: 1,
        ..LuxWebsocketLimits::default()
    })
    .await?;
    let _first = server.connect_account(&server.viewer).await?;
    let attempt = server
        .handshake(with_cookie(
            server.request(),
            &server.viewer.session_cookie(),
        ))
        .await?;
    assert_eq!(attempt.status, 429);
    assert_eq!(attempt.retry_after.as_deref(), Some("30"));
    // Another account still has its own slot, so the refusal is per user.
    assert_eq!(
        server
            .refusal(with_cookie(
                server.request(),
                &server.viewer_two.session_cookie()
            ))
            .await?,
        101
    );
    Ok(())
}

#[tokio::test]
async fn the_global_budget_answers_429_and_releases_the_slot()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(LuxWebsocketLimits {
        user_max_connections: 1,
        user_max_connections_per_user: 4,
        ..LuxWebsocketLimits::default()
    })
    .await?;
    let first = server.connect_account(&server.viewer).await?;
    assert_eq!(
        server
            .refusal(with_cookie(
                server.request(),
                &server.viewer_two.session_cookie()
            ))
            .await?,
        429
    );
    drop(first);
    // Poll the budget instead of waiting a fixed time: the server may need a moment to notice the
    // disconnect, and a loaded machine may need more than a short window.
    server
        .wait_for_capacity(&server.viewer_two, CONDITION_DEADLINE)
        .await?;
    Ok(())
}

#[tokio::test]
async fn the_admin_and_user_budgets_are_separate_and_classified()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(LuxWebsocketLimits {
        user_max_connections: 1,
        user_max_connections_per_user: 1,
        admin_max_connections: 1,
        admin_max_connections_per_user: 1,
        ..LuxWebsocketLimits::default()
    })
    .await?;
    // One viewer occupies the whole user budget; the administrator's connection is counted in the
    // admin budget instead, so it is admitted even though the user budget is exhausted.
    let _viewer = server.connect_account(&server.viewer).await?;
    let _operator = server.connect_account(&server.operator).await?;
    assert_eq!(
        server
            .refusal(with_cookie(
                server.request(),
                &server.viewer_two.session_cookie()
            ))
            .await?,
        429,
        "the user budget is full"
    );
    assert_eq!(
        server
            .handshake(with_cookie(
                server.request(),
                &server.operator.session_cookie(),
            ))
            .await?
            .status,
        429,
        "the admin budget is full, and an administrator is never counted against the user budget"
    );
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Revalidation: demotion, revocation, transient failure
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_demotion_stops_admin_frames_without_closing_the_connection()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(fast_timers()).await?;
    let mut operator = server.connect_account(&server.operator).await?;

    server
        .set_can_manage_server(&server.admin, &server.operator, false)
        .await?;
    // The permission change is itself an admin event (`users`) plus a user `home`; drain whatever
    // it published so the observation starts from a clean slate (bounded, not an assertion).
    let _ = collect_frames(&mut operator, QUIET_WINDOW).await;

    // Poll until a published settings invalidation reaches the user channel but no longer the admin
    // channel. Each attempt publishes one event, waits for its user frame and then looks for the
    // admin frame of the same publish in a short quiet window; two clean attempts in a row are
    // required, so one delayed admin frame cannot be mistaken for the demotion.
    let deadline = Instant::now() + CONDITION_DEADLINE;
    let mut clean_attempts = 0;
    let mut last_frames: Vec<Value> = Vec::new();
    while Instant::now() < deadline {
        server
            .patch_settings(&server.admin, "After Demotion")
            .await?;
        let mut frames = wait_for_frames(&mut operator, Duration::from_secs(5), |frames| {
            frames.iter().any(|frame| is_scope(frame, "user", "home"))
        })
        .await?;
        frames.extend(collect_frames(&mut operator, QUIET_WINDOW).await);
        let saw_user = frames.iter().any(|frame| is_scope(frame, "user", "home"));
        let has_admin = frames
            .iter()
            .any(|frame| is_scope(frame, "admin", "settings"));
        // A clean attempt means "this publish reached the user channel but not the admin channel";
        // a publish that produced no user frame proves nothing and restarts the count.
        clean_attempts = if saw_user && !has_admin {
            clean_attempts + 1
        } else {
            0
        };
        last_frames = frames;
        if clean_attempts >= 2 {
            break;
        }
    }
    assert!(
        clean_attempts >= 2,
        "a demoted connection must stop receiving admin frames; last frames {last_frames:?}"
    );

    // Regaining the permission resumes the admin channel; the connection was never closed.
    server
        .set_can_manage_server(&server.admin, &server.operator, true)
        .await?;
    let _ = collect_frames(&mut operator, QUIET_WINDOW).await;
    let mut resumed = false;
    let deadline = Instant::now() + CONDITION_DEADLINE;
    while Instant::now() < deadline {
        server
            .patch_settings(&server.admin, "After Promotion")
            .await?;
        if let Some(frame) = wait_for_frame(&mut operator, Duration::from_secs(5), |frame| {
            is_scope(frame, "admin", "settings")
        })
        .await?
        {
            assert_eq!(frame["channel"], "admin");
            resumed = true;
            break;
        }
    }
    assert!(
        resumed,
        "the admin channel must resume after the permission is restored"
    );
    Ok(())
}

#[tokio::test]
async fn a_revoked_session_closes_with_1008() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(fast_timers()).await?;
    let mut viewer = server.connect_account(&server.viewer).await?;
    let logout = server
        .client
        .post(format!("http://{}/api/v1/auth/logout", server.address))
        .header(COOKIE, &server.viewer.cookies)
        .header("x-csrf-token", &server.viewer.csrf)
        .send()
        .await?;
    assert_eq!(logout.status(), reqwest::StatusCode::NO_CONTENT);
    let close = next_close(&mut viewer, CONDITION_DEADLINE).await?;
    assert_eq!(close.code, Some(1008), "{}", close.describe("1008"));
    Ok(())
}

#[tokio::test]
async fn a_transient_revalidation_failure_keeps_the_connection()
-> Result<(), Box<dyn std::error::Error>> {
    // A slow timer, so every rule below exists before the first revalidation runs; an intermediate
    // state (a few rules, a matching address) would legitimately log the session out instead.
    let server = spawn_server(LuxWebsocketLimits {
        protocol: WebsocketLimits {
            revalidate_interval: Duration::from_millis(800),
            ..WebsocketLimits::default()
        },
        ..LuxWebsocketLimits::default()
    })
    .await?;
    let mut viewer = server.connect_account(&server.viewer).await?;
    // More enabled IP rules than one request may evaluate make `resolve_principal` report
    // `AccessRulesUnavailable`, i.e. the temporary 5xx branch, without touching the session.
    for index in 0..201 {
        sqlx::query(
            "INSERT INTO user_access_blacklist_rules (
                 id, user_id, rule_type, match_value, is_enabled, created_by,
                 created_at, updated_at
             ) VALUES (?, ?, 'IP_CIDR', ?, 1, ?, unixepoch(), unixepoch())",
        )
        .bind(format!("lux-ws-ip-{index}"))
        .bind(&server.viewer.id)
        .bind(format!("10.0.{}/32", index % 256))
        .bind(&server.admin.id)
        .execute(server.database.pool())
        .await?;
    }
    // Poll: publish one invalidation at a time and wait for its user frame, until at least four
    // revalidation intervals (4 x 800 ms) elapsed while the connection stayed usable. A passing run
    // stops as soon as both conditions hold; only a failing one waits for the deadline.
    let started = tokio::time::Instant::now();
    let deadline = tokio::time::Instant::now() + CONDITION_DEADLINE;
    let mut rounds = 0;
    while tokio::time::Instant::now() < deadline
        && (rounds < 3 || started.elapsed() < Duration::from_millis(3200))
    {
        server
            .patch_settings(&server.admin, "Transient Failure")
            .await?;
        if let Some(frame) = wait_for_frame(&mut viewer, Duration::from_secs(5), |frame| {
            is_scope(frame, "user", "home")
        })
        .await?
        {
            assert_eq!(frame["channel"], "user");
            rounds += 1;
        }
    }
    assert!(
        rounds >= 3,
        "the connection must survive several failed revalidations, saw {rounds}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Shutdown
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn shutdown_closes_with_1001_without_blocking_and_refuses_new_handshakes()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let mut operator = server.connect_account(&server.operator).await?;
    let mut viewer = server.connect_account(&server.viewer).await?;

    let started = Instant::now();
    // The meaningful bound is the 5 s drain timeout inside `shutdown_websocket_connections`: it
    // answered `true`, so no connection outlived it. The wall clock gets a generous cap only so a
    // loaded machine cannot turn the scheduling delay into a failure.
    assert!(
        server.shutdown_websocket_connections().await,
        "every connection must release its slot inside the drain timeout"
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "an open connection must not block the shutdown chain, took {:?}",
        started.elapsed()
    );
    let operator_close = next_close(&mut operator, CONDITION_DEADLINE).await?;
    assert_eq!(
        operator_close.code,
        Some(1001),
        "{}",
        operator_close.describe("1001")
    );
    let viewer_close = next_close(&mut viewer, CONDITION_DEADLINE).await?;
    assert_eq!(
        viewer_close.code,
        Some(1001),
        "{}",
        viewer_close.describe("1001")
    );

    // After the signal a new handshake is refused instead of upgrading into a dead connection.
    assert_eq!(
        server
            .refusal(with_cookie(
                server.request(),
                &server.viewer_two.session_cookie()
            ))
            .await?,
        503
    );
    Ok(())
}
