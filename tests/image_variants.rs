//! LUX-826 image variant integration coverage.
//!
//! These tests exercise the real `AppState`, a temporary config directory and a
//! real JPEG poster so the generation path is covered end to end.

use std::time::Duration;

use image::{Rgb, RgbImage};
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
use reqwest::{
    StatusCode,
    header::{
        CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, ETAG, IF_NONE_MATCH, SET_COOKIE,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

struct VariantHarness {
    base_url: String,
    client: reqwest::Client,
    cookies: String,
    csrf: String,
    item_id: String,
    config_dir: std::path::PathBuf,
    database: Database,
    original_bytes: Vec<u8>,
    server: tokio::task::JoinHandle<()>,
    _temp_dir: tempfile::TempDir,
}

impl Drop for VariantHarness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn start_harness() -> Result<VariantHarness, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Variant Movie (2026)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Variant.Movie.2026.mkv"), b"movie").await?;
    let poster = RgbImage::from_pixel(1600, 2400, Rgb([40, 90, 160]));
    let poster_path = movie_dir.join("poster.jpg");
    poster.save(&poster_path)?;
    let original_bytes = tokio::fs::read(&poster_path).await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE sort_title = 'variant movie'")
            .fetch_one(database.pool())
            .await?;
    MetadataEnricher::new(database.clone())
        .enrich_movie_library(library.id)
        .await?;
    let content_tag = format!("{:x}", Sha256::digest(&original_bytes));
    sqlx::query(
        "UPDATE item_images SET width = 1600, height = 2400, content_tag = ?
         WHERE item_id = ? AND image_type = 'POSTER'",
    )
    .bind(&content_tag)
    .bind(&item_id)
    .execute(database.pool())
    .await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let session = cookie_value(login.headers(), "lux_session");
    let csrf = cookie_value(login.headers(), "lux_csrf");
    Ok(VariantHarness {
        base_url,
        client,
        cookies: format!("lux_session={session}; lux_csrf={csrf}"),
        csrf,
        item_id,
        config_dir: config.config_dir.clone(),
        database,
        original_bytes,
        server,
        _temp_dir: temp_dir,
    })
}

impl VariantHarness {
    async fn enable_server_switch(&self, enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
        let settings: Value = self
            .client
            .get(format!("{}/api/v1/admin/settings", self.base_url))
            .header(COOKIE, &self.cookies)
            .send()
            .await?
            .json()
            .await?;
        let mut strategy = settings["mediaStrategy"].clone();
        strategy["images"]["variantCache"] = json!({
            "enabled": enabled,
            "maxFiles": 20000,
            "maxBytes": 2147483648u64,
            "concurrency": 2,
        });
        let response = self
            .client
            .patch(format!("{}/api/v1/admin/settings", self.base_url))
            .header(COOKIE, &self.cookies)
            .header("X-CSRF-Token", &self.csrf)
            .json(&json!({ "mediaStrategy": strategy }))
            .send()
            .await?;
        if response.status() != StatusCode::OK {
            return Err(format!("admin settings update failed: {}", response.status()).into());
        }
        Ok(())
    }

    async fn set_user_preference(&self, optimized: bool) -> Result<(), Box<dyn std::error::Error>> {
        let preferences: Value = self
            .client
            .get(format!("{}/api/v1/auth/account-preferences", self.base_url))
            .header(COOKIE, &self.cookies)
            .send()
            .await?
            .json()
            .await?;
        let response = self
            .client
            .patch(format!("{}/api/v1/auth/account-preferences", self.base_url))
            .header(COOKIE, &self.cookies)
            .header("X-CSRF-Token", &self.csrf)
            .json(&json!({
                "expectedVersion": preferences["version"],
                "theme": preferences["theme"],
                "accentColor": preferences["accentColor"],
                "showMediaLibraries": preferences["showMediaLibraries"],
                "showContinueWatching": preferences["showContinueWatching"],
                "audioLanguage": preferences["audioLanguage"],
                "subtitleLanguage": preferences["subtitleLanguage"],
                "autoPlayNextEpisode": preferences["autoPlayNextEpisode"],
                "optimizedImages": optimized,
                "libraryOrder": preferences["libraryOrder"],
                "playedPercent": preferences["playedPercent"],
            }))
            .send()
            .await?;
        if response.status() != StatusCode::OK {
            return Err(format!("account preference update failed: {}", response.status()).into());
        }
        let body: Value = response.json().await?;
        assert_eq!(body["optimizedImages"], json!(optimized));
        Ok(())
    }

    fn image_url(&self, width: Option<u32>) -> String {
        match width {
            Some(width) => format!(
                "{}/api/v1/items/{}/images/poster?w={width}",
                self.base_url, self.item_id
            ),
            None => format!(
                "{}/api/v1/items/{}/images/poster",
                self.base_url, self.item_id
            ),
        }
    }

    async fn fetch(
        &self,
        width: Option<u32>,
    ) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
        Ok(self
            .client
            .get(self.image_url(width))
            .header(COOKIE, &self.cookies)
            .send()
            .await?)
    }

    /// The production response budget deliberately falls back to the original
    /// image when a cold generation takes too long. Debug-build encoding is
    /// slower than release, so integration coverage waits for the background
    /// task and then asserts on the cached variant response.
    async fn variant_response(&self) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
        for _ in 0..600 {
            let response = self.fetch(Some(360)).await?;
            if response.headers()[CACHE_CONTROL] == "private, max-age=31536000, immutable" {
                return Ok(response);
            }
            let _ = response.bytes().await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err("image variant was not generated within the test timeout".into())
    }
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

#[tokio::test]
async fn default_switch_off_serves_the_original_image_bytes()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    let response = harness.fetch(Some(360)).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CACHE_CONTROL], "private, max-age=3600");
    assert_eq!(
        response.bytes().await?.as_ref(),
        harness.original_bytes.as_slice()
    );
    assert!(!harness.config_dir.join("image-cache").exists());
    Ok(())
}

