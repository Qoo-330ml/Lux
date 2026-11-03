//! Administrator manual danmaku match API (ADR-052 P5).
//!
//! The fixtures install a stub `org.lux.danmaku` plugin that answers `danmaku.search`,
//! `danmaku.fetch` and `danmaku.match` with **different** XML bodies, so a written sidecar proves
//! which host path produced it: the manual match must always go through `danmaku.fetch`.

use std::{os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        plugin_protocol::{
            DANMAKU_FETCH_CAPABILITY, DANMAKU_MATCH_CAPABILITY, DANMAKU_SEARCH_CAPABILITY,
        },
        scanner::LibraryScanner,
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::{net::TcpListener, task::JoinHandle};

const PLUGIN_ID: &str = "org.lux.danmaku";
/// XML the stub returns for `danmaku.match`; the manual path must never produce it here.
const MATCH_XML: &[u8] = b"<i><d p=\"1,1,25,16777215,0,0,0,0\">matched</d></i>";
/// XML the stub returns for `danmaku.fetch`.
const FETCH_XML: &[u8] = b"<i><d p=\"2,1,25,16777215,0,0,0,0\">fetched</d></i>";
/// XML of a sidecar the fixture writes itself, standing in for a user-provided file.
const USER_XML: &[u8] = b"<i><d p=\"3,1,25,16777215,0,0,0,0\">user</d></i>";

/// Minimal `org.lux.danmaku` stub: one search candidate with two episodes, and a fetch answer that
/// differs from the match answer.
fn stub_script() -> String {
    let template = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"plugin.hello"'*)
      printf '{"id":"%s","result":{"id":"org.lux.danmaku","name":"danmaku stub","apiVersion":1,"capabilities":["danmaku.match","danmaku.search","danmaku.fetch"],"supportedItemTypes":[]}}\n' "$id"
      ;;
    *'"danmaku.match"'*)
      printf '{"id":"%s","result":{"status":"MATCHED","provider":"stub","episodeId":"ep-1","xmlBase64":"__MATCH_XML__"}}\n' "$id"
      ;;
    *'"danmaku.search"'*)
      case "$line" in
        *'"query":"huge"'*)
          printf '{"id":"%s","result":{"candidates":[]}}\n' "$id"
          ;;
        *)
          printf '{"id":"%s","result":{"candidates":[{"provider":"stub","animeId":"anime-1","animeTitle":"Demo Anime","episodes":[{"episodeId":"ep-1","title":"Episode 1"},{"episodeId":"ep-2","title":"Episode 2"}]}]}}\n' "$id"
          ;;
      esac
      ;;
    *'"danmaku.fetch"'*)
      printf '{"id":"%s","result":{"xmlBase64":"__FETCH_XML__"}}\n' "$id"
      ;;
    *'"plugin.shutdown"'*)
      printf '{"id":"%s","result":{"accepted":true}}\n' "$id"
      exit 0
      ;;
    *'"plugin.health"'*)
      printf '{"id":"%s","result":{"available":true,"configured":true}}\n' "$id"
      ;;
    *)
      printf '{"id":"%s","error":{"code":"PLUGIN_UNSUPPORTED_METHOD","message":"unsupported plugin method"}}\n' "$id"
      ;;
  esac
done
"#;
    template
        .replace("__MATCH_XML__", &BASE64.encode(MATCH_XML))
        .replace("__FETCH_XML__", &BASE64.encode(FETCH_XML))
}

fn manifest_value(capabilities: &[&str]) -> Value {
    json!({
        "formatVersion": 1,
        "id": PLUGIN_ID,
        "name": "弹幕匹配",
        "version": "1.0.0",
        "apiVersion": 1,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "danmaku",
        "category": "MEDIA",
        "capabilities": capabilities,
        "configFields": [
            {"key": "providerBaseUrl", "label": "Provider", "type": "text", "required": true, "sensitive": true},
            {"key": "libraryIds", "label": "Libraries", "type": "select", "multiple": true, "optionsSource": "media-libraries", "defaultValue": []},
            {"key": "matchOriginalFilename", "label": "Original", "type": "toggle", "defaultValue": true},
            {"key": "matchSimplifiedTraditionalTitles", "label": "CJK", "type": "toggle", "defaultValue": true},
            {"key": "matchEnglishTitle", "label": "English", "type": "toggle", "defaultValue": false},
            {"key": "concurrency", "label": "Concurrency", "type": "number", "defaultValue": 2, "minimum": 0, "maximum": 64},
            {"key": "overwrite", "label": "Overwrite", "type": "toggle", "defaultValue": false},
            {"key": "schedule", "label": "Schedule", "type": "text", "required": true, "defaultValue": "0 6 * * *"}
        ],
        "permissions": {"network": ["*"], "filesystem": []},
        "files": []
    })
}

