//! LUX-826 image variant integration coverage.
//!
//! These tests exercise the real `AppState`, a temporary config directory and a
//! real JPEG poster so the generation path is covered end to end.

use std::{
    io::{self, Write},
    sync::{Arc, LazyLock, Mutex, Once},
    time::Duration,
};

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
        AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, ETAG, IF_NONE_MATCH,
        SET_COOKIE,
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
    poster_tag: String,
    viewer_id: String,
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

    let poster_tag: String = sqlx::query_scalar(
        "SELECT id FROM item_images WHERE item_id = ? AND image_type = 'POSTER'",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    // A user without library access exercises the Emby authorization order.
    let viewer_id = luxd::auth::users::UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?
        .id
        .to_string();

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
        .no_proxy()
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
        poster_tag,
        viewer_id,
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

    async fn save_account_preferences(&self) -> Result<(), Box<dyn std::error::Error>> {
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
                "libraryOrder": preferences["libraryOrder"],
                "playedPercent": preferences["playedPercent"],
            }))
            .send()
            .await?;
        if response.status() != StatusCode::OK {
            return Err(format!("account preference update failed: {}", response.status()).into());
        }
        let body: Value = response.json().await?;
        assert!(body.get("optimizedImages").is_none());
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

    async fn emby_token(
        &self,
        username: &str,
        password: &str,
        device_id: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let response = self
            .client
            .post(format!("{}/Users/AuthenticateByName", self.base_url))
            .header(
                AUTHORIZATION,
                format!(
                    "Emby Client=\"LuxTest\", Device=\"Mac\", DeviceId=\"{device_id}\", Version=\"1\""
                ),
            )
            .json(&json!({ "Username": username, "Pw": password }))
            .send()
            .await?;
        let body: Value = response.json().await?;
        Ok(body["AccessToken"]
            .as_str()
            .ok_or("missing Emby access token")?
            .to_owned())
    }

    fn emby_image_url(&self, item_id: &str, query: &str) -> String {
        format!("{}/Items/{item_id}/Images/Primary?{query}", self.base_url)
    }

    fn emby_image_url_at_index(&self, item_id: &str, image_index: i64, query: &str) -> String {
        format!(
            "{}/Items/{item_id}/Images/Primary/{image_index}?{query}",
            self.base_url
        )
    }

    async fn emby_fetch(
        &self,
        item_id: &str,
        token: Option<&str>,
        query: &str,
    ) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
        self.emby_fetch_url(self.emby_image_url(item_id, query), token)
            .await
    }

    async fn emby_fetch_url(
        &self,
        url: String,
        token: Option<&str>,
    ) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
        let mut request = self.client.get(url);
        if let Some(token) = token {
            request = request.header("X-Emby-Token", token);
        }
        Ok(request.send().await?)
    }

    /// Waits for the Emby route to answer from the variant cache instead of the
    /// cold-start fallback, then returns that response.
    async fn emby_variant_response(
        &self,
        item_id: &str,
        token: Option<&str>,
        query: &str,
    ) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
        self.emby_variant_response_url(self.emby_image_url(item_id, query), token)
            .await
    }

    async fn emby_variant_response_url(
        &self,
        url: String,
        token: Option<&str>,
    ) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
        for _ in 0..600 {
            let response = self.emby_fetch_url(url.clone(), token).await?;
            if response.headers()[CACHE_CONTROL] == "private, max-age=31536000, immutable" {
                return Ok(response);
            }
            let _ = response.bytes().await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err(format!("Emby image variant was not generated for {url}").into())
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
async fn global_switch_alone_serves_a_smaller_immutable_variant()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;

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
async fn accounts_with_saved_preferences_use_the_global_switch()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;
    harness.save_account_preferences().await?;

    let response = harness.variant_response().await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );
    Ok(())
}

#[tokio::test]
async fn changing_the_content_tag_invalidates_the_previous_variant()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;

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
    assert!(initial.get("imageVariantsAvailable").is_none());
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
    assert!(available_only.get("imageVariantsAvailable").is_none());
    assert_eq!(available_only["imageVariantsEnabled"], json!(true));

    harness.enable_server_switch(false).await?;
    let disabled: Value = harness
        .client
        .get(format!("{}/api/v1/auth/me", harness.base_url))
        .header(COOKIE, &harness.cookies)
        .send()
        .await?
        .json()
        .await?;
    assert!(disabled.get("imageVariantsAvailable").is_none());
    assert_eq!(disabled["imageVariantsEnabled"], json!(false));
    Ok(())
}