#[tokio::test]
async fn enabled_switch_with_user_preference_serves_a_smaller_immutable_variant()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;
    harness.set_user_preference(true).await?;

    let response = harness.variant_response().await?;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    let body = response.bytes().await?;
    assert!(
        body.len() < harness.original_bytes.len(),
        "variant must be smaller than the original"
    );
    assert_eq!(headers[CONTENT_TYPE], "image/jpeg");
    assert_eq!(headers[CONTENT_LENGTH], body.len().to_string().as_str());
    let etag = headers[ETAG].to_str()?;
    assert!(etag.ends_with("-w360\""), "unexpected etag {etag}");
    assert_eq!(
        headers[CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );
    assert_eq!(&body[..2], &[0xFF, 0xD8], "variant must be a JPEG");
    let decoded = image::load_from_memory(&body)?;
    assert_eq!(decoded.width().max(decoded.height()), 360);

    let shard = format!("{:02x}", Sha256::digest(harness.item_id.as_bytes())[0]);
    let tag = format!("{:x}", Sha256::digest(&harness.original_bytes));
    let cached = harness
        .config_dir
        .join("image-cache/v1")
        .join(shard)
        .join(&harness.item_id)
        .join(format!("{tag}-w360.jpg"));
    assert_eq!(tokio::fs::read(cached).await?, body);
    Ok(())
}

