//! `GET /embywebsocket` (LUX-891).
//!
//! Yamby's server-status page opens `ws(s)://{host}/embywebsocket?api_key=…&deviceId=…`, sends
//! `SessionsStart` and `ScheduledTasksInfoStart`, and silently retries the handshake without any
//! backoff when it fails. These tests pin the whole contract over a real socket: the handshake
//! answers on the root path, the `/emby` mount and any casing; the pushed `Data` is exactly what
//! `GET /Sessions` / `GET /ScheduledTasks` return for the same credential; a non-administrator
//! never gets the scan state; every resource limit answers before the upgrade; and an open
//! connection cannot stall the graceful shutdown.

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use luxd::{
    api::{AppState, EmbyWebsocketLimits, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Error as WsError, Message},
};

type ClientSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct TestServer {
    base_url: String,
    ws_url: String,
    client: reqwest::Client,
    admin_token: String,
    viewer_token: String,
    database: Database,
    state: AppState,
    shutdown: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<Result<(), std::io::Error>>,
    _temp_dir: tempfile::TempDir,
}

fn uuid_to_emby_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
}

async fn emby_login(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
    device_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            format!(
                r#"Emby Client="WebSocketTest", Device="Mac", DeviceId="{device_id}", Version="1""#
            ),
        )
        .json(&json!({ "Username": username, "Pw": password }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await?;
    Ok(body["AccessToken"]
        .as_str()
        .ok_or("missing access token")?
        .to_owned())
}

/// Starts an isolated Lux instance: one movie library with one file, an administrator and a
/// non-administrator account, both with an Emby token.
async fn spawn_server(
    limits: EmbyWebsocketLimits,
) -> Result<TestServer, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("WebSocket Test 2024.mkv"), b"video").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let state = AppState::ready(config, database.clone(), setup, web_auth, emby_auth)
        .with_emby_websocket_limits(limits);
    let app = app_with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown_state = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
                // Mirrors `src/main.rs`: connections are told to close before the drain waits.
                shutdown_state.signal_emby_websocket_shutdown();
            })
            .await
    });
    let base_url = format!("http://{address}");
    let ws_url = format!("ws://{address}");
    let client = reqwest::Client::builder().no_proxy().build()?;
    let admin_token =
        emby_login(&client, &base_url, "Admin", "correct password", "ws-admin").await?;
    let viewer_token =
        emby_login(&client, &base_url, "viewer", "viewer password", "ws-viewer").await?;
    Ok(TestServer {
        base_url,
        ws_url,
        client,
        admin_token,
        viewer_token,
        database,
        state,
        shutdown,
        server,
        _temp_dir: temp_dir,
    })
}