/// LUX-881: Emby clients ask for a display size instead of the exact `w=` the
/// Web client sends. The Emby routes must reuse the same variant cache.
#[tokio::test]
async fn emby_max_width_serves_the_cached_variant() -> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;
    let token = harness
        .emby_token("admin", "correct password", "image-variant-admin")
        .await?;

    // The fixture poster is 1600×2400, so a 300 px wide box needs a 450 px long edge and the
    // 720 tier is the smallest one that covers it.
    let response = harness
        .emby_variant_response(
            &harness.item_id,
            Some(&token),
            &format!("maxWidth=300&api_key={token}"),
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    let body = response.bytes().await?;
    assert!(body.len() < harness.original_bytes.len());
    assert_eq!(headers[CONTENT_TYPE], "image/jpeg");
    assert_eq!(headers[CONTENT_LENGTH], body.len().to_string().as_str());
    let etag = headers[ETAG].to_str()?;
    assert!(etag.ends_with("-w720\""), "unexpected etag {etag}");
    assert_eq!(
        headers[CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );
    assert_eq!(&body[..2], &[0xFF, 0xD8], "variant must be a JPEG");
    let decoded = image::load_from_memory(&body)?;
    assert_eq!(decoded.width(), 480);
    assert_eq!(decoded.height(), 720);

    let shard = format!("{:02x}", Sha256::digest(harness.item_id.as_bytes())[0]);
    let tag = format!("{:x}", Sha256::digest(&harness.original_bytes));
    let cached = harness
        .config_dir
        .join("image-cache/v1")
        .join(shard)
        .join(&harness.item_id)
        .join(format!("{tag}-w720.jpg"));
    assert_eq!(tokio::fs::read(cached).await?, body);
    Ok(())
}

#[tokio::test]
async fn emby_size_hints_keep_the_original_while_the_switch_is_off()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    let token = harness
        .emby_token("admin", "correct password", "image-variant-admin")
        .await?;

    let response = harness
        .emby_fetch(
            &harness.item_id,
            Some(&token),
            &format!("maxWidth=300&maxHeight=400&api_key={token}"),
        )
        .await?;
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
async fn emby_requests_without_a_usable_variant_hint_keep_the_original()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;
    let token = harness
        .emby_token("admin", "correct password", "image-variant-admin")
        .await?;

    // No size hint, a PNG request the JPEG cache cannot satisfy, and every overlay parameter
    // must keep the original bytes even though the cache is enabled.
    for query in [
        "api_key=admin".to_owned(),
        format!("format=png&maxWidth=300&api_key={token}"),
        format!("blur=true&maxWidth=300&api_key={token}"),
        format!("percentPlayed=40&maxWidth=300&api_key={token}"),
        format!("unplayedCount=3&maxWidth=300&api_key={token}"),
        format!("addPlayedIndicator=true&maxWidth=300&api_key={token}"),
    ] {
        let response = harness
            .emby_fetch(&harness.item_id, Some(&token), &query)
            .await?;
        assert_eq!(response.status(), StatusCode::OK, "query {query}");
        assert_eq!(
            response.headers()[CACHE_CONTROL],
            "private, max-age=3600",
            "query {query}"
        );
        assert_eq!(
            response.bytes().await?.as_ref(),
            harness.original_bytes.as_slice(),
            "query {query}"
        );
    }
    assert!(!harness.config_dir.join("image-cache").exists());
    Ok(())
}

#[tokio::test]
async fn emby_size_hint_aliases_and_jpeg_formats_hit_the_cache()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;
    let token = harness
        .emby_token("admin", "correct password", "image-variant-admin")
        .await?;

    // Every accepted spelling asks for the same 720 tier on the 1600×2400 fixture.
    for query in [
        format!("MaxWidth=300&api_key={token}"),
        format!("maxwidth=300&api_key={token}"),
        format!("maxHeight=450&api_key={token}"),
        format!("width=300&format=jpg&api_key={token}"),
        format!("height=450&format=JPEG&api_key={token}"),
    ] {
        let response = harness
            .emby_variant_response(&harness.item_id, Some(&token), &query)
            .await?;
        assert_eq!(response.status(), StatusCode::OK, "query {query}");
        let etag = response.headers()[ETAG].to_str()?.to_owned();
        assert!(etag.ends_with("-w720\""), "query {query} gave etag {etag}");
        let body = response.bytes().await?;
        assert_eq!(&body[..2], &[0xFF, 0xD8], "query {query}");
        let decoded = image::load_from_memory(&body)?;
        assert_eq!(decoded.width().max(decoded.height()), 720, "query {query}");
    }
    Ok(())
}