#[tokio::test]
async fn conditional_and_head_requests_follow_the_variant_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;
    harness.set_user_preference(true).await?;

    let first = harness.variant_response().await?;
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first.headers()[ETAG].to_str()?.to_owned();
    let length = first.headers()[CONTENT_LENGTH].to_str()?.to_owned();
    let _ = first.bytes().await?;

    let conditional = harness
        .client
        .get(harness.image_url(Some(360)))
        .header(COOKIE, &harness.cookies)
        .header(IF_NONE_MATCH, &etag)
        .send()
        .await?;
    assert_eq!(conditional.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(conditional.headers()[ETAG], etag.as_str());

    let head = harness
        .client
        .head(harness.image_url(Some(360)))
        .header(COOKIE, &harness.cookies)
        .send()
        .await?;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(head.headers()[ETAG], etag.as_str());
    assert_eq!(head.headers()[CONTENT_LENGTH], length.as_str());
    assert_eq!(
        head.headers()[CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );
    assert!(head.bytes().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn invalid_widths_are_rejected_only_while_the_switch_is_on()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    let disabled = harness.fetch(Some(999)).await?;
    assert_eq!(disabled.status(), StatusCode::OK);
    assert_eq!(
        disabled.bytes().await?.as_ref(),
        harness.original_bytes.as_slice()
    );
    let malformed = harness
        .client
        .get(format!(
            "{}/api/v1/items/{}/images/poster?w=abc",
            harness.base_url, harness.item_id
        ))
        .header(COOKIE, &harness.cookies)
        .send()
        .await?;
    assert_eq!(malformed.status(), StatusCode::OK);
    assert_eq!(
        malformed.bytes().await?.as_ref(),
        harness.original_bytes.as_slice()
    );

    harness.enable_server_switch(true).await?;
    harness.set_user_preference(true).await?;
    let enabled = harness.fetch(Some(999)).await?;
    assert_eq!(enabled.status(), StatusCode::BAD_REQUEST);
    let body: Value = enabled.json().await?;
    assert_eq!(body["error"]["code"], "INVALID_REQUEST");

    let malformed_enabled = harness
        .client
        .get(format!(
            "{}/api/v1/items/{}/images/poster?w=abc",
            harness.base_url, harness.item_id
        ))
        .header(COOKIE, &harness.cookies)
        .send()
        .await?;
    assert_eq!(malformed_enabled.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn user_preference_off_keeps_the_original_image() -> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;
    harness.set_user_preference(false).await?;

    let response = harness.fetch(Some(360)).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CACHE_CONTROL], "private, max-age=3600");
    assert_eq!(
        response.bytes().await?.as_ref(),
        harness.original_bytes.as_slice()
    );
    Ok(())
}

#[tokio::test]
async fn changing_the_content_tag_invalidates_the_previous_variant()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;
    harness.set_user_preference(true).await?;

    let first = harness.variant_response().await?;
    let first_etag = first.headers()[ETAG].to_str()?.to_owned();
    assert_eq!(first.status(), StatusCode::OK);

    let replacement = "9".repeat(64);
    sqlx::query(
        "UPDATE item_images SET content_tag = ? WHERE item_id = ? AND image_type = 'POSTER'",
    )
    .bind(&replacement)
    .bind(&harness.item_id)
    .execute(harness.database.pool())
    .await?;

    let second = harness.variant_response().await?;
    assert_eq!(second.status(), StatusCode::OK);
    let second_etag = second.headers()[ETAG].to_str()?.to_owned();
    assert_ne!(first_etag, second_etag);
    assert!(second_etag.starts_with(&format!("\"{replacement}-w360\"")));
    Ok(())
}

#[tokio::test]
async fn unwritable_cache_directory_falls_back_to_the_original()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;
    harness.set_user_preference(true).await?;

    let item_shard = format!("{:02x}", Sha256::digest(harness.item_id.as_bytes())[0]);
    let shard = harness.config_dir.join("image-cache/v1").join(item_shard);
    tokio::fs::create_dir_all(shard.parent().ok_or("missing cache parent")?).await?;
    // Occupy the shard path with a regular file so the directory cannot be created.
    tokio::fs::write(&shard, b"blocked").await?;

    let response = harness.fetch(Some(360)).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CACHE_CONTROL], "private, max-age=3600");
    assert_eq!(
        response.bytes().await?.as_ref(),
        harness.original_bytes.as_slice()
    );
    Ok(())
}

#[tokio::test]
async fn unauthenticated_image_requests_stay_unauthorized() -> Result<(), Box<dyn std::error::Error>>
{
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;
    let response = harness
        .client
        .get(harness.image_url(Some(360)))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn auth_me_advertises_the_effective_variant_capability()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    let initial: Value = harness
        .client
        .get(format!("{}/api/v1/auth/me", harness.base_url))
        .header(COOKIE, &harness.cookies)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(initial["imageVariantsAvailable"], json!(false));
    assert_eq!(initial["imageVariantsEnabled"], json!(false));

    harness.enable_server_switch(true).await?;
    let available_only: Value = harness
        .client
        .get(format!("{}/api/v1/auth/me", harness.base_url))
        .header(COOKIE, &harness.cookies)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(available_only["imageVariantsAvailable"], json!(true));
    assert_eq!(available_only["imageVariantsEnabled"], json!(false));

    harness.set_user_preference(true).await?;
    let both: Value = harness
        .client
        .get(format!("{}/api/v1/auth/me", harness.base_url))
        .header(COOKIE, &harness.cookies)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(both["imageVariantsAvailable"], json!(true));
    assert_eq!(both["imageVariantsEnabled"], json!(true));
    Ok(())
}