/// A running server with one movie library, an installed stub plugin and an admin session.
struct Fixture {
    directory: tempfile::TempDir,
    database: Database,
    base_url: String,
    cookies: String,
    csrf: String,
    client: reqwest::Client,
    item_id: String,
    source_id: String,
    library_id: String,
    media_root: PathBuf,
    server: JoinHandle<std::io::Result<()>>,
}

impl Fixture {
    fn sidecar_path(&self) -> PathBuf {
        self.media_root.join("Demo.Movie.2024.xml")
    }

    fn search_url(&self) -> String {
        format!(
            "{}/api/v1/admin/items/{}/danmaku/search",
            self.base_url, self.item_id
        )
    }

    fn match_url(&self) -> String {
        format!(
            "{}/api/v1/admin/items/{}/danmaku/match",
            self.base_url, self.item_id
        )
    }

    fn delete_url(&self) -> String {
        format!(
            "{}/api/v1/admin/items/{}/danmaku",
            self.base_url, self.item_id
        )
    }

    async fn wait_for_job(&self, job_id: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let response = self
                .client
                .get(format!(
                    "{}/api/v1/admin/danmaku/match-jobs/{job_id}",
                    self.base_url
                ))
                .header(COOKIE, &self.cookies)
                .send()
                .await?;
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            let job = response.json::<Value>().await?["job"].clone();
            let status = job["status"].as_str().unwrap_or_default().to_owned();
            if !matches!(status.as_str(), "PENDING" | "RUNNING") {
                return Ok(job);
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "danmaku job {job_id} did not finish"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn track_row(
        &self,
    ) -> Result<Option<(String, Option<String>, Option<String>)>, sqlx::Error> {
        sqlx::query_as("SELECT status, provider, provider_episode_id FROM danmaku_tracks WHERE media_source_id = ?")
            .bind(&self.source_id)
            .fetch_optional(self.database.pool())
            .await
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// Builds the fixture; `rate_limit` overrides the per-session search budget.
async fn setup(
    capabilities: &[&str],
    rate_limit: Option<(u32, Duration)>,
) -> Result<Fixture, Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let config_dir = directory.path().join("config");
    let media_root = directory.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    tokio::fs::write(media_root.join("Demo.Movie.2024.mkv"), b"video").await?;

    let binaries = config_dir.join(format!("plugins/{PLUGIN_ID}/binaries"));
    tokio::fs::create_dir_all(&binaries).await?;
    tokio::fs::write(binaries.join("plugin"), stub_script()).await?;
    let mut permissions = tokio::fs::metadata(binaries.join("plugin"))
        .await?
        .permissions();
    permissions.set_mode(0o700);
    tokio::fs::set_permissions(binaries.join("plugin"), permissions).await?;
    tokio::fs::write(
        config_dir.join(format!("plugins/{PLUGIN_ID}/manifest.json")),
        serde_json::to_vec(&manifest_value(capabilities))?,
    )
    .await?;

    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let setup_service = SetupService::new(database.clone())?;
    setup_service
        .complete("Admin", "Admin", "correct password")
        .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    LibraryService::new(database.clone())
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-utf8 media root")?,
        )
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE title = 'Demo Movie'")
            .fetch_one(database.pool())
            .await?;
    let source_id: String = sqlx::query_scalar(
        "SELECT id FROM media_sources WHERE item_id = ? ORDER BY is_default DESC LIMIT 1",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let mut state = AppState::ready(config, database.clone(), setup_service, auth, emby_auth);
    if let Some((limit, window)) = rate_limit {
        state = state.with_danmaku_search_rate_limit(limit, window);
    }
    let app = app_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({"username": "admin", "password": "correct password"}))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session = cookie_value(login.headers(), "lux_session");
    let csrf = cookie_value(login.headers(), "lux_csrf");
    let cookies = format!("lux_session={session}; lux_csrf={csrf}");

    let installed = client
        .post(format!(
            "{base_url}/api/v1/admin/plugins/{PLUGIN_ID}/install"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(installed.status(), reqwest::StatusCode::CREATED);
    let configured = client
        .put(format!(
            "{base_url}/api/v1/admin/plugins/{PLUGIN_ID}/config"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "providerBaseUrl": "https://danmu.example.test/api",
            "libraryIds": [library.id.to_string()],
            "schedule": "0 6 * * *"
        }))
        .send()
        .await?;
    assert_eq!(configured.status(), reqwest::StatusCode::OK);

    Ok(Fixture {
        directory,
        database,
        base_url,
        cookies,
        csrf,
        client,
        item_id,
        source_id,
        library_id: library.id.to_string(),
        media_root,
        server,
    })
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
        .expect("expected cookie")
}

fn manual_match_body(episode_id: &str, overwrite: bool) -> Value {
    json!({
        "provider": "stub",
        "animeId": "anime-1",
        "episodeId": episode_id,
        "overwrite": overwrite
    })
}

#[tokio::test]
async fn manual_danmaku_endpoints_reject_non_administrators_and_missing_csrf()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = setup(
        &[
            DANMAKU_MATCH_CAPABILITY,
            DANMAKU_SEARCH_CAPABILITY,
            DANMAKU_FETCH_CAPABILITY,
        ],
        None,
    )
    .await?;
    let created = fixture
        .client
        .post(format!("{}/api/v1/admin/users", fixture.base_url))
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .json(&json!({
            "username": "regular",
            "displayName": "Regular",
            "password": "regular password"
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let login = fixture
        .client
        .post(format!("{}/api/v1/auth/login", fixture.base_url))
        .json(&json!({"username": "regular", "password": "regular password"}))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let regular_session = cookie_value(login.headers(), "lux_session");
    let regular_csrf = cookie_value(login.headers(), "lux_csrf");
    let regular_cookies = format!("lux_session={regular_session}; lux_csrf={regular_csrf}");

    let forbidden_search = fixture
        .client
        .get(fixture.search_url())
        .header(COOKIE, &regular_cookies)
        .send()
        .await?;
    assert_eq!(forbidden_search.status(), reqwest::StatusCode::FORBIDDEN);
    let forbidden_search_body = forbidden_search.json::<Value>().await?;
    assert_eq!(forbidden_search_body["error"]["code"], "PERMISSION_DENIED");
    let forbidden_match = fixture
        .client
        .post(fixture.match_url())
        .header(COOKIE, &regular_cookies)
        .header("x-csrf-token", &regular_csrf)
        .json(&manual_match_body("ep-1", false))
        .send()
        .await?;
    assert_eq!(forbidden_match.status(), reqwest::StatusCode::FORBIDDEN);
    let forbidden_delete = fixture
        .client
        .delete(fixture.delete_url())
        .header(COOKIE, &regular_cookies)
        .header("x-csrf-token", &regular_csrf)
        .send()
        .await?;
    assert_eq!(forbidden_delete.status(), reqwest::StatusCode::FORBIDDEN);

    // The administrator session still needs a CSRF token for the two mutating endpoints.
    let missing_csrf_match = fixture
        .client
        .post(fixture.match_url())
        .header(COOKIE, &fixture.cookies)
        .json(&manual_match_body("ep-1", false))
        .send()
        .await?;
    assert_eq!(missing_csrf_match.status(), reqwest::StatusCode::FORBIDDEN);
    let missing_csrf_match_body = missing_csrf_match.json::<Value>().await?;
    assert_eq!(missing_csrf_match_body["error"]["code"], "CSRF_FAILED");
    let missing_csrf_delete = fixture
        .client
        .delete(fixture.delete_url())
        .header(COOKIE, &fixture.cookies)
        .send()
        .await?;
    assert_eq!(missing_csrf_delete.status(), reqwest::StatusCode::FORBIDDEN);
    let job_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM danmaku_match_jobs")
        .fetch_one(fixture.database.pool())
        .await?;
    assert_eq!(job_count, 0, "rejected requests must not queue a job");
    Ok(())
}

#[tokio::test]
async fn manual_search_returns_candidates_and_enforces_the_documented_bounds()
-> Result<(), Box<dyn std::error::Error>> {
    // A generous per-session budget keeps the bounds assertions independent of request timing.
    let fixture = setup(
        &[
            DANMAKU_MATCH_CAPABILITY,
            DANMAKU_SEARCH_CAPABILITY,
            DANMAKU_FETCH_CAPABILITY,
        ],
        Some((100, Duration::from_secs(60))),
    )
    .await?;

    // No query: the endpoint searches with the item's own title.
    let response = fixture
        .client
        .get(fixture.search_url())
        .header(COOKIE, &fixture.cookies)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.json::<Value>().await?;
    assert_eq!(body["supported"], true);
    let items = body["items"].as_array().ok_or("missing search items")?;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["provider"], "stub");
    assert_eq!(items[0]["animeId"], "anime-1");
    assert_eq!(items[0]["animeTitle"], "Demo Anime");
    let episodes = items[0]["episodes"].as_array().ok_or("missing episodes")?;
    assert_eq!(episodes.len(), 2);
    assert_eq!(episodes[0]["episodeId"], "ep-1");
    assert_eq!(episodes[1]["title"], "Episode 2");

    let explicit_limit = fixture
        .client
        .get(fixture.search_url())
        .query(&[("query", "Demo"), ("limit", "50")])
        .header(COOKIE, &fixture.cookies)
        .send()
        .await?;
    assert_eq!(explicit_limit.status(), reqwest::StatusCode::OK);
    for (label, query) in [
        (
            "above the page bound",
            vec![("query", "Demo".to_owned()), ("limit", "51".to_owned())],
        ),
        (
            "below the page bound",
            vec![("query", "Demo".to_owned()), ("limit", "0".to_owned())],
        ),
        ("oversized query", vec![("query", "a".repeat(201))]),
    ] {
        let response = fixture
            .client
            .get(fixture.search_url())
            .query(&query)
            .header(COOKIE, &fixture.cookies)
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{label} must be rejected"
        );
        let body = response.json::<Value>().await?;
        assert_eq!(body["error"]["code"], "INVALID_REQUEST", "{label}");
    }

    let unknown_item = fixture
        .client
        .get(format!(
            "{}/api/v1/admin/items/{}/danmaku/search",
            fixture.base_url,
            uuid::Uuid::now_v7()
        ))
        .header(COOKIE, &fixture.cookies)
        .send()
        .await?;
    assert_eq!(unknown_item.status(), reqwest::StatusCode::NOT_FOUND);
    let invalid_item = fixture
        .client
        .get(format!(
            "{}/api/v1/admin/items/not-an-id/danmaku/search",
            fixture.base_url
        ))
        .header(COOKIE, &fixture.cookies)
        .send()
        .await?;
    assert_eq!(invalid_item.status(), reqwest::StatusCode::BAD_REQUEST);

    // A season container owns a source but is not a local video, so it has no sidecar to match.
    let season_item_id = uuid::Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title, identification_status, identity_key
         ) VALUES (?, ?, 'SEASON', 'Demo Season', 'Demo Season', 'LOCAL_CONFIRMED', 'manual-api-season')",
    )
    .bind(&season_item_id)
    .bind(&fixture.library_id)
    .execute(fixture.database.pool())
    .await?;
    let season_entry_id = uuid::Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO filesystem_entries (
            id, library_root_id, relative_path, entry_kind, size,
            modified_at, last_seen_generation, is_missing
         )
         SELECT ?, id, 'Demo.Season.mkv', 'FILE', 1, 1, 'manual-api-season', 0
         FROM library_roots WHERE library_id = ?",
    )
    .bind(&season_entry_id)
    .bind(&fixture.library_id)
    .execute(fixture.database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_sources (
            id, item_id, source_kind, filesystem_entry_id, container, size, is_default, probe_status
         ) VALUES (?, ?, 'LOCAL_FILE', ?, 'mkv', 1, 1, 'PENDING')",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(&season_item_id)
    .bind(&season_entry_id)
    .execute(fixture.database.pool())
    .await?;
    let season_search = fixture
        .client
        .get(format!(
            "{}/api/v1/admin/items/{season_item_id}/danmaku/search",
            fixture.base_url
        ))
        .header(COOKIE, &fixture.cookies)
        .send()
        .await?;
    assert_eq!(season_search.status(), reqwest::StatusCode::BAD_REQUEST);
    let season_body = season_search.json::<Value>().await?;
    assert_eq!(season_body["error"]["code"], "INVALID_REQUEST");
    Ok(())
}

#[tokio::test]
async fn manual_search_reports_an_unsupported_plugin_as_a_conflict()
-> Result<(), Box<dyn std::error::Error>> {
    // A plugin that only declares `danmaku.match` keeps automatic matching and hides manual search.
    let fixture = setup(&[DANMAKU_MATCH_CAPABILITY], None).await?;
    let response = fixture
        .client
        .get(fixture.search_url())
        .query(&[("query", "Demo")])
        .header(COOKIE, &fixture.cookies)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body = response.json::<Value>().await?;
    assert_eq!(body["error"]["code"], "PLUGIN_UNAVAILABLE");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("不支持手动匹配")
    );
    let create = fixture
        .client
        .post(fixture.match_url())
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .json(&manual_match_body("ep-1", false))
        .send()
        .await?;
    assert_eq!(create.status(), reqwest::StatusCode::CONFLICT);
    let job_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM danmaku_match_jobs")
        .fetch_one(fixture.database.pool())
        .await?;
    assert_eq!(job_count, 0);
    Ok(())
}

