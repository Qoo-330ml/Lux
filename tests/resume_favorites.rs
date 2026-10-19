use std::time::Duration;

use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        scanner::{LibraryScanner, ScanJobService},
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn emby_public_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
}

#[tokio::test]
async fn resume_thresholds_and_favorite_played_endpoints_share_user_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let unrelated_library = libraries
        .create_library("Unrelated", LibraryKind::Movie, false)
        .await?;
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, ?, 1)",
    )
    .bind(viewer.id.to_string())
    .bind(unrelated_library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE users SET library_access_mode = 'SELECTED' WHERE id = ?")
        .bind(viewer.id.to_string())
        .execute(database.pool())
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Eligible.Movie.2024.mkv"), b"eligible").await?;
    tokio::fs::write(root.join("Almost.Movie.2025.mkv"), b"almost").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let eligible_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE title = 'Eligible Movie'")
            .fetch_one(database.pool())
            .await?;
    let almost_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE title = 'Almost Movie'")
            .fetch_one(database.pool())
            .await?;
    let emby_library_id = emby_public_id(&library.id.to_string());
    let emby_eligible_id = emby_public_id(&eligible_id);
    let emby_almost_id = emby_public_id(&almost_id);
    sqlx::query(
        "UPDATE media_sources SET duration_ticks = 2000000000
         WHERE item_id IN (?, ?)",
    )
    .bind(&eligible_id)
    .bind(&almost_id)
    .execute(database.pool())
    .await?;
    let admin_id = admin.id.to_string();
    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, position_ticks, last_played_at)
         VALUES (?, ?, ?, unixepoch()), (?, ?, ?, unixepoch())",
    )
    .bind(&admin_id)
    .bind(&eligible_id)
    .bind(1_300_000_000_i64)
    .bind(&admin_id)
    .bind(&almost_id)
    .bind(1_900_000_000_i64)
    .execute(database.pool())
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
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="ResumeTest", Device="Mac", DeviceId="resume-admin", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();

    let resume = client
        .get(format!("{base_url}/Users/{admin_id}/Items/Resume"))
        .query(&[("api_key", token.as_str()), ("Limit", "10")])
        .send()
        .await?;
    assert_eq!(resume.status(), reqwest::StatusCode::OK);
    let resume_body = resume.json::<Value>().await?;
    assert_eq!(resume_body["TotalRecordCount"], 1);
    assert_eq!(resume_body["Items"][0]["Id"], emby_eligible_id);

    let filmly_resume = client
        .get(format!("{base_url}/emby/Users/{admin_id}/Items/Resume"))
        .query(&[("X-Emby-Token", token.as_str()), ("Limit", "10")])
        .send()
        .await?;
    assert_eq!(filmly_resume.status(), reqwest::StatusCode::OK);
    let filmly_resume_body = filmly_resume.json::<Value>().await?;
    assert_eq!(filmly_resume_body["TotalRecordCount"], 1);
    assert_eq!(filmly_resume_body["Items"][0]["Id"], emby_eligible_id);

    // iOS Filmly restores visibility using a JSON body after reading Resume.
    let original_state: (i64, i64, i64, i64, Option<i64>) = sqlx::query_as(
        "SELECT position_ticks, is_played, is_favorite, play_count, last_played_at
         FROM user_item_state WHERE user_id = ? AND item_id = ?",
    )
    .bind(&admin_id)
    .bind(&eligible_id)
    .fetch_one(database.pool())
    .await?;
    for prefix in ["/emby", ""] {
        let flag_url =
            format!("{base_url}{prefix}/Users/{admin_id}/Items/{emby_eligible_id}/HideFromResume");
        for use_query in [false, true] {
            for hidden in [true, true, false, false] {
                let mut request = client.post(&flag_url).header("X-Emby-Token", &token);
                request = if use_query {
                    request.query(&[("Hide", hidden.to_string())])
                } else {
                    request.json(&json!({ "Hide": hidden }))
                };
                let response = request.send().await?;
                assert_eq!(response.status(), reqwest::StatusCode::OK);
                let body = response.json::<Value>().await?;
                assert_eq!(body["HideFromResume"], hidden);
                assert_eq!(body["ItemId"], emby_eligible_id);
                assert_eq!(body["PlaybackPositionTicks"], 1_300_000_000_i64);
                assert_eq!(body["Played"], false);
                let resume = client
                    .get(format!("{base_url}{prefix}/Users/{admin_id}/Items/Resume"))
                    .header("X-Emby-Token", &token)
                    .send()
                    .await?
                    .json::<Value>()
                    .await?;
                assert_eq!(resume["TotalRecordCount"], if hidden { 0 } else { 1 });
                assert_eq!(
                    resume["Items"].as_array().map(Vec::len),
                    Some(if hidden { 0 } else { 1 })
                );
                // Empty pages must still count after hidden-item filtering.
                let empty_page = client
                    .get(format!("{base_url}{prefix}/Users/{admin_id}/Items/Resume"))
                    .header("X-Emby-Token", &token)
                    .query(&[("StartIndex", "100")])
                    .send()
                    .await?
                    .json::<Value>()
                    .await?;
                assert_eq!(empty_page["TotalRecordCount"], if hidden { 0 } else { 1 });
            }
        }
        for invalid_body in [
            json!({}),
            json!({"Hide": null}),
            json!({"Hide": "false"}),
            json!({"Hide": 0}),
        ] {
            let response = client
                .post(&flag_url)
                .header("X-Emby-Token", &token)
                .json(&invalid_body)
                .send()
                .await?;
            assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        }
        for invalid_query in ["", "Hide=", "Hide=0", "Hide=no", "Hide=true&Hide=false"] {
            let response = client
                .post(format!("{flag_url}?{invalid_query}"))
                .header("X-Emby-Token", &token)
                .send()
                .await?;
            assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        }
        let conflicting = client
            .post(&flag_url)
            .header("X-Emby-Token", &token)
            .query(&[("Hide", "true")])
            .json(&json!({"Hide": false}))
            .send()
            .await?;
        assert_eq!(conflicting.status(), reqwest::StatusCode::BAD_REQUEST);
        let anonymous = client
            .post(&flag_url)
            .json(&json!({"Hide": true}))
            .send()
            .await?;
        assert_eq!(anonymous.status(), reqwest::StatusCode::UNAUTHORIZED);
        let missing_item = client
            .post(format!(
                "{base_url}{prefix}/Users/{admin_id}/Items/{}/HideFromResume",
                uuid::Uuid::now_v7()
            ))
            .header("X-Emby-Token", &token)
            .json(&json!({"Hide": true}))
            .send()
            .await?;
        assert_eq!(missing_item.status(), reqwest::StatusCode::NOT_FOUND);
    }
    let final_state: (i64, i64, i64, i64, Option<i64>) = sqlx::query_as(
        "SELECT position_ticks, is_played, is_favorite, play_count, last_played_at
         FROM user_item_state WHERE user_id = ? AND item_id = ?",
    )
    .bind(&admin_id)
    .bind(&eligible_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(final_state, original_state);

    let web_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let session = cookie_value(&web_login, "lux_session")?;
    let csrf = cookie_value(&web_login, "lux_csrf")?;
    for hidden in [true, false] {
        let response = client
            .post(format!(
                "{base_url}/emby/Users/{admin_id}/Items/{eligible_id}/HideFromResume"
            ))
            .query(&[("api_key", token.as_str())])
            .json(&json!({"Hide": hidden}))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let web_resume = client
            .get(format!(
                "{base_url}/api/v1/continue-watching?page=1&pageSize=10"
            ))
            .header(COOKIE, format!("lux_session={session}"))
            .send()
            .await?
            .json::<Value>()
            .await?;
        assert_eq!(web_resume["total"], if hidden { 0 } else { 1 });
        let detail = client
            .get(format!(
                "{base_url}/Users/{admin_id}/Items/{emby_eligible_id}"
            ))
            .header("X-Emby-Token", &token)
            .send()
            .await?
            .json::<Value>()
            .await?;
        assert_eq!(detail["UserData"]["HideFromResume"], hidden);
    }
    let settings = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, format!("lux_session={session}; lux_csrf={csrf}"))
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "resumePlayedPercent": 95, "resumeMinTicks": 0 }))
        .send()
        .await?;
    assert_eq!(settings.status(), reqwest::StatusCode::OK);
    let personal_settings = client
        .patch(format!("{base_url}/api/v1/auth/settings"))
        .header(COOKIE, format!("lux_session={session}; lux_csrf={csrf}"))
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "playedPercent": 100 }))
        .send()
        .await?;
    assert_eq!(personal_settings.status(), reqwest::StatusCode::OK);
    let default_settings = client
        .get(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, format!("lux_session={session}; lux_csrf={csrf}"))
        .send()
        .await?;
    assert_eq!(default_settings.status(), reqwest::StatusCode::OK);
    let default_settings_body = default_settings.json::<Value>().await?;
    assert_eq!(
        default_settings_body["mediaStrategy"]["images"]["poster"],
        true
    );
    assert_eq!(
        default_settings_body["mediaStrategy"]["images"]["minDownloadWidth"],
        1280
    );
    assert_eq!(
        default_settings_body["mediaStrategy"]["images"]["disc"],
        false
    );
    assert_eq!(
        default_settings_body["mediaStrategy"]["images"]["thumbnailScrapingMode"],
        "SCRAPER_FIRST"
    );
    assert_eq!(
        default_settings_body["mediaStrategy"]["metadataRefreshMode"],
        "FILL_MISSING"
    );
    assert_eq!(default_settings_body["networkProxy"]["configured"], false);
    assert_eq!(default_settings_body["networkProxy"]["source"], "none");

    let network_proxy = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, format!("lux_session={session}; lux_csrf={csrf}"))
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "networkProxyUrl": "socks5h://127.0.0.1:1080" }))
        .send()
        .await?;
    assert_eq!(network_proxy.status(), reqwest::StatusCode::OK);
    let network_proxy_body = network_proxy.json::<Value>().await?;
    assert_eq!(network_proxy_body["networkProxy"]["configured"], true);
    assert_eq!(network_proxy_body["networkProxy"]["source"], "settings");
    assert_eq!(
        network_proxy_body["networkProxy"]["url"],
        "socks5h://127.0.0.1:1080"
    );
    assert_eq!(network_proxy_body["networkProxy"]["restartRequired"], true);

    let credentialed_proxy = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, format!("lux_session={session}; lux_csrf={csrf}"))
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "networkProxyUrl": "http://proxy-user@127.0.0.1:7890" }))
        .send()
        .await?;
    let credentialed_proxy_body = credentialed_proxy.text().await?;
    assert!(credentialed_proxy_body.contains("http://127.0.0.1:7890/"));
    assert!(!credentialed_proxy_body.contains("proxy-user"));

    let clear_proxy = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, format!("lux_session={session}; lux_csrf={csrf}"))
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "networkProxyUrl": null }))
        .send()
        .await?;
    assert_eq!(clear_proxy.status(), reqwest::StatusCode::OK);
    let cleared_proxy_body = clear_proxy.json::<Value>().await?;
    assert_eq!(cleared_proxy_body["networkProxy"]["configured"], false);

    let invalid_proxy_test = client
        .post(format!(
            "{base_url}/api/v1/admin/settings/network-proxy/test"
        ))
        .header(COOKIE, format!("lux_session={session}; lux_csrf={csrf}"))
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "networkProxyUrl": "ftp://proxy.invalid:7890" }))
        .send()
        .await?;
    assert_eq!(
        invalid_proxy_test.status(),
        reqwest::StatusCode::BAD_REQUEST
    );

    let media_strategy = json!({
        "metadataLanguage": "en-US",
        "imageLanguage": "",
        "region": "US",
        "scraperId": null,
        "applyScope": "ALL_CONTENT",
        "images": {
            "poster": true,
            "artwork": false,
            "banner": true,
            "logo": false,
            "thumbnail": false,
            "disc": false,
            "wallpaper": true,
            "maxBackdropCount": 2,
            "minDownloadWidth": 1920
        },
        "subtitles": {
            "autoDownload": true,
            "languages": ["en", "zh-CN"],
            "forcedOnly": false,
            "hearingImpaired": true
        }
    });
    let updated_settings = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, format!("lux_session={session}; lux_csrf={csrf}"))
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "mediaStrategy": media_strategy }))
        .send()
        .await?;
    assert_eq!(updated_settings.status(), reqwest::StatusCode::OK);
    let updated_settings_body = updated_settings.json::<Value>().await?;
    assert_eq!(updated_settings_body["mediaStrategy"]["region"], "US");
    assert_eq!(
        updated_settings_body["mediaStrategy"]["applyScope"],
        "ALL_CONTENT"
    );
    assert_eq!(
        updated_settings_body["mediaStrategy"]["images"]["minDownloadWidth"],
        1920
    );
    assert_eq!(
        updated_settings_body["mediaStrategy"]["metadataRefreshMode"],
        "FILL_MISSING"
    );

    let persisted_settings = client
        .get(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, format!("lux_session={session}; lux_csrf={csrf}"))
        .send()
        .await?;
    let persisted_settings_body = persisted_settings.json::<Value>().await?;
    assert_eq!(
        persisted_settings_body["mediaStrategy"]["subtitles"]["hearingImpaired"],
        true
    );
    assert_eq!(
        persisted_settings_body["mediaStrategy"]["imageLanguage"],
        ""
    );

    let invalid_media_strategy = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, format!("lux_session={session}; lux_csrf={csrf}"))
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "mediaStrategy": {
                "metadataLanguage": "en-US",
                "imageLanguage": "en",
                "region": "US",
                "scraperId": null,
                "applyScope": "ALL_CONTENT",
                "images": {
                    "poster": true,
                    "artwork": false,
                    "banner": true,
                    "logo": false,
                    "thumbnail": false,
                    "thumbnailScrapingMode": "UNKNOWN",
                    "disc": false,
                    "wallpaper": true,
                    "maxBackdropCount": 2,
                    "minDownloadWidth": 1920
                },
                "subtitles": {
                    "autoDownload": true,
                    "languages": ["en"],
                    "forcedOnly": false,
                    "hearingImpaired": true
                }
            }
        }))
        .send()
        .await?;
    assert_eq!(
        invalid_media_strategy.status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let invalid_media_strategy_body = invalid_media_strategy.json::<Value>().await?;
    assert_eq!(
        invalid_media_strategy_body["error"]["code"],
        "INVALID_REQUEST"
    );
    assert_eq!(
        invalid_media_strategy_body["error"]["message"],
        "全局媒体策略无效"
    );
    let relaxed_resume = client
        .get(format!("{base_url}/Users/{admin_id}/Items/Resume"))
        .query(&[("api_key", token.as_str()), ("Limit", "10")])
        .send()
        .await?;
    let relaxed_body = relaxed_resume.json::<Value>().await?;
    assert_eq!(relaxed_body["TotalRecordCount"], 2);
    assert_eq!(relaxed_body["Items"][0]["Id"], emby_almost_id);
    assert_eq!(relaxed_body["Items"][1]["Id"], emby_eligible_id);

    let played = client
        .post(format!(
            "{base_url}/Users/{admin_id}/PlayedItems/{emby_eligible_id}"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(played.status(), reqwest::StatusCode::OK);
    let played_body = played.json::<Value>().await?;
    assert_eq!(played_body["ItemId"], emby_eligible_id);
    assert_eq!(played_body["Played"], true);
    assert_eq!(played_body["IsFavorite"], false);
    assert_eq!(played_body["PlayCount"], 1);
    let played_again = client
        .post(format!(
            "{base_url}/Users/{admin_id}/PlayedItems/{emby_eligible_id}"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(played_again.status(), reqwest::StatusCode::OK);
    let played_again_body = played_again.json::<Value>().await?;
    assert_eq!(played_again_body["ItemId"], emby_eligible_id);
    assert_eq!(played_again_body["Played"], true);
    assert_eq!(played_again_body["PlayCount"], 1);
    let favorite = client
        .post(format!(
            "{base_url}/Users/{admin_id}/FavoriteItems/{emby_eligible_id}"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(favorite.status(), reqwest::StatusCode::OK);
    let favorite_body = favorite.json::<Value>().await?;
    assert_eq!(favorite_body["ItemId"], emby_eligible_id);
    assert_eq!(favorite_body["Played"], true);
    assert_eq!(favorite_body["IsFavorite"], true);
    let favorites = client
        .get(format!("{base_url}/Users/{admin_id}/FavoriteItems"))
        .query(&[
            ("api_key", token.as_str()),
            ("StartIndex", "0"),
            ("Limit", "10"),
        ])
        .send()
        .await?;
    assert_eq!(favorites.status(), reqwest::StatusCode::OK);
    let favorites_body = favorites.json::<Value>().await?;
    assert_eq!(favorites_body["TotalRecordCount"], 1);
    assert_eq!(favorites_body["Items"][0]["Id"], emby_eligible_id);

    let filmly_favorites = client
        .get(format!("{base_url}/emby/Users/{admin_id}/FavoriteItems"))
        .query(&[("X-Emby-Token", token.as_str()), ("Limit", "10")])
        .send()
        .await?;
    assert_eq!(filmly_favorites.status(), reqwest::StatusCode::OK);
    let filmly_favorites_body = filmly_favorites.json::<Value>().await?;
    assert_eq!(filmly_favorites_body["TotalRecordCount"], 1);
    assert_eq!(filmly_favorites_body["Items"][0]["Id"], emby_eligible_id);

    let detail = client
        .get(format!("{base_url}/Items/{emby_eligible_id}"))
        .header("X-Emby-Token", &token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert_eq!(detail["UserData"]["Played"], true);
    assert_eq!(detail["UserData"]["PlayCount"], 1);
    assert_eq!(detail["UserData"]["IsFavorite"], true);
    let filtered = client
        .get(format!("{base_url}/Users/{admin_id}/Items"))
        .query(&[
            ("api_key", token.as_str()),
            ("ParentId", emby_library_id.as_str()),
            ("IncludeItemTypes", "Movie"),
            ("IsPlayed", "true"),
            ("IsFavorite", "true"),
        ])
        .send()
        .await?;
    assert_eq!(filtered.status(), reqwest::StatusCode::OK);
    assert_eq!(filtered.json::<Value>().await?["TotalRecordCount"], 1);

    let unplayed = client
        .delete(format!(
            "{base_url}/Users/{admin_id}/PlayedItems/{emby_eligible_id}"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(unplayed.status(), reqwest::StatusCode::OK);
    let unplayed_body = unplayed.json::<Value>().await?;
    assert_eq!(unplayed_body["ItemId"], emby_eligible_id);
    assert_eq!(unplayed_body["Played"], false);
    assert_eq!(unplayed_body["IsFavorite"], true);
    let unfavorite = client
        .delete(format!(
            "{base_url}/Users/{admin_id}/FavoriteItems/{emby_eligible_id}"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(unfavorite.status(), reqwest::StatusCode::OK);
    let unfavorite_body = unfavorite.json::<Value>().await?;
    assert_eq!(unfavorite_body["ItemId"], emby_eligible_id);
    assert_eq!(unfavorite_body["Played"], false);
    assert_eq!(unfavorite_body["IsFavorite"], false);

    let viewer_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="ResumeTest", Device="Mac", DeviceId="resume-viewer", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    let viewer_token = viewer_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    for (target_user, expected) in [
        (admin_id.clone(), reqwest::StatusCode::FORBIDDEN),
        (viewer.id.to_string(), reqwest::StatusCode::NOT_FOUND),
    ] {
        let response = client
            .post(format!(
                "{base_url}/emby/Users/{target_user}/Items/{emby_eligible_id}/HideFromResume"
            ))
            .header("X-Emby-Token", &viewer_token)
            .json(&json!({"Hide": true}))
            .send()
            .await?;
        assert_eq!(response.status(), expected);
    }
    let denied_favorites = client
        .get(format!("{base_url}/Users/{admin_id}/FavoriteItems"))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(denied_favorites.status(), reqwest::StatusCode::FORBIDDEN);
    let denied = client
        .post(format!(
            "{base_url}/Users/{}/FavoriteItems/{emby_eligible_id}",
            viewer.id
        ))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn resume_page_does_not_materialize_unrelated_catalog_items()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let library = LibraryService::new(database.clone())
        .create_library("Large catalog", LibraryKind::Movie, false)
        .await?;

    sqlx::query(
        "WITH RECURSIVE sequence(value) AS (
             SELECT 1
             UNION ALL
             SELECT value + 1 FROM sequence WHERE value < 20000
         )
         INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, runtime_ticks,
             identification_status, has_available_source
         )
         SELECT printf('bulk-%05d', value), ?, 'MOVIE',
                printf('Bulk %05d', value), printf('Bulk %05d', value),
                2000000000, 'LOCAL_CONFIRMED', 1
         FROM sequence",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    let admin_id = admin.id.to_string();
    sqlx::query(
        "INSERT INTO user_item_state (
             user_id, item_id, position_ticks, last_played_at
         ) VALUES (?, 'bulk-20000', 1300000000, unixepoch())",
    )
    .bind(&admin_id)
    .execute(database.pool())
    .await?;

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="ResumeTest", Device="Mac", DeviceId="resume-large", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();

    let resume = tokio::time::timeout(Duration::from_secs(1), async {
        client
            .get(format!("{base_url}/Users/{admin_id}/Items/Resume"))
            .query(&[
                ("api_key", token.as_str()),
                ("StartIndex", "0"),
                ("Limit", "1"),
            ])
            .send()
            .await
    })
    .await
    .map_err(|_| "Resume request materialized the unrelated catalog")??;
    assert_eq!(resume.status(), reqwest::StatusCode::OK);
    let body = resume.json::<Value>().await?;
    assert_eq!(body["TotalRecordCount"], 1);
    assert_eq!(body["StartIndex"], 0);
    assert_eq!(body["Items"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["Items"][0]["Id"], "bulk-20000");

    server.abort();
    Ok(())
}

#[tokio::test]
async fn homevideo_playback_progress_and_played_state_flow_into_continue_watching()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("admin", "Admin", "correct password").await?;
    let library = LibraryService::new(database.clone())
        .create_library("Other videos", LibraryKind::HomeVideos, false)
        .await?;
    let root = temp_dir.path().join("Other videos");
    tokio::fs::create_dir_all(root.join("Clips")).await?;
    tokio::fs::write(root.join("Clips/Family Trip.mkv"), b"video bytes").await?;
    LibraryService::new(database.clone())
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;
    let video_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'VIDEO'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    sqlx::query("UPDATE media_items SET runtime_ticks = 2000000000 WHERE id = ?")
        .bind(&video_id)
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES ('resume_min_ticks', '0')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .execute(database.pool())
    .await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let emby_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="ResumeTest", Device="Mac", DeviceId="resume-homevideos", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(emby_login.status(), reqwest::StatusCode::OK);
    let emby_token = emby_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing Emby token")?
        .to_owned();
    let media_source_id: String =
        sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
            .bind(&video_id)
            .fetch_one(database.pool())
            .await?;
    let emby_progress = client
        .post(format!("{base_url}/Sessions/Playing/Progress"))
        .header("X-Emby-Token", &emby_token)
        .json(&json!({
            "ItemId": emby_public_id(&video_id),
            "MediaSourceId": media_source_id,
            "PlaySessionId": "homevideos-resume-session",
            "PositionTicks": 1_000_000_000_i64,
            "RunTimeTicks": 2_000_000_000_i64
        }))
        .send()
        .await?;
    assert_eq!(emby_progress.status(), reqwest::StatusCode::NO_CONTENT);

    let emby_resume = client
        .get(format!("{base_url}/Users/{}/Items/Resume", admin.id))
        .query(&[("api_key", emby_token.as_str()), ("Limit", "10")])
        .send()
        .await?;
    assert_eq!(emby_resume.status(), reqwest::StatusCode::OK);
    let emby_resume_body = emby_resume.json::<Value>().await?;
    assert_eq!(emby_resume_body["TotalRecordCount"], 1);
    assert_eq!(
        emby_resume_body["Items"][0]["Id"],
        emby_public_id(&video_id)
    );
    assert_eq!(emby_resume_body["Items"][0]["Type"], "Video");

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session = cookie_value(&login, "lux_session")?;
    let csrf = cookie_value(&login, "lux_csrf")?;
    let cookie = format!("lux_session={session}; lux_csrf={csrf}");

    let progress = client
        .post(format!("{base_url}/api/v1/items/{video_id}/progress"))
        .header(COOKIE, &cookie)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({
            "positionTicks": 1_000_000_000_i64,
            "durationTicks": 2_000_000_000_i64,
            "state": "PLAYING"
        }))
        .send()
        .await?;
    assert_eq!(progress.status(), reqwest::StatusCode::NO_CONTENT);

    let home = client
        .get(format!("{base_url}/api/v1/home"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(home.status(), reqwest::StatusCode::OK);
    let home_body = home.json::<Value>().await?;
    assert_eq!(home_body["continueWatchingTotal"], 1);
    assert_eq!(home_body["continueWatching"][0]["id"], video_id);
    assert_eq!(home_body["continueWatching"][0]["itemType"], "VIDEO");

    let carousel = client
        .get(format!("{base_url}/api/v1/home/carousel"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(carousel.status(), reqwest::StatusCode::OK);
    let carousel_body = carousel.json::<Value>().await?;
    assert!(carousel_body["recommended"].is_array());
    assert!(carousel_body.get("continueWatching").is_none());
    assert!(carousel_body.get("libraries").is_none());

    let continue_watching = client
        .get(format!(
            "{base_url}/api/v1/continue-watching?page=1&pageSize=10"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(continue_watching.status(), reqwest::StatusCode::OK);
    let continue_watching_body = continue_watching.json::<Value>().await?;
    assert_eq!(continue_watching_body["total"], 1);
    assert_eq!(continue_watching_body["items"][0]["id"], video_id);
    assert_eq!(continue_watching_body["items"][0]["itemType"], "VIDEO");

    let played = client
        .put(format!("{base_url}/api/v1/items/{video_id}/played"))
        .header(COOKIE, &cookie)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "played": true }))
        .send()
        .await?;
    assert_eq!(played.status(), reqwest::StatusCode::NO_CONTENT);
    let detail = client
        .get(format!("{base_url}/api/v1/items/{video_id}"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(detail.status(), reqwest::StatusCode::OK);
    assert_eq!(detail.json::<Value>().await?["userData"]["isPlayed"], true);
    let home_after_played = client
        .get(format!("{base_url}/api/v1/home"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(home_after_played.status(), reqwest::StatusCode::OK);
    let home_after_played_body = home_after_played.json::<Value>().await?;
    assert_eq!(home_after_played_body["continueWatchingTotal"], 0);
    assert_eq!(
        home_after_played_body["continueWatching"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );
    let continue_watching_after_played = client
        .get(format!(
            "{base_url}/api/v1/continue-watching?page=1&pageSize=10"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(
        continue_watching_after_played.status(),
        reqwest::StatusCode::OK
    );
    let continue_watching_after_played_body =
        continue_watching_after_played.json::<Value>().await?;
    assert_eq!(continue_watching_after_played_body["total"], 0);
    assert_eq!(
        continue_watching_after_played_body["items"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    server.abort();
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn home_continue_watching_advances_to_next_unplayed_episode_after_completion()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let library = LibraryService::new(database.clone())
        .create_library("Series", LibraryKind::Series, false)
        .await?;
    let admin_id = admin.id.to_string();
    let series_id = uuid::Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES (?, ?, 'SERIES', 'Continue Series', 'continue series', 'LOCAL_CONFIRMED')",
    )
    .bind(&series_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    let mut episode_ids = Vec::new();
    for (episode_number, title) in [
        (1_i64, "Continue Series S01E01"),
        (2, "Continue Series S01E02"),
    ] {
        let item_id = uuid::Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, series_id, season_number,
                 episode_number, title, sort_title, runtime_ticks,
                 identification_status, has_available_source
             ) VALUES (?, ?, 'EPISODE', ?, 1, ?, ?, ?, 1000000000, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(&item_id)
        .bind(library.id.to_string())
        .bind(&series_id)
        .bind(episode_number)
        .bind(title)
        .bind(title.to_lowercase())
        .execute(database.pool())
        .await?;
        episode_ids.push(item_id);
    }
    sqlx::query(
        "INSERT INTO user_item_state (
             user_id, item_id, position_ticks, is_played, last_played_at
         ) VALUES (?, ?, 960000000, 1, 100)",
    )
    .bind(&admin_id)
    .bind(&episode_ids[0])
    .execute(database.pool())
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
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let web_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let session = cookie_value(&web_login, "lux_session")?;
    let home_resume = client
        .get(format!(
            "{base_url}/api/v1/continue-watching?page=1&pageSize=10"
        ))
        .header(COOKIE, format!("lux_session={session}"))
        .send()
        .await?;
    assert_eq!(home_resume.status(), reqwest::StatusCode::OK);
    let home_resume_body = home_resume.json::<Value>().await?;
    assert_eq!(home_resume_body["total"], 1);
    assert_eq!(home_resume_body["items"][0]["id"], episode_ids[1]);
    let home_resume_empty_page = client
        .get(format!("{base_url}/api/v1/continue-watching"))
        .query(&[("page", "2"), ("pageSize", "1")])
        .header(COOKIE, format!("lux_session={session}"))
        .send()
        .await?;
    let home_resume_empty_page_body = home_resume_empty_page.json::<Value>().await?;
    assert_eq!(home_resume_empty_page_body["total"], 1);
    assert_eq!(
        home_resume_empty_page_body["items"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    let emby_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="ResumeTest", Device="Mac", DeviceId="resume-next-up", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let emby_token = emby_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing Emby token")?
        .to_owned();
    for path in [
        format!("/Users/{admin_id}/Items/Resume"),
        format!("/emby/Users/{admin_id}/Items/Resume"),
    ] {
        let emby_resume = client
            .get(format!("{base_url}{path}"))
            .query(&[("api_key", emby_token.as_str()), ("Limit", "10")])
            .send()
            .await?;
        assert_eq!(emby_resume.status(), reqwest::StatusCode::OK);
        let emby_resume_body = emby_resume.json::<Value>().await?;
        assert_eq!(emby_resume_body["TotalRecordCount"], 1);
        assert_eq!(
            emby_resume_body["Items"][0]["Id"],
            emby_public_id(&episode_ids[1])
        );
        assert_eq!(
            emby_resume_body["Items"][0]["UserData"]["PlaybackPositionTicks"],
            0
        );
        assert_eq!(emby_resume_body["Items"][0]["UserData"]["Played"], false);
    }

    server.abort();
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn emby_resume_keeps_only_latest_eligible_episode_per_series()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let library = LibraryService::new(database.clone())
        .create_library("Series", LibraryKind::Series, false)
        .await?;
    let admin_id = admin.id.to_string();
    let series_id = uuid::Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES (?, ?, 'SERIES', 'Resume Series', 'resume series', 'LOCAL_CONFIRMED')",
    )
    .bind(&series_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    let episodes = [
        (1_i64, 31_i64, "Resume Series S01E31", 300_i64),
        (1, 32, "Resume Series S01E32", 400),
        (2, 1, "Resume Series S02E01", 200),
    ];
    let mut episode_ids = Vec::new();
    for (season_number, episode_number, title, last_played_at) in episodes {
        let item_id = uuid::Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, series_id, season_number,
                 episode_number, title, sort_title, runtime_ticks,
                 identification_status, has_available_source
             ) VALUES (?, ?, 'EPISODE', ?, ?, ?, ?, ?, 36000000000, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(&item_id)
        .bind(library.id.to_string())
        .bind(&series_id)
        .bind(season_number)
        .bind(episode_number)
        .bind(title)
        .bind(title.to_lowercase())
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO user_item_state (
                 user_id, item_id, position_ticks, last_played_at
             ) VALUES (?, ?, 6000000000, ?)",
        )
        .bind(&admin_id)
        .bind(&item_id)
        .bind(last_played_at)
        .execute(database.pool())
        .await?;
        episode_ids.push(item_id);
    }

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="ResumeTest", Device="Mac", DeviceId="resume-series", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();

    for path in [
        format!("/Users/{admin_id}/Items/Resume"),
        format!("/emby/Users/{admin_id}/Items/Resume"),
    ] {
        let response = client
            .get(format!("{base_url}{path}"))
            .query(&[("api_key", token.as_str()), ("Limit", "10")])
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let body = response.json::<Value>().await?;
        assert_eq!(body["TotalRecordCount"], 1);
        assert_eq!(body["Items"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["Items"][0]["Id"], emby_public_id(&episode_ids[2]));
    }

    server.abort();
    Ok(())
}

fn cookie_value(
    response: &reqwest::Response,
    name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .find_map(|value| {
            let value = value.to_str().ok()?;
            let value = value.strip_prefix(&format!("{name}="))?;
            Some(value.split(';').next()?.to_owned())
        })
        .ok_or_else(|| format!("missing {name} cookie").into())
}