impl TestServer {
    async fn get(&self, path: &str, token: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(format!("{}{path}", self.base_url))
            .header("X-Emby-Token", token)
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::OK,
            "`{path}` must be 200"
        );
        Ok(response.json().await?)
    }

    /// Records one Emby playback session so `/Sessions` has a `NowPlayingItem`/`PlayState` entry.
    async fn start_playback(&self) -> Result<(), Box<dyn std::error::Error>> {
        let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
            .fetch_one(self.database.pool())
            .await?;
        let source_id: Option<String> =
            sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
                .bind(&item_id)
                .fetch_optional(self.database.pool())
                .await?;
        let mut body = json!({
            "ItemId": uuid_to_emby_id(&item_id),
            "PlaySessionId": "websocket-contract-session",
            "PositionTicks": 120_000_000_i64,
            "RunTimeTicks": 600_000_000_i64,
            "CanSeek": true,
        });
        if let Some(source_id) = source_id {
            body["MediaSourceId"] = json!(source_id);
        }
        let response = self
            .client
            .post(format!("{}/Sessions/Playing", self.base_url))
            .header("X-Emby-Token", &self.admin_token)
            .json(&body)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        Ok(())
    }

    /// Opens the WebSocket the way Yamby does: `api_key` in the query, `deviceId` alongside it.
    async fn connect(
        &self,
        path: &str,
        token: &str,
    ) -> Result<ClientSocket, Box<dyn std::error::Error>> {
        let path = path.trim_start_matches('/');
        let url = format!(
            "{}/{path}?api_key={token}&deviceId=ws-test-device",
            self.ws_url
        );
        let (socket, response) = tokio_tungstenite::connect_async(url.as_str()).await?;
        assert_eq!(response.status().as_u16(), 101);
        Ok(socket)
    }

    /// The status code of a refused handshake, or `101` when the upgrade succeeded.
    async fn handshake_status(
        &self,
        path: &str,
        token: &str,
    ) -> Result<u16, Box<dyn std::error::Error>> {
        let path = path.trim_start_matches('/');
        let url = format!(
            "{}/{path}?api_key={token}&deviceId=ws-test-device",
            self.ws_url
        );
        match tokio_tungstenite::connect_async(url.as_str()).await {
            Ok(_) => Ok(101),
            Err(WsError::Http(response)) => Ok(response.status().as_u16()),
            Err(error) => Err(format!("unexpected handshake error: {error}").into()),
        }
    }

    /// Delivers the graceful shutdown the way `src/main.rs` does and returns how long the server
    /// took to stop accepting and draining.
    async fn shutdown(mut self) -> Result<Duration, Box<dyn std::error::Error>> {
        let started = Instant::now();
        let _ = self.shutdown.send(());
        let joined = tokio::time::timeout(Duration::from_secs(5), &mut self.server)
            .await
            .map_err(|_| "the server did not stop within 5 s with an open websocket")?;
        joined??;
        Ok(started.elapsed())
    }
}

async fn send(stream: &mut ClientSocket, payload: Value) -> Result<(), Box<dyn std::error::Error>> {
    stream
        .send(Message::Text(payload.to_string().into()))
        .await?;
    Ok(())
}

async fn send_raw(stream: &mut ClientSocket, text: &str) -> Result<(), Box<dyn std::error::Error>> {
    stream.send(Message::Text(text.into())).await?;
    Ok(())
}

/// Reads the next text frame, skipping the control frames the endpoint itself may send.
async fn next_json(stream: &mut ClientSocket) -> Result<Value, Box<dyn std::error::Error>> {
    let text = next_text(stream).await?;
    Ok(serde_json::from_str(&text)?)
}

async fn next_text(stream: &mut ClientSocket) -> Result<String, Box<dyn std::error::Error>> {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .map_err(|_| "timed out waiting for a websocket message")?;
        match message {
            Some(Ok(Message::Text(text))) => return Ok(text.to_string()),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(Message::Close(frame))) => {
                return Err(format!("websocket closed before a text frame: {frame:?}").into());
            }
            Some(Ok(other)) => return Err(format!("unexpected websocket frame: {other:?}").into()),
            Some(Err(error)) => return Err(format!("websocket error: {error}").into()),
            None => return Err("websocket stream ended".into()),
        }
    }
}

/// Waits for the server's close frame and returns its code, or `None` when the transport ended
/// without one.
async fn next_close(stream: &mut ClientSocket) -> Result<Option<u16>, Box<dyn std::error::Error>> {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .map_err(|_| "timed out waiting for the server to close the websocket")?;
        match message {
            Some(Ok(Message::Close(frame))) => return Ok(frame.map(|frame| u16::from(frame.code))),
            Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Text(_))) => continue,
            Some(Ok(_)) => continue,
            Some(Err(WsError::ConnectionClosed | WsError::Protocol(_) | WsError::Io(_))) | None => {
                return Ok(None);
            }
            Some(Err(error)) => return Err(format!("websocket error: {error}").into()),
        }
    }
}

async fn expect_no_message(
    stream: &mut ClientSocket,
    window: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    match tokio::time::timeout(window, stream.next()).await {
        Err(_) => Ok(()),
        Ok(Some(Ok(message))) => Err(format!("expected no message, received {message:?}").into()),
        Ok(Some(Err(error))) => Err(format!("websocket closed unexpectedly: {error}").into()),
        Ok(None) => Err("websocket closed unexpectedly".into()),
    }
}

