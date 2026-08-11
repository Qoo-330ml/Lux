use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService, metadata::MetadataEnricher, scanner::LibraryScanner,
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, ETAG, IF_NONE_MATCH, SET_COOKIE, USER_AGENT};
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn lux_and_emby_image_endpoints_share_etag_and_reject_escape()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Image Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Image.Movie.2020.mkv"), b"movie").await?;
    tokio::fs::write(movie_dir.join("poster.jpg"), b"poster-bytes").await?;
    tokio::fs::write(movie_dir.join("clearlogo.png"), b"logo-bytes").await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE sort_title = 'image movie'")
            .fetch_one(database.pool())
            .await?;
    MetadataEnricher::new(database.clone())
        .enrich_movie_library(library.id)
        .await?;
    let image_id: String = sqlx::query_scalar(
        "SELECT id FROM item_images WHERE item_id = ? AND image_type = 'POSTER'",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    let logo_image_id: String =
        sqlx::query_scalar("SELECT id FROM item_images WHERE item_id = ? AND image_type = 'LOGO'")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;

    let outside_path = temp_dir.path().join("outside.jpg");
    tokio::fs::write(&outside_path, b"outside").await?;
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
            r#"Emby Client="LuxTest", Device="Mac", DeviceId="image-device", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let emby_token = emby_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();
    let item_detail = client
        .get(format!("{base_url}/Items/{item_id}"))
        .header("X-Emby-Token", &emby_token)
        .send()
        .await?;
    let item_detail_body: Value = item_detail.json().await?;
    assert_eq!(item_detail_body["ImageTags"]["Primary"], image_id);
    assert_eq!(item_detail_body["ImageTags"]["Logo"], logo_image_id);
    let web_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(web_login.headers(), "lux_session"),
        cookie_value(web_login.headers(), "lux_csrf")
    );

    let lux_image = client
        .get(format!("{base_url}/api/v1/items/{item_id}/images/poster"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(lux_image.status(), reqwest::StatusCode::OK);
    assert_eq!(
        lux_image.headers().get("content-type").unwrap(),
        "image/jpeg"
    );
    assert_eq!(lux_image.content_length(), Some(12));
    let etag = lux_image
        .headers()
        .get(ETAG)
        .ok_or("missing etag")?
        .to_str()?
        .to_owned();
    assert_eq!(lux_image.bytes().await?, "poster-bytes".as_bytes());

    let lux_head = client
        .head(format!("{base_url}/api/v1/items/{item_id}/images/poster"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(lux_head.status(), reqwest::StatusCode::OK);
    assert_eq!(lux_head.headers().get("content-length").unwrap(), "12");
    assert!(lux_head.bytes().await?.is_empty());

    let not_modified = client
        .get(format!("{base_url}/api/v1/items/{item_id}/images/poster"))
        .header(COOKIE, &cookies)
        .header(IF_NONE_MATCH, &etag)
        .send()
        .await?;
    assert_eq!(not_modified.status(), reqwest::StatusCode::NOT_MODIFIED);
    assert!(not_modified.bytes().await?.is_empty());

    let emby_image = client
        .get(format!(
            "{base_url}/Items/{item_id}/Images/Primary?api_key={emby_token}"
        ))
        .send()
        .await?;
    assert_eq!(emby_image.status(), reqwest::StatusCode::OK);
    assert_eq!(emby_image.headers().get(ETAG).unwrap(), etag.as_str());
    assert_eq!(emby_image.bytes().await?, "poster-bytes".as_bytes());

    let emby_mobile_image = client
        .get(format!(
            "{base_url}/emby/Items/{item_id}/Images/Primary/0?apiKey={emby_token}&tag={image_id}&maxWidth=600&maxHeight=900&quality=90"
        ))
        .send()
        .await?;
    assert_eq!(emby_mobile_image.status(), reqwest::StatusCode::OK);
    assert_eq!(
        emby_mobile_image.headers().get(ETAG).unwrap(),
        etag.as_str()
    );
    assert_eq!(emby_mobile_image.bytes().await?, "poster-bytes".as_bytes());

    let emby_anonymous_without_tag = client
        .get(format!("{base_url}/emby/Items/{item_id}/Images/Primary/0"))
        .send()
        .await?;
    assert_eq!(
        emby_anonymous_without_tag.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let emby_filmly_image = client
        .get(format!("{base_url}/emby/Items/{item_id}/Images/Primary"))
        .header(
            USER_AGENT,
            "%E7%BD%91%E6%98%93%E7%88%86%E7%B1%B3%E8%8A%B1/2.12.3-423",
        )
        .send()
        .await?;
    assert_eq!(emby_filmly_image.status(), reqwest::StatusCode::OK);
    assert_eq!(emby_filmly_image.bytes().await?, "poster-bytes".as_bytes());

    let emby_capability_image = client
        .get(format!(
            "{base_url}/emby/Items/{item_id}/Images/Primary/0?tag={image_id}"
        ))
        .send()
        .await?;
    assert_eq!(emby_capability_image.status(), reqwest::StatusCode::OK);
    assert_eq!(
        emby_capability_image.bytes().await?,
        "poster-bytes".as_bytes()
    );

    let emby_invalid_capability = client
        .get(format!(
            "{base_url}/emby/Items/{item_id}/Images/Primary/0?tag=not-the-image-tag"
        ))
        .send()
        .await?;
    assert_eq!(
        emby_invalid_capability.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let emby_authorization_image = client
        .get(format!("{base_url}/emby/Items/{item_id}/Images/Primary"))
        .header(
            "X-Emby-Authorization",
            format!(
                "MediaBrowser Client=\"VidHub\", Device=\"iPhone\", DeviceId=\"mobile-image-device\", Version=\"2.1.8\", Token=\"{emby_token}\""
            ),
        )
        .send()
        .await?;
    assert_eq!(emby_authorization_image.status(), reqwest::StatusCode::OK);
    assert_eq!(
        emby_authorization_image.headers().get(ETAG).unwrap(),
        etag.as_str()
    );
    assert_eq!(
        emby_authorization_image.bytes().await?,
        "poster-bytes".as_bytes()
    );

    let emby_bearer_image = client
        .get(format!("{base_url}/emby/Items/{item_id}/Images/Primary"))
        .header("Authorization", format!("Bearer {emby_token}"))
        .send()
        .await?;
    assert_eq!(emby_bearer_image.status(), reqwest::StatusCode::OK);
    assert_eq!(emby_bearer_image.bytes().await?, "poster-bytes".as_bytes());

    let emby_logo = client
        .get(format!("{base_url}/Items/{item_id}/Images/Logo"))
        .header("X-Emby-Token", &emby_token)
        .send()
        .await?;
    assert_eq!(emby_logo.status(), reqwest::StatusCode::OK);
    assert_eq!(
        emby_logo.headers().get("content-type").unwrap(),
        "image/png"
    );
    assert_eq!(emby_logo.bytes().await?, "logo-bytes".as_bytes());

    let missing = client
        .get(format!("{base_url}/Items/{item_id}/Images/Backdrop"))
        .header("X-Emby-Token", &emby_token)
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    sqlx::query("UPDATE item_images SET local_path = ?")
        .bind(outside_path.to_str().ok_or("non-utf8 path")?)
        .execute(database.pool())
        .await?;
    let forbidden = client
        .get(format!("{base_url}/Items/{item_id}/Images/Primary"))
        .header("X-Emby-Token", &emby_token)
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    sqlx::query("UPDATE item_images SET local_path = ? WHERE id = ?")
        .bind(
            movie_dir
                .join("poster.jpg")
                .to_str()
                .ok_or("non-utf8 path")?,
        )
        .bind(&image_id)
        .execute(database.pool())
        .await?;
    let admin_images = client
        .get(format!("{base_url}/api/v1/admin/items/{item_id}/images"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(admin_images.status(), reqwest::StatusCode::OK);
    let admin_images_body = admin_images.json::<Value>().await?;
    assert_eq!(
        admin_images_body["images"].as_array().map(Vec::len),
        Some(2)
    );
    let image_types = admin_images_body["images"]
        .as_array()
        .ok_or("missing admin image list")?
        .iter()
        .filter_map(|image| image["imageType"].as_str())
        .collect::<Vec<_>>();
    assert!(image_types.contains(&"POSTER"));
    assert!(image_types.contains(&"LOGO"));
    let deleted = client
        .delete(format!(
            "{base_url}/api/v1/admin/items/{item_id}/images/{image_id}"
        ))
        .header(COOKIE, &cookies)
        .header(
            "x-csrf-token",
            cookie_value(web_login.headers(), "lux_csrf"),
        )
        .send()
        .await?;
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(!movie_dir.join("poster.jpg").exists());
    let after_delete = client
        .get(format!("{base_url}/api/v1/items/{item_id}/images/poster"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(after_delete.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    assert_eq!(admin.display_name, "Admin");
    Ok(())
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
