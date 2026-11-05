//! Host-side coverage of the `RATINGS_SYNC` run against a stub `org.lux.trakt-ratings` plugin.
//!
//! The real plugin lives in the `Lux-plugins` repository. The stub here speaks the same JSON-lines
//! protocol and answers `ratings.batch` from the request text, so the host keeps coverage of the
//! RPC boundary, the untrusted-response validation, the rating-only sidecar write and the
//! post-write metadata refresh without shipping the plugin binary.

#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use luxd::application::{
    libraries::LibraryService,
    nfo::NfoWriteService,
    plugins::PluginService,
    ratings_sync::{RatingsSyncError, RatingsSyncService, RatingsSyncTiming},
    scanner::{LibraryScanner, ScanJobService},
    scheduled_tasks::{ScheduledTaskError, ScheduledTaskService},
    strm_probe::StrmProbeService,
};
use luxd::{config::Config, domain::ids::LibraryId, library::LibraryKind, storage::Database};
use serde_json::json;
use tokio::time::Instant;

const RATINGS_PLUGIN_ID: &str = "org.lux.trakt-ratings";
const WAIT: Duration = Duration::from_secs(30);

/// Stub plugin process. Every item is classified by its identifier so the harness can drive each
/// response shape without editing the script:
///
/// - `tt1000001` (and anything unknown) → a trusted match with a 8.7 Trakt score;
/// - `tt1000002` → a match whose writeback is not confident;
/// - `tt1000003` → `TRAKT_NOT_FOUND`;
/// - `tt1000004` → `TRAKT_UNAUTHORIZED`, emitted before every other result;
/// - `tt1000005` → a confident payload with a score of 11, which the host must reject;
/// - `tt1000006` → a low-confidence match;
/// - `tt1000007` → a `title+year` match;
/// - `tt1000009` → a slow batch, so a second start overlaps the first;
/// - a show lookup whose TMDb id is `1396` → a trusted series match with Trakt id 42, which its
///   episodes then reuse through `show`.
const RATINGS_STUB_SCRIPT: &str = r#"#!/bin/sh
# Host-side stand-in for org.lux.trakt-ratings: answers ratings.batch from the request text alone.
log="$0.requests"

rating_entry() {
  printf '{"ok":true,"input":%s,"result":{"traktId":%s,"matchedBy":"%s","lowConfidence":%s,"writeback":{"confident":%s,"source":"trakt","rating":%s,"votes":%s,"criticRating":%s,"nfo":{"ratings":[{"name":"tmdb","value":8.1,"votes":50,"max":10,"default":false},{"name":"trakt","value":%s,"votes":%s,"max":10,"default":true}],"xml":"","rating":%s,"votes":%s,"criticRating":%s}}}}' \
    "$1" "$2" "$3" "$4" "$5" "$6" "$7" "$8" "$6" "$7" "$6" "$7" "$8"
}

error_entry() {
  printf '{"ok":false,"input":%s,"error":{"code":"%s","message":"upstream refused"}}' "$1" "$2"
}

while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"ratings.batch"'*)
      items=$(printf '%s' "$line" | sed -n 's/.*"items":\[\(.*\)\]}.*/\1/p')
      printf '%s\n' "$items" >> "$log"
      case "$items" in
        *'"imdb":"tt1000009"'*) sleep 2 ;;
      esac
      list=$(printf '%s' "$items" | sed 's/},{/}\
{/g')
      results=""
      pass=1
      while [ "$pass" -le 2 ]; do
        while IFS= read -r item; do
          [ -n "$item" ] || continue
          entry=""
          case "$item" in
            *'"imdb":"tt1000004"'*)
              if [ "$pass" -eq 1 ]; then entry=$(error_entry "$item" TRAKT_UNAUTHORIZED); fi
              ;;
            *)
              if [ "$pass" -eq 2 ]; then
                case "$item" in
                  *'"imdb":"tt1000002"'*) entry=$(rating_entry "$item" 7 imdb false false 8.7 100 7.1) ;;
                  *'"imdb":"tt1000003"'*) entry=$(error_entry "$item" TRAKT_NOT_FOUND) ;;
                  *'"imdb":"tt1000005"'*) entry=$(rating_entry "$item" 5 imdb false true 11.0 10 7.1) ;;
                  *'"imdb":"tt1000006"'*) entry=$(rating_entry "$item" 6 imdb true true 8.7 100 7.1) ;;
                  *'"imdb":"tt1000007"'*) entry=$(rating_entry "$item" 8 title+year false true 8.7 100 7.1) ;;
                  *'"tmdb":"1396"'*) entry=$(rating_entry "$item" 42 tmdb false true 9.4 300 8.0) ;;
                  *'"kind":"episode"'*) entry=$(rating_entry "$item" 43 episode false true 9.1 90 7.5) ;;
                  *) entry=$(rating_entry "$item" 9 imdb false true 8.7 100 7.1) ;;
                esac
              fi
              ;;
          esac
          [ -n "$entry" ] || continue
          if [ -z "$results" ]; then results="$entry"; else results="$results,$entry"; fi
        done <<EOF