#[tokio::test]
async fn manual_search_is_rate_limited_per_administrator_session()
-> Result<(), Box<dyn std::error::Error>> {
    // The production window is one second; the fixture pins a long one so the rejection is not a
    // timing race.
    let fixture = setup(
        &[
            DANMAKU_MATCH_CAPABILITY,
            DANMAKU_SEARCH_CAPABILITY,
            DANMAKU_FETCH_CAPABILITY,
        ],
        Some((1, Duration::from_secs(60))),
    )
    .await?;
    let first = fixture
        .client
        .get(fixture.search_url())
        .header(COOKIE, &fixture.cookies)
        .send()
        .await?;
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let second = fixture
        .client
        .get(fixture.search_url())
        .header(COOKIE, &fixture.cookies)
        .send()
        .await?;
    assert_eq!(second.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        second
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok()),
        Some("1")
    );
    let body = second.json::<Value>().await?;
    assert_eq!(body["error"]["code"], "RATE_LIMITED");
    Ok(())
}

#[tokio::test]
async fn manual_match_queues_one_job_and_writes_the_fetched_episode()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = setup(
        &[
            DANMAKU_MATCH_CAPABILITY,
            DANMAKU_SEARCH_CAPABILITY,
            DANMAKU_FETCH_CAPABILITY,
        ],
        None,
    )
    .await?;
    let created = fixture
        .client
        .post(fixture.match_url())
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .json(&manual_match_body("ep-2", false))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::ACCEPTED);
    let job = created.json::<Value>().await?["job"].clone();
    assert_eq!(job["totalCount"], 1);
    assert_eq!(job["libraryId"], fixture.library_id);
    let job_id = job["id"].as_str().ok_or("missing job id")?.to_owned();
    let finished = fixture.wait_for_job(&job_id).await?;
    assert_eq!(finished["status"], "COMPLETED", "{finished}");
    assert_eq!(finished["successCount"], 1);
    assert_eq!(finished["failedCount"], 0);

    // The fetched XML, not the match XML, reached the sidecar. It also proves the worker passed the
    // selected episode to `danmaku.fetch`.
    assert_eq!(tokio::fs::read(fixture.sidecar_path()).await?, FETCH_XML);
    let (status, provider, episode_id) =
        fixture.track_row().await?.ok_or("missing danmaku track")?;
    assert_eq!(status, "READY");
    assert_eq!(provider.as_deref(), Some("stub"));
    assert_eq!(episode_id.as_deref(), Some("ep-2"));
    let (item_provider_anime, item_episode): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT provider_anime_id, provider_episode_id
             FROM danmaku_match_job_items WHERE job_id = ?",
    )
    .bind(&job_id)
    .fetch_one(fixture.database.pool())
    .await?;
    assert_eq!(item_provider_anime.as_deref(), Some("anime-1"));
    assert_eq!(item_episode.as_deref(), Some("ep-2"));

    // Retrying a manual job must not silently widen it into a library sweep.
    let retried = fixture
        .client
        .post(format!(
            "{}/api/v1/admin/danmaku/match-jobs/{job_id}/retry",
            fixture.base_url
        ))
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .send()
        .await?;
    assert_ne!(retried.status(), reqwest::StatusCode::ACCEPTED);
    Ok(())
}

