use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

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
         VALUES (?, ?, ?, unixepoch() - 1), (?, ?, ?, unixepoch())",
    )
    .bind(&admin_id)
    .bind(&eligible_id)
    .bind(1_300_000_000_i64)
    .bind(&admin_id)
    .bind(&almost_id)
    .bind(1_850_000_000_i64)
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
    assert_eq!(resume_body["Items"][0]["Id"], eligible_id);

    let web_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let session = cookie_value(&web_login, "lux_session")?;
    let csrf = cookie_value(&web_login, "lux_csrf")?;
    let settings = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, format!("lux_session={session}; lux_csrf={csrf}"))
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "resumePlayedPercent": 95, "resumeMinTicks": 0 }))
        .send()
        .await?;
    assert_eq!(settings.status(), reqwest::StatusCode::OK);
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

    let media_strategy = json!({
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
    let relaxed_resume = client
        .get(format!("{base_url}/Users/{admin_id}/Items/Resume"))
        .query(&[("api_key", token.as_str()), ("Limit", "10")])
        .send()
        .await?;
    let relaxed_body = relaxed_resume.json::<Value>().await?;
    assert_eq!(relaxed_body["TotalRecordCount"], 2);

    let played = client
        .post(format!(
            "{base_url}/Users/{admin_id}/PlayedItems/{eligible_id}"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(played.status(), reqwest::StatusCode::NO_CONTENT);
    let played_again = client
        .post(format!(
            "{base_url}/Users/{admin_id}/PlayedItems/{eligible_id}"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(played_again.status(), reqwest::StatusCode::NO_CONTENT);
    let favorite = client
        .post(format!(
            "{base_url}/Users/{admin_id}/FavoriteItems/{eligible_id}"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(favorite.status(), reqwest::StatusCode::NO_CONTENT);
    let detail = client
        .get(format!("{base_url}/Items/{eligible_id}"))
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
            ("ParentId", library.id.to_string().as_str()),
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
            "{base_url}/Users/{admin_id}/PlayedItems/{eligible_id}"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(unplayed.status(), reqwest::StatusCode::NO_CONTENT);
    let unfavorite = client
        .delete(format!(
            "{base_url}/Users/{admin_id}/FavoriteItems/{eligible_id}"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(unfavorite.status(), reqwest::StatusCode::NO_CONTENT);

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
    let denied = client
        .post(format!(
            "{base_url}/Users/{}/FavoriteItems/{eligible_id}",
            viewer.id
        ))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::NOT_FOUND);

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
