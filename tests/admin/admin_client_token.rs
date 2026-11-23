use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{
        emby::EmbyAuthService,
        sessions::WebAuthService,
        users::{UserStore, UserUpdate},
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::net::TcpListener;

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn emby_login(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            "Authorization",
            r#"Emby Client="LuxNative", Device="iPhone", DeviceId="lux-native-1", Version="1.0""#,
        )
        .json(&json!({ "Username": username, "Pw": password }))
        .send()
        .await?;
    assert_eq!(login.status(), StatusCode::OK);
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing AccessToken")?
        .to_owned();
    Ok(token)
}

#[tokio::test]
async fn admin_user_client_tokens_authenticate_phase1_admin_endpoints_and_audit()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup
        .complete("Admin", "Administrator", "admin password")
        .await?;
    let users = UserStore::new(database.clone())?;
    users
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;

    let libraries = LibraryService::new(database.clone());
    let enabled_library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Example Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example Movie (2024).mkv"), b"video").await?;
    libraries
        .add_root(enabled_library.id, media_root.to_str().ok_or("utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(enabled_library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;

    let disabled_library = libraries
        .create_library("Disabled Library", LibraryKind::Movie, false)
        .await?;
    sqlx::query("UPDATE libraries SET is_enabled = 0 WHERE id = ?")
        .bind(disabled_library.id.to_string())
        .execute(database.pool())
        .await?;

    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let _server = AbortOnDrop(tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    }));
    let client = reqwest::Client::builder().no_proxy().build()?;

    let admin_token = emby_login(&client, &base_url, "admin", "admin password").await?;
    let viewer_token = emby_login(&client, &base_url, "viewer", "viewer password").await?;

    // 1. /api/v1/auth/me returns full user permission flags.
    let me = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(me.status(), StatusCode::OK);
    let me_body: Value = me.json().await?;
    assert_eq!(me_body["user"]["isAdmin"], true);
    assert_eq!(me_body["user"]["canManageServer"], true);
    assert_eq!(me_body["user"]["canDownload"], false);
    assert_eq!(me_body["user"]["isDisabled"], false);

    // 2. Unauthenticated & non-admin client tokens are rejected properly.
    let unauth_scan = client
        .post(format!("{base_url}/api/v1/admin/libraries/scan"))
        .send()
        .await?;
    assert_eq!(unauth_scan.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauth_scan.json::<Value>().await?["error"]["code"],
        "AUTHENTICATION_REQUIRED"
    );

    for (method, path) in [
        (reqwest::Method::GET, "/api/v1/admin/libraries".to_owned()),
        (
            reqwest::Method::POST,
            "/api/v1/admin/libraries/scan".to_owned(),
        ),
        (reqwest::Method::GET, "/api/v1/admin/dashboard".to_owned()),
        (reqwest::Method::GET, "/api/v1/admin/health".to_owned()),
        (reqwest::Method::GET, "/api/v1/admin/jobs".to_owned()),
        (
            reqwest::Method::GET,
            "/api/v1/admin/scheduled-tasks".to_owned(),
        ),
        (
            reqwest::Method::PATCH,
            format!("/api/v1/items/{item_id}/metadata"),
        ),
        (
            reqwest::Method::POST,
            format!("/api/v1/items/{item_id}/images/select"),
        ),
    ] {
        let mut req = client
            .request(method, format!("{base_url}{path}"))
            .header("X-Lux-Token", &viewer_token);
        if path.ends_with("/metadata") {
            req = req.json(&json!({ "title": "Nope" }));
        } else if path.ends_with("/images/select") {
            req = req.json(&json!({ "imageType": "POSTER", "url": "https://example.com/a.jpg" }));
        }
        let resp = req.send().await?;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "path={path}");
        assert_eq!(
            resp.json::<Value>().await?["error"]["code"],
            "PERMISSION_DENIED"
        );
    }

    // 3. Admin client token works across Phase 1 read/write endpoints with X-Lux-Token, X-Emby-Token, Bearer.
    for (header_name, header_val) in [
        ("X-Lux-Token", admin_token.clone()),
        ("X-Emby-Token", admin_token.clone()),
        ("Authorization", format!("Bearer {admin_token}")),
    ] {
        let libs = client
            .get(format!("{base_url}/api/v1/admin/libraries"))
            .header(header_name, &header_val)
            .send()
            .await?;
        assert_eq!(libs.status(), StatusCode::OK, "header={header_name}");
    }

    for path in [
        "/api/v1/admin/dashboard",
        "/api/v1/admin/health",
        "/api/v1/admin/jobs",
        "/api/v1/admin/scheduled-tasks",
    ] {
        let resp = client
            .get(format!("{base_url}{path}"))
            .header("X-Lux-Token", &admin_token)
            .send()
            .await?;
        assert_eq!(resp.status(), StatusCode::OK, "path={path}");
    }

    // 4. All-library scan endpoint queues only enabled libraries and is idempotent on active jobs.
    let all_scan = client
        .post(format!("{base_url}/api/v1/admin/libraries/scan"))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(all_scan.status(), StatusCode::ACCEPTED);
    let all_scan_body: Value = all_scan.json().await?;
    assert_eq!(all_scan_body["scope"], "ALL");
    let jobs = all_scan_body["jobs"].as_array().ok_or("missing jobs")?;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["libraryId"], enabled_library.id.to_string());
    let first_job_id = jobs[0]["id"].as_str().ok_or("missing job id")?.to_owned();

    // Re-triggering while active or immediately after succeeds with 202 and returns the job.
    let second_scan = client
        .post(format!("{base_url}/api/v1/admin/libraries/scan"))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(second_scan.status(), StatusCode::ACCEPTED);
    let second_body: Value = second_scan.json().await?;
    assert_eq!(second_body["jobs"].as_array().map(Vec::len), Some(1));

    // Cancel endpoint works with admin client token (may be 202 or 409 if background scan already finished).
    let cancel_resp = client
        .post(format!(
            "{base_url}/api/v1/admin/jobs/{first_job_id}/cancel"
        ))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert!(
        matches!(
            cancel_resp.status(),
            StatusCode::ACCEPTED | StatusCode::CONFLICT
        ),
        "unexpected cancel status: {}",
        cancel_resp.status()
    );

    // 5. Item metadata edit & item scan & image upload/delete/search/select & identify/refresh auth work with X-Lux-Token.
    let patched_meta = client
        .patch(format!("{base_url}/api/v1/items/{item_id}/metadata"))
        .header("X-Lux-Token", &admin_token)
        .json(&json!({
            "title": "Token Edited Movie",
            "originalTitle": "Example Movie",
            "overview": "Edited via X-Lux-Token",
            "productionYear": 2024,
            "lockedFields": ["title"]
        }))
        .send()
        .await?;
    assert_eq!(patched_meta.status(), StatusCode::OK);
    assert_eq!(
        patched_meta.json::<Value>().await?["title"],
        "Token Edited Movie"
    );

    let item_scan = client
        .post(format!("{base_url}/api/v1/admin/items/{item_id}/scan"))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(item_scan.status(), StatusCode::ACCEPTED);

    // Custom local image upload & delete via X-Lux-Token.
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x60,
        0x00, 0x02, 0x00, 0x00, 0x05, 0x00, 0x01, 0xE2, 0x26, 0x05, 0x9B, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let bad_upload = client
        .put(format!(
            "{base_url}/api/v1/admin/items/{item_id}/images/POSTER"
        ))
        .header("X-Lux-Token", &admin_token)
        .header("Content-Type", "image/png")
        .body(b"not-a-png".to_vec())
        .send()
        .await?;
    assert_eq!(bad_upload.status(), StatusCode::BAD_REQUEST);

    let uploaded = client
        .put(format!(
            "{base_url}/api/v1/admin/items/{item_id}/images/POSTER"
        ))
        .header("X-Lux-Token", &admin_token)
        .header("Content-Type", "image/png")
        .body(PNG_1X1.to_vec())
        .send()
        .await?;
    assert_eq!(uploaded.status(), StatusCode::OK);
    let uploaded_body: Value = uploaded.json().await?;
    assert_eq!(uploaded_body["image"]["imageType"], "POSTER");
    assert_eq!(uploaded_body["image"]["source"], "MANUAL");
    let uploaded_image_id = uploaded_body["image"]["id"]
        .as_str()
        .ok_or("missing uploaded image id")?
        .to_owned();

    let fetched_poster = client
        .get(format!("{base_url}/api/v1/items/{item_id}/images/poster"))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(fetched_poster.status(), StatusCode::OK);

    let deleted_image = client
        .delete(format!(
            "{base_url}/api/v1/admin/items/{item_id}/images/{uploaded_image_id}"
        ))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(deleted_image.status(), StatusCode::NO_CONTENT);

    // Image search/select & item identify (including providerIds) / refresh pass admin auth (not 401/403).
    let image_search = client
        .post(format!("{base_url}/api/v1/items/{item_id}/images/search"))
        .header("X-Lux-Token", &admin_token)
        .json(&json!({ "imageType": "POSTER", "language": "", "source": "" }))
        .send()
        .await?;
    assert_ne!(image_search.status(), StatusCode::FORBIDDEN);
    assert_ne!(image_search.status(), StatusCode::UNAUTHORIZED);

    let select_invalid = client
        .post(format!("{base_url}/api/v1/items/{item_id}/images/select"))
        .header("X-Lux-Token", &admin_token)
        .json(&json!({ "imageType": "POSTER", "url": "http://127.0.0.1/blocked.jpg" }))
        .send()
        .await?;
    assert_ne!(select_invalid.status(), StatusCode::FORBIDDEN);
    assert_ne!(select_invalid.status(), StatusCode::UNAUTHORIZED);

    let identify_search = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{item_id}/identify/candidates"
        ))
        .header("X-Lux-Token", &admin_token)
        .json(&json!({ "query": "Example Movie", "year": 2024 }))
        .send()
        .await?;
    assert_ne!(identify_search.status(), StatusCode::FORBIDDEN);
    assert_ne!(identify_search.status(), StatusCode::UNAUTHORIZED);

    let identify_by_provider = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{item_id}/identify/candidates"
        ))
        .header("X-Lux-Token", &admin_token)
        .json(&json!({ "providerIds": { "Tmdb": "603" } }))
        .send()
        .await?;
    assert_ne!(identify_by_provider.status(), StatusCode::BAD_REQUEST);
    assert_ne!(identify_by_provider.status(), StatusCode::FORBIDDEN);

    let refresh = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{item_id}/metadata/refresh"
        ))
        .header("X-Lux-Token", &admin_token)
        .json(&json!({ "mode": "FILL_MISSING" }))
        .send()
        .await?;
    assert_eq!(refresh.status(), StatusCode::ACCEPTED);

    // 5b. Admin user/permission management and global playback sessions via X-Lux-Token.
    let users_list = client
        .get(format!("{base_url}/api/v1/admin/users"))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(users_list.status(), StatusCode::OK);
    let created_user = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header("X-Lux-Token", &admin_token)
        .json(&json!({
            "username": "managed-user",
            "displayName": "Managed User",
            "password": "managed-password",
            "isAdmin": false
        }))
        .send()
        .await?;
    assert_eq!(created_user.status(), StatusCode::CREATED);
    let created_user_id = created_user.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing created user id")?
        .to_owned();
    let updated_user = client
        .patch(format!("{base_url}/api/v1/admin/users/{created_user_id}"))
        .header("X-Lux-Token", &admin_token)
        .json(&json!({ "canDownload": true }))
        .send()
        .await?;
    assert_eq!(updated_user.status(), StatusCode::OK);
    assert_eq!(
        updated_user.json::<Value>().await?["user"]["canDownload"],
        true
    );

    let progress = client
        .post(format!("{base_url}/api/v1/items/{item_id}/progress"))
        .header("X-Lux-Token", &viewer_token)
        .json(&json!({ "positionTicks": 1000, "durationTicks": 10000, "state": "PLAYING" }))
        .send()
        .await?;
    assert_eq!(progress.status(), StatusCode::NO_CONTENT);

    let playback_sessions = client
        .get(format!("{base_url}/api/v1/admin/playback-sessions"))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(playback_sessions.status(), StatusCode::OK);
    let playback_sessions_body: Value = playback_sessions.json().await?;
    let active_playback_id = playback_sessions_body["sessions"]
        .as_array()
        .and_then(|sessions| sessions.first())
        .and_then(|session| session["id"].as_str())
        .ok_or("missing active playback session")?
        .to_owned();
    assert_eq!(playback_sessions_body["sessions"][0]["canStop"], false);
    let stop_direct = client
        .delete(format!(
            "{base_url}/api/v1/admin/playback-sessions/{active_playback_id}"
        ))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(stop_direct.status(), StatusCode::CONFLICT);
    let stop_missing = client
        .delete(format!(
            "{base_url}/api/v1/admin/playback-sessions/{}",
            uuid::Uuid::now_v7()
        ))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(stop_missing.status(), StatusCode::NOT_FOUND);

    // 6. Audit logs record the admin user's actorUserId & actorUsername when using X-Lux-Token.
    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit"))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(audit.status(), StatusCode::OK);
    let audit_body: Value = audit.json().await?;
    let events = audit_body["events"].as_array().ok_or("missing events")?;
    let meta_event = events
        .iter()
        .find(|event| event["eventType"] == "METADATA_EDITED")
        .ok_or("missing METADATA_EDITED audit event")?;
    assert_eq!(meta_event["actorUserId"], admin.id.to_string());
    assert_eq!(meta_event["actorUsername"], "admin");
    assert!(!audit_body.to_string().contains(&admin_token));

    // 7. Web-session-only endpoints still reject admin client tokens.
    let api_key_get = client
        .get(format!("{base_url}/api/v1/admin/api-key"))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(api_key_get.status(), StatusCode::FORBIDDEN);
    let api_key_rotate = client
        .post(format!("{base_url}/api/v1/admin/api-key/rotate"))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(api_key_rotate.status(), StatusCode::FORBIDDEN);

    // 8. Demoted admin (canManageServer = false) token is immediately rejected with 403.
    users
        .create_user("backup-admin", "Backup Admin", "backup password", true)
        .await?;
    users
        .update_user(
            &admin.id.to_string(),
            UserUpdate {
                can_manage_server: Some(false),
                ..UserUpdate::default()
            },
        )
        .await?;
    let demoted_libs = client
        .get(format!("{base_url}/api/v1/admin/libraries"))
        .header("X-Lux-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(demoted_libs.status(), StatusCode::FORBIDDEN);

    database.close().await;
    Ok(())
}