#[tokio::test]
async fn manual_match_skips_an_existing_sidecar_unless_overwrite_is_set()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = setup(
        &[
            DANMAKU_MATCH_CAPABILITY,
            DANMAKU_SEARCH_CAPABILITY,
            DANMAKU_FETCH_CAPABILITY,
        ],
        None,
    )
    .await?;
    tokio::fs::write(fixture.sidecar_path(), USER_XML).await?;

    let created = fixture
        .client
        .post(fixture.match_url())
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .json(&manual_match_body("ep-1", false))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::ACCEPTED);
    let job_id = created.json::<Value>().await?["job"]["id"]
        .as_str()
        .ok_or("missing job id")?
        .to_owned();
    let finished = fixture.wait_for_job(&job_id).await?;
    assert_eq!(finished["status"], "COMPLETED", "{finished}");
    assert_eq!(finished["skippedCount"], 1);
    assert_eq!(finished["successCount"], 0);
    assert_eq!(
        tokio::fs::read(fixture.sidecar_path()).await?,
        USER_XML,
        "overwrite=false must keep the existing sidecar"
    );

    let overwritten = fixture
        .client
        .post(fixture.match_url())
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .json(&manual_match_body("ep-1", true))
        .send()
        .await?;
    assert_eq!(overwritten.status(), reqwest::StatusCode::ACCEPTED);
    let job_id = overwritten.json::<Value>().await?["job"]["id"]
        .as_str()
        .ok_or("missing job id")?
        .to_owned();
    let finished = fixture.wait_for_job(&job_id).await?;
    assert_eq!(finished["status"], "COMPLETED", "{finished}");
    assert_eq!(finished["successCount"], 1);
    assert_eq!(tokio::fs::read(fixture.sidecar_path()).await?, FETCH_XML);
    Ok(())
}

