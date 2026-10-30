use std::time::Duration;

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, HeaderMap, SET_COOKIE};
use serde_json::{Value, json};
use tokio::{net::TcpListener, task::JoinHandle};

struct TestApp {
    base: String,
    client: reqwest::Client,
    server: JoinHandle<Result<(), std::io::Error>>,
    item_id: String,
    source_id: String,
    /// Kept alive so the scanned media root outlives the server.
    _temp: tempfile::TempDir,
}

impl TestApp {
    fn abort(&self) {
        self.server.abort();
    }
}

/// One `/api/v1/events` stream with its own frame buffer, so a chunk that
/// carries several frames is not lost between assertions.
struct EventStream {
    response: reqwest::Response,
    buffer: String,
}

impl EventStream {
    async fn open(
        client: &reqwest::Client,
        base: &str,
        cookies: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut response = client
            .get(format!("{base}/api/v1/events"))
            .header(COOKIE, cookies)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let ready = response.chunk().await?.ok_or("missing ready frame")?;
        let ready = String::from_utf8(ready.to_vec())?;
        assert!(ready.contains("event: ready"));
        Ok(Self {
            response,
            buffer: ready,
        })
    }

    /// Returns the data object of the next `invalidate` frame.
    async fn next_invalidation(&mut self) -> Result<Value, Box<dyn std::error::Error>> {
        loop {
            if let Some(index) = self.buffer.find("\n\n") {
                let frame = self.buffer[..index].to_owned();
                self.buffer.drain(..index + 2);
                if frame.contains("event: invalidate") {
                    let data = frame
                        .lines()
                        .find_map(|line| line.strip_prefix("data: "))
                        .ok_or("missing data")?;
                    return Ok(serde_json::from_str(data)?);
                }
                continue;
            }
            let chunk = tokio::time::timeout(Duration::from_secs(2), self.response.chunk())
                .await??
                .ok_or("event stream closed")?;
            self.buffer.push_str(std::str::from_utf8(&chunk)?);
        }
    }

    async fn next_invalidation_scope(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        let data = self.next_invalidation().await?;
        Ok(data["scope"].as_str().ok_or("scope")?.to_owned())
    }

    async fn expect_silence(&mut self, message: &str) {
        if let Ok(Ok(Some(chunk))) =
            tokio::time::timeout(Duration::from_millis(750), self.response.chunk()).await
        {
            let received = String::from_utf8_lossy(&chunk).to_string();
            panic!("{message}; received: {received}");
        }
    }
}

fn cookie_value(headers: &HeaderMap, name: &str) -> String {
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

/// Boots one SQLite-backed server with an admin, a `viewer` account and one
/// scanned movie so both accounts can open an SSE stream and the admin can act.
async fn spawn_app() -> Result<TestApp, Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:0".parse()?,
        config_dir: temp.path().join("config"),
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
    let root = temp.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Scoped.Event.2024.mp4"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    Ok(TestApp {
        base: format!("http://{address}"),
        client: reqwest::Client::builder().no_proxy().build()?,
        server,
        item_id,
        source_id,
        _temp: temp,
    })
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

#[tokio::test]
async fn user_level_events_stay_private_while_admin_events_reach_everyone()
-> Result<(), Box<dyn std::error::Error>> {
    let app = spawn_app().await?;
    let base = app.base.as_str();
    let client = &app.client;
    let item_id = app.item_id.as_str();

    let (admin_cookies, admin_csrf) = login(client, base, "admin", "correct password").await?;
    let (viewer_cookies, _viewer_csrf) = login(client, base, "viewer", "viewer password").await?;
    let mut admin_events = EventStream::open(client, base, &admin_cookies).await?;
    let mut viewer_events = EventStream::open(client, base, &viewer_cookies).await?;

    // Favorites are per-user state: only the acting user's connections react.
    let favorite = client
        .put(format!("{base}/api/v1/items/{item_id}/favorite"))
        .header(COOKIE, &admin_cookies)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({ "favorite": true }))
        .send()
        .await?;
    assert_eq!(favorite.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(admin_events.next_invalidation_scope().await?, "favorites");
    viewer_events
        .expect_silence("another user's favorites event must not reach the viewer's stream")
        .await;

    // Playback progress is per-user as well.
    let progress = client
        .post(format!("{base}/api/v1/items/{item_id}/progress"))
        .header(COOKIE, &admin_cookies)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({
            "state": "PLAYING",
            "positionTicks": 1_000_000_000_i64,
            "durationTicks": 2_000_000_000_i64
        }))
        .send()
        .await?;
    assert_eq!(progress.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(admin_events.next_invalidation_scope().await?, "home");
    viewer_events
        .expect_silence("another user's progress event must not reach the viewer's stream")
        .await;

    // Directory-level administrator changes are still broadcast.
    let settings = client
        .patch(format!("{base}/api/v1/admin/settings"))
        .header(COOKIE, &admin_cookies)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({ "serverName": "Scoped Event Test" }))
        .send()
        .await?;
    assert_eq!(settings.status(), reqwest::StatusCode::OK);
    assert_eq!(viewer_events.next_invalidation_scope().await?, "home");
    app.abort();
    Ok(())
}

#[tokio::test]
async fn web_playback_progress_is_private_and_played_flips_notify_the_whole_user()
-> Result<(), Box<dyn std::error::Error>> {
    let app = spawn_app().await?;
    let base = app.base.as_str();
    let client = &app.client;
    let item_id = app.item_id.as_str();

    let (admin_cookies, admin_csrf) = login(client, base, "admin", "correct password").await?;
    let (viewer_cookies, _viewer_csrf) = login(client, base, "viewer", "viewer password").await?;
    let mut admin_events = EventStream::open(client, base, &admin_cookies).await?;
    let mut viewer_events = EventStream::open(client, base, &viewer_cookies).await?;

    let session = client
        .post(format!("{base}/api/v1/playback/sessions"))
        .header(COOKIE, &admin_cookies)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({
            "itemId": item_id,
            "sourceId": app.source_id.as_str(),
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
    let session_id = session["sessionId"].as_str().ok_or("sessionId")?.to_owned();

    // A position at the end of the media reports "played", so this tick produces
    // both the narrow progress event and the played-flip item event.
    let reported = client
        .post(format!(
            "{base}/api/v1/playback/sessions/{session_id}/events"
        ))
        .header(COOKIE, &admin_cookies)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({
            "eventId": "scoped-progress-1",
            "sequence": 1,
            "state": "PLAYING",
            "positionTicks": 2_000_000_000_i64,
            "durationTicks": 2_000_000_000_i64
        }))
        .send()
        .await?;
    assert_eq!(reported.status(), reqwest::StatusCode::OK);

    let progress = admin_events.next_invalidation().await?;
    assert_eq!(progress["scope"].as_str(), Some("progress"));
    assert_eq!(progress["itemId"].as_str(), Some(item_id));
    let played_flip = admin_events.next_invalidation().await?;
    assert_eq!(
        played_flip["scope"].as_str(),
        Some("item"),
        "a played flip also invalidates the user's played markers"
    );

    viewer_events
        .expect_silence("another user's web playback event must not reach the viewer's stream")
        .await;
    app.abort();
    Ok(())
}
