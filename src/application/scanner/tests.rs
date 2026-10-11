use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::library::read_strm_target;
use super::names::{MixedManifestClassification, infer_sibling_movie_variant_suffix_with_probe};
use super::{
    LOCAL_METADATA_COMPLETION_FALLBACK, LOCAL_METADATA_JOB_REFRESH_FALLBACK, LibraryScanner,
    LocalMetadataCompletenessTrigger, MANIFEST_DISCOVERY_BATCH_SIZE,
    MANIFEST_STREAMED_ENTRY_BATCH_SIZE, MANIFEST_STREAMED_INDEX_BATCH_SIZE, MAX_STRM_TARGET_BYTES,
    ManifestDirectoryReader, ManifestFilenameInput, ManifestRemovalOutcome,
    ManifestRootDiscoveryContext, MixedClassification, MixedClassificationCache,
    NewScanManifestDiscoveryChunk, NewScanManifestEntry, PendingManifestDirectoryChunk,
    PreparedManifestFilename, ScanJobService, ScannerError, ScraperAvailabilityPreflight,
    classify_manifest_removal_outcomes, classify_mixed_file, configured_scan_concurrency,
    infer_sibling_movie_variant_suffix, is_lite_manifest_discovery,
    manifest_file_observation_matches, manifest_root_identity_matches, media_source_folder,
    merge_movie_provider_ids, metadata_auto_match_policy_for_scan_job,
    metadata_auto_match_policy_for_scan_job_snapshot, normalize_incremental_path,
    notify_local_metadata_images_waiter, parse_episode_filename, parse_movie_filename,
    prepare_manifest_filename, read_manifest_strm_target, resolve_local_metadata_auto_match_policy,
    safe_scan_activity_label, scan_lock_database_wait_backoff,
    stat_manifest_directory_file_batch_sync, stat_manifest_relative_file_sync,
    stat_manifest_root_sync,
};
use crate::application::scraper::{
    ScraperAdapter, ScraperCreditsResponse, ScraperError, ScraperExternalIdsResponse,
    ScraperFuture, ScraperGetRequest, ScraperImageRequest, ScraperImagesResponse, ScraperMetadata,
    ScraperMetadataBundle, ScraperProvider, ScraperSearchRequest, ScraperSearchResponse,
    ScraperTrailersResponse,
};
use crate::storage::{MetadataAutoMatchPolicy, StoredScanJob};
use tokio::sync::{Notify, Semaphore};

#[derive(Clone)]
struct GateSearchScraper {
    search_started: Arc<Notify>,
    release_search: Arc<Semaphore>,
}

#[test]
fn scraper_preflight_failure_enters_bounded_fill_missing_retry() {
    assert!(ScraperAvailabilityPreflight::from_check_result(Err(())).allows_fill_missing());
    assert!(ScraperAvailabilityPreflight::from_check_result(Ok(Some(true))).allows_fill_missing());
    assert!(
        !ScraperAvailabilityPreflight::from_check_result(Ok(Some(false))).allows_fill_missing()
    );
    assert!(!ScraperAvailabilityPreflight::from_check_result(Ok(None)).allows_fill_missing());
}

fn unsupported_scraper_call<T: Send + 'static>(
    capability: &'static str,
) -> ScraperFuture<'static, Result<T, ScraperError>> {
    Box::pin(std::future::ready(Err(
        ScraperError::UnsupportedCapability(capability.to_owned()),
    )))
}

impl ScraperAdapter for GateSearchScraper {
    fn provider_key(&self) -> &str {
        "scan-test-provider"
    }