fn limits() -> EmbyWebsocketLimits {
    EmbyWebsocketLimits::default()
}

#[tokio::test]
async fn emby_websocket_pushes_the_same_sessions_as_the_http_endpoint()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    server.start_playback().await?;
    let http_sessions = server.get("/Sessions", &server.admin_token).await?;

    let mut socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    send(
        &mut socket,
        json!({"MessageType": "SessionsStart", "Data": "0,1500,0,true,true"}),
    )
    .await?;
    let message = next_json(&mut socket).await?;
    assert_eq!(message["MessageType"], "Sessions");
    let pushed = message["Data"]
        .as_array()
        .ok_or("Sessions Data must be an array")?;
    assert_eq!(
        message["Data"].clone(),
        http_sessions,
        "the pushed Data must equal GET /Sessions for the same credential"
    );
    // Yamby drops an entry without either field, so both must be present.
    let session = pushed.first().ok_or("expected one playback session")?;
    assert!(session["NowPlayingItem"].is_object());
    assert!(session["PlayState"].is_object());
    assert_eq!(session["PlayState"]["PositionTicks"], 120_000_000_i64);
    Ok(())
}

#[tokio::test]
async fn emby_websocket_pushes_scheduled_tasks_to_administrators()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let http_tasks = server.get("/ScheduledTasks", &server.admin_token).await?;

    let mut socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    send(
        &mut socket,
        json!({"MessageType": "ScheduledTasksInfoStart", "Data": "1000,1000"}),
    )
    .await?;
    let message = next_json(&mut socket).await?;
    assert_eq!(message["MessageType"], "ScheduledTasksInfo");
    assert_eq!(
        message["Data"].clone(),
        http_tasks,
        "the pushed Data must equal GET /ScheduledTasks for the same credential"
    );
    Ok(())
}

#[tokio::test]
async fn emby_websocket_ignores_scheduled_tasks_for_non_administrators()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let mut socket = server
        .connect("/embywebsocket", &server.viewer_token)
        .await?;
    send(
        &mut socket,
        json!({"MessageType": "ScheduledTasksInfoStart", "Data": "0,1000"}),
    )
    .await?;
    // Nothing may arrive: the request is ignored silently, not turned into a push or an error.
    expect_no_message(&mut socket, Duration::from_millis(1600)).await?;
    // The connection is still usable, which is what distinguishes "ignored" from "disconnected".
    send(&mut socket, json!({"MessageType": "KeepAlive"})).await?;
    let reply = next_json(&mut socket).await?;
    assert_eq!(reply["MessageType"], "KeepAlive");
    Ok(())
}

#[tokio::test]
async fn emby_websocket_rejects_an_invalid_token_before_upgrading()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    assert_eq!(
        server
            .handshake_status("/embywebsocket", "not-a-token")
            .await?,
        401,
        "an invalid token must fail the handshake, never complete a 101"
    );
    assert_eq!(
        server.handshake_status("/emby/embywebsocket", "").await?,
        401,
        "a missing token must fail the handshake too"
    );
    Ok(())
}

#[tokio::test]
async fn emby_websocket_rejects_playback_tokens() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let status = server
        .handshake_status("/embywebsocket", "luxPlaybackSignature=deadbeef")
        .await?;
    assert_eq!(
        status, 401,
        "a playback-only token must not open the realtime channel"
    );
    Ok(())
}

#[tokio::test]
async fn emby_websocket_answers_on_every_path_shape() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    for path in [
        "/embywebsocket",
        "/emby/embywebsocket",
        "/EmbyWebSocket",
        "/EMBY/EmbyWebSocket",
    ] {
        let mut socket = server.connect(path, &server.admin_token).await?;
        send(&mut socket, json!({"MessageType": "KeepAlive"})).await?;
        let reply = next_json(&mut socket).await?;
        assert_eq!(reply["MessageType"], "KeepAlive", "`{path}` must be routed");
    }
    Ok(())
}

