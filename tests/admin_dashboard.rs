use std::{net::SocketAddr, time::Duration};

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    observability::logs::LogStore,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

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

#[tokio::test]
async fn admin_dashboard_returns_server_playback_and_activity_data()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let log_store = LogStore::new(&config.config_dir);
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    sqlx::query("UPDATE users SET can_remote_access = 1 WHERE id = ?")
        .bind(admin.id.to_string())
        .execute(database.pool())
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let disabled_library = libraries
        .create_library("Disabled", LibraryKind::Movie, false)
        .await?;
    sqlx::query("UPDATE libraries SET is_enabled = 0 WHERE id = ?")
        .bind(disabled_library.id.to_string())
        .execute(database.pool())
        .await?;
    let root = temp_dir.path().join("Shows");
    let season_dir = root.join("九门/Season 01");
    tokio::fs::create_dir_all(&season_dir).await?;
    tokio::fs::write(
        season_dir.join("张启山和吴老狗达成合作.S01E02.mkv"),
        b"video",
    )
    .await?;
    let second_season_dir = root.join("九门/Season 02");
    tokio::fs::create_dir_all(&second_season_dir).await?;
    tokio::fs::write(second_season_dir.join("霍三娘登场.S02E01.mkv"), b"video").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;

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
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?;
    let unauthenticated_retention_update = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .json(&json!({ "userActivityRetentionDays": 30 }))
        .send()
        .await?;
    assert_eq!(
        unauthenticated_retention_update.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let emby_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            "Authorization",
            r#"Emby Client="DashboardTest", Device="Mac", DeviceId="dashboard-device", Version="1""#,
        )
        .header("X-Forwarded-For", "203.0.113.10")
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(emby_login.status(), reqwest::StatusCode::OK);
    let emby_body = emby_login.text().await?;
    let token = serde_json::from_str::<Value>(&emby_body)?["AccessToken"]
        .as_str()
        .ok_or("missing Emby token")?
        .to_owned();
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'EPISODE' AND season_number = 1 AND episode_number = 2 LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let source_id: String =
        sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ? LIMIT 1")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    let user_id = admin.id.to_string();
    sqlx::query(
        "INSERT INTO web_playback_sessions (
            id, user_id, item_id, media_source_id, play_session_id,
            tier, plan, state, is_admin, expires_at, last_heartbeat_at,
            video_codec, audio_codec, video_bitrate, audio_bitrate, transcoding_container
         ) VALUES (?, ?, ?, ?, ?, 4, 'SERVER_HLS', 'ACTIVE', 1,
                   unixepoch() + 900, unixepoch(), 'h264', 'aac', 1000000, 192000, 'ts')",
    )
    .bind("dashboard-transcode-session")
    .bind(&user_id)
    .bind(&item_id)
    .bind(&source_id)
    .bind("lux-emby:dashboard-transcode-session")
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO audit_events (
            id, actor_user_id, event_type, target_type, target_id, metadata_json, created_at
         ) VALUES ('legacy-playback-activity', ?, 'PLAYBACK_PAUSED', 'media_item', ?, '{}', unixepoch())",
    )
    .bind(&user_id)
    .bind(&item_id)
    .execute(database.pool())
    .await?;
    assert_eq!(database.migrate_legacy_audit_events_to_logs().await?, 1);
    sqlx::query("DROP TABLE audit_events")
        .execute(database.pool())
        .await?;
    let playing = client
        .post(format!("{base_url}/Sessions/Playing"))
        .header("X-Emby-Token", &token)
        .header("X-Forwarded-For", "203.0.113.10")
        .json(&json!({
            "ItemId": item_id,
            "MediaSourceId": source_id,
            "PlaySessionId": "lux-emby:dashboard-transcode-session",
            "PositionTicks": 600000000,
            "RunTimeTicks": 36_000_000_000i64,
            "DeviceId": "dashboard-device",
            "Client": "DashboardTest",
            "DeviceName": "Mac",
            "DeviceType": "Desktop",
            "ApplicationVersion": "4.2.1",
        }))
        .send()
        .await?;
    assert_eq!(playing.status(), reqwest::StatusCode::NO_CONTENT);

    let web_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let session_cookie = cookie_value(web_login.headers(), "lux_session");
    let csrf_cookie = cookie_value(web_login.headers(), "lux_csrf");
    let cookies = format!("lux_session={session_cookie}; lux_csrf={csrf_cookie}");
    let initial_settings: Value = client
        .get(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(initial_settings["userDeletionGraceDays"], 7);
    assert_eq!(initial_settings["userActivityRetentionDays"], 90);
    assert_eq!(
        initial_settings["newUserPermissions"]["libraryAccessMode"],
        "NONE"
    );
    assert_eq!(
        initial_settings["newUserPermissions"]["libraryIds"],
        json!([])
    );
    assert_eq!(
        initial_settings["newUserPermissions"]["maxSimultaneousPlaybacks"],
        2
    );
    assert_eq!(initial_settings["trustedProxyCidrs"], json!([]));

    let missing_csrf_proxy_update = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .json(&json!({ "trustedProxyCidrs": ["10.0.0.0/8"] }))
        .send()
        .await?;
    assert_eq!(
        missing_csrf_proxy_update.status(),
        reqwest::StatusCode::FORBIDDEN
    );

    let trusted_proxy_update = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf_cookie)
        .json(&json!({
            "trustedProxyCidrs": ["10.1.2.3/8", "2001:db8::1/32"]
        }))
        .send()
        .await?;
    assert_eq!(trusted_proxy_update.status(), reqwest::StatusCode::OK);
    let trusted_proxy_body: Value = trusted_proxy_update.json().await?;
    let canonical_trusted_proxies = json!(["10.0.0.0/8", "2001:db8::/32"]);
    assert_eq!(
        trusted_proxy_body["trustedProxyCidrs"],
        canonical_trusted_proxies
    );

    let excessive_cidrs = (0..101)
        .map(|octet| format!("192.0.2.{octet}/32"))
        .collect::<Vec<_>>();
    for invalid_trusted_proxies in [
        json!(["not-an-ip"]),
        json!(["10.0.0.0/8", "10.2.3.4/8"]),
        json!(excessive_cidrs),
    ] {
        let rejected = client
            .patch(format!("{base_url}/api/v1/admin/settings"))
            .header(COOKIE, &cookies)
            .header("X-CSRF-Token", &csrf_cookie)
            .json(&json!({ "trustedProxyCidrs": invalid_trusted_proxies }))
            .send()
            .await?;
        assert_eq!(rejected.status(), reqwest::StatusCode::BAD_REQUEST);
    }
    let preserved_trusted_proxies: Value = client
        .get(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        preserved_trusted_proxies["trustedProxyCidrs"],
        canonical_trusted_proxies
    );

    let selected_defaults = json!({
        "libraryAccessMode": "SELECTED",
        "libraryIds": [library.id.to_string()],
        "canDownload": true,
        "canPlay": false,
        "maxSimultaneousPlaybacks": 4,
        "allowRemux": false,
        "allowAudioTranscode": false,
        "allowVideoTranscode": true,
    });
    let updated_defaults = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf_cookie)
        .json(&json!({ "newUserPermissions": selected_defaults }))
        .send()
        .await?;
    assert_eq!(updated_defaults.status(), reqwest::StatusCode::OK);
    let updated_defaults_body: Value = updated_defaults.json().await?;
    assert_eq!(
        updated_defaults_body["newUserPermissions"]["libraryAccessMode"],
        "SELECTED"
    );
    assert_eq!(
        updated_defaults_body["newUserPermissions"]["libraryIds"],
        json!([library.id.to_string()])
    );
    assert_eq!(
        updated_defaults_body["newUserPermissions"]["canDownload"],
        true
    );
    assert_eq!(
        updated_defaults_body["newUserPermissions"]["maxSimultaneousPlaybacks"],
        4
    );

    for invalid_defaults in [
        json!({
            "libraryAccessMode": "SELECTED",
            "libraryIds": [library.id.to_string()],
            "canDownload": true,
            "canPlay": true,
            "maxSimultaneousPlaybacks": -1,
            "allowRemux": true,
            "allowAudioTranscode": true,
            "allowVideoTranscode": true,
        }),
        json!({
            "libraryAccessMode": "SELECTED",
            "libraryIds": [disabled_library.id.to_string()],
            "canDownload": true,
            "canPlay": true,
            "maxSimultaneousPlaybacks": 2,
            "allowRemux": true,
            "allowAudioTranscode": true,
            "allowVideoTranscode": true,
        }),
        json!({
            "libraryAccessMode": "ALL",
            "libraryIds": [library.id.to_string()],
            "canDownload": true,
            "canPlay": true,
            "maxSimultaneousPlaybacks": 2,
            "allowRemux": true,
            "allowAudioTranscode": true,
            "allowVideoTranscode": true,
        }),
        json!({
            "libraryAccessMode": "SELECTED",
            "libraryIds": [library.id.to_string(), library.id.to_string()],
            "canDownload": true,
            "canPlay": true,
            "maxSimultaneousPlaybacks": 2,
            "allowRemux": true,
            "allowAudioTranscode": true,
            "allowVideoTranscode": true,
        }),
    ] {
        let invalid_defaults_response = client
            .patch(format!("{base_url}/api/v1/admin/settings"))
            .header(COOKIE, &cookies)
            .header("X-CSRF-Token", &csrf_cookie)
            .json(&json!({ "newUserPermissions": invalid_defaults }))
            .send()
            .await?;
        assert_eq!(
            invalid_defaults_response.status(),
            reqwest::StatusCode::BAD_REQUEST
        );
    }

    let defaults_after_rejection: Value = client
        .get(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        defaults_after_rejection["newUserPermissions"]["libraryAccessMode"],
        "SELECTED"
    );
    assert_eq!(
        defaults_after_rejection["newUserPermissions"]["libraryIds"],
        json!([library.id.to_string()])
    );

    let empty_selection = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf_cookie)
        .json(&json!({
            "newUserPermissions": {
                "libraryAccessMode": "SELECTED",
                "libraryIds": [],
                "canDownload": false,
                "canPlay": true,
                "maxSimultaneousPlaybacks": 2,
                "allowRemux": true,
                "allowAudioTranscode": true,
                "allowVideoTranscode": true,
            }
        }))
        .send()
        .await?;
    assert_eq!(empty_selection.status(), reqwest::StatusCode::OK);
    let empty_selection_body: Value = empty_selection.json().await?;
    assert_eq!(
        empty_selection_body["newUserPermissions"]["libraryAccessMode"],
        "NONE"
    );
    assert_eq!(
        empty_selection_body["newUserPermissions"]["libraryIds"],
        json!([])
    );

    let settings = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf_cookie)
        .json(&json!({ "serverName": "客厅 Lux", "userDeletionGraceDays": 30 }))
        .send()
        .await?;
    assert_eq!(settings.status(), reqwest::StatusCode::OK);
    let settings_body: Value = settings.json().await?;
    assert_eq!(settings_body["userDeletionGraceDays"], 30);
    assert_eq!(
        settings_body["trustedProxyCidrs"],
        canonical_trusted_proxies
    );

    let cleared_trusted_proxies = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf_cookie)
        .json(&json!({ "trustedProxyCidrs": [] }))
        .send()
        .await?;
    assert_eq!(cleared_trusted_proxies.status(), reqwest::StatusCode::OK);
    let cleared_trusted_proxy_body: Value = cleared_trusted_proxies.json().await?;
    assert_eq!(cleared_trusted_proxy_body["trustedProxyCidrs"], json!([]));

    for invalid_days in [0, 3_651] {
        let invalid_retention = client
            .patch(format!("{base_url}/api/v1/admin/settings"))
            .header(COOKIE, &cookies)
            .header("X-CSRF-Token", &csrf_cookie)
            .json(&json!({ "userDeletionGraceDays": invalid_days }))
            .send()
            .await?;
        assert_eq!(invalid_retention.status(), reqwest::StatusCode::BAD_REQUEST);
    }

    let settings_after_rejection: Value = client
        .get(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(settings_after_rejection["userDeletionGraceDays"], 30);

    let activity_retention = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf_cookie)
        .json(&json!({ "userActivityRetentionDays": 180 }))
        .send()
        .await?;
    assert_eq!(activity_retention.status(), reqwest::StatusCode::OK);
    let activity_retention_body: Value = activity_retention.json().await?;
    assert_eq!(activity_retention_body["userActivityRetentionDays"], 180);

    for invalid_days in [0, -1, 3_651] {
        let invalid_activity_retention = client
            .patch(format!("{base_url}/api/v1/admin/settings"))
            .header(COOKIE, &cookies)
            .header("X-CSRF-Token", &csrf_cookie)
            .json(&json!({ "userActivityRetentionDays": invalid_days }))
            .send()
            .await?;
        assert_eq!(
            invalid_activity_retention.status(),
            reqwest::StatusCode::BAD_REQUEST
        );
    }
    let activity_retention_after_rejection: Value = client
        .get(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        activity_retention_after_rejection["userActivityRetentionDays"],
        180
    );

    for _ in 0..24 {
        let repeated_login = client
            .post(format!("{base_url}/api/v1/auth/login"))
            .header("X-Forwarded-For", "203.0.113.10")
            .json(&json!({ "username": "admin", "password": "correct password" }))
            .send()
            .await?;
        assert_eq!(repeated_login.status(), reqwest::StatusCode::OK);
    }
    log_store
        .append_json(json!({
            "recordType": "admin_audit_event",
            "id": "activity-with-deleted-user-and-media",
            "actorUserId": "deleted-user",
            "actorUsername": null,
            "eventType": "PLAYBACK_PAUSED",
            "targetType": "media_item",
            "targetId": "deleted-media",
            "metadata": {"state": "PAUSED"},
            "createdAt": 1,
        }))
        .await?;

    let dashboard = client
        .get(format!("{base_url}/api/v1/admin/dashboard"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(dashboard.status(), reqwest::StatusCode::OK);
    let body: Value = dashboard.json().await?;
    assert_eq!(body["server"]["name"], "客厅 Lux");
    assert_eq!(body["server"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["stats"]["movieCount"], 0);
    assert_eq!(body["stats"]["seriesCount"], 1);
    assert_eq!(body["stats"]["userCount"], 1);
    assert!(body["health"]["runtime"]["seconds"].is_number());
    assert_eq!(body["health"]["jobs"]["scanRunning"], 0);
    assert_eq!(body["health"]["jobs"]["scanFailed"], 0);
    assert_eq!(body["health"]["resources"]["cpu"]["source"], "cgroup");
    assert!(
        body["health"]["resources"]["cpu"]
            .get("capacityCores")
            .is_some()
    );
    assert!(
        body["health"]["resources"]["cpu"]
            .get("usageCores")
            .is_some()
    );
    assert_eq!(
        body["health"]["resources"]["mediaStorage"]["path"],
        "/media"
    );
    assert_eq!(body["nowPlaying"][0]["userName"], "Admin");
    assert_eq!(body["nowPlaying"][0]["title"], "张启山和吴老狗达成合作");
    assert_eq!(body["nowPlaying"][0]["seriesTitle"], "九门");
    assert!(body["nowPlaying"][0]["seriesId"].is_string());
    assert_eq!(body["nowPlaying"][0]["client"], "DashboardTest");
    assert_eq!(body["nowPlaying"][0]["clientVersion"], "4.2.1");
    assert_eq!(body["nowPlaying"][0]["deviceName"], "Mac");
    assert_eq!(body["nowPlaying"][0]["deviceType"], "Desktop");
    assert_eq!(body["nowPlaying"][0]["deviceId"], "dashboard-device");
    assert_eq!(body["nowPlaying"][0]["playMethod"], "Transcode");
    assert_eq!(body["nowPlaying"][0]["playbackPlan"], "SERVER_HLS");
    assert_eq!(body["nowPlaying"][0]["serverTier"], 4);
    assert_eq!(body["nowPlaying"][0]["output"]["videoCodec"], "h264");
    assert_eq!(body["nowPlaying"][0]["output"]["audioCodec"], "aac");
    assert_eq!(body["nowPlaying"][0]["output"]["videoBitrate"], 1_000_000);
    assert_eq!(body["nowPlaying"][0]["output"]["audioBitrate"], 192_000);
    assert_eq!(body["nowPlaying"][0]["output"]["container"], "ts");
    // Trusted proxies were cleared above, so the raw X-Forwarded-For header must be ignored.
    assert_eq!(body["nowPlaying"][0]["remoteIp"], "127.0.0.1");
    assert!(body["nowPlaying"][0]["remoteIpLocation"].is_null());
    let events = body["activity"].as_array().ok_or("missing activity")?;
    assert!(
        events
            .iter()
            .any(|event| event["eventType"] == "AUTH_LOGIN")
    );
    assert!(
        events.iter().any(|event| {
            event["eventType"] == "AUTH_LOGIN" && event["remoteIp"] == "127.0.0.1"
        })
    );
    assert!(
        events
            .iter()
            .any(|event| event["eventType"] == "PLAYBACK_STARTED")
    );
    assert!(
        events
            .iter()
            .any(|event| event["id"] == "legacy-playback-activity")
    );
    let playback_activity = events
        .iter()
        .find(|event| event["eventType"] == "PLAYBACK_STARTED")
        .ok_or("missing playback activity")?;
    assert_eq!(playback_activity["remoteIp"], "127.0.0.1");
    assert_eq!(playback_activity["metadata"]["remoteIp"], "127.0.0.1");
    assert_eq!(playback_activity["targetSeriesTitle"], "九门");
    assert_eq!(playback_activity["targetSeasonNumber"], 1);
    assert_eq!(playback_activity["targetSeriesSeasonCount"], 2);
    assert!(playback_activity["remoteIpLocation"].is_null());
    assert!(events.len() <= 24);
    assert!(
        events
            .iter()
            .filter(|event| event["eventType"] == "AUTH_LOGIN")
            .count()
            <= 12
    );
    assert!(
        events
            .iter()
            .filter(|event| event["eventType"] != "AUTH_LOGIN")
            .count()
            <= 12
    );
    assert!(events.windows(2).all(|pair| {
        let previous_time = pair[0]["createdAt"].as_i64().unwrap_or_default();
        let next_time = pair[1]["createdAt"].as_i64().unwrap_or_default();
        previous_time > next_time
            || (previous_time == next_time
                && pair[0]["id"].as_str().unwrap_or_default()
                    >= pair[1]["id"].as_str().unwrap_or_default())
    }));
    let orphaned_activity = events
        .iter()
        .find(|event| event["id"] == "activity-with-deleted-user-and-media")
        .ok_or("missing activity with deleted user and media")?;
    assert!(orphaned_activity["userName"].is_null());
    assert!(orphaned_activity["targetTitle"].is_null());

    sqlx::query(
        "UPDATE playback_sessions
         SET last_event_at = unixepoch() - 3600
         WHERE play_session_id = ?",
    )
    .bind("lux-emby:dashboard-transcode-session")
    .execute(database.pool())
    .await?;
    let stale_dashboard = client
        .get(format!("{base_url}/api/v1/admin/dashboard"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert!(
        stale_dashboard["nowPlaying"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );

    let stopped = client
        .post(format!("{base_url}/Sessions/Playing/Stopped"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": body["nowPlaying"][0]["itemId"],
            "MediaSourceId": source_id,
            "PlaySessionId": "lux-emby:dashboard-transcode-session",
            "PositionTicks": 600000000,
        }))
        .send()
        .await?;
    assert_eq!(stopped.status(), reqwest::StatusCode::NO_CONTENT);
    let after_stop = client
        .get(format!("{base_url}/api/v1/admin/dashboard"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert!(
        after_stop["nowPlaying"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert!(after_stop["activity"].as_array().is_some_and(|events| {
        events
            .iter()
            .any(|event| event["eventType"] == "PLAYBACK_STOPPED")
    }));
    let stopped_activity = after_stop["activity"]
        .as_array()
        .and_then(|events| {
            events
                .iter()
                .find(|event| event["eventType"] == "PLAYBACK_STOPPED")
        })
        .ok_or("missing stopped activity")?;
    assert_eq!(stopped_activity["remoteIp"], "127.0.0.1");

    server.abort();
    Ok(())
}