    fn search(
        &self,
        _request: ScraperSearchRequest,
    ) -> ScraperFuture<'_, Result<ScraperSearchResponse, ScraperError>> {
        let started = self.search_started.clone();
        let release = self.release_search.clone();
        Box::pin(async move {
            started.notify_one();
            let permit = release
                .acquire()
                .await
                .map_err(|error| ScraperError::Provider(error.to_string()))?;
            permit.forget();
            Ok(ScraperSearchResponse::default())
        })
    }

    fn get(
        &self,
        _request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperMetadata, ScraperError>> {
        unsupported_scraper_call("metadata.get")
    }

    fn bundle(
        &self,
        _request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperMetadataBundle, ScraperError>> {
        unsupported_scraper_call("metadata.bundle")
    }

    fn images(
        &self,
        _request: ScraperImageRequest,
    ) -> ScraperFuture<'_, Result<ScraperImagesResponse, ScraperError>> {
        unsupported_scraper_call("metadata.images")
    }

    fn credits(
        &self,
        _request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperCreditsResponse, ScraperError>> {
        unsupported_scraper_call("metadata.credits")
    }

    fn external_ids(
        &self,
        _request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperExternalIdsResponse, ScraperError>> {
        unsupported_scraper_call("metadata.externalIds")
    }

    fn trailers(
        &self,
        _request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperTrailersResponse, ScraperError>> {
        unsupported_scraper_call("metadata.trailers")
    }
}

#[test]
fn configured_scan_concurrency_prefers_global_override_then_library_value() {
    assert_eq!(configured_scan_concurrency(Some(8), Some(4), 16), 8);
    assert_eq!(configured_scan_concurrency(None, Some(4), 16), 4);
    assert_eq!(configured_scan_concurrency(None, None, 16), 16);
}

fn stored_scan_job(job_type: &str, auto_metadata_match: bool) -> StoredScanJob {
    StoredScanJob {
        id: "scan-job".to_owned(),
        library_id: "library".to_owned(),
        job_type: job_type.to_owned(),
        status: "RUNNING".to_owned(),
        generation: "generation".to_owned(),
        cursor: None,
        processed_count: 0,
        total_count: 0,
        cancel_requested: false,
        error: None,
        created_at: 0,
        started_at: None,
        finished_at: None,
        discovery_completed: false,
        auto_metadata_match,
        current_item: None,
        scan_phase: "INDEXING".to_owned(),
    }
}

#[test]
fn metadata_completeness_policy_is_explicit_for_scan_triggers() {
    let incremental_disabled = stored_scan_job("INCREMENTAL_SCAN", false);
    let incremental_enabled = stored_scan_job("INCREMENTAL_SCAN", true);
    let full_scan = stored_scan_job("RECONCILE_LIBRARY", false);

    assert_eq!(
        metadata_auto_match_policy_for_scan_job(Some(&incremental_disabled)),
        MetadataAutoMatchPolicy::Disabled
    );
    assert_eq!(
        metadata_auto_match_policy_for_scan_job(Some(&incremental_enabled)),
        MetadataAutoMatchPolicy::Enabled
    );
    assert_eq!(
        metadata_auto_match_policy_for_scan_job(Some(&full_scan)),
        MetadataAutoMatchPolicy::UseLibrarySetting
    );
    assert_eq!(
        metadata_auto_match_policy_for_scan_job(None),
        MetadataAutoMatchPolicy::Disabled
    );
    assert!(!MetadataAutoMatchPolicy::Disabled.allows_scraper_lookup());
    assert!(MetadataAutoMatchPolicy::UseLibrarySetting.allows_scraper_lookup());
    assert!(MetadataAutoMatchPolicy::Enabled.allows_scraper_lookup());
}

#[tokio::test]
async fn library_setting_trigger_preserves_library_completeness_policy()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{config::Config, storage::Database};

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    let policy = resolve_local_metadata_auto_match_policy(
        &database,
        LocalMetadataCompletenessTrigger::LibrarySetting,
    )
    .await?;

    assert_eq!(policy, MetadataAutoMatchPolicy::UseLibrarySetting);
    Ok(())
}

#[tokio::test]
async fn incremental_scan_completeness_policy_does_not_reload_its_metadata_page_job()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let job_id = "incremental-completeness-page-policy";
    sqlx::query(
        "INSERT INTO scan_jobs (
                 id, library_id, job_type, status, generation, auto_metadata_match
             ) VALUES (?, ?, 'INCREMENTAL_SCAN', 'RUNNING', 'generation', 1)",
    )
    .bind(job_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    database.reset_query_count();
    let page = database.load_scan_job_metadata_page(job_id, 8).await?;
    assert_eq!(database.query_count(), 1);
    assert!(page.auto_metadata_match);
    database.reset_query_count();

    let policy = resolve_local_metadata_auto_match_policy(
        &database,
        LocalMetadataCompletenessTrigger::ScanJobPolicy(
            metadata_auto_match_policy_for_scan_job_snapshot(
                page.job_type.as_deref(),
                page.auto_metadata_match,
            ),
        ),
    )
    .await?;

    assert_eq!(policy, MetadataAutoMatchPolicy::Enabled);
    assert_eq!(
        database.query_count(),
        0,
        "completeness policy should reuse scan-job values returned with its metadata page"
    );
    Ok(())
}

#[tokio::test]
async fn library_deletion_waits_for_terminal_active_run_and_fences_new_runs()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let job_id = "active-terminal-job";
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, scan_phase)
             VALUES (?, ?, 'RECONCILE_LIBRARY', 'COMPLETED', 'generation', 'POSTPROCESSING')",
    )
    .bind(job_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    let jobs = ScanJobService::new(database.clone());
    let library_id = library.id.to_string();
    let active_run = jobs
        .track_scan_job_run(job_id, &library_id)
        .ok_or("initial run should register before deletion starts")?;
    sqlx::query("UPDATE scan_jobs SET status = 'CANCELLED', scan_phase = 'IDLE' WHERE id = ?")
        .bind(job_id)
        .execute(database.pool())
        .await?;

    let deletion_jobs = jobs.clone();
    let preparation =
        tokio::spawn(async move { deletion_jobs.prepare_library_deletion(library.id).await });
    let fence_set = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while let Some(late_run) = jobs.track_scan_job_run("late-run", &library_id) {
            drop(late_run);
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .is_ok();
    assert!(fence_set, "deletion should fence new runs promptly");
    assert!(
        !preparation.is_finished(),
        "deletion must still wait when the active run's database row is terminal"
    );

    drop(active_run);
    let _deletion_guard =
        tokio::time::timeout(std::time::Duration::from_secs(1), preparation).await??;
    libraries.delete_library(library.id).await?;
    Ok(())
}

#[test]
fn scan_lock_database_wait_backoff_starts_at_ten_ms_and_caps_at_250_ms() {
    let delays = (0..=6)
        .map(scan_lock_database_wait_backoff)
        .collect::<Vec<_>>();

    assert_eq!(
        delays,
        [10, 20, 40, 80, 160, 250, 250].map(std::time::Duration::from_millis)
    );
    assert_eq!(
        scan_lock_database_wait_backoff(usize::MAX),
        std::time::Duration::from_millis(250)
    );
}

#[test]
fn movie_folder_provider_ids_fill_missing_values_without_overriding_file_tags() {
    let file_provider_ids =
        std::collections::BTreeMap::from([("Tmdb".to_owned(), "file-id".to_owned())]);
    let folder_provider_ids = std::collections::BTreeMap::from([
        ("Tmdb".to_owned(), "folder-id".to_owned()),
        ("Imdb".to_owned(), "tt1234567".to_owned()),
    ]);

    let merged = merge_movie_provider_ids(file_provider_ids, &folder_provider_ids);

    assert_eq!(
        merged,
        std::collections::BTreeMap::from([
            ("Imdb".to_owned(), "tt1234567".to_owned()),
            ("Tmdb".to_owned(), "file-id".to_owned()),
        ])
    );
}

#[test]
fn manifest_filename_preparation_classifies_and_reuses_parsed_names() {
    let movie = prepare_manifest_filename(
        "Movie.Name.2020.1080p.mkv",
        ManifestFilenameInput::MovieLibrary,
    )
    .expect("movie-library classification");
    assert!(matches!(
        movie,
        PreparedManifestFilename::Movie(parsed) if parsed.production_year == Some(2020)
    ));

    let unresolved = prepare_manifest_filename("", ManifestFilenameInput::MovieLibrary)
        .expect("unparseable movie becomes unresolved");
    assert!(matches!(unresolved, PreparedManifestFilename::Unresolved));

    let episode = prepare_manifest_filename(
        "Example.Show.S01E02.mkv",
        ManifestFilenameInput::SeriesLibrary,
    )
    .expect("series-library classification");
    assert!(matches!(
        episode,
        PreparedManifestFilename::Episode(parsed) if parsed.season == 1 && parsed.episode == 2
    ));

    assert!(
        prepare_manifest_filename(
            "",
            ManifestFilenameInput::LegacyMixed(Some(MixedClassification::Movie)),
        )
        .is_none()
    );

    let carried_name =
        parse_movie_filename("Movie.Name.2020.1080p.mkv").expect("known movie filename parses");
    let mixed_movie = prepare_manifest_filename(
        "not-a-movie.mkv",
        ManifestFilenameInput::PreparedMixed(MixedManifestClassification::Movie(Some(
            carried_name,
        ))),
    )
    .expect("mixed classifier's parsed name is reused");
    assert!(matches!(
        mixed_movie,
        PreparedManifestFilename::Movie(parsed) if parsed.production_year == Some(2020)
    ));

    let mixed_episode = prepare_manifest_filename(
        "not-an-episode.mkv",
        ManifestFilenameInput::PreparedMixed(MixedManifestClassification::Episode(
            parse_episode_filename("Example.Show.S01E02.mkv"),
        )),
    )
    .expect("mixed classifier's parsed episode is reused");
    assert!(matches!(
        mixed_episode,
        PreparedManifestFilename::Episode(parsed) if parsed.season == 1 && parsed.episode == 2
    ));
}

#[tokio::test]
async fn movie_manifest_groups_generic_suffixes_with_exact_sibling_bases()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    for code in ["ADN-610", "ADN-723"] {
        let movie_dir = media_root.join(code);
        tokio::fs::create_dir_all(&movie_dir).await?;
        tokio::fs::write(movie_dir.join(format!("{code}.mp4")), b"base").await?;
        tokio::fs::write(movie_dir.join(format!("{code}-C.mp4")), b"variant").await?;
    }

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF-8 media root")?,
        )
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;

    let movies: Vec<(String, i64)> = sqlx::query_as(
        "SELECT item.title, COUNT(source.id)
             FROM media_items item
             JOIN media_sources source ON source.item_id = item.id
             WHERE item.library_id = ? AND item.item_type = 'MOVIE' AND item.removed_at IS NULL
             GROUP BY item.id
             ORDER BY item.title",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    let editions: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT entry.relative_path, source.edition_name
             FROM media_sources source
             JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
             ORDER BY entry.relative_path",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        editions,
        vec![
            ("ADN-610/ADN-610-C.mp4".to_owned(), Some("C".to_owned())),
            ("ADN-610/ADN-610.mp4".to_owned(), None),
            ("ADN-723/ADN-723-C.mp4".to_owned(), Some("C".to_owned())),
            ("ADN-723/ADN-723.mp4".to_owned(), None),
        ]
    );
    assert_eq!(
        movies,
        vec![("ADN 610".to_owned(), 2), ("ADN 723".to_owned(), 2)]
    );

    for code in ["ADN-610", "ADN-723"] {
        let legacy_item_id = format!("legacy-{code}");
        let legacy_title = format!("{} C", code.replace('-', " "));
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, original_title,
                    identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(&legacy_item_id)
        .bind(library.id.to_string())
        .bind(&legacy_title)
        .bind(legacy_title.to_lowercase())
        .bind(&legacy_title)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "UPDATE media_sources SET item_id = ?, edition_name = NULL
                 WHERE filesystem_entry_id = (
                     SELECT id FROM filesystem_entries
                     WHERE library_root_id = ? AND relative_path = ?
                 )",
        )
        .bind(&legacy_item_id)
        .bind(root.id.to_string())
        .bind(format!("{code}/{code}-C.mp4"))
        .execute(database.pool())
        .await?;
    }

    let rescan = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&rescan.id, 100, None).await?;
    let movies: Vec<(String, i64)> = sqlx::query_as(
        "SELECT item.title, COUNT(source.id)
             FROM media_items item
             JOIN media_sources source ON source.item_id = item.id
             WHERE item.library_id = ? AND item.item_type = 'MOVIE' AND item.removed_at IS NULL
             GROUP BY item.id
             ORDER BY item.title",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        movies,
        vec![("ADN 610".to_owned(), 2), ("ADN 723".to_owned(), 2)]
    );
    let removed_legacy_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items
             WHERE id IN (?, ?) AND removed_at IS NOT NULL",
    )
    .bind("legacy-ADN-610")
    .bind("legacy-ADN-723")
    .fetch_one(database.pool())
    .await?;
    assert_eq!(removed_legacy_items, 2);

    sqlx::query("UPDATE media_sources SET probe_status = 'READY'")
        .execute(database.pool())
        .await?;
    let scan_again = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&scan_again.id, 100, None).await?;
    let probe_statuses: Vec<String> =
        sqlx::query_scalar("SELECT probe_status FROM media_sources ORDER BY id")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(probe_statuses, vec!["READY".to_owned(); 4]);
    Ok(())
}

#[tokio::test]
async fn inferred_variant_requires_a_regular_base_video_in_the_same_directory() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let variant_dir = temp_dir.path().join("Variants");
    let other_dir = temp_dir.path().join("Other");
    tokio::fs::create_dir_all(&variant_dir)
        .await
        .expect("variant directory");
    tokio::fs::create_dir_all(&other_dir)
        .await
        .expect("other directory");
    let variant_path = variant_dir.join("ADN-610-Alternate.mp4");
    tokio::fs::write(&variant_path, b"variant")
        .await
        .expect("variant file");
    tokio::fs::write(other_dir.join("ADN-610.mp4"), b"base elsewhere")
        .await
        .expect("base in a different directory");

    assert_eq!(
        infer_sibling_movie_variant_suffix(&variant_path).await,
        None
    );

    tokio::fs::write(variant_dir.join("ADN-610.mkv"), b"base")
        .await
        .expect("base video with a different supported extension");
    assert_eq!(
        infer_sibling_movie_variant_suffix(&variant_path)
            .await
            .as_deref(),
        Some("Alternate")
    );

    let provider_tagged = variant_dir.join("ADN-610-C [tmdbid=123].mp4");
    tokio::fs::write(&provider_tagged, b"provider tagged")
        .await
        .expect("tagged filename");
    assert_eq!(
        infer_sibling_movie_variant_suffix(&provider_tagged).await,
        None,
        "provider ID tags must not be mistaken for variant suffixes"
    );
    tokio::fs::remove_file(&provider_tagged)
        .await
        .expect("remove provider tagged filename");

    tokio::fs::remove_file(&variant_path)
        .await
        .expect("remove competing base candidate");
    let multi_hyphen_variant = variant_dir.join("ADN-610-Alternate-Cut.mp4");
    tokio::fs::write(&multi_hyphen_variant, b"multi-hyphen variant")
        .await
        .expect("multi-hyphen variant file");
    assert_eq!(
        infer_sibling_movie_variant_suffix(&multi_hyphen_variant)
            .await
            .as_deref(),
        Some("Alternate-Cut")
    );

    tokio::fs::write(&variant_path, b"variant")
        .await
        .expect("restore competing variant file");
    tokio::fs::write(variant_dir.join("ADN.mp4"), b"ambiguous base")
        .await
        .expect("second possible exact base");
    assert_eq!(
        infer_sibling_movie_variant_suffix(&variant_path).await,
        None,
        "more than one exact prefix match must not be guessed"
    );
}

#[tokio::test]
async fn inferred_variant_counts_candidate_metadata_probes() {
    let variant_path = PathBuf::from("/virtual/ADN-725-Alternate-Cut.mp4");
    let base_path = variant_path
        .parent()
        .expect("variant parent")
        .join("ADN-725.mp4");
    let expected_base_path = Arc::new(base_path);
    let probe_count = Arc::new(AtomicUsize::new(0));
    let inferred_suffix = infer_sibling_movie_variant_suffix_with_probe(&variant_path, {
        let expected_base_path = Arc::clone(&expected_base_path);
        let probe_count = Arc::clone(&probe_count);
        move |candidate| {
            let expected_base_path = Arc::clone(&expected_base_path);
            let probe_count = Arc::clone(&probe_count);
            async move {
                probe_count.fetch_add(1, Ordering::Relaxed);
                candidate == *expected_base_path
            }
        }
    })
    .await;

    assert_eq!(inferred_suffix.as_deref(), Some("Alternate-Cut"));
    assert_eq!(probe_count.load(Ordering::Relaxed), 13);
}