#[tokio::test]
async fn emby_websocket_stops_pushing_after_sessions_stop() -> Result<(), Box<dyn std::error::Error>>
{
    let server = spawn_server(limits()).await?;
    let mut socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    send(
        &mut socket,
        json!({"MessageType": "SessionsStart", "Data": "0,1000"}),
    )
    .await?;
    let first = next_json(&mut socket).await?;
    assert_eq!(first["MessageType"], "Sessions");
    send(&mut socket, json!({"MessageType": "SessionsStop"})).await?;
    expect_no_message(&mut socket, Duration::from_millis(1300)).await?;
    send(&mut socket, json!({"MessageType": "KeepAlive"})).await?;
    let reply = next_json(&mut socket).await?;
    assert_eq!(reply["MessageType"], "KeepAlive");
    Ok(())
}

#[tokio::test]
async fn emby_websocket_ignores_unknown_and_malformed_messages()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let mut socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    send(
        &mut socket,
        json!({"MessageType": "UserDataChanged", "Data": []}),
    )
    .await?;
    send_raw(&mut socket, "this is not json").await?;
    send_raw(&mut socket, "{}").await?;
    send(&mut socket, json!({"MessageType": "KeepAlive"})).await?;
    let reply = next_json(&mut socket).await?;
    assert_eq!(
        reply["MessageType"], "KeepAlive",
        "unknown and malformed messages must not close the connection"
    );
    Ok(())
}

#[tokio::test]
async fn emby_websocket_clamps_the_requested_poll_interval()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(EmbyWebsocketLimits {
        min_interval: Duration::from_millis(200),
        max_interval: Duration::from_millis(400),
        ..limits()
    })
    .await?;
    let mut socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;

    // A 1 ms poll is clamped up to the 200 ms floor.
    send(
        &mut socket,
        json!({"MessageType": "SessionsStart", "Data": "0,1"}),
    )
    .await?;
    let first = next_json(&mut socket).await?;
    assert_eq!(first["MessageType"], "Sessions");
    let started = Instant::now();
    let second = next_json(&mut socket).await?;
    let clamped_floor = started.elapsed();
    assert_eq!(second["MessageType"], "Sessions");
    assert!(
        clamped_floor >= Duration::from_millis(150),
        "a 1 ms interval must be clamped to the 200 ms floor, measured {clamped_floor:?}"
    );

    // A ~100 s poll is clamped down to the 400 ms ceiling.
    send(
        &mut socket,
        json!({"MessageType": "SessionsStart", "Data": "0,100000"}),
    )
    .await?;
    let started = Instant::now();
    let third = next_json(&mut socket).await?;
    let clamped_ceiling = started.elapsed();
    assert_eq!(third["MessageType"], "Sessions");
    assert!(
        clamped_ceiling <= Duration::from_millis(900),
        "a 100 s interval must be clamped to the 400 ms ceiling, measured {clamped_ceiling:?}"
    );
    Ok(())
}

#[tokio::test]
async fn emby_websocket_enforces_the_per_user_connection_limit()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(EmbyWebsocketLimits {
        max_connections: 3,
        max_connections_per_user: 1,
        ..limits()
    })
    .await?;
    let first = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    assert_eq!(
        server
            .handshake_status("/embywebsocket", &server.admin_token)
            .await?,
        503,
        "the second connection of one account must be refused before the upgrade"
    );
    // Another account still has its own slot, so the refusal is per user and not global.
    let mut viewer = server
        .connect("/embywebsocket", &server.viewer_token)
        .await?;
    send(&mut viewer, json!({"MessageType": "KeepAlive"})).await?;
    assert_eq!(next_json(&mut viewer).await?["MessageType"], "KeepAlive");
    drop(first);

    // Dropping a connection must release its slot; the server may need a moment to notice.
    let mut reopened = None;
    for _ in 0..40 {
        if server
            .handshake_status("/embywebsocket", &server.admin_token)
            .await?
            == 101
        {
            reopened = Some(());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        reopened.is_some(),
        "the connection slot must be released after the client disconnects"
    );
    Ok(())
}

