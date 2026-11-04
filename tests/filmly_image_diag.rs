//! LUX-882 review: the Filmly compatibility branch only logs an image diagnostic when it served an
//! image.
//!
//! `LUX_FILMLY_IMAGE_MODE` is read while `AppState` is built, so this binary keeps exactly one test
//! and owns its process; no other thread reads the environment while it is modified.

use std::{
    io::{self, Write},
    sync::{Arc, LazyLock, Mutex},
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
use reqwest::StatusCode;
use tokio::net::TcpListener;

const DIAGNOSTIC_TARGET: &str = "luxd::emby_image_diag";

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

fn install_diagnostic_capture() {
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "luxd::emby_image_diag=info",
        ))
        .with_writer(DiagnosticLogWriter)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
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

fn diagnostic_line_count(client: &str) -> usize {
    let needle = format!("\"client\":\"{client}\"");
    diagnostic_logs()
        .lines()
        .filter(|line| line.contains(DIAGNOSTIC_TARGET) && line.contains(&needle))
        .count()
}

#[tokio::test(flavor = "current_thread")]
async fn filmly_compat_image_errors_do_not_log_a_served_image()
-> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: this integration-test binary contains exactly this one test, so nothing else reads
    // the environment while it is modified (same pattern as tests/background_work_budget.rs).
    unsafe { std::env::set_var("LUX_FILMLY_IMAGE_MODE", "compat") };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:0".parse()?,
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
    let movie_dir = media_root.join("Filmly Movie (2026)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Filmly.Movie.2026.mkv"), b"movie").await?;
    RgbImage::from_pixel(300, 450, Rgb([30, 120, 90])).save(movie_dir.join("poster.jpg"))?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    MetadataEnricher::new(database.clone())
        .enrich_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE sort_title = 'filmly movie'")
            .fetch_one(database.pool())
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

    install_diagnostic_capture();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()?;
    let base_url = format!("http://{address}");
    let missing_url = format!("{base_url}/Items/{}/Images/Primary", uuid::Uuid::now_v7());
    let movie_url = format!("{base_url}/Items/{item_id}/Images/Primary");

    // An untagged Filmly image loader request for an unknown item: the compat branch answers 404,
    // which resolved no image and must not claim an original was served.
    let missing = client
        .get(&missing_url)
        .header("user-agent", "Filmly/1.0")
        .header("X-Emby-Client", "FilmlyProbe")
        .send()
        .await?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let _ = missing.bytes().await?;
    assert_eq!(
        diagnostic_line_count("FilmlyProbe"),
        0,
        "a 404 filmly compat response must not log: {}",
        diagnostic_logs()
    );

    // The same branch serving the stored poster still reports the original image.
    let served = client
        .get(&movie_url)
        .header("user-agent", "Filmly/1.0")
        .header("X-Emby-Client", "FilmlyProbe")
        .send()
        .await?;
    assert_eq!(served.status(), StatusCode::OK);
    let _ = served.bytes().await?;
    let logs = diagnostic_logs();
    let lines: Vec<&str> = logs
        .lines()
        .filter(|line| line.contains(DIAGNOSTIC_TARGET) && line.contains("FilmlyProbe"))
        .collect();
    assert_eq!(lines.len(), 1, "lines: {lines:?}");
    let line = lines[0];
    assert!(line.contains("\"outcome\":\"original\""), "{line}");
    assert!(
        line.contains("\"reason\":\"variant_unavailable\""),
        "{line}"
    );
    assert!(line.contains("\"auth_mode\":\"filmly_compat\""), "{line}");
    server.abort();
    Ok(())
}
