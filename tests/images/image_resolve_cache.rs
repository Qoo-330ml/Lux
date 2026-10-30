use std::path::PathBuf;

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
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::json;
use tokio::net::TcpListener;

/// A movie whose poster sits inside the library root, served through a warm image resolution cache.
///
/// The movie is nested one directory below the root so both a direct parent swap and an
/// intermediate directory swap can be exercised.
struct ImageCacheFixture {
    _temp: tempfile::TempDir,
    client: reqwest::Client,
    base_url: String,
    item_id: String,
    cookies: String,
    studio_dir: PathBuf,
    movie_dir: PathBuf,
    server: tokio::task::JoinHandle<Result<(), std::io::Error>>,
}

impl Drop for ImageCacheFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl ImageCacheFixture {
    async fn start() -> Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        setup.complete("Admin", "Admin", "correct password").await?;
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let media_root = temp.path().join("Movies");
        let studio_dir = media_root.join("Studio");
        let movie_dir = studio_dir.join("Cached Image Movie (2020)");
        tokio::fs::create_dir_all(&movie_dir).await?;
        tokio::fs::write(movie_dir.join("Cached.Image.Movie.2020.mkv"), b"movie").await?;
        tokio::fs::write(movie_dir.join("poster.jpg"), b"poster-bytes").await?;
        libraries
            .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
            .await?;
        LibraryScanner::new(database.clone())
            .scan_movie_library(library.id)
            .await?;
        let item_id: String = sqlx::query_scalar(
            "SELECT id FROM media_items WHERE sort_title = 'cached image movie'",
        )
        .fetch_one(database.pool())
        .await?;
        MetadataEnricher::new(database.clone())
            .enrich_movie_library(library.id)
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
        let base_url = format!("http://{address}");
        let client = reqwest::Client::builder().no_proxy().build()?;
        let login = client
            .post(format!("{base_url}/api/v1/auth/login"))
            .json(&json!({ "username": "admin", "password": "correct password" }))
            .send()
            .await?;
        let cookies = format!(
            "lux_session={}; lux_csrf={}",
            cookie_value(login.headers(), "lux_session"),
            cookie_value(login.headers(), "lux_csrf")
        );
        Ok(Self {
            _temp: temp,
            client,
            base_url,
            item_id,
            cookies,
            studio_dir,
            movie_dir,
            server,
        })
    }

    async fn poster(&self) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
        Ok(self
            .client
            .get(format!(
                "{}/api/v1/items/{}/images/poster",
                self.base_url, self.item_id
            ))
            .header(COOKIE, &self.cookies)
            .send()
            .await?)
    }

    /// Serves the poster twice so the second response comes from a warm resolution cache, and
    /// asserts both responses are identical.
    async fn warm_poster_cache(&self) -> Result<(), Box<dyn std::error::Error>> {
        for attempt in 0..2 {
            let response = self.poster().await?;
            assert_eq!(
                response.status(),
                reqwest::StatusCode::OK,
                "poster request {attempt} must succeed"
            );
            assert_eq!(
                response.bytes().await?.as_ref(),
                b"poster-bytes",
                "poster request {attempt} must serve the file bytes"
            );
        }
        Ok(())
    }
}

#[tokio::test]
async fn deleting_a_cached_poster_stops_serving_it() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = ImageCacheFixture::start().await?;
    fixture.warm_poster_cache().await?;

    tokio::fs::remove_file(fixture.movie_dir.join("poster.jpg")).await?;

    let response = fixture.poster().await?;
    assert_eq!(
        response.status(),
        reqwest::StatusCode::NOT_FOUND,
        "a cached resolution must fall back when the file disappears"
    );
    Ok(())
}

#[tokio::test]
async fn a_cached_poster_is_rejected_after_its_directory_escapes_the_library_root()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = ImageCacheFixture::start().await?;
    fixture.warm_poster_cache().await?;

    // Move the validated directory out of the library root and link it back. The poster keeps its
    // inode, so only re-checking the path containment can reject the escape.
    let outside = fixture._temp.path().join("outside").join("escaped-movie");
    tokio::fs::create_dir_all(outside.parent().ok_or("missing outside directory")?).await?;
    tokio::fs::rename(&fixture.movie_dir, &outside).await?;
    std::os::unix::fs::symlink(&outside, &fixture.movie_dir)?;

    let response = fixture.poster().await?;
    assert_eq!(
        response.status(),
        reqwest::StatusCode::FORBIDDEN,
        "a cached resolution must not serve an image that left the library root"
    );
    Ok(())
}

#[tokio::test]
async fn a_cached_poster_is_rejected_after_an_intermediate_directory_escapes_the_library_root()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = ImageCacheFixture::start().await?;
    fixture.warm_poster_cache().await?;

    // Swap a directory above the poster's parent for a link to the moved tree. The poster and its
    // direct parent keep their inodes, so the hit has to re-resolve the ancestor chain.
    let outside = fixture._temp.path().join("outside").join("escaped-studio");
    tokio::fs::create_dir_all(outside.parent().ok_or("missing outside directory")?).await?;
    tokio::fs::rename(&fixture.studio_dir, &outside).await?;
    std::os::unix::fs::symlink(&outside, &fixture.studio_dir)?;

    let response = fixture.poster().await?;
    assert_eq!(
        response.status(),
        reqwest::StatusCode::FORBIDDEN,
        "a cached resolution must not serve an image reached through an escaping directory"
    );
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