$list
EOF
        pass=$((pass + 1))
      done
      printf '{"id":"%s","result":{"count":1,"ok":1,"failed":0,"results":[%s]}}\n' "$id" "$results"
      ;;
    *)
      printf '{"id":"%s","error":{"code":"PLUGIN_UNSUPPORTED_METHOD","message":"unsupported"}}\n' "$id"
      ;;
  esac
done
"#;

struct Harness {
    _directory: tempfile::TempDir,
    database: Database,
    plugins: PluginService,
    library_id: LibraryId,
    root: PathBuf,
    config_dir: PathBuf,
}

fn movie_nfo(title: &str, rating: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <movie>\n  <title>{title}</title>\n  <rating>{rating}</rating>\n</movie>\n"
    )
}

/// Writes the stub property list: manifest, executable entrypoint and the saved plugin config.
async fn write_ratings_plugin(config_dir: &Path, library_ids: &[String]) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let plugin_dir = config_dir.join(format!("plugins/{RATINGS_PLUGIN_ID}"));
    tokio::fs::create_dir_all(plugin_dir.join("binaries")).await?;
    tokio::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": RATINGS_PLUGIN_ID,
            "name": "Trakt ratings",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "metadata",
            "category": "MEDIA",
            "capabilities": [],
            "configFields": [
                {"key": "clientId", "label": "Client id", "type": "text", "required": true},
                {"key": "libraryIds", "label": "媒体库", "type": "select", "multiple": true,
                 "optionsSource": "media-libraries", "defaultValue": []},
                {"key": "schedule", "label": "执行计划", "type": "text", "required": true,
                 "defaultValue": "0 4 * * 0"}
            ],
            "scheduledTasks": [{
                "taskType": "RATINGS_SYNC",
                "ownerType": "LIBRARY",
                "name": "评分同步",
                "description": "按计划把 Trakt 评分写入已有 NFO 旁车。",
                "ownerConfigKey": "libraryIds",
                "scheduleConfigKey": "schedule",
                "defaultSchedule": "0 4 * * 0",
                "requiredConfigKeys": ["clientId", "libraryIds"],
                "resourceLimit": {}
            }],
            "permissions": {"network": ["trakt.tv"], "filesystem": []},
            "files": []
        }))?,
    )
    .await?;
    let entrypoint = plugin_dir.join("binaries/plugin");
    tokio::fs::write(&entrypoint, RATINGS_STUB_SCRIPT).await?;
    std::fs::set_permissions(&entrypoint, std::fs::Permissions::from_mode(0o700))?;
    tokio::fs::create_dir_all(config_dir.join("plugin-config")).await?;
    tokio::fs::write(
        config_dir.join(format!("plugin-config/{RATINGS_PLUGIN_ID}.json")),
        serde_json::to_vec(&json!({
            "clientId": "test-client",
            "libraryIds": library_ids,
            "schedule": "0 4 * * 0"
        }))?,
    )
    .await?;
    Ok(())
}

/// A movie library with the stub plugin installed and selected.
async fn movie_harness(root_count: usize) -> Result<Harness, Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let config_dir = directory.path().join("config");
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    })
    .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let mut root = directory.path().join("Movies");
    for index in 0..root_count {
        root = directory.path().join(format!("Movies {index}"));
        tokio::fs::create_dir_all(&root).await?;
        libraries
            .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
            .await?;
    }
    write_ratings_plugin(&config_dir, &[library.id.to_string()]).await?;
    let plugins = PluginService::new(database.clone(), config_dir.clone());
    plugins.install(RATINGS_PLUGIN_ID).await?;
    Ok(Harness {
        _directory: directory,
        database,
        plugins,
        library_id: library.id,
        root,
        config_dir,
    })
}