#[tokio::test]
async fn emby_tagged_image_hits_the_cached_variant() -> Result<(), Box<dyn std::error::Error>> {
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;

    // The tagged route needs no session: the tag itself is the capability.
    let response = harness
        .emby_variant_response(
            &harness.item_id,
            None,
            &format!("tag={}&maxWidth=300", harness.poster_tag),
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response.headers()[ETAG].to_str()?.to_owned();
    assert!(etag.ends_with("-w720\""), "unexpected etag {etag}");
    let body = response.bytes().await?;
    assert_eq!(&body[..2], &[0xFF, 0xD8]);
    let decoded = image::load_from_memory(&body)?;
    assert_eq!(decoded.width().max(decoded.height()), 720);
    Ok(())
}

/// The indexed image route (`/Images/Primary/0`) resolves the same variant, both with a
/// principal and through the tagged capability path.
#[tokio::test]
async fn emby_image_index_route_hits_the_cached_variant() -> Result<(), Box<dyn std::error::Error>>
{
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;
    let token = harness
        .emby_token("admin", "correct password", "image-variant-admin")
        .await?;

    let response = harness
        .emby_variant_response_url(
            harness.emby_image_url_at_index(
                &harness.item_id,
                0,
                &format!("maxWidth=300&api_key={token}"),
            ),
            Some(&token),
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let etag = response.headers()[ETAG].to_str()?.to_owned();
    assert!(etag.ends_with("-w720\""), "unexpected etag {etag}");
    assert_eq!(
        response.headers()[CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );
    let body = response.bytes().await?;
    assert_eq!(&body[..2], &[0xFF, 0xD8], "variant must be a JPEG");
    let decoded = image::load_from_memory(&body)?;
    assert_eq!(decoded.width(), 480);
    assert_eq!(decoded.height(), 720);

    // The same indexed route without a session but with the image tag.
    let tagged = harness
        .emby_variant_response_url(
            harness.emby_image_url_at_index(
                &harness.item_id,
                0,
                &format!("tag={}&maxWidth=300", harness.poster_tag),
            ),
            None,
        )
        .await?;
    assert_eq!(tagged.status(), StatusCode::OK);
    let tagged_etag = tagged.headers()[ETAG].to_str()?.to_owned();
    assert!(
        tagged_etag.ends_with("-w720\""),
        "unexpected etag {tagged_etag}"
    );
    let tagged_body = tagged.bytes().await?;
    assert_eq!(&tagged_body[..2], &[0xFF, 0xD8]);
    let decoded = image::load_from_memory(&tagged_body)?;
    assert_eq!(decoded.width().max(decoded.height()), 720);
    Ok(())
}

#[tokio::test]
async fn emby_variant_hints_keep_the_authorization_order() -> Result<(), Box<dyn std::error::Error>>
{
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;
    let admin = harness
        .emby_token("admin", "correct password", "image-variant-admin")
        .await?;
    // Restrict the viewer to an explicit library list so the item is out of reach.
    sqlx::query("UPDATE users SET library_access_mode = 'SELECTED' WHERE id = ?")
        .bind(&harness.viewer_id)
        .execute(harness.database.pool())
        .await?;
    let viewer = harness
        .emby_token("viewer", "viewer password", "image-variant-viewer")
        .await?;

    let anonymous = harness
        .emby_fetch(&harness.item_id, None, "maxWidth=300")
        .await?;
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    let without_library_access = harness
        .emby_fetch(&harness.item_id, Some(&viewer), "maxWidth=300")
        .await?;
    assert_eq!(without_library_access.status(), StatusCode::NOT_FOUND);

    let missing_item = harness
        .emby_fetch(
            &uuid::Uuid::now_v7().to_string(),
            Some(&admin),
            "maxWidth=300",
        )
        .await?;
    assert_eq!(missing_item.status(), StatusCode::NOT_FOUND);

    // The rejected requests never reach the variant cache.
    assert!(!harness.config_dir.join("image-cache").exists());
    Ok(())
}

/// Target of the Emby image diagnostics; the capture keeps only this target.
const DIAGNOSTIC_TARGET: &str = "luxd::emby_image_diag";

/// Process-wide capture shared by the diagnostic tests.
///
/// `tracing` installs one subscriber per process and caches call-site interest globally, so a
/// per-test subscriber cannot be scoped to one test: every diagnostic test writes into this buffer
/// and attributes its own lines through a `client` label that only it sends.
static DIAGNOSTIC_LOGS: LazyLock<Arc<Mutex<Vec<u8>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));

struct DiagnosticLogWriter;

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for DiagnosticLogWriter {
    type Writer = DiagnosticLogSink;

    fn make_writer(&'a self) -> Self::Writer {
        DiagnosticLogSink(Arc::clone(&DIAGNOSTIC_LOGS))
    }
}

struct DiagnosticLogSink(Arc<Mutex<Vec<u8>>>);

impl Write for DiagnosticLogSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut output = self
            .0
            .lock()
            .map_err(|_| io::Error::other("captured diagnostic log buffer is unavailable"))?;
        output.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Installs the process-wide capture once; later calls are no-ops.
fn install_diagnostic_capture() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "luxd::emby_image_diag=info",
            ))
            .with_writer(DiagnosticLogWriter)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