#[tokio::test]
async fn emby_websocket_enforces_the_global_connection_limit()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(EmbyWebsocketLimits {
        max_connections: 1,
        max_connections_per_user: 4,
        ..limits()
    })
    .await?;
    let _first = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    assert_eq!(
        server
            .handshake_status("/embywebsocket", &server.viewer_token)
            .await?,
        503,
        "the global connection budget must refuse the handshake"
    );
    Ok(())
}

#[tokio::test]
async fn emby_websocket_closes_when_the_token_is_revoked() -> Result<(), Box<dyn std::error::Error>>
{
    let server = spawn_server(EmbyWebsocketLimits {
        min_interval: Duration::from_millis(200),
        ..limits()
    })
    .await?;
    // A second device token, so the connection under test can be revoked on its own.
    let token = emby_login(
        &server.client,
        &server.base_url,
        "Admin",
        "correct password",
        "ws-revoked",
    )
    .await?;
    let mut socket = server.connect("/embywebsocket", &token).await?;
    send(
        &mut socket,
        json!({"MessageType": "SessionsStart", "Data": "0,200"}),
    )
    .await?;
    assert_eq!(next_json(&mut socket).await?["MessageType"], "Sessions");

    let logout = server
        .client
        .post(format!("{}/Sessions/Logout", server.base_url))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(logout.status(), reqwest::StatusCode::NO_CONTENT);

    let code = next_close(&mut socket).await?;
    assert_eq!(
        code,
        Some(1008),
        "a revoked token must close the connection with a policy violation"
    );
    Ok(())
}

#[tokio::test]
async fn emby_websocket_closes_an_idle_connection() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(EmbyWebsocketLimits {
        idle_timeout: Duration::from_millis(300),
        heartbeat_interval: Duration::from_millis(100),
        ..limits()
    })
    .await?;
    // The client never reads, so it never answers the server pings: the heartbeat idle timeout
    // has to close the connection instead of keeping it forever.
    let mut socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let code = next_close(&mut socket).await?;
    assert_eq!(
        code,
        Some(1001),
        "an idle connection must be closed with 1001 (going away)"
    );
    Ok(())
}

#[tokio::test]
async fn emby_websocket_rejects_oversized_inbound_messages()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(EmbyWebsocketLimits {
        max_message_bytes: 1024,
        ..limits()
    })
    .await?;
    let mut socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    let oversized = "x".repeat(64 * 1024);
    // The write itself may fail once the server resets; either way the connection must end
    // without taking the process down.
    let _ = send_raw(&mut socket, &oversized).await;
    let code = next_close(&mut socket).await?;
    assert!(
        code.is_none() || code == Some(1009),
        "an oversized frame must end the connection, not be processed, saw {code:?}"
    );
    // The endpoint is still serving other connections.
    let mut healthy = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    send(&mut healthy, json!({"MessageType": "KeepAlive"})).await?;
    assert_eq!(next_json(&mut healthy).await?["MessageType"], "KeepAlive");
    Ok(())
}

#[tokio::test]
async fn emby_websocket_does_not_block_the_graceful_shutdown()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let mut socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    send(
        &mut socket,
        json!({"MessageType": "SessionsStart", "Data": "0,1000"}),
    )
    .await?;
    assert_eq!(next_json(&mut socket).await?["MessageType"], "Sessions");

    let state = server.state.clone();
    let elapsed = server.shutdown().await?;
    assert!(
        elapsed < Duration::from_secs(3),
        "an open websocket must not stall the graceful shutdown, took {elapsed:?}"
    );
    assert!(
        state
            .drain_emby_websocket_connections(Duration::from_secs(1))
            .await,
        "the open connection must release its slot during shutdown"
    );
    Ok(())
}