impl Harness {
    fn scanner(&self) -> LibraryScanner {
        LibraryScanner::new(self.database.clone())
    }

    async fn add_movie(
        &self,
        directory: &Path,
        file_name: &str,
        title: &str,
        imdb: &str,
        nfo_rating: Option<&str>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let directory = if directory.is_absolute() {
            directory.to_path_buf()
        } else {
            self.root.join(directory)
        };
        tokio::fs::create_dir_all(&directory).await?;
        let media = directory.join(format!("{file_name}.mkv"));
        tokio::fs::write(&media, b"fixture").await?;
        if let Some(rating) = nfo_rating {
            tokio::fs::write(
                directory.join(format!("{file_name}.nfo")),
                movie_nfo(title, rating),
            )
            .await?;
        }
        self.scanner().scan_movie_library(self.library_id).await?;
        let item_id: String =
            sqlx::query_scalar("SELECT id FROM media_items WHERE library_id = ? AND title = ?")
                .bind(self.library_id.to_string())
                .bind(title)
                .fetch_one(self.database.pool())
                .await?;
        sqlx::query("UPDATE media_items SET provider_ids_json = ? WHERE id = ?")
            .bind(json!({ "imdb": imdb }).to_string())
            .bind(&item_id)
            .execute(self.database.pool())
            .await?;
        Ok(item_id)
    }

    fn sync_service(&self) -> RatingsSyncService {
        RatingsSyncService::new(
            self.database.clone(),
            self.plugins.clone(),
            NfoWriteService::new(self.database.clone()),
            ScanJobService::new(self.database.clone()),
        )
        .with_timing(RatingsSyncTiming {
            batch_timeout: Duration::from_secs(20),
            batch_backoff: [Duration::from_millis(10), Duration::from_millis(10)],
            rate_limit_delay: Duration::from_millis(10),
        })
    }

    fn requests_log(&self) -> PathBuf {
        self.config_dir.join(format!(
            "plugins/{RATINGS_PLUGIN_ID}/binaries/plugin.requests"
        ))
    }

    fn recorded_requests(&self) -> String {
        std::fs::read_to_string(self.requests_log()).unwrap_or_default()
    }
}