#[tokio::test]
async fn movie_variant_rescan_reuses_the_suffix_found_by_the_quick_check()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("ADN-725");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("ADN-725.mp4"), b"base").await?;
    let variant_path = movie_dir.join("ADN-725-Alternate-Cut.mp4");
    tokio::fs::write(&variant_path, b"variant").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF-8 media root")?,
        )
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_movie_library(library.id).await?;

    sqlx::query(
        "UPDATE media_sources SET edition_name = NULL
             WHERE filesystem_entry_id = (
                 SELECT id FROM filesystem_entries
                 WHERE relative_path = 'ADN-725/ADN-725-Alternate-Cut.mp4'
             )",
    )
    .execute(database.pool())
    .await?;
    scanner.scan_movie_library(library.id).await?;
    let edition: Option<String> = sqlx::query_scalar(
        "SELECT source.edition_name
             FROM media_sources source
             JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
             WHERE entry.relative_path = 'ADN-725/ADN-725-Alternate-Cut.mp4'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(edition.as_deref(), Some("Alternate-Cut"));

    tokio::fs::write(&variant_path, b"variant content changed").await?;
    let report = scanner.scan_movie_library(library.id).await?;
    assert_eq!(report.changed_files, 1);

    let edition: Option<String> = sqlx::query_scalar(
        "SELECT source.edition_name
             FROM media_sources source
             JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
             WHERE entry.relative_path = 'ADN-725/ADN-725-Alternate-Cut.mp4'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(edition.as_deref(), Some("Alternate-Cut"));
    Ok(())
}

#[test]
fn lite_manifest_discovery_requires_the_streamed_v3_contract() {
    assert!(is_lite_manifest_discovery(2, 3, "LITE"));
    assert!(!is_lite_manifest_discovery(1, 3, "LITE"));
    assert!(!is_lite_manifest_discovery(2, 2, "LITE"));
    assert!(!is_lite_manifest_discovery(2, 3, "PERSISTED"));
}

#[tokio::test]
async fn lite_manifest_session_loads_root_baselines_with_one_query()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library_service = LibraryService::new(database.clone());
    let library = library_service
        .create_library("Multi-root", LibraryKind::Movie, false)
        .await?;
    let mut root_ids = Vec::new();
    for index in 0..4 {
        let root_path = temp_dir.path().join(format!("root-{index}"));
        std::fs::create_dir_all(&root_path)?;
        let root = library_service
            .add_root(library.id, root_path.to_str().ok_or("non-UTF-8 root path")?)
            .await?
            .root;
        root_ids.push(root.id.to_string());
    }

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let manifest = database
        .get_scan_manifest_by_job(&job.id)
        .await?
        .ok_or("scan manifest was not created")?;
    for (root_id, state) in [(&root_ids[0], "COMPLETE"), (&root_ids[1], "UNAVAILABLE")] {
        sqlx::query(
            "UPDATE scan_manifest_roots SET state = ?
                 WHERE manifest_id = ? AND library_root_id = ?",
        )
        .bind(state)
        .bind(&manifest.id)
        .bind(root_id)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 last_seen_generation
             ) VALUES ('existing-entry', ?, 'existing.mkv', 'FILE', 1, 1, 'generation')",
    )
    .bind(&root_ids[3])
    .execute(database.pool())
    .await?;

    database.reset_query_count();
    jobs.ensure_lite_manifest_discovery_session(&manifest.id)
        .await?;
    assert_eq!(database.query_count(), 1);

    let directories = jobs.pop_lite_manifest_directories(&manifest.id, 10);
    assert_eq!(directories.len(), 2);
    let baselines = directories
        .into_iter()
        .map(|(root_id, relative_directory, has_filesystem_entries)| {
            assert!(relative_directory.is_empty());
            (root_id, has_filesystem_entries)
        })
        .collect::<HashMap<_, _>>();
    assert!(!baselines.contains_key(&root_ids[0]));
    assert!(!baselines.contains_key(&root_ids[1]));
    assert_eq!(baselines.get(&root_ids[2]), Some(&false));
    assert_eq!(baselines.get(&root_ids[3]), Some(&true));
    Ok(())
}

#[test]
fn manifest_discovery_budget_stays_within_streamed_file_budget() {
    assert_eq!(MANIFEST_DISCOVERY_BATCH_SIZE, 80);
    assert_eq!(MANIFEST_STREAMED_INDEX_BATCH_SIZE, 8_000);
    assert_eq!(MANIFEST_STREAMED_ENTRY_BATCH_SIZE, 8_192);
    const _: () =
        assert!(MANIFEST_DISCOVERY_BATCH_SIZE * 100 <= MANIFEST_STREAMED_INDEX_BATCH_SIZE);
    const _: () = assert!(MANIFEST_STREAMED_INDEX_BATCH_SIZE < MANIFEST_STREAMED_ENTRY_BATCH_SIZE);
}

#[tokio::test]
async fn manifest_single_chunk_commit_rejects_replaced_empty_directory()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root_path = temp_dir.path().join("Movies");
    let bucket_path = root_path.join("Bucket");
    std::fs::create_dir_all(&bucket_path)?;
    let root_path = std::fs::canonicalize(root_path)?;
    let bucket_path = root_path.join("Bucket");
    let root_record = libraries
        .add_root(library.id, root_path.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;
    let root = database
        .find_library_root(&root_record.id.to_string())
        .await?
        .ok_or("stored library root is missing")?;

    let reader = ManifestDirectoryReader::open(&root_path, "Bucket")?;
    let root_observation = reader.root_observation.clone();
    let directory_observation = reader.directory_observation.clone();
    let (reader, batch) = reader.next_batch(16)?;
    drop(reader);
    std::fs::rename(&bucket_path, root_path.join("Bucket.old"))?;
    std::fs::create_dir(&bucket_path)?;

    let service = ScanJobService::new(database);
    let cancellation = std::sync::atomic::AtomicBool::new(false);
    let child_directories = batch.child_directories;
    let entries = batch.entries;
    let incomplete_chunk = PendingManifestDirectoryChunk {
        relative_directory: "Bucket".to_owned(),
        root_observation: root_observation.clone(),
        directory_observation: directory_observation.clone(),
        child_directories: child_directories.clone(),
        entries: entries.clone(),
        completed_directory: None,
    };
    let result = service
        .commit_pending_manifest_directory_chunks(
            ManifestRootDiscoveryContext {
                job_id: "replacement-test-job",
                manifest_id: "replacement-test-manifest",
                root: &root,
                cancellation: &cancellation,
                stream_files_during_discovery: true,
                skip_baseline_queries: false,
                library_kind: "MOVIE",
                preparation_concurrency: 1,
                expected_root_identity: root_observation.device.zip(root_observation.inode),
            },
            &[incomplete_chunk],
        )
        .await;
    assert!(
        matches!(result, Err(ScannerError::RootIdentityChanged(_))),
        "incomplete unchanged-only chunks must not checkpoint after directory replacement"
    );

    let batched_chunk = PendingManifestDirectoryChunk {
        relative_directory: "Bucket".to_owned(),
        root_observation: root_observation.clone(),
        directory_observation: directory_observation.clone(),
        child_directories: child_directories.clone(),
        entries: entries.clone(),
        completed_directory: Some("Bucket".to_owned()),
    };
    let result = service
        .commit_pending_manifest_directory_chunks(
            ManifestRootDiscoveryContext {
                job_id: "replacement-test-job",
                manifest_id: "replacement-test-manifest",
                root: &root,
                cancellation: &cancellation,
                stream_files_during_discovery: true,
                skip_baseline_queries: false,
                library_kind: "MOVIE",
                preparation_concurrency: 1,
                expected_root_identity: root_observation.device.zip(root_observation.inode),
            },
            &[batched_chunk],
        )
        .await;
    assert!(
        matches!(result, Err(ScannerError::RootIdentityChanged(_))),
        "batched discovery must reject an empty directory replacement"
    );

    let single_chunk = NewScanManifestDiscoveryChunk {
        manifest_id: "replacement-test-manifest",
        job_id: "replacement-test-job",
        library_root_id: &root.id,
        child_directories: &child_directories,
        entries: &entries,
        positive_indexes: &[],
        unchanged_paths: &[],
        seen_filesystem_entries: &[],
        completed_directory: Some("Bucket"),
    };
    let result = service
        .commit_scan_manifest_discovery_chunk(
            &single_chunk,
            &cancellation,
            true,
            root_path,
            root_observation,
            directory_observation,
        )
        .await;
    assert!(
        matches!(result, Err(ScannerError::RootIdentityChanged(_))),
        "single-directory discovery must reject an empty directory replacement"
    );
    Ok(())
}