fn diagnostic_logs() -> String {
    String::from_utf8(
        DIAGNOSTIC_LOGS
            .lock()
            .expect("captured diagnostic log buffer")
            .clone(),
    )
    .expect("captured diagnostic logs are UTF-8")
}

/// Diagnostic lines carrying the given client label; every test uses a label only it sends.
fn diagnostic_lines_for(client: &str) -> Vec<String> {
    let needle = format!("\"client\":\"{client}\"");
    diagnostic_logs()
        .lines()
        .filter(|line| line.contains(DIAGNOSTIC_TARGET) && line.contains(&needle))
        .map(str::to_owned)
        .collect()
}

fn diagnostic_line_count(client: &str) -> usize {
    diagnostic_lines_for(client).len()
}

/// LUX-882: every Emby image request leaves one desensitized diagnostic line naming the variant
/// decision, and neither the API key in the URL nor the access token reaches the captured logs.
#[tokio::test(flavor = "current_thread")]
async fn emby_image_diagnostics_report_the_variant_without_credentials()
-> Result<(), Box<dyn std::error::Error>> {
    install_diagnostic_capture();
    let harness = start_harness().await?;
    harness.enable_server_switch(true).await?;
    let token = harness
        .emby_token("admin", "correct password", "image-variant-diag")
        .await?;

    // Warm the cache first; the very first request may still fall back to the original while the
    // variant generates in the background.
    let warmed = harness
        .emby_variant_response(&harness.item_id, Some(&token), "maxWidth=300")
        .await?;
    assert_eq!(
        warmed.headers()[CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );

    // The diagnosed request authenticates with the API key in the query string and identifies
    // itself only through X-Emby-Client.
    let diagnosed = harness
        .client
        .get(harness.emby_image_url(&harness.item_id, &format!("maxWidth=300&api_key={token}")))
        .header("X-Emby-Client", "Infuse")
        .send()
        .await?;
    assert_eq!(diagnosed.status(), StatusCode::OK);
    let headers = diagnosed.headers().clone();
    let body = diagnosed.bytes().await?;
    assert_eq!(headers[CONTENT_TYPE], "image/jpeg");
    assert_eq!(
        headers[CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );
    let etag = headers[ETAG].to_str()?;
    assert!(etag.ends_with("-w720\""), "unexpected etag {etag}");
    assert!(body.len() < harness.original_bytes.len());

    // A non-JPEG format keeps the original and must report the reason.
    let original = harness
        .client
        .get(harness.emby_image_url(&harness.item_id, "format=png&maxWidth=300"))
        .header("X-Emby-Token", &token)
        .header("X-Emby-Client", "Infuse")
        .send()
        .await?;
    assert_eq!(original.status(), StatusCode::OK);
    assert_eq!(
        original.bytes().await?.as_ref(),
        harness.original_bytes.as_slice()
    );

    let diagnostic_lines = diagnostic_lines_for("Infuse");
    assert!(
        !diagnostic_lines.is_empty(),
        "no emby_image_diag line in {}",
        diagnostic_logs()
    );
    let variant_line = diagnostic_lines
        .iter()
        .rev()
        .find(|line| line.contains("\"outcome\":\"variant\""))
        .ok_or("no variant outcome in the diagnostic lines")?;
    assert!(variant_line.contains("\"variant_width\":720"));
    assert!(variant_line.contains("\"client\":\"Infuse\""));
    assert!(variant_line.contains("\"auth_mode\":\"principal\""));
    assert!(variant_line.contains("\"image_type\":\"POSTER\""));
    assert!(variant_line.contains("\"has_tag\":false"));
    assert!(variant_line.contains("\"max_width\":300"));
    assert!(
        diagnostic_lines
            .iter()
            .any(|line| line.contains("\"reason\":\"non_jpeg_format\"")),
        "no non_jpeg_format reason in {:?}",
        diagnostic_lines
    );
    // The API key in the URL and the access token never reach the log.
    let logs = diagnostic_logs();
    assert!(
        !logs.contains(&token),
        "the access token leaked into {logs}"
    );
    assert!(
        !logs.contains("api_key"),
        "the query string leaked into {logs}"
    );
    Ok(())
}

/// LUX-882 review: the library-cover path only reports an original image when it actually served
/// one. The 404 for a library the caller cannot see must stay silent, while the 200 still logs.
#[tokio::test]
async fn emby_library_cover_errors_do_not_log_a_served_image()
-> Result<(), Box<dyn std::error::Error>> {
    install_diagnostic_capture();
    let harness = start_harness().await?;
    let admin = harness
        .emby_token("admin", "correct password", "diag-cover-admin")
        .await?;
    let viewer = harness
        .emby_token("viewer", "viewer password", "diag-cover-viewer")
        .await?;
    // The viewer keeps no library access, so the cover branch answers 404 instead of an image.
    sqlx::query("UPDATE users SET library_access_mode = 'SELECTED' WHERE id = ?")
        .bind(&harness.viewer_id)
        .execute(harness.database.pool())
        .await?;
    let library_id: String = sqlx::query_scalar("SELECT id FROM libraries WHERE name = 'Movies'")
        .fetch_one(harness.database.pool())
        .await?;
    let covers_dir = harness.config_dir.join("library-covers");
    tokio::fs::create_dir_all(&covers_dir).await?;
    RgbImage::from_pixel(120, 180, Rgb([12, 34, 56])).save(covers_dir.join("diag-cover.jpg"))?;
    sqlx::query(
        "UPDATE libraries
         SET cover_image_path = ?, cover_image_content_type = 'image/jpeg',
             cover_image_size = ?, cover_image_tag = 'diag-cover-tag'
         WHERE id = ?",
    )
    .bind("diag-cover.jpg")
    .bind(1024_i64)
    .bind(&library_id)
    .execute(harness.database.pool())
    .await?;

    let cover_url = format!("{}/Items/{library_id}/Images/Primary", harness.base_url);
    let before = diagnostic_line_count("CoverProbe");
    let denied = harness
        .client
        .get(&cover_url)
        .header("X-Emby-Client", "CoverProbe")
        .header("X-Emby-Token", &viewer)
        .send()
        .await?;
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
    let _ = denied.bytes().await?;
    assert_eq!(
        diagnostic_line_count("CoverProbe"),
        before,
        "a 404 cover response must not claim a served original: {}",
        diagnostic_logs()
    );

    let served = harness
        .client
        .get(&cover_url)
        .header("X-Emby-Client", "CoverProbe")
        .header("X-Emby-Token", &admin)
        .send()
        .await?;
    assert_eq!(served.status(), StatusCode::OK);
    let _ = served.bytes().await?;
    let lines = diagnostic_lines_for("CoverProbe");
    assert_eq!(lines.len(), before + 1, "lines: {lines:?}");
    let line = lines.last().ok_or("missing cover diagnostic")?;
    assert!(line.contains("\"outcome\":\"original\""), "{line}");
    assert!(
        line.contains("\"reason\":\"variant_unavailable\""),
        "{line}"
    );
    assert!(line.contains("\"auth_mode\":\"principal\""), "{line}");
    Ok(())
}

/// LUX-882 review: the person-image path only reports an original image when it served one. A
/// people index that cannot be read answers 503 and must stay silent.
#[tokio::test]
async fn emby_person_image_errors_do_not_log_a_served_image()
-> Result<(), Box<dyn std::error::Error>> {
    install_diagnostic_capture();
    let harness = start_harness().await?;
    let admin = harness
        .emby_token("admin", "correct password", "diag-person-admin")
        .await?;
    // Replace the people index root with a regular file: the person lookup then fails with an I/O
    // error and the route answers 503 instead of an image.
    let people_root = harness.config_dir.join("metadata/people");
    match tokio::fs::remove_dir_all(&people_root).await {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    tokio::fs::create_dir_all(harness.config_dir.join("metadata")).await?;
    tokio::fs::write(&people_root, b"not a directory").await?;

    let person_url = format!("{}/Items/Neo%20Person/Images/Primary", harness.base_url);
    let before = diagnostic_line_count("PersonProbe");
    let response = harness
        .client
        .get(&person_url)
        .header("X-Emby-Client", "PersonProbe")
        .header("X-Emby-Token", &admin)
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let _ = response.bytes().await?;
    assert_eq!(
        diagnostic_line_count("PersonProbe"),
        before,
        "a 503 person response must not claim a served original: {}",
        diagnostic_logs()
    );
    Ok(())
}