async fn wait_for_rating(
    database: &Database,
    item_id: &str,
    expected: f64,
) -> Result<Option<f64>, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + WAIT;
    loop {
        let rating: Option<f64> = sqlx::query_scalar("SELECT rating FROM media_items WHERE id = ?")
            .bind(item_id)
            .fetch_one(database.pool())
            .await?;
        if rating.is_some_and(|rating| (rating - expected).abs() < 1e-9) {
            return Ok(rating);
        }
        if Instant::now() > deadline {
            return Ok(rating);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn nfo_content(path: &Path) -> String {
    tokio::fs::read_to_string(path).await.unwrap_or_default()
}

#[tokio::test]
async fn writes_ratings_for_sidecars_and_refreshes_the_catalog()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = movie_harness(1).await?;
    let movie = harness
        .add_movie(
            Path::new("Confident Movie (2020)"),
            "Confident.Movie.2020",
            "Confident Movie",
            "tt1000001",
            Some("1.0"),
        )
        .await?;
    let nfo = harness
        .root
        .join("Confident Movie (2020)/Confident.Movie.2020.nfo");

    let service = harness.sync_service();
    let run = service
        .start(&harness.library_id.to_string(), RATINGS_PLUGIN_ID)
        .await?;
    assert!(!run.id.is_empty());
    assert!(
        service.wait_for_idle(WAIT).await,
        "the run did not finish in time"
    );

    let content = nfo_content(&nfo).await;
    assert!(
        content.contains("<ratings>") && content.contains("<rating>8.7</rating>"),
        "the sidecar must carry the Trakt score: {content}"
    );
    assert!(
        content.contains(r#"<rating name="trakt" max="10" default="true">"#),
        "the container keeps the plugin order, primary source last: {content}"
    );
    assert!(
        !content.contains("<rating>1.0</rating>"),
        "the old score must be replaced: {content}"
    );

    let rating = wait_for_rating(&harness.database, &movie, 8.7).await?;
    assert_eq!(
        rating,
        Some(8.7),
        "the refresh must make the catalog expose the new score"
    );
    Ok(())
}

#[tokio::test]
async fn writes_series_and_episode_ratings_through_the_series_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let config_dir = directory.path().join("config");
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    })
    .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let root = directory.path().join("Shows");
    let season = root.join("Example Show (2020)").join("Season 01");
    tokio::fs::create_dir_all(&season).await?;
    tokio::fs::write(season.join("Example.Show.S01E01.mkv"), b"fixture").await?;
    tokio::fs::write(
        root.join("Example Show (2020)/tvshow.nfo"),
        "<tvshow>\n  <title>Example Show</title>\n  <rating>1.0</rating>\n</tvshow>\n",
    )
    .await?;
    tokio::fs::write(
        season.join("episode.nfo"),
        concat!(
            "<episodedetails>\n",
            "  <title>One</title>\n",
            "  <season>1</season>\n",
            "  <episode>1</episode>\n",
            "  <rating>2.0</rating>\n",
            "</episodedetails>\n",
        ),
    )
    .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;
    // The stub answers the `show` lookup with Trakt id 42; the episode then reuses it because the
    // series has no IMDb id of its own.
    sqlx::query("UPDATE media_items SET provider_ids_json = ? WHERE library_id = ? AND item_type = 'SERIES'")
        .bind(json!({ "tmdb": "1396" }).to_string())
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_items SET production_year = 2008 WHERE library_id = ? AND item_type = 'SERIES'")
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
    write_ratings_plugin(&config_dir, &[library.id.to_string()]).await?;
    let plugins = PluginService::new(database.clone(), config_dir.clone());
    plugins.install(RATINGS_PLUGIN_ID).await?;
    let service = RatingsSyncService::new(
        database.clone(),
        plugins,
        NfoWriteService::new(database.clone()),
        ScanJobService::new(database.clone()),
    )
    .with_timing(RatingsSyncTiming {
        batch_timeout: Duration::from_secs(20),
        batch_backoff: [Duration::from_millis(10), Duration::from_millis(10)],
        rate_limit_delay: Duration::from_millis(10),
    });

    service
        .start(&library.id.to_string(), RATINGS_PLUGIN_ID)
        .await?;
    assert!(service.wait_for_idle(WAIT).await, "the run did not finish");

    let series_nfo = nfo_content(&root.join("Example Show (2020)/tvshow.nfo")).await;
    assert!(
        series_nfo.contains("<rating>9.4</rating>"),
        "the series title lookup must resolve through TMDb: {series_nfo}"
    );
    let episode_nfo = nfo_content(&season.join("episode.nfo")).await;
    assert!(
        episode_nfo.contains("<rating>9.1</rating>"),
        "the episode must reuse the series Trakt id: {episode_nfo}"
    );
    let requests = std::fs::read_to_string(config_dir.join(format!(
        "plugins/{RATINGS_PLUGIN_ID}/binaries/plugin.requests"
    )))
    .unwrap_or_default();
    assert!(
        requests.contains(r#""kind":"episode""#) && requests.contains(r#""show":"42""#),
        "the episode lookup must use the cached numeric Trakt id: {requests}"
    );
    Ok(())
}

#[tokio::test]
async fn skips_unconfident_not_found_and_invalid_results() -> Result<(), Box<dyn std::error::Error>>
{
    let harness = movie_harness(1).await?;
    let cases = [
        (
            "Unconfident Movie (2020)",
            "Unconfident.Movie.2020",
            "Unconfident Movie",
            "tt1000002",
        ),
        (
            "Missing Movie (2020)",
            "Missing.Movie.2020",
            "Missing Movie",
            "tt1000003",
        ),
        (
            "Low Confidence Movie (2020)",
            "Low.Confidence.Movie.2020",
            "Low Confidence Movie",
            "tt1000006",
        ),
        (
            "Title Only Movie (2020)",
            "Title.Only.Movie.2020",
            "Title Only Movie",
            "tt1000007",
        ),
        (
            "Invalid Movie (2020)",
            "Invalid.Movie.2020",
            "Invalid Movie",
            "tt1000005",
        ),
    ];
    let mut nfo_paths = Vec::new();
    for (directory, file_name, title, imdb) in cases {
        harness
            .add_movie(Path::new(directory), file_name, title, imdb, Some("1.0"))
            .await?;
        nfo_paths.push(
            harness
                .root
                .join(directory)
                .join(format!("{file_name}.nfo")),
        );
    }

    let service = harness.sync_service();
    service
        .start(&harness.library_id.to_string(), RATINGS_PLUGIN_ID)
        .await?;
    assert!(service.wait_for_idle(WAIT).await, "the run did not finish");

    for nfo in &nfo_paths {
        let content = nfo_content(nfo).await;
        assert!(
            content.contains("<rating>1.0</rating>"),
            "{} must keep its old score, got: {content}",
            nfo.display()
        );
    }
    Ok(())
}

