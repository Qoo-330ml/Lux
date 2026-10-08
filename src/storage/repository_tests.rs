use super::*;
use crate::{
    application::{
        access::MediaAccessService,
        candidates::MetadataCandidateService,
        catalog::CatalogService,
        libraries::LibraryService,
        metadata::MetadataEnricher,
        scanner::{LibraryScanner, ScanJobService},
        setup::SetupService,
    },
    config::{Config, DatabaseBackend, DatabaseConfiguration, PostgresConnection},
    library::LibraryKind,
    storage::{
        ItemImageBatchInsert, ItemImageInsert, MetadataAutoMatchPolicy, MetadataCapabilityResult,
        MetadataImageUnavailable, NewItemMetadataCompletenessCheck,
        NewItemMetadataCompletenessResult, NewMediaChapterMarker, NewMetadataCandidate,
        NewNotificationDestination, NewNotificationEvent,
    },
};

async fn refresh_recommendation_stats(database: &Database) {
    sqlx::query(
        "UPDATE recommendation_stats_state
         SET batch_key = batch_key - 1
         WHERE id = 1",
    )
    .execute(database.pool())
    .await
    .expect("invalidate recommendation stats batch");
    assert!(
        database
            .refresh_recommendation_stats_if_needed()
            .await
            .expect("refresh recommendation stats")
    );
}

#[tokio::test]
async fn scan_job_metadata_target_selection_uses_one_query_without_a_local_source()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await?;
    database
        .query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('metadata-target-without-source', ?, 'MOVIE', 'Movie', 'movie', 'PENDING')",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_job_targets (
                 job_id, target_type, target_id, item_id, change_kind, metadata_state
             ) VALUES (?, 'ITEM', 'metadata-target-without-source',
                       'metadata-target-without-source', 'NEW', 'PENDING')",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;

    database.reset_query_count();
    let report = MetadataEnricher::new(database.clone())
        .enrich_scan_job_targets(&job.id, 32)
        .await?;

    assert_eq!(report.items_processed, 0);
    assert_eq!(
        database.query_count(),
        1,
        "pending state and the unavailable-source result should share one bounded page query"
    );
    Ok(())
}

#[tokio::test]
async fn local_nfo_metadata_batch_rolls_back_all_updates_on_a_batch_error()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    for item_id in ["nfo-batch-first", "nfo-batch-second"] {
        database
            .query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', 'Old title', 'old title', 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .execute(database.pool())
            .await?;
    }
    sqlx::query(
        "CREATE TRIGGER reject_second_local_nfo_update
         BEFORE UPDATE OF title ON media_items
         WHEN OLD.id = 'nfo-batch-second'
         BEGIN SELECT RAISE(ABORT, 'injected metadata failure'); END",
    )
    .execute(database.pool())
    .await?;

    let first_fingerprint = [1_u8; 32];
    let second_fingerprint = [2_u8; 32];
    let updates = [
        MediaMetadataUpdate {
            item_id: "nfo-batch-first",
            title: "First title",
            original_title: None,
            overview: None,
            production_year: None,
            premiere_date: None,
            rating: None,
            rating_source: None,
            provider_ids_json: None,
            metadata_fingerprint: &first_fingerprint,
            provenance_json: "{}",
            locked_fields_json: "{}",
        },
        MediaMetadataUpdate {
            item_id: "nfo-batch-second",
            title: "Second title",
            original_title: None,
            overview: None,
            production_year: None,
            premiere_date: None,
            rating: None,
            rating_source: None,
            provider_ids_json: None,
            metadata_fingerprint: &second_fingerprint,
            provenance_json: "{}",
            locked_fields_json: "{}",
        },
    ];

    assert!(
        database
            .update_media_item_metadata_batch(&updates)
            .await
            .is_err()
    );
    let titles: Vec<String> = sqlx::query_scalar(
        "SELECT title FROM media_items
         WHERE id IN ('nfo-batch-first', 'nfo-batch-second') ORDER BY id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(titles, ["Old title", "Old title"]);
    Ok(())
}

#[tokio::test]
async fn scan_job_metadata_page_preserves_kind_priority_and_item_order()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Mixed", LibraryKind::Mixed, false)
        .await?;
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path).await?;
    let root_path = root_path.to_str().ok_or("non-UTF8 test root")?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('metadata-page-root', ?, ?, ?, 1, 0)",
        )
        .bind(library.id.to_string())
        .bind(root_path)
        .bind(root_path)
        .execute(database.pool())
        .await?;
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await?;

    for (item_id, item_type, parent_id, series_id, season_number) in [
        ("movie-z", "MOVIE", None, None, None),
        ("movie-a", "MOVIE", None, None, None),
        ("video-a", "VIDEO", None, None, None),
        ("series-a", "SERIES", None, None, None),
        (
            "season-a",
            "SEASON",
            Some("series-a"),
            Some("series-a"),
            Some(1_i64),
        ),
        (
            "episode-a",
            "EPISODE",
            Some("season-a"),
            Some("series-a"),
            Some(1_i64),
        ),
    ] {
        database
            .query(
                "INSERT INTO media_items (
                     id, library_id, item_type, parent_id, series_id, season_number,
                     title, sort_title, identification_status
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .bind(item_type)
            .bind(parent_id)
            .bind(series_id)
            .bind(season_number)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
    }
    for item_id in ["movie-z", "movie-a", "video-a", "episode-a"] {
        let entry_id = format!("entry-{item_id}");
        let source_id = format!("source-{item_id}");
        let relative_path = format!("{item_id}.mkv");
        database
            .query(
                "INSERT INTO filesystem_entries (
                     id, library_root_id, relative_path, entry_kind, size, modified_at,
                     last_seen_generation
                 ) VALUES (?, 'metadata-page-root', ?, 'FILE', 1, 1, 'generation')",
            )
            .bind(&entry_id)
            .bind(&relative_path)
            .execute(database.pool())
            .await?;
        database
            .query(
                "INSERT INTO media_sources (
                     id, item_id, source_kind, filesystem_entry_id, is_default
                 ) VALUES (?, ?, 'LOCAL_FILE', ?, 1)",
            )
            .bind(&source_id)
            .bind(item_id)
            .bind(&entry_id)
            .execute(database.pool())
            .await?;
        database
            .query(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, item_id, change_kind, metadata_state
                 ) VALUES (?, 'ITEM', ?, ?, 'NEW', 'PENDING')",
            )
            .bind(&job.id)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
    }
    database.reset_query_count();
    let page = database.load_scan_job_metadata_page(&job.id, 1).await?;
    assert!(page.has_pending);
    let StoredScanJobMetadataSources::Movies(movies) = page.sources else {
        return Err("movie targets should have priority over other media types".into());
    };
    assert_eq!(movies.len(), 1);
    assert_eq!(movies[0].item_id, "movie-a");
    assert_eq!(database.query_count(), 1);

    database
        .query(
            "UPDATE scan_job_targets SET metadata_state = 'DONE'
             WHERE job_id = ? AND target_id IN ('movie-a', 'movie-z')",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    database
        .query("DELETE FROM media_sources WHERE item_id = 'movie-z'")
        .execute(database.pool())
        .await?;
    database
        .query(
            "UPDATE scan_job_targets SET metadata_state = 'PENDING'
             WHERE job_id = ? AND target_id = 'movie-z'",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    database.reset_query_count();
    let page = database.load_scan_job_metadata_page(&job.id, 8).await?;
    let StoredScanJobMetadataSources::HomeVideos(videos) = page.sources else {
        return Err("home video targets should follow movies".into());
    };
    assert_eq!(
        videos
            .iter()
            .map(|source| source.item_id.as_str())
            .collect::<Vec<_>>(),
        ["video-a"]
    );
    assert_eq!(database.query_count(), 1);

    database
        .query(
            "UPDATE scan_job_targets SET metadata_state = 'DONE'
             WHERE job_id = ? AND target_id = 'video-a'",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    database.reset_query_count();
    let page = database.load_scan_job_metadata_page(&job.id, 8).await?;
    let StoredScanJobMetadataSources::Episodes(episodes) = page.sources else {
        return Err("episode targets should follow home videos".into());
    };
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0].episode_id, "episode-a");
    assert_eq!(episodes[0].series_id, "series-a");
    assert_eq!(database.query_count(), 1);

    database
        .query(
            "UPDATE scan_job_targets SET metadata_state = 'DONE'
             WHERE job_id = ? AND target_id = 'episode-a'",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    database.reset_query_count();
    let page = database.load_scan_job_metadata_page(&job.id, 8).await?;
    assert!(
        page.has_pending,
        "an unavailable movie source remains pending"
    );
    assert!(matches!(page.sources, StoredScanJobMetadataSources::None));
    assert_eq!(database.query_count(), 1);

    database
        .query(
            "UPDATE scan_job_targets SET metadata_state = 'FAILED'
             WHERE job_id = ? AND target_id = 'movie-z'",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    database.reset_query_count();
    let page = database.load_scan_job_metadata_page(&job.id, 8).await?;
    assert!(!page.has_pending);
    assert!(matches!(page.sources, StoredScanJobMetadataSources::None));
    assert_eq!(database.query_count(), 1);
    Ok(())
}

#[tokio::test]
async fn scan_manifest_postprocessing_state_loads_roots_in_one_query()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path).await?;
    let root_path = root_path.to_str().ok_or("non-UTF8 test root")?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('manifest-state-root', ?, ?, ?, 1, 0)",
        )
        .bind(library.id.to_string())
        .bind(root_path)
        .bind(root_path)
        .execute(database.pool())
        .await?;
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await?;
    database
        .query(
            "UPDATE scan_manifests SET state = 'POSTPROCESSING', workflow_version = 3,
                 discovery_format_version = 3, discovery_mode = 'LITE'
             WHERE job_id = ?",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    let manifest_id: String = sqlx::query_scalar("SELECT id FROM scan_manifests WHERE job_id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_manifest_roots (
                 manifest_id, library_root_id, state, postprocessing_target_stage
             ) VALUES (?, 'manifest-state-root', 'COMPLETE', 'NEW')
             ON CONFLICT(manifest_id, library_root_id) DO UPDATE SET
                 state = 'COMPLETE', postprocessing_target_stage = 'NEW'",
        )
        .bind(&manifest_id)
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_manifest_entries (
                 manifest_id, library_root_id, relative_path, observation_sequence,
                 entry_kind, size, modified_at, device, inode
             ) VALUES (?, 'manifest-state-root', '', 1,
                       'DIRECTORY', 0, 1, 11, 22)
             ON CONFLICT(manifest_id, library_root_id, relative_path, observation_sequence)
             DO UPDATE SET device = 11, inode = 22",
        )
        .bind(&manifest_id)
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 last_seen_generation, last_seen_change_kind
             ) VALUES ('manifest-positive-entry', 'manifest-state-root', 'movie.mkv',
                       'FILE', 1, 1, ?, 'NEW')",
        )
        .bind(&job.generation)
        .execute(database.pool())
        .await?;

    database.reset_query_count();
    let state = database
        .get_scan_manifest_postprocessing_state_by_job(&job.id)
        .await?
        .ok_or("scan manifest state should be present")?;
    assert_eq!(state.manifest_id, manifest_id);
    assert_eq!(state.workflow_version, 3);
    assert_eq!(state.discovery_format_version, 3);
    assert_eq!(state.roots.len(), 1);
    assert_eq!(state.roots[0].expected_device, Some(11));
    assert_eq!(state.roots[0].expected_inode, Some(22));
    assert!(state.roots[0].has_positive_rows);
    assert!(state.roots[0].has_stage_rows);
    assert_eq!(database.query_count(), 1);

    database.reset_query_count();
    assert!(
        database
            .get_scan_manifest_postprocessing_state_by_job("missing-manifest-job")
            .await?
            .is_none()
    );
    assert_eq!(database.query_count(), 1);
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_metadata_batch_writes_preserve_upsert_and_retry_semantics()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration =
        crate::config::DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(
        &config,
        &crate::config::DatabaseConfiguration::Postgres(PostgresConnection {
            database: database_name.clone(),
            ..admin_connection
        }),
    )
    .await?;

    let assertions = async {
        let library = LibraryService::new(database.clone())
            .create_library("Metadata", LibraryKind::Movie, false)
            .await?;
        database
            .query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES ('postgres-metadata-batch-item', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
            )
            .bind(library.id.to_string())
            .execute(database.pool())
            .await?;

        let candidates = [
            NewMetadataCandidate {
                id: "candidate-first",
                item_id: "postgres-metadata-batch-item",
                provider: "tmdb",
                provider_id: "movie-1",
                candidate_json: r#"{"title":"lower"}"#,
                score: 70.0,
                expires_at: Some(1_000),
            },
            NewMetadataCandidate {
                id: "candidate-second",
                item_id: "postgres-metadata-batch-item",
                provider: "tmdb",
                provider_id: "movie-1",
                candidate_json: r#"{"title":"higher"}"#,
                score: 90.0,
                expires_at: Some(2_000),
            },
            NewMetadataCandidate {
                id: "candidate-other",
                item_id: "postgres-metadata-batch-item",
                provider: "tmdb",
                provider_id: "movie-2",
                candidate_json: r#"{"title":"other"}"#,
                score: 60.0,
                expires_at: Some(3_000),
            },
        ];
        database.reset_query_count();
        database.insert_metadata_candidates(&candidates).await?;
        assert_eq!(database.query_count(), 1);
        let rows: Vec<(String, String, f64)> = sqlx::query_as(
            "SELECT id, candidate_json, score FROM metadata_candidates
             WHERE item_id = 'postgres-metadata-batch-item' AND provider_id = 'movie-1'",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(rows, vec![(
            "candidate-first".to_owned(),
            r#"{"title":"higher"}"#.to_owned(),
            90.0,
        )]);

        let capabilities = [MetadataCapabilityResult {
            capability: "EXTERNAL_IDS",
            has_data: false,
        }];
        database.reset_query_count();
        database
            .record_metadata_capability_results(
                "postgres-metadata-batch-item",
                "tmdb",
                "movie-1",
                &capabilities,
                1_000,
            )
            .await?;
        assert_eq!(database.query_count(), 1);
        database.reset_query_count();
        database
            .record_metadata_capability_failures(
                "postgres-metadata-batch-item",
                "tmdb",
                "movie-1",
                &["EXTERNAL_IDS", "TRAILERS"],
                2_000,
            )
            .await?;
        assert_eq!(database.query_count(), 2);

        let unavailable = [
            MetadataImageUnavailable {
                image_type: "POSTER",
                candidate_key: "tmdb:movie-1:POSTER",
            },
            MetadataImageUnavailable {
                image_type: "FANART",
                candidate_key: "tmdb:movie-1:FANART",
            },
        ];
        database.reset_query_count();
        database
            .mark_metadata_images_unavailable(
                "postgres-metadata-batch-item",
                &unavailable,
                3_000,
            )
            .await?;
        assert_eq!(database.query_count(), 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    assertions?;
    drop_database?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_scan_write_transaction_uses_local_async_commit()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration =
        crate::config::DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER DATABASE {database_name} SET synchronous_commit = on"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let connection = PostgresConnection {
        database: database_name.clone(),
        ..admin_connection
    };
    let database = Database::connect_with_configuration(
        &config,
        &crate::config::DatabaseConfiguration::Postgres(connection),
    )
    .await?;
    let mut metadata_transaction = database.begin_metadata_write_transaction().await?;
    let metadata_setting: String = sqlx::query_scalar("SHOW synchronous_commit")
        .fetch_one(&mut *metadata_transaction)
        .await?;
    metadata_transaction.commit().await?;

    let mut transaction = database.begin_scan_write_transaction().await?;
    let transaction_setting: String = sqlx::query_scalar("SHOW synchronous_commit")
        .fetch_one(&mut *transaction)
        .await?;
    transaction.commit().await?;
    let session_setting: String = sqlx::query_scalar("SHOW synchronous_commit")
        .fetch_one(database.pool())
        .await?;

    database.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    admin_pool.close().await;

    assert_eq!(transaction_setting, "off");
    assert_eq!(metadata_setting, "on");
    assert_eq!(session_setting, "on");
    Ok(())
}

#[tokio::test]
async fn recommendation_stats_are_refreshed_once_per_batch_and_deduplicate_users() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let admin = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Recommendations", LibraryKind::Movie, false)
        .await
        .expect("library");
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await
        .expect("current timestamp");
    let admin_id = admin.id.to_string();
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO users (
                id, username_normalized, display_name, password_hash
             ) VALUES ('recommendation-user-2', 'recommendation-user-2',
                       'Recommendation User 2', 'test')",
    )
    .execute(database.pool())
    .await
    .expect("second user");
    for item_id in ["playback-item", "favorite-item", "expired-item"] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, last_played_at)
         VALUES (?, 'playback-item', ?),
                ('recommendation-user-2', 'playback-item', ?),
                (?, 'expired-item', ?),
                (?, 'favorite-item', 0),
                ('recommendation-user-2', 'favorite-item', 0)",
    )
    .bind(&admin_id)
    .bind(now)
    .bind(now)
    .bind(&admin_id)
    .bind(now - 180 * 86_400)
    .bind(&admin_id)
    .execute(database.pool())
    .await
    .expect("user item states");
    sqlx::query(
        "UPDATE user_item_state
         SET is_favorite = 1
         WHERE item_id = 'favorite-item'",
    )
    .execute(database.pool())
    .await
    .expect("favorite states");
    sqlx::query(
        "INSERT INTO playback_sessions (
                id, user_id, item_id, play_session_id, device_id, state, last_event_at
             ) VALUES ('recommendation-session', ?, 'playback-item',
                       'recommendation-play-session', 'test', 'PLAYING', ?)",
    )
    .bind(&admin_id)
    .bind(now)
    .execute(database.pool())
    .await
    .expect("playback session");

    refresh_recommendation_stats(&database).await;
    let scores = sqlx::query(
        "SELECT item_id, recent_playback_score, favorite_score
         FROM recommendation_item_stats
         WHERE item_id IN ('playback-item', 'favorite-item', 'expired-item')
         ORDER BY item_id",
    )
    .fetch_all(database.pool())
    .await
    .expect("recommendation scores");
    assert_eq!(scores.len(), 3);
    assert_eq!(scores[0].get::<String, _>("item_id"), "expired-item");
    assert_eq!(scores[0].get::<i64, _>("recent_playback_score"), 0);
    assert_eq!(scores[1].get::<String, _>("item_id"), "favorite-item");
    assert_eq!(scores[1].get::<i64, _>("favorite_score"), 10);
    assert_eq!(scores[2].get::<String, _>("item_id"), "playback-item");
    assert_eq!(scores[2].get::<i64, _>("recent_playback_score"), 2);
    assert!(
        !database
            .refresh_recommendation_stats_if_needed()
            .await
            .expect("same-batch stats refresh")
    );

    sqlx::query(
        "UPDATE media_items
         SET removed_at = ?
         WHERE id = 'expired-item'",
    )
    .bind(now)
    .execute(database.pool())
    .await
    .expect("remove recommendation item");
    refresh_recommendation_stats(&database).await;
    let remaining_stats: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM recommendation_item_stats
         WHERE item_id IN ('playback-item', 'favorite-item', 'expired-item')",
    )
    .fetch_one(database.pool())
    .await
    .expect("remaining recommendation stats");
    assert_eq!(remaining_stats, 2);
}

#[tokio::test]
async fn played_container_state_sync_batches_parent_queries_and_preserves_state() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let admin = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Shows", LibraryKind::Series, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    let user_id = admin.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, parent_id, series_id, title, sort_title,
             identification_status, has_available_source
         ) VALUES
            ('played-parent-series', ?, 'SERIES', NULL, NULL, 'Series', 'series', 'LOCAL_CONFIRMED', 1),
            ('played-parent-season', ?, 'SEASON', 'played-parent-series', 'played-parent-series', 'Season', 'season', 'LOCAL_CONFIRMED', 1),
            ('played-episode-1', ?, 'EPISODE', 'played-parent-season', 'played-parent-series', 'Episode 1', 'episode 1', 'LOCAL_CONFIRMED', 1),
            ('played-episode-2', ?, 'EPISODE', 'played-parent-season', 'played-parent-series', 'Episode 2', 'episode 2', 'LOCAL_CONFIRMED', 1)",
    )
    .bind(&library_id)
    .bind(&library_id)
    .bind(&library_id)
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("media hierarchy");
    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, is_played, play_count)
         VALUES (?, 'played-episode-1', 1, 1), (?, 'played-episode-2', 1, 1)",
    )
    .bind(&user_id)
    .bind(&user_id)
    .execute(database.pool())
    .await
    .expect("episode states");

    database.reset_query_count();
    database
        .sync_played_container_states(&user_id, "played-episode-1")
        .await
        .expect("played parent states");
    assert_eq!(database.query_count(), 3);

    let played_states: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT item_id, is_played, play_count FROM user_item_state
         WHERE user_id = ? AND item_id IN ('played-parent-season', 'played-parent-series')
         ORDER BY item_id",
    )
    .bind(&user_id)
    .fetch_all(database.pool())
    .await
    .expect("played parent state rows");
    assert_eq!(
        played_states,
        vec![
            ("played-parent-season".to_owned(), 1, 1),
            ("played-parent-series".to_owned(), 1, 1),
        ]
    );

    sqlx::query(
        "UPDATE user_item_state SET is_played = 0 WHERE user_id = ? AND item_id = 'played-episode-2'",
    )
    .bind(&user_id)
    .execute(database.pool())
    .await
    .expect("unmark episode");
    database.reset_query_count();
    database
        .sync_played_container_states(&user_id, "played-episode-2")
        .await
        .expect("unplayed parent states");
    assert_eq!(database.query_count(), 3);

    let unplayed_states: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT item_id, is_played, version FROM user_item_state
         WHERE user_id = ? AND item_id IN ('played-parent-season', 'played-parent-series')
         ORDER BY item_id",
    )
    .bind(&user_id)
    .fetch_all(database.pool())
    .await
    .expect("unplayed parent state rows");
    assert_eq!(
        unplayed_states,
        vec![
            ("played-parent-season".to_owned(), 0, 1),
            ("played-parent-series".to_owned(), 0, 1),
        ]
    );
}

#[tokio::test]
async fn home_resume_page_returns_items_and_total_with_one_query() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let admin = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Home resume", LibraryKind::Movie, false)
        .await
        .expect("library");
    let user_id = admin.id.to_string();
    let library_id = library.id.to_string();
    for item_id in ["resume-one", "resume-two"] {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, runtime_ticks,
                 identification_status, has_available_source
             ) VALUES (?, ?, 'MOVIE', ?, ?, 2000000000, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
        sqlx::query(
            "INSERT INTO user_item_state (
                 user_id, item_id, is_played, position_ticks, last_played_at
             ) VALUES (?, ?, 0, 1000000000, 1)",
        )
        .bind(&user_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("resume state");
    }

    database.reset_query_count();
    let (rows, total) = database
        .list_resume_items_with_total(
            &ResumeItemsQuery {
                user_id: &user_id,
                library_ids: std::slice::from_ref(&library_id),
                item_types: &["MOVIE"],
                played_percent: 90,
                minimum_ticks: 0,
                offset: 0,
                limit: 1,
            },
            true,
        )
        .await
        .expect("home resume page");

    assert_eq!(database.query_count(), 1);
    assert_eq!(total, 2);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].item_id, "resume-one");

    database.reset_query_count();
    let (past_end, total) = database
        .list_resume_items_with_total(
            &ResumeItemsQuery {
                user_id: &user_id,
                library_ids: std::slice::from_ref(&library_id),
                item_types: &["MOVIE"],
                played_percent: 90,
                minimum_ticks: 0,
                offset: 10,
                limit: 1,
            },
            true,
        )
        .await
        .expect("out-of-range home resume page");

    assert!(past_end.is_empty());
    assert_eq!(total, 2);
    assert_eq!(database.query_count(), 2);
}

#[tokio::test]
async fn home_resume_settings_are_read_with_one_query() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let admin = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let user_id = admin.id.to_string();
    database
        .set_user_played_percent(&user_id, 85)
        .await
        .expect("played percent");
    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES ('resume_min_ticks', '300000000')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .execute(database.pool())
    .await
    .expect("minimum resume ticks");

    database.reset_query_count();
    let settings = database
        .home_resume_settings(&user_id)
        .await
        .expect("home resume settings");

    assert_eq!(database.query_count(), 1);
    assert_eq!(settings, (85, 300_000_000));
}

#[tokio::test]
async fn thumbnail_scraper_retries_are_persisted_and_claimed_at_due_times() {
    const SIX_HOURS: i64 = 6 * 60 * 60;
    const ONE_DAY: i64 = 24 * 60 * 60;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Retry Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir)
        .await
        .expect("movie directory");
    tokio::fs::write(movie_dir.join("Retry.Movie.2024.mkv"), b"video")
        .await
        .expect("movie file");
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("index movie");
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE'")
            .fetch_one(database.pool())
            .await
            .expect("indexed item");

    let first_attempt_at = 1_000;
    assert!(
        database
            .ensure_thumbnail_scraper_retry(
                &item_id,
                first_attempt_at,
                first_attempt_at + SIX_HOURS,
            )
            .await
            .expect("seed first retry")
    );
    assert!(
        !database
            .ensure_thumbnail_scraper_retry(
                &item_id,
                first_attempt_at + 1,
                first_attempt_at + SIX_HOURS + 1,
            )
            .await
            .expect("do not reset retry history")
    );
    assert!(
        database
            .claim_due_thumbnail_scraper_retries(
                first_attempt_at + SIX_HOURS - 1,
                first_attempt_at + SIX_HOURS + 300,
                10,
            )
            .await
            .expect("check before due")
            .is_empty()
    );

    let second_attempt = database
        .claim_due_thumbnail_scraper_retries(
            first_attempt_at + SIX_HOURS,
            first_attempt_at + SIX_HOURS + 300,
            10,
        )
        .await
        .expect("claim six-hour retry");
    assert_eq!(second_attempt.len(), 1);
    assert_eq!(second_attempt[0].attempt_count, 1);
    assert!(
        database
            .finish_thumbnail_scraper_retry(
                &item_id,
                2,
                Some(first_attempt_at + ONE_DAY),
                first_attempt_at + SIX_HOURS,
            )
            .await
            .expect("schedule twenty-four-hour retry")
    );

    let third_attempt = database
        .claim_due_thumbnail_scraper_retries(
            first_attempt_at + ONE_DAY,
            first_attempt_at + ONE_DAY + 300,
            10,
        )
        .await
        .expect("claim twenty-four-hour retry");
    assert_eq!(third_attempt.len(), 1);
    assert_eq!(third_attempt[0].attempt_count, 2);
    assert!(
        database
            .finish_thumbnail_scraper_retry(&item_id, 3, None, first_attempt_at + ONE_DAY,)
            .await
            .expect("complete retry sequence")
    );

    let completed = database
        .find_thumbnail_scraper_retry(&item_id)
        .await
        .expect("read completed retry")
        .expect("retry state exists");
    assert_eq!(completed.status, "COMPLETE");
    assert_eq!(completed.attempt_count, 3);
    assert_eq!(completed.first_attempt_at, first_attempt_at);
    assert_eq!(completed.next_retry_at, None);
}

#[tokio::test]
async fn recommendation_daily_batch_is_stable_until_the_next_batch() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Recommendations", LibraryKind::Movie, false)
        .await
        .expect("library");
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await
        .expect("current timestamp");
    let library_id = library.id.to_string();
    let user_id = user.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, added_at, has_available_source
             ) VALUES ('daily-item-1', ?, 'MOVIE', 'Daily item 1', 'daily item 1',
                       'LOCAL_CONFIRMED', ?, 1)",
    )
    .bind(&library_id)
    .bind(now - 15 * 86_400)
    .execute(database.pool())
    .await
    .expect("initial media item");
    refresh_recommendation_stats(&database).await;

    let service = CatalogService::new(database.clone(), MediaAccessService::new(database.clone()));
    let first = service
        .list_recommended_for_library_ids(std::slice::from_ref(&library_id), &user_id, 7)
        .await
        .expect("first daily recommendation");
    assert_eq!(first.len(), 1);
    let first_ids = first.iter().map(|item| item.id.clone()).collect::<Vec<_>>();

    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, added_at, has_available_source, rating
             ) VALUES ('daily-item-2', ?, 'MOVIE', 'Daily item 2', 'daily item 2',
                       'LOCAL_CONFIRMED', ?, 1, 10.0)",
    )
    .bind(&library_id)
    .bind(now)
    .execute(database.pool())
    .await
    .expect("new media item");
    let same_batch = service
        .list_recommended_for_library_ids(std::slice::from_ref(&library_id), &user_id, 7)
        .await
        .expect("same daily recommendation");
    assert_eq!(
        same_batch
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>(),
        first_ids
    );

    sqlx::query(
        "UPDATE recommendation_daily_batches
         SET batch_key = batch_key - 1
         WHERE user_id = ?",
    )
    .bind(&user_id)
    .execute(database.pool())
    .await
    .expect("advance recommendation batch");
    let next_batch = service
        .list_recommended_for_library_ids(&[library_id], &user_id, 7)
        .await
        .expect("next daily recommendation");
    assert_eq!(next_batch.len(), 2);
    assert!(next_batch.iter().any(|item| item.id == "daily-item-2"));
}

#[tokio::test]
async fn listing_recommendations_does_not_refresh_global_stats_synchronously() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Recommendations", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, has_available_source
             ) VALUES ('home-recommendation-item', ?, 'MOVIE',
                       'Home recommendation item', 'home recommendation item',
                       'LOCAL_CONFIRMED', 1)",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    sqlx::query(
        "UPDATE recommendation_stats_state
         SET batch_key = -1, refreshed_at = 0
         WHERE id = 1",
    )
    .execute(database.pool())
    .await
    .expect("invalidate recommendation stats batch");

    let library_id = library.id.to_string();
    let user_id = user.id.to_string();
    let service = CatalogService::new(database.clone(), MediaAccessService::new(database.clone()));
    service
        .list_recommended_for_library_ids(std::slice::from_ref(&library_id), &user_id, 7)
        .await
        .expect("recommendations");

    let batch_key: i64 =
        sqlx::query_scalar("SELECT batch_key FROM recommendation_stats_state WHERE id = 1")
            .fetch_one(database.pool())
            .await
            .expect("recommendation stats state");
    assert_eq!(batch_key, -1);
}

#[tokio::test]
async fn cancelled_recommendation_listing_still_saves_the_daily_batch() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Recommendations", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, has_available_source
             ) VALUES ('cancel-recommendation-item', ?, 'MOVIE',
                       'Cancel recommendation item', 'cancel recommendation item',
                       'LOCAL_CONFIRMED', 1)",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");

    let library_id = library.id.to_string();
    let user_id = user.id.to_string();
    let service = CatalogService::new(database.clone(), MediaAccessService::new(database.clone()));

    // Hold the scoring lock so the detached task is parked after it has been spawned, then drop
    // the caller the way a home-cache invalidation does.
    let compute_lock = service.recommendation_compute_lock();
    let held = compute_lock.lock().await;
    let caller = {
        let service = service.clone();
        let library_id = library_id.clone();
        let user_id = user_id.clone();
        tokio::spawn(async move {
            service
                .list_recommended_for_library_ids(&[library_id], &user_id, 7)
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    caller.abort();
    let _ = caller.await;
    drop(held);

    let mut saved = 0_i64;
    for _ in 0..100 {
        saved = sqlx::query_scalar("SELECT COUNT(*) FROM recommendation_daily_batches")
            .fetch_one(database.pool())
            .await
            .expect("daily batch count");
        if saved > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(saved, 1, "the detached scoring task must persist its batch");
}

#[test]
fn database_pool_max_connections_uses_backend_defaults() {
    assert_eq!(
        resolve_database_pool_max_connections(DatabaseBackend::Sqlite, None)
            .expect("SQLite default pool size"),
        8
    );
    assert_eq!(
        resolve_database_pool_max_connections(DatabaseBackend::Postgres, None)
            .expect("PostgreSQL default pool size"),
        20
    );
}

#[test]
fn database_pool_max_connections_accepts_a_bounded_override() {
    assert_eq!(
        resolve_database_pool_max_connections(DatabaseBackend::Sqlite, Some(""))
            .expect("empty pool override uses the default"),
        8
    );
    assert_eq!(
        resolve_database_pool_max_connections(DatabaseBackend::Sqlite, Some("12"))
            .expect("configured pool size"),
        12
    );
    assert_eq!(
        resolve_database_pool_max_connections(DatabaseBackend::Postgres, Some(" 24 "))
            .expect("trimmed configured pool size"),
        24
    );
}

#[test]
fn database_pool_max_connections_rejects_invalid_overrides() {
    for value in ["0", "101", "not-a-number"] {
        assert!(
            resolve_database_pool_max_connections(DatabaseBackend::Sqlite, Some(value)).is_err()
        );
    }
}

#[tokio::test]
async fn changed_sidecar_target_requeues_completed_local_metadata() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Example Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir)
        .await
        .expect("movie directory");
    tokio::fs::write(movie_dir.join("Example.Movie.2020.mkv"), b"video")
        .await
        .expect("movie file");

    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root = libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root")
        .root;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("initial index");

    let jobs = ScanJobService::new(database.clone());
    let job = jobs
        .create_movie_scan_job(library.id)
        .await
        .expect("scan job");
    let root_id = root.id.to_string();
    let media_path = "Example Movie (2020)/Example.Movie.2020.mkv".to_owned();
    database
        .record_scan_job_targets(&job.id, &root_id, &[media_path], "NEW")
        .await
        .expect("record media target");
    sqlx::query(
        "UPDATE scan_job_targets
         SET metadata_state = 'DONE'
         WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&job.id)
    .execute(database.pool())
    .await
    .expect("complete local metadata target");

    let changed = database
        .record_scan_job_sidecar_targets(
            &job.id,
            &root_id,
            &["Example Movie (2020)/poster.jpg".to_owned()],
        )
        .await
        .expect("record changed sidecar target");
    assert!(changed);
    let state: String = sqlx::query_scalar(
        "SELECT metadata_state
         FROM scan_job_targets
         WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await
    .expect("local metadata target state");
    assert_eq!(state, "PENDING");

    sqlx::query(
        "UPDATE scan_job_targets
         SET updated_at = 1
         WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&job.id)
    .execute(database.pool())
    .await
    .expect("set stable sidecar timestamp");
    let changed = database
        .record_scan_job_sidecar_targets(
            &job.id,
            &root_id,
            &["Example Movie (2020)/poster.jpg".to_owned()],
        )
        .await
        .expect("repeat unchanged sidecar target");
    assert!(!changed);
    let updated_at: i64 = sqlx::query_scalar(
        "SELECT updated_at
         FROM scan_job_targets
         WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await
    .expect("unchanged sidecar target timestamp");
    assert_eq!(updated_at, 1);
}

#[tokio::test]
async fn progressive_scan_metadata_batches_are_bounded_idempotent_and_recoverable() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root)
        .await
        .expect("media root");
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Progressive metadata", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root = libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root");
    let root_id = root.root.id.to_string();
    let sources = vec!["source-a".to_owned()];

    for invalid_sources in [
        Vec::new(),
        vec!["".to_owned()],
        vec!["same".to_owned(), "same".to_owned()],
    ] {
        let invalid = NewScanLocalMetadataBatch {
            id: "invalid-batch",
            job_id: "scan-job",
            library_root_id: &root_id,
            batch_sequence: 0,
            source_ids: &invalid_sources,
        };
        assert!(
            database
                .enqueue_scan_local_metadata_batch(invalid)
                .await
                .is_err()
        );
    }
    let oversized_sources = (0..257)
        .map(|index| format!("source-{index}"))
        .collect::<Vec<_>>();
    let maximum_sources = (0..256)
        .map(|index| format!("max-source-{index}"))
        .collect::<Vec<_>>();
    assert!(
        database
            .enqueue_scan_local_metadata_batch(NewScanLocalMetadataBatch {
                id: "oversized-batch",
                job_id: "scan-job",
                library_root_id: &root_id,
                batch_sequence: 0,
                source_ids: &oversized_sources,
            })
            .await
            .is_err()
    );
    assert!(
        database
            .enqueue_scan_local_metadata_batch(NewScanLocalMetadataBatch {
                id: "max-size-batch",
                job_id: "limit-job",
                library_root_id: &root_id,
                batch_sequence: 0,
                source_ids: &maximum_sources,
            })
            .await
            .expect("256 sources are within the limit")
    );
    assert_eq!(
        database
            .cancel_scan_local_metadata_batches("limit-job")
            .await
            .expect("clean up boundary batch"),
        1
    );

    let first = NewScanLocalMetadataBatch {
        id: "batch-a",
        job_id: "scan-job",
        library_root_id: &root_id,
        batch_sequence: 0,
        source_ids: &sources,
    };
    assert!(
        database
            .enqueue_scan_local_metadata_batch(first)
            .await
            .expect("enqueue")
    );
    assert!(
        !database
            .enqueue_scan_local_metadata_batch(first)
            .await
            .expect("idempotent enqueue")
    );
    let changed_payload = vec!["source-b".to_owned()];
    assert!(
        database
            .enqueue_scan_local_metadata_batch(NewScanLocalMetadataBatch {
                id: "batch-a",
                job_id: "scan-job",
                library_root_id: &root_id,
                batch_sequence: 0,
                source_ids: &changed_payload,
            })
            .await
            .is_err()
    );
    for (id, sequence) in [("batch-b", 1), ("batch-c", 2), ("batch-d", 3)] {
        database
            .enqueue_scan_local_metadata_batch(NewScanLocalMetadataBatch {
                id,
                job_id: "scan-job",
                library_root_id: &root_id,
                batch_sequence: sequence,
                source_ids: &sources,
            })
            .await
            .expect("enqueue batch");
    }

    let page = database
        .list_scan_local_metadata_batches(None, None, 1)
        .await
        .expect("first page");
    assert_eq!(page.len(), 1);
    assert!(
        database
            .list_scan_local_metadata_batches(None, None, 101)
            .await
            .is_err()
    );
    assert!(
        database
            .list_scan_local_metadata_batches(Some(page[0].created_at), None, 1)
            .await
            .is_err(),
        "a cursor must provide both timestamp and id"
    );
    let second_page = database
        .list_scan_local_metadata_batches(Some(page[0].created_at), Some(&page[0].id), 1)
        .await
        .expect("second page");
    assert_eq!(second_page.len(), 1);
    assert_ne!(page[0].id, second_page[0].id);

    let (claimed_a, claimed_b) = tokio::join!(
        database.claim_next_scan_local_metadata_batch(),
        database.claim_next_scan_local_metadata_batch(),
    );
    let claimed_a = claimed_a.expect("claim a").expect("batch a");
    let claimed_b = claimed_b.expect("claim b").expect("batch b");
    assert_ne!(
        claimed_a.id, claimed_b.id,
        "concurrent claims must be distinct"
    );
    assert_eq!(claimed_a.status, "RUNNING");
    assert_eq!(claimed_a.attempts, 1);
    assert_eq!(claimed_a.job_id, "scan-job");
    assert_eq!(claimed_a.library_root_id, root_id);
    assert_eq!(claimed_a.source_count, 1);
    assert_eq!(claimed_a.source_refs_json, r#"["source-a"]"#);
    assert_eq!(claimed_a.batch_sequence, 0);
    assert_eq!(claimed_a.next_attempt_at, None);
    assert_eq!(claimed_a.error, None);
    assert!(claimed_a.updated_at >= claimed_a.created_at);
    assert!(
        database
            .has_pending_scan_local_metadata_images("scan-job")
            .await
            .expect("image stage is pending")
    );
    assert!(
        database
            .mark_scan_local_metadata_images_complete(&claimed_a.id)
            .await
            .expect("mark local images complete")
    );
    let claimed_a_images_completed_at: Option<i64> = sqlx::query_scalar(
        "SELECT images_completed_at FROM scan_local_metadata_batches WHERE id = ?",
    )
    .bind(&claimed_a.id)
    .fetch_one(database.pool())
    .await
    .expect("read image completion marker");
    assert!(claimed_a_images_completed_at.is_some());
    assert!(
        database
            .has_pending_scan_local_metadata_images("scan-job")
            .await
            .expect("another batch image stage remains pending")
    );
    assert!(
        database
            .complete_scan_local_metadata_batch(&claimed_a.id)
            .await
            .expect("complete")
    );
    assert!(
        !database
            .complete_scan_local_metadata_batch(&claimed_a.id)
            .await
            .expect("CAS complete")
    );

    let cancel_count = database
        .cancel_scan_local_metadata_batches("scan-job")
        .await
        .expect("cancel pending and running batches");
    assert_eq!(cancel_count, 3);
    assert!(
        !database
            .fail_scan_local_metadata_batch(&claimed_b.id, "temporary failure", Some(i64::MAX),)
            .await
            .expect("cancelled batch failure CAS"),
        "a cancelled running batch must reject later completion"
    );
    database
        .enqueue_scan_local_metadata_batch(NewScanLocalMetadataBatch {
            id: "retry-batch",
            job_id: "retry-job",
            library_root_id: &root_id,
            batch_sequence: 0,
            source_ids: &sources,
        })
        .await
        .expect("enqueue retry batch");
    let retryable = database
        .claim_next_scan_local_metadata_batch()
        .await
        .expect("claim retryable batch")
        .expect("retryable batch");
    assert_eq!(retryable.id, "retry-batch");
    assert!(
        database
            .fail_scan_local_metadata_batch(&retryable.id, "temporary failure", Some(i64::MAX))
            .await
            .expect("fail with delayed retry")
    );
    assert!(
        database
            .claim_next_scan_local_metadata_batch()
            .await
            .expect("deferred retry check")
            .is_none(),
        "a retry must not be claimable before next_attempt_at"
    );
    sqlx::query("UPDATE scan_local_metadata_batches SET next_attempt_at = 0 WHERE id = ?")
        .bind(&retryable.id)
        .execute(database.pool())
        .await
        .expect("make delayed retry due");
    let retried = database
        .claim_next_scan_local_metadata_batch()
        .await
        .expect("claim retry")
        .expect("due retry");
    assert_eq!(retried.id, retryable.id);
    assert_eq!(retried.attempts, 2);
    assert!(
        database
            .has_pending_scan_local_metadata_images("retry-job")
            .await
            .expect("claim resets the image stage")
    );
    assert!(
        database
            .mark_scan_local_metadata_images_complete(&retried.id)
            .await
            .expect("mark retried images complete")
    );
    assert!(
        database
            .complete_scan_local_metadata_batch(&retried.id)
            .await
            .expect("complete retry")
    );

    assert_eq!(
        database
            .cancel_scan_local_metadata_batches("retry-job")
            .await
            .expect("cancel completed job remainder"),
        0
    );
    let interrupted = database
        .enqueue_scan_local_metadata_batch(NewScanLocalMetadataBatch {
            id: "batch-e",
            job_id: "interrupted-job",
            library_root_id: &root_id,
            batch_sequence: 0,
            source_ids: &sources,
        })
        .await
        .expect("enqueue interrupted batch");
    assert!(interrupted);
    let running = database
        .claim_next_scan_local_metadata_batch()
        .await
        .expect("claim interrupted batch")
        .expect("running batch");
    assert_eq!(running.id, "batch-e");
    assert!(
        database
            .mark_scan_local_metadata_images_complete(&running.id)
            .await
            .expect("mark interrupted images complete")
    );
    assert!(
        !database
            .has_pending_scan_local_metadata_images("interrupted-job")
            .await
            .expect("persisted image completion")
    );
    assert_eq!(
        database
            .requeue_interrupted_scan_local_metadata_batches()
            .await
            .expect("recover interrupted batch"),
        1
    );
    let recovered = database
        .claim_next_scan_local_metadata_batch()
        .await
        .expect("claim recovered batch")
        .expect("recovered batch");
    assert_eq!(recovered.id, "batch-e");
    assert_eq!(recovered.attempts, 2);
    assert!(
        database
            .has_pending_scan_local_metadata_images("interrupted-job")
            .await
            .expect("reclaimed batch reruns the image stage")
    );

    database.close().await;
}

#[tokio::test]
async fn scan_local_metadata_backfill_roots_register_in_one_query()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Backfill roots", LibraryKind::Movie, false)
        .await?;

    database.reset_query_count();
    assert_eq!(
        database.ensure_scan_local_metadata_backfill_roots().await?,
        0
    );
    assert_eq!(
        database.query_count(),
        1,
        "an empty root list uses one query"
    );

    for index in 0..4 {
        let root_path = temp_dir.path().join(format!("root-{index}"));
        std::fs::create_dir_all(&root_path)?;
        libraries
            .add_root(library.id, root_path.to_str().ok_or("non-UTF-8 root path")?)
            .await?;
    }

    database.reset_query_count();
    assert_eq!(
        database.ensure_scan_local_metadata_backfill_roots().await?,
        4
    );
    assert_eq!(database.query_count(), 1, "four roots use one query");
    let row_count: i64 = database
        .query_scalar("SELECT COUNT(*) FROM scan_local_metadata_backfills")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(row_count, 4);

    database.reset_query_count();
    assert_eq!(
        database.ensure_scan_local_metadata_backfill_roots().await?,
        0
    );
    assert_eq!(
        database.query_count(),
        1,
        "idempotent registration uses one query"
    );
    Ok(())
}

#[tokio::test]
async fn notification_deliveries_are_written_in_bounded_batches()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let mut destination_ids = Vec::new();
    for index in 0..205 {
        let id = format!("notification-destination-{index:03}");
        database
            .create_notification_destination(NewNotificationDestination {
                id: &id,
                name: &id,
                url: "https://example.test/webhook",
                enabled: true,
                allow_private_network: false,
                event_types_json: "[]",
                payload_format: "LUX",
                provider_plugin_id: "builtin.webhook",
                provider_config_json: "{}",
            })
            .await?;
        destination_ids.push(id);
    }

    let event_id = "notification-event-batch";
    let event = || NewNotificationEvent {
        id: event_id,
        event_type: "ITEM_ADDED",
        schema_version: 1,
        occurred_at: 1_800_000_000,
        dedupe_key: "notification-event-batch-key",
        payload_json: "{}",
    };
    sqlx::query(
        "CREATE TRIGGER reject_notification_delivery_batch
         BEFORE INSERT ON notification_deliveries
         WHEN NEW.destination_id = 'notification-destination-100'
         BEGIN
             SELECT RAISE(ABORT, 'forced delivery batch failure');
         END",
    )
    .execute(database.pool())
    .await?;
    assert!(
        database
            .insert_notification_event_with_deliveries(event(), &destination_ids)
            .await
            .is_err()
    );
    let rolled_back_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notification_events WHERE id = ?")
            .bind(event_id)
            .fetch_one(database.pool())
            .await?;
    let rolled_back_deliveries: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notification_deliveries WHERE event_id = ?")
            .bind(event_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(rolled_back_events, 0);
    assert_eq!(rolled_back_deliveries, 0);
    sqlx::query("DROP TRIGGER reject_notification_delivery_batch")
        .execute(database.pool())
        .await?;

    database.reset_query_count();
    assert!(
        database
            .insert_notification_event_with_deliveries(event(), &destination_ids)
            .await?
    );
    assert_eq!(
        database.query_count(),
        4,
        "one event plus three batches of at most 100 deliveries"
    );

    let delivery_rows: i64 = database
        .query_scalar("SELECT COUNT(*) FROM notification_deliveries WHERE event_id = ?")
        .bind(event_id)
        .fetch_one(database.pool())
        .await?;
    let pending_rows: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM notification_deliveries
             WHERE event_id = ? AND status = 'PENDING'",
        )
        .bind(event_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(delivery_rows, 205);
    assert_eq!(pending_rows, 205);

    database.reset_query_count();
    assert!(
        !database
            .insert_notification_event_with_deliveries(event(), &destination_ids)
            .await?
    );
    assert_eq!(
        database.query_count(),
        1,
        "deduped events skip delivery inserts"
    );

    database.reset_query_count();
    assert!(
        database
            .insert_notification_event_with_deliveries(
                NewNotificationEvent {
                    id: "notification-event-empty",
                    event_type: "ITEM_ADDED",
                    schema_version: 1,
                    occurred_at: 1_800_000_001,
                    dedupe_key: "notification-event-empty-key",
                    payload_json: "{}",
                },
                &[],
            )
            .await?
    );
    assert_eq!(
        database.query_count(),
        1,
        "empty destination list only inserts event"
    );

    let event_count: i64 = database
        .query_scalar("SELECT COUNT(*) FROM notification_events")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(event_count, 2);
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_notification_deliveries_are_written_in_bounded_batches()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_url = crate::config::DatabaseConfiguration::Postgres(admin_connection.clone())
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(
        &config,
        &DatabaseConfiguration::Postgres(PostgresConnection {
            database: database_name.clone(),
            ..admin_connection
        }),
    )
    .await?;
    let assertions = async {
        let mut destination_ids = Vec::new();
        for index in 0..205 {
            let id = format!("postgres-notification-destination-{index:03}");
            database
                .create_notification_destination(NewNotificationDestination {
                    id: &id,
                    name: &id,
                    url: "https://example.test/webhook",
                    enabled: true,
                    allow_private_network: false,
                    event_types_json: "[]",
                    payload_format: "LUX",
                    provider_plugin_id: "builtin.webhook",
                    provider_config_json: "{}",
                })
                .await?;
            destination_ids.push(id);
        }

        let event_id = "postgres-notification-event-batch";
        let event = || NewNotificationEvent {
            id: event_id,
            event_type: "ITEM_ADDED",
            schema_version: 1,
            occurred_at: 1_800_000_000,
            dedupe_key: "postgres-notification-event-batch-key",
            payload_json: "{}",
        };
        database.reset_query_count();
        assert!(
            database
                .insert_notification_event_with_deliveries(event(), &destination_ids)
                .await?
        );
        assert_eq!(database.query_count(), 4);
        let delivery_rows: i64 = database
            .query_scalar("SELECT COUNT(*) FROM notification_deliveries WHERE event_id = ?")
            .bind(event_id)
            .fetch_one(database.pool())
            .await?;
        assert_eq!(delivery_rows, 205);

        database.reset_query_count();
        assert!(
            !database
                .insert_notification_event_with_deliveries(event(), &destination_ids)
                .await?
        );
        assert_eq!(database.query_count(), 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    assertions?;
    drop_database?;
    Ok(())
}

#[tokio::test]
async fn progressive_scan_metadata_backfill_is_bounded_recoverable_and_root_scoped() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root)
        .await
        .expect("media root");
    for name in [
        "First.Movie.2023.mkv",
        "Second.Movie.2024.mkv",
        "Third.Movie.2025.mkv",
        "Fourth.Movie.2026.mkv",
    ] {
        tokio::fs::write(media_root.join(name), b"video")
            .await
            .expect("movie file");
    }
    tokio::fs::write(media_root.join("notes.txt"), b"not media")
        .await
        .expect("non-media file");

    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Backfill", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root = libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root");
    let root_id = root.root.id.to_string();
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("index media files");

    let expected_ids = database
        .query_scalar::<String>(
            "SELECT entry.id
             FROM filesystem_entries entry
             JOIN media_sources source ON source.filesystem_entry_id = entry.id
             JOIN media_items item ON item.id = source.item_id
             WHERE entry.library_root_id = ? AND entry.entry_kind = 'FILE'
               AND entry.is_missing = 0 AND item.removed_at IS NULL
             ORDER BY entry.id",
        )
        .bind(&root_id)
        .fetch_all(database.pool())
        .await
        .expect("read media source entry ids");
    assert_eq!(expected_ids.len(), 4, "the text file is not a media source");
    sqlx::query("UPDATE filesystem_entries SET is_missing = 1 WHERE id = ?")
        .bind(&expected_ids[0])
        .execute(database.pool())
        .await
        .expect("mark one entry missing");
    sqlx::query(
        "UPDATE media_items SET removed_at = unixepoch()
         WHERE id = (SELECT item_id FROM media_sources WHERE filesystem_entry_id = ?)",
    )
    .bind(&expected_ids[1])
    .execute(database.pool())
    .await
    .expect("remove one source item");

    assert_eq!(
        database
            .ensure_scan_local_metadata_backfill_roots()
            .await
            .expect("register existing roots"),
        1
    );
    assert!(
        !database
            .ensure_scan_local_metadata_backfill_root(&root_id)
            .await
            .expect("register existing root again"),
        "root registration is idempotent"
    );
    assert_eq!(
        database
            .ensure_scan_local_metadata_backfill_roots()
            .await
            .expect("register roots again"),
        0
    );
    assert!(
        database
            .claim_next_scan_local_metadata_backfill_page(0)
            .await
            .is_err(),
        "a zero-sized page is invalid"
    );
    assert!(
        database
            .claim_next_scan_local_metadata_backfill_page(17)
            .await
            .is_err(),
        "the public storage limit caps backfill windows"
    );

    let first = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await
        .expect("claim first page")
        .expect("first page exists");
    assert_eq!(first.library_root_id, root_id);
    assert_eq!(first.entry_ids.len(), 1);
    assert!(first.has_more);
    assert_eq!(first.cursor_entry_id, None);
    assert_eq!(first.attempts, 1);
    assert_eq!(first.entry_ids[0], expected_ids[2]);

    assert!(
        database
            .fail_scan_local_metadata_backfill_page(
                &first,
                "temporary local failure",
                Some(i64::MAX),
            )
            .await
            .expect("persist page failure")
    );
    assert!(
        database
            .claim_next_scan_local_metadata_backfill_page(1)
            .await
            .expect("check retry delay")
            .is_none(),
        "failed page remains deferred"
    );
    sqlx::query(
        "UPDATE scan_local_metadata_backfills SET next_attempt_at = 0 WHERE library_root_id = ?",
    )
    .bind(&root_id)
    .execute(database.pool())
    .await
    .expect("make retry due");
    let retried = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await
        .expect("claim retry")
        .expect("retry page exists");
    assert_eq!(retried.entry_ids, first.entry_ids);
    assert_eq!(retried.cursor_entry_id, first.cursor_entry_id);
    assert_eq!(retried.attempts, 2);
    assert!(
        !database
            .complete_scan_local_metadata_backfill_page(&first)
            .await
            .expect("reject stale worker completion"),
        "an old attempt cannot advance the retry cursor"
    );
    database
        .query(
            "UPDATE scan_local_metadata_backfills SET cursor_entry_id = ?
             WHERE library_root_id = ?",
        )
        .bind(&expected_ids[0])
        .bind(&root_id)
        .execute(database.pool())
        .await
        .expect("simulate a cursor change during the claimed attempt");
    assert!(
        !database
            .complete_scan_local_metadata_backfill_page(&retried)
            .await
            .expect("reject mismatched cursor CAS"),
        "a page cannot advance a cursor that changed during its attempt"
    );
    database
        .query(
            "UPDATE scan_local_metadata_backfills SET cursor_entry_id = NULL
             WHERE library_root_id = ?",
        )
        .bind(&root_id)
        .execute(database.pool())
        .await
        .expect("restore the claimed page cursor");
    assert!(
        database
            .complete_scan_local_metadata_backfill_page(&retried)
            .await
            .expect("advance first page")
    );

    let second = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await
        .expect("claim second page")
        .expect("second page exists");
    assert_eq!(
        second.cursor_entry_id.as_deref(),
        Some(first.entry_ids[0].as_str())
    );
    assert_eq!(second.entry_ids, vec![expected_ids[3].clone()]);
    assert!(!second.has_more);
    assert_eq!(
        database
            .requeue_interrupted_scan_local_metadata_backfills()
            .await
            .expect("recover interrupted page"),
        1
    );
    let recovered = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await
        .expect("claim recovered page")
        .expect("recovered page exists");
    assert_eq!(recovered.entry_ids, second.entry_ids);
    assert_eq!(recovered.cursor_entry_id, second.cursor_entry_id);
    assert_eq!(recovered.attempts, 4);
    assert!(
        database
            .complete_scan_local_metadata_backfill_page(&recovered)
            .await
            .expect("complete backfill")
    );
    let completed_status: String = database
        .query_scalar("SELECT status FROM scan_local_metadata_backfills WHERE library_root_id = ?")
        .bind(&root_id)
        .fetch_one(database.pool())
        .await
        .expect("read completed status");
    assert_eq!(completed_status, "COMPLETED");
    assert!(
        database
            .claim_next_scan_local_metadata_backfill_page(1)
            .await
            .expect("check completed root")
            .is_none()
    );

    let empty_root_path = temp_dir.path().join("Empty");
    tokio::fs::create_dir_all(&empty_root_path)
        .await
        .expect("empty root directory");
    let empty_library = libraries
        .create_library("Empty backfill", LibraryKind::Movie, false)
        .await
        .expect("empty library");
    let empty_root = libraries
        .add_root(
            empty_library.id,
            empty_root_path.to_str().expect("empty root path"),
        )
        .await
        .expect("empty library root");
    let empty_root_id = empty_root.root.id.to_string();
    assert!(
        database
            .ensure_scan_local_metadata_backfill_root(&empty_root_id)
            .await
            .expect("register empty root")
    );

    let populated_root_path = temp_dir.path().join("Populated");
    tokio::fs::create_dir_all(&populated_root_path)
        .await
        .expect("populated root directory");
    tokio::fs::write(populated_root_path.join("Later.Movie.2026.mkv"), b"video")
        .await
        .expect("populated root media file");
    let populated_library = libraries
        .create_library("Populated backfill", LibraryKind::Movie, false)
        .await
        .expect("populated library");
    let populated_root = libraries
        .add_root(
            populated_library.id,
            populated_root_path.to_str().expect("populated root path"),
        )
        .await
        .expect("populated library root");
    let populated_root_id = populated_root.root.id.to_string();
    LibraryScanner::new(database.clone())
        .scan_movie_library(populated_library.id)
        .await
        .expect("index populated root");
    assert!(
        database
            .ensure_scan_local_metadata_backfill_root(&populated_root_id)
            .await
            .expect("register populated root")
    );
    database
        .query(
            "UPDATE scan_local_metadata_backfills SET updated_at = 0
             WHERE library_root_id = ?",
        )
        .bind(&empty_root_id)
        .execute(database.pool())
        .await
        .expect("order empty root first");
    database
        .query(
            "UPDATE scan_local_metadata_backfills SET updated_at = 1
             WHERE library_root_id = ?",
        )
        .bind(&populated_root_id)
        .execute(database.pool())
        .await
        .expect("order populated root second");
    let populated_page = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await
        .expect("skip empty root and keep claiming")
        .expect("populated root page remains claimable");
    assert_eq!(populated_page.library_root_id, populated_root_id);
    assert_eq!(populated_page.entry_ids.len(), 1);
    assert!(
        database
            .complete_scan_local_metadata_backfill_page(&populated_page)
            .await
            .expect("complete populated root")
    );
    let empty_status: String = database
        .query_scalar("SELECT status FROM scan_local_metadata_backfills WHERE library_root_id = ?")
        .bind(&empty_root_id)
        .fetch_one(database.pool())
        .await
        .expect("read empty-root status");
    assert_eq!(empty_status, "COMPLETED");

    database
        .query("DELETE FROM library_roots WHERE id = ?")
        .bind(&empty_root_id)
        .execute(database.pool())
        .await
        .expect("delete empty root");
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM scan_local_metadata_backfills WHERE library_root_id = ?",
            )
            .bind(&empty_root_id)
            .fetch_one(database.pool())
            .await
            .expect("check root cascade"),
        0
    );

    database.close().await;
}

#[tokio::test]
async fn progressive_scan_metadata_completeness_is_versioned_and_paged() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Versioned Movie (2025)");
    let second_movie_dir = media_root.join("Second Movie (2025)");
    tokio::fs::create_dir_all(&movie_dir)
        .await
        .expect("movie directory");
    tokio::fs::create_dir_all(&second_movie_dir)
        .await
        .expect("second movie directory");
    tokio::fs::write(movie_dir.join("Versioned.Movie.2025.mkv"), b"video")
        .await
        .expect("movie file");
    tokio::fs::write(second_movie_dir.join("Second.Movie.2025.mkv"), b"video")
        .await
        .expect("second movie file");
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Completeness", LibraryKind::Movie, false)
        .await
        .expect("library");
    libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("index movie");
    let item_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE'",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await
    .expect("indexed items");
    assert_eq!(item_ids.len(), 2);
    let item_id = item_ids[0].clone();
    let second_item_id = item_ids[1].clone();

    let old_fingerprint = b"source-version-1";
    let current_fingerprint = b"source-version-2";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await
            .expect("prepare poster check")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await
            .expect("claim poster check")
    );
    assert_eq!(
        database
            .requeue_interrupted_item_metadata_completeness_checks()
            .await
            .expect("recover interrupted check"),
        1
    );
    assert_eq!(
        database
            .requeue_interrupted_item_metadata_completeness_checks()
            .await
            .expect("recovery is idempotent"),
        0
    );
    assert!(
        !database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                old_fingerprint,
                true,
                999,
            )
            .await
            .expect("recovered old worker CAS")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await
            .expect("reclaim after restart")
    );
    assert!(
        !database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await
            .expect("claim once")
    );
    assert!(
        !database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await
            .expect("same running input is idempotent")
    );

    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", current_fingerprint)
            .await
            .expect("replace stale input version")
    );
    let pending = database
        .find_item_metadata_completeness(&item_id, "POSTER")
        .await
        .expect("read reset status")
        .expect("completeness row");
    assert_eq!(pending.local_state, "PENDING");
    assert_eq!(pending.is_missing, None);
    assert_eq!(
        pending.input_fingerprint.as_deref(),
        Some(current_fingerprint.as_slice())
    );
    assert!(
        !database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                old_fingerprint,
                true,
                1_000,
            )
            .await
            .expect("stale worker result CAS")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", current_fingerprint)
            .await
            .expect("claim current input")
    );
    assert!(
        database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                current_fingerprint,
                true,
                1_001,
            )
            .await
            .expect("confirm missing poster")
    );
    let ready = database
        .find_item_metadata_completeness(&item_id, "POSTER")
        .await
        .expect("read ready state")
        .expect("ready completeness");
    assert_eq!(ready.local_state, "READY");
    assert_eq!(ready.is_missing, Some(true));
    assert_eq!(ready.checked_at, Some(1_001));
    assert!(
        !database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", current_fingerprint)
            .await
            .expect("same ready input is idempotent")
    );

    let missing = database
        .list_confirmed_missing_metadata("POSTER", None, 1)
        .await
        .expect("list confirmed missing posters");
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].item_id, item_id);
    assert!(
        database
            .list_confirmed_missing_metadata("POSTER", Some(&item_id), 1)
            .await
            .expect("page after item")
            .is_empty()
    );
    assert!(
        database
            .list_confirmed_missing_metadata("POSTER", None, 101)
            .await
            .is_err()
    );

    let failed_fingerprint = b"source-version-3";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", failed_fingerprint)
            .await
            .expect("prepare next version")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", failed_fingerprint)
            .await
            .expect("claim next version")
    );
    assert!(
        database
            .fail_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                failed_fingerprint,
                Some(4_000_000_000),
                "local read failed",
            )
            .await
            .expect("record local read failure")
    );
    assert!(
        !database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", failed_fingerprint,)
            .await
            .expect("respect completeness retry backoff")
    );
    database
        .query(
            "UPDATE item_metadata_completeness
             SET retry_after = 0
             WHERE item_id = ? AND capability = 'POSTER'",
        )
        .bind(&item_id)
        .execute(database.pool())
        .await
        .expect("expire completeness retry backoff");
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", failed_fingerprint,)
            .await
            .expect("claim completeness after retry backoff")
    );
    assert!(
        database
            .list_confirmed_missing_metadata("POSTER", None, 1)
            .await
            .expect("failed item must not be reported missing")
            .is_empty()
    );

    let backdrop_fingerprint = b"backdrop-input";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "BACKDROP", backdrop_fingerprint)
            .await
            .expect("prepare backdrop")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "BACKDROP", backdrop_fingerprint)
            .await
            .expect("claim backdrop")
    );
    assert!(
        database
            .cancel_item_metadata_completeness_check(&item_id, "BACKDROP", backdrop_fingerprint)
            .await
            .expect("cancel backdrop")
    );
    assert!(
        !database
            .finish_item_metadata_completeness_check(
                &item_id,
                "BACKDROP",
                backdrop_fingerprint,
                true,
                1_003,
            )
            .await
            .expect("cancelled result CAS")
    );

    let current_fingerprint = b"source-version-4";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", current_fingerprint)
            .await
            .expect("prepare current poster state")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", current_fingerprint)
            .await
            .expect("claim current poster state")
    );
    assert!(
        database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                current_fingerprint,
                true,
                1_004,
            )
            .await
            .expect("confirm first missing poster")
    );
    assert!(
        database
            .prepare_item_metadata_completeness_check(&second_item_id, "POSTER", b"second-poster")
            .await
            .expect("prepare second poster")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&second_item_id, "POSTER", b"second-poster")
            .await
            .expect("claim second poster")
    );
    assert!(
        database
            .finish_item_metadata_completeness_check(
                &second_item_id,
                "POSTER",
                b"second-poster",
                true,
                1_005,
            )
            .await
            .expect("confirm second missing poster")
    );
    assert!(
        database
            .prepare_item_metadata_completeness_check(&second_item_id, "BACKDROP", b"backdrop")
            .await
            .expect("prepare available backdrop")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&second_item_id, "BACKDROP", b"backdrop")
            .await
            .expect("claim available backdrop")
    );
    assert!(
        database
            .finish_item_metadata_completeness_check(
                &second_item_id,
                "BACKDROP",
                b"backdrop",
                false,
                1_006,
            )
            .await
            .expect("confirm available backdrop")
    );

    let poster_page_one = database
        .list_confirmed_missing_metadata("POSTER", None, 1)
        .await
        .expect("first missing poster page");
    assert_eq!(poster_page_one.len(), 1);
    let poster_page_two = database
        .list_confirmed_missing_metadata("POSTER", Some(&poster_page_one[0].item_id), 1)
        .await
        .expect("second missing poster page");
    assert_eq!(poster_page_two.len(), 1);
    assert_ne!(poster_page_one[0].item_id, poster_page_two[0].item_id);
    assert!(
        database
            .list_confirmed_missing_metadata("POSTER", Some(&poster_page_two[0].item_id), 1)
            .await
            .expect("end of missing poster pages")
            .is_empty()
    );
    assert!(
        database
            .list_confirmed_missing_metadata("BACKDROP", None, 1)
            .await
            .expect("available backdrop is not missing")
            .is_empty()
    );

    database.close().await;
}

#[tokio::test]
async fn local_metadata_completeness_claim_result_and_enqueue_are_atomic() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Atomic Completeness (2025)");
    tokio::fs::create_dir_all(&movie_dir)
        .await
        .expect("movie directory");
    tokio::fs::write(movie_dir.join("Atomic.Completeness.2025.mkv"), b"video")
        .await
        .expect("movie file");

    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Atomic completeness", LibraryKind::Movie, false)
        .await
        .expect("library");
    libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("index movie");
    let library_id = library.id.to_string();
    let item_id = database
        .query_scalar::<String>(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE'",
        )
        .bind(&library_id)
        .fetch_one(database.pool())
        .await
        .expect("movie item");
    let results = [
        NewItemMetadataCompletenessResult {
            item_id: &item_id,
            capability: "POSTER",
            input_fingerprint: b"atomic-completeness-v1",
            is_missing: true,
            checked_at: 1,
        },
        NewItemMetadataCompletenessResult {
            item_id: &item_id,
            capability: "METADATA",
            input_fingerprint: b"atomic-completeness-v1",
            is_missing: false,
            checked_at: 1,
        },
    ];
    let eligible_item_ids = [item_id.clone()];

    sqlx::query(
        "CREATE TRIGGER reject_atomic_fill_missing_item
         BEFORE INSERT ON metadata_reidentify_job_items
         WHEN EXISTS (
             SELECT 1 FROM metadata_reidentify_jobs
             WHERE id = NEW.job_id AND mode = 'FILL_MISSING'
         )
         BEGIN
             SELECT RAISE(ABORT, 'forced fill-missing enqueue failure');
         END",
    )
    .execute(database.pool())
    .await
    .expect("install enqueue failure trigger");

    assert!(
        database
            .complete_local_metadata_completeness_batch_with_policy(
                &library_id,
                &results,
                &eligible_item_ids,
                MetadataAutoMatchPolicy::Enabled,
            )
            .await
            .is_err(),
        "enqueue failure should abort the combined transaction"
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM item_metadata_completeness
                 WHERE item_id = ?",
            )
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("count completeness rows after rollback"),
        0,
        "failed transaction must not leave a RUNNING or READY claim"
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM metadata_reidentify_jobs
                 WHERE library_id = ? AND mode = 'FILL_MISSING'",
            )
            .bind(&library_id)
            .fetch_one(database.pool())
            .await
            .expect("count fill-missing jobs after rollback"),
        0
    );

    sqlx::query("DROP TRIGGER reject_atomic_fill_missing_item")
        .execute(database.pool())
        .await
        .expect("remove enqueue failure trigger");
    let independent_database = Database::connect(&config)
        .await
        .expect("independent database handle");
    let (retry_a, retry_b) = tokio::join!(
        database.complete_local_metadata_completeness_batch_with_policy(
            &library_id,
            &results,
            &eligible_item_ids,
            MetadataAutoMatchPolicy::Enabled,
        ),
        independent_database.complete_local_metadata_completeness_batch_with_policy(
            &library_id,
            &results,
            &eligible_item_ids,
            MetadataAutoMatchPolicy::Enabled,
        ),
    );
    let retry_a = retry_a.expect("first retry should commit");
    let retry_b = retry_b.expect("duplicate retry should commit without changes");
    assert_eq!(retry_a.updated_count + retry_b.updated_count, 2);
    assert_eq!(
        retry_a.scheduled_job_ids.len() + retry_b.scheduled_job_ids.len(),
        1
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM item_metadata_completeness
                 WHERE item_id = ? AND local_state = 'READY'",
            )
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("read completed completeness states"),
        2
    );
    let job_id = retry_a
        .scheduled_job_ids
        .first()
        .or_else(|| retry_b.scheduled_job_ids.first())
        .expect("one concurrent request schedules a job");
    fail_fill_missing_job_with_unavailable_provider(&database, job_id, &item_id)
        .await
        .expect("mark the first job deferred for provider unavailability");
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET automatic_retry_after = unixepoch() - 1
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(job_id)
    .bind(&item_id)
    .execute(database.pool())
    .await
    .expect("make the provider retry due");
    let disabled_retry = database
        .complete_local_metadata_completeness_batch_with_policy(
            &library_id,
            &results,
            &eligible_item_ids,
            MetadataAutoMatchPolicy::Disabled,
        )
        .await
        .expect("disabled auto-match must preserve due retry without enqueueing");
    assert_eq!(disabled_retry.updated_count, 0);
    assert!(disabled_retry.scheduled_job_ids.is_empty());
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
                 WHERE job_id = ? AND item_id = ?",
            )
            .bind(job_id)
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("read retry state after disabled policy"),
        0
    );
    sqlx::query(
        "CREATE TRIGGER reject_atomic_fill_missing_retry_item
         BEFORE INSERT ON metadata_reidentify_job_items
         WHEN EXISTS (
             SELECT 1 FROM metadata_reidentify_jobs
             WHERE id = NEW.job_id AND mode = 'FILL_MISSING'
         )
         BEGIN
             SELECT RAISE(ABORT, 'forced retried fill-missing enqueue failure');
         END",
    )
    .execute(database.pool())
    .await
    .expect("install retry enqueue failure trigger");
    assert!(
        database
            .complete_local_metadata_completeness_batch_with_policy(
                &library_id,
                &results,
                &eligible_item_ids,
                MetadataAutoMatchPolicy::Enabled,
            )
            .await
            .is_err(),
        "retry enqueue failure should abort retry consumption"
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
                 WHERE job_id = ? AND item_id = ?",
            )
            .bind(job_id)
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("read retry state after rollback"),
        0,
        "failed retry enqueue must restore the deferred retry"
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM metadata_reidentify_jobs
                 WHERE library_id = ? AND mode = 'FILL_MISSING'",
            )
            .bind(&library_id)
            .fetch_one(database.pool())
            .await
            .expect("count jobs after retry rollback"),
        1,
        "failed retry enqueue must not leave a partial job"
    );
    sqlx::query("DROP TRIGGER reject_atomic_fill_missing_retry_item")
        .execute(database.pool())
        .await
        .expect("remove retry enqueue failure trigger");
    let enabled_retry = database
        .complete_local_metadata_completeness_batch_with_policy(
            &library_id,
            &results,
            &eligible_item_ids,
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("enabled auto-match must schedule the due provider retry");
    assert_eq!(enabled_retry.updated_count, 0);
    assert_eq!(enabled_retry.scheduled_job_ids.len(), 1);
    assert_ne!(enabled_retry.scheduled_job_ids[0], *job_id);
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
                 WHERE job_id = ? AND item_id = ?",
            )
            .bind(job_id)
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("read consumed retry state"),
        1
    );
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'COMPLETED' WHERE job_id = ?",
    )
    .bind(&enabled_retry.scheduled_job_ids[0])
    .execute(database.pool())
    .await
    .expect("complete the retried fill-missing item");
    sqlx::query(
        "UPDATE metadata_reidentify_jobs
         SET status = 'COMPLETED', processed_count = total_count WHERE id = ?",
    )
    .bind(&enabled_retry.scheduled_job_ids[0])
    .execute(database.pool())
    .await
    .expect("complete the retried fill-missing job");
    let unchanged = database
        .complete_local_metadata_completeness_batch_with_policy(
            &library_id,
            &results,
            &eligible_item_ids,
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("unchanged ready completeness should be a no-op");
    assert_eq!(unchanged.updated_count, 0);
    assert!(unchanged.scheduled_job_ids.is_empty());
    independent_database.close().await;
    database.close().await;
}

#[tokio::test]
async fn progressive_scan_metadata_completeness_batches_are_atomic_and_versioned() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Batch Claims (2024)");
    tokio::fs::create_dir_all(&movie_dir)
        .await
        .expect("movie directory");
    tokio::fs::write(movie_dir.join("Batch.Claims.2024.mkv"), b"movie")
        .await
        .expect("movie file");
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Completeness claims", LibraryKind::Movie, false)
        .await
        .expect("library");
    libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("index item");
    let item_id = database
        .query_scalar::<String>(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE'",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await
        .expect("movie item");
    let fingerprint_v1 = b"batch-input-v1";
    let checks = [
        NewItemMetadataCompletenessCheck {
            item_id: &item_id,
            capability: "POSTER",
            input_fingerprint: fingerprint_v1,
        },
        NewItemMetadataCompletenessCheck {
            item_id: &item_id,
            capability: "METADATA",
            input_fingerprint: fingerprint_v1,
        },
    ];

    let (claimed_a, claimed_b) = tokio::join!(
        database.prepare_and_claim_item_metadata_completeness_checks(&checks),
        database.prepare_and_claim_item_metadata_completeness_checks(&checks),
    );
    let claimed_a = claimed_a.expect("first batch claim");
    let claimed_b = claimed_b.expect("concurrent batch claim");
    assert_eq!(claimed_a.len() + claimed_b.len(), 2);
    assert!(
        claimed_a.is_empty() || claimed_b.is_empty(),
        "concurrent workers cannot claim one input twice"
    );

    let library_id = library.id.to_string();
    let ready_results = [
        NewItemMetadataCompletenessResult {
            item_id: &item_id,
            capability: "POSTER",
            input_fingerprint: fingerprint_v1,
            is_missing: true,
            checked_at: 1_000,
        },
        NewItemMetadataCompletenessResult {
            item_id: &item_id,
            capability: "METADATA",
            input_fingerprint: fingerprint_v1,
            is_missing: false,
            checked_at: 1_000,
        },
    ];
    let completed = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &ready_results, &[])
        .await
        .expect("save local results without dispatch");
    assert_eq!(completed.updated_count, 2);
    assert!(completed.scheduled_job_ids.is_empty());
    assert!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&checks)
            .await
            .expect("same completed input stays ready")
            .is_empty()
    );

    let fingerprint_v2 = b"batch-input-v2";
    let replacement = [NewItemMetadataCompletenessCheck {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: fingerprint_v2,
    }];
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&replacement)
            .await
            .expect("replace changed input version"),
        vec![0]
    );
    assert!(
        !database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                fingerprint_v1,
                true,
                1_001,
            )
            .await
            .expect("reject stale worker result")
    );
    let replacement_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: fingerprint_v2,
        is_missing: false,
        checked_at: 1_002,
    }];
    assert_eq!(
        database
            .complete_local_metadata_and_enqueue_fill_missing(&library_id, &replacement_result, &[])
            .await
            .expect("complete replacement version")
            .updated_count,
        1
    );

    let retry_fingerprint = b"batch-retry-input";
    let retry_check = [NewItemMetadataCompletenessCheck {
        item_id: &item_id,
        capability: "BACKDROP",
        input_fingerprint: retry_fingerprint,
    }];
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&retry_check)
            .await
            .expect("claim retry check"),
        vec![0]
    );
    assert!(
        database
            .fail_item_metadata_completeness_check(
                &item_id,
                "BACKDROP",
                retry_fingerprint,
                Some(2_000),
                "temporary local error",
            )
            .await
            .expect("fail local capability")
    );
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&retry_check)
            .await
            .expect("retry failed check"),
        vec![0]
    );
    let retry_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "BACKDROP",
        input_fingerprint: retry_fingerprint,
        is_missing: false,
        checked_at: 2_001,
    }];
    assert_eq!(
        database
            .complete_local_metadata_and_enqueue_fill_missing(&library_id, &retry_result, &[])
            .await
            .expect("complete failed check retry")
            .updated_count,
        1
    );

    let duplicate_checks = [checks[0], checks[0]];
    assert!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&duplicate_checks)
            .await
            .is_err()
    );
    let empty_fingerprint = [NewItemMetadataCompletenessCheck {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: &[],
    }];
    assert!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&empty_fingerprint)
            .await
            .is_err()
    );
    let oversized_fingerprint = vec![0; 257];
    let oversized_fingerprint_check = [NewItemMetadataCompletenessCheck {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: &oversized_fingerprint,
    }];
    assert!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&oversized_fingerprint_check)
            .await
            .is_err()
    );
    let oversized_checks = vec![checks[0]; 513];
    assert!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&oversized_checks)
            .await
            .is_err()
    );
    database.close().await;
}

#[tokio::test]
async fn metadata_completeness_claims_use_bounded_sql_batches() {
    const CHECK_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Completeness batch", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    let item_ids = (0..CHECK_COUNT)
        .map(|index| format!("completeness-batch-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    let checks = item_ids
        .iter()
        .map(|item_id| NewItemMetadataCompletenessCheck {
            item_id,
            capability: "POSTER",
            input_fingerprint: b"completeness-batch-v1",
        })
        .collect::<Vec<_>>();

    database.reset_query_count();
    let claimed = database
        .prepare_and_claim_item_metadata_completeness_checks(&checks)
        .await
        .expect("claim completeness batch");
    assert_eq!(database.query_count(), 6);
    assert_eq!(claimed, (0..CHECK_COUNT).collect::<Vec<_>>());
    let running_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM item_metadata_completeness WHERE local_state = 'RUNNING'",
    )
    .fetch_one(database.pool())
    .await
    .expect("running completeness count");
    assert_eq!(running_count, CHECK_COUNT as i64);

    database.close().await;
}

#[tokio::test]
async fn metadata_completeness_results_use_bounded_update_batches() {
    const RESULT_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Completeness result batch", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    let item_ids = (0..RESULT_COUNT)
        .map(|index| format!("completeness-result-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    let checks = item_ids
        .iter()
        .map(|item_id| NewItemMetadataCompletenessCheck {
            item_id,
            capability: "POSTER",
            input_fingerprint: b"completeness-result-v1",
        })
        .collect::<Vec<_>>();
    database
        .prepare_and_claim_item_metadata_completeness_checks(&checks)
        .await
        .expect("claim completeness results");
    let results = item_ids
        .iter()
        .enumerate()
        .map(|(index, item_id)| NewItemMetadataCompletenessResult {
            item_id,
            capability: "POSTER",
            input_fingerprint: b"completeness-result-v1",
            is_missing: index % 2 == 0,
            checked_at: 10,
        })
        .collect::<Vec<_>>();

    database.reset_query_count();
    let commit = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &results,
            &[],
            MetadataAutoMatchPolicy::Disabled,
        )
        .await
        .expect("complete completeness results");
    assert_eq!(database.query_count(), 4);
    assert_eq!(commit.updated_count, RESULT_COUNT);
    let ready_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM item_metadata_completeness WHERE local_state = 'READY'",
    )
    .fetch_one(database.pool())
    .await
    .expect("ready completeness count");
    let missing_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM item_metadata_completeness
         WHERE local_state = 'READY' AND is_missing = 1",
    )
    .fetch_one(database.pool())
    .await
    .expect("missing completeness count");
    assert_eq!(ready_count, RESULT_COUNT as i64);
    assert_eq!(missing_count, RESULT_COUNT.div_ceil(2) as i64);

    database.close().await;
}

#[tokio::test]
async fn local_metadata_due_retry_candidates_use_bounded_query_batches() {
    const ITEM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Completeness retry batches", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    let item_ids = (0..ITEM_COUNT)
        .map(|index| format!("completeness-retry-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    let results = item_ids
        .iter()
        .map(|item_id| NewItemMetadataCompletenessResult {
            item_id,
            capability: "POSTER",
            input_fingerprint: b"completeness-retry-batch-v1",
            is_missing: false,
            checked_at: 10,
        })
        .collect::<Vec<_>>();

    database.reset_query_count();
    let commit = database
        .complete_local_metadata_completeness_batch_with_policy(
            &library_id,
            &results,
            &item_ids,
            MetadataAutoMatchPolicy::Disabled,
        )
        .await
        .expect("complete a candidate page larger than one SQL chunk");

    assert_eq!(commit.updated_count, ITEM_COUNT);
    assert_eq!(database.query_count(), 13);
    database.close().await;
}

#[tokio::test]
async fn user_library_order_is_replaced_in_bounded_batches() {
    const LIBRARY_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user_id = "library-order-batch-user";
    database
        .insert_user(
            user_id,
            user_id,
            "Library order user",
            "test-hash",
            false,
            true,
        )
        .await
        .expect("user");
    let library_ids = (0..LIBRARY_COUNT)
        .map(|index| format!("library-order-batch-{index:03}"))
        .collect::<Vec<_>>();
    for library_id in &library_ids {
        sqlx::query("INSERT INTO libraries (id, name, kind) VALUES (?, ?, 'MOVIE')")
            .bind(library_id)
            .bind(library_id)
            .execute(database.pool())
            .await
            .expect("library");
    }

    database.reset_query_count();
    database
        .replace_user_library_order(user_id, &library_ids)
        .await
        .expect("replace library order");
    assert_eq!(database.query_count(), 4);
    let stored = database
        .user_library_order(user_id)
        .await
        .expect("library order");
    assert_eq!(stored, library_ids);
    let positions: Vec<i64> = sqlx::query_scalar(
        "SELECT position FROM user_library_order WHERE user_id = ? ORDER BY position",
    )
    .bind(user_id)
    .fetch_all(database.pool())
    .await
    .expect("stored positions");
    assert_eq!(positions, (0..LIBRARY_COUNT as i64).collect::<Vec<_>>());

    let mut invalid_ids = library_ids.clone();
    invalid_ids[LIBRARY_COUNT - 1] = invalid_ids[0].clone();
    database.reset_query_count();
    assert!(
        database
            .replace_user_library_order(user_id, &invalid_ids)
            .await
            .is_err()
    );
    assert_eq!(database.query_count(), 4);
    assert_eq!(
        database
            .user_library_order(user_id)
            .await
            .expect("rollback order"),
        library_ids
    );

    database.reset_query_count();
    database
        .replace_user_library_order(user_id, &[])
        .await
        .expect("clear library order");
    assert_eq!(database.query_count(), 1);
    assert!(
        database
            .user_library_order(user_id)
            .await
            .expect("cleared library order")
            .is_empty()
    );
    database.close().await;
}

#[tokio::test]
async fn image_source_urls_are_checked_with_one_bounded_query() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Image source URL lookup", LibraryKind::Movie, false)
        .await
        .expect("library");
    database
        .query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('image-url-batch-item', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await
        .expect("media item");
    for (index, source_url) in [
        (0, "https://images.example/old-1"),
        (1, "https://images.example/old-2"),
    ] {
        database
            .query(
                "INSERT INTO item_images (
                    id, item_id, image_type, image_index, local_path, source, source_url
                 ) VALUES (?, 'image-url-batch-item', 'FANART', ?, ?, 'TMDB', ?)",
            )
            .bind(format!("image-{index}"))
            .bind(index)
            .bind(format!("/images/{index}.jpg"))
            .bind(source_url)
            .execute(database.pool())
            .await
            .expect("seed indexed image URL");
    }

    let urls = vec![
        "https://images.example/old-1".to_owned(),
        "https://images.example/new-1".to_owned(),
        "https://images.example/old-2".to_owned(),
        "https://images.example/new-2".to_owned(),
    ];
    database.reset_query_count();
    let existing = database
        .list_existing_item_image_source_urls("image-url-batch-item", "FANART", &urls)
        .await
        .expect("lookup indexed image URLs");
    assert_eq!(database.query_count(), 1);
    assert_eq!(
        existing,
        std::collections::HashSet::from([
            "https://images.example/old-1".to_owned(),
            "https://images.example/old-2".to_owned(),
        ])
    );

    database.reset_query_count();
    assert!(
        database
            .list_existing_item_image_source_urls("image-url-batch-item", "FANART", &[])
            .await
            .expect("empty image URL batch")
            .is_empty()
    );
    assert_eq!(database.query_count(), 0);
    database.close().await;
}

#[tokio::test]
async fn local_item_image_batch_is_bounded_idempotent_and_atomic() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    for directory in ["Batch First (2024)", "Batch Second (2024)"] {
        let movie_dir = media_root.join(directory);
        tokio::fs::create_dir_all(&movie_dir)
            .await
            .expect("movie directory");
        tokio::fs::write(
            movie_dir.join(format!("{}.mkv", directory.replace(' ', "."))),
            b"video",
        )
        .await
        .expect("movie file");
    }
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Local image batch", LibraryKind::Movie, false)
        .await
        .expect("library");
    libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("index movies");
    let item_ids: Vec<String> = database
        .query_scalar(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY title",
        )
        .bind(library.id.to_string())
        .fetch_all(database.pool())
        .await
        .expect("movie ids");
    assert_eq!(item_ids.len(), 2);

    for item_id in &item_ids {
        database
            .set_poster_fallback_required(item_id, true)
            .await
            .expect("require fallback before poster write");
    }
    let make_image = |image_type: &str, index: i64, path: &str, tag: &str| ItemImageInsert {
        image_type: image_type.to_owned(),
        image_index: index,
        local_path: path.to_owned(),
        file_size: 128,
        width: Some(64),
        height: Some(96),
        content_tag: tag.to_owned(),
        source: "LOCAL".to_owned(),
        source_url: None,
    };
    let batch = [
        ItemImageBatchInsert {
            item_id: item_ids[0].clone(),
            images: vec![
                make_image("POSTER", 0, "/media/first/poster.jpg", "first-poster-v1"),
                make_image("FANART", 0, "/media/first/fanart.jpg", "first-fanart-v1"),
                make_image("FANART", 1, "/media/first/fanart-2.jpg", "first-fanart-v2"),
            ],
            clear_poster_fallback: true,
        },
        ItemImageBatchInsert {
            item_id: item_ids[1].clone(),
            images: vec![make_image(
                "POSTER",
                0,
                "/media/second/poster.jpg",
                "second-poster-v1",
            )],
            clear_poster_fallback: true,
        },
    ];
    database.reset_query_count();
    assert_eq!(
        database
            .insert_item_images_batch_at_indices(&batch)
            .await
            .expect("insert images for both items"),
        4
    );
    assert_eq!(
        database.query_count(),
        2,
        "image rows and fallback clear share one transaction"
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM item_images WHERE item_id = ? AND image_type = 'FANART'",
            )
            .bind(&item_ids[0])
            .fetch_one(database.pool())
            .await
            .expect("count ordered fanart"),
        2
    );
    let fallback: Vec<i64> = database
        .query_scalar(
            "SELECT poster_fallback_required FROM media_items
             WHERE id IN (?, ?) ORDER BY title",
        )
        .bind(&item_ids[0])
        .bind(&item_ids[1])
        .fetch_all(database.pool())
        .await
        .expect("read fallback flags");
    assert_eq!(fallback, vec![0, 0]);
    assert_eq!(
        database
            .insert_item_images_batch_at_indices(&batch)
            .await
            .expect("repeat identical images"),
        0,
        "identical content is an idempotent upsert"
    );
    let changed_poster_batch = [ItemImageBatchInsert {
        item_id: item_ids[0].clone(),
        images: vec![make_image(
            "POSTER",
            0,
            "/media/first/poster-updated.jpg",
            "first-poster-v2",
        )],
        clear_poster_fallback: true,
    }];
    assert_eq!(
        database
            .insert_item_images_batch_at_indices(&changed_poster_batch)
            .await
            .expect("update changed poster path"),
        1
    );
    assert_eq!(
        database
            .query_scalar::<String>(
                "SELECT local_path FROM item_images
                 WHERE item_id = ? AND image_type = 'POSTER' AND image_index = 0",
            )
            .bind(&item_ids[0])
            .fetch_one(database.pool())
            .await
            .expect("read updated poster path"),
        "/media/first/poster-updated.jpg"
    );
    assert_eq!(
        database
            .insert_item_images_batch_at_indices(&[])
            .await
            .expect("empty page is a no-op"),
        0
    );
    database.reset_query_count();
    assert_eq!(
        database
            .insert_item_images_batch_at_indices(&[ItemImageBatchInsert {
                item_id: item_ids[0].clone(),
                images: Vec::new(),
                clear_poster_fallback: false,
            }])
            .await
            .expect("image-less item page is a no-op"),
        0
    );
    assert_eq!(database.query_count(), 0);
    let oversized_batch = (0..17)
        .map(|index| ItemImageBatchInsert {
            item_id: format!("item-{index}"),
            images: Vec::new(),
            clear_poster_fallback: false,
        })
        .collect::<Vec<_>>();
    assert!(
        database
            .insert_item_images_batch_at_indices(&oversized_batch)
            .await
            .is_err(),
        "a storage page cannot exceed sixteen items"
    );

    let second_item_id = item_ids[1].clone();
    let trigger_sql = format!(
        "CREATE TRIGGER reject_second_local_image
         BEFORE INSERT ON item_images
         WHEN NEW.item_id = '{second_item_id}' AND NEW.local_path LIKE '%reject%'
         BEGIN SELECT RAISE(ABORT, 'injected image batch failure'); END"
    );
    sqlx::query(sqlx::AssertSqlSafe(trigger_sql))
        .execute(database.pool())
        .await
        .expect("install failure trigger");
    for item_id in &item_ids {
        database
            .set_poster_fallback_required(item_id, true)
            .await
            .expect("restore fallback before rollback test");
    }
    let failing_batch = [
        ItemImageBatchInsert {
            item_id: item_ids[0].clone(),
            images: vec![make_image(
                "POSTER",
                0,
                "/media/first/poster-failed.jpg",
                "first-poster-v3",
            )],
            clear_poster_fallback: true,
        },
        ItemImageBatchInsert {
            item_id: item_ids[1].clone(),
            images: vec![make_image(
                "LOGO",
                0,
                "/media/second/reject-logo.jpg",
                "reject-logo",
            )],
            clear_poster_fallback: true,
        },
    ];
    assert!(
        database
            .insert_item_images_batch_at_indices(&failing_batch)
            .await
            .is_err(),
        "a single image failure rolls back the complete item page"
    );
    sqlx::query("DROP TRIGGER reject_second_local_image")
        .execute(database.pool())
        .await
        .expect("drop failure trigger");
    let first_image_path: String = database
        .query_scalar(
            "SELECT local_path FROM item_images
             WHERE item_id = ? AND image_type = 'POSTER' AND image_index = 0",
        )
        .bind(&item_ids[0])
        .fetch_one(database.pool())
        .await
        .expect("read first image after rollback");
    assert_eq!(first_image_path, "/media/first/poster-updated.jpg");
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM item_images WHERE item_id = ? AND image_type = 'LOGO'",
            )
            .bind(&item_ids[1])
            .fetch_one(database.pool())
            .await
            .expect("count rolled-back logo"),
        0
    );
    let fallback: Vec<i64> = database
        .query_scalar(
            "SELECT poster_fallback_required FROM media_items
             WHERE id IN (?, ?) ORDER BY title",
        )
        .bind(&item_ids[0])
        .bind(&item_ids[1])
        .fetch_all(database.pool())
        .await
        .expect("read rolled-back fallback flags");
    assert_eq!(fallback, vec![1, 1]);
    database.close().await;
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_local_item_image_batch_is_bounded_idempotent_and_atomic()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration = DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let connection = PostgresConnection {
        database: database_name.clone(),
        ..admin_connection
    };
    let database =
        Database::connect_with_configuration(&config, &DatabaseConfiguration::Postgres(connection))
            .await?;
    let media_root = temp_dir.path().join("Movies");
    for directory in [
        "Postgres Batch First (2024)",
        "Postgres Batch Second (2024)",
    ] {
        let movie_dir = media_root.join(directory);
        tokio::fs::create_dir_all(&movie_dir).await?;
        tokio::fs::write(
            movie_dir.join(format!("{}.mkv", directory.replace(' ', "."))),
            b"video",
        )
        .await?;
    }
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Postgres local image batch", LibraryKind::Movie, false)
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
    let item_ids: Vec<String> = database
        .query_scalar(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY title",
        )
        .bind(library.id.to_string())
        .fetch_all(database.pool())
        .await?;
    assert_eq!(item_ids.len(), 2);
    for item_id in &item_ids {
        database.set_poster_fallback_required(item_id, true).await?;
    }
    let make_image = |image_type: &str, index: i64, path: String, tag: String| ItemImageInsert {
        image_type: image_type.to_owned(),
        image_index: index,
        local_path: path,
        file_size: 128,
        width: Some(64),
        height: Some(96),
        content_tag: tag,
        source: "LOCAL".to_owned(),
        source_url: None,
    };
    let mut first_images = vec![make_image(
        "POSTER",
        0,
        "/media/first/poster.jpg".to_owned(),
        "poster-v1".to_owned(),
    )];
    first_images.extend((0..65).map(|index| {
        make_image(
            "FANART",
            index,
            format!("/media/first/fanart-{index}.jpg"),
            format!("fanart-v{index}"),
        )
    }));
    let batch = [
        ItemImageBatchInsert {
            item_id: item_ids[0].clone(),
            images: first_images,
            clear_poster_fallback: true,
        },
        ItemImageBatchInsert {
            item_id: item_ids[1].clone(),
            images: vec![make_image(
                "POSTER",
                0,
                "/media/second/poster.jpg".to_owned(),
                "poster-v1".to_owned(),
            )],
            clear_poster_fallback: true,
        },
    ];
    assert_eq!(
        database.insert_item_images_batch_at_indices(&batch).await?,
        67
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM item_images WHERE item_id = ? AND image_type = 'FANART'",
            )
            .bind(&item_ids[0])
            .fetch_one(database.pool())
            .await?,
        65
    );
    assert_eq!(
        database.insert_item_images_batch_at_indices(&batch).await?,
        0
    );
    let changed_poster_batch = [ItemImageBatchInsert {
        item_id: item_ids[0].clone(),
        images: vec![make_image(
            "POSTER",
            0,
            "/media/first/poster-updated.jpg".to_owned(),
            "poster-v2".to_owned(),
        )],
        clear_poster_fallback: true,
    }];
    assert_eq!(
        database
            .insert_item_images_batch_at_indices(&changed_poster_batch)
            .await?,
        1
    );
    assert_eq!(
        database
            .query_scalar::<String>(
                "SELECT local_path FROM item_images
                 WHERE item_id = ? AND image_type = 'POSTER' AND image_index = 0",
            )
            .bind(&item_ids[0])
            .fetch_one(database.pool())
            .await?,
        "/media/first/poster-updated.jpg"
    );
    let empty_batch: [ItemImageBatchInsert; 0] = [];
    assert_eq!(
        database
            .insert_item_images_batch_at_indices(&empty_batch)
            .await?,
        0
    );

    let oversized = (0..17)
        .map(|index| ItemImageBatchInsert {
            item_id: format!("postgres-item-{index}"),
            images: Vec::new(),
            clear_poster_fallback: false,
        })
        .collect::<Vec<_>>();
    assert!(
        database
            .insert_item_images_batch_at_indices(&oversized)
            .await
            .is_err()
    );

    let trigger_function = format!(
        "CREATE FUNCTION reject_postgres_local_image_batch() RETURNS trigger AS $$
         BEGIN
             IF NEW.item_id = '{second_item}' AND NEW.local_path LIKE '%reject%' THEN
                 RAISE EXCEPTION 'injected image batch failure';
             END IF;
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql",
        second_item = item_ids[1],
    );
    sqlx::query(sqlx::AssertSqlSafe(trigger_function))
        .execute(database.pool())
        .await?;
    sqlx::query(
        "CREATE TRIGGER reject_postgres_local_image_batch
         BEFORE INSERT ON item_images
         FOR EACH ROW EXECUTE FUNCTION reject_postgres_local_image_batch()",
    )
    .execute(database.pool())
    .await?;
    for item_id in &item_ids {
        database.set_poster_fallback_required(item_id, true).await?;
    }
    let failing_batch = [
        ItemImageBatchInsert {
            item_id: item_ids[0].clone(),
            images: vec![make_image(
                "POSTER",
                0,
                "/media/first/poster-failed.jpg".to_owned(),
                "poster-v3".to_owned(),
            )],
            clear_poster_fallback: true,
        },
        ItemImageBatchInsert {
            item_id: item_ids[1].clone(),
            images: vec![make_image(
                "LOGO",
                0,
                "/media/second/reject-logo.jpg".to_owned(),
                "reject-logo".to_owned(),
            )],
            clear_poster_fallback: true,
        },
    ];
    assert!(
        database
            .insert_item_images_batch_at_indices(&failing_batch)
            .await
            .is_err()
    );
    sqlx::query("DROP TRIGGER reject_postgres_local_image_batch ON item_images")
        .execute(database.pool())
        .await?;
    sqlx::query("DROP FUNCTION reject_postgres_local_image_batch()")
        .execute(database.pool())
        .await?;
    assert_eq!(
        database
            .query_scalar::<String>(
                "SELECT local_path FROM item_images
                 WHERE item_id = ? AND image_type = 'POSTER' AND image_index = 0",
            )
            .bind(&item_ids[0])
            .fetch_one(database.pool())
            .await?,
        "/media/first/poster-updated.jpg"
    );
    let fallback: Vec<i64> = database
        .query_scalar(
            "SELECT poster_fallback_required FROM media_items
             WHERE id IN (?, ?) ORDER BY title",
        )
        .bind(&item_ids[0])
        .bind(&item_ids[1])
        .fetch_all(database.pool())
        .await?;
    assert_eq!(fallback, vec![1, 1]);
    database.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    admin_pool.close().await;
    Ok(())
}

#[tokio::test]
async fn progressive_scan_metadata_dispatch_is_atomic_and_deduplicated() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    for directory in [
        "First Movie (2025)",
        "Second Movie (2025)",
        "Third Movie (2025)",
        "Replay Movie (2025)",
    ] {
        let movie_dir = media_root.join(directory);
        tokio::fs::create_dir_all(&movie_dir)
            .await
            .expect("movie directory");
        let filename = directory.replace(' ', ".");
        tokio::fs::write(movie_dir.join(format!("{filename}.mkv")), b"video")
            .await
            .expect("movie file");
    }
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Progressive dispatch", LibraryKind::Movie, false)
        .await
        .expect("library");
    libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("index movies");
    let item_ids: Vec<String> = database
        .query_scalar(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY id",
        )
        .bind(library.id.to_string())
        .fetch_all(database.pool())
        .await
        .expect("indexed movies");
    assert_eq!(item_ids.len(), 4);
    let library_id = library.id.to_string();

    sqlx::query("UPDATE libraries SET scan_missing_metadata_auto_match_enabled = 0 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("disable scan auto-match");
    let poster_fingerprints = [
        b"poster-v1".to_vec(),
        b"poster-v2".to_vec(),
        b"poster-v3".to_vec(),
        b"poster-v4".to_vec(),
    ];
    let mut poster_results = Vec::new();
    for (item_id, fingerprint) in item_ids.iter().zip(&poster_fingerprints) {
        assert!(
            database
                .prepare_item_metadata_completeness_check(item_id, "POSTER", fingerprint)
                .await
                .expect("prepare poster")
        );
        assert!(
            database
                .claim_item_metadata_completeness_check(item_id, "POSTER", fingerprint)
                .await
                .expect("claim poster")
        );
        poster_results.push(NewItemMetadataCompletenessResult {
            item_id,
            capability: "POSTER",
            input_fingerprint: fingerprint,
            is_missing: true,
            checked_at: 10,
        });
    }
    let disabled = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &poster_results, &item_ids)
        .await
        .expect("persist missing posters while auto-match is disabled");
    assert_eq!(disabled.updated_count, 4);
    assert!(disabled.scheduled_job_ids.is_empty());
    for item_id in &item_ids {
        let completeness = database
            .find_item_metadata_completeness(item_id, "POSTER")
            .await
            .expect("read disabled missing poster")
            .expect("poster completeness");
        assert_eq!(completeness.local_state, "READY");
        assert_eq!(completeness.is_missing, Some(true));
    }

    sqlx::query("UPDATE libraries SET scan_missing_metadata_auto_match_enabled = 1 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("enable scan auto-match");

    let incremental_disabled_fingerprint = b"incremental-disabled";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                &item_ids[0],
                "EXTERNAL_IDS",
                incremental_disabled_fingerprint,
            )
            .await
            .expect("prepare disabled incremental capability")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                &item_ids[0],
                "EXTERNAL_IDS",
                incremental_disabled_fingerprint,
            )
            .await
            .expect("claim disabled incremental capability")
    );
    let incremental_disabled_result = [NewItemMetadataCompletenessResult {
        item_id: &item_ids[0],
        capability: "EXTERNAL_IDS",
        input_fingerprint: incremental_disabled_fingerprint,
        is_missing: true,
        checked_at: 10,
    }];
    let incremental_disabled = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &incremental_disabled_result,
            std::slice::from_ref(&item_ids[0]),
            MetadataAutoMatchPolicy::Disabled,
        )
        .await
        .expect("incremental job policy overrides enabled full-scan policy");
    assert_eq!(incremental_disabled.updated_count, 1);
    assert!(incremental_disabled.scheduled_job_ids.is_empty());

    sqlx::query("UPDATE libraries SET scan_missing_metadata_auto_match_enabled = 0 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("disable full-scan policy");
    let incremental_enabled_fingerprint = b"incremental-enabled";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                &item_ids[2],
                "TRAILERS",
                incremental_enabled_fingerprint,
            )
            .await
            .expect("prepare enabled incremental capability")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                &item_ids[2],
                "TRAILERS",
                incremental_enabled_fingerprint,
            )
            .await
            .expect("claim enabled incremental capability")
    );
    let incremental_enabled_result = [NewItemMetadataCompletenessResult {
        item_id: &item_ids[2],
        capability: "TRAILERS",
        input_fingerprint: incremental_enabled_fingerprint,
        is_missing: true,
        checked_at: 10,
    }];
    database.reset_query_count();
    let incremental_enabled = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &incremental_enabled_result,
            std::slice::from_ref(&item_ids[2]),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("incremental job policy can enable auto-match for its sources");
    assert_eq!(incremental_enabled.scheduled_job_ids.len(), 1);
    assert_eq!(
        database.query_count(),
        7,
        "completeness confirmation and schedulable-item lookup should share one page query"
    );

    let replay_fingerprint = b"replay-without-new-result";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_ids[3], "POSTER", replay_fingerprint,)
            .await
            .expect("prepare replay poster")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_ids[3], "POSTER", replay_fingerprint,)
            .await
            .expect("claim replay poster")
    );
    let replay_result = [NewItemMetadataCompletenessResult {
        item_id: &item_ids[3],
        capability: "POSTER",
        input_fingerprint: replay_fingerprint,
        is_missing: true,
        checked_at: 10,
    }];
    let policy_disabled = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &replay_result,
            std::slice::from_ref(&item_ids[3]),
            MetadataAutoMatchPolicy::Disabled,
        )
        .await
        .expect("persist missing without scheduling when the call disables auto-match");
    assert_eq!(policy_disabled.updated_count, 1);
    assert!(policy_disabled.scheduled_job_ids.is_empty());
    let replayed_without_new_result = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &[],
            std::slice::from_ref(&item_ids[3]),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("schedule an already-confirmed missing poster after policy changes");
    assert_eq!(replayed_without_new_result.updated_count, 0);
    assert_eq!(replayed_without_new_result.scheduled_job_ids.len(), 1);
    let replayed_active_job = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &[],
            std::slice::from_ref(&item_ids[3]),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("deduplicate replay against the active fill-missing job");
    assert!(replayed_active_job.scheduled_job_ids.is_empty());

    sqlx::query(
        "UPDATE metadata_reidentify_jobs
         SET status = 'DEFERRED', updated_at = unixepoch()
         WHERE id IN (
             SELECT job_id FROM metadata_reidentify_job_items WHERE item_id = ?
         ) AND mode = 'FILL_MISSING'",
    )
    .bind(&item_ids[3])
    .execute(database.pool())
    .await
    .expect("defer the existing fill-missing job");
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'FAILED', error = 'SCRAPER_UNAVAILABLE'
         WHERE item_id = ? AND job_id IN (
             SELECT id FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'
         )",
    )
    .bind(&item_ids[3])
    .execute(database.pool())
    .await
    .expect("record provider-unavailable item result");
    let replayed_deferred_job = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &[],
            std::slice::from_ref(&item_ids[3]),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("deduplicate replay against the deferred fill-missing job");
    assert!(replayed_deferred_job.scheduled_job_ids.is_empty());

    sqlx::query("UPDATE libraries SET scan_missing_metadata_auto_match_enabled = 1 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("restore full-scan policy");
    database
        .create_metadata_reidentify_job(
            "manual-fill-missing",
            &[item_ids[0].clone()],
            "FILL_MISSING",
        )
        .await
        .expect("create active manual fill-missing job");

    let backdrop_fingerprints = [
        b"backdrop-v1".to_vec(),
        b"backdrop-v2".to_vec(),
        b"backdrop-v3".to_vec(),
        b"backdrop-v4".to_vec(),
    ];
    let mut backdrop_results = Vec::new();
    for (item_id, fingerprint) in item_ids.iter().zip(&backdrop_fingerprints) {
        assert!(
            database
                .prepare_item_metadata_completeness_check(item_id, "BACKDROP", fingerprint)
                .await
                .expect("prepare backdrop")
        );
        assert!(
            database
                .claim_item_metadata_completeness_check(item_id, "BACKDROP", fingerprint)
                .await
                .expect("claim backdrop")
        );
        backdrop_results.push(NewItemMetadataCompletenessResult {
            item_id,
            capability: "BACKDROP",
            input_fingerprint: fingerprint,
            is_missing: true,
            checked_at: 11,
        });
    }

    sqlx::query(
        "CREATE TRIGGER reject_fill_missing_job
         BEFORE INSERT ON metadata_reidentify_job_items
         WHEN EXISTS (
             SELECT 1 FROM metadata_reidentify_jobs
             WHERE id = NEW.job_id AND mode = 'FILL_MISSING'
         )
         BEGIN SELECT RAISE(ABORT, 'injected fill-missing job failure'); END",
    )
    .execute(database.pool())
    .await
    .expect("install rollback trigger");
    assert!(
        database
            .complete_local_metadata_and_enqueue_fill_missing(
                &library_id,
                &backdrop_results,
                &item_ids,
            )
            .await
            .is_err()
    );
    sqlx::query("DROP TRIGGER reject_fill_missing_job")
        .execute(database.pool())
        .await
        .expect("remove rollback trigger");
    for item_id in &item_ids {
        let completeness = database
            .find_item_metadata_completeness(item_id, "BACKDROP")
            .await
            .expect("read rolled-back completeness")
            .expect("backdrop completeness");
        assert_eq!(completeness.local_state, "RUNNING");
        assert_eq!(completeness.is_missing, None);
    }

    let scheduled = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &backdrop_results, &item_ids)
        .await
        .expect("atomically persist and schedule missing backdrops");
    assert_eq!(scheduled.updated_count, 4);
    assert_eq!(scheduled.scheduled_job_ids.len(), 1);
    let scheduled_job_id = &scheduled.scheduled_job_ids[0];
    let job: (String, String, i64, String) = database
        .query_as(
            "SELECT mode, status, total_count, job_scope
             FROM metadata_reidentify_jobs WHERE id = ?",
        )
        .bind(scheduled_job_id)
        .fetch_one(database.pool())
        .await
        .expect("scheduled fill-missing job");
    assert_eq!(
        job,
        (
            "FILL_MISSING".to_owned(),
            "QUEUED".to_owned(),
            4,
            "ITEMS".to_owned()
        )
    );
    let scheduled_items: Vec<String> = database
        .query_scalar(
            "SELECT item_id FROM metadata_reidentify_job_items WHERE job_id = ? ORDER BY item_id",
        )
        .bind(scheduled_job_id)
        .fetch_all(database.pool())
        .await
        .expect("scheduled item page");
    assert_eq!(scheduled_items, item_ids.clone());

    let queued_merge_fingerprint = b"queued-merge-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                &item_ids[0],
                "STILL",
                queued_merge_fingerprint,
            )
            .await
            .expect("prepare queued merge check")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                &item_ids[0],
                "STILL",
                queued_merge_fingerprint,
            )
            .await
            .expect("claim queued merge check")
    );
    let queued_merge_result = [NewItemMetadataCompletenessResult {
        item_id: &item_ids[0],
        capability: "STILL",
        input_fingerprint: queued_merge_fingerprint,
        is_missing: true,
        checked_at: 11,
    }];
    let merged = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &queued_merge_result,
            &[item_ids[0].clone()],
        )
        .await
        .expect("merge into queued fill-missing job");
    assert!(merged.scheduled_job_ids.is_empty());
    assert_eq!(
        database
            .query_scalar::<i64>("SELECT total_count FROM metadata_reidentify_jobs WHERE id = ?",)
            .bind(scheduled_job_id)
            .fetch_one(database.pool())
            .await
            .expect("read merged queued job count"),
        4
    );

    let replayed = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &backdrop_results, &item_ids)
        .await
        .expect("replay the already completed local result");
    assert_eq!(replayed.updated_count, 0);
    assert!(replayed.scheduled_job_ids.is_empty());

    let still_fingerprints = [
        b"still-v1".to_vec(),
        b"still-v2".to_vec(),
        b"still-v3".to_vec(),
        b"still-v4".to_vec(),
    ];
    let duplicate_results = item_ids
        .iter()
        .zip(&still_fingerprints)
        .map(|(item_id, fingerprint)| NewItemMetadataCompletenessResult {
            item_id,
            capability: "STILL",
            input_fingerprint: fingerprint,
            is_missing: true,
            checked_at: 12,
        })
        .collect::<Vec<_>>();
    for result in &duplicate_results {
        assert!(
            database
                .prepare_item_metadata_completeness_check(
                    result.item_id,
                    result.capability,
                    result.input_fingerprint,
                )
                .await
                .expect("prepare still")
        );
        assert!(
            database
                .claim_item_metadata_completeness_check(
                    result.item_id,
                    result.capability,
                    result.input_fingerprint,
                )
                .await
                .expect("claim still")
        );
    }
    let deduplicated = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &duplicate_results,
            &item_ids,
        )
        .await
        .expect("reuse active fill-missing jobs");
    assert_eq!(deduplicated.updated_count, 4);
    assert!(deduplicated.scheduled_job_ids.is_empty());
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'",
            )
            .fetch_one(database.pool())
            .await
            .expect("count fill-missing jobs"),
        2,
        "queued fill-missing jobs are coalesced per library"
    );

    let pagination_root_path = temp_dir.path().join("Pagination Movies");
    for index in 0..103 {
        let directory = format!("Pagination Movie {index:03} (2025)");
        let movie_dir = pagination_root_path.join(&directory);
        tokio::fs::create_dir_all(&movie_dir)
            .await
            .expect("pagination movie directory");
        tokio::fs::write(
            movie_dir.join(format!("Pagination.Movie.{index:03}.2025.mkv")),
            b"video",
        )
        .await
        .expect("pagination movie file");
    }
    let pagination_library = libraries
        .create_library("Progressive dispatch pagination", LibraryKind::Movie, false)
        .await
        .expect("pagination library");
    libraries
        .add_root(
            pagination_library.id,
            pagination_root_path.to_str().expect("pagination root"),
        )
        .await
        .expect("pagination library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(pagination_library.id)
        .await
        .expect("index pagination movies");
    let pagination_library_id = pagination_library.id.to_string();
    let pagination_item_ids: Vec<String> = database
        .query_scalar(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY id",
        )
        .bind(&pagination_library_id)
        .fetch_all(database.pool())
        .await
        .expect("read pagination movies");
    assert_eq!(pagination_item_ids.len(), 103);
    let unsupported_item_id = &pagination_item_ids[0];
    let removed_item_id = &pagination_item_ids[1];
    database
        .query("UPDATE media_items SET item_type = 'FOLDER' WHERE id = ?")
        .bind(unsupported_item_id)
        .execute(database.pool())
        .await
        .expect("mark one item type as unsupported for automatic metadata jobs");
    database
        .query("UPDATE media_items SET removed_at = 1 WHERE id = ?")
        .bind(removed_item_id)
        .execute(database.pool())
        .await
        .expect("mark one item removed before metadata completion");
    let pagination_fingerprint = b"pagination-input-v1";
    let pagination_checks = pagination_item_ids
        .iter()
        .map(|item_id| NewItemMetadataCompletenessCheck {
            item_id,
            capability: "POSTER",
            input_fingerprint: pagination_fingerprint,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&pagination_checks)
            .await
            .expect("claim pagination completeness checks")
            .len(),
        103
    );
    let pagination_results = pagination_item_ids
        .iter()
        .map(|item_id| NewItemMetadataCompletenessResult {
            item_id,
            capability: "POSTER",
            input_fingerprint: pagination_fingerprint,
            is_missing: true,
            checked_at: 13,
        })
        .collect::<Vec<_>>();
    let pagination_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &pagination_library_id,
            &pagination_results,
            &pagination_item_ids,
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("enqueue pagination jobs");
    assert_eq!(pagination_dispatch.updated_count, 102);
    assert_eq!(pagination_dispatch.scheduled_job_ids.len(), 2);
    let pagination_job_counts: Vec<i64> = database
        .query_scalar(
            "SELECT total_count FROM metadata_reidentify_jobs
             WHERE library_id = ? AND mode = 'FILL_MISSING' ORDER BY total_count DESC",
        )
        .bind(&pagination_library_id)
        .fetch_all(database.pool())
        .await
        .expect("read pagination job sizes");
    assert_eq!(pagination_job_counts, vec![100, 1]);
    let pagination_job_item_ids: Vec<String> = database
        .query_scalar(
            "SELECT job_items.item_id FROM metadata_reidentify_job_items job_items
             JOIN metadata_reidentify_jobs jobs ON jobs.id = job_items.job_id
             WHERE jobs.library_id = ? AND jobs.mode = 'FILL_MISSING'",
        )
        .bind(&pagination_library_id)
        .fetch_all(database.pool())
        .await
        .expect("read paginated job items");
    assert_eq!(pagination_job_item_ids.len(), 101);
    assert!(!pagination_job_item_ids.contains(unsupported_item_id));
    assert!(!pagination_job_item_ids.contains(removed_item_id));
    let removed_completeness = database
        .find_item_metadata_completeness(removed_item_id, "POSTER")
        .await
        .expect("read removed item completeness")
        .expect("removed item completeness row");
    assert_eq!(removed_completeness.local_state, "RUNNING");

    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET error = 'METADATA_WRITE_FAILED'
         WHERE item_id = ? AND status = 'FAILED'
           AND job_id IN (
               SELECT id FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'
           )",
    )
    .bind(&item_ids[3])
    .execute(database.pool())
    .await
    .expect("set a non-provider deferred error");
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'COMPLETED'
         WHERE item_id = ? AND status = 'PENDING'
           AND job_id IN (
               SELECT id FROM metadata_reidentify_jobs
               WHERE mode = 'FILL_MISSING' AND status = 'QUEUED'
           )",
    )
    .bind(&item_ids[3])
    .execute(database.pool())
    .await
    .expect("finish the changed snapshot job before testing deferred retry");
    sqlx::query(
        "UPDATE metadata_reidentify_jobs
         SET status = 'COMPLETED', processed_count = total_count
         WHERE mode = 'FILL_MISSING' AND status = 'QUEUED' AND id IN (
             SELECT job_id FROM metadata_reidentify_job_items WHERE item_id = ?
         )",
    )
    .bind(&item_ids[3])
    .execute(database.pool())
    .await
    .expect("close the queued snapshot job before testing deferred retry");
    let retried_non_provider_failure = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &[],
            std::slice::from_ref(&item_ids[3]),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("allow retry after a non-provider failure");
    assert_eq!(retried_non_provider_failure.scheduled_job_ids.len(), 1);

    sqlx::query(
        "UPDATE metadata_reidentify_jobs
         SET status = 'DEFERRED', updated_at = unixepoch() - 3601
         WHERE id IN (
             SELECT job_id FROM metadata_reidentify_job_items WHERE item_id = ?
         ) AND mode = 'FILL_MISSING'",
    )
    .bind(&item_ids[3])
    .execute(database.pool())
    .await
    .expect("age prior deferred fill-missing jobs");
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'FAILED', error = 'SCRAPER_UNAVAILABLE',
             automatic_retry_after = unixepoch() - 1
         WHERE item_id = ? AND job_id IN (
             SELECT id FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'
         )",
    )
    .bind(&item_ids[3])
    .execute(database.pool())
    .await
    .expect("restore provider failure after deferral window");
    let retried_expired_provider_failure = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &[],
            std::slice::from_ref(&item_ids[3]),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("allow retry after deferred deduplication expires");
    assert_eq!(retried_expired_provider_failure.scheduled_job_ids.len(), 1);

    database.close().await;
}

#[tokio::test]
async fn changed_fill_request_is_not_deduplicated_by_recent_provider_deferral()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Deferred snapshot", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    sqlx::query("UPDATE libraries SET scan_missing_metadata_auto_match_enabled = 1 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    let item_id = "deferred-snapshot-item";
    database
        .query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .execute(database.pool())
        .await?;

    let first_fingerprint = b"deferred-input-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(item_id, "POSTER", first_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(item_id, "POSTER", first_fingerprint)
            .await?
    );
    let first_result = [NewItemMetadataCompletenessResult {
        item_id,
        capability: "POSTER",
        input_fingerprint: first_fingerprint,
        is_missing: true,
        checked_at: 10,
    }];
    let first = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &first_result,
            &[item_id.into()],
        )
        .await?;
    let first_job_id = &first.scheduled_job_ids[0];
    sqlx::query(
        "UPDATE metadata_reidentify_jobs SET status = 'DEFERRED', updated_at = unixepoch()
         WHERE id = ?",
    )
    .bind(first_job_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE metadata_reidentify_job_items SET status = 'FAILED', error = 'SCRAPER_UNAVAILABLE'
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(first_job_id)
    .bind(item_id)
    .execute(database.pool())
    .await?;
    let same_snapshot = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &[], &[item_id.into()])
        .await?;
    assert!(same_snapshot.scheduled_job_ids.is_empty());

    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET automatic_retry_after = unixepoch() - 1
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(first_job_id)
    .bind(item_id)
    .execute(database.pool())
    .await?;

    let changed_fingerprint = b"deferred-input-v2";
    assert!(
        database
            .prepare_item_metadata_completeness_check(item_id, "POSTER", changed_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(item_id, "POSTER", changed_fingerprint)
            .await?
    );
    let changed_result = [NewItemMetadataCompletenessResult {
        item_id,
        capability: "POSTER",
        input_fingerprint: changed_fingerprint,
        is_missing: true,
        checked_at: 11,
    }];
    let changed = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &changed_result,
            &[item_id.into()],
        )
        .await?;
    assert_eq!(changed.scheduled_job_ids.len(), 1);
    assert_ne!(changed.scheduled_job_ids[0], *first_job_id);

    let changed_job_id = &changed.scheduled_job_ids[0];
    sqlx::query(
        "UPDATE metadata_reidentify_job_items SET status = 'COMPLETED'
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(changed_job_id)
    .bind(item_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE metadata_reidentify_jobs
         SET status = 'COMPLETED', processed_count = total_count
         WHERE id = ?",
    )
    .bind(changed_job_id)
    .execute(database.pool())
    .await?;

    let old_retry_consumed: i64 = database
        .query_scalar(
            "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(first_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        old_retry_consumed, 1,
        "the newer fingerprint supersedes the old provider failure"
    );
    let (_, due_retries) = database
        .prepare_and_claim_item_metadata_completeness_checks_with_due_fill_missing_retries(
            &[NewItemMetadataCompletenessCheck {
                item_id,
                capability: "POSTER",
                input_fingerprint: changed_fingerprint,
            }],
            &library_id,
            &[item_id.to_owned()],
        )
        .await?;
    assert!(
        due_retries.is_empty(),
        "an obsolete fingerprint must not keep unlocking the completed newer request"
    );
    Ok(())
}

async fn fail_fill_missing_job_with_unavailable_provider(
    database: &Database,
    job_id: &str,
    item_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    assert!(database.claim_metadata_reidentify_job(job_id).await?);
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(job_id, 1)
            .await?,
        vec![item_id.to_owned()]
    );
    database
        .finish_metadata_reidentify_item(job_id, item_id, "FAILED", 0, Some("SCRAPER_UNAVAILABLE"))
        .await?;
    database
        .finish_metadata_reidentify_job(job_id, "DEFERRED", Some("DEFERRED_PROVIDER_UNAVAILABLE"))
        .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_changed_fill_request_consumes_obsolete_provider_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration = DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let connection = PostgresConnection {
        database: database_name.clone(),
        ..admin_connection
    };
    let database =
        Database::connect_with_configuration(&config, &DatabaseConfiguration::Postgres(connection))
            .await?;

    let assertions = async {
        let library = LibraryService::new(database.clone())
            .create_library("Postgres obsolete failure", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        database
            .query("UPDATE libraries SET scan_missing_metadata_auto_match_enabled = 1 WHERE id = ?")
            .bind(&library_id)
            .execute(database.pool())
            .await?;
        let item_id = "postgres-obsolete-fill-failure";
        database
            .query(
                "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(&library_id)
            .execute(database.pool())
            .await?;

        let old_fingerprint = b"postgres-obsolete-input-v1";
        assert!(
            database
                .prepare_item_metadata_completeness_check(item_id, "POSTER", old_fingerprint)
                .await?
        );
        assert!(
            database
                .claim_item_metadata_completeness_check(item_id, "POSTER", old_fingerprint)
                .await?
        );
        let old_result = [NewItemMetadataCompletenessResult {
            item_id,
            capability: "POSTER",
            input_fingerprint: old_fingerprint,
            is_missing: true,
            checked_at: 10,
        }];
        let old_dispatch = database
            .complete_local_metadata_and_enqueue_fill_missing(
                &library_id,
                &old_result,
                &[item_id.into()],
            )
            .await?;
        let old_job_id = old_dispatch
            .scheduled_job_ids
            .first()
            .ok_or("expected old fill-missing job")?
            .clone();
        fail_fill_missing_job_with_unavailable_provider(&database, &old_job_id, item_id).await?;
        database
            .query(
                "UPDATE metadata_reidentify_job_items
                 SET automatic_retry_after = unixepoch() - 1
                 WHERE job_id = ? AND item_id = ?",
            )
            .bind(&old_job_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;

        let new_fingerprint = b"postgres-obsolete-input-v2";
        assert!(
            database
                .prepare_item_metadata_completeness_check(item_id, "POSTER", new_fingerprint)
                .await?
        );
        assert!(
            database
                .claim_item_metadata_completeness_check(item_id, "POSTER", new_fingerprint)
                .await?
        );
        let new_result = [NewItemMetadataCompletenessResult {
            item_id,
            capability: "POSTER",
            input_fingerprint: new_fingerprint,
            is_missing: true,
            checked_at: 11,
        }];
        let new_dispatch = database
            .complete_local_metadata_and_enqueue_fill_missing(
                &library_id,
                &new_result,
                &[item_id.into()],
            )
            .await?;
        let new_job_id = new_dispatch
            .scheduled_job_ids
            .first()
            .ok_or("expected new fill-missing job")?
            .clone();
        assert_ne!(new_job_id, old_job_id);
        assert_eq!(
            database
                .query_scalar::<i64>(
                    "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
                     WHERE job_id = ? AND item_id = ?",
                )
                .bind(&old_job_id)
                .bind(item_id)
                .fetch_one(database.pool())
                .await?,
            1,
            "the newer fingerprint supersedes the old provider failure"
        );

        database
            .query(
                "UPDATE metadata_reidentify_job_items SET status = 'COMPLETED'
                 WHERE job_id = ? AND item_id = ?",
            )
            .bind(&new_job_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
        database
            .query(
                "UPDATE metadata_reidentify_jobs
                 SET status = 'COMPLETED', processed_count = total_count
                 WHERE id = ?",
            )
            .bind(&new_job_id)
            .execute(database.pool())
            .await?;

        let (_, due_retries) = database
            .prepare_and_claim_item_metadata_completeness_checks_with_due_fill_missing_retries(
                &[NewItemMetadataCompletenessCheck {
                    item_id,
                    capability: "POSTER",
                    input_fingerprint: new_fingerprint,
                }],
                &library_id,
                &[item_id.to_owned()],
            )
            .await?;
        assert!(
            due_retries.is_empty(),
            "an obsolete fingerprint must not unlock the completed newer request"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    assertions?;
    drop_database?;
    Ok(())
}

#[tokio::test]
async fn automatic_fill_missing_provider_retry_backoff_is_capped_and_snapshot_scoped()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Fill backoff", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    sqlx::query("UPDATE libraries SET scan_missing_metadata_auto_match_enabled = 1 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    let item_id = "fill-backoff-item";
    database
        .query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .execute(database.pool())
        .await?;

    let first_fingerprint = b"fill-backoff-input-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(item_id, "POSTER", first_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(item_id, "POSTER", first_fingerprint)
            .await?
    );
    let first_result = [NewItemMetadataCompletenessResult {
        item_id,
        capability: "POSTER",
        input_fingerprint: first_fingerprint,
        is_missing: true,
        checked_at: 10,
    }];
    let first_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &first_result,
            &[item_id.to_owned()],
        )
        .await?;
    let first_job_id = first_dispatch.scheduled_job_ids[0].clone();
    fail_fill_missing_job_with_unavailable_provider(&database, &first_job_id, item_id).await?;

    let first_backoff: (i64, i64) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_after - unixepoch()
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&first_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(first_backoff.0, 1);
    assert!((299..=300).contains(&first_backoff.1));

    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET automatic_retry_count = 0, automatic_retry_after = NULL
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(&first_job_id)
    .bind(item_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE server_settings SET value = CAST(unixepoch() + 300 AS TEXT)
         WHERE key = 'metadata_fill_missing_legacy_retry_after'",
    )
    .execute(database.pool())
    .await?;
    let suppressed = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &[], &[item_id.to_owned()])
        .await?;
    assert!(suppressed.scheduled_job_ids.is_empty());
    sqlx::query(
        "UPDATE server_settings SET value = CAST(unixepoch() - 1 AS TEXT)
         WHERE key = 'metadata_fill_missing_legacy_retry_after'",
    )
    .execute(database.pool())
    .await?;
    let due_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &[], &[item_id.to_owned()])
        .await?;
    assert_eq!(due_dispatch.scheduled_job_ids.len(), 1);
    let second_job_id = due_dispatch.scheduled_job_ids[0].clone();
    assert_ne!(second_job_id, first_job_id);
    let inherited_count: i64 = database
        .query_scalar(
            "SELECT automatic_retry_count FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(&second_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(inherited_count, 1);

    fail_fill_missing_job_with_unavailable_provider(&database, &second_job_id, item_id).await?;
    let second_backoff: (i64, i64) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_after - unixepoch()
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&second_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(second_backoff.0, 2);
    assert!((1_799..=1_800).contains(&second_backoff.1));

    assert!(
        database
            .retry_metadata_reidentify_job(&second_job_id)
            .await?
    );
    let manual_retry_state: (String, Option<i64>) = database
        .query_as(
            "SELECT items.status, items.automatic_retry_after
             FROM metadata_reidentify_job_items items
             WHERE items.job_id = ? AND items.item_id = ?",
        )
        .bind(&second_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(manual_retry_state, ("PENDING".to_owned(), None));
    fail_fill_missing_job_with_unavailable_provider(&database, &second_job_id, item_id).await?;
    let capped_backoff: (i64, i64) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_after - unixepoch()
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&second_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(capped_backoff.0, 3);
    assert!((21_599..=21_600).contains(&capped_backoff.1));

    let changed_fingerprint = b"fill-backoff-input-v2";
    assert!(
        database
            .prepare_item_metadata_completeness_check(item_id, "POSTER", changed_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(item_id, "POSTER", changed_fingerprint)
            .await?
    );
    let changed_result = [NewItemMetadataCompletenessResult {
        item_id,
        capability: "POSTER",
        input_fingerprint: changed_fingerprint,
        is_missing: true,
        checked_at: 11,
    }];
    let changed_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &changed_result,
            &[item_id.to_owned()],
        )
        .await?;
    assert_eq!(changed_dispatch.scheduled_job_ids.len(), 1);
    let changed_job_id = &changed_dispatch.scheduled_job_ids[0];
    let changed_retry_state: (i64, Option<i64>) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_after
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(changed_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(changed_retry_state, (0, None));

    fail_fill_missing_job_with_unavailable_provider(&database, changed_job_id, item_id).await?;
    let changed_capability_request = [crate::storage::MetadataFillMissingRequest {
        item_id: item_id.to_owned(),
        input_fingerprint: Some(changed_fingerprint.to_vec()),
        capabilities_json: "[\"BACKDROP\"]".to_owned(),
        automatic_retry_count: 0,
    }];
    let mut transaction = database.begin_metadata_write_transaction().await?;
    let changed_capability_job_ids = database
        .enqueue_or_update_fill_missing_requests_in_transaction(
            &mut transaction,
            &library_id,
            &changed_capability_request,
        )
        .await?;
    transaction.commit().await?;
    assert_eq!(changed_capability_job_ids.len(), 1);
    let changed_capability_retry_state: (i64, Option<i64>, String) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_after, request_capabilities_json
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&changed_capability_job_ids[0])
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        changed_capability_retry_state,
        (0, None, "[\"BACKDROP\"]".to_owned())
    );

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn automatic_fill_missing_retry_backoff_is_consumed_after_a_successful_retry()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Retry consumption", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    sqlx::query("UPDATE libraries SET scan_missing_metadata_auto_match_enabled = 1 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    let item_id = "retry-consumption-item";
    database
        .query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .execute(database.pool())
        .await?;

    let fingerprint = b"retry-consumption-input";
    assert!(
        database
            .prepare_item_metadata_completeness_check(item_id, "POSTER", fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(item_id, "POSTER", fingerprint)
            .await?
    );
    let result = [NewItemMetadataCompletenessResult {
        item_id,
        capability: "POSTER",
        input_fingerprint: fingerprint,
        is_missing: true,
        checked_at: 10,
    }];
    let first_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &result,
            &[item_id.to_owned()],
        )
        .await?;
    let first_job_id = first_dispatch.scheduled_job_ids[0].clone();
    fail_fill_missing_job_with_unavailable_provider(&database, &first_job_id, item_id).await?;
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET automatic_retry_after = unixepoch() - 1
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(&first_job_id)
    .bind(item_id)
    .execute(database.pool())
    .await?;

    let retry_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &[], &[item_id.to_owned()])
        .await?;
    assert_eq!(retry_dispatch.scheduled_job_ids.len(), 1);
    let retry_job_id = retry_dispatch.scheduled_job_ids[0].clone();
    let consumed_legacy_retry: i64 = database
        .query_scalar(
            "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(&first_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(consumed_legacy_retry, 1);
    assert!(
        database
            .claim_metadata_reidentify_job(&retry_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(&retry_job_id, 1)
            .await?,
        vec![item_id.to_owned()]
    );
    database
        .finish_metadata_reidentify_item(&retry_job_id, item_id, "COMPLETED", 1, None)
        .await?;
    database
        .finish_metadata_reidentify_job(&retry_job_id, "COMPLETED", None)
        .await?;

    let retry_check = [NewItemMetadataCompletenessCheck {
        item_id,
        capability: "POSTER",
        input_fingerprint: fingerprint,
    }];
    let (claimed_indices, due_retry_item_ids) = database
        .prepare_and_claim_item_metadata_completeness_checks_with_due_fill_missing_retries(
            &retry_check,
            &library_id,
            &[item_id.to_owned()],
        )
        .await?;
    assert!(claimed_indices.is_empty());
    assert!(
        due_retry_item_ids.is_empty(),
        "a historical expired failure must not trigger another automatic retry"
    );

    assert!(
        database
            .retry_metadata_reidentify_job(&first_job_id)
            .await?
    );
    assert!(
        database
            .claim_metadata_reidentify_job(&first_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(&first_job_id, 1)
            .await?,
        vec![item_id.to_owned()]
    );
    database
        .finish_metadata_reidentify_item(
            &first_job_id,
            item_id,
            "FAILED",
            0,
            Some("SCRAPER_UNAVAILABLE"),
        )
        .await?;
    database
        .finish_metadata_reidentify_job(
            &first_job_id,
            "DEFERRED",
            Some("DEFERRED_PROVIDER_UNAVAILABLE"),
        )
        .await?;
    let manual_retry_backoff: (i64, Option<i64>) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_consumed
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&first_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(manual_retry_backoff, (2, Some(0)));

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn automatic_fill_missing_retry_backoff_excludes_manual_modes_and_cancelled_failures()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Fill backoff scope", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let item_ids = [
        "fill-backoff-manual",
        "fill-backoff-reidentify",
        "fill-backoff-full-refresh",
        "fill-backoff-cancelled",
    ];
    for item_id in item_ids {
        database
            .query(
                "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
    }

    let manual_job_id = database
        .create_or_merge_fill_missing_job(&library_id, &[item_ids[0].to_owned()])
        .await?;
    fail_fill_missing_job_with_unavailable_provider(&database, &manual_job_id, item_ids[0]).await?;
    for (job_id, item_id, mode) in [
        ("retry-reidentify", item_ids[1], "REIDENTIFY"),
        ("retry-full-refresh", item_ids[2], "FULL_REFRESH"),
    ] {
        database
            .create_metadata_reidentify_job(job_id, &[item_id.to_owned()], mode)
            .await?;
        fail_fill_missing_job_with_unavailable_provider(&database, job_id, item_id).await?;
    }

    let cancelled_item = item_ids[3];
    let fingerprint = b"cancelled-fill-backoff-input";
    assert!(
        database
            .prepare_item_metadata_completeness_check(cancelled_item, "POSTER", fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(cancelled_item, "POSTER", fingerprint)
            .await?
    );
    let result = [NewItemMetadataCompletenessResult {
        item_id: cancelled_item,
        capability: "POSTER",
        input_fingerprint: fingerprint,
        is_missing: true,
        checked_at: 20,
    }];
    let dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &result,
            &[cancelled_item.to_owned()],
        )
        .await?;
    let cancelled_job_id = &dispatch.scheduled_job_ids[0];
    assert!(
        database
            .claim_metadata_reidentify_job(cancelled_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(cancelled_job_id, 1)
            .await?,
        vec![cancelled_item.to_owned()]
    );
    assert!(
        database
            .request_metadata_reidentify_job_cancel(cancelled_job_id)
            .await?
    );
    database
        .finish_metadata_reidentify_item(
            cancelled_job_id,
            cancelled_item,
            "FAILED",
            0,
            Some("SCRAPER_UNAVAILABLE"),
        )
        .await?;
    database
        .finish_metadata_reidentify_job(
            cancelled_job_id,
            "DEFERRED",
            Some("DEFERRED_PROVIDER_UNAVAILABLE"),
        )
        .await?;

    let retry_states: Vec<(String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT items.item_id, items.automatic_retry_count, items.automatic_retry_after
         FROM metadata_reidentify_job_items items
         WHERE items.job_id IN (?, ?, ?, ?)
         ORDER BY items.item_id",
    )
    .bind(&manual_job_id)
    .bind("retry-reidentify")
    .bind("retry-full-refresh")
    .bind(cancelled_job_id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(retry_states.len(), 4);
    for (_, retry_count, retry_after) in retry_states {
        assert_eq!(retry_count, 0);
        assert_eq!(retry_after, None);
    }

    database.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_progressive_scan_metadata_storage_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration = DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let connection = PostgresConnection {
        database: database_name.clone(),
        ..admin_connection
    };
    let raw_database_url = DatabaseConfiguration::Postgres(connection.clone())
        .postgres_url()?
        .ok_or("missing PostgreSQL test URL")?;
    let database =
        Database::connect_with_configuration(&config, &DatabaseConfiguration::Postgres(connection))
            .await?;
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Postgres Movie (2025)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Postgres.Movie.2025.mkv"), b"video").await?;
    tokio::fs::write(media_root.join("Second.Postgres.Movie.2026.mkv"), b"video").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Postgres progressive", LibraryKind::Movie, false)
        .await?;
    let root = libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?;
    let root_id = root.root.id.to_string();
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_ids: Vec<String> = database
        .query_scalar(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY id",
        )
        .bind(library.id.to_string())
        .fetch_all(database.pool())
        .await?;
    assert_eq!(item_ids.len(), 2);
    let item_id = item_ids[0].clone();
    let replay_item_id = item_ids[1].clone();
    let scan_job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await?;
    let scan_job_id = scan_job.id;
    let manifest_id: String = sqlx::query_scalar("SELECT id FROM scan_manifests WHERE job_id = $1")
        .bind(&scan_job_id)
        .fetch_one(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_manifest_roots (
                 manifest_id, library_root_id, state, postprocessing_target_stage
             ) VALUES (?, ?, 'COMPLETE', 'NEW')
             ON CONFLICT(manifest_id, library_root_id) DO UPDATE SET
                 state = 'COMPLETE', postprocessing_target_stage = 'NEW'",
        )
        .bind(&manifest_id)
        .bind(&root_id)
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_job_targets (
                 job_id, target_type, target_id, item_id, change_kind, metadata_state
             ) VALUES (?, 'ITEM', ?, ?, 'NEW', 'PENDING')
             ON CONFLICT(job_id, target_type, target_id) DO UPDATE SET metadata_state = 'PENDING'",
        )
        .bind(&scan_job_id)
        .bind(&item_id)
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    database.reset_query_count();
    let metadata_page = database
        .load_scan_job_metadata_page(&scan_job_id, 8)
        .await?;
    assert!(metadata_page.has_pending);
    assert!(matches!(
        metadata_page.sources,
        StoredScanJobMetadataSources::Movies(ref sources)
            if sources.iter().any(|source| source.item_id == item_id)
    ));
    assert_eq!(database.query_count(), 1);
    database.reset_query_count();
    let manifest_state = database
        .get_scan_manifest_postprocessing_state_by_job(&scan_job_id)
        .await?
        .ok_or("PostgreSQL scan manifest should be present")?;
    assert!(!manifest_state.roots.is_empty());
    assert_eq!(database.query_count(), 1);

    let combined_fingerprint = b"postgres-combined-completeness-v1";
    let combined_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "COMBINED_CHECK",
        input_fingerprint: combined_fingerprint,
        is_missing: false,
        checked_at: 999,
    }];
    assert_eq!(
        database
            .complete_local_metadata_completeness_batch_with_policy(
                &library.id.to_string(),
                &combined_result,
                &[],
                MetadataAutoMatchPolicy::Disabled,
            )
            .await?
            .updated_count,
        1
    );
    assert_eq!(
        database
            .complete_local_metadata_completeness_batch_with_policy(
                &library.id.to_string(),
                &combined_result,
                &[],
                MetadataAutoMatchPolicy::Disabled,
            )
            .await?
            .updated_count,
        0,
        "the same PostgreSQL fingerprint should be complete only once"
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'",
            )
            .fetch_one(database.pool())
            .await?,
        0,
        "a non-missing combined result must not create a fill-missing job"
    );

    let completeness_fingerprint_v1 = b"postgres-batch-input-v1";
    let completeness_checks = [
        NewItemMetadataCompletenessCheck {
            item_id: &item_id,
            capability: "POSTER",
            input_fingerprint: completeness_fingerprint_v1,
        },
        NewItemMetadataCompletenessCheck {
            item_id: &item_id,
            capability: "METADATA",
            input_fingerprint: completeness_fingerprint_v1,
        },
    ];
    let (check_a, check_b) = tokio::join!(
        database.prepare_and_claim_item_metadata_completeness_checks(&completeness_checks),
        database.prepare_and_claim_item_metadata_completeness_checks(&completeness_checks),
    );
    let check_a = check_a?;
    let check_b = check_b?;
    assert_eq!(check_a.len() + check_b.len(), 2);
    assert!(check_a.is_empty() || check_b.is_empty());

    const DEADLOCK_TEST_ADVISORY_KEY: i64 = 913_579_246_813_579;
    let raw_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(&raw_database_url)
        .await?;
    sqlx::query(
        r#"CREATE FUNCTION test_completeness_lock_order_trigger() RETURNS trigger
           LANGUAGE plpgsql AS $$
           BEGIN
               IF NEW.local_state = 'PENDING' AND NEW.capability = 'DEADLOCK_TEST' THEN
                   PERFORM pg_advisory_xact_lock(913579246813579);
                   PERFORM 1 FROM media_items WHERE id = NEW.item_id FOR KEY SHARE;
               END IF;
               RETURN NEW;
           END;
           $$"#,
    )
    .execute(&raw_pool)
    .await?;
    sqlx::query(
        "CREATE TRIGGER test_completeness_lock_order
         BEFORE UPDATE ON item_metadata_completeness
         FOR EACH ROW EXECUTE FUNCTION test_completeness_lock_order_trigger()",
    )
    .execute(&raw_pool)
    .await?;
    sqlx::query(
        "INSERT INTO item_metadata_completeness
             (item_id, capability, local_state, is_missing, input_fingerprint)
         VALUES ($1, 'DEADLOCK_TEST', 'RUNNING', NULL, $2)",
    )
    .bind(&replay_item_id)
    .bind(b"old".as_slice())
    .execute(&raw_pool)
    .await?;

    let mut advisory_connection = raw_pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(DEADLOCK_TEST_ADVISORY_KEY)
        .execute(&mut *advisory_connection)
        .await?;
    let mut parent_transaction = raw_pool.begin().await?;
    sqlx::query("SELECT id FROM media_items WHERE id = $1 FOR UPDATE")
        .bind(&replay_item_id)
        .fetch_one(&mut *parent_transaction)
        .await?;

    let completion_database = database.clone();
    let completion_library_id = library.id.to_string();
    let completion_item_id = replay_item_id.clone();
    let completion_task = tokio::spawn(async move {
        let results = [NewItemMetadataCompletenessResult {
            item_id: &completion_item_id,
            capability: "DEADLOCK_TEST",
            input_fingerprint: b"old",
            is_missing: false,
            checked_at: 2_000,
        }];
        completion_database
            .complete_local_metadata_and_enqueue_fill_missing_with_policy(
                &completion_library_id,
                &results,
                &[],
                MetadataAutoMatchPolicy::Disabled,
            )
            .await
    });
    let mut completion_waiting_for_parent = false;
    for _ in 0..100 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_stat_activity
             WHERE datname = current_database()
               AND wait_event_type = 'Lock'
               AND query LIKE '%SELECT id FROM media_items%'",
        )
        .fetch_one(&raw_pool)
        .await?;
        if waiting > 0 {
            completion_waiting_for_parent = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        completion_waiting_for_parent,
        "completion transaction did not reach the parent-row lock"
    );

    let claim_database = database.clone();
    let claim_item_id = replay_item_id.clone();
    let claim_task = tokio::spawn(async move {
        let checks = [NewItemMetadataCompletenessCheck {
            item_id: &claim_item_id,
            capability: "DEADLOCK_TEST",
            input_fingerprint: b"new",
        }];
        claim_database
            .prepare_and_claim_item_metadata_completeness_checks(&checks)
            .await
    });
    parent_transaction.commit().await?;

    let mut claim_waiting_for_advisory = false;
    for _ in 0..300 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_locks
             WHERE locktype = 'advisory' AND granted = false",
        )
        .fetch_one(&raw_pool)
        .await?;
        if waiting > 0 {
            claim_waiting_for_advisory = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        claim_waiting_for_advisory,
        "claim transaction did not reach the controlled parent-lock point"
    );
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(DEADLOCK_TEST_ADVISORY_KEY)
        .execute(&mut *advisory_connection)
        .await?;
    let (claim_result, completion_result) =
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(claim_task, completion_task)
        })
        .await?;
    let claim_result = claim_result.map_err(|error| std::io::Error::other(error.to_string()))??;
    let completion_result =
        completion_result.map_err(|error| std::io::Error::other(error.to_string()))??;
    assert_eq!(claim_result, vec![0]);
    assert_eq!(completion_result.updated_count, 1);
    sqlx::query(
        "DELETE FROM item_metadata_completeness
         WHERE item_id = $1 AND capability = 'DEADLOCK_TEST'",
    )
    .bind(&replay_item_id)
    .execute(&raw_pool)
    .await?;
    drop(advisory_connection);
    raw_pool.close().await;

    let completeness_results = [
        NewItemMetadataCompletenessResult {
            item_id: &item_id,
            capability: "POSTER",
            input_fingerprint: completeness_fingerprint_v1,
            is_missing: true,
            checked_at: 1_000,
        },
        NewItemMetadataCompletenessResult {
            item_id: &item_id,
            capability: "METADATA",
            input_fingerprint: completeness_fingerprint_v1,
            is_missing: false,
            checked_at: 1_000,
        },
    ];
    assert_eq!(
        database
            .complete_local_metadata_and_enqueue_fill_missing(
                &library.id.to_string(),
                &completeness_results,
                &[],
            )
            .await?
            .updated_count,
        2
    );
    assert!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&completeness_checks)
            .await?
            .is_empty()
    );

    let library_id = library.id.to_string();
    database
        .query("UPDATE libraries SET scan_missing_metadata_auto_match_enabled = 0 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    let replay_fingerprint = b"postgres-replay-without-new-result";
    assert!(database
        .prepare_item_metadata_completeness_check(
            &replay_item_id,
            "POSTER",
            replay_fingerprint,
        )
        .await?);
    assert!(
        database
            .claim_item_metadata_completeness_check(&replay_item_id, "POSTER", replay_fingerprint,)
            .await?
    );
    let replay_result = [NewItemMetadataCompletenessResult {
        item_id: &replay_item_id,
        capability: "POSTER",
        input_fingerprint: replay_fingerprint,
        is_missing: true,
        checked_at: 1_001,
    }];
    let policy_disabled = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &replay_result,
            std::slice::from_ref(&replay_item_id),
            MetadataAutoMatchPolicy::Disabled,
        )
        .await?;
    assert_eq!(policy_disabled.updated_count, 1);
    assert!(policy_disabled.scheduled_job_ids.is_empty());
    let default_policy_replay = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&replay_item_id),
        )
        .await?;
    assert!(default_policy_replay.scheduled_job_ids.is_empty());
    let enabled_policy_replay = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &[],
            std::slice::from_ref(&replay_item_id),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await?;
    assert_eq!(enabled_policy_replay.updated_count, 0);
    assert_eq!(enabled_policy_replay.scheduled_job_ids.len(), 1);
    let duplicate_policy_replay = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &[],
            std::slice::from_ref(&replay_item_id),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await?;
    assert!(duplicate_policy_replay.scheduled_job_ids.is_empty());
    let existing_fill_missing_job_id = enabled_policy_replay
        .scheduled_job_ids
        .first()
        .ok_or("enabled policy replay did not schedule a fill-missing job")?;
    assert!(
        database
            .claim_metadata_reidentify_job(existing_fill_missing_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(existing_fill_missing_job_id, 1)
            .await?,
        vec![replay_item_id.clone()]
    );
    database
        .finish_metadata_reidentify_item(
            existing_fill_missing_job_id,
            &replay_item_id,
            "COMPLETED",
            1,
            None,
        )
        .await?;
    database
        .finish_metadata_reidentify_job(existing_fill_missing_job_id, "COMPLETED", None)
        .await?;
    database
        .query("UPDATE libraries SET scan_missing_metadata_auto_match_enabled = 1 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;

    let failed_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&replay_item_id),
        )
        .await?;
    let failed_job_id = failed_dispatch
        .scheduled_job_ids
        .first()
        .ok_or("PostgreSQL provider-unavailable job was not scheduled")?
        .clone();
    fail_fill_missing_job_with_unavailable_provider(&database, &failed_job_id, &replay_item_id)
        .await?;
    database
        .query(
            "UPDATE metadata_reidentify_job_items
             SET automatic_retry_after = unixepoch() - 1
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(&failed_job_id)
        .bind(&replay_item_id)
        .execute(database.pool())
        .await?;
    let retry_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&replay_item_id),
        )
        .await?;
    let retry_job_id = retry_dispatch
        .scheduled_job_ids
        .first()
        .ok_or("PostgreSQL due retry was not scheduled")?
        .clone();
    let consumed_retry: i64 = database
        .query_scalar(
            "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(&failed_job_id)
        .bind(&replay_item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(consumed_retry, 1);
    assert!(
        database
            .claim_metadata_reidentify_job(&retry_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(&retry_job_id, 1)
            .await?,
        vec![replay_item_id.clone()]
    );
    database
        .finish_metadata_reidentify_item(&retry_job_id, &replay_item_id, "COMPLETED", 1, None)
        .await?;
    database
        .finish_metadata_reidentify_job(&retry_job_id, "COMPLETED", None)
        .await?;
    let retry_check = [NewItemMetadataCompletenessCheck {
        item_id: &replay_item_id,
        capability: "POSTER",
        input_fingerprint: replay_fingerprint,
    }];
    let (claimed_retry, due_retry_item_ids) = database
        .prepare_and_claim_item_metadata_completeness_checks_with_due_fill_missing_retries(
            &retry_check,
            &library_id,
            std::slice::from_ref(&replay_item_id),
        )
        .await?;
    assert!(claimed_retry.is_empty());
    assert!(due_retry_item_ids.is_empty());

    let completeness_fingerprint_v2 = b"postgres-batch-input-v2";
    let replacement_check = [NewItemMetadataCompletenessCheck {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: completeness_fingerprint_v2,
    }];
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&replacement_check)
            .await?,
        vec![0]
    );
    assert!(
        !database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                completeness_fingerprint_v1,
                true,
                1_001,
            )
            .await?
    );
    let retry_fingerprint = b"postgres-batch-retry";
    let retry_check = [NewItemMetadataCompletenessCheck {
        item_id: &item_id,
        capability: "BACKDROP",
        input_fingerprint: retry_fingerprint,
    }];
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&retry_check)
            .await?,
        vec![0]
    );
    assert!(
        database
            .fail_item_metadata_completeness_check(
                &item_id,
                "BACKDROP",
                retry_fingerprint,
                Some(2_000),
                "temporary local error",
            )
            .await?
    );
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&retry_check)
            .await?,
        vec![0]
    );
    let retry_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "BACKDROP",
        input_fingerprint: retry_fingerprint,
        is_missing: false,
        checked_at: 2_001,
    }];
    assert_eq!(
        database
            .complete_local_metadata_and_enqueue_fill_missing(
                &library.id.to_string(),
                &retry_result,
                &[],
            )
            .await?
            .updated_count,
        1
    );

    assert_eq!(
        database.ensure_scan_local_metadata_backfill_roots().await?,
        1
    );
    let (claim_a, claim_b) = tokio::join!(
        database.claim_next_scan_local_metadata_backfill_page(1),
        database.claim_next_scan_local_metadata_backfill_page(1),
    );
    let mut claimed_pages = [claim_a?, claim_b?].into_iter().flatten();
    let backfill_page = claimed_pages
        .next()
        .ok_or("PostgreSQL backfill page was not claimable")?;
    assert!(
        claimed_pages.next().is_none(),
        "concurrent PostgreSQL claimers cannot claim the same root page twice"
    );
    assert_eq!(backfill_page.library_root_id, root_id);
    assert_eq!(backfill_page.entry_ids.len(), 1);
    assert!(backfill_page.has_more);
    assert!(
        database
            .fail_scan_local_metadata_backfill_page(
                &backfill_page,
                "temporary local failure",
                Some(i64::MAX),
            )
            .await?
    );
    assert!(
        database
            .claim_next_scan_local_metadata_backfill_page(1)
            .await?
            .is_none()
    );
    database
        .query(
            "UPDATE scan_local_metadata_backfills SET next_attempt_at = 0
             WHERE library_root_id = ?",
        )
        .bind(&root_id)
        .execute(database.pool())
        .await?;
    let retry_page = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await?
        .ok_or("failed PostgreSQL page did not retry")?;
    assert_eq!(retry_page.entry_ids, backfill_page.entry_ids);
    assert_eq!(retry_page.cursor_entry_id, backfill_page.cursor_entry_id);
    assert_eq!(retry_page.attempts, 2);
    assert!(
        !database
            .complete_scan_local_metadata_backfill_page(&backfill_page)
            .await?
    );
    database
        .query(
            "UPDATE scan_local_metadata_backfills SET cursor_entry_id = 'stale-cursor'
             WHERE library_root_id = ?",
        )
        .bind(&root_id)
        .execute(database.pool())
        .await?;
    assert!(
        !database
            .complete_scan_local_metadata_backfill_page(&retry_page)
            .await?
    );
    database
        .query(
            "UPDATE scan_local_metadata_backfills SET cursor_entry_id = NULL
             WHERE library_root_id = ?",
        )
        .bind(&root_id)
        .execute(database.pool())
        .await?;
    assert!(
        database
            .complete_scan_local_metadata_backfill_page(&retry_page)
            .await?
    );
    let second_page = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await?
        .ok_or("second PostgreSQL page was not claimable")?;
    assert_eq!(
        second_page.cursor_entry_id,
        retry_page.entry_ids.first().cloned()
    );
    assert!(!second_page.has_more);
    assert_eq!(
        database
            .requeue_interrupted_scan_local_metadata_backfills()
            .await?,
        1
    );
    let recovered_page = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await?
        .ok_or("interrupted PostgreSQL page was not recovered")?;
    assert_eq!(recovered_page.entry_ids, second_page.entry_ids);
    assert_eq!(recovered_page.cursor_entry_id, second_page.cursor_entry_id);
    assert_eq!(recovered_page.attempts, 4);
    assert!(
        !database
            .complete_scan_local_metadata_backfill_page(&second_page)
            .await?
    );
    assert!(
        database
            .complete_scan_local_metadata_backfill_page(&recovered_page)
            .await?
    );
    assert!(
        !database
            .ensure_scan_local_metadata_backfill_root(&root_id)
            .await?
    );

    let source_ids = vec!["postgres-source".to_owned()];
    let batch = NewScanLocalMetadataBatch {
        id: "postgres-outbox-batch",
        job_id: "postgres-scan-job",
        library_root_id: &root_id,
        batch_sequence: 0,
        source_ids: &source_ids,
    };
    assert!(database.enqueue_scan_local_metadata_batch(batch).await?);
    assert!(!database.enqueue_scan_local_metadata_batch(batch).await?);
    let claimed = database
        .claim_next_scan_local_metadata_batch()
        .await?
        .ok_or("outbox batch was not claimable")?;
    assert_eq!(claimed.status, "RUNNING");
    assert_eq!(claimed.attempts, 1);
    assert_eq!(claimed.source_refs_json, r#"["postgres-source"]"#);
    assert_eq!(
        database
            .cancel_scan_local_metadata_batches("postgres-scan-job")
            .await?,
        1
    );
    assert!(
        !database
            .complete_scan_local_metadata_batch(&claimed.id)
            .await?
    );

    let recovery_batch = NewScanLocalMetadataBatch {
        id: "postgres-recovery-batch",
        job_id: "postgres-recovery-job",
        library_root_id: &root_id,
        batch_sequence: 0,
        source_ids: &source_ids,
    };
    assert!(
        database
            .enqueue_scan_local_metadata_batch(recovery_batch)
            .await?
    );
    assert!(
        database
            .claim_next_scan_local_metadata_batch()
            .await?
            .is_some()
    );
    assert_eq!(
        database
            .requeue_interrupted_scan_local_metadata_batches()
            .await?,
        1
    );
    let recovered = database
        .claim_next_scan_local_metadata_batch()
        .await?
        .ok_or("interrupted outbox batch was not recovered")?;
    assert_eq!(recovered.id, "postgres-recovery-batch");
    assert_eq!(recovered.attempts, 2);
    assert!(
        database
            .complete_scan_local_metadata_batch(&recovered.id)
            .await?
    );

    let old_fingerprint = b"postgres-input-v1";
    let current_fingerprint = b"postgres-input-v2";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await?
    );
    assert_eq!(
        database
            .requeue_interrupted_item_metadata_completeness_checks()
            .await?,
        1
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await?
    );
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", current_fingerprint)
            .await?
    );
    assert!(
        !database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                old_fingerprint,
                true,
                1_000,
            )
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", current_fingerprint)
            .await?
    );
    assert!(
        database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                current_fingerprint,
                true,
                1_001,
            )
            .await?
    );
    let missing = database
        .list_confirmed_missing_metadata("POSTER", None, 10)
        .await?;
    assert_eq!(missing.len(), 2);
    assert!(missing.iter().any(|entry| entry.item_id == item_id));

    let dispatch_fingerprint = b"postgres-dispatch-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "BACKDROP", dispatch_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "BACKDROP", dispatch_fingerprint)
            .await?
    );
    let dispatch_results = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "BACKDROP",
        input_fingerprint: dispatch_fingerprint,
        is_missing: true,
        checked_at: 1_002,
    }];
    sqlx::query(
        "CREATE FUNCTION lux_reject_fill_missing_job() RETURNS trigger AS $$
         BEGIN
             IF NEW.mode = 'FILL_MISSING' THEN
                 RAISE EXCEPTION 'injected fill-missing job failure';
             END IF;
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "CREATE TRIGGER reject_fill_missing_job
         BEFORE INSERT ON metadata_reidentify_jobs
         FOR EACH ROW EXECUTE FUNCTION lux_reject_fill_missing_job()",
    )
    .execute(database.pool())
    .await?;
    // Drain the earlier queued job above so this injected failure exercises new-job insertion,
    // rather than correctly reusing that job's remaining capacity.
    assert!(
        database
            .complete_local_metadata_and_enqueue_fill_missing(
                &library_id,
                &dispatch_results,
                std::slice::from_ref(&item_id),
            )
            .await
            .is_err()
    );
    let rolled_back = database
        .find_item_metadata_completeness(&item_id, "BACKDROP")
        .await?
        .ok_or("missing rolled-back completeness row")?;
    assert_eq!(rolled_back.local_state, "RUNNING");
    assert_eq!(rolled_back.is_missing, None);
    sqlx::query("DROP TRIGGER reject_fill_missing_job ON metadata_reidentify_jobs")
        .execute(database.pool())
        .await?;
    sqlx::query("DROP FUNCTION lux_reject_fill_missing_job()")
        .execute(database.pool())
        .await?;

    let scheduled = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &dispatch_results,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert_eq!(scheduled.updated_count, 1);
    assert_eq!(scheduled.scheduled_job_ids.len(), 1);
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM metadata_reidentify_job_items
                 WHERE job_id = ? AND item_id = ? AND status = 'PENDING'",
            )
            .bind(&scheduled.scheduled_job_ids[0])
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?,
        1
    );
    let replayed = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &dispatch_results,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert_eq!(replayed.updated_count, 0);
    assert!(replayed.scheduled_job_ids.is_empty());

    fail_fill_missing_job_with_unavailable_provider(
        &database,
        &scheduled.scheduled_job_ids[0],
        &item_id,
    )
    .await?;
    let retry_pending = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(retry_pending.scheduled_job_ids.is_empty());
    let failed_retry_state: (i64, Option<i64>) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_after
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&scheduled.scheduled_job_ids[0])
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    let now: i64 = database
        .query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(failed_retry_state.0, 1);
    assert!(failed_retry_state.1.is_some_and(|retry_at| retry_at > now));

    database
        .query(
            "UPDATE metadata_reidentify_job_items
         SET automatic_retry_count = 0, automatic_retry_after = NULL
         WHERE job_id = ? AND item_id = ?",
        )
        .bind(&scheduled.scheduled_job_ids[0])
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    database
        .query(
            "UPDATE server_settings SET value = CAST(unixepoch() + 300 AS TEXT)
             WHERE key = 'metadata_fill_missing_legacy_retry_after'",
        )
        .execute(database.pool())
        .await?;
    let legacy_retry_pending = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(legacy_retry_pending.scheduled_job_ids.is_empty());
    database
        .query(
            "UPDATE server_settings SET value = CAST(unixepoch() - 1 AS TEXT)
             WHERE key = 'metadata_fill_missing_legacy_retry_after'",
        )
        .execute(database.pool())
        .await?;
    let due_retry = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert_eq!(due_retry.scheduled_job_ids.len(), 1);
    assert_ne!(
        due_retry.scheduled_job_ids[0],
        scheduled.scheduled_job_ids[0]
    );
    let inherited_retry_state: (i64, Option<i64>) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_after
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&due_retry.scheduled_job_ids[0])
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(inherited_retry_state, (1, None));

    let queued_capacity_fingerprint = b"postgres-queued-capacity-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                &replay_item_id,
                "TRAILER",
                queued_capacity_fingerprint,
            )
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                &replay_item_id,
                "TRAILER",
                queued_capacity_fingerprint,
            )
            .await?
    );
    let queued_capacity_result = [NewItemMetadataCompletenessResult {
        item_id: &replay_item_id,
        capability: "TRAILER",
        input_fingerprint: queued_capacity_fingerprint,
        is_missing: true,
        checked_at: 1_003,
    }];
    database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &queued_capacity_result,
            std::slice::from_ref(&replay_item_id),
        )
        .await?;
    let reused_queued_job_id: String = database
        .query_scalar(
            "SELECT job_id FROM metadata_reidentify_job_items
             WHERE item_id = ? AND status = 'PENDING'
               AND job_id = ?",
        )
        .bind(&replay_item_id)
        .bind(&due_retry.scheduled_job_ids[0])
        .fetch_one(database.pool())
        .await?;
    assert_eq!(reused_queued_job_id, due_retry.scheduled_job_ids[0]);

    let still_fingerprint = b"postgres-still-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "STILL", still_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "STILL", still_fingerprint)
            .await?
    );
    let still_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "STILL",
        input_fingerprint: still_fingerprint,
        is_missing: true,
        checked_at: 1_003,
    }];
    let fill_missing_jobs_before_deduplicated: i64 = database
        .query_scalar("SELECT COUNT(*) FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'")
        .fetch_one(database.pool())
        .await?;
    let deduplicated = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &still_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert_eq!(deduplicated.updated_count, 1);
    assert!(deduplicated.scheduled_job_ids.is_empty());
    let fill_missing_jobs_after_deduplicated: i64 = database
        .query_scalar("SELECT COUNT(*) FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        fill_missing_jobs_after_deduplicated, fill_missing_jobs_before_deduplicated,
        "a deduplicated completeness result must not create another job"
    );

    let pagination_root_path = temp_dir.path().join("Pagination Movies");
    for index in 0..103 {
        let directory = format!("Pagination Movie {index:03} (2025)");
        let movie_dir = pagination_root_path.join(&directory);
        tokio::fs::create_dir_all(&movie_dir).await?;
        tokio::fs::write(
            movie_dir.join(format!("Pagination.Movie.{index:03}.2025.mkv")),
            b"video",
        )
        .await?;
    }
    let pagination_library = libraries
        .create_library("Postgres dispatch pagination", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            pagination_library.id,
            pagination_root_path
                .to_str()
                .ok_or("non-UTF8 pagination root")?,
        )
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(pagination_library.id)
        .await?;
    let pagination_library_id = pagination_library.id.to_string();
    let pagination_item_ids: Vec<String> = database
        .query_scalar(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY id",
        )
        .bind(&pagination_library_id)
        .fetch_all(database.pool())
        .await?;
    assert_eq!(pagination_item_ids.len(), 103);
    let unsupported_item_id = &pagination_item_ids[0];
    let removed_item_id = &pagination_item_ids[1];
    database
        .query("UPDATE media_items SET item_type = 'FOLDER' WHERE id = ?")
        .bind(unsupported_item_id)
        .execute(database.pool())
        .await?;
    database
        .query("UPDATE media_items SET removed_at = 1 WHERE id = ?")
        .bind(removed_item_id)
        .execute(database.pool())
        .await?;
    let pagination_fingerprint = b"postgres-pagination-input-v1";
    let pagination_checks = pagination_item_ids
        .iter()
        .map(|item_id| NewItemMetadataCompletenessCheck {
            item_id,
            capability: "POSTER",
            input_fingerprint: pagination_fingerprint,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&pagination_checks)
            .await?
            .len(),
        103
    );
    let pagination_results = pagination_item_ids
        .iter()
        .map(|item_id| NewItemMetadataCompletenessResult {
            item_id,
            capability: "POSTER",
            input_fingerprint: pagination_fingerprint,
            is_missing: true,
            checked_at: 1_004,
        })
        .collect::<Vec<_>>();
    let pagination_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &pagination_library_id,
            &pagination_results,
            &pagination_item_ids,
            MetadataAutoMatchPolicy::Enabled,
        )
        .await?;
    assert_eq!(pagination_dispatch.updated_count, 102);
    assert_eq!(pagination_dispatch.scheduled_job_ids.len(), 2);
    let pagination_job_counts: Vec<i64> = database
        .query_scalar(
            "SELECT total_count FROM metadata_reidentify_jobs
             WHERE library_id = ? AND mode = 'FILL_MISSING' ORDER BY total_count DESC",
        )
        .bind(&pagination_library_id)
        .fetch_all(database.pool())
        .await?;
    assert_eq!(pagination_job_counts, vec![100, 1]);
    let pagination_job_item_ids: Vec<String> = database
        .query_scalar(
            "SELECT job_items.item_id FROM metadata_reidentify_job_items job_items
             JOIN metadata_reidentify_jobs jobs ON jobs.id = job_items.job_id
             WHERE jobs.library_id = ? AND jobs.mode = 'FILL_MISSING'",
        )
        .bind(&pagination_library_id)
        .fetch_all(database.pool())
        .await?;
    assert_eq!(pagination_job_item_ids.len(), 101);
    assert!(!pagination_job_item_ids.contains(unsupported_item_id));
    assert!(!pagination_job_item_ids.contains(removed_item_id));
    let removed_completeness = database
        .find_item_metadata_completeness(removed_item_id, "POSTER")
        .await?
        .ok_or("missing removed item completeness row")?;
    assert_eq!(removed_completeness.local_state, "RUNNING");

    let empty_root_path = temp_dir.path().join("Empty");
    tokio::fs::create_dir_all(&empty_root_path).await?;
    let empty_library = libraries
        .create_library("Postgres empty backfill", LibraryKind::Movie, false)
        .await?;
    let empty_root = libraries
        .add_root(
            empty_library.id,
            empty_root_path.to_str().ok_or("non-UTF8 empty root")?,
        )
        .await?;
    let empty_root_id = empty_root.root.id.to_string();
    assert!(
        database
            .ensure_scan_local_metadata_backfill_root(&empty_root_id)
            .await?
    );
    assert!(
        database
            .claim_next_scan_local_metadata_backfill_page(1)
            .await?
            .is_none()
    );
    database
        .query("DELETE FROM library_roots WHERE id = ?")
        .bind(&empty_root_id)
        .execute(database.pool())
        .await?;
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM scan_local_metadata_backfills WHERE library_root_id = ?",
            )
            .bind(&empty_root_id)
            .fetch_one(database.pool())
            .await?,
        0
    );

    database.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    admin_pool.close().await;
    Ok(())
}

#[tokio::test]
async fn marking_seen_visible_media_does_not_rewrite_item_state() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root)
        .await
        .expect("media root");
    tokio::fs::write(media_root.join("Visible.Movie.2024.mkv"), b"video")
        .await
        .expect("movie file");

    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root = libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root")
        .root;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("initial index");

    sqlx::query(
        "CREATE TABLE media_item_update_audit (
             item_id TEXT NOT NULL,
             old_removed_at INTEGER,
             new_removed_at INTEGER
         )",
    )
    .execute(database.pool())
    .await
    .expect("audit table");
    sqlx::query(
        "CREATE TRIGGER audit_media_item_update
         AFTER UPDATE ON media_items
         BEGIN
             INSERT INTO media_item_update_audit (item_id, old_removed_at, new_removed_at)
             VALUES (old.id, old.removed_at, new.removed_at);
         END",
    )
    .execute(database.pool())
    .await
    .expect("audit trigger");
    let entry_id: String = sqlx::query_scalar(
        "SELECT id FROM filesystem_entries
         WHERE library_root_id = ? AND relative_path = 'Visible.Movie.2024.mkv'",
    )
    .bind(root.id.to_string())
    .fetch_one(database.pool())
    .await
    .expect("filesystem entry");
    database
        .mark_filesystem_entries_seen_batch(std::slice::from_ref(&entry_id), "next-generation")
        .await
        .expect("mark entry seen");
    let updates: Vec<(String, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT item_id, old_removed_at, new_removed_at
         FROM media_item_update_audit",
    )
    .fetch_all(database.pool())
    .await
    .expect("audit rows");
    assert!(updates.is_empty());

    sqlx::query("DELETE FROM media_item_update_audit")
        .execute(database.pool())
        .await
        .expect("clear audit rows");
    sqlx::query("UPDATE filesystem_entries SET is_missing = 1 WHERE id = ?")
        .bind(&entry_id)
        .execute(database.pool())
        .await
        .expect("mark entry missing");
    sqlx::query("DELETE FROM media_item_update_audit")
        .execute(database.pool())
        .await
        .expect("clear missing audit row");
    database
        .mark_filesystem_entries_seen_batch(std::slice::from_ref(&entry_id), "recovered-generation")
        .await
        .expect("restore entry");
    let recovery_update_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_item_update_audit")
            .fetch_one(database.pool())
            .await
            .expect("recovery audit count");
    assert_eq!(recovery_update_count, 1);
    let available: i64 = sqlx::query_scalar(
        "SELECT has_available_source FROM media_items
         WHERE id = (SELECT item_id FROM media_sources WHERE filesystem_entry_id = ?)",
    )
    .bind(&entry_id)
    .fetch_one(database.pool())
    .await
    .expect("recovered availability");
    assert_eq!(available, 1);
}

#[tokio::test]
async fn sidecar_targets_batch_multiple_directories_in_one_query() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    for (directory, file_name) in [
        ("Alpha", "Alpha.Movie.2020.mkv"),
        ("Beta", "Beta.Movie.2021.mkv"),
        ("Gamma", "Gamma.Movie.2022.mkv"),
    ] {
        let directory = media_root.join(directory);
        tokio::fs::create_dir_all(&directory)
            .await
            .expect("movie directory");
        tokio::fs::write(directory.join(file_name), b"video")
            .await
            .expect("movie file");
    }

    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root = libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root")
        .root;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("initial index");
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await
        .expect("scan job");

    database.reset_query_count();
    database
        .record_scan_job_sidecar_targets(
            &job.id,
            &root.id.to_string(),
            &[
                "Alpha/poster.jpg".to_owned(),
                "Beta/poster.jpg".to_owned(),
                "Gamma/poster.jpg".to_owned(),
            ],
        )
        .await
        .expect("record sidecar targets");

    assert_eq!(database.query_count(), 1);
    let target_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_job_targets
         WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await
    .expect("sidecar target count");
    assert_eq!(target_count, 3);
}

#[tokio::test]
async fn library_listing_uses_constant_number_of_child_queries() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let service = LibraryService::new(database.clone());

    for index in 0..3 {
        let library = service
            .create_library_with_scraper(
                &format!("Library {index}"),
                LibraryKind::Movie,
                false,
                Some("tmdb"),
                false,
            )
            .await
            .expect("library");
        let root = temp_dir.path().join(format!("root-{index}"));
        tokio::fs::create_dir(&root).await.expect("library root");
        service
            .add_root(library.id, root.to_str().expect("utf-8 root"))
            .await
            .expect("library root record");
    }

    database.reset_query_count();
    let views = service.list_libraries().await.expect("library views");

    assert_eq!(views.len(), 3);
    assert!(views.iter().all(|view| view.library.scrapers.len() == 1));
    assert!(views.iter().all(|view| view.roots.len() == 1));
    assert_eq!(database.query_count(), 3);
}

#[tokio::test]
async fn migration_library_identity_listing_reads_only_enabled_libraries_once() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let service = LibraryService::new(database.clone());

    let enabled = service
        .create_library("Enabled", LibraryKind::Movie, false)
        .await
        .expect("enabled library");
    let disabled = service
        .create_library("Disabled", LibraryKind::Movie, false)
        .await
        .expect("disabled library");
    let root = temp_dir.path().join("enabled-root");
    tokio::fs::create_dir(&root).await.expect("library root");
    let canonical_root = tokio::fs::canonicalize(&root)
        .await
        .expect("canonical library root");
    service
        .add_root(enabled.id, root.to_str().expect("utf-8 root"))
        .await
        .expect("enabled library root");
    sqlx::query("UPDATE libraries SET is_enabled = 0 WHERE id = ?")
        .bind(disabled.id.to_string())
        .execute(database.pool())
        .await
        .expect("disable library");

    database.reset_query_count();
    let identities = database
        .list_enabled_library_identities()
        .await
        .expect("migration identities");

    assert_eq!(database.query_count(), 1);
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0].id, enabled.id.to_string());
    assert_eq!(identities[0].name, "Enabled");
    assert_eq!(
        identities[0].root_paths,
        vec![canonical_root.to_string_lossy().into_owned()]
    );
}

#[tokio::test]
async fn recent_catalog_rows_use_one_query_for_multiple_libraries() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let service = LibraryService::new(database.clone());
    let first = service
        .create_library("First", LibraryKind::Movie, false)
        .await
        .expect("first library");
    let second = service
        .create_library("Second", LibraryKind::Movie, false)
        .await
        .expect("second library");
    let first_id = first.id.to_string();
    let second_id = second.id.to_string();

    for (item_id, library_id, title, added_at) in [
        ("recent-first-old", &first_id, "First old movie", 10_i64),
        ("recent-first-new", &first_id, "First new movie", 20_i64),
        ("recent-second-old", &second_id, "Second old movie", 5_i64),
        ("recent-second-new", &second_id, "Second new movie", 15_i64),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, added_at, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?, 1)",
        )
        .bind(item_id)
        .bind(library_id)
        .bind(title)
        .bind(title.to_ascii_lowercase())
        .bind(added_at)
        .execute(database.pool())
        .await
        .expect("media item");
    }

    database.reset_query_count();
    let rows = database
        .list_recent_catalog_rows_by_library(&[first_id, second_id], 1)
        .await
        .expect("recent catalog rows");

    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<std::collections::HashSet<_>>(),
        std::collections::HashSet::from(["recent-first-new", "recent-second-new"])
    );
    assert_eq!(database.query_count(), 1);
}

#[tokio::test]
async fn recent_catalog_rows_include_visible_unavailable_series() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Series", LibraryKind::Series, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();

    for (item_id, title, added_at) in [
        ("recent-series-movie-old", "Old movie", 10_i64),
        ("recent-series-movie-new", "New movie", 20_i64),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, added_at, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?, 1)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(title)
        .bind(title.to_ascii_lowercase())
        .bind(added_at)
        .execute(database.pool())
        .await
        .expect("movie");
    }
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, added_at, has_available_source
             ) VALUES ('recent-visible-series', ?, 'SERIES',
                       'Visible series', 'visible series', 'LOCAL_CONFIRMED', 30, 0)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("unavailable series");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, parent_id, title, sort_title,
                identification_status, added_at, has_available_source
             ) VALUES ('recent-visible-episode', ?, 'EPISODE',
                       'recent-visible-series', 'Visible episode', 'visible episode',
                       'LOCAL_CONFIRMED', 40, 1)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("visible episode");

    let rows = database
        .list_recent_catalog_rows_by_library(&[library_id], 2)
        .await
        .expect("recent catalog rows");

    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["recent-visible-series", "recent-series-movie-new"]
    );
}

#[tokio::test]
async fn recent_catalog_rows_promote_series_when_a_new_episode_is_added() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Series", LibraryKind::Series, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();

    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, added_at, has_available_source
             ) VALUES ('recent-series-movie', ?, 'MOVIE', 'Movie', 'movie',
                       'LOCAL_CONFIRMED', 20, 1)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("movie");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, added_at, has_available_source
             ) VALUES ('recent-series', ?, 'SERIES', 'Series', 'series',
                       'LOCAL_CONFIRMED', 10, 0)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("series");
    for (item_id, title, added_at) in [
        ("recent-series-episode-old", "Episode 1", 10_i64),
        ("recent-series-episode-new", "Episode 2", 30_i64),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, parent_id, series_id,
                    title, sort_title, identification_status, added_at,
                    has_available_source
                 ) VALUES (?, ?, 'EPISODE', 'recent-series', 'recent-series',
                           ?, ?, 'LOCAL_CONFIRMED', ?, 1)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(title)
        .bind(title.to_ascii_lowercase())
        .bind(added_at)
        .execute(database.pool())
        .await
        .expect("episode");
    }

    let rows = database
        .list_recent_catalog_rows_by_library(&[library_id], 2)
        .await
        .expect("recent catalog rows");

    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["recent-series", "recent-series-movie"]
    );
}

#[tokio::test]
async fn recommended_catalog_rows_stop_awarding_freshness_after_seven_days() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await
        .expect("current timestamp");
    let library_id = library.id.to_string();
    let user_id = user.id.to_string();

    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, added_at, has_available_source
             ) VALUES
                ('item-fifteen-days-old', ?, 'MOVIE', 'Fifteen Days Old', 'fifteen days old', 'LOCAL_CONFIRMED', ?, 1),
                ('item-new', ?, 'MOVIE', 'New Movie', 'new movie', 'LOCAL_CONFIRMED', ?, 1)",
    )
    .bind(&library_id)
    .bind(now - 15 * 86_400)
    .bind(&library_id)
    .bind(now)
    .execute(database.pool())
    .await
    .expect("media items");
    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id)
         VALUES (?, 'item-new')",
    )
    .bind(&user_id)
    .execute(database.pool())
    .await
    .expect("user item state");
    refresh_recommendation_stats(&database).await;

    let rows = database
        .list_recommended_catalog_rows(&user_id, &[library_id], 0, 2)
        .await
        .expect("recommended catalog rows");

    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        ["item-fifteen-days-old", "item-new"]
    );
}

#[tokio::test]
async fn recommended_catalog_rows_limit_recent_playback_items_and_remove_old_state_bonuses() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
        .await
        .expect("library root");
    let root_id: String = sqlx::query_scalar("SELECT id FROM library_roots LIMIT 1")
        .fetch_one(database.pool())
        .await
        .expect("library root id");
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await
        .expect("current timestamp");
    let library_id = library.id.to_string();
    let user_id = user.id.to_string();
    let item_ids = [
        "active-1",
        "active-2",
        "active-3",
        "active-4",
        "active-5",
        "active-6",
        "unplayed-1",
        "unplayed-2",
        "favorite-only",
    ];

    for item_id in item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, added_at, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?, 1)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .bind(now - 15 * 86_400)
        .execute(database.pool())
        .await
        .expect("media item");
    }

    for (entry_id, source_id, relative_path) in [
        ("active-1-entry-a", "active-1-source-a", "active-1-a.mkv"),
        ("active-1-entry-b", "active-1-source-b", "active-1-b.mkv"),
    ] {
        sqlx::query(
            "INSERT INTO filesystem_entries
             (id, library_root_id, relative_path, entry_kind, size, modified_at, last_seen_generation)
             VALUES (?, ?, ?, 'FILE', 1, 1, 'generation')",
        )
        .bind(entry_id)
        .bind(&root_id)
        .bind(relative_path)
        .execute(database.pool())
        .await
        .expect("filesystem entry");
        sqlx::query(
            "INSERT INTO media_sources (id, item_id, source_kind, filesystem_entry_id)
             VALUES (?, 'active-1', 'LOCAL_FILE', ?)",
        )
        .bind(source_id)
        .bind(entry_id)
        .execute(database.pool())
        .await
        .expect("media source");
    }

    for item_id in [
        "active-1", "active-2", "active-3", "active-4", "active-5", "active-6",
    ] {
        sqlx::query(
            "INSERT INTO user_item_state (
                    user_id, item_id, position_ticks, is_favorite, last_played_at
                 ) VALUES (?, ?, 1, 1, ?)",
        )
        .bind(&user_id)
        .bind(item_id)
        .bind(now)
        .execute(database.pool())
        .await
        .expect("active user item state");
    }
    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, is_favorite)
         VALUES (?, 'favorite-only', 1)",
    )
    .bind(&user_id)
    .execute(database.pool())
    .await
    .expect("favorite user item state");
    refresh_recommendation_stats(&database).await;

    let rows = database
        .list_recommended_catalog_rows(&user_id, &[library_id], 0, 7)
        .await
        .expect("recommended catalog rows");

    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<std::collections::HashSet<_>>(),
        std::collections::HashSet::from([
            "active-1",
            "active-2",
            "active-3",
            "active-4",
            "active-5",
            "unplayed-1",
            "unplayed-2",
        ])
    );
}

#[tokio::test]
async fn recommended_catalog_rows_cap_engagement_scores_and_expire_old_playback() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await
        .expect("current timestamp");
    let playback_cutoff = now - 180 * 86_400;
    let library_id = library.id.to_string();
    let user_id = user.id.to_string();
    let item_ids = [
        "favorite-11",
        "favorite-20",
        "play-51",
        "play-60",
        "play-expired",
        "baseline",
    ];

    for item_id in item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, added_at, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?, 1)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .bind(now - 15 * 86_400)
        .execute(database.pool())
        .await
        .expect("media item");
    }

    for index in 0..60 {
        let playback_user_id = format!("recommendation-user-{index}");
        sqlx::query(
            "INSERT INTO users (
                    id, username_normalized, display_name, password_hash
                 ) VALUES (?, ?, ?, 'test')",
        )
        .bind(&playback_user_id)
        .bind(&playback_user_id)
        .bind(&playback_user_id)
        .execute(database.pool())
        .await
        .expect("playback user");

        if index < 51 {
            sqlx::query(
                "INSERT INTO user_item_state (user_id, item_id, last_played_at)
                 VALUES (?, 'play-51', ?)",
            )
            .bind(&playback_user_id)
            .bind(now)
            .execute(database.pool())
            .await
            .expect("play-51 state");
        }
        sqlx::query(
            "INSERT INTO user_item_state (user_id, item_id, last_played_at)
             VALUES (?, 'play-60', ?)",
        )
        .bind(&playback_user_id)
        .bind(now)
        .execute(database.pool())
        .await
        .expect("play-60 state");
        sqlx::query(
            "INSERT INTO user_item_state (user_id, item_id, last_played_at)
             VALUES (?, 'play-expired', ?)",
        )
        .bind(&playback_user_id)
        .bind(playback_cutoff)
        .execute(database.pool())
        .await
        .expect("expired playback state");
        if index < 11 {
            sqlx::query(
                "INSERT INTO user_item_state (user_id, item_id, is_favorite)
                 VALUES (?, 'favorite-11', 1)",
            )
            .bind(&playback_user_id)
            .execute(database.pool())
            .await
            .expect("favorite-11 state");
        }
        if index < 20 {
            sqlx::query(
                "INSERT INTO user_item_state (user_id, item_id, is_favorite)
                 VALUES (?, 'favorite-20', 1)",
            )
            .bind(&playback_user_id)
            .execute(database.pool())
            .await
            .expect("favorite-20 state");
        }
    }

    refresh_recommendation_stats(&database).await;

    let rows = database
        .list_recommended_catalog_rows(&user_id, &[library_id], 0, 5)
        .await
        .expect("recommended catalog rows");

    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        [
            "favorite-11",
            "favorite-20",
            "play-51",
            "play-60",
            "baseline",
        ]
    );
}

#[tokio::test]
async fn recommended_catalog_rows_use_rating_median_for_missing_ratings() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await
        .expect("current timestamp");
    let library_id = library.id.to_string();
    let user_id = user.id.to_string();

    for (item_id, rating) in [
        ("rating-low", Some(0.0_f64)),
        ("rating-top", Some(10.0_f64)),
        ("rating-unknown", None),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, added_at, has_available_source, rating
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?, 1, ?)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .bind(now - 15 * 86_400)
        .bind(rating)
        .execute(database.pool())
        .await
        .expect("rated media item");
    }
    refresh_recommendation_stats(&database).await;

    database.reset_query_count();
    let rows = database
        .list_recommended_catalog_rows(&user_id, std::slice::from_ref(&library_id), 0, 3)
        .await
        .expect("recommended catalog rows");
    let first_query_count = database.query_count();
    database.reset_query_count();
    let cached_rows = database
        .list_recommended_catalog_rows(&user_id, std::slice::from_ref(&library_id), 0, 3)
        .await
        .expect("cached recommended catalog rows");

    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        ["rating-top", "rating-unknown", "rating-low"]
    );
    assert_eq!(first_query_count, 4);
    assert_eq!(database.query_count(), 1);
    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        cached_rows
            .iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>()
    );

    sqlx::query("UPDATE media_items SET updated_at = 0 WHERE id = 'rating-low'")
        .execute(database.pool())
        .await
        .expect("reset metadata timestamp");
    database
        .update_media_item_metadata(MediaMetadataUpdate {
            item_id: "rating-low",
            title: "rating-low",
            original_title: None,
            overview: None,
            production_year: None,
            premiere_date: None,
            rating: Some(10.0),
            rating_source: Some("TEST"),
            provider_ids_json: None,
            metadata_fingerprint: &[],
            provenance_json: "{}",
            locked_fields_json: "{}",
        })
        .await
        .expect("updated rating");
    let updated_at: i64 =
        sqlx::query_scalar("SELECT updated_at FROM media_items WHERE id = 'rating-low'")
            .fetch_one(database.pool())
            .await
            .expect("metadata timestamp");
    assert!(updated_at > 0);
    sqlx::query("UPDATE media_items SET updated_at = 123 WHERE id = 'rating-low'")
        .execute(database.pool())
        .await
        .expect("set stable metadata timestamp");
    database
        .update_media_item_metadata(MediaMetadataUpdate {
            item_id: "rating-low",
            title: "rating-low",
            original_title: None,
            overview: None,
            production_year: None,
            premiere_date: None,
            rating: Some(10.0),
            rating_source: Some("TEST"),
            provider_ids_json: None,
            metadata_fingerprint: &[],
            provenance_json: "{}",
            locked_fields_json: "{}",
        })
        .await
        .expect("no-op metadata update");
    let unchanged_at: i64 =
        sqlx::query_scalar("SELECT updated_at FROM media_items WHERE id = 'rating-low'")
            .fetch_one(database.pool())
            .await
            .expect("no-op metadata timestamp");
    assert_eq!(unchanged_at, 123);
    database.reset_query_count();
    let refreshed_rows = database
        .list_recommended_catalog_rows(&user_id, std::slice::from_ref(&library_id), 0, 3)
        .await
        .expect("refreshed recommended catalog rows");
    assert_eq!(database.query_count(), 1);
    assert_eq!(
        refreshed_rows
            .iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        ["rating-low", "rating-top", "rating-unknown"]
    );
}

#[tokio::test]
async fn selecting_metadata_candidate_keeps_recommendation_rating_median_until_ttl() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Ratings", LibraryKind::Movie, false)
        .await
        .expect("library");
    let item_id = "rating-selection-item";
    let candidate_id = "rating-selection-candidate";
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, has_available_source, rating
             ) VALUES (?, ?, 'MOVIE', 'Rating selection', 'rating selection',
                       'LOCAL_CONFIRMED', 1, 1.0)",
    )
    .bind(item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    sqlx::query(
        "INSERT INTO metadata_candidates (
                id, item_id, provider, provider_id, candidate_json, score, status
             ) VALUES (?, ?, 'TMDB', 'rating-selection', '{}', 100, 'PENDING')",
    )
    .bind(candidate_id)
    .bind(item_id)
    .execute(database.pool())
    .await
    .expect("metadata candidate");

    assert_eq!(
        database
            .recommendation_rating_median(&[library.id.to_string()])
            .await
            .expect("initial median"),
        1.0
    );
    assert!(
        database
            .select_metadata_candidate(SelectedMetadataUpdate {
                item_id,
                candidate_id,
                title: "Rating selection",
                original_title: None,
                overview: None,
                production_year: None,
                premiere_date: None,
                last_air_date: None,
                status: None,
                original_language: None,
                rating: Some(9.0),
                rating_source: Some("TMDB"),
                provider_ids_json: "{}",
                metadata_scraper_id: None,
                metadata_fingerprint: &[],
                provenance_json: "{}",
                locked_fields_json: "[]",
                poster_fallback_required: false,
                keep_pending: false,
            })
            .await
            .expect("select metadata candidate")
    );
    assert_eq!(
        database
            .recommendation_rating_median(&[library.id.to_string()])
            .await
            .expect("cached median"),
        1.0
    );
}

#[tokio::test]
async fn recommendation_rating_median_survives_restart_until_thirty_day_ttl() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Ratings", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, has_available_source, rating
             ) VALUES ('persistent-rating-item', ?, 'MOVIE', 'Persistent rating',
                       'persistent rating', 'LOCAL_CONFIRMED', 1, 7.0)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("media item");
    assert_eq!(
        database
            .recommendation_rating_median(std::slice::from_ref(&library_id))
            .await
            .expect("initial median"),
        7.0
    );
    database.close().await;

    let restarted = Database::connect(&config)
        .await
        .expect("restarted database");
    sqlx::query("UPDATE media_items SET rating = 9.0 WHERE id = 'persistent-rating-item'")
        .execute(restarted.pool())
        .await
        .expect("updated rating");
    assert_eq!(
        restarted
            .recommendation_rating_median(std::slice::from_ref(&library_id))
            .await
            .expect("persistent median"),
        7.0
    );
    sqlx::query(
        "UPDATE recommendation_rating_cache
         SET calculated_at = unixepoch() - 30 * 86400",
    )
    .execute(restarted.pool())
    .await
    .expect("expired median cache");
    restarted.close().await;
    let expired = Database::connect(&config)
        .await
        .expect("expired cache database");
    assert_eq!(
        expired
            .recommendation_rating_median(std::slice::from_ref(&library_id))
            .await
            .expect("refreshed median"),
        9.0
    );
}

#[tokio::test]
async fn pending_metadata_candidates_load_current_items_in_one_batch() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Metadata candidates", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    for (index, title) in [(1, "First"), (2, "Second")] {
        let item_id = format!("candidate-item-{index}");
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'PENDING')",
        )
        .bind(&item_id)
        .bind(&library_id)
        .bind(title)
        .bind(title.to_ascii_lowercase())
        .execute(database.pool())
        .await
        .expect("media item");
        sqlx::query(
            "INSERT INTO metadata_candidates (
                    id, item_id, provider, provider_id, candidate_json, score, status
                 ) VALUES (?, ?, 'TMDB', ?, ?, 80, 'PENDING')",
        )
        .bind(format!("candidate-{index}"))
        .bind(&item_id)
        .bind(index.to_string())
        .bind(serde_json::json!({"title": title}).to_string())
        .execute(database.pool())
        .await
        .expect("metadata candidate");
    }

    database.reset_query_count();
    let page = MetadataCandidateService::new(database.clone())
        .list_pending(0, 50)
        .await
        .expect("pending candidates");

    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].field_diffs.len(), 0);
    assert_eq!(page.items[1].field_diffs.len(), 0);
    assert_eq!(database.query_count(), 3);
}

#[tokio::test]
async fn automatic_candidate_summary_keeps_the_best_results_from_the_existing_page_window() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Automatic candidate summary", LibraryKind::Movie, false)
        .await
        .expect("library");
    database
        .query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('automatic-summary-item', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await
        .expect("media item");

    for index in 0..60 {
        let score = match index {
            49 => 99.0,
            40 => 98.0,
            59 => 100.0,
            _ => index as f64,
        };
        database
            .query(
                "INSERT INTO metadata_candidates (
                    id, item_id, provider, provider_id, candidate_json, score, status
                 ) VALUES (?, 'automatic-summary-item', 'tmdb', ?, '{}', ?, 'PENDING')",
            )
            .bind(format!("candidate-{index:03}"))
            .bind(format!("movie-{index}"))
            .bind(score)
            .execute(database.pool())
            .await
            .expect("metadata candidate");
    }

    database.reset_query_count();
    let (candidates, total_count) = database
        .list_best_pending_metadata_candidates_for_item("automatic-summary-item", 50, 2)
        .await
        .expect("automatic candidate summary");
    assert_eq!(database.query_count(), 1);
    assert_eq!(total_count, 60);
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.id.as_str())
            .collect::<Vec<_>>(),
        ["candidate-049", "candidate-040"],
        "the summary ranks only the same first-50 page the previous loader exposed"
    );

    database.close().await;
}

#[tokio::test]
async fn collection_refresh_uses_provider_index_and_batch_insert() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Collections", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    for index in 1..=3 {
        let item_id = format!("collection-movie-{index}");
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, provider_ids_json,
                    identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, ?, 'ONLINE_CONFIRMED')",
        )
        .bind(&item_id)
        .bind(&library_id)
        .bind(format!("Movie {index}"))
        .bind(format!("movie {index}"))
        .bind(serde_json::json!({"tmdb": index.to_string()}).to_string())
        .execute(database.pool())
        .await
        .expect("media item");
    }
    let member_provider_ids = (1..=3)
        .map(|index| ("TMDB".to_owned(), index.to_string(), index))
        .collect::<Vec<_>>();

    database.reset_query_count();
    let result = database
        .upsert_collection(NewCollection {
            library_id: &library_id,
            provider: "tmdb",
            provider_id: "collection-1",
            title: "Collection",
            overview: None,
            poster_path: None,
            backdrop_path: None,
            member_provider_ids: &member_provider_ids,
        })
        .await
        .expect("collection refresh");

    assert_eq!(result.member_count, 3);
    assert_eq!(database.query_count(), 8);
    let member_ids = sqlx::query_scalar::<_, String>(
        "SELECT item_id FROM collection_items
         WHERE collection_id = (SELECT id FROM collections WHERE provider_id = 'collection-1')
         ORDER BY sort_order",
    )
    .fetch_all(database.pool())
    .await
    .expect("collection members");
    assert_eq!(
        member_ids,
        vec![
            "collection-movie-1".to_owned(),
            "collection-movie-2".to_owned(),
            "collection-movie-3".to_owned(),
        ]
    );
}

#[tokio::test]
async fn emby_collection_member_mutations_use_bounded_batches() {
    const ITEM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Manual collections", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    let other_library = LibraryService::new(database.clone())
        .create_library("Other manual collections", LibraryKind::Movie, false)
        .await
        .expect("other library");
    let item_ids = (0..ITEM_COUNT)
        .map(|index| format!("manual-collection-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    let foreign_item_id = "manual-collection-foreign";
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
    )
    .bind(foreign_item_id)
    .bind(other_library.id.to_string())
    .bind(foreign_item_id)
    .bind(foreign_item_id)
    .execute(database.pool())
    .await
    .expect("foreign media item");
    let removed_item_id = "manual-collection-removed";
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status, removed_at
         ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', unixepoch())",
    )
    .bind(removed_item_id)
    .bind(&library_id)
    .bind(removed_item_id)
    .bind(removed_item_id)
    .execute(database.pool())
    .await
    .expect("removed media item");

    let collection = database
        .create_emby_collection("Manual batch", &[item_ids[0].clone()])
        .await
        .expect("create collection")
        .expect("collection");
    let mut requested_ids = item_ids.clone();
    requested_ids.extend([foreign_item_id.to_owned(), removed_item_id.to_owned()]);

    database.reset_query_count();
    database
        .add_emby_collection_items(&collection.collection_item_id, &requested_ids)
        .await
        .expect("add collection members")
        .expect("collection exists");
    assert_eq!(database.query_count(), 5);
    let stored_ids: Vec<String> = sqlx::query_scalar(
        "SELECT mi.id
         FROM collection_items ci
         JOIN collections c ON c.id = ci.collection_id
         JOIN media_items mi ON mi.id = ci.item_id
         WHERE c.item_id = ?
         ORDER BY ci.sort_order, mi.id",
    )
    .bind(&collection.collection_item_id)
    .fetch_all(database.pool())
    .await
    .expect("stored collection members");
    assert_eq!(stored_ids, item_ids);

    database.reset_query_count();
    database
        .remove_emby_collection_items(&collection.collection_item_id, &requested_ids)
        .await
        .expect("remove collection members")
        .expect("collection exists");
    assert_eq!(database.query_count(), 2);
    let remaining: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM collection_items ci
         JOIN collections c ON c.id = ci.collection_id
         WHERE c.item_id = ?",
    )
    .bind(&collection.collection_item_id)
    .fetch_one(database.pool())
    .await
    .expect("remaining collection members");
    assert_eq!(remaining, 0);

    database.close().await;
}

#[tokio::test]
async fn favorite_catalog_filter_uses_favorite_state_index() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library_ids = vec!["library".to_owned()];
    let empty_item_types = Vec::new();
    let empty_excluded_item_types = Vec::new();
    let empty_years = Vec::new();
    let filter = CatalogFilterQuery {
        library_ids: &library_ids,
        user_id: "user",
        item_types: &empty_item_types,
        excluded_item_types: &empty_excluded_item_types,
        item_ids: None,
        person_id: None,
        media_source_ids: None,
        provider_id_equals: None,
        years: &empty_years,
        is_played: None,
        is_favorite: Some(true),
        min_date_last_saved: None,
        metadata_pending: false,
        parent_id_scope: None,
        sort_by: CatalogSort::DateCreated,
        descending: true,
        offset: 0,
        limit: 24,
    };
    let (where_clause, binds) = catalog_filter_where_clause(&filter);
    let query = format!(
        "EXPLAIN QUERY PLAN
             SELECT COUNT(*) FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             {where_clause}"
    );
    let mut statement = database.query(sqlx::AssertSqlSafe(query));
    for bind in &binds {
        statement = match bind {
            CatalogBind::Text(value) => statement.bind(*value),
            CatalogBind::Integer(value) => statement.bind(*value),
            CatalogBind::Real(value) => statement.bind(*value),
        };
    }
    let plan = statement
        .fetch_all(database.pool())
        .await
        .expect("favorite query plan")
        .into_iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>();

    assert!(
        plan.iter()
            .any(|detail| detail.contains("idx_user_item_state_favorites")),
        "favorite query did not use the favorite state index: {plan:?}"
    );
}

#[tokio::test]
async fn recommendation_query_uses_materialized_stats_and_image_indexes() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Recommendation plan", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    let query = format!(
        "EXPLAIN QUERY PLAN
         SELECT mi.id
                , COALESCE(rs.recent_playback_score, 0)
         FROM media_items mi
         JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
         LEFT JOIN recommendation_item_stats rs ON rs.item_id = mi.id
         LEFT JOIN user_item_state us
           ON us.item_id = mi.id AND us.user_id = ?
         WHERE mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
           AND mi.item_type IN ('MOVIE', 'SERIES')
           AND mi.library_id IN (?)"
    );
    let plan = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind("plan-user")
        .bind(&library_id)
        .fetch_all(database.pool())
        .await
        .expect("recommendation query plan")
        .into_iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>();
    assert!(
        plan.iter()
            .any(|detail| detail.contains("recommendation_item_stats")),
        "recommendation query did not use materialized stats: {plan:?}"
    );

    let image_plan = sqlx::query(
        "EXPLAIN QUERY PLAN
         SELECT (SELECT id FROM item_images
                 WHERE item_id = ? AND image_type = 'POSTER'
                 ORDER BY image_index LIMIT 1)",
    )
    .bind("plan-item")
    .fetch_all(database.pool())
    .await
    .expect("image query plan")
    .into_iter()
    .map(|row| row.get::<String, _>("detail"))
    .collect::<Vec<_>>();
    assert!(
        image_plan
            .iter()
            .any(|detail| detail.contains("idx_item_images_recommendation_lookup")),
        "recommendation image lookup did not use the covering index: {image_plan:?}"
    );
}

#[tokio::test]
async fn concurrent_metadata_capability_writes_are_serialized() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let item_id = "metadata-write-item";
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Metadata', 'metadata', 'LOCAL_CONFIRMED')",
    )
    .bind(item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");

    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..32 {
        let database = database.clone();
        tasks.spawn(async move {
            let results = std::iter::repeat_with(|| MetadataCapabilityResult {
                capability: "CREDITS",
                has_data: true,
            })
            .take(128)
            .collect::<Vec<_>>();
            database
                .record_metadata_capability_results(
                    item_id,
                    "tmdb",
                    &format!("{index}"),
                    &results,
                    1_000 + index,
                )
                .await
        });
    }

    while let Some(result) = tasks.join_next().await {
        result
            .expect("metadata writer task should not panic")
            .expect("metadata writes should not fail under concurrency");
    }
    database.close().await;
}

#[tokio::test]
async fn metadata_candidate_batch_uses_one_statement_and_preserves_upsert_behavior() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('candidate-batch-item', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");

    let candidates = [
        NewMetadataCandidate {
            id: "candidate-first-id",
            item_id: "candidate-batch-item",
            provider: "tmdb",
            provider_id: "movie-1",
            candidate_json: r#"{"title":"Lower score"}"#,
            score: 70.0,
            expires_at: Some(1_000),
        },
        NewMetadataCandidate {
            id: "candidate-second-id",
            item_id: "candidate-batch-item",
            provider: "tmdb",
            provider_id: "movie-1",
            candidate_json: r#"{"title":"Higher score, first result"}"#,
            score: 90.0,
            expires_at: Some(2_000),
        },
        NewMetadataCandidate {
            id: "candidate-third-id",
            item_id: "candidate-batch-item",
            provider: "tmdb",
            provider_id: "movie-1",
            candidate_json: r#"{"title":"Higher score, later tie"}"#,
            score: 90.0,
            expires_at: Some(2_500),
        },
        NewMetadataCandidate {
            id: "candidate-other-id",
            item_id: "candidate-batch-item",
            provider: "tmdb",
            provider_id: "movie-2",
            candidate_json: r#"{"title":"Other candidate"}"#,
            score: 60.0,
            expires_at: Some(3_000),
        },
    ];

    database.reset_query_count();
    database
        .insert_metadata_candidates(&candidates)
        .await
        .expect("insert candidate batch");
    assert_eq!(database.query_count(), 1);

    let rows: Vec<(String, String, String, f64, Option<i64>)> = sqlx::query_as(
        "SELECT id, provider_id, candidate_json, score, expires_at
         FROM metadata_candidates
         WHERE item_id = 'candidate-batch-item'
         ORDER BY provider_id",
    )
    .fetch_all(database.pool())
    .await
    .expect("candidate rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        (
            "candidate-first-id".to_owned(),
            "movie-1".to_owned(),
            r#"{"title":"Higher score, later tie"}"#.to_owned(),
            90.0,
            Some(2_500),
        )
    );
    assert_eq!(rows[1].0, "candidate-other-id");
    assert_eq!(rows[1].2, r#"{"title":"Other candidate"}"#);

    database.reset_query_count();
    let (page, total) = database
        .list_pending_metadata_candidates_for_item_with_count("candidate-batch-item", 0, 1)
        .await
        .expect("candidate page with total");
    assert_eq!(database.query_count(), 1);
    assert_eq!(page.len(), 1);
    assert_eq!(total, 2);

    database.close().await;
}

#[tokio::test]
async fn metadata_capability_batches_preserve_status_and_retry_backoff() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('capability-batch-item', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");

    let capabilities = [
        MetadataCapabilityResult {
            capability: "CREDITS",
            has_data: true,
        },
        MetadataCapabilityResult {
            capability: "EXTERNAL_IDS",
            has_data: false,
        },
        MetadataCapabilityResult {
            capability: "CREDITS",
            has_data: false,
        },
    ];
    database.reset_query_count();
    database
        .record_metadata_capability_results(
            "capability-batch-item",
            "tmdb",
            "movie-1",
            &capabilities,
            1_000,
        )
        .await
        .expect("record capability batch");
    assert_eq!(database.query_count(), 1);
    let capability_rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT capability, status, attempt_count
         FROM metadata_capability_attempts
         WHERE item_id = 'capability-batch-item' AND provider = 'tmdb'
         ORDER BY capability",
    )
    .fetch_all(database.pool())
    .await
    .expect("capability attempts");
    assert_eq!(
        capability_rows,
        vec![
            ("CREDITS".to_owned(), "UNAVAILABLE".to_owned(), 1),
            ("EXTERNAL_IDS".to_owned(), "UNAVAILABLE".to_owned(), 1),
        ]
    );

    database.reset_query_count();
    database
        .record_metadata_capability_failures(
            "capability-batch-item",
            "tmdb",
            "movie-1",
            &["EXTERNAL_IDS", "TRAILERS"],
            2_000,
        )
        .await
        .expect("record capability failure batch");
    assert_eq!(database.query_count(), 2);
    let failure_rows: Vec<(String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT capability, attempt_count, next_retry_at
         FROM metadata_capability_attempts
         WHERE item_id = 'capability-batch-item' AND provider = 'tmdb'
           AND status = 'FAILED'
         ORDER BY capability",
    )
    .fetch_all(database.pool())
    .await
    .expect("failed capability attempts");
    assert_eq!(
        failure_rows,
        vec![
            ("EXTERNAL_IDS".to_owned(), 2, Some(2_600)),
            ("TRAILERS".to_owned(), 1, Some(2_300)),
        ]
    );

    database.close().await;
}

#[tokio::test]
async fn unavailable_metadata_image_batch_uses_one_statement_and_preserves_attempt_count() {
    type MetadataImageAttemptRow = (String, String, i64, i64, Option<i64>, Option<String>, i64);

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('image-attempt-batch-item', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    sqlx::query(
        "INSERT INTO metadata_image_attempts (
                item_id, image_type, candidate_key, status, attempt_count,
                last_attempt_at, updated_at
             ) VALUES (
                'image-attempt-batch-item', 'POSTER', 'tmdb:1:POSTER',
                'FAILED', 5, 10, 10
             )",
    )
    .execute(database.pool())
    .await
    .expect("existing image attempt");
    let unavailable = [
        MetadataImageUnavailable {
            image_type: "POSTER",
            candidate_key: "tmdb:1:POSTER",
        },
        MetadataImageUnavailable {
            image_type: "FANART",
            candidate_key: "tmdb:1:FANART",
        },
    ];

    database.reset_query_count();
    database
        .mark_metadata_images_unavailable("image-attempt-batch-item", &unavailable, 200)
        .await
        .expect("mark missing images in one batch");
    assert_eq!(database.query_count(), 1);

    let rows: Vec<MetadataImageAttemptRow> = sqlx::query_as(
        "SELECT image_type, status, attempt_count, last_attempt_at,
                    next_retry_at, error_code, updated_at
             FROM metadata_image_attempts
             WHERE item_id = 'image-attempt-batch-item'
             ORDER BY image_type",
    )
    .fetch_all(database.pool())
    .await
    .expect("updated image attempts");
    assert_eq!(
        rows,
        vec![
            (
                "FANART".to_owned(),
                "UNAVAILABLE".to_owned(),
                1,
                200,
                None,
                Some("NO_IMAGE".to_owned()),
                200,
            ),
            (
                "POSTER".to_owned(),
                "UNAVAILABLE".to_owned(),
                5,
                200,
                None,
                Some("NO_IMAGE".to_owned()),
                200,
            ),
        ]
    );

    database.reset_query_count();
    database
        .mark_metadata_images_unavailable("image-attempt-batch-item", &[], 300)
        .await
        .expect("empty image batch is a no-op");
    assert_eq!(database.query_count(), 0);

    database.close().await;
}

#[tokio::test]
async fn metadata_job_list_counts_only_pending_items_on_the_requested_page() {
    sqlx::any::install_default_drivers();
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(
            AnyConnectOptions::from_str("sqlite://?mode=memory").expect("in-memory SQLite options"),
        )
        .await
        .expect("in-memory SQLite connection");
    sqlx::query(
        "CREATE TABLE metadata_reidentify_jobs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                processed_count INTEGER NOT NULL,
                total_count INTEGER NOT NULL,
                error TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                started_at INTEGER,
                finished_at INTEGER,
                mode TEXT NOT NULL,
                cancel_requested INTEGER NOT NULL,
                library_id TEXT,
                job_scope TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata jobs table");
    sqlx::query(
        "CREATE TABLE metadata_reidentify_job_items (
                job_id TEXT NOT NULL,
                item_id TEXT NOT NULL,
                status TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata job items table");
    sqlx::query(
        "CREATE TABLE metadata_candidates (
                item_id TEXT NOT NULL,
                status TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata candidates table");
    sqlx::query(
        "CREATE INDEX idx_metadata_reidentify_items_status
             ON metadata_reidentify_job_items(job_id, status, item_id)",
    )
    .execute(&pool)
    .await
    .expect("create metadata job item index");
    sqlx::query(
        "CREATE INDEX idx_metadata_candidates_item
             ON metadata_candidates(item_id, status)",
    )
    .execute(&pool)
    .await
    .expect("create metadata candidate index");
    for (id, created_at) in [("older", 1_i64), ("newer", 2_i64)] {
        sqlx::query(
            "INSERT INTO metadata_reidentify_jobs (
                    id, status, processed_count, total_count, error,
                    created_at, updated_at, started_at, finished_at, mode,
                    cancel_requested, library_id, job_scope
                 ) VALUES (?, 'QUEUED', 0, 2, NULL, ?, ?, NULL, NULL,
                           'REIDENTIFY', 0, NULL, 'ITEMS')",
        )
        .bind(id)
        .bind(created_at)
        .bind(created_at)
        .execute(&pool)
        .await
        .expect("insert metadata job");
    }
    for (job_id, item_id) in [("older", "old-item"), ("newer", "new-item")] {
        sqlx::query(
            "INSERT INTO metadata_reidentify_job_items (job_id, item_id, status)
                 VALUES (?, ?, 'PENDING')",
        )
        .bind(job_id)
        .bind(item_id)
        .execute(&pool)
        .await
        .expect("insert metadata job item");
    }
    sqlx::query(
        "INSERT INTO metadata_candidates (item_id, status)
             VALUES ('old-item', 'PENDING'), ('new-item', 'PENDING'),
                    ('new-item', 'PENDING')",
    )
    .execute(&pool)
    .await
    .expect("insert metadata candidates");

    let database = Database {
        pool,
        log_store: LogStore::new(Path::new("unused-metadata-summary-test")),
        pool_max_connections: 1,
        path: PathBuf::from("metadata-summary-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
        recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
            RecommendationRatingMedianCache::default(),
        )),
        query_count: Arc::new(AtomicUsize::new(0)),
    };
    let jobs = database
        .list_metadata_reidentify_jobs(None, 0, 1)
        .await
        .expect("list metadata jobs");

    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, "newer");
    assert_eq!(jobs[0].pending_count, 1);

    let plan = sqlx::query(
        "EXPLAIN QUERY PLAN
             WITH selected_jobs AS (
                 SELECT id
                 FROM metadata_reidentify_jobs
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1 OFFSET 0
             ), pending_counts AS (
                 SELECT job_items.job_id, COUNT(DISTINCT candidates.item_id) AS pending_count
                 FROM metadata_reidentify_job_items job_items
                 JOIN selected_jobs ON selected_jobs.id = job_items.job_id
                 JOIN metadata_candidates candidates
                   ON candidates.item_id = job_items.item_id
                  AND candidates.status = 'PENDING'
                 GROUP BY job_items.job_id
             )
             SELECT selected_jobs.id, pending_counts.pending_count
             FROM selected_jobs
             LEFT JOIN pending_counts ON pending_counts.job_id = selected_jobs.id",
    )
    .fetch_all(database.pool())
    .await
    .expect("explain metadata summary query");
    let plan_details = plan
        .iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>();
    assert!(
        plan_details
            .iter()
            .any(|detail| detail
                .contains("USING COVERING INDEX idx_metadata_reidentify_items_status")),
        "metadata summary should seek selected job items by job_id: {plan_details:?}"
    );
    assert!(
        plan_details
            .iter()
            .any(|detail| detail.contains("USING COVERING INDEX idx_metadata_candidates_item")),
        "metadata summary should seek candidates by item_id: {plan_details:?}"
    );
    assert!(
        plan_details
            .iter()
            .all(|detail| !detail.contains("SCAN metadata_reidentify_job_items")),
        "metadata summary must not scan all job items: {plan_details:?}"
    );
    assert!(
        plan_details
            .iter()
            .all(|detail| !detail.contains("SCAN metadata_candidates")),
        "metadata summary must not scan all candidates: {plan_details:?}"
    );
    database.close().await;
}

#[tokio::test]
async fn person_credits_migration_creates_the_index_table() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'person_credits'",
    )
    .fetch_one(database.pool())
    .await
    .expect("person credits table");
    assert_eq!(table_count, 1);

    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-credits', ?, 'MOVIE', 'Credits', 'credits', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("media item");
    database
        .replace_person_credits(
            "item-credits",
            &[
                NewPersonCredit {
                    person_id: "1".to_owned(),
                    lux_person_id: Some("lux-000001".to_owned()),
                    person_type: "Actor".to_owned(),
                    person_name: "演员甲".to_owned(),
                    provider: "tmdb".to_owned(),
                    role: "角色甲".to_owned(),
                    sort_order: 0,
                    biography: None,
                    birthday: None,
                    deathday: None,
                    known_for_department: None,
                    place_of_birth: None,
                    provider_ids: BTreeMap::new(),
                    genres: Vec::new(),
                    tags: Vec::new(),
                    production_locations: Vec::new(),
                    premiere_date: None,
                    production_year: None,
                    taglines: Vec::new(),
                },
                NewPersonCredit {
                    person_id: "1".to_owned(),
                    lux_person_id: Some("lux-000001".to_owned()),
                    person_type: "Actor".to_owned(),
                    person_name: "重复演员甲".to_owned(),
                    provider: "tmdb".to_owned(),
                    role: "角色甲".to_owned(),
                    sort_order: 0,
                    biography: None,
                    birthday: None,
                    deathday: None,
                    known_for_department: None,
                    place_of_birth: None,
                    provider_ids: BTreeMap::new(),
                    genres: Vec::new(),
                    tags: Vec::new(),
                    production_locations: Vec::new(),
                    premiere_date: None,
                    production_year: None,
                    taglines: Vec::new(),
                },
                NewPersonCredit {
                    person_id: "9".to_owned(),
                    lux_person_id: Some("lux-000001".to_owned()),
                    person_type: "Actor".to_owned(),
                    person_name: "演员甲".to_owned(),
                    provider: "douban".to_owned(),
                    role: "角色甲".to_owned(),
                    sort_order: 0,
                    biography: None,
                    birthday: None,
                    deathday: None,
                    known_for_department: None,
                    place_of_birth: None,
                    provider_ids: BTreeMap::new(),
                    genres: Vec::new(),
                    tags: Vec::new(),
                    production_locations: Vec::new(),
                    premiere_date: None,
                    production_year: None,
                    taglines: Vec::new(),
                },
                NewPersonCredit {
                    person_id: "2".to_owned(),
                    lux_person_id: None,
                    person_type: "Actor".to_owned(),
                    person_name: "演员乙".to_owned(),
                    provider: "tmdb".to_owned(),
                    role: "角色乙".to_owned(),
                    sort_order: 1,
                    biography: None,
                    birthday: None,
                    deathday: None,
                    known_for_department: None,
                    place_of_birth: None,
                    provider_ids: BTreeMap::new(),
                    genres: Vec::new(),
                    tags: Vec::new(),
                    production_locations: Vec::new(),
                    premiere_date: None,
                    production_year: None,
                    taglines: Vec::new(),
                },
            ],
        )
        .await
        .expect("person credits");
    let stored_credit_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_credits WHERE item_id = 'item-credits'")
            .fetch_one(database.pool())
            .await
            .expect("stored person credit count");
    assert_eq!(stored_credit_count, 3);
    let stored_name: String = sqlx::query_scalar(
        "SELECT person_name FROM person_credits
             WHERE item_id = 'item-credits' AND person_id = '1' AND provider = 'tmdb'",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored first duplicate");
    assert_eq!(stored_name, "演员甲");
    let (credits, total) = database
        .list_person_credits_for_library(
            &library_id,
            "Actor",
            PersonListOptions {
                recursive: true,
                sort_by: PersonSort::Name,
                descending: false,
                offset: 0,
                limit: 10,
            },
        )
        .await
        .expect("list person credits");
    assert_eq!(total, 2);
    assert_eq!(credits.len(), 2);
    let names = credits
        .iter()
        .map(|credit| credit.person_name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"演员甲"));
    assert!(names.contains(&"演员乙"));
    assert_eq!(
        credits
            .iter()
            .filter(|credit| credit.lux_person_id.as_deref() == Some("lux-000001"))
            .count(),
        1
    );
}

#[tokio::test]
async fn person_credit_replacement_batches_large_credit_sets() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('item-large-credits', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    let credits = (0..41)
        .map(|index| NewPersonCredit {
            person_id: format!("person-{index}"),
            lux_person_id: None,
            person_type: "Actor".to_owned(),
            person_name: format!("演员{index}"),
            provider: "tmdb".to_owned(),
            role: format!("角色{index}"),
            sort_order: index,
            biography: None,
            birthday: None,
            deathday: None,
            known_for_department: None,
            place_of_birth: None,
            provider_ids: BTreeMap::new(),
            genres: Vec::new(),
            tags: Vec::new(),
            production_locations: Vec::new(),
            premiere_date: None,
            production_year: None,
            taglines: Vec::new(),
        })
        .collect::<Vec<_>>();
    database.reset_query_count();
    database
        .replace_person_credits("item-large-credits", &credits)
        .await
        .expect("large credit replacement");
    assert_eq!(database.query_count(), 4);
    let stored_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM person_credits WHERE item_id = 'item-large-credits'",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored credit count");
    assert_eq!(stored_count, 41);
}

#[tokio::test]
async fn person_credit_list_uses_one_consistent_representative_row() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    for (item_id, added_at) in [("item-a", 200_i64), ("item-b", 100_i64)] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, added_at
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .bind(added_at)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    database
        .replace_person_credits(
            "item-a",
            &[NewPersonCredit {
                person_id: "1".to_owned(),
                lux_person_id: Some("lux-000001".to_owned()),
                person_type: "Actor".to_owned(),
                person_name: "同一演员".to_owned(),
                provider: "tmdb".to_owned(),
                role: "alpha-role".to_owned(),
                sort_order: 0,
                biography: Some("z-biography".to_owned()),
                birthday: None,
                deathday: None,
                known_for_department: None,
                place_of_birth: None,
                provider_ids: BTreeMap::new(),
                genres: Vec::new(),
                tags: Vec::new(),
                production_locations: Vec::new(),
                premiere_date: None,
                production_year: None,
                taglines: Vec::new(),
            }],
        )
        .await
        .expect("first person credit");
    database
        .replace_person_credits(
            "item-b",
            &[NewPersonCredit {
                person_id: "9".to_owned(),
                lux_person_id: Some("lux-000001".to_owned()),
                person_type: "Actor".to_owned(),
                person_name: "同一演员".to_owned(),
                provider: "douban".to_owned(),
                role: "zeta-role".to_owned(),
                sort_order: 0,
                biography: Some("a-biography".to_owned()),
                birthday: None,
                deathday: None,
                known_for_department: None,
                place_of_birth: None,
                provider_ids: BTreeMap::new(),
                genres: Vec::new(),
                tags: Vec::new(),
                production_locations: Vec::new(),
                premiere_date: None,
                production_year: None,
                taglines: Vec::new(),
            }],
        )
        .await
        .expect("second person credit");

    let (credits, total) = database
        .list_person_credits_for_library(
            &library_id,
            "Actor",
            PersonListOptions {
                recursive: true,
                sort_by: PersonSort::Name,
                descending: false,
                offset: 0,
                limit: 10,
            },
        )
        .await
        .expect("list person credits");

    assert_eq!(total, 1);
    let credit = credits.first().expect("representative person credit");
    assert_eq!(credit.provider, "tmdb");
    assert_eq!(credit.person_id, "1");
    assert_eq!(credit.role, "alpha-role");
    assert_eq!(credit.biography.as_deref(), Some("z-biography"));
    assert_eq!(credit.date_created, 100);
}

#[tokio::test]
async fn canonical_people_migration_creates_recoverable_identity_tables() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    for table in ["people", "person_identities", "person_id_sequence"] {
        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(table)
        .fetch_one(database.pool())
        .await
        .expect("canonical people table");
        assert_eq!(table_count, 1, "missing canonical people table {table}");
    }
}

#[tokio::test]
async fn canonical_people_reuse_one_lux_id_across_provider_identities() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let first = database
        .resolve_or_create_canonical_person(
            "华晨宇",
            "tmdb",
            "57975",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("first canonical person");
    assert_eq!(first.id, "lux-000001");

    let second = database
        .attach_canonical_person_identity(
            &first.id,
            "douban",
            "1313123",
            "MEDIA_BRIDGE",
            Some(0.98),
            r#"{"source":"same-media"}"#,
        )
        .await
        .expect("second canonical identity");
    assert_eq!(second.id, first.id);

    let repeated = database
        .resolve_or_create_canonical_person(
            "华晨宇",
            "tmdb",
            "57975",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("repeated canonical identity");
    assert_eq!(repeated.id, first.id);

    let identity_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM person_identities")
        .fetch_one(database.pool())
        .await
        .expect("identity count");
    assert_eq!(identity_count, 2);
}

#[tokio::test]
async fn canonical_people_batch_identity_lookup_returns_all_matches_in_one_query() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let first = database
        .resolve_or_create_canonical_person(
            "华晨宇",
            "tmdb",
            "57975",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("first canonical person");
    let second = database
        .resolve_or_create_canonical_person(
            "另一位演员",
            "tmdb",
            "57976",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("second canonical person");

    database.reset_query_count();
    let matches = database
        .find_canonical_people_by_identities(&[
            ("tmdb".to_owned(), "57975".to_owned()),
            ("tmdb".to_owned(), "57976".to_owned()),
            ("tmdb".to_owned(), "missing".to_owned()),
        ])
        .await
        .expect("batch identity lookup");

    assert_eq!(database.query_count(), 1);
    assert_eq!(
        matches,
        vec![
            ("tmdb".to_owned(), "57975".to_owned(), first.id),
            ("tmdb".to_owned(), "57976".to_owned(), second.id),
        ]
    );
}

#[tokio::test]
async fn canonical_people_batch_identity_attach_is_atomic() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let owner = database
        .resolve_or_create_canonical_person(
            "人物甲",
            "tmdb",
            "57975",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("owner");
    database.reset_query_count();
    database
        .attach_canonical_person_identities(
            &owner.id,
            &[
                ("douban".to_owned(), "1313123".to_owned()),
                ("imdb".to_owned(), "nm0000001".to_owned()),
            ],
            "SAME_SOURCE_ID_SET",
            Some(0.99),
            r#"{"method":"test"}"#,
        )
        .await
        .expect("batch attach");
    assert_eq!(database.query_count(), 3);
    let identity_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_identities WHERE person_id = ?")
            .bind(&owner.id)
            .fetch_one(database.pool())
            .await
            .expect("identity count");
    assert_eq!(identity_count, 3);

    let other = database
        .resolve_or_create_canonical_person(
            "人物乙",
            "tmdb",
            "57976",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("other owner");
    let result = database
        .attach_canonical_person_identities(
            &owner.id,
            &[
                ("douban".to_owned(), "new-id".to_owned()),
                ("tmdb".to_owned(), "57976".to_owned()),
            ],
            "SAME_SOURCE_ID_SET",
            Some(0.99),
            r#"{"method":"conflict"}"#,
        )
        .await;
    assert!(result.is_err());
    let new_identity_owner: Option<String> = sqlx::query_scalar(
        "SELECT person_id FROM person_identities
         WHERE provider = 'douban' AND provider_id = 'new-id'",
    )
    .fetch_optional(database.pool())
    .await
    .expect("new identity lookup");
    assert!(new_identity_owner.is_none());
    assert_eq!(other.id, "lux-000002");
}

#[tokio::test]
async fn restoring_a_manifest_rejects_a_provider_identity_owned_by_another_person() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    database
        .resolve_or_create_canonical_person(
            "华晨宇",
            "tmdb",
            "57975",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("first canonical person");

    let error = database
        .restore_canonical_person(
            "lux-000002",
            "另一位演员",
            &[("douban", "new-id"), ("tmdb", "57975")],
        )
        .await
        .expect_err("conflicting manifest must be rejected");
    assert!(matches!(error, StorageError::Conflict(_)));
    let new_identity_owner: Option<String> = sqlx::query_scalar(
        "SELECT person_id FROM person_identities
         WHERE provider = 'douban' AND provider_id = 'new-id'",
    )
    .fetch_optional(database.pool())
    .await
    .expect("new identity lookup");
    assert!(new_identity_owner.is_none());
    let restored_person: Option<String> =
        sqlx::query_scalar("SELECT id FROM people WHERE id = 'lux-000002'")
            .fetch_optional(database.pool())
            .await
            .expect("restored person lookup");
    assert!(restored_person.is_none());
    assert_eq!(
        database
            .find_canonical_person_by_identity("tmdb", "57975")
            .await
            .expect("identity lookup")
            .expect("existing identity")
            .id,
        "lux-000001"
    );
}

#[tokio::test]
async fn restoring_a_person_manifest_batches_identity_queries() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let identities = [
        ("tmdb", "57975"),
        ("douban", "1313123"),
        ("imdb", "nm0000001"),
        ("anilist", "42"),
    ];

    database.reset_query_count();
    database
        .restore_canonical_person("lux-000001", "华晨宇", &identities)
        .await
        .expect("restore person");

    assert_eq!(database.query_count(), 5);
    let identity_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_identities WHERE person_id = 'lux-000001'")
            .fetch_one(database.pool())
            .await
            .expect("identity count");
    assert_eq!(identity_count, identities.len() as i64);
}

#[tokio::test]
async fn restoring_a_person_manifest_chunks_large_identity_sets() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let owned_identities = (0..205)
        .map(|index| ("tmdb".to_owned(), format!("id-{index}")))
        .collect::<Vec<_>>();
    let identities = owned_identities
        .iter()
        .map(|(provider, provider_id)| (provider.as_str(), provider_id.as_str()))
        .collect::<Vec<_>>();

    database.reset_query_count();
    database
        .restore_canonical_person("lux-000001", "Person", &identities)
        .await
        .expect("restore person with many identities");

    assert_eq!(database.query_count(), 9);
    let identity_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_identities WHERE person_id = 'lux-000001'")
            .fetch_one(database.pool())
            .await
            .expect("identity count");
    assert_eq!(identity_count, owned_identities.len() as i64);
}

#[tokio::test]
async fn person_match_candidates_are_persistent_and_idempotent() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-1', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    let first_id = database
        .enqueue_person_match_candidate(
            "item-1",
            "douban",
            "1313123",
            r#"["lux-000001","lux-000002"]"#,
            Some(0.62),
            r#"{"method":"same-media-ambiguous"}"#,
        )
        .await
        .expect("first candidate");
    let second_id = database
        .enqueue_person_match_candidate(
            "item-1",
            "douban",
            "1313123",
            r#"["lux-000002","lux-000001"]"#,
            Some(0.65),
            r#"{"method":"same-media-ambiguous","retry":true}"#,
        )
        .await
        .expect("idempotent candidate update");
    assert_eq!(first_id, second_id);

    let (count, status, score): (i64, String, f64) = sqlx::query_as(
        "SELECT COUNT(*), MIN(status), MAX(score)
             FROM person_match_candidates
             WHERE item_id = 'item-1' AND provider = 'douban' AND provider_id = '1313123'",
    )
    .fetch_one(database.pool())
    .await
    .expect("candidate row");
    assert_eq!(count, 1);
    assert_eq!(status, "PENDING");
    assert_eq!(score, 0.65);

    sqlx::query("UPDATE person_match_candidates SET status = 'CONFIRMED' WHERE id = ?")
        .bind(&first_id)
        .execute(database.pool())
        .await
        .expect("mark candidate decided");
    database
        .enqueue_person_match_candidate(
            "item-1",
            "douban",
            "1313123",
            r#"["lux-000002"]"#,
            Some(0.9),
            r#"{"method":"retry"}"#,
        )
        .await
        .expect("retry decided candidate");
    let preserved_status: String =
        sqlx::query_scalar("SELECT status FROM person_match_candidates WHERE id = ?")
            .bind(&first_id)
            .fetch_one(database.pool())
            .await
            .expect("preserved candidate status");
    assert_eq!(preserved_status, "CONFIRMED");
}

#[tokio::test]
async fn confirming_person_match_moves_identity_and_credit_atomically() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-confirm', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("media item");
    let old = database
        .resolve_or_create_canonical_person(
            "旧人物",
            "douban",
            "1313123",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"douban"}"#,
        )
        .await
        .expect("old person");
    let target = database
        .resolve_or_create_canonical_person(
            "目标人物",
            "tmdb",
            "57975",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("target person");
    database
        .replace_person_credits(
            "item-confirm",
            &[NewPersonCredit {
                person_id: "1313123".to_owned(),
                lux_person_id: Some(old.id.clone()),
                person_type: "Actor".to_owned(),
                person_name: "旧人物".to_owned(),
                provider: "douban".to_owned(),
                role: "角色".to_owned(),
                sort_order: 0,
                biography: None,
                birthday: None,
                deathday: None,
                known_for_department: None,
                place_of_birth: None,
                provider_ids: BTreeMap::new(),
                genres: Vec::new(),
                tags: Vec::new(),
                production_locations: Vec::new(),
                premiere_date: None,
                production_year: None,
                taglines: Vec::new(),
            }],
        )
        .await
        .expect("credit");
    database
        .enqueue_person_match_candidate(
            "item-confirm",
            "douban",
            "1313123",
            &format!("[\"{}\"]", target.id),
            Some(0.9),
            r#"{"method":"same-media"}"#,
        )
        .await
        .expect("candidate");
    let candidate_id: String = sqlx::query_scalar(
        "SELECT id FROM person_match_candidates
             WHERE item_id = 'item-confirm'",
    )
    .fetch_one(database.pool())
    .await
    .expect("candidate id");

    let moved = database
        .confirm_person_match_candidate(&candidate_id, &target.id, r#"{"method":"manual-confirm"}"#)
        .await
        .expect("confirm candidate");
    assert_eq!(moved.previous_person_id.as_deref(), Some(old.id.as_str()));
    assert_eq!(
        database
            .find_canonical_person_by_identity("douban", "1313123")
            .await
            .expect("identity lookup")
            .expect("moved identity")
            .id,
        target.id
    );
    let lux_id: String = sqlx::query_scalar(
        "SELECT lux_person_id FROM person_credits
             WHERE item_id = 'item-confirm'",
    )
    .fetch_one(database.pool())
    .await
    .expect("credit lux id");
    assert_eq!(lux_id, target.id);
    let status: String = sqlx::query_scalar(
        "SELECT status FROM person_match_candidates
             WHERE id = ?",
    )
    .bind(&candidate_id)
    .fetch_one(database.pool())
    .await
    .expect("candidate status");
    assert_eq!(status, "CONFIRMED");

    database
        .undo_person_match_candidate(&candidate_id, r#"{"reason":"test-undo"}"#)
        .await
        .expect("undo candidate");
    assert_eq!(
        database
            .find_canonical_person_by_identity("douban", "1313123")
            .await
            .expect("identity lookup after undo")
            .expect("restored identity")
            .id,
        old.id
    );
    let restored_lux_id: String = sqlx::query_scalar(
        "SELECT lux_person_id FROM person_credits
             WHERE item_id = 'item-confirm'",
    )
    .fetch_one(database.pool())
    .await
    .expect("restored credit lux id");
    assert_eq!(restored_lux_id, old.id);
    let undone_status: String =
        sqlx::query_scalar("SELECT status FROM person_match_candidates WHERE id = ?")
            .bind(candidate_id)
            .fetch_one(database.pool())
            .await
            .expect("undone candidate status");
    assert_eq!(undone_status, "REJECTED");
}

#[tokio::test]
async fn splitting_person_identity_allocates_a_new_lux_person_and_repoints_credits() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-split', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    let old = database
        .resolve_or_create_canonical_person(
            "人物甲",
            "douban",
            "1313123",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"douban"}"#,
        )
        .await
        .expect("person");
    database
        .replace_person_credits(
            "item-split",
            &[NewPersonCredit {
                person_id: "1313123".to_owned(),
                lux_person_id: Some(old.id.clone()),
                person_type: "Actor".to_owned(),
                person_name: "人物甲".to_owned(),
                provider: "douban".to_owned(),
                role: "角色".to_owned(),
                sort_order: 0,
                biography: None,
                birthday: None,
                deathday: None,
                known_for_department: None,
                place_of_birth: None,
                provider_ids: BTreeMap::new(),
                genres: Vec::new(),
                tags: Vec::new(),
                production_locations: Vec::new(),
                premiere_date: None,
                production_year: None,
                taglines: Vec::new(),
            }],
        )
        .await
        .expect("credit");
    let split = database
        .split_canonical_person_identity(
            &old.id,
            "douban",
            "1313123",
            "人物乙",
            r#"{"method":"undo-merge"}"#,
        )
        .await
        .expect("split");
    assert_ne!(split.id, old.id);
    assert_eq!(split.id, "lux-000002");
    assert_eq!(
        database
            .find_canonical_person_by_identity("douban", "1313123")
            .await
            .expect("identity")
            .expect("new owner")
            .id,
        split.id
    );
    let lux_id: String =
        sqlx::query_scalar("SELECT lux_person_id FROM person_credits WHERE item_id = 'item-split'")
            .fetch_one(database.pool())
            .await
            .expect("credit owner");
    assert_eq!(lux_id, split.id);
}

#[tokio::test]
async fn catalog_tie_breakers_use_displayed_title_when_sort_key_is_stale() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, premiere_date,
                rating, identification_status, added_at, has_available_source
             ) VALUES
                ('item-alpha', ?, 'MOVIE', 'Alpha', 'zzz', '2020-01-01', 8.0, 'LOCAL_CONFIRMED', 100, 1),
                ('item-beta', ?, 'MOVIE', 'Beta', 'aaa', '2020-01-01', 8.0, 'LOCAL_CONFIRMED', 100, 1)",
        )
        .bind(&library_id)
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("media items");

    let library_ids = vec![library_id];
    let item_types = vec!["MOVIE".to_owned()];
    let empty = Vec::new();
    let empty_years = Vec::<i64>::new();
    for (sort_by, descending) in [
        (CatalogSort::DateCreated, false),
        (CatalogSort::DateCreated, true),
        (CatalogSort::PremiereDate, false),
        (CatalogSort::PremiereDate, true),
        (CatalogSort::Rating, false),
        (CatalogSort::Rating, true),
    ] {
        let filter = CatalogFilterQuery {
            library_ids: &library_ids,
            user_id: "test-user",
            item_types: &item_types,
            excluded_item_types: &empty,
            item_ids: None,
            person_id: None,
            media_source_ids: None,
            provider_id_equals: None,
            years: &empty_years,
            is_played: None,
            is_favorite: None,
            min_date_last_saved: None,
            metadata_pending: false,
            parent_id_scope: None,
            sort_by,
            descending,
            offset: 0,
            limit: 10,
        };
        let (rows, total) = database
            .list_filtered_catalog_rows(&filter)
            .await
            .expect("catalog rows");
        let titles = rows.into_iter().map(|row| row.title).collect::<Vec<_>>();
        let expected = vec!["Alpha", "Beta"];
        assert_eq!(total, 2);
        assert_eq!(titles, expected, "descending={descending}");
    }
}

#[tokio::test]
async fn catalog_filter_parent_scope_applies_before_pagination() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Home videos", LibraryKind::HomeVideos, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, parent_id, title, sort_title,
            identification_status, has_available_source
         ) VALUES
            ('root-video', ?, 'VIDEO', NULL, 'Root video', 'root video', 'LOCAL_CONFIRMED', 1),
            ('root-folder', ?, 'FOLDER', ?, 'Root folder', 'root folder', 'LOCAL_CONFIRMED', 0),
            ('nested-video', ?, 'VIDEO', 'root-folder', 'Nested video', 'nested video', 'LOCAL_CONFIRMED', 1),
            ('nested-folder', ?, 'FOLDER', 'root-folder', 'Nested folder', 'nested folder', 'LOCAL_CONFIRMED', 0),
            ('deep-video', ?, 'VIDEO', 'nested-folder', 'Deep video', 'deep video', 'LOCAL_CONFIRMED', 1)",
    )
    .bind(&library_id)
    .bind(&library_id)
    .bind(&library_id)
    .bind(&library_id)
    .bind(&library_id)
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("home video entries");

    let library_ids = vec![library_id];
    let item_types = vec!["FOLDER".to_owned(), "VIDEO".to_owned()];
    let empty = Vec::new();
    let empty_years = Vec::<i64>::new();
    let mut filter = CatalogFilterQuery {
        library_ids: &library_ids,
        user_id: "test-user",
        item_types: &item_types,
        excluded_item_types: &empty,
        item_ids: None,
        person_id: None,
        media_source_ids: None,
        provider_id_equals: None,
        years: &empty_years,
        is_played: None,
        is_favorite: None,
        min_date_last_saved: None,
        metadata_pending: false,
        sort_by: CatalogSort::Name,
        descending: false,
        offset: 0,
        limit: 1,
        parent_id_scope: Some(None),
    };

    let (root_page, root_total) = database
        .list_filtered_catalog_rows(&filter)
        .await
        .expect("root catalog page");
    assert_eq!(root_total, 2);
    assert_eq!(root_page.len(), 1);
    assert_eq!(root_page[0].title, "Root folder");

    filter.offset = 1;
    let (second_root_page, _) = database
        .list_filtered_catalog_rows(&filter)
        .await
        .expect("second root catalog page");
    assert_eq!(second_root_page.len(), 1);
    assert_eq!(second_root_page[0].title, "Root video");

    filter.parent_id_scope = Some(Some("root-folder"));
    filter.offset = 0;
    filter.limit = 10;
    let (nested_page, nested_total) = database
        .list_filtered_catalog_rows(&filter)
        .await
        .expect("nested catalog page");
    assert_eq!(nested_total, 2);
    assert_eq!(nested_page.len(), 2);
    assert!(
        nested_page
            .iter()
            .all(|item| item.parent_id.as_deref() == Some("root-folder"))
    );

    filter.parent_id_scope = None;
    let (_, unfiltered_total) = database
        .list_filtered_catalog_rows(&filter)
        .await
        .expect("unfiltered catalog page");
    assert_eq!(unfiltered_total, 5);
}

#[tokio::test]
async fn catalog_root_counts_are_grouped_by_library_in_one_query() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let first = libraries
        .create_library("First", LibraryKind::Mixed, false)
        .await
        .expect("first library");
    let second = libraries
        .create_library("Second", LibraryKind::Mixed, false)
        .await
        .expect("second library");
    let first_id = first.id.to_string();
    let second_id = second.id.to_string();
    sqlx::query(
        "INSERT INTO media_items
         (id, library_id, item_type, title, sort_title, identification_status, has_available_source)
         VALUES
           ('root-count-movie-1', ?, 'MOVIE', 'Movie 1', 'movie 1', 'LOCAL_CONFIRMED', 1),
           ('root-count-series-1', ?, 'SERIES', 'Series 1', 'series 1', 'LOCAL_CONFIRMED', 1),
           ('root-count-movie-2', ?, 'MOVIE', 'Movie 2', 'movie 2', 'LOCAL_CONFIRMED', 1),
           ('root-count-series-2', ?, 'SERIES', 'Series 2', 'series 2', 'LOCAL_CONFIRMED', 1),
           ('root-count-folder', ?, 'FOLDER', 'Folder', 'folder', 'LOCAL_CONFIRMED', 1),
           ('root-count-removed', ?, 'MOVIE', 'Removed', 'removed', 'LOCAL_CONFIRMED', 1)",
    )
    .bind(&first_id)
    .bind(&first_id)
    .bind(&second_id)
    .bind(&second_id)
    .bind(&first_id)
    .bind(&first_id)
    .execute(database.pool())
    .await
    .expect("media items");
    sqlx::query("UPDATE media_items SET removed_at = 1 WHERE id = 'root-count-removed'")
        .execute(database.pool())
        .await
        .expect("removed item");

    database.reset_query_count();
    let counts = database
        .count_catalog_root_items_by_library(&[first_id.clone(), second_id.clone()])
        .await
        .expect("root counts");

    assert_eq!(database.query_count(), 1);
    assert_eq!(
        counts.get(&first_id).map(|value| value.movie_count),
        Some(1)
    );
    assert_eq!(
        counts.get(&first_id).map(|value| value.series_count),
        Some(1)
    );
    assert_eq!(
        counts.get(&second_id).map(|value| value.movie_count),
        Some(1)
    );
    assert_eq!(
        counts.get(&second_id).map(|value| value.series_count),
        Some(1)
    );
}

#[tokio::test]
async fn catalog_premiere_date_sort_falls_back_to_production_year() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, production_year,
                identification_status, added_at, has_available_source
             ) VALUES
                ('item-newer', ?, 'MOVIE', 'A Newer Movie', 'a newer movie', 2025, 'LOCAL_CONFIRMED', 100, 1),
                ('item-older', ?, 'MOVIE', 'B Older Movie', 'b older movie', 2010, 'LOCAL_CONFIRMED', 100, 1)",
    )
    .bind(&library_id)
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("media items");

    let library_ids = vec![library_id];
    let item_types = vec!["MOVIE".to_owned()];
    let empty = Vec::new();
    let empty_years = Vec::<i64>::new();
    for (descending, expected) in [
        (false, vec!["B Older Movie", "A Newer Movie"]),
        (true, vec!["A Newer Movie", "B Older Movie"]),
    ] {
        let filter = CatalogFilterQuery {
            library_ids: &library_ids,
            user_id: "test-user",
            item_types: &item_types,
            excluded_item_types: &empty,
            item_ids: None,
            person_id: None,
            media_source_ids: None,
            provider_id_equals: None,
            years: &empty_years,
            is_played: None,
            is_favorite: None,
            min_date_last_saved: None,
            metadata_pending: false,
            parent_id_scope: None,
            sort_by: CatalogSort::PremiereDate,
            descending,
            offset: 0,
            limit: 10,
        };
        let (rows, total) = database
            .list_filtered_catalog_rows(&filter)
            .await
            .expect("catalog rows");
        let titles = rows.into_iter().map(|row| row.title).collect::<Vec<_>>();
        assert_eq!(total, 2);
        assert_eq!(titles, expected, "descending={descending}");
    }
}

#[tokio::test]
async fn media_source_library_page_respects_limit_and_offset() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    tokio::fs::write(root_path.join("First.Movie.2024.mkv"), b"first")
        .await
        .expect("first movie");
    tokio::fs::write(root_path.join("Second.Movie.2024.mkv"), b"second")
        .await
        .expect("second movie");
    tokio::fs::write(
        root_path.join("First.Remote.2024.strm"),
        b"https://media.example.invalid/first.mkv",
    )
    .await
    .expect("first STRM");
    tokio::fs::write(
        root_path.join("Second.Remote.2024.strm"),
        b"https://media.example.invalid/second.mkv",
    )
    .await
    .expect("second STRM");
    libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
        .await
        .expect("library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("scan");

    let first_page = database
        .list_media_sources_for_library_page(&library.id.to_string(), 1, 0)
        .await
        .expect("first page");
    let second_page = database
        .list_media_sources_for_library_page(&library.id.to_string(), 1, 1)
        .await
        .expect("second page");

    assert_eq!(first_page.len(), 1);
    assert_eq!(second_page.len(), 1);
    assert_ne!(first_page[0].source_id, second_page[0].source_id);
    let existing_entries = database
        .list_filesystem_entries_for_paths(
            &database
                .list_library_roots(&library.id.to_string())
                .await
                .expect("roots")
                .into_iter()
                .next()
                .expect("root")
                .id,
            &["First.Movie.2024.mkv".to_owned()],
        )
        .await
        .expect("existing entries");
    assert_eq!(existing_entries.len(), 1);
    assert_eq!(
        database
            .list_local_thumbnail_sources_for_library_page(&library.id.to_string(), 1, 0,)
            .await
            .expect("thumbnail page")
            .len(),
        1
    );
    assert_eq!(
        database
            .list_movie_metadata_sources_page(&library.id.to_string(), 1, 1)
            .await
            .expect("metadata page")
            .len(),
        1
    );
    let first_strm_page = database
        .list_strm_media_sources_for_library_page(&library.id.to_string(), None, 1)
        .await
        .expect("first STRM page");
    let second_strm_page = database
        .list_strm_media_sources_for_library_page(
            &library.id.to_string(),
            first_strm_page
                .first()
                .map(|source| source.source_id.as_str()),
            1,
        )
        .await
        .expect("second STRM page");
    let final_strm_page = database
        .list_strm_media_sources_for_library_page(
            &library.id.to_string(),
            second_strm_page
                .first()
                .map(|source| source.source_id.as_str()),
            1,
        )
        .await
        .expect("final STRM page");
    assert_eq!(first_strm_page.len(), 1);
    assert_eq!(second_strm_page.len(), 1);
    assert_ne!(first_strm_page[0].source_id, second_strm_page[0].source_id);
    assert!(final_strm_page.is_empty());
    database.close().await;
}

#[tokio::test]
async fn subtitle_stream_query_is_source_scoped_and_paginated() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    tokio::fs::write(root_path.join("Subtitle.Movie.2024.mkv"), b"fixture")
        .await
        .expect("movie");
    libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
        .await
        .expect("library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("scan");

    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await
            .expect("item");
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await
        .expect("source");
    for (stream_index, codec, title) in [(2_i64, "srt", "English"), (3, "ass", "中文")] {
        sqlx::query(
            "INSERT INTO media_streams
             (id, media_source_id, stream_index, stream_type, codec, language, title,
              details_json, is_external, is_default, is_forced)
             VALUES (?, ?, ?, 'SUBTITLE', ?, ?, ?, ?, 0, ?, 0)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&source_id)
        .bind(stream_index)
        .bind(codec)
        .bind(if stream_index == 2 { "eng" } else { "zho" })
        .bind(title)
        .bind(r#"{"disposition":{"default":true}}"#)
        .bind(if stream_index == 2 { 1_i64 } else { 0 })
        .execute(database.pool())
        .await
        .expect("subtitle stream");
    }

    let page = database
        .list_subtitle_streams(&item_id, Some(&source_id), 1, 1)
        .await
        .expect("source-scoped page");
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].media_source_id, source_id);
    assert_eq!(page[0].item_id, item_id);
    assert_eq!(page[0].stream_index, 3);
    assert_eq!(page[0].codec.as_deref(), Some("ass"));
    assert_eq!(page[0].source_kind, "LOCAL_FILE");
    assert_eq!(page[0].relative_path, "Subtitle.Movie.2024.mkv");
    assert!(page[0].external_path.is_none());

    let default_page = database
        .list_subtitle_streams(&page[0].item_id, None, 0, 10)
        .await
        .expect("default source page");
    assert_eq!(default_page.len(), 2);
    assert!(
        database
            .list_subtitle_streams(&item_id, Some("not-this-source"), 0, 10)
            .await
            .expect("other source page")
            .is_empty()
    );
    database.close().await;
}

#[tokio::test]
async fn library_roots_can_be_loaded_by_id_in_one_query() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let first_path = temp_dir.path().join("first");
    let second_path = temp_dir.path().join("second");
    tokio::fs::create_dir_all(&first_path)
        .await
        .expect("first root");
    tokio::fs::create_dir_all(&second_path)
        .await
        .expect("second root");
    let first = libraries
        .add_root(library.id, first_path.to_str().expect("first path"))
        .await
        .expect("first library root")
        .root;
    let second = libraries
        .add_root(library.id, second_path.to_str().expect("second path"))
        .await
        .expect("second library root")
        .root;

    database.reset_query_count();
    let roots = database
        .list_library_roots_by_ids(&[first.id.to_string(), second.id.to_string()])
        .await
        .expect("roots");

    assert_eq!(roots.len(), 2);
    assert_eq!(database.query_count(), 1);
}

#[tokio::test]
async fn movie_batch_insert_uses_one_item_for_multiple_sources() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
        .await
        .expect("library root");
    let root = database
        .list_library_roots(&library.id.to_string())
        .await
        .expect("roots")
        .into_iter()
        .next()
        .expect("root");
    let files = vec![
        NewMovieFile {
            filesystem_entry_id: "entry-1".to_owned(),
            source_id: "source-1".to_owned(),
            relative_path: "Movie/Movie.2024.mkv".to_owned(),
            size: 1,
            modified_at: 1,
            fingerprint: vec![1],
            title: "Movie".to_owned(),
            sort_title: "movie".to_owned(),
            original_title: "Movie".to_owned(),
            production_year: Some(2024),
            provider_ids_json: None,
            source_kind: "LOCAL_FILE".to_owned(),
            strm_target_kind: None,
            edition_name: None,
            quality_label: None,
            container: "mkv".to_owned(),
            external_url: None,
        },
        NewMovieFile {
            filesystem_entry_id: "entry-2".to_owned(),
            source_id: "source-2".to_owned(),
            relative_path: "Movie/Movie.2024.Directors.Cut.mkv".to_owned(),
            size: 2,
            modified_at: 2,
            fingerprint: vec![2],
            title: "Movie".to_owned(),
            sort_title: "movie".to_owned(),
            original_title: "Movie".to_owned(),
            production_year: Some(2024),
            provider_ids_json: Some(r#"{"tmdb":"1"}"#.to_owned()),
            source_kind: "LOCAL_FILE".to_owned(),
            strm_target_kind: None,
            edition_name: Some("Director's Cut".to_owned()),
            quality_label: None,
            container: "mkv".to_owned(),
            external_url: None,
        },
    ];
    database.reset_query_count();
    let created_items = database
        .insert_movie_files_batch(&library.id.to_string(), &root.id, "generation", &files)
        .await
        .expect("batch insert");

    assert_eq!(created_items, 1);
    assert_eq!(database.query_count(), 7);
    let item_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_items WHERE item_type <> 'FOLDER'")
            .fetch_one(database.pool())
            .await
            .expect("item count");
    let source_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_sources")
        .fetch_one(database.pool())
        .await
        .expect("source count");
    assert_eq!(item_count, 1);
    assert_eq!(source_count, 2);
    let folder_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_items WHERE item_type = 'FOLDER'")
            .fetch_one(database.pool())
            .await
            .expect("folder count");
    assert_eq!(folder_count, 1);

    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type <> 'FOLDER'")
            .fetch_one(database.pool())
            .await
            .expect("item id");
    let rows = database
        .list_catalog_rows_by_ids(std::slice::from_ref(&item_id))
        .await
        .expect("catalog rows");
    assert_eq!(rows.iter().filter(|row| row.item_id == item_id).count(), 2);
    let details = database
        .list_catalog_details_by_ids(std::slice::from_ref(&item_id))
        .await
        .expect("catalog details");
    assert!(details.contains_key(&item_id));
    let provider_ids: Option<String> =
        sqlx::query_scalar("SELECT provider_ids_json FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("provider ids");
    assert_eq!(provider_ids.as_deref(), Some(r#"{"tmdb":"1"}"#));

    let follow_up_file = NewMovieFile {
        filesystem_entry_id: "entry-3".to_owned(),
        source_id: "source-3".to_owned(),
        relative_path: "Another.Movie.2025.mkv".to_owned(),
        size: 3,
        modified_at: 3,
        fingerprint: vec![3],
        title: "Another Movie".to_owned(),
        sort_title: "another movie".to_owned(),
        original_title: "Another Movie".to_owned(),
        production_year: Some(2025),
        provider_ids_json: Some(r#"{"tmdb":"2"}"#.to_owned()),
        source_kind: "LOCAL_FILE".to_owned(),
        strm_target_kind: None,
        edition_name: None,
        quality_label: None,
        container: "mkv".to_owned(),
        external_url: None,
    };
    database.reset_query_count();
    assert_eq!(
        database
            .insert_movie_files_batch(
                &library.id.to_string(),
                &root.id,
                "generation-2",
                &[follow_up_file],
            )
            .await
            .expect("follow-up batch insert"),
        1
    );
    assert_eq!(database.query_count(), 4);
}

#[tokio::test]
async fn movie_batch_insert_updates_strm_poster_fallbacks_as_a_set() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
        .await
        .expect("library root")
        .root;
    let files = [
        NewMovieFile {
            filesystem_entry_id: "strm-entry-1".to_owned(),
            source_id: "strm-source-1".to_owned(),
            relative_path: "First.Remote.2024.strm".to_owned(),
            size: 1,
            modified_at: 1,
            fingerprint: vec![1],
            title: "First Remote".to_owned(),
            sort_title: "first remote".to_owned(),
            original_title: "First Remote".to_owned(),
            production_year: Some(2024),
            provider_ids_json: None,
            source_kind: "STRM_URL".to_owned(),
            strm_target_kind: Some("URL".to_owned()),
            edition_name: None,
            quality_label: None,
            container: "strm".to_owned(),
            external_url: Some("https://example.invalid/first".to_owned()),
        },
        NewMovieFile {
            filesystem_entry_id: "strm-entry-2".to_owned(),
            source_id: "strm-source-2".to_owned(),
            relative_path: "Second.Remote.2025.strm".to_owned(),
            size: 1,
            modified_at: 2,
            fingerprint: vec![2],
            title: "Second Remote".to_owned(),
            sort_title: "second remote".to_owned(),
            original_title: "Second Remote".to_owned(),
            production_year: Some(2025),
            provider_ids_json: None,
            source_kind: "STRM_URL".to_owned(),
            strm_target_kind: Some("URL".to_owned()),
            edition_name: None,
            quality_label: None,
            container: "strm".to_owned(),
            external_url: Some("https://example.invalid/second".to_owned()),
        },
    ];

    database.reset_query_count();
    database
        .insert_movie_files_batch(
            &library.id.to_string(),
            &root.id.to_string(),
            "generation",
            &files,
        )
        .await
        .expect("batch insert");

    assert_eq!(
        database.query_count(),
        5,
        "STRM poster fallback promotion should use one set-based update"
    );
    let fallback_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items WHERE item_type = 'MOVIE' AND poster_fallback_required = 1",
    )
    .fetch_one(database.pool())
    .await
    .expect("poster fallback count");
    assert_eq!(fallback_count, 2);
}

#[tokio::test]
async fn movie_batch_insert_refreshes_existing_parent_folders_as_a_set() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
        .await
        .expect("library root")
        .root;

    let make_file = |index: usize, folder: &str, name: &str| NewMovieFile {
        filesystem_entry_id: format!("folder-entry-{index}"),
        source_id: format!("folder-source-{index}"),
        relative_path: format!("{folder}/{name}.202{index}.mkv"),
        size: 1,
        modified_at: index as i64,
        fingerprint: vec![index as u8],
        title: format!("{name} {index}"),
        sort_title: format!("{name} {index}"),
        original_title: format!("{name} {index}"),
        production_year: Some(2020 + index as i64),
        provider_ids_json: None,
        source_kind: "LOCAL_FILE".to_owned(),
        strm_target_kind: None,
        edition_name: None,
        quality_label: None,
        container: "mkv".to_owned(),
        external_url: None,
    };

    let first_files = vec![
        make_file(1, "Alpha", "First"),
        make_file(2, "Beta", "First"),
        make_file(3, "Gamma", "First"),
    ];
    database
        .insert_movie_files_batch(
            &library.id.to_string(),
            &root.id.to_string(),
            "generation-1",
            &first_files,
        )
        .await
        .expect("first batch");

    sqlx::query(
        "UPDATE media_items
         SET title = 'stale', sort_title = 'stale', removed_at = unixepoch()
         WHERE identity_key = ?",
    )
    .bind(format!("folder:{}:Alpha", root.id))
    .execute(database.pool())
    .await
    .expect("stale folder");

    let second_files = vec![
        make_file(4, "Alpha", "Second"),
        make_file(5, "Beta", "Second"),
        make_file(6, "Gamma", "Second"),
    ];
    database.reset_query_count();
    database
        .insert_movie_files_batch(
            &library.id.to_string(),
            &root.id.to_string(),
            "generation-2",
            &second_files,
        )
        .await
        .expect("second batch");

    assert_eq!(
        database.query_count(),
        6,
        "existing parent folders should refresh in one set-based statement"
    );
    let (title, removed_at): (String, Option<i64>) =
        sqlx::query_as("SELECT title, removed_at FROM media_items WHERE identity_key = ?")
            .bind(format!("folder:{}:Alpha", root.id))
            .fetch_one(database.pool())
            .await
            .expect("refreshed folder");
    assert_eq!(title, "Alpha");
    assert!(removed_at.is_none());
}

#[tokio::test]
async fn write_probe_reports_a_query_only_sqlite_connection() {
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(
            AnyConnectOptions::from_str("sqlite://?mode=memory").expect("in-memory SQLite options"),
        )
        .await
        .expect("in-memory SQLite connection");
    sqlx::query("CREATE TABLE lux_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("create probe table");
    sqlx::query("PRAGMA query_only = ON")
        .execute(&pool)
        .await
        .expect("enable query-only mode");

    let database = Database {
        pool,
        log_store: LogStore::new(Path::new("unused-query-only-test")),
        pool_max_connections: 1,
        path: PathBuf::from("query-only-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
        recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
            RecommendationRatingMedianCache::default(),
        )),
        query_count: Arc::new(AtomicUsize::new(0)),
    };
    assert!(database.probe_write().await.is_err());
    database.close().await;
}

#[tokio::test]
async fn metadata_jobs_process_series_before_seasons_and_episodes() {
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(
            AnyConnectOptions::from_str("sqlite://?mode=memory").expect("in-memory SQLite options"),
        )
        .await
        .expect("in-memory SQLite connection");
    sqlx::query("CREATE TABLE media_items (id TEXT PRIMARY KEY, item_type TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("create media items table");
    sqlx::query(
        "CREATE TABLE metadata_reidentify_job_items (
                job_id TEXT NOT NULL,
                item_id TEXT NOT NULL,
                status TEXT NOT NULL,
                priority INTEGER NOT NULL,
                PRIMARY KEY (job_id, item_id)
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata job items table");
    for (item_id, item_type, priority) in [
        ("episode", "EPISODE", 2_i64),
        ("season", "SEASON", 1_i64),
        ("series", "SERIES", 0_i64),
    ] {
        sqlx::query("INSERT INTO media_items (id, item_type) VALUES (?, ?)")
            .bind(item_id)
            .bind(item_type)
            .execute(&pool)
            .await
            .expect("insert media item");
        sqlx::query(
            "INSERT INTO metadata_reidentify_job_items
                 (job_id, item_id, status, priority)
                 VALUES ('job', ?, 'PENDING', ?)",
        )
        .bind(item_id)
        .bind(priority)
        .execute(&pool)
        .await
        .expect("insert metadata job item");
    }
    let database = Database {
        pool,
        log_store: LogStore::new(Path::new("unused-metadata-order-test")),
        pool_max_connections: 1,
        path: PathBuf::from("metadata-order-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
        recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
            RecommendationRatingMedianCache::default(),
        )),
        query_count: Arc::new(AtomicUsize::new(0)),
    };

    assert_eq!(
        database.next_metadata_reidentify_item("job").await.unwrap(),
        Some("series".to_owned())
    );
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
             SET status = 'COMPLETED'
             WHERE job_id = 'job' AND item_id = 'series'",
    )
    .execute(&database.pool)
    .await
    .expect("complete series item");
    assert_eq!(
        database.next_metadata_reidentify_item("job").await.unwrap(),
        Some("season".to_owned())
    );
    database.close().await;
}

#[tokio::test]
async fn metadata_jobs_claim_items_in_priority_order_as_a_batch() {
    sqlx::any::install_default_drivers();
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(
            AnyConnectOptions::from_str("sqlite://?mode=memory").expect("in-memory SQLite options"),
        )
        .await
        .expect("in-memory SQLite connection");
    sqlx::query("CREATE TABLE media_items (id TEXT PRIMARY KEY, item_type TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("create media items table");
    sqlx::query(
        "CREATE TABLE metadata_reidentify_jobs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                cancel_requested INTEGER NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata jobs table");
    sqlx::query(
        "CREATE TABLE metadata_reidentify_job_items (
                job_id TEXT NOT NULL,
                item_id TEXT NOT NULL,
                status TEXT NOT NULL,
                priority INTEGER NOT NULL,
                updated_at INTEGER NOT NULL DEFAULT 0,
                request_fingerprint BLOB,
                request_capabilities_json TEXT NOT NULL DEFAULT '[]',
                claimed_request_fingerprint BLOB,
                claimed_request_capabilities_json TEXT NOT NULL DEFAULT '[]',
                PRIMARY KEY (job_id, item_id)
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata job items table");
    sqlx::query(
        "CREATE INDEX idx_metadata_reidentify_items_claim_priority
             ON metadata_reidentify_job_items(job_id, status, priority, item_id)",
    )
    .execute(&pool)
    .await
    .expect("create metadata job claim index");
    for (item_id, item_type, priority) in [
        ("episode", "EPISODE", 2_i64),
        ("season", "SEASON", 1_i64),
        ("series", "SERIES", 0_i64),
        ("movie", "MOVIE", 0_i64),
    ] {
        sqlx::query("INSERT INTO media_items (id, item_type) VALUES (?, ?)")
            .bind(item_id)
            .bind(item_type)
            .execute(&pool)
            .await
            .expect("insert media item");
        sqlx::query(
            "INSERT INTO metadata_reidentify_job_items
                 (job_id, item_id, status, priority)
                 VALUES ('job', ?, 'PENDING', ?)",
        )
        .bind(item_id)
        .bind(priority)
        .execute(&pool)
        .await
        .expect("insert metadata job item");
    }
    sqlx::query("INSERT INTO metadata_reidentify_jobs (id, status, cancel_requested) VALUES ('job', 'RUNNING', 0)")
        .execute(&pool)
        .await
        .expect("insert metadata job");
    let plan = sqlx::query(
        "EXPLAIN QUERY PLAN
         SELECT MIN(priority) FROM metadata_reidentify_job_items
         WHERE job_id = 'job' AND status = 'PENDING'",
    )
    .fetch_all(&pool)
    .await
    .expect("explain priority lookup");
    let plan_details = plan
        .iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>();
    assert!(
        plan_details.iter().any(|detail| {
            detail.contains("USING COVERING INDEX idx_metadata_reidentify_items_claim_priority")
        }),
        "priority lookup should use the claim index: {plan_details:?}"
    );
    let database = Database {
        pool,
        log_store: LogStore::new(Path::new("unused-metadata-batch-claim-test")),
        pool_max_connections: 1,
        path: PathBuf::from("metadata-batch-claim-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
        recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
            RecommendationRatingMedianCache::default(),
        )),
        query_count: Arc::new(AtomicUsize::new(0)),
    };

    let claimed = database
        .claim_next_metadata_reidentify_items("job", 2)
        .await
        .expect("claim metadata items");
    assert_eq!(claimed, vec!["movie", "series"]);
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'COMPLETED'
         WHERE job_id = 'job' AND item_id IN ('movie', 'series')",
    )
    .execute(&database.pool)
    .await
    .expect("complete series item");
    let claimed = database
        .claim_next_metadata_reidentify_items("job", 2)
        .await
        .expect("claim remaining metadata items");
    assert_eq!(claimed, vec!["season"]);
    let statuses = sqlx::query_as::<_, (String, String)>(
        "SELECT item_id, status FROM metadata_reidentify_job_items
         WHERE job_id = 'job' ORDER BY item_id",
    )
    .fetch_all(&database.pool)
    .await
    .expect("read claimed statuses");
    assert_eq!(
        statuses,
        vec![
            ("episode".to_owned(), "PENDING".to_owned()),
            ("movie".to_owned(), "COMPLETED".to_owned()),
            ("season".to_owned(), "RUNNING".to_owned()),
            ("series".to_owned(), "COMPLETED".to_owned()),
        ]
    );
    database.close().await;
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_metadata_claim_uses_materialized_priorities_and_preserves_group_order()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration = DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    admin_pool.close().await;

    let connection = PostgresConnection {
        database: database_name.clone(),
        ..admin_connection.clone()
    };
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database =
        Database::connect_with_configuration(&config, &DatabaseConfiguration::Postgres(connection))
            .await?;
    let result = async {
        let library = LibraryService::new(database.clone())
            .create_library("PostgreSQL metadata claim", LibraryKind::Movie, false)
            .await?;
        let item_ids = [
            "metadata-claim-movie",
            "metadata-claim-series",
            "metadata-claim-season",
            "metadata-claim-episode",
        ];
        for (item_id, item_type) in [
            (item_ids[0], "MOVIE"),
            (item_ids[1], "SERIES"),
            (item_ids[2], "SEASON"),
            (item_ids[3], "EPISODE"),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES ($1, $2, $3, $1, $1, 'PENDING')",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .bind(item_type)
            .execute(database.pool())
            .await?;
        }
        database
            .create_metadata_reidentify_job(
                "metadata-claim-job",
                &item_ids.map(str::to_owned),
                "REIDENTIFY",
            )
            .await?;
        let priorities: Vec<(String, i32)> = sqlx::query_as(
            "SELECT item_id, priority FROM metadata_reidentify_job_items
             WHERE job_id = $1 ORDER BY priority, item_id",
        )
        .bind("metadata-claim-job")
        .fetch_all(database.pool())
        .await?;
        let claim_index_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_indexes
             WHERE schemaname = current_schema()
               AND indexname = 'idx_metadata_reidentify_items_claim_priority')",
        )
        .fetch_one(database.pool())
        .await?;

        let first_claim = database
            .claim_next_metadata_reidentify_items("metadata-claim-job", 2)
            .await?;
        let blocked_claim = database
            .claim_next_metadata_reidentify_items("metadata-claim-job", 2)
            .await?;
        sqlx::query(
            "UPDATE metadata_reidentify_job_items SET status = 'COMPLETED'
             WHERE job_id = $1 AND priority = 0",
        )
        .bind("metadata-claim-job")
        .execute(database.pool())
        .await?;
        let next_claim = database
            .claim_next_metadata_reidentify_items("metadata-claim-job", 2)
            .await?;

        Ok::<_, Box<dyn std::error::Error>>((
            priorities,
            claim_index_exists,
            first_claim,
            blocked_claim,
            next_claim,
        ))
    }
    .await;
    database.close().await;

    let cleanup_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&cleanup_pool)
    .await?;
    cleanup_pool.close().await;

    let (priorities, claim_index_exists, first_claim, blocked_claim, next_claim) = result?;
    assert!(claim_index_exists);
    assert_eq!(
        priorities,
        [
            ("metadata-claim-movie".to_owned(), 0),
            ("metadata-claim-series".to_owned(), 0),
            ("metadata-claim-season".to_owned(), 1),
            ("metadata-claim-episode".to_owned(), 2),
        ]
    );
    assert_eq!(
        first_claim,
        ["metadata-claim-movie", "metadata-claim-series"]
    );
    assert!(blocked_claim.is_empty());
    assert_eq!(next_claim, ["metadata-claim-season"]);
    Ok(())
}

#[tokio::test]
async fn server_settings_are_written_with_one_upsert() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    database.reset_query_count();
    database
        .set_server_settings(
            95,
            300_000_000,
            r#"{"thumbnailScrapingMode":"SCRAPER_FIRST"}"#,
            true,
            "PLUGIN",
        )
        .await?;

    assert_eq!(database.query_count(), 1);
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, value FROM server_settings
         WHERE key IN (
             'resume_played_percent', 'resume_min_ticks', 'media_strategy',
             'force_admin_library_order', 'login_background_source'
         )
         ORDER BY key",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        rows,
        vec![
            ("force_admin_library_order".to_owned(), "1".to_owned()),
            ("login_background_source".to_owned(), "PLUGIN".to_owned()),
            (
                "media_strategy".to_owned(),
                r#"{"thumbnailScrapingMode":"SCRAPER_FIRST"}"#.to_owned()
            ),
            ("resume_min_ticks".to_owned(), "300000000".to_owned()),
            ("resume_played_percent".to_owned(), "95".to_owned()),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn uninstalling_a_plugin_batches_library_scraper_rewrites()
-> Result<(), Box<dyn std::error::Error>> {
    const REMOVED_PLUGIN: &str = "org.lux.removed-scraper";
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let first = libraries
        .create_library("First", LibraryKind::Movie, false)
        .await?;
    let second = libraries
        .create_library("Second", LibraryKind::Movie, false)
        .await?;
    let first_id = first.id.to_string();
    let second_id = second.id.to_string();

    sqlx::query(
        "INSERT INTO installed_plugins (plugin_id, is_enabled)
         VALUES (?, 1)",
    )
    .bind(REMOVED_PLUGIN)
    .execute(database.pool())
    .await?;
    for (library_id, scrapers) in [
        (
            first_id.as_str(),
            [
                (REMOVED_PLUGIN, 0_i64, "PRIMARY"),
                ("backup-one", 1, "BACKUP"),
            ]
            .as_slice(),
        ),
        (
            second_id.as_str(),
            [
                (REMOVED_PLUGIN, 0_i64, "PRIMARY"),
                ("backup-two", 1, "BACKUP"),
                ("supplement-two", 2, "SUPPLEMENT"),
            ]
            .as_slice(),
        ),
    ] {
        for (scraper_id, position, role) in scrapers {
            sqlx::query(
                "INSERT INTO library_scrapers (library_id, scraper_id, position, role)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(library_id)
            .bind(scraper_id)
            .bind(position)
            .bind(role)
            .execute(database.pool())
            .await?;
        }
    }
    sqlx::query(
        "UPDATE libraries
         SET scraper_id = ?, chapter_source_id = ?
         WHERE id = ?",
    )
    .bind(REMOVED_PLUGIN)
    .bind(REMOVED_PLUGIN)
    .bind(&first_id)
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE libraries SET scraper_id = ? WHERE id = ?")
        .bind(REMOVED_PLUGIN)
        .bind(&second_id)
        .execute(database.pool())
        .await?;

    database.reset_query_count();
    database.uninstall_plugin(REMOVED_PLUGIN).await?;
    assert_eq!(database.query_count(), 6);

    let first_scrapers: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT scraper_id, position, role FROM library_scrapers
         WHERE library_id = ? ORDER BY position",
    )
    .bind(&first_id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        first_scrapers,
        vec![("backup-one".to_owned(), 0, "PRIMARY".to_owned())]
    );
    let second_scrapers: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT scraper_id, position, role FROM library_scrapers
         WHERE library_id = ? ORDER BY position",
    )
    .bind(&second_id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        second_scrapers,
        vec![
            ("backup-two".to_owned(), 0, "PRIMARY".to_owned()),
            ("supplement-two".to_owned(), 1, "SUPPLEMENT".to_owned()),
        ]
    );
    let library_values: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, scraper_id, chapter_source_id FROM libraries
         WHERE id IN (?, ?) ORDER BY id",
    )
    .bind(&first_id)
    .bind(&second_id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        library_values,
        vec![
            (first_id.clone(), Some("backup-one".to_owned()), None),
            (second_id.clone(), Some("backup-two".to_owned()), None),
        ]
    );
    let installed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM installed_plugins WHERE plugin_id = ?")
            .bind(REMOVED_PLUGIN)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(installed, 0);
    Ok(())
}

#[tokio::test]
async fn metadata_attempt_state_loads_both_attempt_tables_with_one_query() {
    sqlx::any::install_default_drivers();
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(
            AnyConnectOptions::from_str("sqlite://?mode=memory").expect("in-memory SQLite options"),
        )
        .await
        .expect("in-memory SQLite connection");
    sqlx::query(
        "CREATE TABLE metadata_capability_attempts (
                item_id TEXT NOT NULL,
                provider TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                capability TEXT NOT NULL,
                status TEXT NOT NULL,
                next_retry_at INTEGER
            )",
    )
    .execute(&pool)
    .await
    .expect("create capability attempts");
    sqlx::query(
        "CREATE TABLE metadata_image_attempts (
                item_id TEXT NOT NULL,
                image_type TEXT NOT NULL,
                candidate_key TEXT NOT NULL,
                status TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .expect("create image attempts");
    sqlx::query(
        "INSERT INTO metadata_capability_attempts
             (item_id, provider, provider_id, capability, status, next_retry_at)
         VALUES ('item', 'tmdb', '42', 'CREDITS', 'UNAVAILABLE', 123)",
    )
    .execute(&pool)
    .await
    .expect("insert capability attempt");
    sqlx::query(
        "INSERT INTO metadata_image_attempts
             (item_id, image_type, candidate_key, status)
         VALUES ('item', 'POSTER', 'tmdb:42', 'FAILED')",
    )
    .execute(&pool)
    .await
    .expect("insert image attempt");
    let database = Database {
        pool,
        log_store: LogStore::new(Path::new("unused-metadata-attempt-test")),
        pool_max_connections: 1,
        path: PathBuf::from("metadata-attempt-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
        recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
            RecommendationRatingMedianCache::default(),
        )),
        query_count: Arc::new(AtomicUsize::new(0)),
    };
    database.reset_query_count();

    let (capability_attempts, image_attempts) = database
        .list_metadata_attempts("item")
        .await
        .expect("load metadata attempts");

    assert_eq!(capability_attempts.len(), 1);
    assert_eq!(capability_attempts[0].capability, "CREDITS");
    assert_eq!(capability_attempts[0].next_retry_at, Some(123));
    assert_eq!(image_attempts.len(), 1);
    assert_eq!(image_attempts[0].image_type, "POSTER");
    assert_eq!(database.query_count(), 1);
    database.close().await;
}

#[tokio::test]
async fn item_media_strategy_settings_use_one_query_and_require_an_active_item() {
    sqlx::any::install_default_drivers();
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(
            AnyConnectOptions::from_str("sqlite://?mode=memory").expect("in-memory SQLite options"),
        )
        .await
        .expect("in-memory SQLite connection");
    sqlx::query(
        "CREATE TABLE libraries (
                id TEXT PRIMARY KEY,
                is_enabled INTEGER NOT NULL,
                media_strategy_json TEXT
            )",
    )
    .execute(&pool)
    .await
    .expect("create libraries");
    sqlx::query(
        "CREATE TABLE media_items (
                id TEXT PRIMARY KEY,
                library_id TEXT NOT NULL,
                removed_at INTEGER
            )",
    )
    .execute(&pool)
    .await
    .expect("create media items");
    sqlx::query("CREATE TABLE server_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("create server settings");
    sqlx::query(
        "INSERT INTO libraries (id, is_enabled, media_strategy_json)
         VALUES ('enabled', 1, '{\"thumbnailScrapingMode\":\"SCREENSHOT_FIRST\"}'),
                ('disabled', 0, '{\"thumbnailScrapingMode\":\"NONE\"}')",
    )
    .execute(&pool)
    .await
    .expect("insert libraries");
    sqlx::query(
        "INSERT INTO media_items (id, library_id, removed_at)
         VALUES ('active-item', 'enabled', NULL), ('disabled-item', 'disabled', NULL),
                ('removed-item', 'enabled', 123)",
    )
    .execute(&pool)
    .await
    .expect("insert media items");
    sqlx::query(
        "INSERT INTO server_settings (key, value)
         VALUES ('media_strategy', '{\"thumbnailScrapingMode\":\"SCRAPER_FIRST\"}')",
    )
    .execute(&pool)
    .await
    .expect("insert global strategy");
    let database = Database {
        pool,
        log_store: LogStore::new(Path::new("unused-media-strategy-test")),
        pool_max_connections: 1,
        path: PathBuf::from("media-strategy-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
        recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
            RecommendationRatingMedianCache::default(),
        )),
        query_count: Arc::new(AtomicUsize::new(0)),
    };
    database.reset_query_count();

    let strategy = database
        .find_item_media_strategy_settings("active-item")
        .await
        .expect("load active item strategy");

    assert_eq!(
        strategy,
        Some((
            Some("{\"thumbnailScrapingMode\":\"SCREENSHOT_FIRST\"}".to_owned()),
            Some("{\"thumbnailScrapingMode\":\"SCRAPER_FIRST\"}".to_owned()),
        ))
    );
    assert_eq!(database.query_count(), 1);
    assert_eq!(
        database
            .find_item_media_strategy_settings("disabled-item")
            .await
            .expect("ignore disabled library"),
        None
    );
    assert_eq!(
        database
            .find_item_media_strategy_settings("removed-item")
            .await
            .expect("ignore removed item"),
        None
    );
    database.close().await;
}

#[tokio::test]
async fn local_metadata_completeness_dependencies_are_read_in_bounded_batches()
-> Result<(), Box<dyn std::error::Error>> {
    const ITEM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Completeness dependencies", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let item_ids = (0..ITEM_COUNT)
        .map(|index| format!("completeness-dependency-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO metadata_capability_attempts
             (item_id, provider, provider_id, capability, status, next_retry_at)
         VALUES (?, 'tmdb', '42', 'CREDITS', 'UNAVAILABLE', 123)",
    )
    .bind(&item_ids[0])
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO metadata_image_attempts
             (item_id, image_type, candidate_key, status)
         VALUES (?, 'POSTER', 'tmdb:42:POSTER', 'FAILED')",
    )
    .bind(&item_ids[0])
    .execute(database.pool())
    .await?;

    database.reset_query_count();
    for item_id in &item_ids {
        database.find_item_media_strategy_settings(item_id).await?;
        database.list_item_images(item_id).await?;
        database.list_metadata_attempts(item_id).await?;
    }
    assert_eq!(database.query_count(), ITEM_COUNT * 3);

    database.reset_query_count();
    let strategies = database
        .list_item_media_strategy_settings_by_ids(&item_ids)
        .await?;
    let images = database.list_item_images_by_ids(&item_ids).await?;
    let attempts = database
        .list_metadata_attempts_by_item_ids(&item_ids)
        .await?;

    assert_eq!(strategies.len(), ITEM_COUNT);
    assert_eq!(images.len(), 0);
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[&item_ids[0]].0[0].capability, "CREDITS");
    assert_eq!(attempts[&item_ids[0]].1[0].image_type, "POSTER");
    assert_eq!(database.query_count(), 3);
    Ok(())
}

#[tokio::test]
async fn item_scraper_configurations_are_read_in_one_bounded_batch()
-> Result<(), Box<dyn std::error::Error>> {
    const ITEM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library_with_scraper(
            "Scraper configuration",
            LibraryKind::Movie,
            false,
            Some("legacy-tmdb"),
            true,
        )
        .await?;
    let library_id = library.id.to_string();
    let item_ids = (0..ITEM_COUNT)
        .map(|index| format!("scraper-config-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await?;
    }

    database.reset_query_count();
    for item_id in &item_ids {
        database
            .query(
                "SELECT ls.scraper_id, ls.position, ls.role
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 JOIN library_scrapers ls ON ls.library_id = l.id
                 WHERE mi.id = ? AND mi.removed_at IS NULL
                 ORDER BY ls.position",
            )
            .bind(item_id)
            .fetch_all(database.pool())
            .await?;
    }
    assert_eq!(database.query_count(), ITEM_COUNT);

    database.reset_query_count();
    let configurations = database
        .list_item_scraper_configurations_by_ids(&item_ids)
        .await?;
    assert_eq!(configurations.len(), ITEM_COUNT);
    let (configured, legacy) = &configurations[&item_ids[0]];
    assert_eq!(configured.len(), 1);
    assert_eq!(configured[0].scraper_id, "legacy-tmdb");
    assert_eq!(legacy.as_deref(), Some("legacy-tmdb"));
    assert_eq!(database.query_count(), 1);
    Ok(())
}

#[tokio::test]
async fn plugin_installation_statuses_are_read_in_one_bounded_batch()
-> Result<(), Box<dyn std::error::Error>> {
    const PLUGIN_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let plugin_ids = (0..PLUGIN_COUNT)
        .map(|index| format!("org.lux.batch-plugin-{index:03}"))
        .collect::<Vec<_>>();
    sqlx::query(
        "INSERT INTO installed_plugins (plugin_id, is_enabled)
         VALUES (?, 1), (?, 0)",
    )
    .bind(&plugin_ids[0])
    .bind(&plugin_ids[1])
    .execute(database.pool())
    .await?;

    database.reset_query_count();
    for plugin_id in &plugin_ids {
        database.plugin_installation_status(plugin_id).await?;
    }
    assert_eq!(database.query_count(), PLUGIN_COUNT);

    database.reset_query_count();
    let statuses = database
        .list_plugin_installation_statuses_by_ids(&plugin_ids)
        .await?;
    assert_eq!(statuses.len(), 2);
    assert!(statuses[&plugin_ids[0]]);
    assert!(!statuses[&plugin_ids[1]]);
    assert_eq!(database.query_count(), 1);
    Ok(())
}

#[tokio::test]
async fn active_media_metadata_with_libraries_uses_one_bounded_query()
-> Result<(), Box<dyn std::error::Error>> {
    const ITEM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Metadata preflight", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let item_ids = (0..ITEM_COUNT)
        .map(|index| format!("metadata-preflight-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await?;
    }

    database.reset_query_count();
    for item_id in &item_ids {
        assert!(database.find_media_item_metadata(item_id).await?.is_some());
        assert_eq!(
            database.find_item_library_id(item_id).await?.as_deref(),
            Some(library_id.as_str())
        );
    }
    assert_eq!(database.query_count(), ITEM_COUNT * 2);

    database.reset_query_count();
    let metadata = database
        .list_active_media_item_metadata_with_libraries(&item_ids)
        .await?;
    assert_eq!(metadata.len(), ITEM_COUNT);
    assert!(
        metadata
            .values()
            .all(|(id, item)| { id == &library_id && item.item_type == "MOVIE" })
    );
    assert_eq!(database.query_count(), 1);
    Ok(())
}

#[tokio::test]
async fn media_writeback_contexts_are_read_in_one_bounded_batch()
-> Result<(), Box<dyn std::error::Error>> {
    const ITEM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Writeback contexts", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let item_ids = (0..ITEM_COUNT)
        .map(|index| format!("writeback-context-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await?;
    }

    database.reset_query_count();
    for item_id in &item_ids {
        assert_eq!(
            database
                .find_media_item_kind(item_id)
                .await?
                .as_ref()
                .map(|kind| kind.item_type.as_str()),
            Some("MOVIE")
        );
        assert!(
            database
                .find_metadata_writeback_source_path(item_id)
                .await?
                .is_none()
        );
    }
    assert_eq!(database.query_count(), ITEM_COUNT * 2);

    database.reset_query_count();
    let contexts = database
        .list_media_item_writeback_contexts_by_ids(&item_ids)
        .await?;
    assert_eq!(contexts.len(), ITEM_COUNT);
    assert_eq!(contexts[&item_ids[0]].item_type, "MOVIE");
    assert!(contexts[&item_ids[0]].source.is_none());
    assert_eq!(database.query_count(), 1);
    Ok(())
}

#[tokio::test]
async fn media_writeback_contexts_preserve_direct_and_episode_source_selection()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Writeback source selection", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES
            ('writeback-movie', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED'),
            ('writeback-series', ?, 'SERIES', 'Series', 'series', 'LOCAL_CONFIRMED'),
            ('writeback-episode', ?, 'EPISODE', 'Episode', 'episode', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .bind(&library_id)
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE media_items
         SET series_id = 'writeback-series'
         WHERE id = 'writeback-episode'",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('writeback-root', ?, ?, ?, 1, 1)",
    )
    .bind(&library_id)
    .bind(temp_dir.path().to_string_lossy().as_ref())
    .bind(temp_dir.path().to_string_lossy().as_ref())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO filesystem_entries (
             id, library_root_id, relative_path, entry_kind, size, modified_at,
             last_seen_generation
         ) VALUES
            ('writeback-movie-file', 'writeback-root', 'movie.mkv', 'FILE', 10, 1, 'generation'),
            ('writeback-episode-file', 'writeback-root', 'show/episode.mkv', 'FILE', 10, 1, 'generation')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_sources (
             id, item_id, source_kind, filesystem_entry_id, is_default, probe_status
         ) VALUES
            ('writeback-movie-source', 'writeback-movie', 'LOCAL_FILE',
             'writeback-movie-file', 1, 'READY'),
            ('writeback-episode-source', 'writeback-episode', 'LOCAL_FILE',
             'writeback-episode-file', 1, 'READY')",
    )
    .execute(database.pool())
    .await?;

    database.reset_query_count();
    let contexts = database
        .list_media_item_writeback_contexts_by_ids(&[
            "writeback-movie".to_owned(),
            "writeback-series".to_owned(),
        ])
        .await?;
    assert_eq!(database.query_count(), 1);
    assert_eq!(contexts["writeback-movie"].item_type, "MOVIE");
    assert_eq!(
        contexts["writeback-movie"]
            .source
            .as_ref()
            .map(|source| source.item_id.as_str()),
        Some("writeback-movie")
    );
    assert_eq!(contexts["writeback-series"].item_type, "SERIES");
    assert_eq!(
        contexts["writeback-series"]
            .source
            .as_ref()
            .map(|source| source.item_id.as_str()),
        Some("writeback-episode")
    );
    Ok(())
}

#[tokio::test]
async fn metadata_jobs_reconcile_items_left_running_by_workers() {
    sqlx::any::install_default_drivers();
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(
            AnyConnectOptions::from_str("sqlite://?mode=memory").expect("in-memory SQLite options"),
        )
        .await
        .expect("in-memory SQLite connection");
    sqlx::query(
        "CREATE TABLE metadata_reidentify_jobs (
                id TEXT PRIMARY KEY,
                processed_count INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata jobs table");
    sqlx::query(
        "CREATE TABLE metadata_reidentify_job_items (
                job_id TEXT NOT NULL,
                item_id TEXT NOT NULL,
                status TEXT NOT NULL,
                candidate_count INTEGER NOT NULL,
                error TEXT,
                updated_at INTEGER NOT NULL,
                request_fingerprint BLOB,
                request_capabilities_json TEXT NOT NULL DEFAULT '[]',
                claimed_request_fingerprint BLOB,
                claimed_request_capabilities_json TEXT NOT NULL DEFAULT '[]',
                PRIMARY KEY (job_id, item_id)
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata job items table");
    sqlx::query(
        "INSERT INTO metadata_reidentify_jobs (id, processed_count, updated_at)
             VALUES ('job', 0, unixepoch())",
    )
    .execute(&pool)
    .await
    .expect("insert metadata job");
    for (item_id, status) in [
        ("running-1", "RUNNING"),
        ("running-2", "RUNNING"),
        ("done", "COMPLETED"),
    ] {
        sqlx::query(
            "INSERT INTO metadata_reidentify_job_items (
                    job_id, item_id, status, candidate_count, error, updated_at
                 ) VALUES ('job', ?, ?, 0, NULL, unixepoch())",
        )
        .bind(item_id)
        .bind(status)
        .execute(&pool)
        .await
        .expect("insert metadata job item");
    }
    let database = Database {
        pool,
        log_store: LogStore::new(Path::new("unused-metadata-reconcile-test")),
        pool_max_connections: 1,
        path: PathBuf::from("metadata-reconcile-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
        recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
            RecommendationRatingMedianCache::default(),
        )),
        query_count: Arc::new(AtomicUsize::new(0)),
    };

    let reconciled = database
        .fail_running_metadata_reidentify_items("job", "WORKER_FAILED")
        .await
        .expect("reconcile running items");

    assert_eq!(reconciled, 2);
    let failed_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM metadata_reidentify_job_items
             WHERE job_id = 'job' AND status = 'FAILED' AND error = 'WORKER_FAILED'",
    )
    .fetch_one(database.pool())
    .await
    .expect("failed item count");
    assert_eq!(failed_count, 2);
    let processed_count: i64 =
        sqlx::query_scalar("SELECT processed_count FROM metadata_reidentify_jobs WHERE id = 'job'")
            .fetch_one(database.pool())
            .await
            .expect("processed count");
    assert_eq!(processed_count, 2);
    database.close().await;
}

#[test]
fn provider_identity_uses_the_selected_scraper_without_falling_back_to_another_id() {
    let providers = Some(
        serde_json::json!({
            "Imdb": "tt123",
            "Tvdb": "456"
        })
        .to_string(),
    );

    assert_eq!(
        first_provider_id(providers.clone(), None, Some("org.example.tvdb")),
        Some(("Tvdb".to_owned(), "456".to_owned()))
    );
    assert_eq!(first_provider_id(providers, None, Some("tmdb")), None);
}

#[test]
fn postgres_placeholder_adapter_preserves_quoted_question_marks() {
    let sql = "SELECT ?, '?' AS literal, \"?\" AS identifier, ?";
    assert_eq!(
        adapt_sql_for_backend(DatabaseBackend::Postgres, sql),
        "SELECT $1, '?' AS literal, \"?\" AS identifier, $2"
    );
    assert_eq!(adapt_sql_for_backend(DatabaseBackend::Sqlite, sql), sql);
}

#[tokio::test]
async fn chapter_detection_job_creation_is_atomic_per_library() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Shows", LibraryKind::Series, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    fn new_job<'a>(id: &'a str, library_id: &'a str) -> NewChapterDetectionJob<'a> {
        NewChapterDetectionJob {
            id,
            library_id,
            plugin_id: "org.lux.intro-outro-detector",
            concurrency: 1,
            intro_window_seconds: 180,
            credits_window_seconds: 180,
            match_threshold: 0.8,
            total_count: 0,
        }
    }

    assert!(
        database
            .create_chapter_detection_job(new_job("chapter-job-1", &library_id))
            .await
            .expect("first job should be created")
    );
    assert!(
        !database
            .create_chapter_detection_job(new_job("chapter-job-2", &library_id))
            .await
            .expect("active duplicate should be rejected")
    );
}

#[tokio::test]
async fn chapter_detection_job_items_are_inserted_in_bounded_batches() {
    const ITEM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Chapter batch", LibraryKind::Series, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('chapter-season-batch', ?, 'SEASON', 'Season 1', 'season-1', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("season item");
    sqlx::query(
        "WITH RECURSIVE sequence(value) AS (
             SELECT 1 UNION ALL SELECT value + 1 FROM sequence WHERE value < ?
         )
         INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         )
         SELECT 'chapter-item-' || value, ?, 'EPISODE', 'Episode ' || value,
                'episode-' || value, 'LOCAL_CONFIRMED'
         FROM sequence",
    )
    .bind(ITEM_COUNT as i64)
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("episode items");
    sqlx::query(
        "WITH RECURSIVE sequence(value) AS (
             SELECT 1 UNION ALL SELECT value + 1 FROM sequence WHERE value < ?
         )
         INSERT INTO media_sources (id, item_id, source_kind, probe_status)
         SELECT 'chapter-source-' || value, 'chapter-item-' || value, 'LOCAL_FILE', 'READY'
         FROM sequence",
    )
    .bind(ITEM_COUNT as i64)
    .execute(database.pool())
    .await
    .expect("media sources");

    let source_ids = (1..=ITEM_COUNT)
        .map(|index| format!("chapter-source-{index}"))
        .collect::<Vec<_>>();
    let item_ids = (1..=ITEM_COUNT)
        .map(|index| format!("chapter-item-{index}"))
        .collect::<Vec<_>>();
    let job_id = "chapter-job-batch-insert";
    let new_job = |id| NewChapterDetectionJob {
        id,
        library_id: &library_id,
        plugin_id: "org.lux.intro-outro-detector",
        concurrency: 1,
        intro_window_seconds: 180,
        credits_window_seconds: 180,
        match_threshold: 0.8,
        total_count: ITEM_COUNT as i64,
    };
    assert!(
        database
            .create_chapter_detection_job(new_job(job_id))
            .await
            .expect("create chapter job")
    );

    database.reset_query_count();
    database
        .insert_chapter_detection_job_items(&[])
        .await
        .expect("empty input should be a no-op");
    assert_eq!(database.query_count(), 0);

    let input_fingerprint = [0x24; 32];
    let source_fingerprint = b"source-fingerprint";
    let items = source_ids
        .iter()
        .zip(&item_ids)
        .enumerate()
        .map(|(index, (source_id, item_id))| NewChapterDetectionJobItem {
            job_id,
            source_id,
            item_id,
            season_id: "chapter-season-batch",
            source_fingerprint,
            input_fingerprint: &input_fingerprint,
            is_context: index % 2 == 0,
        })
        .collect::<Vec<_>>();
    database.reset_query_count();
    database
        .insert_chapter_detection_job_items(&items)
        .await
        .expect("insert chapter job items");
    assert_eq!(database.query_count(), 3);
    let persisted_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM chapter_detection_job_items WHERE job_id = ?")
            .bind(job_id)
            .fetch_one(database.pool())
            .await
            .expect("persisted item count");
    assert_eq!(persisted_count, ITEM_COUNT as i64);
    let context_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chapter_detection_job_items
         WHERE job_id = ? AND is_context = 1",
    )
    .bind(job_id)
    .fetch_one(database.pool())
    .await
    .expect("context item count");
    assert_eq!(context_count, ITEM_COUNT.div_ceil(2) as i64);
    let stored_item: (String, Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT status, source_fingerprint, input_fingerprint
         FROM chapter_detection_job_items WHERE job_id = ? AND source_id = ?",
    )
    .bind(job_id)
    .bind(&source_ids[0])
    .fetch_one(database.pool())
    .await
    .expect("stored item fields");
    assert_eq!(stored_item.0, "PENDING");
    assert_eq!(stored_item.1, source_fingerprint);
    assert_eq!(stored_item.2, input_fingerprint);

    sqlx::query("UPDATE chapter_detection_jobs SET status = 'COMPLETED' WHERE id = ?")
        .bind(job_id)
        .execute(database.pool())
        .await
        .expect("complete first job");
    let rollback_job_id = "chapter-job-batch-rollback";
    assert!(
        database
            .create_chapter_detection_job(new_job(rollback_job_id))
            .await
            .expect("create rollback job")
    );
    let mut invalid_items = source_ids
        .iter()
        .zip(&item_ids)
        .map(|(source_id, item_id)| NewChapterDetectionJobItem {
            job_id: rollback_job_id,
            source_id,
            item_id,
            season_id: "chapter-season-batch",
            source_fingerprint,
            input_fingerprint: &input_fingerprint,
            is_context: false,
        })
        .collect::<Vec<_>>();
    invalid_items[ITEM_COUNT - 1].source_id = &source_ids[0];
    assert!(
        database
            .insert_chapter_detection_job_items(&invalid_items)
            .await
            .is_err()
    );
    let rolled_back_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM chapter_detection_job_items WHERE job_id = ?")
            .bind(rollback_job_id)
            .fetch_one(database.pool())
            .await
            .expect("rolled back item count");
    assert_eq!(rolled_back_count, 0);
    database.close().await;
}

#[tokio::test]
async fn media_probe_streams_are_replaced_in_bounded_batches() {
    const STREAM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Probe batch", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('probe-batch-item', ?, 'MOVIE', 'Probe', 'probe', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    sqlx::query(
        "INSERT INTO media_sources (id, item_id, source_kind, container, probe_status)
         VALUES ('probe-batch-source', 'probe-batch-item', 'LOCAL_FILE', 'mp4', 'PENDING')",
    )
    .execute(database.pool())
    .await
    .expect("media source");
    sqlx::query(
        "INSERT INTO media_streams (id, media_source_id, stream_index, stream_type)
         VALUES ('probe-stale-stream', 'probe-batch-source', 999, 'AUDIO')",
    )
    .execute(database.pool())
    .await
    .expect("stale stream");

    let new_stream = |index: usize, stream_index: i64| {
        let stream_type = match index % 3 {
            0 => "VIDEO",
            1 => "AUDIO",
            _ => "SUBTITLE",
        };
        let is_external = stream_type == "SUBTITLE";
        MediaStreamUpdate {
            stream_index,
            stream_type,
            codec: Some("test-codec"),
            language: Some("eng"),
            title: Some("Test stream"),
            details_json: Some(r#"{"generated":true}"#),
            external_path: is_external.then_some("subs/eng.srt"),
            is_external,
            is_default: index == 0,
            is_forced: index == 5,
        }
    };
    let streams = (0..STREAM_COUNT)
        .map(|index| new_stream(index, index as i64))
        .collect::<Vec<_>>();
    database.reset_query_count();
    database
        .save_media_probe(MediaProbeUpdate {
            source_id: "probe-batch-source",
            container: Some("mkv"),
            source_size: Some(42),
            duration_ticks: Some(456),
            bitrate: Some(789),
            streams: &streams,
            chapters: &[],
        })
        .await
        .expect("save probe result");
    assert_eq!(database.query_count(), 6);

    let stored_indices: Vec<i64> = sqlx::query_scalar(
        "SELECT stream_index FROM media_streams
         WHERE media_source_id = 'probe-batch-source' ORDER BY stream_index",
    )
    .fetch_all(database.pool())
    .await
    .expect("stored stream indexes");
    assert_eq!(stored_indices, (0..STREAM_COUNT as i64).collect::<Vec<_>>());
    #[derive(sqlx::FromRow)]
    struct StoredMediaStream {
        stream_type: String,
        codec: Option<String>,
        language: Option<String>,
        title: Option<String>,
        details_json: Option<String>,
        is_external: i64,
        is_default: i64,
        is_forced: i64,
    }
    let stored_subtitle: StoredMediaStream = sqlx::query_as(
        "SELECT stream_type, codec, language, title, details_json, is_external, is_default, is_forced
         FROM media_streams
         WHERE media_source_id = 'probe-batch-source' AND stream_index = 2",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored subtitle stream");
    assert_eq!(stored_subtitle.stream_type, "SUBTITLE");
    assert_eq!(stored_subtitle.codec.as_deref(), Some("test-codec"));
    assert_eq!(stored_subtitle.language.as_deref(), Some("eng"));
    assert_eq!(stored_subtitle.title.as_deref(), Some("Test stream"));
    assert_eq!(
        stored_subtitle.details_json.as_deref(),
        Some(r#"{"generated":true}"#)
    );
    assert_eq!(stored_subtitle.is_external, 1);
    assert_eq!(stored_subtitle.is_default, 0);
    assert_eq!(stored_subtitle.is_forced, 0);
    let stored_external_path: Option<String> = sqlx::query_scalar(
        "SELECT external_path FROM media_streams
         WHERE media_source_id = 'probe-batch-source' AND stream_index = 2",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored external subtitle path");
    assert_eq!(stored_external_path.as_deref(), Some("subs/eng.srt"));

    let mut invalid_streams = (0..STREAM_COUNT)
        .map(|index| new_stream(index, index as i64))
        .collect::<Vec<_>>();
    invalid_streams[STREAM_COUNT - 1].stream_index = 0;
    database.reset_query_count();
    assert!(
        database
            .save_media_probe(MediaProbeUpdate {
                source_id: "probe-batch-source",
                container: Some("invalid"),
                source_size: Some(100),
                duration_ticks: Some(200),
                bitrate: Some(300),
                streams: &invalid_streams,
                chapters: &[],
            })
            .await
            .is_err()
    );
    assert_eq!(database.query_count(), 5);
    let retained_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_streams WHERE media_source_id = 'probe-batch-source'",
    )
    .fetch_one(database.pool())
    .await
    .expect("retained stream count after rollback");
    assert_eq!(retained_count, STREAM_COUNT as i64);
    let retained_source: (
        Option<String>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        String,
    ) = sqlx::query_as(
        "SELECT container, size, duration_ticks, bitrate, probe_status
             FROM media_sources WHERE id = 'probe-batch-source'",
    )
    .fetch_one(database.pool())
    .await
    .expect("retained media source after rollback");
    assert_eq!(
        retained_source,
        (
            Some("mp4".to_owned()),
            Some(42),
            Some(456),
            Some(789),
            "READY".to_owned()
        )
    );

    database.reset_query_count();
    database
        .save_media_probe(MediaProbeUpdate {
            source_id: "probe-batch-source",
            container: None,
            source_size: None,
            duration_ticks: None,
            bitrate: None,
            streams: &[],
            chapters: &[],
        })
        .await
        .expect("save probe result with no streams");
    assert_eq!(database.query_count(), 3);
    let empty_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_streams WHERE media_source_id = 'probe-batch-source'",
    )
    .fetch_one(database.pool())
    .await
    .expect("empty stream count");
    assert_eq!(empty_count, 0);
    database.close().await;
}

#[tokio::test]
async fn scan_job_status_counts_are_aggregated_in_storage() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Scan jobs", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    for (id, status, job_type) in [
        ("scan-count-pending", "PENDING", "INCREMENTAL_SCAN"),
        ("scan-count-running", "RUNNING", "RECONCILE_LIBRARY"),
        ("scan-count-failed", "FAILED", "INCREMENTAL_SCAN"),
        ("scan-count-completed", "COMPLETED", "INCREMENTAL_SCAN"),
    ] {
        sqlx::query(
            "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
             VALUES (?, ?, ?, ?, 'generation')",
        )
        .bind(id)
        .bind(&library_id)
        .bind(job_type)
        .bind(status)
        .execute(database.pool())
        .await
        .expect("scan job");
    }

    assert_eq!(
        database
            .count_scan_jobs_by_status()
            .await
            .expect("status counts"),
        StoredScanJobCounts {
            running: 2,
            failed: 1,
        }
    );
}

#[tokio::test]
async fn filesystem_entry_scan_updates_include_inode_in_the_state_write() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Filesystem entry updates", LibraryKind::Movie, false)
        .await
        .expect("library");
    let media_root = temp_dir.path().join("filesystem-entry-updates");
    tokio::fs::create_dir_all(&media_root)
        .await
        .expect("media root");
    let root = LibraryService::new(database.clone())
        .add_root(library.id, media_root.to_str().expect("UTF-8 media root"))
        .await
        .expect("library root")
        .root;
    let root_id = root.id.to_string();
    database
        .insert_filesystem_entry(NewFilesystemEntry {
            id: "filesystem-entry-update",
            library_root_id: &root_id,
            relative_path: "movie.mkv",
            entry_kind: "FILE",
            size: 10,
            modified_at: 20,
            inode: Some(30),
            fingerprint: b"old-fingerprint",
            last_seen_generation: "old-generation",
        })
        .await
        .expect("filesystem entry");
    sqlx::query(
        "UPDATE filesystem_entries SET is_missing = 1 WHERE id = 'filesystem-entry-update'",
    )
    .execute(database.pool())
    .await
    .expect("mark filesystem entry missing");

    database.reset_query_count();
    database
        .update_filesystem_entry(
            "filesystem-entry-update",
            40,
            50,
            b"new-fingerprint",
            Some(60),
            "new-generation",
        )
        .await
        .expect("filesystem entry update");
    assert_eq!(database.query_count(), 2);

    let updated: (i64, i64, Option<i64>, Vec<u8>, String, i64) = sqlx::query_as(
        "SELECT size, modified_at, inode, fingerprint, last_seen_generation, is_missing
         FROM filesystem_entries WHERE id = 'filesystem-entry-update'",
    )
    .fetch_one(database.pool())
    .await
    .expect("updated filesystem entry");
    assert_eq!(
        updated,
        (
            40,
            50,
            Some(60),
            b"new-fingerprint".to_vec(),
            "new-generation".to_owned(),
            0,
        )
    );

    sqlx::query(
        "UPDATE filesystem_entries SET is_missing = 1 WHERE id = 'filesystem-entry-update'",
    )
    .execute(database.pool())
    .await
    .expect("mark filesystem entry missing again");
    database.reset_query_count();
    database
        .mark_filesystem_entry_seen("filesystem-entry-update", "seen-generation", Some(70))
        .await
        .expect("mark filesystem entry seen");
    assert_eq!(database.query_count(), 2);

    let seen: (Option<i64>, String, i64) = sqlx::query_as(
        "SELECT inode, last_seen_generation, is_missing
         FROM filesystem_entries WHERE id = 'filesystem-entry-update'",
    )
    .fetch_one(database.pool())
    .await
    .expect("seen filesystem entry");
    assert_eq!(seen, (Some(70), "seen-generation".to_owned(), 0));
}

#[tokio::test]
async fn incremental_scan_paths_are_enqueued_with_bounded_sql_and_last_change_wins() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Incremental queue", LibraryKind::Movie, false)
        .await
        .expect("library");
    let media_root = temp_dir.path().join("incremental-queue");
    tokio::fs::create_dir_all(&media_root)
        .await
        .expect("media root");
    let root = LibraryService::new(database.clone())
        .add_root(library.id, media_root.to_str().expect("UTF-8 media root"))
        .await
        .expect("library root")
        .root;
    let library_id = library.id.to_string();
    let root_id = root.id.to_string();
    database
        .create_scan_job(
            "incremental-queue-job",
            &library_id,
            "INCREMENTAL_SCAN",
            "generation",
            0,
            false,
        )
        .await
        .expect("scan job");

    database.reset_query_count();
    database
        .enqueue_incremental_scan_path(
            "incremental-queue-job",
            &root_id,
            "folder/file-000.mkv",
            "CREATE",
        )
        .await
        .expect("initial incremental path");
    assert_eq!(database.query_count(), 2);
    sqlx::query(
        "UPDATE scan_job_paths SET processed_at = 1
         WHERE job_id = 'incremental-queue-job' AND relative_path = 'folder/file-000.mkv'",
    )
    .execute(database.pool())
    .await
    .expect("mark initial path processed");

    let mut paths = (0..205)
        .map(|index| {
            (
                root_id.clone(),
                format!("folder/file-{index:03}.mkv"),
                "CREATE".to_owned(),
            )
        })
        .collect::<Vec<_>>();
    paths.push((
        root_id.clone(),
        "folder/file-000.mkv".to_owned(),
        "MODIFY".to_owned(),
    ));
    let paths = paths
        .iter()
        .map(|(root_id, relative_path, change_kind)| {
            (
                root_id.as_str(),
                relative_path.as_str(),
                change_kind.as_str(),
            )
        })
        .collect::<Vec<_>>();

    database.reset_query_count();
    database
        .enqueue_incremental_scan_paths("incremental-queue-job", &paths)
        .await
        .expect("incremental paths");

    assert_eq!(database.query_count(), 6);
    let stored: (i64, i64, String, Option<i64>) = sqlx::query_as(
        "SELECT job.total_count,
                (SELECT COUNT(*) FROM scan_job_paths WHERE job_id = job.id),
                path.change_kind, path.processed_at
         FROM scan_jobs job
         JOIN scan_job_paths path ON path.job_id = job.id
         WHERE job.id = 'incremental-queue-job'
           AND path.relative_path = 'folder/file-000.mkv'",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored incremental paths");
    assert_eq!(stored, (205, 205, "MODIFY".to_owned(), None));
}

#[tokio::test]
async fn reconciliation_entries_use_scan_safe_batches() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Scan batches", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("media root path"))
        .await
        .expect("library root")
        .root;
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await
        .expect("scan job");
    sqlx::query("UPDATE scan_jobs SET status = 'RUNNING' WHERE id = ?")
        .bind(&job.id)
        .execute(database.pool())
        .await
        .expect("start scan job");
    database
        .clear_reconciliation_scan_entries(&job.id)
        .await
        .expect("clear root entry");

    let paths = (0..1_025)
        .map(|index| format!("Movie {index:04}.mkv"))
        .collect::<Vec<_>>();
    database.reset_query_count();
    database
        .commit_reconciliation_discovery_chunk(&job.id, &root.id.to_string(), &[], &paths, None)
        .await
        .expect("commit reconciliation entries");

    database.reset_query_count();
    database
        .commit_reconciliation_discovery_chunk(&job.id, &root.id.to_string(), &[], &paths, None)
        .await
        .expect("re-commit reconciliation entries");

    assert_eq!(database.query_count(), 6);
    let stored: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = ? AND entry_type = 'FILE'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await
    .expect("stored reconciliation entries");
    assert_eq!(stored, 1_025);
    let total_count: i64 = sqlx::query_scalar("SELECT total_count FROM scan_jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await
        .expect("scan total count");
    assert_eq!(total_count, 1_025);
}

#[tokio::test]
async fn reconciliation_discovery_chunk_commits_entries_progress_and_completion() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Discovery commit", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("media root path"))
        .await
        .expect("library root")
        .root;
    let root_id = root.id.to_string();
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await
        .expect("scan job");
    sqlx::query("UPDATE scan_jobs SET status = 'RUNNING' WHERE id = ?")
        .bind(&job.id)
        .execute(database.pool())
        .await
        .expect("start scan job");
    database
        .clear_reconciliation_scan_entries(&job.id)
        .await
        .expect("clear initial directory");
    sqlx::query(
        "INSERT INTO reconciliation_scan_entries (
             job_id, library_root_id, relative_path, entry_type, status
         ) VALUES (?, ?, '', 'DIRECTORY', 'PENDING')",
    )
    .bind(&job.id)
    .bind(&root_id)
    .execute(database.pool())
    .await
    .expect("root directory entry");

    database
        .commit_reconciliation_discovery_chunk(
            &job.id,
            &root_id,
            &["Movie".to_owned()],
            &[
                "Movie/First.Movie.2024.mkv".to_owned(),
                "Movie/Second.Movie.2024.mkv".to_owned(),
                "Movie/First.Movie.2024.mkv".to_owned(),
            ],
            Some(""),
        )
        .await
        .expect("discovery chunk");

    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM reconciliation_scan_entries
              WHERE job_id = ? AND entry_type = 'DIRECTORY'),
             (SELECT COUNT(*) FROM reconciliation_scan_entries
              WHERE job_id = ? AND entry_type = 'FILE'),
             (SELECT total_count FROM scan_jobs WHERE id = ?)",
    )
    .bind(&job.id)
    .bind(&job.id)
    .bind(&job.id)
    .fetch_one(database.pool())
    .await
    .expect("discovery state");
    assert_eq!(counts, (1, 2, 2));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM reconciliation_scan_entries
             WHERE job_id = ? AND relative_path = '' AND entry_type = 'DIRECTORY'",
        )
        .bind(&job.id)
        .fetch_one(database.pool())
        .await
        .expect("completed directory count"),
        0
    );
}

#[tokio::test]
async fn marking_an_unchanged_scan_stage_does_not_touch_the_row() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Scan stage", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('unchanged-stage-job', ?, 'INCREMENTAL_SCAN', 'COMPLETED', 'generation')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("scan job");
    sqlx::query(
        "INSERT INTO scan_job_targets (
             job_id, target_type, target_id, item_id, change_kind,
             metadata_state, updated_at
         ) VALUES ('unchanged-stage-job', 'ITEM', 'unchanged-target', 'item', 'NEW',
                   'DONE', 1)",
    )
    .execute(database.pool())
    .await
    .expect("scan target");

    database
        .mark_scan_job_target_stage(
            "unchanged-stage-job",
            "ITEM",
            &["unchanged-target".to_owned()],
            "METADATA",
            "DONE",
        )
        .await
        .expect("mark unchanged stage");
    let updated_at: i64 = sqlx::query_scalar(
        "SELECT updated_at FROM scan_job_targets WHERE target_id = 'unchanged-target'",
    )
    .fetch_one(database.pool())
    .await
    .expect("unchanged target timestamp");
    assert_eq!(updated_at, 1);
}

#[tokio::test]
async fn chapter_detection_outcomes_commit_status_state_and_progress_as_one_batch() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Chapter jobs", LibraryKind::Series, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 root"))
        .await
        .expect("library root");
    let root_id: String = sqlx::query_scalar("SELECT id FROM library_roots LIMIT 1")
        .fetch_one(database.pool())
        .await
        .expect("root");
    let item_id = "chapter-item";
    let source_id = "chapter-source";
    let entry_id = "chapter-entry";
    sqlx::query(
        "INSERT INTO media_items (id, library_id, item_type, title, sort_title, identification_status)
         VALUES (?, ?, 'EPISODE', 'Episode', 'episode', 'LOCAL_CONFIRMED')",
    )
    .bind(item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    sqlx::query(
        "INSERT INTO filesystem_entries
         (id, library_root_id, relative_path, entry_kind, size, modified_at, last_seen_generation)
         VALUES (?, ?, 'episode.mkv', 'FILE', 1, 1, 'generation')",
    )
    .bind(entry_id)
    .bind(&root_id)
    .execute(database.pool())
    .await
    .expect("filesystem entry");
    sqlx::query(
        "INSERT INTO media_sources (id, item_id, source_kind, filesystem_entry_id, duration_ticks)
         VALUES (?, ?, 'LOCAL_FILE', ?, 10000000)",
    )
    .bind(source_id)
    .bind(item_id)
    .bind(entry_id)
    .execute(database.pool())
    .await
    .expect("media source");
    database
        .create_chapter_detection_job(NewChapterDetectionJob {
            id: "chapter-job-batch",
            library_id: &library.id.to_string(),
            plugin_id: "builtin",
            concurrency: 1,
            intro_window_seconds: 15,
            credits_window_seconds: 15,
            match_threshold: 0.8,
            total_count: 1,
        })
        .await
        .expect("chapter job");
    database
        .claim_chapter_detection_job("chapter-job-batch")
        .await
        .expect("claim chapter job");
    sqlx::query(
        "INSERT INTO chapter_detection_job_items
         (job_id, source_id, item_id, season_id, source_fingerprint, input_fingerprint, is_context, status)
         VALUES (?, ?, ?, ?, ?, ?, 0, 'PENDING')",
    )
    .bind("chapter-job-batch")
    .bind(source_id)
    .bind(item_id)
    .bind(item_id)
    .bind(vec![1_u8])
    .bind(vec![2_u8])
    .execute(database.pool())
    .await
    .expect("chapter item");

    database.reset_query_count();
    database
        .apply_chapter_detection_outcomes(
            "chapter-job-batch",
            "builtin",
            &[ChapterDetectionOutcomeUpdate {
                source_id: source_id.to_owned(),
                status: "COMPLETED".to_owned(),
                error: None,
                source_state: Some(ChapterDetectionSourceStateUpdate {
                    input_fingerprint: vec![2],
                    status: "NOT_FOUND".to_owned(),
                    last_checked_at: 10,
                    last_success_at: None,
                    next_retry_at: Some(20),
                    error: None,
                    intro_fingerprint: None,
                    credits_fingerprint: None,
                }),
            }],
            Some(source_id),
            1,
        )
        .await
        .expect("apply chapter batch");
    assert_eq!(database.query_count(), 3);
    let item_status: String = sqlx::query_scalar(
        "SELECT status FROM chapter_detection_job_items WHERE job_id = ? AND source_id = ?",
    )
    .bind("chapter-job-batch")
    .bind(source_id)
    .fetch_one(database.pool())
    .await
    .expect("item status");
    let source_status: String = sqlx::query_scalar(
        "SELECT status FROM chapter_detection_source_states WHERE source_id = ? AND plugin_id = ?",
    )
    .bind(source_id)
    .bind("builtin")
    .fetch_one(database.pool())
    .await
    .expect("source state");
    let progress: (i64, String) =
        sqlx::query_as("SELECT processed_count, cursor FROM chapter_detection_jobs WHERE id = ?")
            .bind("chapter-job-batch")
            .fetch_one(database.pool())
            .await
            .expect("job progress");
    assert_eq!(item_status, "COMPLETED");
    assert_eq!(source_status, "NOT_FOUND");
    assert_eq!(progress, (1, source_id.to_owned()));
}

#[tokio::test]
async fn detected_media_chapters_are_inserted_in_one_bounded_batch()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Chapter markers", LibraryKind::Series, false)
        .await?;
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path).await?;
    libraries
        .add_root(library.id, root_path.to_str().ok_or("non-utf8 root")?)
        .await?;
    let root_id: String = sqlx::query_scalar("SELECT id FROM library_roots LIMIT 1")
        .fetch_one(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO media_items (id, library_id, item_type, title, sort_title, identification_status)
         VALUES ('marker-item', ?, 'EPISODE', 'Episode', 'episode', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO filesystem_entries
         (id, library_root_id, relative_path, entry_kind, size, modified_at, fingerprint, last_seen_generation)
         VALUES ('marker-entry', ?, 'episode.mkv', 'FILE', 1, 1, ?, 'generation')",
    )
    .bind(&root_id)
    .bind(vec![1_u8])
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_sources (id, item_id, source_kind, filesystem_entry_id)
         VALUES ('marker-source', 'marker-item', 'LOCAL_FILE', 'marker-entry')",
    )
    .execute(database.pool())
    .await?;

    let markers = (0_usize..3)
        .map(|index| NewMediaChapterMarker {
            start_position_ticks: ((index + 1) * 10_000_000) as i64,
            name: None,
            marker_type: ["INTRO_START", "INTRO_END", "CREDITS_START"][index].to_owned(),
            chapter_index: index as i64,
            confidence: 0.9,
        })
        .collect::<Vec<_>>();
    database.reset_query_count();
    assert!(
        database
            .replace_detected_media_chapters("marker-source", "detector", &[1], &markers)
            .await?
    );
    assert_eq!(database.query_count(), 3);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM media_chapters
             WHERE media_source_id = 'marker-source' AND provider_id = 'detector'",
        )
        .fetch_one(database.pool())
        .await?,
        3
    );
    Ok(())
}

#[tokio::test]
async fn person_index_rebuild_tasks_are_token_guarded_and_requeueable() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("People", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();

    let jobs = database
        .sync_person_index_rebuild_jobs(1)
        .await
        .expect("sync jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, "QUEUED");
    assert!(
        database
            .claim_person_index_rebuild_job(&library_id, "run-a")
            .await
            .expect("claim first run")
    );
    assert!(
        database
            .update_person_index_rebuild_progress(&library_id, "run-a", "item-a", 1, 2)
            .await
            .expect("record first run progress")
            .is_some()
    );
    assert!(
        database
            .request_person_index_rebuild_job_cancel(&library_id)
            .await
            .expect("request first run cancellation")
    );
    let active_jobs = database
        .sync_person_index_rebuild_jobs(1)
        .await
        .expect("keep a current run active");
    assert_eq!(active_jobs[0].status, "RUNNING");
    let active_job: (Option<String>, i64, i64, i64, Option<String>) = sqlx::query_as(
        "SELECT cursor_id, processed_count, total_count, cancel_requested, run_token
         FROM person_index_rebuild_jobs WHERE library_id = ?",
    )
    .bind(&library_id)
    .fetch_one(database.pool())
    .await
    .expect("current run fields");
    assert_eq!(
        active_job,
        (Some("item-a".to_owned()), 1, 2, 1, Some("run-a".to_owned()))
    );
    sqlx::query(
        "UPDATE person_index_rebuild_jobs
             SET updated_at = unixepoch() - 61
             WHERE library_id = ?",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("mark interrupted run stale");
    let recovered_jobs = database
        .sync_person_index_rebuild_jobs(1)
        .await
        .expect("recover stale run");
    assert_eq!(recovered_jobs[0].status, "QUEUED");
    let recovered_token: Option<String> =
        sqlx::query_scalar("SELECT run_token FROM person_index_rebuild_jobs WHERE library_id = ?")
            .bind(&library_id)
            .fetch_one(database.pool())
            .await
            .expect("read recovered token");
    assert_eq!(recovered_token, None);
    assert!(
        database
            .claim_person_index_rebuild_job(&library_id, "run-b")
            .await
            .expect("claim recovered run")
    );
    assert!(
        database
            .request_person_index_rebuild_job_cancel(&library_id)
            .await
            .expect("request cancellation")
    );
    assert!(
        database
            .request_person_index_rebuild_job(&library_id, 1)
            .await
            .expect("requeue job")
    );
    assert!(
        !database
            .finish_person_index_rebuild_job(&library_id, "run-a", "COMPLETED", None)
            .await
            .expect("ignore stale completion")
    );
    assert!(
        !database
            .finish_person_index_rebuild_job(&library_id, "run-b", "COMPLETED", None)
            .await
            .expect("ignore cancelled run completion")
    );
    assert!(
        database
            .claim_person_index_rebuild_job(&library_id, "run-c")
            .await
            .expect("claim requeued run")
    );
    assert!(
        database
            .update_person_index_rebuild_progress(&library_id, "run-a", "item-a", 1, 2)
            .await
            .expect("ignore stale progress")
            .is_none()
    );
    assert!(
        database
            .update_person_index_rebuild_progress(&library_id, "run-c", "item-c", 2, 2)
            .await
            .expect("update progress")
            .is_some()
    );
    assert!(
        database
            .finish_person_index_rebuild_job(&library_id, "run-c", "COMPLETED", None)
            .await
            .expect("finish current run")
    );
    let jobs = database
        .list_person_index_rebuild_jobs(0, 20)
        .await
        .expect("list jobs");
    assert_eq!(jobs[0].status, "COMPLETED");
    assert_eq!(jobs[0].processed_count, 2);
}

#[tokio::test]
async fn syncing_person_rebuild_jobs_batches_libraries_and_skips_noop_updates() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let mut enabled_library_ids = Vec::new();
    for index in 0..4 {
        let library = libraries
            .create_library(&format!("People {index}"), LibraryKind::Movie, false)
            .await
            .expect("enabled library");
        enabled_library_ids.push(library.id.to_string());
    }
    let disabled_library = libraries
        .create_library("Disabled People", LibraryKind::Movie, false)
        .await
        .expect("disabled library");
    let disabled_library_id = disabled_library.id.to_string();
    sqlx::query("UPDATE libraries SET is_enabled = 0 WHERE id = ?")
        .bind(&disabled_library_id)
        .execute(database.pool())
        .await
        .expect("disable library");

    database.reset_query_count();
    let jobs = database
        .sync_person_index_rebuild_jobs(1)
        .await
        .expect("create jobs for enabled libraries");
    assert_eq!(database.query_count(), 2);
    assert_eq!(jobs.len(), enabled_library_ids.len());
    let disabled_job_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_index_rebuild_jobs WHERE library_id = ?")
            .bind(&disabled_library_id)
            .fetch_one(database.pool())
            .await
            .expect("disabled library job count");
    assert_eq!(disabled_job_count, 0);

    sqlx::query("CREATE TABLE person_index_rebuild_update_probe (count INTEGER NOT NULL)")
        .execute(database.pool())
        .await
        .expect("create update probe");
    sqlx::query("INSERT INTO person_index_rebuild_update_probe (count) VALUES (0)")
        .execute(database.pool())
        .await
        .expect("initialize update probe");
    sqlx::query(
        "CREATE TRIGGER person_index_rebuild_update_probe_trigger
         AFTER UPDATE ON person_index_rebuild_jobs
         BEGIN
             UPDATE person_index_rebuild_update_probe SET count = count + 1;
         END",
    )
    .execute(database.pool())
    .await
    .expect("create update trigger");

    database.reset_query_count();
    database
        .sync_person_index_rebuild_jobs(1)
        .await
        .expect("sync unchanged jobs");
    assert_eq!(database.query_count(), 2);
    let unchanged_update_count: i64 =
        sqlx::query_scalar("SELECT count FROM person_index_rebuild_update_probe")
            .fetch_one(database.pool())
            .await
            .expect("unchanged update count");
    assert_eq!(unchanged_update_count, 0);

    sqlx::query(
        "UPDATE person_index_rebuild_jobs
         SET status = 'RUNNING', cursor_id = 'checkpoint', processed_count = 2,
             total_count = 4, cancel_requested = 1, run_token = 'run-token', error = 'old'
         WHERE library_id = ?",
    )
    .bind(&enabled_library_ids[0])
    .execute(database.pool())
    .await
    .expect("prepare old-schema job");
    sqlx::query("UPDATE person_index_rebuild_update_probe SET count = 0")
        .execute(database.pool())
        .await
        .expect("reset update probe");

    database.reset_query_count();
    database
        .sync_person_index_rebuild_jobs(2)
        .await
        .expect("reset jobs for new schema");
    assert_eq!(database.query_count(), 2);
    type PersonIndexRebuildSchemaResetRow = (
        i64,
        String,
        Option<String>,
        i64,
        i64,
        i64,
        Option<String>,
        Option<String>,
    );
    let schema_changed: PersonIndexRebuildSchemaResetRow = sqlx::query_as(
        "SELECT schema_version, status, cursor_id, processed_count, total_count,
                    cancel_requested, run_token, error
             FROM person_index_rebuild_jobs WHERE library_id = ?",
    )
    .bind(&enabled_library_ids[0])
    .fetch_one(database.pool())
    .await
    .expect("schema-changed job");
    assert_eq!(
        schema_changed,
        (2, "QUEUED".to_owned(), None, 0, 0, 0, None, None)
    );
    let schema_change_update_count: i64 =
        sqlx::query_scalar("SELECT count FROM person_index_rebuild_update_probe")
            .fetch_one(database.pool())
            .await
            .expect("schema change update count");
    assert_eq!(schema_change_update_count, enabled_library_ids.len() as i64);
}

#[tokio::test]
async fn person_index_keyset_pages_and_fingerprints_are_conservative() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("People", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    for item_id in ["item-a", "item-b", "item-c"] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    let first_page = database
        .list_person_index_item_ids(&library_id, None, 2)
        .await
        .expect("first keyset page");
    assert_eq!(first_page, ["item-a", "item-b"]);
    let second_page = database
        .list_person_index_item_ids(&library_id, first_page.last().map(String::as_str), 2)
        .await
        .expect("second keyset page");
    assert_eq!(second_page, ["item-c"]);
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-ab', ?, 'MOVIE', 'item-ab', 'item-ab', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("insert item before the cursor");
    let second_page_after_insert = database
        .list_person_index_item_ids(&library_id, first_page.last().map(String::as_str), 2)
        .await
        .expect("second keyset page after insert");
    assert_eq!(second_page_after_insert, ["item-c"]);
    sqlx::query("DELETE FROM media_items WHERE id = 'item-c'")
        .execute(database.pool())
        .await
        .expect("delete item after the cursor");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-z', ?, 'MOVIE', 'item-z', 'item-z', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("insert item after the cursor");
    let second_page_after_delete = database
        .list_person_index_item_ids(&library_id, first_page.last().map(String::as_str), 2)
        .await
        .expect("second keyset page after delete");
    assert_eq!(second_page_after_delete, ["item-z"]);

    database
        .replace_person_credits_with_fingerprint("item-a", &[], Some("fingerprint-a"))
        .await
        .expect("store fingerprint");
    assert!(
        database
            .person_index_item_state_is_current("item-a", Some("fingerprint-a"))
            .await
            .expect("same fingerprint")
    );
    assert!(
        !database
            .person_index_item_state_is_current("item-a", None)
            .await
            .expect("missing fingerprint must not be current")
    );
    assert!(
        !database
            .person_index_item_state_is_current("item-a", Some("fingerprint-b"))
            .await
            .expect("changed fingerprint")
    );
    sqlx::query(
        "UPDATE person_index_item_state
             SET relation_schema_version = 3
             WHERE item_id = 'item-a'",
    )
    .execute(database.pool())
    .await
    .expect("change relation schema version");
    assert!(
        !database
            .person_index_item_state_is_current("item-a", Some("fingerprint-a"))
            .await
            .expect("changed relation schema version")
    );
    database
        .clear_person_credits("item-a")
        .await
        .expect("clear person credits");
    assert!(
        !database
            .person_index_item_state_is_current("item-a", Some("fingerprint-a"))
            .await
            .expect("cleared relation must be rebuilt")
    );
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_metadata_candidate_selection_accepts_integer_boolean_flags() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let connection = DatabaseConfiguration::Postgres(PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .unwrap_or_else(|_| "55432".to_owned())
            .parse()
            .expect("test port"),
        database: std::env::var("POSTGRES_TEST_DATABASE").unwrap_or_else(|_| "lux".to_owned()),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    });
    let database = Database::connect_with_configuration(&config, &connection)
        .await
        .expect("PostgreSQL database");
    let library = LibraryService::new(database.clone())
        .create_library("Metadata selection", LibraryKind::Movie, false)
        .await
        .expect("library");
    let item_id = Uuid::now_v7().to_string();
    let candidate_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status,
                has_available_source
             ) VALUES (?, ?, 'MOVIE', 'Metadata selection', 'metadata selection',
                       'LOCAL_CONFIRMED', 1)",
    )
    .bind(&item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    sqlx::query(
        "INSERT INTO metadata_candidates (
                id, item_id, provider, provider_id, candidate_json, score, status
             ) VALUES (?, ?, 'TMDB', '603', '{}', 100, 'PENDING')",
    )
    .bind(&candidate_id)
    .bind(&item_id)
    .execute(database.pool())
    .await
    .expect("metadata candidate");

    let update = |keep_pending| SelectedMetadataUpdate {
        item_id: &item_id,
        candidate_id: &candidate_id,
        title: "Metadata selection",
        original_title: None,
        overview: None,
        production_year: None,
        premiere_date: None,
        last_air_date: None,
        status: None,
        original_language: None,
        rating: None,
        rating_source: None,
        provider_ids_json: "{}",
        metadata_scraper_id: None,
        metadata_fingerprint: &[],
        provenance_json: "{}",
        locked_fields_json: "[]",
        poster_fallback_required: false,
        keep_pending,
    };

    assert!(
        database
            .select_metadata_candidate(update(true))
            .await
            .expect("keep-pending selection")
    );
    let identification_status: String =
        sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("identification status");
    assert_eq!(identification_status, "PENDING");
    let candidate_status: String =
        sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
            .bind(&candidate_id)
            .fetch_one(database.pool())
            .await
            .expect("candidate status");
    assert_eq!(candidate_status, "PENDING");

    assert!(
        database
            .select_metadata_candidate(update(false))
            .await
            .expect("confirmed selection")
    );
    let identification_status: String =
        sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("confirmed identification status");
    assert_eq!(identification_status, "ONLINE_CONFIRMED");
    let candidate_status: String =
        sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
            .bind(&candidate_id)
            .fetch_one(database.pool())
            .await
            .expect("confirmed candidate status");
    assert_eq!(candidate_status, "SELECTED");
}

#[tokio::test]
async fn expired_web_playback_sessions_are_stopped_in_a_bounded_batch() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let setup = SetupService::new(database.clone()).expect("setup service");
    setup
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let user_id: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(database.pool())
        .await
        .expect("user");
    let library = LibraryService::new(database.clone())
        .create_library("Playback cleanup", LibraryKind::Movie, false)
        .await
        .expect("library");
    let item_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Playback cleanup', 'playback cleanup', 'LOCAL_CONFIRMED')",
    )
    .bind(&item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    database
        .insert_web_playback_session(NewWebPlaybackSession {
            id: "expired-session",
            user_id: &user_id,
            item_id: &item_id,
            media_source_id: None,
            play_session_id: "lux-web:expired-session",
            tier: 1,
            plan: "SERVER_HLS",
            temp_dir: Some("/config/web-playback/expired-session"),
            is_admin: true,
            expires_at: 99,
            now: 1,
        })
        .await
        .expect("web playback session");

    for index in 0..129 {
        let id = format!("expired-session-{index:03}");
        database
            .insert_web_playback_session(NewWebPlaybackSession {
                id: &id,
                user_id: &user_id,
                item_id: &item_id,
                media_source_id: None,
                play_session_id: &format!("lux-web:{id}"),
                tier: 1,
                plan: "SERVER_HLS",
                temp_dir: None,
                is_admin: true,
                expires_at: 99,
                now: 1,
            })
            .await
            .expect("additional web playback session");
    }

    database.reset_query_count();
    let expired = database
        .take_expired_web_playback_sessions(100)
        .await
        .expect("expired sessions");

    assert_eq!(expired.len(), 128);
    assert_eq!(database.query_count(), 2);
    assert_eq!(expired[0].id, "expired-session");
    let state: String =
        sqlx::query_scalar("SELECT state FROM web_playback_sessions WHERE id = 'expired-session'")
            .fetch_one(database.pool())
            .await
            .expect("session state");
    assert_eq!(state, "STOPPED");
    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM web_playback_sessions
         WHERE state = 'ACTIVE' AND expires_at < 100",
    )
    .fetch_one(database.pool())
    .await
    .expect("remaining expired sessions");
    assert_eq!(active_count, 2);
}

#[tokio::test]
async fn inactive_server_hls_sessions_are_stopped_in_a_bounded_batch() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let setup = SetupService::new(database.clone()).expect("setup service");
    setup
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let user_id: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(database.pool())
        .await
        .expect("user");
    let library = LibraryService::new(database.clone())
        .create_library("Playback stale cleanup", LibraryKind::Movie, false)
        .await
        .expect("library");
    let item_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Playback stale cleanup', 'playback stale cleanup', 'LOCAL_CONFIRMED')",
    )
    .bind(&item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    database
        .insert_web_playback_session(NewWebPlaybackSession {
            id: "inactive-session",
            user_id: &user_id,
            item_id: &item_id,
            media_source_id: None,
            play_session_id: "lux-web:inactive-session",
            tier: 4,
            plan: "SERVER_HLS",
            temp_dir: Some("/config/web-playback/inactive-session"),
            is_admin: true,
            expires_at: 10_000,
            now: 1_000,
        })
        .await
        .expect("web playback session");

    for index in 0..129 {
        let id = format!("inactive-session-{index:03}");
        database
            .insert_web_playback_session(NewWebPlaybackSession {
                id: &id,
                user_id: &user_id,
                item_id: &item_id,
                media_source_id: None,
                play_session_id: &format!("lux-web:{id}"),
                tier: 4,
                plan: "SERVER_HLS",
                temp_dir: None,
                is_admin: true,
                expires_at: 10_000,
                now: 1_000,
            })
            .await
            .expect("additional inactive web playback session");
        sqlx::query(
            "UPDATE web_playback_sessions
             SET last_heartbeat_at = 900
             WHERE id = ?",
        )
        .bind(&id)
        .execute(database.pool())
        .await
        .expect("stale additional heartbeat");
    }
    assert_eq!(
        database
            .accept_web_playback_event(NewWebPlaybackEvent {
                session_id: "inactive-session",
                user_id: &user_id,
                event_id: "playing-1",
                sequence: 0,
                state: "PLAYING",
                position_ticks: 0,
                duration_ticks: Some(10_000),
                now: 1_000,
            })
            .await
            .expect("playing event"),
        WebPlaybackEventClaim::Accepted
    );
    let last_heartbeat_at: i64 = sqlx::query_scalar(
        "SELECT last_heartbeat_at FROM web_playback_sessions WHERE id = 'inactive-session'",
    )
    .fetch_one(database.pool())
    .await
    .expect("heartbeat timestamp");
    assert_eq!(last_heartbeat_at, 1_000);
    sqlx::query(
        "UPDATE web_playback_sessions
         SET last_heartbeat_at = 900
         WHERE id = 'inactive-session'",
    )
    .execute(database.pool())
    .await
    .expect("stale heartbeat");

    database.reset_query_count();
    let inactive = database
        .take_inactive_web_playback_sessions(1_000, 90)
        .await
        .expect("inactive sessions");

    assert_eq!(inactive.len(), 128);
    assert_eq!(database.query_count(), 2);
    assert_eq!(inactive[0].id, "inactive-session");
    let state: String =
        sqlx::query_scalar("SELECT state FROM web_playback_sessions WHERE id = 'inactive-session'")
            .fetch_one(database.pool())
            .await
            .expect("session state");
    assert_eq!(state, "STOPPED");
    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM web_playback_sessions
         WHERE state = 'ACTIVE' AND plan = 'SERVER_HLS' AND last_heartbeat_at < 910",
    )
    .fetch_one(database.pool())
    .await
    .expect("remaining inactive sessions");
    assert_eq!(active_count, 2);
}

#[tokio::test]
async fn web_playback_cleanup_updates_each_kind_in_one_statement() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let setup = SetupService::new(database.clone()).expect("setup service");
    setup
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let user_id: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(database.pool())
        .await
        .expect("user");
    let library = LibraryService::new(database.clone())
        .create_library("Playback cleanup batch", LibraryKind::Movie, false)
        .await
        .expect("library");
    let item_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Playback cleanup batch',
                       'playback cleanup batch', 'LOCAL_CONFIRMED')",
    )
    .bind(&item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    for prefix in ["expired", "inactive"] {
        for index in 0..3 {
            let id = format!("{prefix}-session-{index}");
            database
                .insert_web_playback_session(NewWebPlaybackSession {
                    id: &id,
                    user_id: &user_id,
                    item_id: &item_id,
                    media_source_id: None,
                    play_session_id: &format!("lux-web:{id}"),
                    tier: 1,
                    plan: "SERVER_HLS",
                    temp_dir: None,
                    is_admin: true,
                    expires_at: if prefix == "expired" { 99 } else { 10_000 },
                    now: 1_000,
                })
                .await
                .expect("web playback session");
        }
    }
    sqlx::query(
        "UPDATE web_playback_sessions
         SET last_heartbeat_at = 900
         WHERE id LIKE 'inactive-session-%'",
    )
    .execute(database.pool())
    .await
    .expect("inactive heartbeat");

    database.reset_query_count();
    let expired = database
        .take_expired_web_playback_sessions(1_000)
        .await
        .expect("expired sessions");
    assert_eq!(expired.len(), 3);
    assert_eq!(database.query_count(), 2);

    database.reset_query_count();
    let inactive = database
        .take_inactive_web_playback_sessions(1_000, 90)
        .await
        .expect("inactive sessions");
    assert_eq!(inactive.len(), 3);
    assert_eq!(database.query_count(), 2);
}

#[tokio::test]
async fn user_updates_wait_for_a_concurrent_sqlite_writer() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user_id = Uuid::now_v7().to_string();
    database
        .insert_initial_user(&user_id, "admin", "Admin", "hash")
        .await
        .expect("user");

    let mut blocker = database.pool().acquire().await.expect("blocker connection");
    sqlx::query("BEGIN EXCLUSIVE")
        .execute(&mut *blocker)
        .await
        .expect("begin exclusive transaction");

    let update_database = database.clone();
    let update = tokio::spawn(async move {
        update_database
            .update_user(
                &user_id,
                UpdateUser {
                    display_name: Some("Updated"),
                    password_hash: None,
                    has_password: None,
                    is_disabled: None,
                    is_admin: None,
                    can_manage_server: None,
                    can_remote_access: None,
                    can_download: None,
                },
            )
            .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    sqlx::query("COMMIT")
        .execute(&mut *blocker)
        .await
        .expect("release exclusive transaction");
    let updated = update
        .await
        .expect("user update task")
        .expect("user update")
        .expect("updated user");

    assert_eq!(updated.display_name, "Updated");
}

#[tokio::test]
async fn database_lifecycle_cleanup_is_one_time_and_preserves_retry_state() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Cleanup", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("root directory");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("root path"))
        .await
        .expect("root")
        .root;
    let library_id = library.id.to_string();
    let root_id = root.id.to_string();
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await
        .expect("current timestamp");

    for (job_id, job_type, status, cursor, current_item, cancel_requested) in [
        (
            "cleanup-completed",
            "RECONCILE_LIBRARY",
            "COMPLETED",
            Some("completed-cursor"),
            Some("completed-item"),
            1_i64,
        ),
        (
            "cleanup-failed",
            "INCREMENTAL_SCAN",
            "FAILED",
            Some("failed-cursor"),
            Some("failed-item"),
            0_i64,
        ),
        (
            "cleanup-active",
            "INCREMENTAL_SCAN",
            "RUNNING",
            Some("active-cursor"),
            Some("active-item"),
            0_i64,
        ),
    ] {
        sqlx::query(
            "INSERT INTO scan_jobs (
                id, library_id, job_type, status, generation, cursor,
                current_item, cancel_requested, scan_phase, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'IDLE', ?, ?)",
        )
        .bind(job_id)
        .bind(&library_id)
        .bind(job_type)
        .bind(status)
        .bind(format!("generation-{job_id}"))
        .bind(cursor)
        .bind(current_item)
        .bind(cancel_requested)
        .bind(now - 10 * 86_400)
        .bind(now - 10 * 86_400)
        .execute(database.pool())
        .await
        .expect("scan job");
    }
    sqlx::query(
        "INSERT INTO scan_jobs (
            id, library_id, job_type, status, generation, cursor,
            current_item, cancel_requested, scan_phase, created_at, updated_at
         ) VALUES ('cleanup-postprocessing', ?, 'RECONCILE_LIBRARY', 'COMPLETED', ?,
                   'postprocessing-cursor', 'postprocessing-item', 0, 'POSTPROCESSING', ?, ?)",
    )
    .bind(&library_id)
    .bind("generation-cleanup-postprocessing")
    .bind(now - 10 * 86_400)
    .bind(now - 10 * 86_400)
    .execute(database.pool())
    .await
    .expect("postprocessing scan job");

    sqlx::query(
        "INSERT INTO scan_job_paths (job_id, library_root_id, relative_path, change_kind)
         VALUES ('cleanup-completed', ?, 'completed.mkv', 'MODIFY'),
                ('cleanup-failed', ?, 'failed.mkv', 'MODIFY'),
                ('cleanup-active', ?, 'active.mkv', 'MODIFY')",
    )
    .bind(&root_id)
    .bind(&root_id)
    .bind(&root_id)
    .execute(database.pool())
    .await
    .expect("scan job paths");
    sqlx::query(
        "INSERT INTO reconciliation_scan_entries (
            job_id, library_root_id, relative_path, entry_type
         ) VALUES ('cleanup-completed', ?, 'completed', 'FILE'),
                  ('cleanup-failed', ?, 'failed', 'FILE'),
                  ('cleanup-active', ?, 'active', 'FILE')",
    )
    .bind(&root_id)
    .bind(&root_id)
    .bind(&root_id)
    .execute(database.pool())
    .await
    .expect("reconciliation entries");

    for (job_id, target_id, metadata_state) in [
        ("cleanup-completed", "completed-target", "DONE"),
        ("cleanup-failed", "failed-target-done", "DONE"),
        ("cleanup-failed", "failed-target-retry", "FAILED"),
        ("cleanup-active", "active-target", "PENDING"),
        ("cleanup-postprocessing", "postprocessing-target", "PENDING"),
    ] {
        sqlx::query(
            "INSERT INTO scan_job_targets (
                job_id, target_type, target_id, item_id, change_kind,
                probe_state, metadata_state, thumbnail_state
             ) VALUES (?, 'ITEM', ?, ?, 'CHANGED', 'SKIPPED', ?, 'SKIPPED')",
        )
        .bind(job_id)
        .bind(target_id)
        .bind(target_id)
        .bind(metadata_state)
        .execute(database.pool())
        .await
        .expect("scan target");
    }

    sqlx::query("DROP TABLE scan_job_events")
        .execute(database.pool())
        .await
        .expect("drop legacy log table to verify cleanup does not query it");

    let report = database
        .run_database_lifecycle_cleanup()
        .await
        .expect("cleanup")
        .expect("cleanup should be claimed");
    assert_eq!(report.scan_job_paths_deleted, 1);
    assert_eq!(report.reconciliation_entries_deleted, 1);
    assert_eq!(report.scan_job_targets_deleted, 2);
    assert_eq!(report.scan_jobs_summarized, 2);

    let remaining_paths: Vec<String> =
        sqlx::query_scalar("SELECT job_id FROM scan_job_paths ORDER BY job_id")
            .fetch_all(database.pool())
            .await
            .expect("remaining paths");
    assert_eq!(remaining_paths, ["cleanup-active", "cleanup-failed"]);
    let remaining_entries: Vec<String> =
        sqlx::query_scalar("SELECT job_id FROM reconciliation_scan_entries ORDER BY job_id")
            .fetch_all(database.pool())
            .await
            .expect("remaining entries");
    assert_eq!(remaining_entries, ["cleanup-active", "cleanup-failed"]);
    let remaining_targets: Vec<(String, String)> =
        sqlx::query_as("SELECT job_id, target_id FROM scan_job_targets ORDER BY job_id, target_id")
            .fetch_all(database.pool())
            .await
            .expect("remaining targets");
    assert_eq!(
        remaining_targets,
        [
            ("cleanup-active".to_owned(), "active-target".to_owned()),
            (
                "cleanup-failed".to_owned(),
                "failed-target-retry".to_owned()
            ),
            (
                "cleanup-postprocessing".to_owned(),
                "postprocessing-target".to_owned()
            ),
        ]
    );
    let summary: (Option<String>, Option<String>, i64) = sqlx::query_as(
        "SELECT cursor, current_item, cancel_requested
         FROM scan_jobs WHERE id = 'cleanup-completed'",
    )
    .fetch_one(database.pool())
    .await
    .expect("completed summary");
    assert_eq!(summary, (None, None, 0));
    let active_cursor: Option<String> =
        sqlx::query_scalar("SELECT cursor FROM scan_jobs WHERE id = 'cleanup-active'")
            .fetch_one(database.pool())
            .await
            .expect("active job");
    assert_eq!(active_cursor.as_deref(), Some("active-cursor"));
    let postprocessing_summary: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT cursor, current_item FROM scan_jobs
         WHERE id = 'cleanup-postprocessing'",
    )
    .fetch_one(database.pool())
    .await
    .expect("postprocessing job");
    assert_eq!(
        postprocessing_summary,
        (
            Some("postprocessing-cursor".to_owned()),
            Some("postprocessing-item".to_owned())
        )
    );

    assert!(
        database
            .run_database_lifecycle_cleanup()
            .await
            .expect("second cleanup")
            .is_none()
    );
    let marker: String = sqlx::query_scalar(
        "SELECT value FROM lux_meta WHERE key = 'database_lifecycle_cleanup_v1'",
    )
    .fetch_one(database.pool())
    .await
    .expect("cleanup marker");
    assert_eq!(marker, "COMPLETED");

    assert!(
        database
            .run_database_lifecycle_cleanup()
            .await
            .expect("recurring cleanup")
            .is_none()
    );
}

#[tokio::test]
async fn reconciliation_batch_commit_is_atomic_and_counts_confirmed_and_missing_entries() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Atomic scan", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("root directory");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 root"))
        .await
        .expect("library root")
        .root;
    let library_id = library.id.to_string();
    let root_id = root.id.to_string();

    let job_id = "reconciliation-atomic-test";
    let generation = "generation-1";
    sqlx::query(
        "INSERT INTO scan_jobs (
             id, library_id, job_type, status, generation, total_count,
             discovery_completed, processed_count
         ) VALUES (?, ?, 'RECONCILE_LIBRARY', 'RUNNING', ?, 0, 1, 0)",
    )
    .bind(job_id)
    .bind(&library_id)
    .bind(generation)
    .execute(database.pool())
    .await
    .expect("scan job");

    let first_path = "Atomic.Movie.2024.mkv";
    let second_path = "Already.Done.Movie.2023.mkv";
    for path in [first_path, second_path] {
        sqlx::query(
            "INSERT INTO reconciliation_scan_entries (
                 job_id, library_root_id, relative_path, entry_type, status
             ) VALUES (?, ?, ?, 'FILE', 'PENDING')",
        )
        .bind(job_id)
        .bind(&root_id)
        .bind(path)
        .execute(database.pool())
        .await
        .expect("pending work item");
    }
    sqlx::query(
        "INSERT INTO reconciliation_scan_entries (
             job_id, library_root_id, relative_path, entry_type, status
         ) VALUES (?, ?, '', 'DIRECTORY', 'PENDING')",
    )
    .bind(job_id)
    .bind(&root_id)
    .execute(database.pool())
    .await
    .expect("pending directory work item");

    let movie_file = NewMovieFile {
        filesystem_entry_id: "atomic-filesystem-entry".to_owned(),
        source_id: "atomic-source".to_owned(),
        relative_path: first_path.to_owned(),
        size: 7,
        modified_at: 1,
        fingerprint: vec![1, 2, 3],
        title: "Atomic Movie".to_owned(),
        sort_title: "atomic movie".to_owned(),
        original_title: "Atomic Movie".to_owned(),
        production_year: Some(2024),
        provider_ids_json: None,
        source_kind: "LOCAL_FILE".to_owned(),
        strm_target_kind: None,
        edition_name: None,
        quality_label: None,
        container: "mkv".to_owned(),
        external_url: None,
    };
    let entries = vec![
        StoredReconciliationScanEntry {
            library_root_id: root_id.clone(),
            relative_path: first_path.to_owned(),
        },
        StoredReconciliationScanEntry {
            library_root_id: root_id.clone(),
            relative_path: second_path.to_owned(),
        },
    ];
    let new_paths = vec![first_path.to_owned()];
    let missing_paths = vec![second_path.to_owned()];

    sqlx::query(
        "CREATE TRIGGER fail_reconciliation_targets
         BEFORE INSERT ON scan_job_targets
         BEGIN SELECT RAISE(ABORT, 'injected target failure'); END",
    )
    .execute(database.pool())
    .await
    .expect("failure trigger");

    let batch = ReconciliationBatchCommit {
        job_id,
        library_id: &library_id,
        library_root_id: &root_id,
        generation,
        entries: &entries,
        movie_files: std::slice::from_ref(&movie_file),
        episode_files: &[],
        seen_entry_ids: &[],
        missing_paths: &missing_paths,
        new_paths: &new_paths,
        changed_paths: &[],
        sidecar_paths: &[],
    };
    assert!(database.commit_reconciliation_batch(&batch).await.is_err());

    let rollback_state: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM filesystem_entries),
             (SELECT COUNT(*) FROM media_sources),
             (SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?),
             (SELECT processed_count FROM scan_jobs WHERE id = ?)",
    )
    .bind(job_id)
    .bind(job_id)
    .fetch_one(database.pool())
    .await
    .expect("rollback state");
    assert_eq!(rollback_state, (0, 0, 0, 0));
    let pending_after_failure: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = ? AND entry_type = 'FILE' AND status = 'PENDING'",
    )
    .bind(job_id)
    .fetch_one(database.pool())
    .await
    .expect("pending after failure");
    assert_eq!(pending_after_failure, 2);

    sqlx::query("DROP TRIGGER fail_reconciliation_targets")
        .execute(database.pool())
        .await
        .expect("drop failure trigger");
    database.reset_query_count();
    let committed = database
        .commit_reconciliation_batch(&batch)
        .await
        .expect("retry batch");
    assert_eq!(committed.confirmed_entries, 2);
    assert!(committed.metadata_targets_changed);
    assert_eq!(database.query_count(), 11);

    let second_commit = database
        .commit_reconciliation_batch(&batch)
        .await
        .expect("idempotent confirmation");
    assert_eq!(second_commit.confirmed_entries, 0);
    assert!(!second_commit.metadata_targets_changed);

    let entry_states: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, COUNT(*)
         FROM reconciliation_scan_entries
         WHERE job_id = ?
         GROUP BY status
         ORDER BY status",
    )
    .bind(job_id)
    .fetch_all(database.pool())
    .await
    .expect("entry states");
    assert_eq!(
        entry_states,
        vec![("DONE".to_owned(), 1), ("PENDING".to_owned(), 1)]
    );

    let final_state: (i64, i64, i64, i64, i64, Option<String>) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM filesystem_entries),
             (SELECT COUNT(*) FROM media_sources),
             (SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?),
             (SELECT processed_count FROM scan_jobs WHERE id = ?),
             (SELECT total_count FROM scan_jobs WHERE id = ?),
             (SELECT cursor FROM scan_jobs WHERE id = ?)",
    )
    .bind(job_id)
    .bind(job_id)
    .bind(job_id)
    .bind(job_id)
    .fetch_one(database.pool())
    .await
    .expect("final state");
    assert_eq!(final_state, (1, 1, 2, 2, 2, Some(second_path.to_owned())));
    assert!(final_state.4 >= final_state.3);
}

#[tokio::test]
async fn empty_reconciliation_batch_is_a_noop() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let batch = ReconciliationBatchCommit {
        job_id: "missing-job",
        library_id: "missing-library",
        library_root_id: "missing-root",
        generation: "missing-generation",
        entries: &[],
        movie_files: &[],
        episode_files: &[],
        seen_entry_ids: &[],
        missing_paths: &[],
        new_paths: &[],
        changed_paths: &[],
        sidecar_paths: &[],
    };

    database.reset_query_count();
    let result = database
        .commit_reconciliation_batch(&batch)
        .await
        .expect("empty batch should be ignored");

    assert_eq!(result.confirmed_entries, 0);
    assert_eq!(result.created_items, 0);
    assert!(!result.metadata_targets_changed);
    assert_eq!(database.query_count(), 0);
}

#[tokio::test]
async fn fill_missing_job_creation_coalesces_active_items() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let media_root = temp_dir.path().join("Movies");
    for title in ["First Movie (2025)", "Second Movie (2025)"] {
        let directory = media_root.join(title);
        tokio::fs::create_dir_all(&directory).await?;
        tokio::fs::write(
            directory.join(format!("{}.mkv", title.replace(' ', "."))),
            b"video",
        )
        .await?;
    }
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Fill missing coalesce", LibraryKind::Movie, false)
        .await?;
    LibraryService::new(database.clone())
        .add_root(library.id, media_root.to_str().ok_or("media root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_ids: Vec<String> = database
        .query_scalar(
            "SELECT id FROM media_items
             WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY id",
        )
        .bind(library.id.to_string())
        .fetch_all(database.pool())
        .await?;
    assert_eq!(item_ids.len(), 2);
    let library_id = library.id.to_string();
    let first_job = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[..1])
        .await?;
    let merged_job = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids)
        .await?;
    assert_eq!(merged_job, first_job);
    let repeated_job = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids)
        .await?;
    assert_eq!(repeated_job, first_job);
    let job_count: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM metadata_reidentify_jobs
             WHERE library_id = ? AND mode = 'FILL_MISSING'",
        )
        .bind(&library_id)
        .fetch_one(database.pool())
        .await?;
    let item_count: i64 = database
        .query_scalar("SELECT COUNT(*) FROM metadata_reidentify_job_items WHERE job_id = ?")
        .bind(&first_job)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(job_count, 1);
    assert_eq!(item_count, 2);
    assert!(
        database
            .request_metadata_reidentify_job_cancel(&first_job)
            .await?
    );
    database
        .finish_metadata_reidentify_job(&first_job, "COMPLETED", None)
        .await?;
    let cancelled_items: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM metadata_reidentify_job_items
             WHERE job_id = ? AND status = 'FAILED'",
        )
        .bind(&first_job)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(cancelled_items, 2);
    assert!(database.retry_metadata_reidentify_job(&first_job).await?);
    let pending_items: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM metadata_reidentify_job_items
             WHERE job_id = ? AND status = 'PENDING'",
        )
        .bind(&first_job)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(pending_items, 2);

    sqlx::query(
        "UPDATE metadata_reidentify_jobs SET status = 'DEFERRED', updated_at = unixepoch()
         WHERE id = ?",
    )
    .bind(&first_job)
    .execute(database.pool())
    .await
    .expect("defer fill-missing job after provider failure");
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'FAILED', error = 'SCRAPER_UNAVAILABLE'
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(&first_job)
    .bind(&item_ids[0])
    .execute(database.pool())
    .await
    .expect("record deferred provider failure");
    let repeated_deferred_job = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[..1])
        .await?;
    assert_eq!(repeated_deferred_job, first_job);

    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET error = 'METADATA_WRITE_FAILED'
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(&first_job)
    .bind(&item_ids[0])
    .execute(database.pool())
    .await
    .expect("change deferred error to a non-provider failure");
    let retried_non_provider_failure = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[..1])
        .await?;
    assert_ne!(retried_non_provider_failure, first_job);

    sqlx::query(
        "UPDATE metadata_reidentify_jobs SET status = 'DEFERRED', updated_at = unixepoch() - 3601
         WHERE id = ?",
    )
    .bind(&retried_non_provider_failure)
    .execute(database.pool())
    .await
    .expect("age deferred fill-missing job");
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'FAILED', error = 'SCRAPER_UNAVAILABLE'
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(&retried_non_provider_failure)
    .bind(&item_ids[0])
    .execute(database.pool())
    .await
    .expect("record old provider failure");
    let retried_expired_deferred_job = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[..1])
        .await?;
    assert_ne!(retried_expired_deferred_job, retried_non_provider_failure);
    Ok(())
}

#[tokio::test]
async fn fill_missing_job_creation_reuses_later_queued_capacity()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Fill missing queued capacity", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let item_ids = (0..331)
        .map(|index| format!("queue-capacity-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        database
            .query(
                "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
    }

    database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[..100])
        .await?;
    database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[100..200])
        .await?;
    database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[200..250])
        .await?;
    let initial_jobs: Vec<(String, i64)> = database
        .query_as(
            "SELECT id, total_count FROM metadata_reidentify_jobs
             WHERE library_id = ? AND mode = 'FILL_MISSING'
             ORDER BY created_at, id",
        )
        .bind(&library_id)
        .fetch_all(database.pool())
        .await?;
    assert_eq!(
        initial_jobs
            .iter()
            .map(|(_, count)| *count)
            .collect::<Vec<_>>(),
        vec![100, 100, 50]
    );

    let reused_job_id = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[250..251])
        .await?;
    assert_eq!(reused_job_id, initial_jobs[2].0);

    let jobs: Vec<(String, i64)> = database
        .query_as(
            "SELECT id, total_count FROM metadata_reidentify_jobs
             WHERE library_id = ? AND mode = 'FILL_MISSING'
             ORDER BY created_at, id",
        )
        .bind(&library_id)
        .fetch_all(database.pool())
        .await?;
    assert_eq!(
        jobs.len(),
        3,
        "an available queued slot must prevent a new job"
    );
    assert_eq!(
        jobs.iter().map(|(_, count)| *count).collect::<Vec<_>>(),
        vec![100, 100, 51]
    );

    let first_reused_job_id = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[251..])
        .await?;
    assert_eq!(first_reused_job_id, initial_jobs[2].0);
    let jobs: Vec<(String, i64)> = database
        .query_as(
            "SELECT id, total_count FROM metadata_reidentify_jobs
             WHERE library_id = ? AND mode = 'FILL_MISSING'
             ORDER BY created_at, id",
        )
        .bind(&library_id)
        .fetch_all(database.pool())
        .await?;
    assert_eq!(jobs.len(), 4);
    assert_eq!(
        jobs.iter().map(|(_, count)| *count).collect::<Vec<_>>(),
        vec![100, 100, 100, 31]
    );
    let queued_items: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM metadata_reidentify_job_items
             WHERE job_id IN (
                 SELECT id FROM metadata_reidentify_jobs
                 WHERE library_id = ? AND mode = 'FILL_MISSING'
             )",
        )
        .bind(&library_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(queued_items, 331);
    Ok(())
}

#[tokio::test]
async fn legacy_fill_missing_job_keeps_empty_request_snapshot_defaults()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Snapshot migration", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let item_id = "legacy-fill-item";
    database
        .query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    let job_id = database
        .create_or_merge_fill_missing_job(&library_id, &[item_id.to_owned()])
        .await?;
    let snapshot: (Option<Vec<u8>>, String, Option<Vec<u8>>, String) = database
        .query_as(
            "SELECT request_fingerprint, request_capabilities_json,
                    claimed_request_fingerprint, claimed_request_capabilities_json
             FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(snapshot, (None, "[]".to_owned(), None, "[]".to_owned()));
    Ok(())
}

#[tokio::test]
async fn changed_local_fill_request_during_running_item_is_not_lost()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Running Request Movie (2025)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Running.Request.Movie.2025.mkv"), b"video").await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Running fill request", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("media root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = database
        .query_scalar(
            "SELECT id FROM media_items
             WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY id LIMIT 1",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?;
    let library_id = library.id.to_string();

    let first_fingerprint = b"fill-request-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", first_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", first_fingerprint)
            .await?
    );
    let first_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: first_fingerprint,
        is_missing: true,
        checked_at: 10,
    }];
    let first_commit = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &first_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert_eq!(first_commit.scheduled_job_ids.len(), 1);
    let job_id = &first_commit.scheduled_job_ids[0];
    sqlx::query(
        "CREATE TABLE fill_request_write_probe (
             item_inserts INTEGER NOT NULL DEFAULT 0,
             item_updates INTEGER NOT NULL DEFAULT 0,
             job_inserts INTEGER NOT NULL DEFAULT 0
         );
         INSERT INTO fill_request_write_probe DEFAULT VALUES;
         CREATE TRIGGER fill_request_item_insert_probe AFTER INSERT
         ON metadata_reidentify_job_items
         BEGIN
             UPDATE fill_request_write_probe SET item_inserts = item_inserts + 1;
         END;
         CREATE TRIGGER fill_request_item_update_probe AFTER UPDATE
         ON metadata_reidentify_job_items
         BEGIN
             UPDATE fill_request_write_probe SET item_updates = item_updates + 1;
         END;
         CREATE TRIGGER fill_request_job_insert_probe AFTER INSERT
         ON metadata_reidentify_jobs
         BEGIN
             UPDATE fill_request_write_probe SET job_inserts = job_inserts + 1;
         END;",
    )
    .execute(database.pool())
    .await?;
    let repeated_request = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(repeated_request.scheduled_job_ids.is_empty());
    let repeated_writes: (i64, i64, i64) = database
        .query_as("SELECT item_inserts, item_updates, job_inserts FROM fill_request_write_probe")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(repeated_writes, (0, 0, 0), "same input performs no job DML");

    let queued_fingerprint = b"fill-request-v2";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", queued_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", queued_fingerprint)
            .await?
    );
    let queued_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: queued_fingerprint,
        is_missing: true,
        checked_at: 11,
    }];
    let queued_commit = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &queued_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(queued_commit.scheduled_job_ids.is_empty());
    let changed_queued_writes: (i64, i64, i64) = database
        .query_as("SELECT item_inserts, item_updates, job_inserts FROM fill_request_write_probe")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        changed_queued_writes,
        (0, 1, 0),
        "queued input changes update the existing job item once"
    );
    let queued_snapshot: Vec<u8> = database
        .query_scalar(
            "SELECT request_fingerprint FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(job_id)
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(queued_snapshot, queued_fingerprint);

    assert!(database.claim_metadata_reidentify_job(job_id).await?);
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(job_id, 1)
            .await?,
        vec![item_id.clone()]
    );
    let same_running_request = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(same_running_request.scheduled_job_ids.is_empty());

    let second_fingerprint = b"fill-request-v3";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", second_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", second_fingerprint)
            .await?
    );
    let second_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: second_fingerprint,
        is_missing: true,
        checked_at: 12,
    }];
    let second_commit = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &second_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(second_commit.scheduled_job_ids.is_empty());

    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "CREDITS", second_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "CREDITS", second_fingerprint)
            .await?
    );
    let added_capability_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "CREDITS",
        input_fingerprint: second_fingerprint,
        is_missing: true,
        checked_at: 13,
    }];
    let changed_capability_commit = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &added_capability_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(changed_capability_commit.scheduled_job_ids.is_empty());
    let latest_capabilities: String = database
        .query_scalar(
            "SELECT request_capabilities_json FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(job_id)
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(latest_capabilities, "[\"CREDITS\",\"POSTER\"]");

    database
        .finish_metadata_reidentify_item(job_id, &item_id, "COMPLETED", 1, None)
        .await?;
    assert_eq!(
        database.next_metadata_reidentify_item(job_id).await?,
        Some(item_id.clone()),
        "the changed request must be claimable after the active item finishes"
    );
    let processed_count: i64 = database
        .query_scalar("SELECT processed_count FROM metadata_reidentify_jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(processed_count, 0, "a requeued item is not yet processed");

    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(job_id, 1)
            .await?,
        vec![item_id.clone()]
    );
    assert_eq!(
        database
            .fail_running_metadata_reidentify_items(job_id, "WORKER_FAILED")
            .await?,
        1
    );
    database
        .finish_metadata_reidentify_job(job_id, "FAILED", Some("ITEM_FAILED"))
        .await?;
    assert!(database.retry_metadata_reidentify_job(job_id).await?);
    assert!(database.claim_metadata_reidentify_job(job_id).await?);
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(job_id, 1)
            .await?,
        vec![item_id.clone()]
    );
    let claimed_after_worker_retry: Vec<u8> = database
        .query_scalar(
            "SELECT claimed_request_fingerprint FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(job_id)
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(claimed_after_worker_retry, second_fingerprint);
    database
        .finish_metadata_reidentify_item(job_id, &item_id, "COMPLETED", 1, None)
        .await?;
    assert_eq!(database.next_metadata_reidentify_item(job_id).await?, None);
    let completed_count: i64 = database
        .query_scalar("SELECT processed_count FROM metadata_reidentify_jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(completed_count, 1, "only the final pass is counted");

    database
        .query("UPDATE metadata_reidentify_jobs SET status = 'QUEUED' WHERE id = ?")
        .bind(job_id)
        .execute(database.pool())
        .await?;
    let completed_item_new_fingerprint = b"fill-request-completed-new-input";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                completed_item_new_fingerprint,
            )
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                completed_item_new_fingerprint,
            )
            .await?
    );
    let completed_item_new_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: completed_item_new_fingerprint,
        is_missing: true,
        checked_at: 12,
    }];
    let completed_item_new_commit = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &completed_item_new_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(completed_item_new_commit.scheduled_job_ids.is_empty());
    assert_eq!(
        database.next_metadata_reidentify_item(job_id).await?,
        Some(item_id.clone()),
        "a changed request reopens a completed item in a queued retry"
    );
    let requeued_processed_count: i64 = database
        .query_scalar("SELECT processed_count FROM metadata_reidentify_jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(requeued_processed_count, 0);
    assert!(database.claim_metadata_reidentify_job(job_id).await?);
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(job_id, 1)
            .await?,
        vec![item_id.clone()]
    );
    database
        .finish_metadata_reidentify_item(job_id, &item_id, "COMPLETED", 1, None)
        .await?;

    let cancelled_fingerprint = b"fill-request-v4";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", cancelled_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", cancelled_fingerprint)
            .await?
    );
    let cancelled_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: cancelled_fingerprint,
        is_missing: true,
        checked_at: 13,
    }];
    let cancelled_commit = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &cancelled_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    let cancelled_job_id = &cancelled_commit.scheduled_job_ids[0];
    assert!(
        database
            .claim_metadata_reidentify_job(cancelled_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(cancelled_job_id, 1)
            .await?,
        vec![item_id.clone()]
    );
    let newer_cancelled_fingerprint = b"fill-request-v5";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                newer_cancelled_fingerprint,
            )
            .await?
    );
    assert!(database
        .claim_item_metadata_completeness_check(
            &item_id,
            "POSTER",
            newer_cancelled_fingerprint,
        )
        .await?);
    let newer_cancelled_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: newer_cancelled_fingerprint,
        is_missing: true,
        checked_at: 14,
    }];
    database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &newer_cancelled_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(
        database
            .request_metadata_reidentify_job_cancel(cancelled_job_id)
            .await?
    );
    database
        .finish_metadata_reidentify_item(cancelled_job_id, &item_id, "COMPLETED", 1, None)
        .await?;
    assert_eq!(
        database
            .next_metadata_reidentify_item(cancelled_job_id)
            .await?,
        None,
        "a cancellation request must prevent a follow-up pass"
    );

    let worker_error_item_id = "fill-request-worker-error-item";
    database
        .query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Worker Error', 'worker error', 'LOCAL_CONFIRMED')",
        )
        .bind(worker_error_item_id)
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    let worker_first_fingerprint = b"worker-input-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                worker_error_item_id,
                "POSTER",
                worker_first_fingerprint,
            )
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                worker_error_item_id,
                "POSTER",
                worker_first_fingerprint,
            )
            .await?
    );
    let worker_first_result = [NewItemMetadataCompletenessResult {
        item_id: worker_error_item_id,
        capability: "POSTER",
        input_fingerprint: worker_first_fingerprint,
        is_missing: true,
        checked_at: 15,
    }];
    let worker_first_commit = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &worker_first_result,
            &[worker_error_item_id.into()],
        )
        .await?;
    let worker_job_id = &worker_first_commit.scheduled_job_ids[0];
    assert!(
        database
            .claim_metadata_reidentify_job(worker_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(worker_job_id, 1)
            .await?,
        vec![worker_error_item_id]
    );
    let worker_latest_fingerprint = b"worker-input-v2";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                worker_error_item_id,
                "POSTER",
                worker_latest_fingerprint,
            )
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                worker_error_item_id,
                "POSTER",
                worker_latest_fingerprint,
            )
            .await?
    );
    let worker_latest_result = [NewItemMetadataCompletenessResult {
        item_id: worker_error_item_id,
        capability: "POSTER",
        input_fingerprint: worker_latest_fingerprint,
        is_missing: true,
        checked_at: 16,
    }];
    database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &worker_latest_result,
            &[worker_error_item_id.into()],
        )
        .await?;
    assert_eq!(
        database
            .fail_running_metadata_reidentify_items(worker_job_id, "WORKER_FAILED")
            .await?,
        1
    );
    let latest_after_failure: Vec<u8> = database
        .query_scalar(
            "SELECT request_fingerprint FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(worker_job_id)
        .bind(worker_error_item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(latest_after_failure, worker_latest_fingerprint);
    database
        .finish_metadata_reidentify_job(worker_job_id, "FAILED", Some("ITEM_FAILED"))
        .await?;
    assert!(
        database
            .retry_metadata_reidentify_job(worker_job_id)
            .await?
    );
    assert!(
        database
            .claim_metadata_reidentify_job(worker_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(worker_job_id, 1)
            .await?,
        vec![worker_error_item_id]
    );
    let claimed_after_retry: Vec<u8> = database
        .query_scalar(
            "SELECT claimed_request_fingerprint FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(worker_job_id)
        .bind(worker_error_item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(claimed_after_retry, worker_latest_fingerprint);
    Ok(())
}

#[tokio::test]
async fn person_credit_page_replacement_commits_all_items_atomically() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("People", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    for item_id in ["credit-page-a", "credit-page-b"] {
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }

    let credit = |person_id: &str| NewPersonCredit {
        person_id: person_id.to_owned(),
        lux_person_id: None,
        person_type: "Actor".to_owned(),
        person_name: person_id.to_owned(),
        provider: "tmdb".to_owned(),
        role: "Actor".to_owned(),
        sort_order: 0,
        biography: None,
        birthday: None,
        deathday: None,
        known_for_department: None,
        place_of_birth: None,
        provider_ids: std::collections::BTreeMap::new(),
        genres: Vec::new(),
        tags: Vec::new(),
        production_locations: Vec::new(),
        premiere_date: None,
        production_year: None,
        taglines: Vec::new(),
    };
    let credits_a = [credit("actor-a")];
    let credits_b = [credit("actor-b")];
    let replacements = [
        ("credit-page-a", credits_a.as_slice(), Some("fingerprint-a")),
        ("credit-page-b", credits_b.as_slice(), Some("fingerprint-b")),
    ];

    sqlx::query(
        "CREATE TRIGGER fail_second_person_credit_page_state
         BEFORE INSERT ON person_index_item_state
         WHEN NEW.item_id = 'credit-page-b'
         BEGIN SELECT RAISE(ABORT, 'forced second item failure'); END",
    )
    .execute(database.pool())
    .await
    .expect("failure trigger");
    assert!(
        database
            .replace_person_credits_batch_with_fingerprint(&replacements)
            .await
            .is_err()
    );
    let row_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM person_credits
         WHERE item_id IN ('credit-page-a', 'credit-page-b')",
    )
    .fetch_one(database.pool())
    .await
    .expect("credits after rollback");
    assert_eq!(row_count, 0, "the failed page must roll back both items");
    let state_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM person_index_item_state
         WHERE item_id IN ('credit-page-a', 'credit-page-b')",
    )
    .fetch_one(database.pool())
    .await
    .expect("states after rollback");
    assert_eq!(state_count, 0, "the failed page must roll back both states");

    sqlx::query("DROP TRIGGER fail_second_person_credit_page_state")
        .execute(database.pool())
        .await
        .expect("drop failure trigger");
    database
        .replace_person_credits_batch_with_fingerprint(&replacements)
        .await
        .expect("replace whole page");
    assert!(
        database
            .person_index_item_state_is_current("credit-page-a", Some("fingerprint-a"))
            .await
            .expect("first item fingerprint")
    );
    assert!(
        database
            .person_index_item_state_is_current("credit-page-b", Some("fingerprint-b"))
            .await
            .expect("second item fingerprint")
    );
    let stored_credit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM person_credits
         WHERE item_id IN ('credit-page-a', 'credit-page-b')",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored page credits");
    assert_eq!(stored_credit_count, 2);
}

#[tokio::test]
async fn person_manifest_restore_pending_skips_unchanged_state_updates() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    database
        .mark_person_manifest_restore_pending(3)
        .await
        .expect("mark restore pending");
    sqlx::query(
        "CREATE TABLE person_manifest_restore_update_probe (count INTEGER NOT NULL);
         INSERT INTO person_manifest_restore_update_probe (count) VALUES (0);
         CREATE TRIGGER person_manifest_restore_update_probe_trigger
         AFTER UPDATE ON person_manifest_restore_state
         BEGIN
             UPDATE person_manifest_restore_update_probe SET count = count + 1;
         END",
    )
    .execute(database.pool())
    .await
    .expect("update probe");

    database
        .mark_person_manifest_restore_pending(3)
        .await
        .expect("repeat current pending state");
    let update_count: i64 =
        sqlx::query_scalar("SELECT count FROM person_manifest_restore_update_probe")
            .fetch_one(database.pool())
            .await
            .expect("probe after duplicate pending state");
    assert_eq!(update_count, 0);

    database
        .mark_person_manifest_restore_completed(3)
        .await
        .expect("complete restore");
    database
        .mark_person_manifest_restore_pending(3)
        .await
        .expect("requeue after completion");
    database
        .mark_person_manifest_restore_pending(4)
        .await
        .expect("requeue for a new schema");
    let update_count: i64 =
        sqlx::query_scalar("SELECT count FROM person_manifest_restore_update_probe")
            .fetch_one(database.pool())
            .await
            .expect("probe after meaningful changes");
    assert_eq!(update_count, 3);
    let (status, schema_version): (String, i64) = sqlx::query_as(
        "SELECT status, schema_version FROM person_manifest_restore_state WHERE id = 1",
    )
    .fetch_one(database.pool())
    .await
    .expect("restore state");
    assert_eq!((status.as_str(), schema_version), ("PENDING", 4));
}

#[tokio::test]
async fn person_credit_refresh_preserves_unchanged_rows() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("People", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('credit-refresh-item', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("media item");

    let credit = |person_id: &str, name: &str, role: &str| NewPersonCredit {
        person_id: person_id.to_owned(),
        lux_person_id: None,
        person_type: "Actor".to_owned(),
        person_name: name.to_owned(),
        provider: "tmdb".to_owned(),
        role: role.to_owned(),
        sort_order: 0,
        biography: None,
        birthday: None,
        deathday: None,
        known_for_department: None,
        place_of_birth: None,
        provider_ids: BTreeMap::new(),
        genres: Vec::new(),
        tags: Vec::new(),
        production_locations: Vec::new(),
        premiere_date: None,
        production_year: None,
        taglines: Vec::new(),
    };
    let initial = vec![
        credit("person-1", "Actor One", "Lead"),
        credit("person-2", "Actor Two", "Friend"),
    ];
    database
        .replace_person_credits("credit-refresh-item", &initial)
        .await
        .expect("initial credits");
    let unchanged_row_id: i64 = sqlx::query_scalar(
        "SELECT rowid FROM person_credits
         WHERE item_id = 'credit-refresh-item' AND person_id = 'person-1'",
    )
    .fetch_one(database.pool())
    .await
    .expect("initial row");
    sqlx::query(
        "CREATE TABLE person_credit_update_probe (count INTEGER NOT NULL);
         INSERT INTO person_credit_update_probe (count) VALUES (0);
         CREATE TRIGGER person_credit_update_probe_trigger
         AFTER UPDATE ON person_credits
         BEGIN
             UPDATE person_credit_update_probe SET count = count + 1;
         END;",
    )
    .execute(database.pool())
    .await
    .expect("update probe");

    database
        .replace_person_credits("credit-refresh-item", &initial)
        .await
        .expect("unchanged credits");
    let same_row_id: i64 = sqlx::query_scalar(
        "SELECT rowid FROM person_credits
         WHERE item_id = 'credit-refresh-item' AND person_id = 'person-1'",
    )
    .fetch_one(database.pool())
    .await
    .expect("unchanged row");
    assert_eq!(same_row_id, unchanged_row_id);
    let unchanged_updates: i64 = sqlx::query_scalar("SELECT count FROM person_credit_update_probe")
        .fetch_one(database.pool())
        .await
        .expect("unchanged update count");
    assert_eq!(unchanged_updates, 0);

    let mut refreshed_credit = credit("person-1", "Actor One Updated", "Lead");
    refreshed_credit.lux_person_id = Some("lux-person-1".to_owned());
    let refreshed = vec![refreshed_credit, credit("person-3", "Actor Three", "New")];
    database
        .replace_person_credits("credit-refresh-item", &refreshed)
        .await
        .expect("changed credits");
    let changed_credit: (i64, String, Option<String>) = sqlx::query_as(
        "SELECT rowid, person_name, lux_person_id FROM person_credits
         WHERE item_id = 'credit-refresh-item' AND person_id = 'person-1'",
    )
    .fetch_one(database.pool())
    .await
    .expect("changed row");
    assert_eq!(changed_credit.0, unchanged_row_id);
    assert_eq!(changed_credit.1, "Actor One Updated");
    assert_eq!(changed_credit.2.as_deref(), Some("lux-person-1"));
    let changed_updates: i64 = sqlx::query_scalar("SELECT count FROM person_credit_update_probe")
        .fetch_one(database.pool())
        .await
        .expect("changed update count");
    assert_eq!(changed_updates, 1);
    let remaining_people: Vec<String> = sqlx::query_scalar(
        "SELECT person_id FROM person_credits
         WHERE item_id = 'credit-refresh-item' ORDER BY person_id",
    )
    .fetch_all(database.pool())
    .await
    .expect("remaining credits");
    assert_eq!(remaining_people, ["person-1", "person-3"]);
}
