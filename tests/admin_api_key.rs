use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    application::{libraries::LibraryService, scanner::LibraryScanner},
    auth::{
        admin_api_key::AdminApiKeyService,
        emby::EmbyAuthService,
        sessions::WebAuthService,
        users::{UserStore, UserUpdate},
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use serde_json::json;
use tokio::net::TcpListener;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn emby_public_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
}

#[tokio::test]
async fn shared_admin_key_survives_restart_and_can_be_revoked()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let service = AdminApiKeyService::new(config.config_dir.clone(), database.clone());

    assert!(service.current().await?.is_none());

    let key = service.rotate().await?;
    assert!(key.starts_with("lux_"));
    assert_eq!(service.current().await?.as_deref(), Some(key.as_str()));
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(config.config_dir.join("lux_admin_api_key"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        service.resolve_principal(&key).await?,
        Some(luxd::auth::users::AuthenticationPrincipal::SharedAdminApiKey)
    );

    let restarted = AdminApiKeyService::new(config.config_dir.clone(), database.clone());
    assert_eq!(
        restarted.resolve_principal(&key).await?,
        Some(luxd::auth::users::AuthenticationPrincipal::SharedAdminApiKey)
    );

    service.revoke().await?;
    assert!(service.current().await?.is_none());
    assert!(service.resolve_principal(&key).await?.is_none());

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn shared_admin_key_authenticates_lux_and_emby_requests_without_csrf()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let users = UserStore::new(database.clone())?;
    let admin = users
        .create_initial_admin("Admin", "Administrator", "correct horse battery staple")
        .await?;
    let key_service = AdminApiKeyService::new(config.config_dir.clone(), database.clone());
    let key = key_service.rotate().await?;
    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(admin.id.to_string())
        .execute(database.pool())
        .await?;
    let setup = SetupService::new(database.clone())?;
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
    let client = reqwest::Client::new();

    let me = client
        .get(format!("http://{address}/api/v1/auth/me"))
        .header("X-Lux-Api-Key", &key)
        .send()
        .await?;
    assert_eq!(me.status(), reqwest::StatusCode::OK);
    let me_body = me.json::<serde_json::Value>().await?;
    assert_eq!(me_body["principal"]["type"], "shared_admin_api_key");
    assert_eq!(me_body["principal"]["permissions"]["canManageServer"], true);
    assert_eq!(me_body["principal"]["permissions"]["canRemoteAccess"], true);
    assert!(me_body.get("user").is_none());

    for request in [
        client
            .get(format!("http://{address}/api/v1/auth/me?api_key={key}"))
            .build()?,
        client
            .get(format!("http://{address}/api/v1/auth/me"))
            .header("Authorization", format!("Bearer {key}"))
            .build()?,
        client
            .get(format!("http://{address}/api/v1/auth/me"))
            .header("X-Emby-Token", &key)
            .build()?,
    ] {
        assert_eq!(
            client.execute(request).await?.status(),
            reqwest::StatusCode::OK
        );
    }

    let settings = client
        .patch(format!("http://{address}/api/v1/admin/settings"))
        .header("X-Lux-Api-Key", &key)
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(settings.status(), reqwest::StatusCode::OK);

    let audit = client
        .get(format!("http://{address}/api/v1/admin/audit"))
        .header("X-Lux-Api-Key", &key)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let audit_body = audit.json::<serde_json::Value>().await?;
    assert_eq!(audit_body["events"][0]["metadata"]["auth"], "admin_api_key");
    assert!(!audit_body.to_string().contains(&key));

    users
        .create_initial_admin("Admin", "Administrator", "correct horse battery staple")
        .await?;

    let emby = client
        .get(format!("http://{address}/System/Info?api_key={key}"))
        .send()
        .await?;
    assert_eq!(emby.status(), reqwest::StatusCode::OK);

    let emby_with_lux_header = client
        .get(format!("http://{address}/System/Info"))
        .header("X-Lux-Api-Key", &key)
        .send()
        .await?;
    assert_eq!(emby_with_lux_header.status(), reqwest::StatusCode::OK);

    server.abort();
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn shared_admin_key_can_follow_emby_library_discovery_flow()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup
        .complete("Admin", "Administrator", "correct horse battery staple")
        .await?;
    let users = UserStore::new(database.clone())?;
    let manager = users
        .create_user("aaaoperator", "Operator", "operator password", false)
        .await?;
    let target = users
        .create_user("target", "Target", "target password", false)
        .await?;
    let disabled_target = users
        .create_user(
            "disabled-target",
            "Disabled Target",
            "disabled password",
            false,
        )
        .await?;
    users
        .update_user(
            &disabled_target.id.to_string(),
            UserUpdate {
                is_disabled: Some(true),
                ..UserUpdate::default()
            },
        )
        .await?
        .ok_or("disabled target disappeared")?;
    users
        .update_user(
            &manager.id.to_string(),
            UserUpdate {
                can_manage_server: Some(true),
                ..UserUpdate::default()
            },
        )
        .await?
        .ok_or("operator disappeared after permission update")?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    tokio::fs::write(media_root.join("Movie (2024).mkv"), b"movie").await?;
    let library_root = libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    sqlx::query("DELETE FROM users WHERE id IN (?, ?)")
        .bind(admin.id.to_string())
        .bind(manager.id.to_string())
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
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::new();
    let emby_library_id = emby_public_id(&library.id.to_string());
    let emby_root_id = emby_public_id(&library_root.root.id.to_string());

    let views = client
        .get(format!(
            "http://{address}/Users/{}/Views?api_key={key}",
            target.id
        ))
        .send()
        .await?;
    assert_eq!(views.status(), reqwest::StatusCode::OK);
    let views_body = views.json::<serde_json::Value>().await?;
    assert_eq!(views_body["TotalRecordCount"], 1);
    assert_eq!(views_body["Items"][0]["Id"], emby_library_id);

    for unknown_or_disabled_user_id in [
        "00000000-0000-4000-8000-000000000001".to_owned(),
        disabled_target.id.to_string(),
    ] {
        let views = client
            .get(format!(
                "http://{address}/Users/{unknown_or_disabled_user_id}/Views?api_key={key}"
            ))
            .send()
            .await?;
        assert_eq!(views.status(), reqwest::StatusCode::NOT_FOUND);
    }

    for path in ["/Library/VirtualFolders", "/emby/Library/VirtualFolders"] {
        let virtual_folders = client
            .get(format!("http://{address}{path}?api_key={key}"))
            .send()
            .await?;
        assert_eq!(virtual_folders.status(), reqwest::StatusCode::OK);
        let virtual_folders_body = virtual_folders.json::<serde_json::Value>().await?;
        assert_eq!(virtual_folders_body[0]["Name"], "Movies");
        assert_eq!(virtual_folders_body[0]["Id"], emby_library_id);
        assert_eq!(virtual_folders_body[0]["Guid"], emby_library_id);
        assert_eq!(virtual_folders_body[0]["ItemId"], emby_library_id);
        assert_eq!(virtual_folders_body[0]["CollectionType"], "movies");
        assert_eq!(
            virtual_folders_body[0]["Locations"][0],
            media_root.to_string_lossy().to_string()
        );
        let options = &virtual_folders_body[0]["LibraryOptions"];
        for field in [
            "EnableArchiveMediaFiles",
            "EnablePhotos",
            "EnableRealtimeMonitor",
            "EnableChapterImageExtraction",
            "ExtractChapterImagesDuringLibraryScan",
            "DownloadImagesInAdvance",
            "PathInfos",
            "SaveLocalMetadata",
            "SaveLocalThumbnailSets",
            "ImportMissingEpisodes",
            "EnableAutomaticSeriesGrouping",
            "EnableEmbeddedTitles",
            "EnableAudioResume",
            "AutomaticRefreshIntervalDays",
            "PreferredMetadataLanguage",
            "ContentType",
            "MetadataCountryCode",
            "SeasonZeroDisplayName",
            "MetadataSavers",
            "DisabledLocalMetadataReaders",
            "LocalMetadataReaderOrder",
            "DisabledSubtitleFetchers",
            "SubtitleFetcherOrder",
            "SkipSubtitlesIfEmbeddedSubtitlesPresent",
            "SkipSubtitlesIfAudioTrackMatches",
            "SubtitleDownloadLanguages",
            "RequirePerfectSubtitleMatch",
            "SaveSubtitlesWithMedia",
            "ForcedSubtitlesOnly",
            "TypeOptions",
            "CollapseSingleItemFolders",
            "MinResumePct",
            "MaxResumePct",
            "MinResumeDurationSeconds",
            "ThumbnailImagesIntervalSeconds",
        ] {
            assert!(!options[field].is_null(), "missing LibraryOptions.{field}");
        }
        assert_eq!(options["PreferredMetadataLanguage"], "zh-CN");
        assert_eq!(options["MetadataCountryCode"], "CN");
        assert_eq!(options["ContentType"], "movies");
        assert_eq!(options["EnableRealtimeMonitor"], true);
        assert_eq!(options["MaxResumePct"], 90);
        assert_eq!(options["MinResumeDurationSeconds"], 120);
        assert_eq!(options["SubtitleDownloadLanguages"][0], "chi");
        assert_eq!(
            options["PathInfos"][0]["Path"],
            media_root.to_string_lossy().to_string()
        );
        assert_eq!(options["PathInfos"][0]["NetworkPath"], "");
        assert_eq!(options["TypeOptions"][0]["Type"], "Movie");
        assert!(options["TypeOptions"][0]["MetadataFetchers"].is_array());
        assert!(options["TypeOptions"][0]["ImageFetchers"].is_array());
        assert!(options["TypeOptions"][0]["ImageOptions"].is_array());
        assert_eq!(
            options["TypeOptions"][0]["ImageOptions"][0]["Type"],
            "Primary"
        );
    }

    for path in [
        "/Library/SelectableMediaFolders",
        "/emby/Library/SelectableMediaFolders",
    ] {
        let selectable_folders = client
            .get(format!("http://{address}{path}?api_key={key}"))
            .send()
            .await?;
        assert_eq!(selectable_folders.status(), reqwest::StatusCode::OK);
        let body = selectable_folders.json::<serde_json::Value>().await?;
        assert_eq!(body[0]["Name"], "Movies");
        assert_eq!(body[0]["Id"], emby_library_id);
        assert_eq!(body[0]["Guid"], emby_library_id);
        assert_eq!(body[0]["IsUserAccessConfigurable"], true);
        assert_eq!(body[0]["SubFolders"][0]["Name"], "Movies");
        assert_eq!(body[0]["SubFolders"][0]["Id"], emby_root_id);
        assert_eq!(
            body[0]["SubFolders"][0]["Path"],
            media_root.to_string_lossy().to_string()
        );
        assert_eq!(body[0]["SubFolders"][0]["IsUserAccessConfigurable"], true);
    }

    let root = client
        .get(format!(
            "http://{address}/Users/{}/Items/Root?api_key={key}",
            target.id
        ))
        .send()
        .await?;
    assert_eq!(root.status(), reqwest::StatusCode::OK);
    assert_eq!(root.json::<serde_json::Value>().await?["ChildCount"], 1);

    let items = client
        .get(format!(
            "http://{address}/Users/{}/Items?ParentId={}&IncludeItemTypes=CollectionFolder&Limit=10&api_key={key}",
            target.id, target.id
        ))
        .send()
        .await?;
    assert_eq!(items.status(), reqwest::StatusCode::OK);
    assert_eq!(
        items.json::<serde_json::Value>().await?["TotalRecordCount"],
        1
    );

    let movies = client
        .get(format!(
            "http://{address}/emby/Users/{}/Items?ParentId={emby_library_id}&IncludeItemTypes=Movie&Recursive=true&Limit=10&api_key={key}",
            target.id
        ))
        .send()
        .await?;
    assert_eq!(movies.status(), reqwest::StatusCode::OK);
    let movie_id = movies.json::<serde_json::Value>().await?["Items"][0]["Id"]
        .as_str()
        .ok_or("missing Emby movie id")?
        .to_owned();
    let marked_played = client
        .post(format!(
            "http://{address}/Users/{}/PlayedItems/{movie_id}?api_key={key}",
            target.id
        ))
        .send()
        .await?;
    assert_eq!(marked_played.status(), reqwest::StatusCode::OK);
    assert_eq!(
        marked_played.json::<serde_json::Value>().await?["Played"],
        true
    );
    let playback_event = client
        .post(format!("http://{address}/Sessions/Playing?api_key={key}"))
        .json(&json!({
            "ItemId": movie_id,
            "PositionTicks": 123,
        }))
        .send()
        .await?;
    assert_eq!(playback_event.status(), reqwest::StatusCode::UNAUTHORIZED);
    let movie_detail = client
        .get(format!(
            "http://{address}/emby/Users/{}/Items/{movie_id}?Fields=MediaSources&api_key={key}",
            target.id
        ))
        .send()
        .await?;
    assert_eq!(movie_detail.status(), reqwest::StatusCode::OK);
    assert!(movie_detail.json::<serde_json::Value>().await?["MediaSources"].is_array());

    let search_hints = client
        .get(format!(
            "http://{address}/Search/Hints?SearchTerm=Movie&api_key={key}"
        ))
        .send()
        .await?;
    assert_eq!(search_hints.status(), reqwest::StatusCode::OK);
    assert_eq!(
        search_hints.json::<serde_json::Value>().await?["TotalRecordCount"],
        1
    );

    let stream = client
        .get(format!(
            "http://{address}/Videos/{movie_id}/stream?api_key={key}"
        ))
        .send()
        .await?;
    assert_eq!(stream.status(), reqwest::StatusCode::OK);
    assert_eq!(stream.bytes().await?.as_ref(), b"movie");

    let internal_movie_id = uuid::Uuid::from_u128(movie_id.parse()?).to_string();
    sqlx::query(
        "INSERT INTO playback_sessions (
            id, user_id, item_id, play_session_id, device_id, state, last_event_at
         ) VALUES ('shared-key-session', ?, ?, 'shared-key-play', 'test-device', 'PLAYING', unixepoch())",
    )
    .bind(target.id.to_string())
    .bind(&internal_movie_id)
    .execute(database.pool())
    .await?;
    let sessions = client
        .get(format!("http://{address}/Sessions?api_key={key}"))
        .send()
        .await?;
    assert_eq!(sessions.status(), reqwest::StatusCode::OK);
    let sessions_body = sessions.json::<serde_json::Value>().await?;
    assert_eq!(sessions_body[0]["Id"], "shared-key-session");
    assert_eq!(sessions_body[0]["UserName"], "Target");

    let stop_session = client
        .post(format!(
            "http://{address}/Sessions/shared-key-session/Playing/Stop?api_key={key}"
        ))
        .send()
        .await?;
    assert_eq!(stop_session.status(), reqwest::StatusCode::NO_CONTENT);
    let session_state: String =
        sqlx::query_scalar("SELECT state FROM playback_sessions WHERE id = 'shared-key-session'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(session_state, "STOPPED");

    server.abort();
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn only_web_admins_can_manage_the_shared_key() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let users = UserStore::new(database.clone())?;
    users
        .create_initial_admin("Admin", "Administrator", "correct horse battery staple")
        .await?;
    users
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let setup = SetupService::new(database.clone())?;
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
    let client = reqwest::Client::new();

    let admin_login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({
            "username": "admin",
            "password": "correct horse battery staple"
        }))
        .send()
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(admin_login.headers(), "lux_session")?,
        cookie_value(admin_login.headers(), "lux_csrf")?
    );
    let csrf = cookie_value(admin_login.headers(), "lux_csrf")?;

    let initial = client
        .get(format!("http://{address}/api/v1/admin/api-key"))
        .header("Cookie", &admin_cookies)
        .send()
        .await?;
    assert_eq!(initial.status(), reqwest::StatusCode::OK);
    let initial_body = initial.json::<serde_json::Value>().await?;
    assert_eq!(initial_body["configured"], false);
    assert!(initial_body["apiKey"].is_null());

    let rotated = client
        .post(format!("http://{address}/api/v1/admin/api-key/rotate"))
        .header("Cookie", &admin_cookies)
        .header("X-CSRF-Token", &csrf)
        .send()
        .await?;
    assert_eq!(rotated.status(), reqwest::StatusCode::OK);
    let key = rotated.json::<serde_json::Value>().await?["apiKey"]
        .as_str()
        .ok_or("missing shared API key")?
        .to_owned();

    let listed = client
        .get(format!("http://{address}/api/v1/admin/api-key"))
        .header("Cookie", &admin_cookies)
        .send()
        .await?;
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    assert_eq!(listed.json::<serde_json::Value>().await?["apiKey"], key);

    let key_cannot_manage_itself = client
        .get(format!("http://{address}/api/v1/admin/api-key"))
        .header("X-Lux-Api-Key", &key)
        .send()
        .await?;
    assert_eq!(
        key_cannot_manage_itself.status(),
        reqwest::StatusCode::FORBIDDEN
    );

    let viewer_login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    assert_eq!(viewer_login.status(), reqwest::StatusCode::OK);
    let viewer_cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(viewer_login.headers(), "lux_session")?,
        cookie_value(viewer_login.headers(), "lux_csrf")?
    );
    let viewer_read = client
        .get(format!("http://{address}/api/v1/admin/api-key"))
        .header("Cookie", viewer_cookies)
        .send()
        .await?;
    assert_eq!(viewer_read.status(), reqwest::StatusCode::FORBIDDEN);

    let viewer_emby_login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            "Authorization",
            r#"Emby Client="LuxTest", Device="Mac", DeviceId="viewer-device", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    assert_eq!(viewer_emby_login.status(), reqwest::StatusCode::OK);
    let viewer_emby_token = viewer_emby_login.json::<serde_json::Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer Emby access token")?
        .to_owned();
    let viewer_virtual_folders = client
        .get(format!("http://{address}/Library/VirtualFolders"))
        .header("X-Emby-Token", &viewer_emby_token)
        .send()
        .await?;
    assert_eq!(
        viewer_virtual_folders.status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let viewer_selectable_media_folders = client
        .get(format!("http://{address}/Library/SelectableMediaFolders"))
        .header("X-Emby-Token", &viewer_emby_token)
        .send()
        .await?;
    assert_eq!(
        viewer_selectable_media_folders.status(),
        reqwest::StatusCode::FORBIDDEN
    );

    let viewer_admin_health = client
        .get(format!("http://{address}/api/v1/admin/health"))
        .header("X-Lux-Token", &viewer_emby_token)
        .send()
        .await?;
    assert_eq!(viewer_admin_health.status(), reqwest::StatusCode::FORBIDDEN);

    let viewer_api_key = client
        .get(format!("http://{address}/api/v1/admin/api-key"))
        .header("X-Lux-Token", &viewer_emby_token)
        .send()
        .await?;
    assert_eq!(viewer_api_key.status(), reqwest::StatusCode::FORBIDDEN);

    let revoked = client
        .delete(format!("http://{address}/api/v1/admin/api-key"))
        .header("Cookie", &admin_cookies)
        .header("X-CSRF-Token", &csrf)
        .send()
        .await?;
    assert_eq!(revoked.status(), reqwest::StatusCode::NO_CONTENT);

    server.abort();
    database.close().await;
    Ok(())
}

fn cookie_value(
    headers: &reqwest::header::HeaderMap,
    name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    headers
        .get_all("set-cookie")
        .iter()
        .find_map(|value| {
            let value = value.to_str().ok()?;
            let prefix = format!("{name}=");
            value
                .strip_prefix(&prefix)
                .and_then(|value| value.split(';').next())
                .map(str::to_owned)
        })
        .ok_or_else(|| format!("missing {name} cookie").into())
}