#[tokio::test]
async fn credentials_failure_stops_the_whole_run() -> Result<(), Box<dyn std::error::Error>> {
    let harness = movie_harness(1).await?;
    // The stub always reports UNAUTHORIZED for tt1000004 before any other result, so the run stops
    // before the healthy movie is processed regardless of the batch order.
    harness
        .add_movie(
            Path::new("Rejected Movie (2020)"),
            "Rejected.Movie.2020",
            "Rejected Movie",
            "tt1000004",
            Some("1.0"),
        )
        .await?;
    harness
        .add_movie(
            Path::new("Healthy Movie (2020)"),
            "Healthy.Movie.2020",
            "Healthy Movie",
            "tt1000001",
            Some("1.0"),
        )
        .await?;

    let service = harness.sync_service();
    service
        .start(&harness.library_id.to_string(), RATINGS_PLUGIN_ID)
        .await?;
    assert!(service.wait_for_idle(WAIT).await, "the run did not finish");

    for (directory, file_name) in [
        ("Rejected Movie (2020)", "Rejected.Movie.2020"),
        ("Healthy Movie (2020)", "Healthy.Movie.2020"),
    ] {
        let content = nfo_content(
            &harness
                .root
                .join(directory)
                .join(format!("{file_name}.nfo")),
        )
        .await;
        assert!(
            content.contains("<rating>1.0</rating>"),
            "{file_name} must not be written after a credential failure: {content}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn never_queries_items_without_a_sidecar() -> Result<(), Box<dyn std::error::Error>> {
    let harness = movie_harness(1).await?;
    harness
        .add_movie(
            Path::new("With Nfo Movie (2020)"),
            "With.Nfo.Movie.2020",
            "With Nfo Movie",
            "tt1000001",
            Some("1.0"),
        )
        .await?;
    harness
        .add_movie(
            Path::new("Without Nfo Movie (2020)"),
            "Without.Nfo.Movie.2020",
            "Without Nfo Movie",
            "tt1000099",
            None,
        )
        .await?;

    let service = harness.sync_service();
    service
        .start(&harness.library_id.to_string(), RATINGS_PLUGIN_ID)
        .await?;
    assert!(service.wait_for_idle(WAIT).await, "the run did not finish");

    let requests = harness.recorded_requests();
    assert!(
        requests.contains("tt1000001"),
        "the item with a sidecar must be queried: {requests}"
    );
    assert!(
        !requests.contains("tt1000099"),
        "an item without a sidecar must never be queried: {requests}"
    );
    assert!(
        !harness
            .root
            .join("Without Nfo Movie (2020)/Without.Nfo.Movie.2020.nfo")
            .exists(),
        "the sync must not create a sidecar"
    );
    Ok(())
}

#[tokio::test]
async fn rejects_a_second_run_for_the_same_library() -> Result<(), Box<dyn std::error::Error>> {
    let harness = movie_harness(1).await?;
    harness
        .add_movie(
            Path::new("Slow Movie (2020)"),
            "Slow.Movie.2020",
            "Slow Movie",
            "tt1000009",
            Some("1.0"),
        )
        .await?;

    let service = harness.sync_service();
    let library_id = harness.library_id.to_string();
    let first = service.start(&library_id, RATINGS_PLUGIN_ID).await?;
    let error = service
        .start(&library_id, RATINGS_PLUGIN_ID)
        .await
        .expect_err("a second run must be refused");
    assert!(matches!(error, RatingsSyncError::AlreadyActive));
    assert!(service.wait_for_idle(WAIT).await, "the run did not finish");
    assert!(!first.id.is_empty());

    // The guard is released when the run ends, so a later run is accepted again.
    service.start(&library_id, RATINGS_PLUGIN_ID).await?;
    assert!(
        service.wait_for_idle(WAIT).await,
        "the rerun did not finish"
    );
    Ok(())
}

#[tokio::test]
async fn refreshes_every_root_of_a_multi_root_library() -> Result<(), Box<dyn std::error::Error>> {
    let harness = movie_harness(2).await?;
    let first_root = harness._directory.path().join("Movies 0");
    let second_root = harness._directory.path().join("Movies 1");
    let first_directory = first_root.join("First Movie (2020)");
    let first = harness
        .add_movie(
            &first_directory,
            "First.Movie.2020",
            "First Movie",
            "tt1000001",
            Some("1.0"),
        )
        .await?;
    let second_directory = second_root.join("Second Movie (2020)");
    let second = harness
        .add_movie(
            &second_directory,
            "Second.Movie.2020",
            "Second Movie",
            "tt1000010",
            Some("1.0"),
        )
        .await?;

    let service = harness.sync_service();
    service
        .start(&harness.library_id.to_string(), RATINGS_PLUGIN_ID)
        .await?;
    assert!(service.wait_for_idle(WAIT).await, "the run did not finish");

    assert_eq!(
        wait_for_rating(&harness.database, &first, 8.7).await?,
        Some(8.7),
        "the first root must be refreshed with its own root id"
    );
    assert_eq!(
        wait_for_rating(&harness.database, &second, 8.7).await?,
        Some(8.7),
        "the second root must be refreshed with its own root id"
    );
    Ok(())
}

#[tokio::test]
async fn plugin_manifest_registers_a_library_task_the_scheduler_can_run()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let config_dir = directory.path().join("config");
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    })
    .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Empty Movies", LibraryKind::Movie, false)
        .await?;
    let root = directory.path().join("Empty");
    tokio::fs::create_dir_all(&root).await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    write_ratings_plugin(&config_dir, &[library.id.to_string()]).await?;
    let plugins = PluginService::new(database.clone(), config_dir);
    plugins.install(RATINGS_PLUGIN_ID).await?;

    let registered: (String, String, i64) = sqlx::query_as(
        "SELECT source_type, plugin_id, is_enabled FROM scheduled_task_configs
         WHERE owner_type = 'LIBRARY' AND owner_id = ? AND task_type = 'RATINGS_SYNC'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(registered.0, "PLUGIN");
    assert_eq!(registered.1, RATINGS_PLUGIN_ID);
    assert_eq!(registered.2, 1, "a configured plugin enables its task");

    let scheduler = ScheduledTaskService::new(
        database.clone(),
        plugins.clone(),
        StrmProbeService::new(database.clone(), plugins.clone()),
        None,
    )
    .with_ratings_sync(
        RatingsSyncService::new(
            database.clone(),
            plugins,
            NfoWriteService::new(database.clone()),
            ScanJobService::new(database.clone()),
        )
        .with_timing(RatingsSyncTiming {
            batch_timeout: Duration::from_secs(5),
            batch_backoff: [Duration::from_millis(10), Duration::from_millis(10)],
            rate_limit_delay: Duration::from_millis(10),
        }),
    );
    let run = scheduler
        .run_task("LIBRARY", &library.id.to_string(), "RATINGS_SYNC")
        .await?;
    assert_eq!(run.task_type(), "RATINGS_SYNC");
    match &run {
        luxd::application::scheduled_tasks::ScheduledTaskRun::RatingsSync {
            run_id,
            library_id,
        } => {
            assert!(uuid_like(run_id), "the run id must be a UUID: {run_id}");
            assert_eq!(library_id, &library.id.to_string());
        }
        other => return Err(format!("unexpected run kind: {other:?}").into()),
    }
    let error = scheduler
        .run_task("LIBRARY", &library.id.to_string(), "NOT_A_TASK")
        .await
        .expect_err("unknown task types must still be rejected");
    assert!(matches!(error, ScheduledTaskError::UnsupportedTask));
    scheduler.shutdown().await;
    Ok(())
}

/// Without a UUID crate dependency in the integration test, check the shape of a v7 UUID.
fn uuid_like(value: &str) -> bool {
    value.len() == 36
        && value
            .chars()
            .enumerate()
            .all(|(index, character)| match index {
                8 | 13 | 18 | 23 => character == '-',
                _ => character.is_ascii_hexdigit(),
            })
}