#[tokio::test]
async fn manual_match_conflicts_with_an_active_library_job()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = setup(
        &[
            DANMAKU_MATCH_CAPABILITY,
            DANMAKU_SEARCH_CAPABILITY,
            DANMAKU_FETCH_CAPABILITY,
        ],
        None,
    )
    .await?;
    sqlx::query(
        "INSERT INTO danmaku_match_jobs (id, library_id, status, concurrency, total_count)
         VALUES ('manual-api-active-job', ?, 'PENDING', 2, 0)",
    )
    .bind(&fixture.library_id)
    .execute(fixture.database.pool())
    .await?;
    let response = fixture
        .client
        .post(fixture.match_url())
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .json(&manual_match_body("ep-1", false))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body = response.json::<Value>().await?;
    assert_eq!(body["error"]["code"], "INVALID_REQUEST");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("已有弹幕匹配任务运行")
    );
    Ok(())
}

#[tokio::test]
async fn manual_match_requires_the_library_to_be_selected_in_the_plugin_config()
-> Result<(), Box<dyn std::error::Error>> {
    // docs/LUX-DEVELOPMENT.md §3.7: a library that is not selected cannot create danmaku match
    // jobs. Manual matching keeps that rule instead of queuing a job the worker would fail.
    let fixture = setup(
        &[
            DANMAKU_MATCH_CAPABILITY,
            DANMAKU_SEARCH_CAPABILITY,
            DANMAKU_FETCH_CAPABILITY,
        ],
        None,
    )
    .await?;
    let configured = fixture
        .client
        .put(format!(
            "{}/api/v1/admin/plugins/{PLUGIN_ID}/config",
            fixture.base_url
        ))
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .json(&json!({
            "providerBaseUrl": "https://danmu.example.test/api",
            "libraryIds": [],
            "schedule": "0 6 * * *"
        }))
        .send()
        .await?;
    assert_eq!(configured.status(), reqwest::StatusCode::OK);
    let response = fixture
        .client
        .post(fixture.match_url())
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .json(&manual_match_body("ep-1", false))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let job_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM danmaku_match_jobs")
        .fetch_one(fixture.database.pool())
        .await?;
    assert_eq!(job_count, 0);
    Ok(())
}