#[tokio::test]
async fn full_manifest_scan_refreshes_home_once_before_postprocessing()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::{
            access::{AccessPrincipal, MediaAccessService},
            admin_events::{UserEventHub, UserEventScope},
            catalog::CatalogService,
            home::HomeService,
            libraries::LibraryService,
            setup::SetupService,
            webhooks::WebhookService,
        },
        config::{Config, DatabaseConfiguration, PostgresConnection},
        library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let postgres_backend =
        std::env::var("LUX_SCAN_TEST_BACKEND").is_ok_and(|backend| backend == "postgres");
    let database = if postgres_backend {
        let database_name = std::env::var("POSTGRES_TEST_DATABASE")?;
        if matches!(
            database_name.as_str(),
            "postgres" | "template0" | "template1"
        ) {
            return Err("POSTGRES_TEST_DATABASE must name a disposable empty database".into());
        }
        Database::connect_with_configuration(
            &config,
            &DatabaseConfiguration::Postgres(PostgresConnection {
                host: std::env::var("POSTGRES_TEST_HOST")
                    .unwrap_or_else(|_| "127.0.0.1".to_owned()),
                port: std::env::var("POSTGRES_TEST_PORT")
                    .unwrap_or_else(|_| "55432".to_owned())
                    .parse()?,
                database: database_name,
                username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
                password: std::env::var("POSTGRES_TEST_PASSWORD")
                    .unwrap_or_else(|_| "lux-test-password".to_owned()),
                ssl_mode: "disable".to_owned(),
            }),
        )
        .await?
    } else {
        Database::connect(&config).await?
    };
    let admin = SetupService::new(database.clone())?
        .complete("Admin", "Admin", "correct password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, true)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Home.Refresh.2025.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-UTF-8 root path")?)
        .await?;

    let home = HomeService::new(
        CatalogService::new(database.clone(), MediaAccessService::new(database.clone())),
        libraries,
    );
    let principal = AccessPrincipal::new(admin.id, true);
    let library_ids = [library.id.to_string()];
    let before = home.snapshot(principal, library_ids.to_vec()).await?;
    assert_eq!(before.recently_added.total, 0);

    let user_events = UserEventHub::new();
    let mut receiver = user_events.subscribe();
    let webhooks = WebhookService::new(database.clone(), config.config_dir.clone())?;
    webhooks
        .create_destination(
            "Scan completion test",
            "http://127.0.0.1:8098/hook",
            true,
            true,
            &["SCAN_COMPLETED".to_owned()],
            None,
        )
        .await?;
    let jobs = ScanJobService::new(database.clone())
        .with_home(home.clone())
        .with_user_events(user_events)
        .with_webhooks(webhooks);
    let job = jobs.create_movie_scan_job(library.id).await?;
    loop {
        let report = jobs.run_batch(&job.id, 100).await?;
        if report.completed {
            assert_eq!(report.status, "COMPLETED");
            break;
        }
    }

    let stored_job = database
        .find_scan_job(&job.id)
        .await?
        .ok_or("scan job disappeared")?;
    assert_eq!(stored_job.scan_phase, "POSTPROCESSING");
    assert_eq!(receiver.try_recv(), Ok(UserEventScope::Home));
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let dedupe_placeholder = if postgres_backend { "$1" } else { "?" };
    let scan_completed_query = format!(
        "SELECT COUNT(*) FROM notification_events
             WHERE event_type = 'SCAN_COMPLETED' AND dedupe_key = {dedupe_placeholder}"
    );
    let scan_completed_events: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(scan_completed_query))
        .bind(format!("scan:{}:SCAN_COMPLETED", job.id))
        .fetch_one(database.pool())
        .await?;
    assert_eq!(scan_completed_events, 1);
    let after_index = home.snapshot(principal, library_ids.to_vec()).await?;
    assert_eq!(after_index.recently_added.total, 1);

    let repeated_postprocessing_batch = jobs.run_batch(&job.id, 100).await?;
    assert_eq!(repeated_postprocessing_batch.status, "COMPLETED");

    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    Ok(())
}

#[tokio::test]
async fn local_metadata_image_waiter_wakes_when_batch_images_complete()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?
        .root;
    let job_id = "image-waiter-job";
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, scan_phase)
             VALUES (?, ?, 'RECONCILE_LIBRARY', 'RUNNING', 'generation', 'POSTPROCESSING')",
    )
    .bind(job_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_local_metadata_batches
             (id, job_id, library_root_id, batch_sequence, source_refs_json, source_count, status)
             VALUES ('image-waiter-batch', ?, ?, 0, '[\"source\"]', 1, 'RUNNING')",
    )
    .bind(job_id)
    .bind(root.id.to_string())
    .execute(database.pool())
    .await?;

    let jobs = ScanJobService::new(database.clone());
    let waiter_jobs = jobs.clone();
    let waiter =
        tokio::spawn(async move { waiter_jobs.wait_for_local_metadata_images(job_id).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!waiter.is_finished(), "the image batch is still pending");

    assert!(
        database
            .mark_scan_local_metadata_images_complete("image-waiter-batch")
            .await?
    );
    notify_local_metadata_images_waiter(
        &jobs.local_metadata_image_notifications,
        "image-waiter-job",
    );
    tokio::time::timeout(std::time::Duration::from_millis(250), waiter)
        .await
        .expect("image waiter should wake before the fallback")??;
    Ok(())
}

