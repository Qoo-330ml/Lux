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
    auth::emby::EmbyDeviceInfo,
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
    tungstenite::{Error as WsError, Message, client::IntoClientRequest},
};

type ClientSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct TestServer {
    base_url: String,
    ws_url: String,
    address: std::net::SocketAddr,
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
        address,
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
        self.start_playback_sessions(1).await
    }

    /// Records `count` distinct playback sessions, which makes one `Sessions` document big enough
    /// to overflow a deliberately small write buffer.
    async fn start_playback_sessions(
        &self,
        count: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
            .fetch_one(self.database.pool())
            .await?;
        let source_id: Option<String> =
            sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
                .bind(&item_id)
                .fetch_optional(self.database.pool())
                .await?;
        for index in 0..count {
            let mut body = json!({
                "ItemId": uuid_to_emby_id(&item_id),
                "PlaySessionId": format!("websocket-contract-session-{index}"),
                "PositionTicks": 120_000_000_i64,
                "RunTimeTicks": 600_000_000_i64,
                "CanSeek": true,
                "DeviceName": format!("Device {index}"),
            });
            if let Some(source_id) = source_id.as_deref() {
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
        }
        Ok(())
    }

    /// Issues a real playback ticket (`luxpb_{item}_{random}`) through the same service the
    /// playback routes use, so the rejection test cannot pass on the "unknown token" path.
    async fn issue_playback_ticket(&self) -> Result<(String, String), Box<dyn std::error::Error>> {
        let auth = EmbyAuthService::new(self.database.clone())?;
        let device = EmbyDeviceInfo {
            client: "WebSocketTest".to_owned(),
            device: "Mac".to_owned(),
            device_id: "ws-playback-ticket".to_owned(),
            version: "1".to_owned(),
            user_id: None,
        };
        let login = auth
            .authenticate("Admin", "correct password", &device)
            .await?
            .ok_or("login failed while issuing a playback ticket")?;
        let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
            .fetch_one(self.database.pool())
            .await?;
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or_default()
            + 600;
        let item_public_id = uuid_to_emby_id(&item_id);
        let ticket = auth
            .issue_playback_token(
                &login.user,
                &item_public_id,
                "websocket-playback-ticket",
                &device,
                expires_at,
                None,
            )
            .await?
            .ok_or_else(|| "playback ticket was not issued".to_owned())?;
        Ok((ticket, item_public_id))
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

    /// Opens the WebSocket over a TCP socket whose receive buffer is tiny, so a few pushes fill
    /// the peer window and the server's writes really block (the only way to exercise the send
    /// timeout instead of the message size check).
    async fn connect_with_small_receive_buffer(
        &self,
        token: &str,
    ) -> Result<ClientSocket, Box<dyn std::error::Error>> {
        let url = format!(
            "{}/embywebsocket?api_key={token}&deviceId=ws-test-device",
            self.ws_url
        );
        let socket = tokio::net::TcpSocket::new_v4()?;
        socket.set_recv_buffer_size(1024)?;
        let stream = socket.connect(self.address).await?;
        let request = url.as_str().into_client_request()?;
        let (socket, response) =
            tokio_tungstenite::client_async(request, MaybeTlsStream::Plain(stream)).await?;
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
async fn next_close<S>(stream: &mut S) -> Result<Option<u16>, Box<dyn std::error::Error>>
where
    S: futures_util::Stream<Item = Result<Message, WsError>> + Unpin,
{
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
async fn emby_websocket_rejects_playback_tickets() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    // A real ticket as the playback routes issue them (`luxpb_{item}_{random}`); a made-up
    // `luxPlaybackSignature=...` string would only exercise the "unknown token" path.
    let (ticket, item_id) = server.issue_playback_ticket().await?;
    assert!(ticket.starts_with("luxpb_"), "unexpected ticket shape");
    // The ticket is real and route scoped: it authenticates the item it was issued for, which is
    // on the playback allow-list, and only there.
    let allowed = server
        .client
        .get(format!(
            "{}/emby/Items/{}?api_key={}",
            server.base_url, item_id, ticket
        ))
        .send()
        .await?;
    assert_eq!(
        allowed.status(),
        reqwest::StatusCode::OK,
        "the issued ticket must authenticate on the playback allow-list"
    );
    assert_eq!(
        server.handshake_status("/embywebsocket", &ticket).await?,
        401,
        "a playback-only ticket must not open the realtime channel"
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

    // A ~100 s poll is clamped down to the 400 ms ceiling. The gap is measured between two
    // *consecutive pushes*: the schedule reset only decides when the first of them is allowed, so
    // measuring from the Start frame would prove nothing.
    send(
        &mut socket,
        json!({"MessageType": "SessionsStart", "Data": "0,100000"}),
    )
    .await?;
    let after_reset = next_json(&mut socket).await?;
    assert_eq!(after_reset["MessageType"], "Sessions");
    let started = Instant::now();
    let following = next_json(&mut socket).await?;
    let clamped_ceiling = started.elapsed();
    assert_eq!(following["MessageType"], "Sessions");
    assert!(
        clamped_ceiling >= Duration::from_millis(300),
        "the ceiling must delay the following push, measured {clamped_ceiling:?}"
    );
    assert!(
        clamped_ceiling <= Duration::from_millis(900),
        "a 100 s interval must be clamped to the 400 ms ceiling, measured {clamped_ceiling:?}"
    );
    Ok(())
}

/// Reads every text frame until `window` elapses, remembering when each one arrived.
///
/// Reading must happen while the client sends: a frame that sat in the socket buffer would be
/// timestamped at read time, which would make any interval measurement meaningless.
async fn collect_messages<S>(
    stream: &mut S,
    window: Duration,
) -> Result<Vec<(Value, Instant)>, Box<dyn std::error::Error>>
where
    S: futures_util::Stream<Item = Result<Message, WsError>> + Unpin,
{
    let deadline = tokio::time::Instant::now() + window;
    let mut received = Vec::new();
    loop {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Err(_) => return Ok(received),
            Ok(Some(Ok(Message::Text(text)))) => {
                let received_at = Instant::now();
                received.push((serde_json::from_str(text.as_ref())?, received_at));
            }
            Ok(Some(Ok(
                Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_),
            ))) => {}
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) => return Ok(received),
            Ok(Some(Err(error))) => return Err(format!("websocket error: {error}").into()),
        }
    }
}

#[tokio::test]
async fn emby_websocket_rate_limits_repeated_start_frames() -> Result<(), Box<dyn std::error::Error>>
{
    let server = spawn_server(EmbyWebsocketLimits {
        min_interval: Duration::from_millis(200),
        max_interval: Duration::from_millis(400),
        ..limits()
    })
    .await?;
    let socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    let (mut writer, mut reader) = socket.split();
    // Ten `SessionsStart` frames, each asking to push immediately and then every second. Every one
    // of them used to reset the plan to "now", which let a tiny frame trigger a full rebuild. The
    // client keeps reading while it sends, so the measured gaps are arrival times.
    let producer = tokio::spawn(async move {
        for _ in 0..10 {
            writer
                .send(Message::Text(
                    json!({"MessageType": "SessionsStart", "Data": "0,1000"})
                        .to_string()
                        .into(),
                ))
                .await?;
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        // The two streams are limited independently: a flood of session pushes must not delay the
        // first scheduled-task push, which has no earlier attempt of its own.
        writer
            .send(Message::Text(
                json!({"MessageType": "ScheduledTasksInfoStart", "Data": "0,1000"})
                    .to_string()
                    .into(),
            ))
            .await?;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        Ok::<(), WsError>(())
    });
    let received = collect_messages(&mut reader, Duration::from_millis(1200)).await?;
    producer.await??;
    let pushes = received
        .iter()
        .filter(|(message, _)| message["MessageType"] == "Sessions")
        .collect::<Vec<_>>();
    assert!(
        pushes.len() >= 2,
        "the stream has to keep pushing, received {} pushes",
        pushes.len()
    );
    for pair in pushes.windows(2) {
        let gap = pair[1].1.duration_since(pair[0].1);
        assert!(
            gap >= Duration::from_millis(150),
            "two pushes must stay at least min_interval (200 ms) apart, measured {gap:?}"
        );
    }
    assert!(
        pushes.len() <= 8,
        "1.2 s of a 200 ms floor must not yield more than 8 pushes, received {}",
        pushes.len()
    );

    assert!(
        received
            .iter()
            .any(|(message, _)| message["MessageType"] == "ScheduledTasksInfo"),
        "the scheduled-task stream must keep its own schedule"
    );
    Ok(())
}

#[tokio::test]
async fn emby_websocket_pushes_documents_larger_than_the_initial_write_buffer()
-> Result<(), Box<dyn std::error::Error>> {
    // The initial write buffer is only a capacity hint, not a document limit: a `Sessions`
    // document grows with the number of listed sessions (the store lists up to 500), so it is far
    // larger than the buffer. This uses the production limits on purpose: 100 sessions are about
    // 130 KiB, i.e. one fifth of the documented worst case (~750 KiB), and must fit under the
    // production cap with the same ratio.
    let server = spawn_server(EmbyWebsocketLimits::default()).await?;
    server.start_playback_sessions(100).await?;
    let mut socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    send(
        &mut socket,
        json!({"MessageType": "SessionsStart", "Data": "0,1500"}),
    )
    .await?;
    let message = next_json(&mut socket).await?;
    assert_eq!(message["MessageType"], "Sessions");
    let sessions = message["Data"]
        .as_array()
        .ok_or("Sessions Data must be an array")?;
    assert_eq!(
        sessions.len(),
        100,
        "a document larger than the initial write buffer must be delivered in full"
    );
    // The connection is still usable afterwards, i.e. the oversized document did not close it.
    send(&mut socket, json!({"MessageType": "KeepAlive"})).await?;
    assert_eq!(next_json(&mut socket).await?["MessageType"], "KeepAlive");
    Ok(())
}

#[tokio::test]
async fn emby_websocket_skips_a_document_beyond_the_write_buffer_cap()
-> Result<(), Box<dyn std::error::Error>> {
    // A cap below one document (~13 KiB with 10 sessions) must not disconnect: the push is
    // skipped with a rate-limited warning and the connection stays usable.
    let server = spawn_server(EmbyWebsocketLimits {
        write_buffer_bytes: 1024,
        max_write_buffer_bytes: 4096,
        min_interval: Duration::from_millis(100),
        max_interval: Duration::from_millis(200),
        ..limits()
    })
    .await?;
    server.start_playback_sessions(10).await?;
    let mut socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    send(
        &mut socket,
        json!({"MessageType": "SessionsStart", "Data": "0,100"}),
    )
    .await?;
    // Nothing may be pushed: a partial or truncated document would be worse than no document.
    expect_no_message(&mut socket, Duration::from_millis(400)).await?;
    send(&mut socket, json!({"MessageType": "KeepAlive"})).await?;
    assert_eq!(
        next_json(&mut socket).await?["MessageType"],
        "KeepAlive",
        "an oversized document must not close the connection"
    );
    Ok(())
}

#[tokio::test]
async fn emby_websocket_closes_a_stalled_reader_after_the_send_timeout()
-> Result<(), Box<dyn std::error::Error>> {
    // The document (~13 KiB with 10 sessions) is far below the 4 MiB cap, so the size check is
    // not what closes this connection: the peer's receive buffer is 1 KiB and it never reads, so
    // the writes block and only the send timeout can end the connection.
    let server = spawn_server(EmbyWebsocketLimits {
        min_interval: Duration::from_millis(20),
        max_interval: Duration::from_millis(50),
        write_buffer_bytes: 16 * 1024,
        max_write_buffer_bytes: 4 * 1024 * 1024,
        send_timeout: Duration::from_millis(300),
        idle_timeout: Duration::from_secs(30),
        heartbeat_interval: Duration::from_secs(10),
        max_connections: 1,
        max_connections_per_user: 1,
        ..limits()
    })
    .await?;
    server.start_playback_sessions(10).await?;
    let mut socket = server
        .connect_with_small_receive_buffer(&server.admin_token)
        .await?;
    send(
        &mut socket,
        json!({"MessageType": "SessionsStart", "Data": "0,20"}),
    )
    .await?;
    // The document is far below the cap, so a size failure is impossible: the connection has to
    // survive the first pushes and only the stalled write may end it. A hundred milliseconds in,
    // before the 300 ms send timeout can have fired, the slot must still be taken.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        server
            .handshake_status("/embywebsocket", &server.admin_token)
            .await?,
        503,
        "the stalled connection must still be open before send_timeout elapses"
    );
    // From here on the client never reads; the server must give up and release the slot.
    let mut released = false;
    for _ in 0..80 {
        if server
            .handshake_status("/embywebsocket", &server.admin_token)
            .await?
            == 101
        {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        released,
        "a send that cannot complete within send_timeout must close the connection"
    );
    let _ = next_close(&mut socket).await;
    Ok(())
}

#[tokio::test]
async fn emby_websocket_keeps_timers_running_under_an_inbound_flood()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(EmbyWebsocketLimits {
        min_interval: Duration::from_millis(200),
        max_interval: Duration::from_millis(400),
        revalidate_interval: Duration::from_millis(200),
        ..limits()
    })
    .await?;
    let token = emby_login(
        &server.client,
        &server.base_url,
        "Admin",
        "correct password",
        "ws-flooded",
    )
    .await?;
    let socket = server.connect("/embywebsocket", &token).await?;
    let (mut writer, mut reader) = socket.split();
    // A client that never stops sending: with a biased select the timer branch would never run
    // again, starving pushes, heartbeats and the periodic revalidation.
    let flood = tokio::spawn(async move {
        writer
            .send(Message::Text(
                json!({"MessageType": "SessionsStart", "Data": "0,1000"})
                    .to_string()
                    .into(),
            ))
            .await?;
        for _ in 0..200 {
            writer
                .send(Message::Text(
                    json!({"MessageType": "KeepAlive"}).to_string().into(),
                ))
                .await?;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok::<(), WsError>(())
    });
    // The scheduled push must still arrive while the flood is running. The window covers three
    // intervals (the clamp is 400 ms here) plus a generous margin, so a slow scheduler does not
    // turn a healthy connection into a failure.
    let received = collect_messages(&mut reader, Duration::from_millis(1800)).await?;
    let pushes = received
        .iter()
        .filter(|(message, _)| message["MessageType"] == "Sessions")
        .collect::<Vec<_>>();
    assert!(
        pushes.len() >= 3,
        "pushes must keep running while the client floods inbound frames, got {}",
        pushes.len()
    );
    // Revoking the token must still close the connection: the periodic revalidation is a timer
    // and must not be starved either.
    let logout = server
        .client
        .post(format!("{}/Sessions/Logout", server.base_url))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(logout.status(), reqwest::StatusCode::NO_CONTENT);
    let code = next_close(&mut reader).await?;
    assert_eq!(
        code,
        Some(1008),
        "a revoked token must close a flooded connection"
    );
    let _ = flood.await;
    Ok(())
}

#[tokio::test]
async fn emby_websocket_accepts_extreme_limit_overrides() -> Result<(), Box<dyn std::error::Error>>
{
    // The limits are public API. `usize::MAX` connections would panic in `Semaphore::new`,
    // `Duration::MAX` would panic in `Instant::now() + duration` inside the connection task, and
    // an unclamped initial write buffer would panic in tungstenite's `assert_valid`. All of them
    // must be normalised into a working connection instead.
    let server = spawn_server(EmbyWebsocketLimits {
        max_connections: usize::MAX,
        max_connections_per_user: usize::MAX,
        min_interval: Duration::MAX,
        max_interval: Duration::MAX,
        idle_timeout: Duration::MAX,
        heartbeat_interval: Duration::MAX,
        send_timeout: Duration::MAX,
        revalidate_interval: Duration::MAX,
        auth_warn_interval: Duration::MAX,
        write_buffer_bytes: usize::MAX,
        max_write_buffer_bytes: usize::MAX,
        ..limits()
    })
    .await?;
    let mut socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    send(&mut socket, json!({"MessageType": "KeepAlive"})).await?;
    assert_eq!(
        next_json(&mut socket).await?["MessageType"],
        "KeepAlive",
        "extreme overrides must yield a working connection"
    );
    Ok(())
}

#[tokio::test]
async fn emby_websocket_accepts_equal_write_buffer_limits() -> Result<(), Box<dyn std::error::Error>>
{
    // tungstenite asserts `max_write_buffer_size > write_buffer_size` after the upgrade; an equal
    // pair must be normalised instead of panicking inside the connection task.
    let server = spawn_server(EmbyWebsocketLimits {
        write_buffer_bytes: 1024,
        max_write_buffer_bytes: 1024,
        ..limits()
    })
    .await?;
    let mut socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    send(&mut socket, json!({"MessageType": "KeepAlive"})).await?;
    assert_eq!(
        next_json(&mut socket).await?["MessageType"],
        "KeepAlive",
        "equal write-buffer limits must not kill the connection"
    );
    Ok(())
}

#[tokio::test]
async fn emby_websocket_revalidates_connections_without_a_push_plan()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(EmbyWebsocketLimits {
        revalidate_interval: Duration::from_millis(200),
        ..limits()
    })
    .await?;
    let token = emby_login(
        &server.client,
        &server.base_url,
        "Admin",
        "correct password",
        "ws-idle-revalidate",
    )
    .await?;
    let mut socket = server.connect("/embywebsocket", &token).await?;
    // Deliberately no SessionsStart/ScheduledTasksInfoStart: nothing in the push loop would
    // revalidate the token, so `SessionsStop` used to keep a revoked token alive forever.
    send(&mut socket, json!({"MessageType": "SessionsStop"})).await?;
    send(&mut socket, json!({"MessageType": "KeepAlive"})).await?;
    assert_eq!(next_json(&mut socket).await?["MessageType"], "KeepAlive");

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
        "a revoked token must close a connection without a push plan too"
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
        max_connections: 1,
        max_connections_per_user: 1,
        ..limits()
    })
    .await?;
    // The client never reads, so it never answers the server pings: the heartbeat idle timeout
    // has to close the connection instead of keeping it forever.
    let mut socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    // Under parallel test load the peer can observe a transport-level end instead of the close
    // frame (the server drops the socket right after sending it), so the hard guarantee is that
    // the connection ends and its slot is released; when a close frame does arrive it must be the
    // "going away" code.
    if let Some(code) = next_close(&mut socket).await? {
        assert_eq!(
            code, 1001,
            "an idle connection closes with 1001 (going away)"
        );
    }
    let mut released = false;
    for _ in 0..30 {
        if server
            .handshake_status("/embywebsocket", &server.admin_token)
            .await?
            == 101
        {
            released = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        released,
        "an idle connection must be closed and its slot released"
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

#[tokio::test]
async fn emby_websocket_refuses_new_handshakes_once_shutdown_started()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server(limits()).await?;
    let mut socket = server
        .connect("/embywebsocket", &server.admin_token)
        .await?;
    send(&mut socket, json!({"MessageType": "KeepAlive"})).await?;
    assert_eq!(next_json(&mut socket).await?["MessageType"], "KeepAlive");

    // `src/main.rs` signals the shared shutdown before the graceful drain; both realtime endpoints
    // must stop accepting new handshakes from that moment (ADR-054 D10).
    server.state.signal_websocket_shutdown();
    assert_eq!(
        server
            .handshake_status("/embywebsocket", &server.viewer_token)
            .await?,
        503,
        "a shutting-down process must not complete a new handshake"
    );
    // The connection that was already open is still told to go away.
    assert_eq!(next_close(&mut socket).await?, Some(1001));
    Ok(())
}