#[tokio::test]
async fn manual_delete_removes_lux_sidecars_and_protects_user_files()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = setup(
        &[
            DANMAKU_MATCH_CAPABILITY,
            DANMAKU_SEARCH_CAPABILITY,
            DANMAKU_FETCH_CAPABILITY,
        ],
        None,
    )
    .await?;

    // Nothing is registered for the item yet.
    let missing = fixture
        .client
        .delete(fixture.delete_url())
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
    let missing_body = missing.json::<Value>().await?;
    assert_eq!(missing_body["error"]["code"], "NOT_FOUND");

    // A sidecar the user put next to the media file has no provider and needs confirmation.
    tokio::fs::write(fixture.sidecar_path(), USER_XML).await?;
    sqlx::query(
        "INSERT INTO danmaku_tracks (id, media_source_id, relative_path, format, status)
         VALUES ('manual-api-user-track', ?, 'Demo.Movie.2024.xml', 'XML', 'READY')",
    )
    .bind(&fixture.source_id)
    .execute(fixture.database.pool())
    .await?;
    let unconfirmed = fixture
        .client
        .delete(fixture.delete_url())
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .send()
        .await?;
    assert_eq!(unconfirmed.status(), reqwest::StatusCode::CONFLICT);
    let unconfirmed_body = unconfirmed.json::<Value>().await?;
    assert_eq!(unconfirmed_body["error"]["code"], "INVALID_REQUEST");
    assert!(tokio::fs::try_exists(fixture.sidecar_path()).await?);
    let confirmed = fixture
        .client
        .delete(fixture.delete_url())
        .query(&[("confirmUserSidecar", "true")])
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .send()
        .await?;
    assert_eq!(confirmed.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(!tokio::fs::try_exists(fixture.sidecar_path()).await?);
    assert!(fixture.track_row().await?.is_none());

    // A sidecar Lux wrote is removed without a confirmation flag.
    let created = fixture
        .client
        .post(fixture.match_url())
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .json(&manual_match_body("ep-1", false))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::ACCEPTED);
    let job_id = created.json::<Value>().await?["job"]["id"]
        .as_str()
        .ok_or("missing job id")?
        .to_owned();
    assert_eq!(fixture.wait_for_job(&job_id).await?["status"], "COMPLETED");
    assert_eq!(tokio::fs::read(fixture.sidecar_path()).await?, FETCH_XML);
    let deleted = fixture
        .client
        .delete(fixture.delete_url())
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .send()
        .await?;
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(!tokio::fs::try_exists(fixture.sidecar_path()).await?);
    assert!(fixture.track_row().await?.is_none());
    Ok(())
}