#[tokio::test]
async fn local_metadata_completion_waiter_uses_notification_before_fallback()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let job_id = "local-metadata-completion-waiter";
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, scan_phase)
             VALUES (?, ?, 'RECONCILE_LIBRARY', 'RUNNING', 'generation', 'POSTPROCESSING')",
    )
    .bind(job_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_job_targets
             (job_id, target_type, target_id, item_id, change_kind, metadata_state)
             VALUES (?, 'ITEM', 'item-1', 'item-1', 'NEW', 'PENDING')",
    )
    .bind(job_id)
    .execute(database.pool())
    .await?;

    let jobs = ScanJobService::new(database.clone());
    let notify = Arc::new(Notify::new());
    jobs.metadata_notifications
        .lock()
        .map_err(|_| "metadata notification registry poisoned")?
        .insert(job_id.to_owned(), Arc::clone(&notify));
    let waiter_jobs = jobs.clone();
    let waiter = tokio::spawn(async move { waiter_jobs.wait_for_local_metadata(job_id).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    sqlx::query(
        "UPDATE scan_job_targets SET metadata_state = 'DONE'
             WHERE job_id = ? AND target_id = 'item-1'",
    )
    .bind(job_id)
    .execute(database.pool())
    .await?;
    notify.notify_waiters();
    tokio::time::timeout(std::time::Duration::from_millis(250), waiter)
        .await
        .expect("completion notification should beat the fallback")??;
    Ok(())
}

#[tokio::test]
async fn local_metadata_completion_waiter_wakes_when_scan_is_cancelled()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let job_id = "local-metadata-cancelled-waiter";
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, scan_phase)
             VALUES (?, ?, 'RECONCILE_LIBRARY', 'RUNNING', 'generation', 'POSTPROCESSING')",
    )
    .bind(job_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_job_targets
             (job_id, target_type, target_id, item_id, change_kind, metadata_state)
             VALUES (?, 'ITEM', 'item-1', 'item-1', 'NEW', 'PENDING')",
    )
    .bind(job_id)
    .execute(database.pool())
    .await?;

    let jobs = ScanJobService::new(database.clone());
    let notify = Arc::new(Notify::new());
    jobs.metadata_notifications
        .lock()
        .map_err(|_| "metadata notification registry poisoned")?
        .insert(job_id.to_owned(), Arc::clone(&notify));
    let waiter_jobs = jobs.clone();
    let waiter_job_id = job_id.to_owned();
    let (completed_sender, completed_receiver) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(async move {
        let result = waiter_jobs.wait_for_local_metadata(&waiter_job_id).await;
        let _ = completed_sender.send(());
        result
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    jobs.cancel(job_id).await?;
    let woke_before_fallback =
        tokio::time::timeout(std::time::Duration::from_millis(250), completed_receiver)
            .await
            .is_ok();
    if !woke_before_fallback {
        notify.notify_waiters();
    }
    waiter.await??;
    assert!(
        woke_before_fallback,
        "same-process cancellation should wake the waiter without the 5-second fallback"
    );
    Ok(())
}

#[test]
fn local_metadata_completion_waiter_keeps_five_second_fallback() {
    assert_eq!(
        LOCAL_METADATA_COMPLETION_FALLBACK,
        std::time::Duration::from_secs(5)
    );
}

#[tokio::test]
async fn local_metadata_worker_does_not_reload_scan_job_after_notification()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let job_id = "local-metadata-idle-worker";
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, scan_phase)
             VALUES (?, ?, 'RECONCILE_LIBRARY', 'RUNNING', 'generation', 'POSTPROCESSING')",
    )
    .bind(job_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    let jobs = ScanJobService::new(database.clone());
    database.reset_query_count();
    let mut worker = Some(jobs.start_local_metadata_worker(job_id));
    tokio::time::timeout(
        std::time::Duration::from_millis(250),
        worker
            .as_mut()
            .expect("worker handle should be available")
            .idle_page_receiver
            .recv(),
    )
    .await?
    .expect("worker should finish its startup page before waiting");
    assert_eq!(
        database.query_count(),
        1,
        "worker startup should load job lifecycle with its first metadata page"
    );
    database.reset_query_count();
    jobs.notify_local_metadata_worker(job_id);
    tokio::time::timeout(
        std::time::Duration::from_millis(250),
        worker
            .as_mut()
            .expect("worker handle should be available")
            .idle_page_receiver
            .recv(),
    )
    .await?
    .expect("notification should trigger one lifecycle page read");
    let notification_queries = database.query_count();
    ScanJobService::stop_local_metadata_worker(&mut worker).await;
    assert_eq!(notification_queries, 1);
    sqlx::query("UPDATE scan_jobs SET status = 'FAILED' WHERE id = ?")
        .bind(job_id)
        .execute(database.pool())
        .await?;

    for (terminal_job_id, terminal_status) in [
        ("local-metadata-failed-worker", "FAILED"),
        ("local-metadata-cancelled-worker", "CANCELLED"),
    ] {
        sqlx::query(
            "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, scan_phase)
                 VALUES (?, ?, 'RECONCILE_LIBRARY', 'RUNNING', 'generation', 'POSTPROCESSING')",
        )
        .bind(terminal_job_id)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;

        database.reset_query_count();
        let mut terminal_worker = Some(jobs.start_local_metadata_worker(terminal_job_id));
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            terminal_worker
                .as_mut()
                .expect("worker handle should be available")
                .idle_page_receiver
                .recv(),
        )
        .await?
        .expect("worker should finish its startup page before waiting");
        assert_eq!(database.query_count(), 1);

        sqlx::query("UPDATE scan_jobs SET status = ? WHERE id = ?")
            .bind(terminal_status)
            .bind(terminal_job_id)
            .execute(database.pool())
            .await?;
        database.reset_query_count();
        jobs.notify_local_metadata_worker(terminal_job_id);
        tokio::time::timeout(std::time::Duration::from_millis(250), async {
            while !terminal_worker
                .as_ref()
                .is_some_and(|worker| worker.task.is_finished())
            {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        assert_eq!(database.query_count(), 1);
        ScanJobService::stop_local_metadata_worker(&mut terminal_worker).await;
    }

    database.reset_query_count();
    let mut missing_worker =
        Some(jobs.start_local_metadata_worker("local-metadata-missing-worker"));
    tokio::time::timeout(std::time::Duration::from_millis(250), async {
        while !missing_worker
            .as_ref()
            .is_some_and(|worker| worker.task.is_finished())
        {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    assert_eq!(database.query_count(), 1);
    ScanJobService::stop_local_metadata_worker(&mut missing_worker).await;
    Ok(())
}

#[tokio::test]
async fn local_metadata_worker_defers_targets_when_manifest_root_identity_changes()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    tokio::fs::write(media_root.join("Movie.mkv"), b"movie").await?;
    tokio::fs::write(
        media_root.join("Movie.nfo"),
        "<movie><title>Must Not Be Read</title></movie>",
    )
    .await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?
        .root;
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let manifest_id: String = sqlx::query_scalar("SELECT id FROM scan_manifests WHERE job_id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_roots (
                 manifest_id, library_root_id, state, postprocessing_target_stage
             ) VALUES (?, ?, 'COMPLETE', 'NEW')
             ON CONFLICT(manifest_id, library_root_id) DO UPDATE SET
                 state = 'COMPLETE', postprocessing_target_stage = 'NEW'",
    )
    .bind(&manifest_id)
    .bind(root.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_entries (
                 manifest_id, library_root_id, relative_path, observation_sequence,
                 entry_kind, size, modified_at, device, inode
             ) VALUES (?, ?, '', 1, 'DIRECTORY', 0, 1, 0, 0)
             ON CONFLICT(manifest_id, library_root_id, relative_path, observation_sequence)
             DO UPDATE SET device = 0, inode = 0",
    )
    .bind(&manifest_id)
    .bind(root.id.to_string())
    .execute(database.pool())
    .await?;
    let item_id = "manifest-root-mismatch-item";
    sqlx::query(
        "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Original title', 'original title', 'LOCAL_CONFIRMED')",
    )
    .bind(item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 last_seen_generation, last_seen_change_kind
             ) VALUES ('manifest-root-mismatch-entry', ?, 'Movie.mkv', 'FILE',
                       5, 1, ?, 'NEW')",
    )
    .bind(root.id.to_string())
    .bind(&job.generation)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_sources (
                 id, item_id, source_kind, filesystem_entry_id, is_default
             ) VALUES ('manifest-root-mismatch-source', ?, 'LOCAL_FILE',
                       'manifest-root-mismatch-entry', 1)",
    )
    .bind(item_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_job_targets (
                 job_id, target_type, target_id, item_id, change_kind, metadata_state
             ) VALUES (?, 'ITEM', ?, ?, 'NEW', 'PENDING')",
    )
    .bind(&job.id)
    .bind(item_id)
    .bind(item_id)
    .execute(database.pool())
    .await?;

    let mut worker = Some(jobs.start_local_metadata_worker(&job.id));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let target_state: String = sqlx::query_scalar(
                "SELECT metadata_state FROM scan_job_targets
                     WHERE job_id = ? AND item_id = ?",
            )
            .bind(&job.id)
            .bind(item_id)
            .fetch_one(database.pool())
            .await?;
            if target_state == "FAILED" {
                break Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await??;
    ScanJobService::stop_local_metadata_worker(&mut worker).await;

    let title: String = sqlx::query_scalar("SELECT title FROM media_items WHERE id = ?")
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(title, "Original title");
    let root_available: i64 =
        sqlx::query_scalar("SELECT is_available FROM library_roots WHERE id = ?")
            .bind(root.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(root_available, 0);
    Ok(())
}

#[test]
fn local_metadata_worker_job_state_fallback_is_five_seconds() {
    assert_eq!(
        LOCAL_METADATA_JOB_REFRESH_FALLBACK,
        std::time::Duration::from_secs(5)
    );
}

#[tokio::test]
async fn local_metadata_worker_persists_capability_missing()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::{
            candidates::MetadataSelectionService, images::ImageWriteService,
            libraries::LibraryService, reidentify::MetadataReidentifyService,
            scraper::ScraperProvider,
        },
        config::Config,
        library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Local Completeness (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Local.Completeness.2024.mkv"), b"movie").await?;
    tokio::fs::write(
        movie_dir.join("Local.Completeness.2024.nfo"),
        "<movie><title>NFO Completeness Title</title></movie>",
    )
    .await?;
    tokio::fs::write(movie_dir.join("poster.jpg"), b"poster").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id = sqlx::query_scalar::<_, String>(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    sqlx::query(
        "CREATE TRIGGER fail_capability_nfo_update
             BEFORE UPDATE OF title ON media_items
             WHEN OLD.item_type = 'MOVIE'
             BEGIN SELECT RAISE(ABORT, 'injected capability NFO failure'); END",
    )
    .execute(database.pool())
    .await?;

    let images =
        ImageWriteService::new_with_config_dir(database.clone(), config.config_dir.clone())?;
    let selection = MetadataSelectionService::with_config_dir(
        database.clone(),
        images,
        config.config_dir.clone(),
    );
    let reidentify = MetadataReidentifyService::with_selection(
        database.clone(),
        ScraperProvider::unconfigured(),
        Some(selection.clone()),
    );
    let jobs = ScanJobService::new(database.clone())
        .with_metadata_selection(selection)
        .with_metadata_reidentify(reidentify);
    jobs.start_local_metadata_outbox_worker().await?;

    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status: String = sqlx::query_scalar(
                "SELECT status FROM scan_local_metadata_backfills
                     WHERE library_root_id = (SELECT id FROM library_roots WHERE library_id = ?)",
            )
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
            if status == "FAILED" {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM item_metadata_completeness WHERE item_id = ?",
        )
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?,
        0,
        "a failed local NFO read cannot confirm missing capabilities"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM item_images WHERE item_id = ? AND image_type = 'POSTER'",
        )
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?,
        1,
        "poster is still available before the NFO retry"
    );
    sqlx::query("DROP TRIGGER fail_capability_nfo_update")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "UPDATE scan_local_metadata_backfills SET next_attempt_at = 0
             WHERE library_root_id = (SELECT id FROM library_roots WHERE library_id = ?)",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let backfill_status: String = sqlx::query_scalar(
                "SELECT status FROM scan_local_metadata_backfills
                     WHERE library_root_id = (SELECT id FROM library_roots WHERE library_id = ?)",
            )
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
            let poster: Option<(String, Option<i64>)> = sqlx::query_as(
                "SELECT local_state, is_missing FROM item_metadata_completeness
                     WHERE item_id = ? AND capability = 'POSTER'",
            )
            .bind(&item_id)
            .fetch_optional(database.pool())
            .await?;
            let metadata: Option<(String, Option<i64>)> = sqlx::query_as(
                "SELECT local_state, is_missing FROM item_metadata_completeness
                     WHERE item_id = ? AND capability = 'METADATA'",
            )
            .bind(&item_id)
            .fetch_optional(database.pool())
            .await?;
            if backfill_status == "COMPLETED"
                && poster
                    .as_ref()
                    .is_some_and(|state| state == &("READY".to_owned(), Some(0)))
                && metadata
                    .as_ref()
                    .is_some_and(|state| state == &("READY".to_owned(), Some(1)))
            {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;

    let outbox_movie_dir = media_root.join("Outbox Completeness (2025)");
    tokio::fs::create_dir_all(&outbox_movie_dir).await?;
    tokio::fs::write(
        outbox_movie_dir.join("Outbox.Completeness.2025.mkv"),
        b"movie",
    )
    .await?;
    tokio::fs::write(
        outbox_movie_dir.join("Outbox.Completeness.2025.nfo"),
        "<movie><title>Outbox Completeness</title></movie>",
    )
    .await?;
    tokio::fs::write(outbox_movie_dir.join("poster.jpg"), b"poster").await?;
    let scan_job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&scan_job.id, 100, None).await?;
    let outbox_item_id = sqlx::query_scalar::<_, String>(
        "SELECT item.id FROM media_items item
                 JOIN media_sources source ON source.item_id = item.id
                 JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
                 WHERE entry.relative_path LIKE 'Outbox Completeness (2025)/%'
                   AND item.library_id = ? AND item.item_type = 'MOVIE'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let batch_status: Option<String> = sqlx::query_scalar(
                "SELECT status FROM scan_local_metadata_batches WHERE job_id = ? LIMIT 1",
            )
            .bind(&scan_job.id)
            .fetch_optional(database.pool())
            .await?;
            let poster: Option<(String, Option<i64>)> = sqlx::query_as(
                "SELECT local_state, is_missing FROM item_metadata_completeness
                     WHERE item_id = ? AND capability = 'POSTER'",
            )
            .bind(&outbox_item_id)
            .fetch_optional(database.pool())
            .await?;
            if batch_status.as_deref() == Some("COMPLETED")
                && poster
                    .as_ref()
                    .is_some_and(|state| state == &("READY".to_owned(), Some(0)))
            {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;

    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'",
        )
        .fetch_one(database.pool())
        .await?,
        0,
        "capability checks store missing state without dispatch in this task"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "stage 23 synthetic automatic online queue measurement; requires LUX_PERF_MEDIA_ROOT"]
async fn stage23_online_fill_missing_benchmark() -> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::{
            candidates::MetadataSelectionService, images::ImageWriteService,
            libraries::LibraryService, nfo::LocalNfoMetadataStore,
            reidentify::MetadataReidentifyService,
        },
        config::{Config, DatabaseConfiguration, PostgresConnection},
        library::LibraryKind,
        storage::Database,
    };
    use std::time::{Duration, Instant};
    let count: usize = std::env::var("LUX_PERF_FILE_COUNT")?.parse()?;
    let root = std::path::PathBuf::from(std::env::var("LUX_PERF_MEDIA_ROOT")?);
    let backend = std::env::var("LUX_PERF_BACKEND").unwrap_or_else(|_| "sqlite".into());
    let configuration = match backend.as_str() {
        "sqlite" => DatabaseConfiguration::Sqlite,
        "postgres" => {
            let name = std::env::var("POSTGRES_TEST_DATABASE")?;
            if !name.starts_with("s23_") {
                return Err("benchmark requires an s23_ disposable database".into());
            }
            DatabaseConfiguration::Postgres(PostgresConnection {
                host: "127.0.0.1".into(),
                port: std::env::var("POSTGRES_TEST_PORT")?.parse()?,
                database: name,
                username: std::env::var("POSTGRES_TEST_USER")?,
                password: std::env::var("POSTGRES_TEST_PASSWORD")?,
                ssl_mode: "disable".into(),
            })
        }
        _ => return Err("unsupported benchmark database backend".into()),
    };
    let temp = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:0".parse()?,
        config_dir: temp.path().join("config"),
    };
    let database = Database::connect_with_configuration(&config, &configuration).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Online fixture", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-UTF8 fixture")?)
        .await?;
    let provider = GateSearchScraper {
        search_started: Arc::new(Notify::new()),
        release_search: Arc::new(Semaphore::new(0)),
    };
    let selection = MetadataSelectionService::with_config_dir(
        database.clone(),
        ImageWriteService::new_with_config_dir(database.clone(), config.config_dir.clone())?,
        config.config_dir.clone(),
    );
    let metadata = MetadataReidentifyService::with_selection(
        database.clone(),
        ScraperProvider::from_adapter(provider.clone()),
        Some(selection.clone()),
    );
    let jobs = ScanJobService::new(database.clone())
        .with_nfo_store(LocalNfoMetadataStore::new(database.clone()))
        .with_metadata_selection(selection)
        .with_metadata_reidentify(metadata);
    jobs.start_local_metadata_outbox_worker().await?;
    let job = jobs.create_movie_scan_job(library.id).await?;
    let started = Instant::now();
    let scan_jobs = jobs.clone();
    let job_id = job.id.clone();
    let scan = tokio::spawn(async move {
        scan_jobs.run_to_completion(&job_id, 500, None).await?;
        Ok::<_, super::ScanJobError>(started.elapsed().as_nanos())
    });
    tokio::time::timeout(Duration::from_secs(600), provider.search_started.notified()).await?;
    let first_provider_request_ns = started.elapsed().as_nanos();
    let scan_complete_ns = tokio::time::timeout(Duration::from_secs(600), scan).await???;
    let bind = if backend == "postgres" { "$1" } else { "?" };
    let local_query = format!(
        "SELECT COUNT(*), COALESCE(SUM(CASE WHEN status <> 'COMPLETED' OR images_completed_at IS NULL THEN 1 ELSE 0 END), 0) FROM scan_local_metadata_batches WHERE job_id = {bind}"
    );
    let deadline = Instant::now() + Duration::from_secs(600);
    let local_complete_ns = loop {
        let (batches, unfinished): (i64, i64) =
            sqlx::query_as(sqlx::AssertSqlSafe(local_query.clone()))
                .bind(&job.id)
                .fetch_one(database.pool())
                .await?;
        if batches > 0 && unfinished == 0 {
            break started.elapsed().as_nanos();
        }
        if Instant::now() > deadline {
            return Err("local queue timed out while provider was blocked".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let queue_query = format!(
        "SELECT COUNT(*), COALESCE(SUM(CASE WHEN status IN ('QUEUED', 'RUNNING') THEN 1 ELSE 0 END), 0), COALESCE(MAX(total_count), 0) FROM metadata_reidentify_jobs WHERE library_id = {bind} AND mode = 'FILL_MISSING'"
    );
    let (jobs_before_release, active_before_release, max_items_before_release): (i64, i64, i64) =
        sqlx::query_as(sqlx::AssertSqlSafe(queue_query.clone()))
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert!(
        jobs_before_release > 0 && active_before_release > 0,
        "the provider gate must leave real online jobs active"
    );
    let release_ns = started.elapsed().as_nanos();
    // Title fallback may issue several searches per item. After the gate
    // opens, keep every synthetic search unblocked until the queue drains.
    provider
        .release_search
        .add_permits(Semaphore::MAX_PERMITS / 2);
    let item_query = format!(
        "SELECT COUNT(*), COALESCE(SUM(CASE WHEN item.status = 'COMPLETED' AND item.error = 'LOW_CONFIDENCE' AND item.automatic_retry_count = 0 AND item.automatic_retry_after IS NULL THEN 1 ELSE 0 END), 0) FROM metadata_reidentify_job_items item JOIN metadata_reidentify_jobs job ON job.id = item.job_id WHERE job.library_id = {bind} AND job.mode = 'FILL_MISSING'"
    );
    let mut peak_jobs = active_before_release;
    let (online_complete_ns, job_count, item_count) = loop {
        let (job_count, active, max_items): (i64, i64, i64) =
            sqlx::query_as(sqlx::AssertSqlSafe(queue_query.clone()))
                .bind(library.id.to_string())
                .fetch_one(database.pool())
                .await?;
        let (items, successful): (i64, i64) =
            sqlx::query_as(sqlx::AssertSqlSafe(item_query.clone()))
                .bind(library.id.to_string())
                .fetch_one(database.pool())
                .await?;
        peak_jobs = peak_jobs.max(active);
        assert!(max_items <= 100 && max_items_before_release <= 100);
        assert!(
            items <= count as i64,
            "duplicate automatic work in synthetic fixture"
        );
        if items == count as i64 && successful == items && active == 0 {
            break (started.elapsed().as_nanos(), job_count, items);
        }
        if Instant::now() > deadline {
            return Err(format!("online drain timed out: jobs={job_count}, active={active}, items={items}, lowConfidence={successful}").into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(scan_complete_ns < release_ns && local_complete_ns < release_ns);
    println!(
        "STAGE23 ONLINE RESULT {}",
        serde_json::json!({
            "commit": crate::COMMIT, "architecture": std::env::consts::ARCH,
            "databaseBackend": backend, "fileCount": count,
            "firstProviderRequestMs": first_provider_request_ns / 1_000_000,
            "scanJobCompletionMs": scan_complete_ns / 1_000_000,
            "localQueueCompleteMs": local_complete_ns / 1_000_000,
            "providerGateReleasedMs": release_ns / 1_000_000,
            "onlineQueueCompleteMs": online_complete_ns / 1_000_000,
            "onlineDrainAfterProviderReleaseMs": (online_complete_ns - release_ns) / 1_000_000,
            "onlineFillMissingJobCount": job_count, "onlineItemCount": item_count,
            "maxObservedActiveJobs": peak_jobs, "maxItemsPerJobBound": 100,
            "scanAndLocalQueueCompletedWhileProviderBlocked": true,
            "providerMode": "synthetic empty search; held until scan and local queue complete",
            "timingNote": "providerGateReleasedMs includes intentional gate hold; onlineDrainAfterProviderReleaseMs measures scheduling/storage drain with an immediate empty provider, not real network latency",
        })
    );
    Ok(())
}

#[tokio::test]
async fn local_metadata_worker_dispatches_fill_missing_without_blocking_scan()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::{
            admin_events::{UserEventHub, UserEventScope},
            candidates::MetadataSelectionService,
            images::ImageWriteService,
            libraries::LibraryService,
            nfo::LocalNfoMetadataStore,
            reidentify::MetadataReidentifyService,
        },
        config::Config,
        library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Dispatch Missing Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Dispatch.Missing.Movie.2024.mkv"), b"movie").await?;
    tokio::fs::write(
        movie_dir.join("Dispatch.Missing.Movie.2024.nfo"),
        "<movie><title>Dispatch Missing Movie</title></movie>",
    )
    .await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?;

    let gate_scraper = GateSearchScraper {
        search_started: Arc::new(Notify::new()),
        release_search: Arc::new(Semaphore::new(0)),
    };
    let search_started = gate_scraper.search_started.notified();
    let images =
        ImageWriteService::new_with_config_dir(database.clone(), config.config_dir.clone())?;
    let selection = MetadataSelectionService::with_config_dir(
        database.clone(),
        images,
        config.config_dir.clone(),
    );
    let metadata = MetadataReidentifyService::with_selection(
        database.clone(),
        ScraperProvider::from_adapter(gate_scraper.clone()),
        Some(selection.clone()),
    );
    let user_events = UserEventHub::new();
    let jobs = ScanJobService::new(database.clone())
        .with_nfo_store(LocalNfoMetadataStore::new(database.clone()))
        .with_user_events(user_events.clone())
        .with_metadata_selection(selection)
        .with_metadata_reidentify(metadata);
    jobs.start_local_metadata_outbox_worker().await?;
    let scan_job = jobs.create_movie_scan_job(library.id).await?;
    let scan_job_id = scan_job.id.clone();
    let scanner = jobs.clone();
    let scan_run =
        tokio::spawn(async move { scanner.run_to_completion(&scan_job_id, 100, None).await });

    tokio::time::timeout(std::time::Duration::from_secs(10), search_started).await?;
    scan_run.await??;
    let scan_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&scan_job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(scan_status, "COMPLETED");

    let fill_job: (String, i64, String) = sqlx::query_as(
        "SELECT id, total_count, status FROM metadata_reidentify_jobs
             WHERE mode = 'FILL_MISSING' ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(fill_job.1, 1);
    assert_eq!(
        fill_job.2, "RUNNING",
        "the provider worker is independently running"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM metadata_reidentify_jobs
                 WHERE mode = 'FILL_MISSING' AND library_id = ?",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?,
        1,
        "workflow 3 does not enqueue a second terminal library job"
    );
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let mut user_events_receiver = user_events.subscribe();
    gate_scraper.release_search.add_permits(16);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status: String =
                sqlx::query_scalar("SELECT status FROM metadata_reidentify_jobs WHERE id = ?")
                    .bind(&fill_job.0)
                    .fetch_one(database.pool())
                    .await?;
            if !matches!(status.as_str(), "QUEUED" | "RUNNING") {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    let fill_item: (String, Option<String>, i64, Option<i64>) = sqlx::query_as(
        "SELECT status, error, automatic_retry_count, automatic_retry_after
             FROM metadata_reidentify_job_items
             WHERE job_id = ?",
    )
    .bind(&fill_job.0)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        fill_item.0, "COMPLETED",
        "fill-missing result: {fill_item:?}"
    );
    assert_eq!(
        fill_item.1.as_deref(),
        Some("LOW_CONFIDENCE"),
        "an empty provider search is a no-candidate result, not provider unavailability"
    );
    assert_eq!(
        fill_item.2, 0,
        "no-candidate results do not consume retry budget"
    );
    assert_eq!(
        fill_item.3, None,
        "no-candidate results do not enter cooldown"
    );
    let home_event = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        user_events_receiver.recv(),
    )
    .await??;
    assert_eq!(home_event, UserEventScope::Home);
    Ok(())
}

#[tokio::test]
async fn incremental_metadata_dispatch_uses_the_scan_job_policy_snapshot()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::{
            candidates::MetadataSelectionService, images::ImageWriteService,
            libraries::LibraryService, nfo::LocalNfoMetadataStore,
            reidentify::MetadataReidentifyService, scraper::ScraperProvider,
        },
        config::Config,
        library::LibraryKind,
        storage::Database,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Shows");
    let series_dir = media_root.join("Incremental Policy Show (2024)");
    let season_dir = series_dir.join("Season 01");
    tokio::fs::create_dir_all(&season_dir).await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Incremental policy", LibraryKind::Series, false)
        .await?;
    let root = libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?;
    let library_id = library.id.to_string();
    sqlx::query(
        "UPDATE libraries
             SET realtime_metadata_auto_match_enabled = 1, scraper_id = 'scan-test-provider'
             WHERE id = ?",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;

    let gate_scraper = GateSearchScraper {
        search_started: Arc::new(Notify::new()),
        release_search: Arc::new(Semaphore::new(16)),
    };
    let images =
        ImageWriteService::new_with_config_dir(database.clone(), config.config_dir.clone())?;
    let selection = MetadataSelectionService::with_config_dir(
        database.clone(),
        images,
        config.config_dir.clone(),
    );
    let metadata = MetadataReidentifyService::with_selection(
        database.clone(),
        ScraperProvider::from_adapter(gate_scraper),
        Some(selection.clone()),
    );
    let jobs = ScanJobService::new(database.clone())
        .with_nfo_store(LocalNfoMetadataStore::new(database.clone()))
        .with_metadata_selection(selection)
        .with_metadata_reidentify(metadata.clone());
    let initial_scan = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion_with_metadata(&initial_scan.id, 100, None, Some(metadata.clone()))
        .await?;
    tokio::fs::write(
        season_dir.join("Incremental.Policy.Show.S01E01.mkv"),
        b"episode",
    )
    .await?;
    tokio::fs::write(
        season_dir.join("Incremental.Policy.Show.S01E02.mkv"),
        b"episode",
    )
    .await?;
    let scan_job = jobs
            .enqueue_incremental_changes(
                library.id,
                [1, 2]
                    .into_iter()
                    .map(|episode_number| super::IncrementalScanChange {
                        root_id: root.root.id.to_string(),
                        relative_path: format!(
                            "Incremental Policy Show (2024)/Season 01/Incremental.Policy.Show.S01E{episode_number:02}.mkv"
                        ),
                        kind: crate::application::watch::ChangeKind::Create,
                    })
                    .collect(),
            )
            .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT auto_metadata_match FROM scan_jobs WHERE id = ?",)
            .bind(&scan_job.id)
            .fetch_one(database.pool())
            .await?,
        1
    );
    sqlx::query("UPDATE libraries SET realtime_metadata_auto_match_enabled = 0 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;

    jobs.run_to_completion_with_metadata(&scan_job.id, 100, None, Some(metadata.clone()))
        .await?;
    let incremental_item_id = sqlx::query_scalar::<_, String>(
        "SELECT item.id FROM media_items item
             JOIN media_sources source ON source.item_id = item.id
             JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
             WHERE item.library_id = ? AND item.item_type = 'EPISODE'
               AND entry.relative_path LIKE 'Incremental Policy Show (2024)/%'",
    )
    .bind(&library_id)
    .fetch_one(database.pool())
    .await?;
    let hierarchy_types: Vec<String> = sqlx::query_scalar(
        "SELECT item_type FROM media_items WHERE library_id = ?
             ORDER BY CASE item_type WHEN 'SERIES' THEN 0 WHEN 'SEASON' THEN 1 ELSE 2 END",
    )
    .bind(&library_id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        hierarchy_types,
        vec!["SERIES", "SEASON", "EPISODE", "EPISODE"],
        "the incremental fixture must contain the episode hierarchy"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM item_metadata_completeness
                 WHERE item_id = ? AND capability = 'METADATA'
                   AND local_state = 'READY' AND is_missing = 1",
        )
        .bind(&incremental_item_id)
        .fetch_one(database.pool())
        .await?,
        1,
        "incremental local metadata must persist missing episode metadata before online dispatch"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM metadata_reidentify_jobs
                 WHERE mode = 'FILL_MISSING' AND library_id = ?",
        )
        .bind(&library_id)
        .fetch_one(database.pool())
        .await?,
        1,
        "an opted-in incremental job keeps its creation-time policy snapshot"
    );

    let first_fill_missing_job_id = sqlx::query_scalar::<_, String>(
        "SELECT id FROM metadata_reidentify_jobs
             WHERE mode = 'FILL_MISSING' AND library_id = ?
             ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(&library_id)
    .fetch_one(database.pool())
    .await?;
    let first_fill_missing_item_types: Vec<String> = sqlx::query_scalar(
        "SELECT item.item_type FROM metadata_reidentify_job_items job_item
             JOIN media_items item ON item.id = job_item.item_id
             WHERE job_item.job_id = ?
             ORDER BY CASE item.item_type
                 WHEN 'SERIES' THEN 0 WHEN 'SEASON' THEN 1 WHEN 'EPISODE' THEN 2 ELSE 3 END",
    )
    .bind(&first_fill_missing_job_id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        first_fill_missing_item_types,
        vec!["SERIES", "SEASON", "EPISODE", "EPISODE"],
        "incremental episode metadata should include its season and series containers"
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let status: String =
                sqlx::query_scalar("SELECT status FROM metadata_reidentify_jobs WHERE id = ?")
                    .bind(&first_fill_missing_job_id)
                    .fetch_one(database.pool())
                    .await?;
            if !matches!(status.as_str(), "QUEUED" | "RUNNING") {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
             SET status = 'FAILED', error = 'SCRAPER_UNAVAILABLE',
                 automatic_retry_count = 0, automatic_retry_after = NULL
             WHERE job_id = ?",
    )
    .bind(&first_fill_missing_job_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE server_settings SET value = CAST(unixepoch() - 1 AS TEXT)
             WHERE key = 'metadata_fill_missing_legacy_retry_after'",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE metadata_reidentify_jobs
             SET status = 'DEFERRED', error = 'DEFERRED_PROVIDER_UNAVAILABLE',
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ?",
    )
    .bind(&first_fill_missing_job_id)
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE libraries SET realtime_metadata_auto_match_enabled = 1 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    tokio::fs::write(
        season_dir.join("Incremental.Policy.Show.S01E01.mkv"),
        b"changed episode",
    )
    .await?;
    let retry_scan_job = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![super::IncrementalScanChange {
                root_id: root.root.id.to_string(),
                relative_path:
                    "Incremental Policy Show (2024)/Season 01/Incremental.Policy.Show.S01E01.mkv"
                        .to_owned(),
                kind: crate::application::watch::ChangeKind::Modify,
            }],
        )
        .await?;
    jobs.run_to_completion_with_metadata(&retry_scan_job.id, 100, None, Some(metadata.clone()))
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM metadata_reidentify_jobs
                 WHERE mode = 'FILL_MISSING' AND library_id = ?",
        )
        .bind(&library_id)
        .fetch_one(database.pool())
        .await?,
        2,
        "a later automatic scan must retry a due provider failure when completeness is still READY"
    );
    sqlx::query("UPDATE libraries SET realtime_metadata_auto_match_enabled = 0 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;

    let opted_out_series_dir = media_root.join("Opted Out Policy Show (2024)");
    let opted_out_season_dir = opted_out_series_dir.join("Season 01");
    tokio::fs::create_dir_all(&opted_out_season_dir).await?;
    let opted_out_episode_path = opted_out_season_dir.join("Opted.Out.Policy.Show.S01E01.mkv");
    tokio::fs::write(&opted_out_episode_path, b"episode").await?;
    let opted_out_job = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![super::IncrementalScanChange {
                root_id: root.root.id.to_string(),
                relative_path:
                    "Opted Out Policy Show (2024)/Season 01/Opted.Out.Policy.Show.S01E01.mkv"
                        .to_owned(),
                kind: crate::application::watch::ChangeKind::Create,
            }],
        )
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT auto_metadata_match FROM scan_jobs WHERE id = ?",)
            .bind(&opted_out_job.id)
            .fetch_one(database.pool())
            .await?,
        0
    );
    jobs.run_to_completion_with_metadata(&opted_out_job.id, 100, None, Some(metadata))
        .await?;
    let opted_out_item_id = sqlx::query_scalar::<_, String>(
        "SELECT item.id FROM media_items item
             JOIN media_sources source ON source.item_id = item.id
             JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
             WHERE item.library_id = ? AND item.item_type = 'EPISODE'
               AND entry.relative_path LIKE 'Opted Out Policy Show (2024)/%'",
    )
    .bind(&library_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM item_metadata_completeness
                 WHERE item_id = ? AND capability = 'METADATA'
                   AND local_state = 'READY' AND is_missing = 1",
        )
        .bind(&opted_out_item_id)
        .fetch_one(database.pool())
        .await?,
        1,
        "an opted-out incremental job still persists episode metadata missing state"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(DISTINCT item.id) FROM media_items item
                 JOIN item_metadata_completeness completeness ON completeness.item_id = item.id
                 WHERE item.library_id = ? AND item.item_type IN ('SERIES', 'SEASON', 'EPISODE')
                   AND item.id IN (
                       SELECT id FROM media_items WHERE id = ?
                       UNION SELECT parent_id FROM media_items WHERE id = ?
                       UNION SELECT series_id FROM media_items WHERE id = ?
                   )
                   AND completeness.capability = 'METADATA'
                   AND completeness.local_state = 'READY' AND completeness.is_missing = 1",
        )
        .bind(&library_id)
        .bind(&opted_out_item_id)
        .bind(&opted_out_item_id)
        .bind(&opted_out_item_id)
        .fetch_one(database.pool())
        .await?,
        3,
        "an opted-out incremental scan still records local missing metadata for the hierarchy"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM metadata_reidentify_jobs
                 WHERE mode = 'FILL_MISSING' AND library_id = ?",
        )
        .bind(&library_id)
        .fetch_one(database.pool())
        .await?,
        2,
        "a saved incremental opt-out does not add another job after a due retry"
    );
    Ok(())
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
#[test]
fn manifest_directory_reader_enumerates_from_the_open_directory_handle()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp_dir.path())?;
    std::fs::create_dir(root.join("Nested"))?;
    std::fs::write(root.join("Example.Movie.2025.mkv"), b"fixture")?;

    let reader = ManifestDirectoryReader::open(&root, "")?;
    let (_reader, batch) = reader.next_batch(500)?;

    assert!(batch.completed);
    assert_eq!(batch.child_directories, vec!["Nested"]);
    assert!(
        batch
            .entries
            .iter()
            .any(|entry| entry.relative_path == "Example.Movie.2025.mkv")
    );
    assert!(
        batch
            .entries
            .iter()
            .any(|entry| entry.relative_path.is_empty())
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn manifest_directory_batch_stat_detects_file_changes_without_directory_mtime_change()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp_dir.path())?;
    let bucket = root.join("Bucket");
    std::fs::create_dir(&bucket)?;
    let file_path = bucket.join("Example.Movie.2025.mkv");
    std::fs::write(&file_path, b"original")?;

    let reader = ManifestDirectoryReader::open(&root, "Bucket")?;
    let root_observation = reader.root_observation.clone();
    let directory_observation = reader.directory_observation.clone();
    let (_reader, batch) = reader.next_batch(500)?;
    let file_observation = batch
        .entries
        .into_iter()
        .find(|entry| entry.entry_kind == "FILE")
        .ok_or("file observation is missing")?;
    let directory_modified_before = std::fs::metadata(&bucket)?.modified()?;

    std::fs::write(&file_path, b"replacement content with another size")?;

    let directory_modified_after = std::fs::metadata(&bucket)?.modified()?;
    assert_eq!(directory_modified_before, directory_modified_after);
    let current_files = stat_manifest_directory_file_batch_sync(
        &root,
        &root_observation,
        &directory_observation,
        std::slice::from_ref(&file_observation),
    )?;
    let current_file = current_files
        .into_iter()
        .next()
        .flatten()
        .ok_or("changed file disappeared during the second stat")?;
    assert!(!manifest_file_observation_matches(
        &file_observation,
        &current_file
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn manifest_directory_batch_stat_rejects_replaced_parent_directory()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp_dir.path())?;
    let bucket = root.join("Bucket");
    std::fs::create_dir(&bucket)?;
    std::fs::write(bucket.join("Example.Movie.2025.mkv"), b"fixture")?;

    let reader = ManifestDirectoryReader::open(&root, "Bucket")?;
    let root_observation = reader.root_observation.clone();
    let directory_observation = reader.directory_observation.clone();
    let (_reader, batch) = reader.next_batch(500)?;
    let file_observation = batch
        .entries
        .into_iter()
        .find(|entry| entry.entry_kind == "FILE")
        .ok_or("file observation is missing")?;

    std::fs::rename(&bucket, root.join("Bucket.old"))?;
    std::fs::create_dir(&bucket)?;
    std::fs::write(bucket.join("Example.Movie.2025.mkv"), b"replacement").map(|_| ())?;

    let result = stat_manifest_directory_file_batch_sync(
        &root,
        &root_observation,
        &directory_observation,
        &[file_observation],
    );
    assert!(matches!(result, Err(ScannerError::RootIdentityChanged(_))));
    Ok(())
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
#[tokio::test]
async fn manifest_strm_read_rejects_same_inode_replaced_by_symlink()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let library_path = temp_dir.path().join("Library");
    std::fs::create_dir(&library_path)?;
    let root = std::fs::canonicalize(library_path)?;
    let strm_path = root.join("Example.Movie.2025.strm");
    std::fs::write(&strm_path, "https://example.invalid/video")?;
    let external_path = temp_dir.path().join("Moved.strm");
    let root_observation = stat_manifest_root_sync(&root)?;
    let observation = stat_manifest_relative_file_sync(
        &root,
        "Example.Movie.2025.strm",
        root_observation.device,
        root_observation.inode,
    )?
    .expect("STRM file observation");

    std::fs::rename(&strm_path, &external_path)?;
    std::os::unix::fs::symlink(&external_path, &strm_path)?;

    let result = read_manifest_strm_target(
        root,
        "Example.Movie.2025.strm".to_owned(),
        root_observation.device,
        root_observation.inode,
        observation,
    )
    .await;
    assert!(result.is_err(), "manifest read must not follow the symlink");
    Ok(())
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
#[tokio::test]
async fn manifest_strm_read_rejects_oversized_target_contents()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let library_path = temp_dir.path().join("Library");
    std::fs::create_dir(&library_path)?;
    let root = std::fs::canonicalize(library_path)?;
    let strm_path = root.join("Example.Movie.2025.strm");
    std::fs::write(
        &strm_path,
        format!("https://example.invalid/{}", "x".repeat(1024 * 1024)),
    )?;
    let root_observation = stat_manifest_root_sync(&root)?;
    let observation = stat_manifest_relative_file_sync(
        &root,
        "Example.Movie.2025.strm",
        root_observation.device,
        root_observation.inode,
    )?
    .expect("STRM file observation");

    let result = read_manifest_strm_target(
        root,
        "Example.Movie.2025.strm".to_owned(),
        root_observation.device,
        root_observation.inode,
        observation,
    )
    .await;

    assert!(matches!(
        result,
        Err(ScannerError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::InvalidData
    ));
    Ok(())
}

#[tokio::test]
async fn non_manifest_strm_read_rejects_oversized_target_contents()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let strm_path = temp_dir.path().join("oversized.strm");
    std::fs::write(
        &strm_path,
        format!(
            "https://example.invalid/{}",
            "x".repeat(MAX_STRM_TARGET_BYTES)
        ),
    )?;

    let result = read_strm_target(&strm_path).await;

    assert!(matches!(
        result,
        Err(ScannerError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::InvalidData
    ));
    Ok(())
}

#[test]
fn manifest_removal_confirmation_rejects_reappeared_paths() {
    let decision = classify_manifest_removal_outcomes(&[
        ("reappeared".to_owned(), ManifestRemovalOutcome::Present),
        ("still-missing".to_owned(), ManifestRemovalOutcome::Missing),
    ]);

    assert_eq!(decision.confirmed_missing_ids, vec!["still-missing"]);
    assert_eq!(decision.unstable_ids, vec!["reappeared"]);
    assert!(!decision.root_identity_lost);
}

#[test]
fn manifest_removal_io_error_invalidates_all_sibling_deletes() {
    let decision = classify_manifest_removal_outcomes(&[
        ("missing".to_owned(), ManifestRemovalOutcome::Missing),
        ("unreadable".to_owned(), ManifestRemovalOutcome::PathIoError),
    ]);

    assert!(decision.confirmed_missing_ids.is_empty());
    assert_eq!(decision.unstable_ids, vec!["missing", "unreadable"]);
    assert!(!decision.root_identity_lost);
}

#[test]
fn manifest_root_without_stable_identity_is_never_authoritative() {
    let observed = NewScanManifestEntry {
        relative_path: String::new(),
        entry_kind: "DIRECTORY".to_owned(),
        size: 0,
        modified_at: 0,
        device: None,
        inode: None,
        fingerprint: Vec::new(),
    };

    assert!(!manifest_root_identity_matches(None, None, &observed));
}

#[test]
fn media_source_folder_uses_the_source_parent_directory() {
    assert_eq!(
        media_source_folder("Movies/Dune/Dune.2021.mkv").unwrap(),
        "Movies/Dune"
    );
    assert_eq!(media_source_folder("Dune.2021.mkv").unwrap(), ".");
}

#[test]
fn normalize_incremental_path_canonicalizes_external_separators() {
    assert_eq!(
        normalize_incremental_path(r"Movies\Dune\Dune.2021.mkv").unwrap(),
        "Movies/Dune/Dune.2021.mkv"
    );
}

#[test]
fn scan_activity_label_uses_only_the_relative_basename() {
    assert_eq!(
        safe_scan_activity_label("Movies/Dune/Dune.2021.mkv").as_deref(),
        Some("Dune.2021.mkv")
    );
    assert_eq!(
        safe_scan_activity_label("/private/root/Secret.strm?token=abc").as_deref(),
        Some("Secret.strm")
    );
    assert_eq!(safe_scan_activity_label("/"), None);
}

#[tokio::test]
async fn mixed_classification_cache_reuses_the_series_nfo_probe() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let root = temp_dir.path();
    let series_dir = root.join("Example Show");
    tokio::fs::create_dir_all(&series_dir)
        .await
        .expect("series directory");
    tokio::fs::write(series_dir.join("tvshow.nfo"), "<tvshow />")
        .await
        .expect("series NFO");
    let first = series_dir.join("first.mkv");
    let second = series_dir.join("second.mkv");
    let mut cache = MixedClassificationCache::default();

    assert!(matches!(
        classify_mixed_file(root, &first, &mut cache).await,
        MixedClassification::Unresolved
    ));
    assert!(matches!(
        classify_mixed_file(root, &second, &mut cache).await,
        MixedClassification::Unresolved
    ));
    assert_eq!(cache.nfo_roots.len(), 1);
}