#[tokio::test]
async fn manual_delete_refuses_a_sidecar_path_outside_the_media_root()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = setup(
        &[
            DANMAKU_MATCH_CAPABILITY,
            DANMAKU_SEARCH_CAPABILITY,
            DANMAKU_FETCH_CAPABILITY,
        ],
        None,
    )
    .await?;
    let outside = fixture.directory.path().join("outside.xml");
    tokio::fs::write(&outside, USER_XML).await?;
    sqlx::query(
        "INSERT INTO danmaku_tracks (id, media_source_id, relative_path, format, provider, status)
         VALUES ('manual-api-escape-track', ?, '../outside.xml', 'XML', 'stub', 'READY')",
    )
    .bind(&fixture.source_id)
    .execute(fixture.database.pool())
    .await?;
    // The stored source path escapes the media root, so canonicalisation must refuse the delete.
    sqlx::query(
        "UPDATE filesystem_entries SET relative_path = '../outside.mkv'
         WHERE id = (SELECT filesystem_entry_id FROM media_sources WHERE id = ?)",
    )
    .bind(&fixture.source_id)
    .execute(fixture.database.pool())
    .await?;
    let response = fixture
        .client
        .delete(fixture.delete_url())
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let body = response.json::<Value>().await?;
    assert_eq!(body["error"]["code"], "INVALID_REQUEST");
    assert!(tokio::fs::try_exists(&outside).await?);
    assert!(fixture.track_row().await?.is_some());
    Ok(())
}
