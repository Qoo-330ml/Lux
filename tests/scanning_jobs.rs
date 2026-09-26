mod common;

use std::{sync::Arc, time::Duration};

use common::{TestScraper, TestScraperConfig};
use luxd::{
    application::{
        access::{AccessPrincipal, MediaAccessService},
        catalog::{CatalogFilter, CatalogService},
        libraries::LibraryService,
        nfo::LocalNfoMetadataStore,
        probe::{FfprobeRunner, MediaProbeService},
        reidentify::MetadataReidentifyService,
        scanner::{
            BACKGROUND_SCAN_BATCH_SIZE, IncrementalScanChange, ScanJobError, ScanJobService,
        },
        scraper::ScraperProvider,
        thumbnails::ThumbnailService,
        watch::ChangeKind,
        webhooks::WebhookService,
    },
    config::Config,
    domain::ids::UserId,
    library::LibraryKind,
    storage::Database,
};
use tokio::sync::Semaphore;

async fn seed_legacy_reconciliation_work(
    database: &Database,
    job_id: &str,
    root_id: &str,
    entries: &[(&str, &str)],
    total_count: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query("DELETE FROM scan_manifests WHERE job_id = ?")
        .bind(job_id)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE scan_jobs SET discovery_completed = 1, total_count = ? WHERE id = ?")
        .bind(total_count)
        .bind(job_id)
        .execute(database.pool())
        .await?;
    for (relative_path, entry_type) in entries {
        sqlx::query(
            "INSERT INTO reconciliation_scan_entries (
                 job_id, library_root_id, relative_path, entry_type
             ) VALUES (?, ?, ?, ?)",
        )
        .bind(job_id)
        .bind(root_id)
        .bind(relative_path)
        .bind(entry_type)
        .execute(database.pool())
        .await?;
    }
    Ok(())
}

async fn set_manifest_discovery_format_version(
    database: &Database,
    job_id: &str,
    version: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE scan_manifests SET discovery_format_version = ? WHERE job_id = ?")
        .bind(version)
        .bind(job_id)
        .execute(database.pool())
        .await?;
    Ok(())
}

#[tokio::test]
async fn full_scan_manifest_persists_discovery_and_reobservations_without_directory_work_queue()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    let child = root.join("Nested");
    tokio::fs::create_dir_all(&child).await?;
    tokio::fs::write(root.join("Top.Movie.2024.mkv"), b"top").await?;
    tokio::fs::write(child.join("Nested.Movie.2025.mkv"), b"nested").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    set_manifest_discovery_format_version(&database, &job.id, 2).await?;
    let initial_directory_frontier: Vec<(String, String)> = sqlx::query_as(
        "SELECT library_root_id, relative_path FROM scan_manifest_directories
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&job.id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(initial_directory_frontier.len(), 1);
    assert_eq!(initial_directory_frontier[0].1, "");
    let legacy_directory_work: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = ? AND entry_type = 'DIRECTORY'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(legacy_directory_work, 0);

    let root_batch = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(root_batch.status, "RUNNING");
    let committed_discovery_total: i64 =
        sqlx::query_scalar("SELECT total_count FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(committed_discovery_total, 1);
    let directory_states: Vec<(String, String)> = sqlx::query_as(
        "SELECT relative_path, state FROM scan_manifest_directories
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
         ORDER BY relative_path",
    )
    .bind(&job.id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        directory_states,
        vec![
            ("".to_owned(), "COMPLETE".to_owned()),
            ("Nested".to_owned(), "PENDING".to_owned())
        ]
    );

    let manifest_file_observation: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT observation_sequence, size, modified_at, LENGTH(fingerprint)
         FROM scan_manifest_entries
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND relative_path = 'Top.Movie.2024.mkv' AND entry_kind = 'FILE'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert!(manifest_file_observation.0 > 0);
    assert_eq!(manifest_file_observation.1, 3);
    assert!(manifest_file_observation.2 > 0);
    assert_eq!(manifest_file_observation.3, 32);

    sqlx::query(
        "UPDATE scan_manifest_directories SET state = 'PENDING'
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND relative_path = ''",
    )
    .bind(&job.id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE scan_manifest_roots SET state = 'SCANNING',
             completed_directory_count = completed_directory_count - 1
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&job.id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE scan_manifests SET completed_directory_count = completed_directory_count - 1
         WHERE job_id = ?",
    )
    .bind(&job.id)
    .execute(database.pool())
    .await?;
    jobs.run_batch(&job.id, 1).await?;
    let observation_sequences: Vec<i64> = sqlx::query_scalar(
        "SELECT observation_sequence FROM scan_manifest_entries
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND relative_path = 'Top.Movie.2024.mkv' AND entry_kind = 'FILE'
         ORDER BY observation_sequence",
    )
    .bind(&job.id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(observation_sequences.len(), 2);
    assert!(observation_sequences[0] > 0);
    assert!(observation_sequences[1] > observation_sequences[0]);
    let observation_sequence_high_watermark: i64 = sqlx::query_scalar(
        "SELECT next_observation_sequence FROM scan_manifest_roots
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    let maximum_observation_sequence: i64 = sqlx::query_scalar(
        "SELECT MAX(observation_sequence) FROM scan_manifest_entries
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        observation_sequence_high_watermark,
        maximum_observation_sequence
    );
    let legacy_directory_work: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = ? AND entry_type = 'DIRECTORY'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(legacy_directory_work, 0);
    jobs.run_to_completion(&job.id, 1, None).await?;
    let final_manifest_state: (String, String, i64, i64) = sqlx::query_as(
        "SELECT m.state, r.state, m.observed_file_count, r.completed_directory_count
         FROM scan_manifests m
         JOIN scan_manifest_roots r ON r.manifest_id = m.id
         WHERE m.job_id = ?",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        final_manifest_state,
        ("COMPLETED".to_owned(), "COMPLETE".to_owned(), 2, 2)
    );
    let manifest_file_work: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = ? AND entry_type = 'FILE'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(manifest_file_work, 0);
    Ok(())
}

#[tokio::test]
async fn full_scan_manifest_persists_and_applies_path_deltas_without_rewriting_unchanged_entries()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    for path in [
        "Changed.Movie.2023.mkv",
        "Unchanged.Movie.2022.mkv",
        "Reappeared.Movie.2021.mkv",
        "Removed.Movie.2020.mkv",
    ] {
        tokio::fs::write(root.join(path), b"original file").await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    tokio::fs::write(root.join("Added.Movie.2024.mkv"), b"new file").await?;
    tokio::fs::write(root.join("Changed.Movie.2023.mkv"), b"changed contents").await?;
    tokio::fs::remove_file(root.join("Removed.Movie.2020.mkv")).await?;
    sqlx::query(
        "UPDATE filesystem_entries SET is_missing = 1
         WHERE relative_path = 'Reappeared.Movie.2021.mkv'",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "CREATE TRIGGER fail_unchanged_manifest_rewrite
         BEFORE UPDATE ON filesystem_entries
         WHEN OLD.relative_path = 'Unchanged.Movie.2022.mkv'
         BEGIN SELECT RAISE(ABORT, 'unchanged manifest entry was rewritten'); END",
    )
    .execute(database.pool())
    .await?;

    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    set_manifest_discovery_format_version(&database, &reconciliation.id, 2).await?;
    while !jobs.run_batch(&reconciliation.id, 100).await?.completed {}

    let deltas: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT relative_path, delta_kind, state FROM scan_manifest_deltas
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
         ORDER BY relative_path",
    )
    .bind(&reconciliation.id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        deltas,
        vec![(
            "Removed.Movie.2020.mkv".to_owned(),
            "REMOVE".to_owned(),
            "APPLIED".to_owned()
        )]
    );
    let manifest_counts: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT unchanged_count, add_count, change_count, remove_count, reappeared_count
         FROM scan_manifests WHERE job_id = ?",
    )
    .bind(&reconciliation.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(manifest_counts, (1, 1, 1, 1, 1));
    let path_states: Vec<(String, i64)> = sqlx::query_as(
        "SELECT relative_path, is_missing FROM filesystem_entries
         WHERE relative_path IN (
             'Added.Movie.2024.mkv', 'Reappeared.Movie.2021.mkv',
             'Removed.Movie.2020.mkv'
         ) ORDER BY relative_path",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        path_states,
        vec![
            ("Added.Movie.2024.mkv".to_owned(), 0),
            ("Reappeared.Movie.2021.mkv".to_owned(), 0),
            ("Removed.Movie.2020.mkv".to_owned(), 1),
        ]
    );
    let removed_target_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_job_targets
         WHERE job_id = ? AND change_kind = 'REMOVED'",
    )
    .bind(&reconciliation.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(removed_target_count, 2);
    Ok(())
}

#[tokio::test]
async fn manifest_uses_latest_directory_observation_when_diffing_file_paths()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let relative_path = "Replaced.Movie.2024.mkv";
    tokio::fs::write(root.join(relative_path), b"original file").await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    set_manifest_discovery_format_version(&database, &reconciliation.id, 2).await?;
    loop {
        let state: String = sqlx::query_scalar("SELECT state FROM scan_manifests WHERE job_id = ?")
            .bind(&reconciliation.id)
            .fetch_one(database.pool())
            .await?;
        if state == "READY_TO_DIFF" {
            break;
        }
        let report = jobs.run_batch(&reconciliation.id, 100).await?;
        assert!(!report.completed, "discovery must pause before diffing");
    }

    let observed_sequence: i64 = sqlx::query_scalar(
        "SELECT MAX(observation_sequence) FROM scan_manifest_entries
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND library_root_id = ? AND relative_path = ?",
    )
    .bind(&reconciliation.id)
    .bind(root_record.id.to_string())
    .bind(relative_path)
    .fetch_one(database.pool())
    .await?;
    tokio::fs::remove_file(root.join(relative_path)).await?;
    tokio::fs::create_dir(root.join(relative_path)).await?;
    sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at, fingerprint
         ) VALUES (
             (SELECT id FROM scan_manifests WHERE job_id = ?), ?, ?, ?,
             'DIRECTORY', 0, 0, NULL
         )",
    )
    .bind(&reconciliation.id)
    .bind(root_record.id.to_string())
    .bind(relative_path)
    .bind(observed_sequence + 1)
    .execute(database.pool())
    .await?;

    while !jobs.run_batch(&reconciliation.id, 100).await?.completed {}

    let removal_state: Option<String> = sqlx::query_scalar(
        "SELECT state FROM scan_manifest_deltas
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND relative_path = ? AND delta_kind = 'REMOVE'",
    )
    .bind(&reconciliation.id)
    .bind(relative_path)
    .fetch_optional(database.pool())
    .await?;
    let missing: i64 = sqlx::query_scalar(
        "SELECT is_missing FROM filesystem_entries
         WHERE library_root_id = ? AND relative_path = ?",
    )
    .bind(root_record.id.to_string())
    .bind(relative_path)
    .fetch_one(database.pool())
    .await?;

    assert_eq!(removal_state.as_deref(), Some("UNSTABLE"));
    assert_eq!(
        missing, 0,
        "a path now occupied by a directory is not a safe file removal"
    );
    Ok(())
}

#[tokio::test]
async fn manifest_confirms_missing_file_when_its_parent_directory_is_gone()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    let nested = root.join("Removed");
    tokio::fs::create_dir_all(&nested).await?;
    let relative_path = "Removed/Removed.Movie.2020.mkv";
    tokio::fs::write(root.join(relative_path), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;
    tokio::fs::remove_dir_all(&nested).await?;

    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&reconciliation.id, 100, None)
        .await?;
    let missing: i64 =
        sqlx::query_scalar("SELECT is_missing FROM filesystem_entries WHERE relative_path = ?")
            .bind(relative_path)
            .fetch_one(database.pool())
            .await?;
    let manifest_summary: (String, i64, i64) = sqlx::query_as(
        "SELECT state, remove_count,
                (SELECT COUNT(*) FROM scan_manifest_deltas WHERE manifest_id = scan_manifests.id)
         FROM scan_manifests WHERE job_id = ?",
    )
    .bind(&reconciliation.id)
    .fetch_one(database.pool())
    .await?;

    assert_eq!(missing, 1);
    assert_eq!(manifest_summary, ("COMPLETED".to_owned(), 1, 0));
    Ok(())
}

#[tokio::test]
async fn manifest_apply_does_not_overwrite_a_newer_incremental_filesystem_entry()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let path = root.join("Race.Movie.2024.mkv");
    tokio::fs::write(&path, b"before").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;
    tokio::fs::write(&path, b"after incremental write").await?;

    let job = jobs.create_movie_scan_job(library.id).await?;
    set_manifest_discovery_format_version(&database, &job.id, 2).await?;
    advance_manifest_to_applying(&database, &jobs, &job.id).await?;
    let observed_fingerprint: Vec<u8> = sqlx::query_scalar(
        "SELECT fingerprint FROM scan_manifest_entries
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND relative_path = 'Race.Movie.2024.mkv' AND entry_kind = 'FILE'
         ORDER BY observation_sequence DESC LIMIT 1",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    sqlx::query(
        "UPDATE filesystem_entries
         SET size = 999, modified_at = 123, fingerprint = ?,
             last_seen_generation = 'incremental-generation'
         WHERE relative_path = 'Race.Movie.2024.mkv'",
    )
    .bind(&observed_fingerprint)
    .execute(database.pool())
    .await?;

    loop {
        if jobs.run_batch(&job.id, 100).await?.completed {
            break;
        }
    }
    let persisted_entry: (i64, i64, Vec<u8>, String) = sqlx::query_as(
        "SELECT size, modified_at, fingerprint, last_seen_generation
         FROM filesystem_entries WHERE relative_path = 'Race.Movie.2024.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(persisted_entry.0, 999);
    assert_eq!(persisted_entry.1, 123);
    assert_eq!(persisted_entry.2, observed_fingerprint);
    assert_eq!(persisted_entry.3, "incremental-generation");
    let delta_state: String = sqlx::query_scalar(
        "SELECT state FROM scan_manifest_deltas
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND relative_path = 'Race.Movie.2024.mkv'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(delta_state, "CONFLICT");
    Ok(())
}

#[tokio::test]
async fn manifest_add_conflicts_with_incremental_entry_claimed_after_diff()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Race.Movie.2024.mkv"), b"racing add").await?;
    let root_id = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root
        .id
        .to_string();

    let jobs = ScanJobService::new(database.clone());
    let manifest = jobs.create_movie_scan_job(library.id).await?;
    set_manifest_discovery_format_version(&database, &manifest.id, 2).await?;
    advance_manifest_to_applying(&database, &jobs, &manifest.id).await?;

    let incremental = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id,
                relative_path: "Race.Movie.2024.mkv".to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    jobs.run_to_completion(&incremental.id, 100, None).await?;
    let incremental_status: String =
        sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
            .bind(&incremental.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(incremental_status, "COMPLETED");
    let filesystem_entry_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM filesystem_entries
         WHERE relative_path = 'Race.Movie.2024.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        filesystem_entry_count, 1,
        "incremental scan did not claim the path"
    );
    let before_manifest_apply: (String, String, i64) = sqlx::query_as(
        "SELECT entry.id, entry.last_seen_generation, COUNT(source.id)
         FROM filesystem_entries entry
         LEFT JOIN media_sources source ON source.filesystem_entry_id = entry.id
         WHERE entry.relative_path = 'Race.Movie.2024.mkv'
         GROUP BY entry.id, entry.last_seen_generation",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(before_manifest_apply.2, 1);

    while !jobs.run_batch(&manifest.id, 100).await?.completed {}

    let delta_state: String = sqlx::query_scalar(
        "SELECT state FROM scan_manifest_deltas
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND relative_path = 'Race.Movie.2024.mkv'",
    )
    .bind(&manifest.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(delta_state, "CONFLICT");

    jobs.run_to_completion(&manifest.id, 100, None).await?;

    let after_manifest_apply: (String, String, i64) = sqlx::query_as(
        "SELECT entry.id, entry.last_seen_generation, COUNT(source.id)
         FROM filesystem_entries entry
         LEFT JOIN media_sources source ON source.filesystem_entry_id = entry.id
         WHERE entry.relative_path = 'Race.Movie.2024.mkv'
         GROUP BY entry.id, entry.last_seen_generation",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(after_manifest_apply, before_manifest_apply);
    Ok(())
}

#[tokio::test]
async fn failed_manifest_delta_batch_rolls_back_index_targets_delta_and_progress()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Existing.Movie.2023.mkv"), b"existing").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;
    tokio::fs::write(root.join("Atomic.Movie.2024.mkv"), b"must rollback").await?;

    let job = jobs.create_movie_scan_job(library.id).await?;
    set_manifest_discovery_format_version(&database, &job.id, 2).await?;
    advance_manifest_to_applying(&database, &jobs, &job.id).await?;
    let progress_before: (i64, i64) =
        sqlx::query_as("SELECT processed_count, total_count FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    let trigger_sql = format!(
        "CREATE TRIGGER reject_manifest_target
         BEFORE INSERT ON scan_job_targets
         WHEN NEW.job_id = '{}'
         BEGIN SELECT RAISE(ABORT, 'injected manifest target failure'); END",
        job.id
    );
    sqlx::query(sqlx::AssertSqlSafe(trigger_sql))
        .execute(database.pool())
        .await?;

    assert!(jobs.run_batch(&job.id, 100).await.is_err());
    let state: (i64, i64, String) = sqlx::query_as(
        "SELECT job.processed_count, job.total_count, delta.state
         FROM scan_jobs job
         JOIN scan_manifest_deltas delta ON delta.manifest_id = (
             SELECT id FROM scan_manifests WHERE job_id = job.id
         )
         WHERE job.id = ? AND delta.relative_path = 'Atomic.Movie.2024.mkv'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        state,
        (progress_before.0, progress_before.1, "PENDING".to_owned())
    );
    let rolled_back_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM filesystem_entries
         WHERE relative_path = 'Atomic.Movie.2024.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(rolled_back_rows, 0);
    let rolled_back_sources: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources source
         JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
         WHERE entry.relative_path = 'Atomic.Movie.2024.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(rolled_back_sources, 0);
    Ok(())
}

#[tokio::test]
async fn manifest_apply_uses_requested_bounded_transaction_batch()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    for index in 0..450 {
        tokio::fs::write(
            root.join(format!("Batch.Movie.{index:03}.2024.mkv")),
            b"fixture",
        )
        .await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    set_manifest_discovery_format_version(&database, &job.id, 2).await?;
    advance_manifest_to_applying(&database, &jobs, &job.id).await?;

    let report = jobs.run_batch(&job.id, 500).await?;

    assert_eq!(report.processed, 450);
    let applied_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_deltas
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND state = 'APPLIED'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(applied_count, 450);
    let target_counts: (i64, i64) = sqlx::query_as(
        "SELECT SUM(CASE WHEN target_type = 'SOURCE' THEN 1 ELSE 0 END),
                SUM(CASE WHEN target_type = 'ITEM' THEN 1 ELSE 0 END)
         FROM scan_job_targets WHERE job_id = ?",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(target_counts.0, 450);
    assert!((449..=450).contains(&target_counts.1));
    Ok(())
}

#[tokio::test]
async fn manifest_cancellation_preserves_committed_frontier_and_observations()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    let child = root.join("NotVisited");
    tokio::fs::create_dir_all(&child).await?;
    tokio::fs::write(root.join("Committed.Movie.2024.mkv"), b"fixture").await?;
    tokio::fs::write(child.join("Pending.Movie.2025.mkv"), b"fixture").await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_batch(&job.id, 1).await?;
    jobs.cancel(&job.id).await?;
    let cancelled = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(cancelled.status, "CANCELLED");

    let manifest: (String, i64, i64) = sqlx::query_as(
        "SELECT state, discovered_directory_count, completed_directory_count
         FROM scan_manifests WHERE job_id = ?",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(manifest, ("CANCELLED".to_owned(), 2, 1));
    let directory_states: Vec<(String, String)> = sqlx::query_as(
        "SELECT relative_path, state FROM scan_manifest_directories
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
         ORDER BY relative_path",
    )
    .bind(&job.id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        directory_states,
        vec![
            ("".to_owned(), "COMPLETE".to_owned()),
            ("NotVisited".to_owned(), "PENDING".to_owned())
        ]
    );
    let retained_payload_counts: (i64, i64) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM scan_manifest_entries
              WHERE manifest_id = scan_manifests.id),
             (SELECT COUNT(*) FROM scan_manifest_seen_paths
              WHERE manifest_id = scan_manifests.id)
         FROM scan_manifests WHERE job_id = ?",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(retained_payload_counts, (2, 0));
    let root_state: String = sqlx::query_scalar(
        "SELECT state FROM scan_manifest_roots
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND library_root_id = ?",
    )
    .bind(&job.id)
    .bind(root_record.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(root_state, "INCOMPLETE");
    Ok(())
}

#[tokio::test]
async fn failed_manifest_discovery_retries_its_pending_frontier_safely()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let failed = jobs.create_movie_scan_job(library.id).await?;
    sqlx::query(
        "UPDATE scan_manifest_directories SET relative_path = '../invalid'
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&failed.id)
    .execute(database.pool())
    .await?;
    assert!(jobs.run_batch(&failed.id, 1).await.is_err());
    let failed_state: (String, String, i64) = sqlx::query_as(
        "SELECT m.state, r.state, sj.discovery_completed
         FROM scan_manifests m
         JOIN scan_manifest_roots r ON r.manifest_id = m.id
         JOIN scan_jobs sj ON sj.id = m.job_id
         WHERE sj.id = ?",
    )
    .bind(&failed.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        failed_state,
        ("FAILED".to_owned(), "INCOMPLETE".to_owned(), 0)
    );

    sqlx::query(
        "UPDATE scan_manifest_directories SET relative_path = ''
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&failed.id)
    .execute(database.pool())
    .await?;

    let retried = jobs.retry(&failed.id).await?;
    assert_eq!(retried.id, failed.id);
    let manifests: Vec<(String, String)> = sqlx::query_as(
        "SELECT job_id, state FROM scan_manifests
         WHERE library_id = ? ORDER BY created_at, id",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        manifests,
        vec![(failed.id.clone(), "DISCOVERING".to_owned())]
    );
    let resumed_root_state: String = sqlx::query_scalar(
        "SELECT state FROM scan_manifest_roots
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&failed.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(resumed_root_state, "SCANNING");
    let pending_frontiers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_directories
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND state = 'PENDING'",
    )
    .bind(&retried.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(pending_frontiers, 1);
    jobs.run_to_completion(&failed.id, 1, None).await?;
    Ok(())
}

#[tokio::test]
async fn shutdown_cancelled_manifest_resumes_the_persisted_discovery_frontier()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    let child = root.join("Nested");
    tokio::fs::create_dir_all(&child).await?;
    tokio::fs::write(root.join("Root.Movie.2024.mkv"), b"root").await?;
    tokio::fs::write(child.join("Nested.Movie.2025.mkv"), b"nested").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-UTF-8 root path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    assert_eq!(jobs.run_batch(&job.id, 1).await?.status, "RUNNING");
    let before_restart: (String, i64, i64, i64, String) = sqlx::query_as(
        "SELECT manifest.state, manifest.observed_file_count,
                (SELECT COUNT(*) FROM scan_manifest_directories directory
                 WHERE directory.manifest_id = manifest.id AND directory.state = 'PENDING'),
                (SELECT COUNT(*) FROM scan_manifest_seen_paths seen
                 WHERE seen.manifest_id = manifest.id),
                root.state
         FROM scan_manifests manifest
         JOIN scan_manifest_roots root ON root.manifest_id = manifest.id
         WHERE manifest.job_id = ?",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        before_restart,
        ("DISCOVERING".to_owned(), 1, 1, 0, "SCANNING".to_owned())
    );

    database.cancel_incomplete_jobs_for_shutdown().await?;
    let retried = jobs.retry(&job.id).await?;
    assert_eq!(retried.id, job.id);
    assert_eq!(retried.status, "PENDING");
    let after_resume: (String, i64, i64, i64) = sqlx::query_as(
        "SELECT manifest.state, manifest.observed_file_count,
                (SELECT COUNT(*) FROM scan_manifest_directories directory
                 WHERE directory.manifest_id = manifest.id AND directory.state = 'PENDING'),
                (SELECT COUNT(*) FROM scan_manifest_seen_paths seen
                 WHERE seen.manifest_id = manifest.id)
         FROM scan_manifests manifest WHERE manifest.job_id = ?",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(after_resume, ("DISCOVERING".to_owned(), 1, 1, 0));

    jobs.run_to_completion(&job.id, 1, None).await?;
    let completed: (String, i64, i64) = sqlx::query_as(
        "SELECT state, observed_file_count,
                (SELECT COUNT(*) FROM scan_manifest_entries entry
                 WHERE entry.manifest_id = scan_manifests.id)
         FROM scan_manifests WHERE job_id = ?",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(completed, ("COMPLETED".to_owned(), 2, 0));
    Ok(())
}

#[tokio::test]
async fn cancelled_manifest_apply_resumes_pending_deltas_without_rediscovery()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    for path in ["First.Movie.2024.mkv", "Second.Movie.2025.mkv"] {
        tokio::fs::write(root.join(path), b"fixture").await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-UTF-8 root path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    set_manifest_discovery_format_version(&database, &job.id, 2).await?;
    sqlx::query("UPDATE scan_manifests SET workflow_version = 1 WHERE job_id = ?")
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    assert_eq!(jobs.run_batch(&job.id, 100).await?.status, "RUNNING");
    assert_eq!(jobs.run_batch(&job.id, 100).await?.status, "RUNNING");
    let first_apply = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(first_apply.status, "RUNNING");
    assert_eq!(first_apply.processed, 1);

    jobs.cancel(&job.id).await?;
    let cancelled = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(cancelled.status, "CANCELLED");
    let checkpoint: (String, String, i64, i64) = sqlx::query_as(
        "SELECT state, resume_state,
                (SELECT COUNT(*) FROM scan_manifest_deltas d
                 WHERE d.manifest_id = scan_manifests.id AND d.state = 'APPLIED'),
                (SELECT COUNT(*) FROM scan_manifest_deltas d
                 WHERE d.manifest_id = scan_manifests.id AND d.state = 'PENDING')
         FROM scan_manifests WHERE job_id = ?",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        checkpoint,
        ("CANCELLED".to_owned(), "APPLYING".to_owned(), 1, 1)
    );

    let retried = jobs.retry(&job.id).await?;
    assert_eq!(retried.id, job.id);
    assert_eq!(retried.status, "PENDING");
    let resumed_state: String =
        sqlx::query_scalar("SELECT state FROM scan_manifests WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(resumed_state, "APPLYING");
    while !jobs.run_batch(&job.id, 100).await?.completed {}
    let indexed_counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*),
                (SELECT COUNT(*) FROM media_sources),
                (SELECT COUNT(*) FROM scan_manifests WHERE library_id = ?)
         FROM filesystem_entries WHERE library_root_id = (
             SELECT id FROM library_roots WHERE library_id = ?
         )",
    )
    .bind(library.id.to_string())
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(indexed_counts, (2, 2, 1));
    jobs.run_to_completion(&job.id, 100, None).await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn manifest_discovery_rejects_directory_replaced_by_symlink_outside_root()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    let queued_directory = root.join("Redirected");
    let outside = temp_dir.path().join("Outside");
    tokio::fs::create_dir_all(&queued_directory).await?;
    tokio::fs::create_dir_all(&outside).await?;
    tokio::fs::write(outside.join("Outside.Movie.2025.mkv"), b"outside").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_batch(&job.id, 1).await?;
    tokio::fs::remove_dir(&queued_directory).await?;
    std::os::unix::fs::symlink(&outside, &queued_directory)?;

    jobs.run_batch(&job.id, 1).await?;
    let escaped_observations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_entries
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND relative_path LIKE 'Redirected/%'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(escaped_observations, 0);
    let escaped_work_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = ? AND relative_path LIKE 'Redirected/%'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(escaped_work_items, 0);
    let root_state: String = sqlx::query_scalar(
        "SELECT state FROM scan_manifest_roots
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_ne!(root_state, "COMPLETE");
    Ok(())
}

#[tokio::test]
async fn full_scan_manifest_indexes_safe_positive_batches_during_discovery()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    for index in 0..64 {
        let directory = root.join(format!("Movie {index:03} (2024)"));
        tokio::fs::create_dir_all(&directory).await?;
        tokio::fs::write(
            directory.join(format!("Movie.{index:03}.2024.mkv")),
            b"fixture",
        )
        .await?;
        tokio::fs::write(directory.join("poster.jpg"), b"poster").await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let workflow_version: i64 =
        sqlx::query_scalar("SELECT workflow_version FROM scan_manifests WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        workflow_version, 2,
        "new jobs use the streamed apply workflow"
    );
    let discovery_format_version: i64 =
        sqlx::query_scalar("SELECT discovery_format_version FROM scan_manifests WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        discovery_format_version, 3,
        "new jobs use compact presence observations"
    );
    let targets_ready: i64 = sqlx::query_scalar(
        "SELECT postprocessing_targets_ready FROM scan_manifests WHERE job_id = ?",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        targets_ready, 0,
        "new v3 manifests require target materialization"
    );
    let target_stage: String = sqlx::query_scalar(
        "SELECT postprocessing_target_stage FROM scan_manifest_roots
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?) LIMIT 1",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(target_stage, "NEW");
    let mut visible_items = 0_i64;
    for _ in 0..100 {
        let manifest_state: String =
            sqlx::query_scalar("SELECT state FROM scan_manifests WHERE job_id = ?")
                .bind(&job.id)
                .fetch_one(database.pool())
                .await?;
        if manifest_state != "DISCOVERING" {
            break;
        }
        jobs.run_batch(&job.id, 1).await?;
        visible_items = sqlx::query_scalar(
            "SELECT COUNT(*) FROM media_items
             WHERE item_type = 'MOVIE' AND has_available_source = 1 AND removed_at IS NULL",
        )
        .fetch_one(database.pool())
        .await?;
        if visible_items > 0 {
            assert_eq!(manifest_state, "DISCOVERING");
            assert!(
                visible_items < 64,
                "indexing should remain bounded per batch"
            );
            break;
        }
    }
    assert!(
        visible_items > 0,
        "the discovery worker should commit safe positive indexes before discovery finishes"
    );
    let mut manifest_state = String::new();
    for _ in 0..100 {
        manifest_state = sqlx::query_scalar("SELECT state FROM scan_manifests WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
        if manifest_state != "DISCOVERING" {
            break;
        }
        jobs.run_batch(&job.id, 1).await?;
    }
    assert_eq!(manifest_state, "READY_TO_DIFF");
    let file_observation_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_entries
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND entry_kind = 'FILE'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    let seen_path_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_seen_paths
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    let indexed_generation_path_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM filesystem_entries entry
         JOIN scan_jobs job ON job.id = ?
         WHERE entry.library_root_id = (
             SELECT library_root_id FROM scan_manifest_roots
             WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
         ) AND entry.last_seen_generation = job.generation",
    )
    .bind(&job.id)
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    let manifest_remove_count: i64 =
        sqlx::query_scalar("SELECT remove_count FROM scan_manifests WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(file_observation_count, 0);
    assert_eq!(seen_path_count, 0);
    assert_eq!(indexed_generation_path_count, 128);
    assert_eq!(manifest_remove_count, 0);
    jobs.run_to_completion(&job.id, 1, None).await?;
    let visible_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items
         WHERE item_type = 'MOVIE' AND has_available_source = 1 AND removed_at IS NULL",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(visible_items, 64);
    Ok(())
}

#[tokio::test]
async fn postprocessing_targets_materialize_after_index_and_keep_new_item_precedence()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let changed = root.join("Example.Movie.2024.1080p.mkv");
    tokio::fs::write(&changed, b"before").await?;
    tokio::fs::write(root.join("Example.Movie.2024.2160p.mkv"), b"stable").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let first = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&first.id, 100, None).await?;
    tokio::fs::write(&changed, b"after-with-a-new-size").await?;
    tokio::fs::write(root.join("Example.Movie.2024.720p.mkv"), b"new version").await?;

    let second = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&second.id, 100).await?.completed {
            break;
        }
    }
    let targets_before_materialization: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?")
            .bind(&second.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(targets_before_materialization, 0);

    jobs.materialize_manifest_postprocessing_targets(&second.id)
        .await?;
    let source_kinds: Vec<String> = sqlx::query_scalar(
        "SELECT change_kind FROM scan_job_targets
         WHERE job_id = ? AND target_type = 'SOURCE' ORDER BY change_kind",
    )
    .bind(&second.id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(source_kinds, vec!["CHANGED", "NEW"]);
    let item_kind: String = sqlx::query_scalar(
        "SELECT change_kind FROM scan_job_targets
         WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&second.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(item_kind, "NEW");
    let ready: i64 = sqlx::query_scalar(
        "SELECT postprocessing_targets_ready FROM scan_manifests WHERE job_id = ?",
    )
    .bind(&second.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(ready, 1);
    Ok(())
}

#[tokio::test]
async fn compact_manifest_removes_a_path_only_after_complete_root_and_absence_check()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Compact manifest", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let media_path = root.join("Compact.Movie.2024.mkv");
    tokio::fs::write(&media_path, b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-UTF-8 root")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let first_scan = jobs.create_movie_scan_job(library.id).await?;
    for _ in 0..100 {
        let state: String = sqlx::query_scalar("SELECT state FROM scan_manifests WHERE job_id = ?")
            .bind(&first_scan.id)
            .fetch_one(database.pool())
            .await?;
        if state == "READY_TO_DIFF" {
            break;
        }
        jobs.run_batch(&first_scan.id, 100).await?;
    }
    let seen_before_delete: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_seen_paths seen
         JOIN scan_manifests manifest ON manifest.id = seen.manifest_id
         WHERE manifest.job_id = ? AND seen.relative_path = 'Compact.Movie.2024.mkv'",
    )
    .bind(&first_scan.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(seen_before_delete, 0);
    let manifest_format: i64 =
        sqlx::query_scalar("SELECT discovery_format_version FROM scan_manifests WHERE job_id = ?")
            .bind(&first_scan.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(manifest_format, 3);
    jobs.run_to_completion(&first_scan.id, 100, None).await?;

    tokio::fs::remove_file(&media_path).await?;
    let second_scan = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&second_scan.id, 100, None).await?;

    let source_state: Option<(i64, i64)> = sqlx::query_as(
        "SELECT entry.is_missing, item.removed_at IS NOT NULL
         FROM filesystem_entries entry
         JOIN media_sources source ON source.filesystem_entry_id = entry.id
         JOIN media_items item ON item.id = source.item_id
         WHERE entry.library_root_id = (
             SELECT id FROM library_roots WHERE library_id = ? LIMIT 1
         ) AND entry.relative_path = 'Compact.Movie.2024.mkv'",
    )
    .bind(library.id.to_string())
    .fetch_optional(database.pool())
    .await?;
    let source_state = source_state.ok_or("missing indexed media source after remove")?;
    assert_eq!(source_state, (1, 1));
    let removal_counts: Option<(i64, i64)> = sqlx::query_as(
        "SELECT remove_count, applied_delta_count
         FROM scan_manifests WHERE job_id = ?",
    )
    .bind(&second_scan.id)
    .fetch_optional(database.pool())
    .await?;
    let removal_counts = removal_counts.ok_or("missing completed removal manifest")?;
    assert_eq!(removal_counts, (1, 1));
    Ok(())
}

#[tokio::test]
async fn streamed_manifest_bulk_insert_avoids_redundant_availability_trigger_update()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    assert_eq!(database.schema_version().await?, 136);
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Availability.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    let availability_trigger_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'trigger'
         AND name = 'trg_media_sources_availability_insert'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(
        availability_trigger_sql.contains("COALESCE"),
        "unexpected availability trigger: {availability_trigger_sql}"
    );
    sqlx::query(
        "CREATE TABLE availability_trigger_updates (
             item_id TEXT NOT NULL, previous_value INTEGER NOT NULL, new_value INTEGER NOT NULL
         )",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "CREATE TRIGGER track_availability_trigger_update
         AFTER UPDATE OF has_available_source ON media_items
         BEGIN INSERT INTO availability_trigger_updates(item_id, previous_value, new_value)
               VALUES (NEW.id, OLD.has_available_source, NEW.has_available_source); END",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("CREATE TABLE availability_before_source_insert (value INTEGER NOT NULL)")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "CREATE TRIGGER track_availability_before_source_insert
         BEFORE INSERT ON media_sources
         BEGIN
             INSERT INTO availability_before_source_insert(value)
             SELECT has_available_source FROM media_items WHERE id = NEW.item_id;
         END",
    )
    .execute(database.pool())
    .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;

    let available_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items
         WHERE item_type = 'MOVIE' AND has_available_source = 1 AND removed_at IS NULL",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(available_items, 1);
    let redundant_updates: Vec<(i64, i64)> =
        sqlx::query_as("SELECT previous_value, new_value FROM availability_trigger_updates")
            .fetch_all(database.pool())
            .await?;
    assert!(
        redundant_updates.is_empty(),
        "availability trigger updates: {redundant_updates:?}; trigger SQL: {availability_trigger_sql}"
    );
    let initial_available_values: Vec<i64> =
        sqlx::query_scalar("SELECT value FROM availability_before_source_insert")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(initial_available_values, vec![1]);
    Ok(())
}

#[tokio::test]
async fn streamed_manifest_add_does_not_claim_a_concurrent_filesystem_entry()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Raced.Movie.2024.mkv"), b"scanned version").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let trigger_sql = format!(
        "CREATE TRIGGER claim_manifest_add_after_observation
         AFTER UPDATE ON scan_manifest_directories
         WHEN NEW.relative_path = '' AND NEW.state = 'COMPLETE'
           AND NEW.manifest_id = (
               SELECT id FROM scan_manifests WHERE job_id = '{}'
           )
         BEGIN
             INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 inode, fingerprint, last_seen_generation, is_missing
             ) VALUES (
                 'incremental-entry', NEW.library_root_id, 'Raced.Movie.2024.mkv', 'FILE',
                 777, 888, NULL, X'09080706', 'incremental-generation', 0
             ) ON CONFLICT(library_root_id, relative_path) DO NOTHING;
         END",
        job.id
    );
    sqlx::query(sqlx::AssertSqlSafe(trigger_sql))
        .execute(database.pool())
        .await?;

    jobs.run_to_completion(&job.id, 100, None).await?;

    let filesystem_entry: (String, i64, Vec<u8>, String) = sqlx::query_as(
        "SELECT id, size, fingerprint, last_seen_generation FROM filesystem_entries
         WHERE relative_path = 'Raced.Movie.2024.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        filesystem_entry,
        (
            "incremental-entry".to_owned(),
            777,
            vec![9, 8, 7, 6],
            "incremental-generation".to_owned()
        )
    );
    let media_source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources WHERE filesystem_entry_id = 'incremental-entry'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(media_source_count, 0);
    Ok(())
}

#[tokio::test]
async fn streamed_manifest_change_cas_does_not_overwrite_a_newer_incremental_entry()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let relative_path = "Raced.Movie.2024.mkv";
    tokio::fs::write(root.join(relative_path), b"original").await?;
    let root_id = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root
        .id
        .to_string();

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;
    tokio::fs::write(root.join(relative_path), b"changed on disk").await?;

    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    let manifest_id: String = sqlx::query_scalar("SELECT id FROM scan_manifests WHERE job_id = ?")
        .bind(&reconciliation.id)
        .fetch_one(database.pool())
        .await?;
    let trigger_sql = format!(
        "CREATE TRIGGER advance_manifest_baseline_after_observation
         AFTER UPDATE ON scan_manifest_directories
         WHEN NEW.relative_path = '' AND NEW.state = 'COMPLETE'
           AND NEW.manifest_id = '{}'
         BEGIN
             UPDATE filesystem_entries
             SET size = 777, modified_at = 888, fingerprint = X'09080706',
                 last_seen_generation = 'incremental-generation'
             WHERE library_root_id = NEW.library_root_id
               AND relative_path = 'Raced.Movie.2024.mkv';
         END",
        manifest_id
    );
    sqlx::query(sqlx::AssertSqlSafe(trigger_sql))
        .execute(database.pool())
        .await?;

    jobs.run_to_completion(&reconciliation.id, 100, None)
        .await?;

    let entry: (i64, i64, Vec<u8>, String) = sqlx::query_as(
        "SELECT size, modified_at, fingerprint, last_seen_generation
         FROM filesystem_entries WHERE library_root_id = ? AND relative_path = ?",
    )
    .bind(root_id)
    .bind(relative_path)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        entry,
        (
            777,
            888,
            vec![9, 8, 7, 6],
            "incremental-generation".to_owned()
        )
    );
    let changed_target_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_job_targets
         WHERE job_id = ? AND change_kind = 'CHANGED'",
    )
    .bind(&reconciliation.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(changed_target_count, 0);
    Ok(())
}

#[tokio::test]
async fn manifest_postprocessing_target_batch_rolls_back_targets_and_cursor_together()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Atomic.Movie.2024.mkv"), b"atomic index").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&job.id, 100).await?.completed {
            break;
        }
    }
    let indexed_rows: (i64, i64, i64) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM filesystem_entries
              WHERE relative_path = 'Atomic.Movie.2024.mkv'),
             (SELECT COUNT(*) FROM media_sources source
              JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
              WHERE entry.relative_path = 'Atomic.Movie.2024.mkv'),
             (SELECT postprocessing_targets_ready FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(indexed_rows, (1, 1, 0));

    let trigger_sql = format!(
        "CREATE TRIGGER reject_streamed_manifest_target
         BEFORE INSERT ON scan_job_targets WHEN NEW.job_id = '{}'
         BEGIN SELECT RAISE(ABORT, 'injected streamed target failure'); END",
        job.id
    );
    sqlx::query(sqlx::AssertSqlSafe(trigger_sql))
        .execute(database.pool())
        .await?;

    assert!(
        jobs.materialize_manifest_postprocessing_targets(&job.id)
            .await
            .is_err()
    );
    let rolled_back: (i64, i64, i64, String, Option<String>) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?),
             (SELECT postprocessing_targets_ready FROM scan_manifests WHERE job_id = ?),
             (SELECT COUNT(*) FROM filesystem_entries WHERE relative_path = 'Atomic.Movie.2024.mkv'),
             (SELECT postprocessing_target_stage FROM scan_manifest_roots
              WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)),
             (SELECT postprocessing_target_cursor FROM scan_manifest_roots
              WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?))",
    )
    .bind(&job.id)
    .bind(&job.id)
    .bind(&job.id)
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(rolled_back, (0, 0, 1, "NEW".to_owned(), None));
    sqlx::query("DROP TRIGGER reject_streamed_manifest_target")
        .execute(database.pool())
        .await?;
    jobs.materialize_manifest_postprocessing_targets(&job.id)
        .await?;
    let completed_targets: (i64, i64, String) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?),
             (SELECT postprocessing_targets_ready FROM scan_manifests WHERE job_id = ?),
             (SELECT postprocessing_target_stage FROM scan_manifest_roots
              WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?))",
    )
    .bind(&job.id)
    .bind(&job.id)
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(completed_targets, (2, 1, "DONE".to_owned()));
    Ok(())
}

#[tokio::test]
async fn postprocessing_target_materialization_waits_for_the_indexed_root_identity()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Identity.Movie.2024.mkv"), b"indexed root").await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&job.id, 100).await?.completed {
            break;
        }
    }
    let backup = temp_dir.path().join("Movies-original");
    tokio::fs::rename(&root, &backup).await?;
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Identity.Movie.2024.mkv"), b"replacement root").await?;

    assert!(
        jobs.materialize_manifest_postprocessing_targets(&job.id)
            .await
            .is_err()
    );
    let paused: (i64, i64, String, Option<String>) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?),
             (SELECT postprocessing_targets_ready FROM scan_manifests WHERE job_id = ?),
             root.postprocessing_target_stage, root.postprocessing_target_cursor
         FROM scan_manifest_roots root
         WHERE root.manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&job.id)
    .bind(&job.id)
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(paused, (0, 0, "NEW".to_owned(), None));

    tokio::fs::remove_dir_all(&root).await?;
    tokio::fs::rename(&backup, &root).await?;
    jobs.materialize_manifest_postprocessing_targets(&job.id)
        .await?;
    let resumed: (i64, i64, String) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?),
             (SELECT postprocessing_targets_ready FROM scan_manifests WHERE job_id = ?),
             root.postprocessing_target_stage
         FROM scan_manifest_roots root
         WHERE root.manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&job.id)
    .bind(&job.id)
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(resumed, (2, 1, "DONE".to_owned()));
    let root_available: i64 =
        sqlx::query_scalar("SELECT is_available FROM library_roots WHERE id = ?")
            .bind(root_record.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        root_available, 1,
        "restoring the indexed root clears unavailable state"
    );
    Ok(())
}

#[tokio::test]
async fn incremental_scan_registers_local_metadata_target_after_each_file()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;
    let relative_path = "Incremental.Movie.2024.mkv";
    tokio::fs::write(root.join(relative_path), b"fixture").await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: relative_path.to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    let report = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(report.processed, 1);

    let target: (i64, String) = sqlx::query_as(
        "SELECT COUNT(*), metadata_state FROM scan_job_targets
         WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(target, (1, "PENDING".to_owned()));
    Ok(())
}

#[tokio::test]
async fn full_scan_clamps_an_unbounded_requested_batch_size()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    for index in 0..=BACKGROUND_SCAN_BATCH_SIZE {
        tokio::fs::write(root.join(format!("Movie.{index:03}.2024.mkv")), b"fixture").await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    loop {
        let state: String = sqlx::query_scalar("SELECT state FROM scan_manifests WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
        if state == "READY_TO_DIFF" {
            break;
        }
        jobs.run_batch(&job.id, usize::MAX).await?;
    }
    let discovery_progress: i64 =
        sqlx::query_scalar("SELECT processed_count FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(discovery_progress, 101);
    Ok(())
}

#[tokio::test]
async fn unchanged_incremental_media_and_sidecar_skip_postprocessing_targets()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let media_path = root.join("Stable.Movie.2024.mkv");
    let sidecar_path = root.join("Stable.Movie.2024.nfo");
    tokio::fs::write(&media_path, b"fixture").await?;
    tokio::fs::write(&sidecar_path, b"<movie />").await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    let incremental = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![
                IncrementalScanChange {
                    root_id: root_record.id.to_string(),
                    relative_path: "Stable.Movie.2024.mkv".to_owned(),
                    kind: ChangeKind::Modify,
                },
                IncrementalScanChange {
                    root_id: root_record.id.to_string(),
                    relative_path: "Stable.Movie.2024.nfo".to_owned(),
                    kind: ChangeKind::Modify,
                },
            ],
        )
        .await?;
    let report = jobs.run_batch(&incremental.id, 2).await?;
    assert_eq!(report.processed, 2);

    let target_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?")
            .bind(&incremental.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(target_count, 0);
    Ok(())
}

#[tokio::test]
async fn scan_job_commits_positive_manifest_indexes_during_discovery()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    for (title, year) in [("Alpha", 2020), ("Beta", 2021), ("Gamma", 2022)] {
        let directory = root.join(format!("{title} Movie ({year})"));
        tokio::fs::create_dir_all(&directory).await?;
        tokio::fs::write(
            directory.join(format!("{title}.Movie.{year}.mkv")),
            b"fixture",
        )
        .await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    assert_eq!(job.status, "PENDING");
    assert_eq!(job.total_count, 0);
    assert!(matches!(
        jobs.create_movie_scan_job(library.id).await,
        Err(ScanJobError::AlreadyActive(_))
    ));

    let root_discovery = jobs.run_batch(&job.id, 100).await?;
    assert_eq!(root_discovery.processed, 0);
    let child_discovery = jobs.run_batch(&job.id, 100).await?;
    assert_eq!(child_discovery.processed, 3);
    let discovered_total: i64 =
        sqlx::query_scalar("SELECT total_count FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        discovered_total, 3,
        "discovery persists observed-file progress"
    );

    let visible_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items
         WHERE has_available_source = 1 AND removed_at IS NULL",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(visible_items, 3, "positive indexes commit with discovery");
    let persisted: (String, i64, Option<String>) =
        sqlx::query_as("SELECT status, processed_count, cursor FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(persisted.0, "RUNNING");
    assert_eq!(persisted.1, 3);
    assert_eq!(
        persisted.2, None,
        "Manifest directory checkpoints replace the legacy cursor"
    );
    let positive_delta_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_deltas
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(positive_delta_count, 0);

    let next_worker = ScanJobService::new(database.clone());
    assert!(
        next_worker
            .active_job_ids()
            .await?
            .iter()
            .any(|id| id == &job.id)
    );
    let removal_diff = next_worker.run_batch(&job.id, 1).await?;
    assert_eq!(removal_diff.status, "RUNNING");
    let index_complete = next_worker.run_batch(&job.id, 1).await?;
    assert!(index_complete.completed);
    let pre_postprocessing_target_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        pre_postprocessing_target_count, 0,
        "v3 SOURCE/ITEM targets are materialized only after indexing completes"
    );
    let completed = next_worker.run_batch(&job.id, 10).await?;
    assert_eq!(completed.status, "COMPLETED");
    assert!(completed.completed);
    let final_status: (String, i64, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT status, processed_count, cursor, finished_at FROM scan_jobs WHERE id = ?",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(final_status.0, "COMPLETED");
    assert_eq!(final_status.1, 3);
    assert_eq!(final_status.2, None);
    assert!(final_status.3.is_some());
    let completed_activity: (Option<String>, String) =
        sqlx::query_as("SELECT current_item, scan_phase FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(completed_activity, (None, "POSTPROCESSING".to_owned()));
    assert!(
        next_worker
            .active_job_ids()
            .await?
            .iter()
            .any(|id| id == &job.id)
    );
    next_worker.run_to_completion(&job.id, 10, None).await?;
    let final_status: (String, Option<i64>, String) =
        sqlx::query_as("SELECT status, finished_at, scan_phase FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(final_status.0, "COMPLETED");
    assert!(final_status.1.is_some());
    assert_eq!(final_status.2, "IDLE");
    assert!(
        !next_worker
            .active_job_ids()
            .await?
            .iter()
            .any(|id| id == &job.id)
    );
    let item_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_items WHERE item_type <> 'FOLDER'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(item_count, 3);
    let root_cursor: Option<String> =
        sqlx::query_scalar("SELECT scan_cursor FROM library_roots WHERE library_id = ?")
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(root_cursor, None);
    let event_codes: Vec<String> = sqlx::query_scalar(
        "SELECT event_code FROM scan_job_events WHERE job_id = ? ORDER BY created_at, id",
    )
    .bind(&job.id)
    .fetch_all(database.pool())
    .await?;
    assert!(event_codes.is_empty());

    let cancel_job = next_worker.create_movie_scan_job(library.id).await?;
    next_worker.cancel(&cancel_job.id).await?;
    let cancelled = next_worker.run_batch(&cancel_job.id, 1).await?;
    assert_eq!(cancelled.status, "CANCELLED");
    assert!(cancelled.completed);
    let cancel_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_job_events
         WHERE job_id = ? AND event_code = 'JOB_CANCELLED'",
    )
    .bind(&cancel_job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(cancel_events, 0);
    let cancelled_work: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM reconciliation_scan_entries WHERE job_id = ?")
            .bind(&cancel_job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(cancelled_work, 0);
    Ok(())
}

#[tokio::test]
async fn series_reconciliation_batches_hierarchy_and_versions()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let root = temp_dir.path().join("Shows");
    let season = root.join("Example Show (2024)").join("Season 01");
    tokio::fs::create_dir_all(&season).await?;
    for name in [
        "Example.Show.S01E01.1080p.mkv",
        "Example.Show.S01E01.2160p.mkv",
        "Example.Show.S01E02.mkv",
    ] {
        tokio::fs::write(season.join(name), b"episode").await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;

    let hierarchy_counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT item_type, COUNT(*) FROM media_items
         WHERE library_id = ? GROUP BY item_type ORDER BY item_type",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        hierarchy_counts,
        vec![
            ("EPISODE".to_owned(), 2),
            ("SEASON".to_owned(), 1),
            ("SERIES".to_owned(), 1),
        ]
    );
    let source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources ms
         JOIN media_items mi ON mi.id = ms.item_id
         WHERE mi.library_id = ?",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(source_count, 3);
    Ok(())
}

#[tokio::test]
async fn mixed_reconciliation_batches_known_media_and_keeps_unresolved()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Mixed", LibraryKind::Mixed, false)
        .await?;
    let root = temp_dir.path().join("Mixed");
    let movie_dir = root.join("Known Movie (2020)");
    let episode_dir = root.join("Known Show").join("Season 01");
    let unresolved_dir = root.join("Unclear");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::create_dir_all(&episode_dir).await?;
    tokio::fs::create_dir_all(&unresolved_dir).await?;
    tokio::fs::write(movie_dir.join("Known.Movie.2020.mkv"), b"movie").await?;
    tokio::fs::write(episode_dir.join("Known.Show.S01E01.mkv"), b"episode").await?;
    tokio::fs::write(unresolved_dir.join("Mystery File.mkv"), b"unknown").await?;
    tokio::fs::write(root.join("Known Show").join("tvshow.nfo"), "<tvshow />").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;

    let counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT item_type, COUNT(*) FROM media_items
         WHERE library_id = ? GROUP BY item_type ORDER BY item_type",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        counts,
        vec![
            ("EPISODE".to_owned(), 1),
            ("FOLDER".to_owned(), 2),
            ("MOVIE".to_owned(), 1),
            ("SEASON".to_owned(), 1),
            ("SERIES".to_owned(), 1),
            ("UNRESOLVED".to_owned(), 1),
        ]
    );
    let source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources ms
         JOIN media_items mi ON mi.id = ms.item_id
         WHERE mi.library_id = ?",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(source_count, 3);
    Ok(())
}

#[tokio::test]
async fn series_reconciliation_updates_changed_episode_without_duplicate_source()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let root = temp_dir.path().join("Shows");
    let episode = root
        .join("Example Show")
        .join("Season 01")
        .join("Example.Show.S01E01.mkv");
    if let Some(parent) = episode.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&episode, b"before").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let first = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&first.id, 100, None).await?;
    tokio::fs::write(&episode, b"after with a different size").await?;

    let second = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&second.id, 100, None).await?;
    let source_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_sources")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(source_count, 1);
    let missing_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM filesystem_entries WHERE is_missing = 1")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(missing_count, 0);
    Ok(())
}

#[tokio::test]
async fn cancelling_after_indexing_completion_does_not_cancel_scan()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Cancel.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&job.id, 100).await?.completed {
            break;
        }
    }
    let index_status: (String, String) =
        sqlx::query_as("SELECT status, scan_phase FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        index_status,
        ("COMPLETED".to_owned(), "POSTPROCESSING".to_owned())
    );
    jobs.cancel(&job.id).await?;
    let post_cancel_status: (String, String) =
        sqlx::query_as("SELECT status, scan_phase FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        post_cancel_status,
        ("COMPLETED".to_owned(), "POSTPROCESSING".to_owned())
    );
    let target_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(target_count, 0, "targets are staged after index completion");

    jobs.run_to_completion(&job.id, 100, None).await?;
    let final_target_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(final_target_count, 0);
    Ok(())
}

#[tokio::test]
async fn completed_scan_enqueues_new_media_once_for_webhook_destinations()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Alpha.Movie.2020.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let webhooks = WebhookService::new(database.clone(), config.config_dir.clone())?;
    let event_types = vec!["MEDIA_ADDED".to_owned(), "SCAN_COMPLETED".to_owned()];
    webhooks
        .create_destination(
            "Test destination",
            "https://example.com/lux-hook",
            true,
            false,
            &event_types,
            Some("webhook-test-secret-1234"),
        )
        .await?;
    let jobs = ScanJobService::new(database.clone()).with_webhooks(webhooks);
    let first = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&first.id, 100, None).await?;

    let media_added: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_events WHERE event_type = 'MEDIA_ADDED'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(media_added, 1);
    let payload: String = sqlx::query_scalar(
        "SELECT payload_json FROM notification_events WHERE event_type = 'MEDIA_ADDED'",
    )
    .fetch_one(database.pool())
    .await?;
    let media_added_payload = serde_json::from_str::<serde_json::Value>(&payload)?;
    assert_eq!(media_added_payload["addedCount"], 1);
    assert_eq!(media_added_payload["title"], "Movies新增媒体");
    assert!(
        media_added_payload["content"]
            .as_str()
            .is_some_and(|content| content.contains("总耗时："))
    );
    let scan_completed_payload: String = sqlx::query_scalar(
        "SELECT payload_json FROM notification_events WHERE event_type = 'SCAN_COMPLETED'",
    )
    .fetch_one(database.pool())
    .await?;
    let scan_completed_payload =
        serde_json::from_str::<serde_json::Value>(&scan_completed_payload)?;
    assert_eq!(scan_completed_payload["status"], "COMPLETED");
    assert_eq!(scan_completed_payload["processedCount"], 1);
    assert_eq!(scan_completed_payload["libraryName"], "Movies");
    assert!(
        scan_completed_payload["durationSeconds"]
            .as_i64()
            .is_some_and(|duration| duration >= 0)
    );

    let second = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&second.id, 100, None).await?;
    let media_added_after_rescan: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_events WHERE event_type = 'MEDIA_ADDED'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(media_added_after_rescan, 1);
    Ok(())
}

#[tokio::test]
async fn unchanged_reconciliation_skips_index_targets() -> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    let stable_directory = root.join("Stable Movie (2024)");
    tokio::fs::create_dir_all(&stable_directory).await?;
    tokio::fs::write(stable_directory.join("Stable.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let first = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&first.id, 100).await?.completed {
            break;
        }
    }
    let first_target_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?")
            .bind(&first.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(first_target_count, 0);
    jobs.run_to_completion(&first.id, 100, None).await?;

    let second = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&second.id, 100).await?.completed {
            break;
        }
    }
    let second_target_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?")
            .bind(&second.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(second_target_count, 0);
    jobs.run_to_completion(&second.id, 100, None).await?;

    let movie_item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    sqlx::query("UPDATE media_items SET parent_id = NULL WHERE id = ?")
        .bind(&movie_item_id)
        .execute(database.pool())
        .await?;
    let third = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&third.id, 100, None).await?;
    let parent_after_unchanged_rescan: Option<String> =
        sqlx::query_scalar("SELECT parent_id FROM media_items WHERE id = ?")
            .bind(&movie_item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        parent_after_unchanged_rescan, None,
        "an unchanged reconciliation must not enter the index repair path"
    );
    Ok(())
}

#[tokio::test]
async fn unchanged_reconciliation_does_not_rewrite_filesystem_presence_state()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let media_path = root.join("Stable.Movie.2024.mkv");
    tokio::fs::write(&media_path, b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let first = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&first.id, 100, None).await?;
    sqlx::query(
        "UPDATE filesystem_entries
         SET updated_at = 1234
         WHERE relative_path = 'Stable.Movie.2024.mkv'",
    )
    .execute(database.pool())
    .await?;

    let second = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&second.id, 100, None).await?;

    let state: (i64, i64) = sqlx::query_as(
        "SELECT updated_at, is_missing
         FROM filesystem_entries
         WHERE relative_path = 'Stable.Movie.2024.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(state, (1234, 0));
    Ok(())
}

#[tokio::test]
async fn reconciliation_persists_removed_media_and_sidecar_targets()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let kept = root.join("Kept.Movie.2024.mkv");
    let kept_nfo = root.join("Kept.Movie.2024.nfo");
    let removed = root.join("Removed.Movie.2023.mkv");
    tokio::fs::write(&kept, b"kept").await?;
    tokio::fs::write(&kept_nfo, b"<movie />").await?;
    tokio::fs::write(&removed, b"removed").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let first = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&first.id, 100, None).await?;
    tokio::fs::remove_file(&kept_nfo).await?;
    tokio::fs::remove_file(&removed).await?;

    let second = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&second.id, 100).await?.completed {
            break;
        }
    }
    let targets: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT target_type, target_id, change_kind, metadata_state, thumbnail_state
         FROM scan_job_targets WHERE job_id = ? ORDER BY target_type, target_id",
    )
    .bind(&second.id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(targets.len(), 3);
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.0 == "ITEM" && target.2 == "SIDECAR")
            .count(),
        1
    );
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.0 == "SOURCE" && target.2 == "REMOVED")
            .count(),
        1
    );
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.0 == "ITEM" && target.2 == "REMOVED")
            .count(),
        1
    );
    assert!(targets.iter().all(|target| {
        if target.2 == "SIDECAR" {
            target.3 == "PENDING" && target.4 == "PENDING"
        } else {
            target.3 == "SKIPPED" && target.4 == "SKIPPED"
        }
    }));
    Ok(())
}

#[tokio::test]
async fn reconciliation_sidecar_targets_do_not_cross_directory_prefixes()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    for (directory, stem) in [
        ("A", "Alpha.Movie.2020"),
        ("A2", "Beta.Movie.2021"),
        ("中文", "Chinese.Movie.2022"),
        ("中文2", "Other.Movie.2023"),
    ] {
        tokio::fs::create_dir_all(root.join(directory)).await?;
        tokio::fs::write(root.join(directory).join(format!("{stem}.mkv")), b"fixture").await?;
        tokio::fs::write(
            root.join(directory).join(format!("{stem}.nfo")),
            b"<movie />",
        )
        .await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let first = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&first.id, 100, None).await?;
    tokio::fs::write(
        root.join("A/Alpha.Movie.2020.nfo"),
        b"<movie><title>A</title></movie>",
    )
    .await?;
    tokio::fs::write(
        root.join("中文/Chinese.Movie.2022.nfo"),
        b"<movie><title>Chinese</title></movie>",
    )
    .await?;

    let second = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&second.id, 100).await?.completed {
            break;
        }
    }
    let targeted_paths: Vec<String> = sqlx::query_scalar(
        "SELECT fe.relative_path
         FROM scan_job_targets targets
         JOIN media_sources ms ON ms.item_id = targets.item_id
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE targets.job_id = ? AND targets.change_kind = 'SIDECAR'
         ORDER BY fe.relative_path",
    )
    .bind(&second.id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        targeted_paths,
        vec!["A/Alpha.Movie.2020.mkv", "中文/Chinese.Movie.2022.mkv"]
    );
    Ok(())
}

#[tokio::test]
async fn completed_scan_enqueues_media_removed_for_missing_files()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let media = root.join("Alpha.Movie.2020.mkv");
    tokio::fs::write(&media, b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let webhooks = WebhookService::new(database.clone(), config.config_dir.clone())?;
    let event_types = vec!["MEDIA_REMOVED".to_owned()];
    webhooks
        .create_destination(
            "Removal receiver",
            "https://example.com/lux-hook",
            true,
            false,
            &event_types,
            Some("webhook-test-secret-1234"),
        )
        .await?;
    let jobs = ScanJobService::new(database.clone()).with_webhooks(webhooks);
    let first = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&first.id, 100, None).await?;
    tokio::fs::remove_file(media).await?;

    let second = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&second.id, 100, None).await?;
    let removed_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_events WHERE event_type = 'MEDIA_REMOVED'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(removed_events, 1);
    let payload: String = sqlx::query_scalar(
        "SELECT payload_json FROM notification_events WHERE event_type = 'MEDIA_REMOVED'",
    )
    .fetch_one(database.pool())
    .await?;
    let removed_payload = serde_json::from_str::<serde_json::Value>(&payload)?;
    assert_eq!(removed_payload["removedCount"], 1);
    assert_eq!(removed_payload["title"], "Movies移除媒体");
    assert!(
        removed_payload["content"]
            .as_str()
            .is_some_and(|content| content.contains("移除媒体：1 个"))
    );
    Ok(())
}

#[tokio::test]
async fn manifest_persists_observed_file_count_before_discovery_finishes()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Alpha.Movie.2020.mkv"), b"fixture").await?;
    tokio::fs::write(root.join("Beta.Movie.2021.mkv"), b"fixture").await?;
    tokio::fs::create_dir(root.join("Nested.Movie.2022")).await?;
    tokio::fs::write(
        root.join("Nested.Movie.2022").join("Nested.Movie.2022.mkv"),
        b"fixture",
    )
    .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let first_discovery = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(first_discovery.status, "RUNNING");

    let discovered_count: i64 =
        sqlx::query_scalar("SELECT observed_file_count FROM scan_manifests WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(discovered_count, 2);

    let discovery_completed: i64 =
        sqlx::query_scalar("SELECT discovery_completed FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(discovery_completed, 0);
    Ok(())
}

#[tokio::test]
async fn cancelling_a_pending_scan_finishes_immediately_and_cleans_work()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.cancel(&job.id).await?;

    let status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(status, "CANCELLED");
    let cancel_requested: i64 =
        sqlx::query_scalar("SELECT cancel_requested FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(cancel_requested, 1);
    let event_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_job_events
         WHERE job_id = ?",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(event_count, 0);
    Ok(())
}

#[tokio::test]
async fn deleted_library_scan_worker_exits_as_cancelled_without_touching_media_files()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let media_file = root.join("Movie.2024.mkv");
    tokio::fs::write(&media_file, b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.prepare_library_deletion(library.id).await?;
    libraries.delete_library(library.id).await?;

    let report = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(report.status, "CANCELLED");
    assert!(report.completed);
    assert!(
        media_file.exists(),
        "library deletion must not delete media files"
    );
    Ok(())
}

#[tokio::test]
async fn incremental_scan_waits_until_manifest_targets_are_materialized()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let movie = root.join("Wait.Movie.2024.mkv");
    tokio::fs::write(&movie, b"first version").await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let full_scan = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&full_scan.id, 100).await?.completed {
            break;
        }
    }
    let generation: String = sqlx::query_scalar("SELECT generation FROM scan_jobs WHERE id = ?")
        .bind(&full_scan.id)
        .fetch_one(database.pool())
        .await?;
    tokio::fs::write(&movie, b"second version with another size").await?;
    let incremental = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: "Wait.Movie.2024.mkv".to_owned(),
                kind: ChangeKind::Modify,
            }],
        )
        .await?;
    let incremental_worker = jobs.clone();
    let incremental_id = incremental.id.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let mut incremental_task = tokio::spawn(async move {
        let _ = started_tx.send(());
        incremental_worker.run_batch(&incremental_id, 1).await
    });
    started_rx.await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut incremental_task)
            .await
            .is_err(),
        "incremental scan must wait behind the unready full-scan target checkpoint"
    );
    let persisted_generation: String = sqlx::query_scalar(
        "SELECT last_seen_generation FROM filesystem_entries WHERE library_root_id = ? AND relative_path = ?",
    )
    .bind(root_record.id.to_string())
    .bind("Wait.Movie.2024.mkv")
    .fetch_one(database.pool())
    .await?;
    assert_eq!(persisted_generation, generation);

    jobs.materialize_manifest_postprocessing_targets(&full_scan.id)
        .await?;
    let report = tokio::time::timeout(Duration::from_secs(5), incremental_task).await???;
    assert_eq!(report.processed, 1);
    let latest_generation: String = sqlx::query_scalar(
        "SELECT last_seen_generation FROM filesystem_entries WHERE library_root_id = ? AND relative_path = ?",
    )
    .bind(root_record.id.to_string())
    .bind("Wait.Movie.2024.mkv")
    .fetch_one(database.pool())
    .await?;
    assert_ne!(latest_generation, generation);
    Ok(())
}

#[tokio::test]
async fn active_full_scan_allows_incremental_scan_enqueue() -> Result<(), Box<dyn std::error::Error>>
{
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let full_scan = jobs.create_movie_scan_job(library.id).await?;
    let incremental_scan = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: "New.Movie.2024.mkv".to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    assert_ne!(incremental_scan.id, full_scan.id);
    assert_eq!(incremental_scan.job_type, "INCREMENTAL_SCAN");

    jobs.run_to_completion(&incremental_scan.id, 100, None)
        .await?;
    jobs.run_to_completion(&full_scan.id, 100, None).await?;
    let active_incremental_scan = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: "Another.Movie.2024.mkv".to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    let error = jobs
        .create_movie_scan_job(library.id)
        .await
        .expect_err("an active incremental scan must exclude full index work");
    assert!(matches!(
        error,
        ScanJobError::AlreadyActive(id) if id == active_incremental_scan.id
    ));
    Ok(())
}

#[tokio::test]
async fn realtime_incremental_scan_preempts_running_full_scan()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let scan_lock = Arc::new(Semaphore::new(1));
    let jobs = ScanJobService::new(database.clone()).with_scan_lock(scan_lock.clone());
    let full_scan = jobs.create_movie_scan_job(library.id).await?;
    let first_batch = jobs.run_batch(&full_scan.id, 1).await?;
    assert!(!first_batch.completed);
    let full_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&full_scan.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(full_status, "RUNNING");

    let held_permit = scan_lock.clone().acquire_owned().await?;
    let full_job_id = full_scan.id.clone();
    let full_jobs = jobs.clone();
    let full_worker =
        tokio::spawn(async move { full_jobs.run_to_completion(&full_job_id, 1, None).await });

    tokio::time::sleep(Duration::from_millis(20)).await;
    let incremental_scan = ScanJobService::new(database.clone())
        .with_scan_lock(scan_lock.clone())
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: "Realtime.Movie.2024.mkv".to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    let incremental_job_id = incremental_scan.id.clone();
    let incremental_jobs = ScanJobService::new(database.clone()).with_scan_lock(scan_lock);
    let incremental_worker = tokio::spawn(async move {
        incremental_jobs
            .run_to_completion(&incremental_job_id, 1, None)
            .await
    });

    drop(held_permit);
    tokio::time::timeout(Duration::from_secs(3), async {
        full_worker.await??;
        incremental_worker.await??;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .await??;

    let statuses: Vec<(String, String)> =
        sqlx::query_as("SELECT id, status FROM scan_jobs WHERE id IN (?, ?) ORDER BY id")
            .bind(&full_scan.id)
            .bind(&incremental_scan.id)
            .fetch_all(database.pool())
            .await?;
    assert_eq!(statuses.len(), 2);
    assert!(statuses.iter().all(|(_, status)| status == "COMPLETED"));
    Ok(())
}

#[tokio::test]
async fn incremental_scan_seen_before_manifest_diff_is_not_marked_missing()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("A.Movie.2020.mkv"), b"a").await?;
    tokio::fs::write(root.join("B.Movie.2021.mkv"), b"b").await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    while sqlx::query_scalar::<_, String>("SELECT state FROM scan_manifests WHERE job_id = ?")
        .bind(&reconciliation.id)
        .fetch_one(database.pool())
        .await?
        == "DISCOVERING"
    {
        jobs.run_batch(&reconciliation.id, 100).await?;
    }

    let incremental = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: "A.Movie.2020.mkv".to_owned(),
                kind: ChangeKind::Modify,
            }],
        )
        .await?;
    jobs.run_to_completion(&incremental.id, 1, None).await?;
    jobs.run_to_completion(&reconciliation.id, 100, None)
        .await?;

    let missing: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM filesystem_entries WHERE library_root_id = ? AND is_missing = 1",
    )
    .bind(root_record.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(missing, 0);
    Ok(())
}

#[tokio::test]
async fn file_deleted_after_manifest_observation_waits_for_next_scan_confirmation()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let retained_path = "Retained.Movie.2020.mkv";
    let deleted_path = "Deleted.Movie.2021.mkv";
    tokio::fs::write(root.join(retained_path), b"retained").await?;
    tokio::fs::write(root.join(deleted_path), b"deleted").await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    let discovery = jobs.run_batch(&reconciliation.id, 100).await?;
    assert_eq!(discovery.processed, 2);
    tokio::fs::remove_file(root.join(deleted_path)).await?;

    while !jobs.run_batch(&reconciliation.id, 100).await?.completed {}

    let missing: i64 = sqlx::query_scalar(
        "SELECT is_missing FROM filesystem_entries
         WHERE library_root_id = ? AND relative_path = ?",
    )
    .bind(root_record.id.to_string())
    .bind(deleted_path)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        missing, 0,
        "the observed snapshot became unstable during indexing"
    );

    let confirmation = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&confirmation.id, 100, None).await?;
    let missing: i64 = sqlx::query_scalar(
        "SELECT is_missing FROM filesystem_entries
         WHERE library_root_id = ? AND relative_path = ?",
    )
    .bind(root_record.id.to_string())
    .bind(deleted_path)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(missing, 1, "a later complete snapshot confirms the removal");
    Ok(())
}

#[tokio::test]
async fn item_scan_only_reconciles_the_source_folder() -> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    let alpha = root.join("Alpha");
    let beta = root.join("Beta");
    tokio::fs::create_dir_all(&alpha).await?;
    tokio::fs::create_dir_all(&beta).await?;
    tokio::fs::write(alpha.join("Alpha.Movie.2020.mkv"), b"fixture").await?;
    tokio::fs::write(beta.join("Beta.Movie.2021.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;
    let alpha_item_id: String = sqlx::query_scalar(
        "SELECT ms.item_id
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE fe.relative_path = 'Alpha/Alpha.Movie.2020.mkv'",
    )
    .fetch_one(database.pool())
    .await?;

    tokio::fs::write(alpha.join("Alpha.New.Movie.2022.mkv"), b"fixture").await?;
    tokio::fs::write(beta.join("Beta.New.Movie.2023.mkv"), b"fixture").await?;

    let item_scan = jobs.create_item_folder_scan_job(&alpha_item_id).await?;
    assert_eq!(item_scan.job_type, "INCREMENTAL_SCAN");
    let queued_path: String =
        sqlx::query_scalar("SELECT relative_path FROM scan_job_paths WHERE job_id = ?")
            .bind(&item_scan.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(queued_path, "Alpha");
    let auto_metadata_match: i64 =
        sqlx::query_scalar("SELECT auto_metadata_match FROM scan_jobs WHERE id = ?")
            .bind(&item_scan.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(auto_metadata_match, 0);

    jobs.run_to_completion(&item_scan.id, 100, None).await?;
    let alpha_new_entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM filesystem_entries WHERE relative_path = 'Alpha/Alpha.New.Movie.2022.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    let beta_new_entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM filesystem_entries WHERE relative_path = 'Beta/Beta.New.Movie.2023.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(alpha_new_entries, 1);
    assert_eq!(beta_new_entries, 0);
    Ok(())
}

#[tokio::test]
async fn failed_legacy_scan_retry_creates_a_new_manifest_job()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    for (title, year) in [("Alpha", 2020), ("Beta", 2021), ("Gamma", 2022)] {
        tokio::fs::write(root.join(format!("{title}.Movie.{year}.mkv")), b"fixture").await?;
    }
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    // Model a pre-Manifest active job so the legacy checkpoint path remains covered.
    let legacy_entries = [
        ("Alpha.Movie.2020.mkv", "FILE"),
        ("Beta.Movie.2021.mkv", "FILE"),
        ("Gamma.Movie.2022.mkv", "FILE"),
    ];
    seed_legacy_reconciliation_work(
        &database,
        &job.id,
        &root_record.id.to_string(),
        &legacy_entries,
        3,
    )
    .await?;
    jobs.run_batch(&job.id, 1).await?;

    let before_failure: (i64, i64) = sqlx::query_as(
        "SELECT processed_count,
                (SELECT COUNT(*) FROM reconciliation_scan_entries
                 WHERE job_id = scan_jobs.id AND status = 'PENDING')
         FROM scan_jobs WHERE id = ?",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(before_failure, (1, 2));
    sqlx::query(
        "UPDATE scan_jobs
         SET status = 'FAILED', error = 'simulated failure', finished_at = unixepoch()
         WHERE id = ?",
    )
    .bind(&job.id)
    .execute(database.pool())
    .await?;

    let retried = jobs.retry(&job.id).await?;
    assert_ne!(retried.id, job.id);
    assert_eq!(retried.status, "PENDING");
    let pending_old_entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = ? AND status = 'PENDING'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(pending_old_entries, 0, "legacy queue is retired atomically");
    let retry_manifest_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifests WHERE job_id = ? AND state = 'DISCOVERING'",
    )
    .bind(&retried.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(retry_manifest_count, 1);

    jobs.run_to_completion(&retried.id, 100, None).await?;
    let completed: (String, i64, i64) =
        sqlx::query_as("SELECT status, processed_count, total_count FROM scan_jobs WHERE id = ?")
            .bind(&retried.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(completed, ("COMPLETED".to_owned(), 3, 3));
    Ok(())
}

#[tokio::test]
async fn reconciliation_batch_rolls_back_index_and_retries_all_pending_files()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    for (title, year) in [("Alpha", 2020), ("Beta", 2021), ("Gamma", 2022)] {
        tokio::fs::write(root.join(format!("{title}.Movie.{year}.mkv")), b"fixture").await?;
    }
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let legacy_entries = [
        ("Alpha.Movie.2020.mkv", "FILE"),
        ("Beta.Movie.2021.mkv", "FILE"),
        ("Gamma.Movie.2022.mkv", "FILE"),
    ];
    seed_legacy_reconciliation_work(
        &database,
        &job.id,
        &root_record.id.to_string(),
        &legacy_entries,
        3,
    )
    .await?;
    sqlx::query(
        "CREATE TRIGGER fail_reconciliation_target_insert
         BEFORE INSERT ON scan_job_targets
         BEGIN SELECT RAISE(ABORT, 'injected target failure'); END",
    )
    .execute(database.pool())
    .await?;
    let failure = jobs.run_batch(&job.id, 100).await;
    assert!(failure.is_err());

    let rolled_back: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM filesystem_entries),
             (SELECT COUNT(*) FROM media_sources),
             (SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?),
             (SELECT COUNT(*) FROM reconciliation_scan_entries
              WHERE job_id = ? AND entry_type = 'FILE' AND status = 'PENDING')",
    )
    .bind(&job.id)
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(rolled_back, (0, 0, 0, 3));

    sqlx::query("DROP TRIGGER fail_reconciliation_target_insert")
        .execute(database.pool())
        .await?;
    let retried = jobs.retry(&job.id).await?;
    assert_ne!(retried.id, job.id);
    jobs.run_to_completion(&retried.id, 100, None).await?;

    let completed: (String, i64, i64, i64) = sqlx::query_as(
        "SELECT
             (SELECT status FROM scan_jobs WHERE id = ?),
             (SELECT processed_count FROM scan_jobs WHERE id = ?),
             (SELECT total_count FROM scan_jobs WHERE id = ?),
             (SELECT COUNT(*) FROM media_sources)",
    )
    .bind(&retried.id)
    .bind(&retried.id)
    .bind(&retried.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(completed, ("COMPLETED".to_owned(), 3, 3, 3));
    Ok(())
}

#[tokio::test]
async fn reconciliation_retry_recovers_target_after_index_commit_precedes_target_commit()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let movie_name = "Retry.Movie.2024.mkv";
    tokio::fs::write(root.join(movie_name), b"before").await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;
    tokio::fs::write(root.join(movie_name), b"after-with-a-new-size").await?;

    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    seed_legacy_reconciliation_work(
        &database,
        &reconciliation.id,
        &root_record.id.to_string(),
        &[(movie_name, "FILE")],
        1,
    )
    .await?;
    sqlx::query(
        "CREATE TRIGGER fail_changed_target_insert
         BEFORE INSERT ON scan_job_targets
         BEGIN SELECT RAISE(ABORT, 'injected changed target failure'); END",
    )
    .execute(database.pool())
    .await?;
    assert!(jobs.run_batch(&reconciliation.id, 100).await.is_err());
    sqlx::query("DROP TRIGGER fail_changed_target_insert")
        .execute(database.pool())
        .await?;

    let retried = jobs.retry(&reconciliation.id).await?;
    assert_ne!(retried.id, reconciliation.id);
    let mut target = None;
    for _ in 0..8 {
        target = sqlx::query_as::<_, (String, String)>(
            "SELECT change_kind, probe_state
             FROM scan_job_targets
             WHERE job_id = ? AND target_type = 'SOURCE'",
        )
        .bind(&retried.id)
        .fetch_optional(database.pool())
        .await?;
        if target.is_some() {
            break;
        }
        if jobs.run_batch(&retried.id, 100).await?.completed {
            break;
        }
    }
    let target = target.ok_or("manifest retry should persist its changed-source target")?;
    assert_eq!(target, ("CHANGED".to_owned(), "PENDING".to_owned()));
    Ok(())
}

#[tokio::test]
async fn manifest_removal_delta_rolls_back_missing_state_and_targets_atomically()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let movie_name = "Removed.Movie.2024.mkv";
    tokio::fs::write(root.join(movie_name), b"fixture").await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;
    tokio::fs::remove_file(root.join(movie_name)).await?;

    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    advance_manifest_to_applying(&database, &jobs, &reconciliation.id).await?;
    sqlx::query(
        "CREATE TRIGGER fail_missing_entry_update
         BEFORE UPDATE OF is_missing ON filesystem_entries
         BEGIN SELECT RAISE(ABORT, 'injected missing entry failure'); END",
    )
    .execute(database.pool())
    .await?;
    assert!(jobs.run_batch(&reconciliation.id, 100).await.is_err());

    let partial_target_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_job_targets
         WHERE job_id = ? AND change_kind = 'REMOVED'",
    )
    .bind(&reconciliation.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(partial_target_count, 0);
    let missing_state: i64 = sqlx::query_scalar(
        "SELECT is_missing FROM filesystem_entries
         WHERE library_root_id = ? AND relative_path = ?",
    )
    .bind(root_record.id.to_string())
    .bind(movie_name)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        missing_state, 0,
        "filesystem state rolls back with its targets"
    );
    let pending_delta_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_deltas
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND delta_kind = 'REMOVE' AND state = 'PENDING'",
    )
    .bind(&reconciliation.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(pending_delta_count, 1);

    sqlx::query("DROP TRIGGER fail_missing_entry_update")
        .execute(database.pool())
        .await?;
    Ok(())
}

#[tokio::test]
async fn reconciliation_job_discovers_once_and_processes_a_persisted_snapshot()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Alpha.Movie.2020.mkv"), b"fixture").await?;
    tokio::fs::write(root.join("Beta.Movie.2021.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    assert_eq!(job.total_count, 0, "job creation must not walk the root");

    let discovery = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(discovery.status, "RUNNING");
    assert_eq!(discovery.processed, 2);
    assert!(!discovery.completed);
    let discovered_total: i64 =
        sqlx::query_scalar("SELECT total_count FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(discovered_total, 2);

    tokio::fs::write(root.join("Gamma.Movie.2022.mkv"), b"late fixture").await?;
    jobs.run_to_completion(&job.id, 1, None).await?;

    let final_counts: (i64, i64) =
        sqlx::query_as("SELECT processed_count, total_count FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(final_counts, (2, 2));
    let indexed_paths: Vec<String> =
        sqlx::query_scalar("SELECT relative_path FROM filesystem_entries ORDER BY relative_path")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(
        indexed_paths,
        vec![
            "Alpha.Movie.2020.mkv".to_owned(),
            "Beta.Movie.2021.mkv".to_owned()
        ]
    );
    let remaining_work: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM reconciliation_scan_entries WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_work, 0);
    Ok(())
}

#[tokio::test]
async fn manifest_streams_large_directory_discovery_in_bounded_chunks()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    for index in 0..1_025 {
        tokio::fs::write(
            root.join(format!("Movie {index:04}.Movie.2024.mkv")),
            b"fixture",
        )
        .await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let discovery = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(discovery.processed, 1_025);
    let pending_paths: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_seen_paths
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(pending_paths, 0);
    let generation_indexed_paths: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM filesystem_entries
         WHERE library_root_id = (
             SELECT root.library_root_id FROM scan_manifest_roots root
             JOIN scan_manifests manifest ON manifest.id = root.manifest_id
             WHERE manifest.job_id = ?
         ) AND last_seen_generation = (SELECT generation FROM scan_jobs WHERE id = ?)",
    )
    .bind(&job.id)
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(generation_indexed_paths, 1_025);
    let directory_entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_directories
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND relative_path <> ''",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(directory_entries, 0);
    jobs.run_to_completion(&job.id, 100, None).await?;
    let unchanged_job = jobs.create_movie_scan_job(library.id).await?;
    for _ in 0..100 {
        let state: String = sqlx::query_scalar("SELECT state FROM scan_manifests WHERE job_id = ?")
            .bind(&unchanged_job.id)
            .fetch_one(database.pool())
            .await?;
        if state == "READY_TO_DIFF" {
            break;
        }
        assert!(!jobs.run_batch(&unchanged_job.id, 100).await?.completed);
    }
    let unchanged_paths: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_seen_paths
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&unchanged_job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(unchanged_paths, 0);
    let unchanged_generation_paths: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM filesystem_entries
         WHERE library_root_id = (
             SELECT root.library_root_id FROM scan_manifest_roots root
             JOIN scan_manifests manifest ON manifest.id = root.manifest_id
             WHERE manifest.job_id = ?
         ) AND last_seen_generation = (SELECT generation FROM scan_jobs WHERE id = ?)",
    )
    .bind(&unchanged_job.id)
    .bind(&unchanged_job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(unchanged_generation_paths, 1_025);
    jobs.run_to_completion(&unchanged_job.id, 100, None).await?;
    Ok(())
}

#[tokio::test]
async fn manifest_frontier_batches_handle_long_and_small_directories()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    let long_directory = root.join("000 Long Directory");
    tokio::fs::create_dir_all(&long_directory).await?;
    for index in 0..2_100 {
        tokio::fs::write(
            long_directory.join(format!("Long Film {index:04}.Movie.2024.mkv")),
            b"fixture",
        )
        .await?;
    }
    for index in 0..70 {
        let directory = root.join(format!("Small Directory {index:03}"));
        tokio::fs::create_dir_all(&directory).await?;
        tokio::fs::write(
            directory.join(format!("Small Film {index:03}.Movie.2024.mkv")),
            b"fixture",
        )
        .await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let root_discovery = jobs.run_batch(&job.id, 1).await?;
    assert!(!root_discovery.completed);
    let pending_frontier_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_directories
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND state = 'PENDING'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(pending_frontier_count, 71);
    jobs.run_batch(&job.id, 100).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;

    let file_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM filesystem_entries
         WHERE library_root_id = (SELECT id FROM library_roots WHERE library_id = ? LIMIT 1)
           AND entry_kind = 'FILE'
           AND last_seen_generation = (SELECT generation FROM scan_jobs WHERE id = ?)",
    )
    .bind(library.id.to_string())
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(file_count, 2_170);

    let root_state: (String, i64) = sqlx::query_as(
        "SELECT state, completed_directory_count FROM scan_manifest_roots
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(root_state, ("COMPLETE".to_owned(), 72));
    Ok(())
}

#[tokio::test]
async fn reconciliation_hides_items_after_their_last_file_is_deleted()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let deleted_path = root.join("Deleted.Movie.2020.mkv");
    tokio::fs::write(&deleted_path, b"fixture").await?;
    tokio::fs::write(root.join("Kept.Movie.2021.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    tokio::fs::remove_file(&deleted_path).await?;
    tokio::fs::write(root.join("Added.Movie.2022.mkv"), b"fixture").await?;
    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&reconciliation.id, 100, None)
        .await?;

    let deleted_is_missing: i64 = sqlx::query_scalar(
        "SELECT is_missing FROM filesystem_entries WHERE relative_path = 'Deleted.Movie.2020.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(deleted_is_missing, 1);

    let catalog = CatalogService::new(database.clone(), MediaAccessService::new(database.clone()));
    let principal = AccessPrincipal::new(UserId::new(), true);
    let page = catalog
        .list_library_items_filtered(
            principal,
            &library.id.to_string(),
            &CatalogFilter::default(),
            0,
            100,
        )
        .await?;
    let titles = page
        .items
        .iter()
        .map(|item| item.title.as_str())
        .collect::<Vec<_>>();
    assert_eq!(page.total, 2);
    assert_eq!(titles, vec!["Added Movie", "Kept Movie"]);

    let unfiltered_page = catalog
        .list_library_items(principal, &library.id.to_string(), 0, 100)
        .await?;
    assert_eq!(unfiltered_page.total, 2);
    let deleted_item_id: String = sqlx::query_scalar(
        "SELECT ms.item_id
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE fe.relative_path = 'Deleted.Movie.2020.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(
        catalog
            .find_item(principal, &deleted_item_id)
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn reconciliation_discovery_uses_persisted_snapshot_for_manual_retry()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    for (directory, filename) in [
        ("Alpha", "Alpha.Movie.2020.mkv"),
        ("Beta", "Beta.Movie.2021.mkv"),
    ] {
        tokio::fs::create_dir_all(root.join(directory)).await?;
        tokio::fs::write(root.join(directory).join(filename), b"fixture").await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let root_discovery = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(root_discovery.processed, 0);
    let queued_directories: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_directories
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND state = 'PENDING'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(queued_directories, 2);

    let late_directory = root.join("Gamma");
    tokio::fs::create_dir_all(&late_directory).await?;
    tokio::fs::write(late_directory.join("Gamma.Movie.2022.mkv"), b"late fixture").await?;

    let next_worker = ScanJobService::new(database.clone());
    next_worker.run_to_completion(&job.id, 1, None).await?;

    let indexed_paths: Vec<String> =
        sqlx::query_scalar("SELECT relative_path FROM filesystem_entries ORDER BY relative_path")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(
        indexed_paths,
        vec![
            "Alpha/Alpha.Movie.2020.mkv".to_owned(),
            "Beta/Beta.Movie.2021.mkv".to_owned()
        ]
    );
    Ok(())
}

#[tokio::test]
async fn reconciliation_does_not_mark_files_missing_when_root_disappears_after_discovery()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Keep.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    let discovery = jobs.run_batch(&reconciliation.id, 100).await?;
    assert_eq!(discovery.processed, 1);
    let discovered_total: i64 =
        sqlx::query_scalar("SELECT total_count FROM scan_jobs WHERE id = ?")
            .bind(&reconciliation.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(discovered_total, 1);

    tokio::fs::rename(&root, temp_dir.path().join("Movies-unmounted")).await?;
    jobs.run_to_completion(&reconciliation.id, 100, None)
        .await?;

    let entry_missing: i64 = sqlx::query_scalar("SELECT is_missing FROM filesystem_entries")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(entry_missing, 0);
    let root_available: i64 = sqlx::query_scalar("SELECT is_available FROM library_roots")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(root_available, 0);
    let remaining_work: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM reconciliation_scan_entries WHERE job_id = ?")
            .bind(&reconciliation.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_work, 0);
    Ok(())
}

#[tokio::test]
async fn manifest_does_not_remove_entries_when_root_is_replaced_after_discovery()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Keep.Movie.2024.mkv"), b"fixture").await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;
    tokio::fs::write(root.join("Keep.Movie.2024.mkv"), b"changed fixture data").await?;

    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&reconciliation.id, 100).await?.completed {
            break;
        }
    }
    let changed_generation_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM filesystem_entries entry
         JOIN scan_jobs job ON job.id = ?
         WHERE entry.library_root_id = ?
           AND entry.relative_path = 'Keep.Movie.2024.mkv'
           AND entry.last_seen_generation = job.generation
           AND entry.last_seen_change_kind = 'CHANGED'",
    )
    .bind(&reconciliation.id)
    .bind(root_record.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(changed_generation_count, 1);
    let observed_root_identity: (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT device, inode FROM scan_manifest_entries
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND library_root_id = ? AND relative_path = ''
         ORDER BY observation_sequence DESC LIMIT 1",
    )
    .bind(&reconciliation.id)
    .bind(root_record.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let moved_root = temp_dir.path().join("Movies-unmounted");
    tokio::fs::rename(&root, &moved_root).await?;
    tokio::fs::create_dir_all(&root).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let replacement_metadata = tokio::fs::metadata(&root).await?;
        assert_ne!(
            observed_root_identity,
            (
                i64::try_from(replacement_metadata.dev()).ok(),
                i64::try_from(replacement_metadata.ino()).ok()
            ),
            "fixture must replace the observed root directory object"
        );
    }
    #[cfg(not(unix))]
    let _ = observed_root_identity;
    assert!(
        jobs.run_to_completion(&reconciliation.id, 100, None)
            .await
            .is_err()
    );

    let postprocessing_failure: (String, String, i64, i64) = sqlx::query_as(
        "SELECT job.status, job.scan_phase, manifest.postprocessing_targets_ready,
                (SELECT COUNT(*) FROM scan_job_events event
                 WHERE event.job_id = job.id AND event.event_code = 'POSTPROCESSING_FAILED')
         FROM scan_jobs job
         JOIN scan_manifests manifest ON manifest.job_id = job.id
         WHERE job.id = ?",
    )
    .bind(&reconciliation.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        postprocessing_failure,
        ("COMPLETED".to_owned(), "IDLE".to_owned(), 0, 1)
    );

    tokio::fs::remove_dir_all(&root).await?;
    tokio::fs::rename(&moved_root, &root).await?;
    let retried = jobs.retry(&reconciliation.id).await?;
    assert_eq!(retried.id, reconciliation.id);
    jobs.run_to_completion(&reconciliation.id, 100, None)
        .await?;

    let entry_missing: i64 = sqlx::query_scalar(
        "SELECT is_missing FROM filesystem_entries
         WHERE library_root_id = ? AND relative_path = 'Keep.Movie.2024.mkv'",
    )
    .bind(root_record.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let root_available: i64 =
        sqlx::query_scalar("SELECT is_available FROM library_roots WHERE id = ?")
            .bind(root_record.id.to_string())
            .fetch_one(database.pool())
            .await?;
    let manifest_root_state: String = sqlx::query_scalar(
        "SELECT state FROM scan_manifest_roots
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
           AND library_root_id = ?",
    )
    .bind(&reconciliation.id)
    .bind(root_record.id.to_string())
    .fetch_one(database.pool())
    .await?;

    assert_eq!(
        entry_missing, 0,
        "a replacement directory is not proof of deletion"
    );
    assert_eq!(
        manifest_root_state, "UNAVAILABLE",
        "root identity was {observed_root_identity:?}"
    );
    assert_eq!(root_available, 1);
    let target_ready: i64 = sqlx::query_scalar(
        "SELECT postprocessing_targets_ready FROM scan_manifests WHERE job_id = ?",
    )
    .bind(&reconciliation.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(target_ready, 1);
    Ok(())
}

#[tokio::test]
async fn failed_reconciliation_keeps_checkpoint_for_retry() -> Result<(), Box<dyn std::error::Error>>
{
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let valid_relative_path = "Valid.Movie.2024.mkv";
    tokio::fs::write(root.join(valid_relative_path), b"fixture").await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    // Model an old-version task that has a persisted legacy work queue but no Manifest.
    sqlx::query("DELETE FROM scan_manifests WHERE job_id = ?")
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE scan_jobs SET discovery_completed = 1, total_count = 1 WHERE id = ?")
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    let invalid_absolute_path = temp_dir.path().join("Outside.Movie.2025.mkv");
    tokio::fs::write(&invalid_absolute_path, b"fixture").await?;
    let invalid_absolute_path = invalid_absolute_path
        .to_str()
        .ok_or("non-utf8 invalid path")?;
    sqlx::query(
        "INSERT INTO filesystem_entries (
             id, library_root_id, relative_path, entry_kind, size, modified_at,
             last_seen_generation
         ) VALUES (?, (SELECT id FROM library_roots WHERE library_id = ?), ?, 'FILE', 0, 0, ?)",
    )
    .bind("invalid-checkpoint-entry")
    .bind(library.id.to_string())
    .bind(invalid_absolute_path)
    .bind("old-generation")
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO reconciliation_scan_entries (
             job_id, library_root_id, relative_path, entry_type
         ) VALUES (?, ?, ?, 'FILE')",
    )
    .bind(&job.id)
    .bind(root_record.id.to_string())
    .bind(invalid_absolute_path)
    .execute(database.pool())
    .await?;

    let error = jobs
        .run_batch(&job.id, 100)
        .await
        .expect_err("invalid persisted work must fail the scan job");
    assert!(matches!(error, ScanJobError::Scanner(_)));
    let failed_status: (String, Option<String>) =
        sqlx::query_as("SELECT status, error FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(failed_status.0, "FAILED");
    assert!(failed_status.1.is_some());
    let remaining_work: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM reconciliation_scan_entries WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_work, 1);

    let retried = jobs.retry(&job.id).await?;
    assert_ne!(retried.id, job.id);
    assert_eq!(retried.status, "PENDING");
    assert!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM scan_manifests WHERE job_id = ? AND state = 'DISCOVERING'",
        )
        .bind(&retried.id)
        .fetch_one(database.pool())
        .await?
            == 1
    );
    jobs.run_to_completion(&retried.id, 100, None).await?;
    let completed_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&retried.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(completed_status, "COMPLETED");
    Ok(())
}

#[tokio::test]
async fn reconciliation_skips_prefetched_sibling_directories_after_one_directory_fails()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    for (directory, filename) in [
        ("Alpha", "Alpha.Movie.2020.mkv"),
        ("Beta", "Beta.Movie.2021.mkv"),
    ] {
        tokio::fs::create_dir_all(root.join(directory)).await?;
        tokio::fs::write(root.join(directory).join(filename), b"fixture").await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_batch(&reconciliation.id, 100).await?;
    tokio::fs::rename(root.join("Alpha"), temp_dir.path().join("Alpha-unmounted")).await?;
    jobs.run_to_completion(&reconciliation.id, 100, None)
        .await?;

    let missing_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM filesystem_entries WHERE is_missing = 1")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(missing_count, 0);
    let root_available: i64 = sqlx::query_scalar("SELECT is_available FROM library_roots")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(root_available, 0);
    Ok(())
}

#[tokio::test]
async fn incremental_scan_processes_only_queued_file() -> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;
    let relative_path = "New.Movie.2024.mkv";
    tokio::fs::write(root.join(relative_path), b"fixture").await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: relative_path.to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    assert_eq!(job.job_type, "INCREMENTAL_SCAN");

    jobs.run_to_completion(&job.id, 100, None).await?;

    let item_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_items")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(item_count, 1);
    let queued_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_job_paths WHERE job_id = ? AND processed_at IS NULL",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(queued_count, 0);
    let retained_path_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_paths WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(retained_path_count, 0);
    Ok(())
}

#[tokio::test]
async fn incremental_series_scan_queues_episode_and_hierarchy_metadata()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library_with_scraper("Shows", LibraryKind::Series, false, Some("tmdb"), true)
        .await?;
    let root = temp_dir.path().join("Shows");
    let season = root.join("Example Show (2024)").join("Season 01");
    tokio::fs::create_dir_all(&season).await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    let tmdb = TestScraper::new(TestScraperConfig {
        timeout: Duration::from_millis(1),
        ..TestScraperConfig::default()
    })?;
    let metadata =
        MetadataReidentifyService::new(database.clone(), ScraperProvider::from_adapter(tmdb));

    for episode_number in [1, 2] {
        tokio::fs::write(
            season.join(format!("Example.Show.S01E{episode_number:02}.mkv")),
            b"episode",
        )
        .await?;
    }
    let incremental = jobs
        .enqueue_incremental_changes(
            library.id,
            [1, 2]
                .into_iter()
                .map(|episode_number| IncrementalScanChange {
                    root_id: root_record.id.to_string(),
                    relative_path: format!(
                        "Example Show (2024)/Season 01/Example.Show.S01E{episode_number:02}.mkv"
                    ),
                    kind: ChangeKind::Create,
                })
                .collect(),
        )
        .await?;
    jobs.run_to_completion_with_metadata(&incremental.id, 100, None, Some(metadata))
        .await?;

    let metadata_job: (String, String, i64) = sqlx::query_as(
        "SELECT id, mode, total_count FROM metadata_reidentify_jobs ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(metadata_job.1, "FILL_MISSING");
    assert_eq!(metadata_job.2, 4);

    let item_types: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT mi.item_type, mi.episode_number
         FROM metadata_reidentify_job_items ji
         JOIN media_items mi ON mi.id = ji.item_id
         WHERE ji.job_id = ?
         ORDER BY CASE mi.item_type WHEN 'SERIES' THEN 0 WHEN 'SEASON' THEN 1 ELSE 2 END,
                  mi.episode_number",
    )
    .bind(&metadata_job.0)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        item_types,
        vec![
            ("SERIES".to_owned(), None),
            ("SEASON".to_owned(), None),
            ("EPISODE".to_owned(), Some(1)),
            ("EPISODE".to_owned(), Some(2)),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn incremental_scan_only_queues_metadata_when_library_switch_is_enabled()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library_with_scraper("Movies", LibraryKind::Movie, false, Some("tmdb"), false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Existing.Movie.2023.mkv"), b"existing").await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    let tmdb = TestScraper::new(TestScraperConfig {
        base_url: "http://127.0.0.1:9".to_owned(),
        proxy_url: None,
        api_key: Some("test-token".to_owned()),
        read_access_token: None,
        timeout: Duration::from_millis(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?;
    let metadata =
        MetadataReidentifyService::new(database.clone(), ScraperProvider::from_adapter(tmdb));

    let disabled_path = "Disabled.Movie.2024.mkv";
    tokio::fs::write(root.join(disabled_path), b"disabled").await?;
    let disabled_job = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: disabled_path.to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    jobs.run_to_completion_with_metadata(&disabled_job.id, 100, None, Some(metadata.clone()))
        .await?;
    let disabled_metadata_jobs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM metadata_reidentify_jobs")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(disabled_metadata_jobs, 0);

    libraries
        .update_settings(
            library.id,
            luxd::application::libraries::LibrarySettingsPatch {
                realtime_metadata_auto_match_enabled: Some(true),
                ..Default::default()
            },
        )
        .await?;
    let enabled_path = "Enabled.Movie.2025.mkv";
    tokio::fs::write(root.join(enabled_path), b"enabled").await?;
    let enabled_job = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: enabled_path.to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    tokio::time::timeout(
        Duration::from_millis(750),
        jobs.run_to_completion_with_metadata(&enabled_job.id, 100, None, Some(metadata.clone())),
    )
    .await??;

    let metadata_job: (String, i64) = sqlx::query_as(
        "SELECT mode, total_count FROM metadata_reidentify_jobs ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(metadata_job, ("FILL_MISSING".to_owned(), 1));

    let sidecar_path = "Enabled.Movie.2025.nfo";
    tokio::fs::write(root.join(sidecar_path), b"<movie />").await?;
    let sidecar_job = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: sidecar_path.to_owned(),
                kind: ChangeKind::Modify,
            }],
        )
        .await?;
    tokio::time::timeout(
        Duration::from_millis(750),
        jobs.run_to_completion_with_metadata(&sidecar_job.id, 100, None, Some(metadata)),
    )
    .await??;
    let metadata_job_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM metadata_reidentify_jobs")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(metadata_job_count, 1);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn completed_scan_runs_pending_ffprobe_before_worker_returns()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{fs, os::unix::fs::PermissionsExt};

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
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Probe Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Probe.Movie.2024.mp4"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let fake_ffprobe = temp_dir.path().join("fake-ffprobe");
    fs::write(
        &fake_ffprobe,
        r#"#!/bin/sh
printf '%s' '{"format":{"format_name":"mp4","duration":"30","bit_rate":"128000"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264"}]}'
"#,
    )?;
    let mut permissions = fs::metadata(&fake_ffprobe)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_ffprobe, permissions)?;

    let jobs = ScanJobService::new(database.clone());
    let probe = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(fake_ffprobe, Duration::from_secs(5)),
    );
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, Some(probe)).await?;

    let source: (String, i64, i64, String) = sqlx::query_as(
        "SELECT container, duration_ticks, bitrate, probe_status FROM media_sources",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        source,
        ("mp4".to_owned(), 300_000_000, 128_000, "READY".to_owned())
    );
    let remaining_targets: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_targets, 0);
    let info_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_job_events
         WHERE job_id = ? AND level = 'INFO'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(info_events, 0);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn incremental_scan_probes_only_changed_local_sources()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{fs, os::unix::fs::PermissionsExt};

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
    let root = temp_dir.path().join("Movies");
    let changed_path = "Changed Movie (2024)/Changed.Movie.2024.mp4";
    let unchanged_path = "Unchanged Movie (2023)/Unchanged.Movie.2023.mp4";
    let strm_path = "Remote Movie (2022)/Remote.Movie.2022.strm";
    tokio::fs::create_dir_all(root.join("Changed Movie (2024)")).await?;
    tokio::fs::create_dir_all(root.join("Unchanged Movie (2023)")).await?;
    tokio::fs::create_dir_all(root.join("Remote Movie (2022)")).await?;
    tokio::fs::write(root.join(changed_path), b"fixture").await?;
    tokio::fs::write(root.join(unchanged_path), b"fixture").await?;
    tokio::fs::write(root.join(strm_path), "https://example.invalid/media.mkv\n").await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let initial_scan = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial_scan.id, 100, None).await?;
    tokio::fs::write(root.join(changed_path), b"changed fixture").await?;

    let fake_ffprobe = temp_dir.path().join("fake-ffprobe");
    fs::write(
        &fake_ffprobe,
        r#"#!/bin/sh
printf '%s' '{"format":{"format_name":"mp4","duration":"30","bit_rate":"128000"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264"}]}'
"#,
    )?;
    let mut permissions = fs::metadata(&fake_ffprobe)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_ffprobe, permissions)?;

    let incremental_scan = jobs
        .enqueue_incremental_changes(
            library.id,
            [changed_path, strm_path]
                .into_iter()
                .map(|relative_path| IncrementalScanChange {
                    root_id: root_record.id.to_string(),
                    relative_path: relative_path.to_owned(),
                    kind: ChangeKind::Modify,
                })
                .collect(),
        )
        .await?;
    let probe = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(fake_ffprobe, Duration::from_secs(5)),
    );
    jobs.run_to_completion(&incremental_scan.id, 100, Some(probe))
        .await?;

    let sources: Vec<(String, String)> = sqlx::query_as(
        "SELECT fe.relative_path, ms.probe_status
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         ORDER BY fe.relative_path",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        sources,
        vec![
            (changed_path.to_owned(), "READY".to_owned()),
            (strm_path.to_owned(), "PENDING".to_owned()),
            (unchanged_path.to_owned(), "PENDING".to_owned()),
        ]
    );
    let remaining_targets: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?")
            .bind(&incremental_scan.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_targets, 0);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn full_scan_imports_existing_strm_media_info_sidecar_without_ffprobe()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{fs, os::unix::fs::PermissionsExt};

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
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Remote Movie (2022)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(
        movie_dir.join("Remote.Movie.2022.strm"),
        "https://example.invalid/media.mkv\n",
    )
    .await?;
    tokio::fs::write(
        movie_dir.join("Remote.Movie.2022-mediainfo.json"),
        br#"[{"MediaSourceInfo":{"Container":"mp4","RunTimeTicks":300000000,"Bitrate":128000,"MediaStreams":[{"Index":0,"Type":"Video","Codec":"h264"}]}}]"#,
    )
    .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let fake_ffprobe = temp_dir.path().join("fake-ffprobe");
    fs::write(
        &fake_ffprobe,
        "#!/bin/sh\nprintf '%s' 'ffprobe must not run' >&2\nexit 1\n",
    )?;
    let mut permissions = fs::metadata(&fake_ffprobe)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_ffprobe, permissions)?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let probe = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(fake_ffprobe, Duration::from_secs(5)),
    );
    jobs.run_to_completion(&job.id, 100, Some(probe)).await?;

    let source: (String, Option<i64>, Option<i64>, String) = sqlx::query_as(
        "SELECT ms.container, ms.duration_ticks, ms.bitrate, ms.probe_status
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE fe.relative_path = 'Remote Movie (2022)/Remote.Movie.2022.strm'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        source,
        (
            "mp4".to_owned(),
            Some(300000000),
            Some(128000),
            "READY".to_owned()
        )
    );

    let stream_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_streams
         JOIN media_sources ON media_sources.id = media_streams.media_source_id
         JOIN filesystem_entries fe ON fe.id = media_sources.filesystem_entry_id
         WHERE fe.relative_path = 'Remote Movie (2022)/Remote.Movie.2022.strm'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stream_count, 1);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn scan_postprocessing_persists_the_current_stage_while_ffprobe_runs()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{fs, os::unix::fs::PermissionsExt};

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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Slow.Movie.2024.mp4"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let fake_ffprobe = temp_dir.path().join("fake-ffprobe");
    fs::write(
        &fake_ffprobe,
        r#"#!/bin/sh
sleep 1
printf '%s' '{"format":{"format_name":"mp4"},"streams":[]}'
"#,
    )?;
    let mut permissions = fs::metadata(&fake_ffprobe)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_ffprobe, permissions)?;

    let jobs = ScanJobService::new(database.clone());
    let probe = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(fake_ffprobe, Duration::from_secs(5)),
    );
    let job = jobs.create_movie_scan_job(library.id).await?;
    let worker = tokio::spawn({
        let jobs = jobs.clone();
        let job_id = job.id.clone();
        async move { jobs.run_to_completion(&job_id, 100, Some(probe)).await }
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let activity: (String, Option<String>) =
                sqlx::query_as("SELECT scan_phase, current_item FROM scan_jobs WHERE id = ?")
                    .bind(&job.id)
                    .fetch_one(database.pool())
                    .await?;
            if activity.0 == "POSTPROCESSING" && activity.1.as_deref() == Some("媒体探测") {
                break Ok::<(), sqlx::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    worker.await??;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn failed_postprocessing_targets_leave_scan_completed_and_retryable()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{fs, os::unix::fs::PermissionsExt};

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
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Retry Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Retry.Movie.2024.mp4"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let fake_ffprobe = temp_dir.path().join("fake-ffprobe");
    fs::write(&fake_ffprobe, "#!/bin/sh\nexit 1\n")?;
    let mut permissions = fs::metadata(&fake_ffprobe)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_ffprobe, permissions)?;

    let jobs = ScanJobService::new(database.clone());
    let failing_probe = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(&fake_ffprobe, Duration::from_secs(5)),
    );
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, Some(failing_probe))
        .await?;

    let status: (String, String, Option<String>) =
        sqlx::query_as("SELECT status, scan_phase, error FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(status.0, "COMPLETED");
    assert_eq!(status.1, "IDLE");
    assert_eq!(status.2, None);
    let target_state: String = sqlx::query_scalar(
        "SELECT probe_state FROM scan_job_targets WHERE job_id = ? AND target_type = 'SOURCE'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(target_state, "FAILED");
    let postprocessing_failed_event: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_job_events
         WHERE job_id = ? AND event_code = 'POSTPROCESSING_FAILED'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(postprocessing_failed_event, 1);

    fs::write(
        &fake_ffprobe,
        "#!/bin/sh\nprintf '%s' '{\"format\":{\"format_name\":\"mp4\"},\"streams\":[]}'\n",
    )?;
    let retried = jobs.retry(&job.id).await?;
    assert_eq!(retried.id, job.id);
    assert_eq!(retried.status, "COMPLETED");
    assert_eq!(retried.scan_phase, "POSTPROCESSING");
    let succeeding_probe = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(fake_ffprobe, Duration::from_secs(5)),
    );
    jobs.run_to_completion(&job.id, 100, Some(succeeding_probe))
        .await?;
    let final_status: (String, String) =
        sqlx::query_as("SELECT status, scan_phase FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(final_status, ("COMPLETED".to_owned(), "IDLE".to_owned()));
    Ok(())
}

#[tokio::test]
async fn pending_postprocessing_targets_make_scan_retryable()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Pending.Movie.2024.mp4"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&job.id, 100).await?.completed {
            break;
        }
    }
    jobs.materialize_manifest_postprocessing_targets(&job.id)
        .await?;
    sqlx::query(
        "UPDATE filesystem_entries SET is_missing = 1
         WHERE relative_path = 'Pending.Movie.2024.mp4'",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE scan_job_targets
         SET probe_state = 'SKIPPED', metadata_state = 'PENDING', thumbnail_state = 'SKIPPED'
         WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&job.id)
    .execute(database.pool())
    .await?;

    jobs.run_to_completion(&job.id, 100, None).await?;
    let status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(status, "COMPLETED");
    let event_code: String = sqlx::query_scalar(
        "SELECT event_code FROM scan_job_events
         WHERE job_id = ? AND event_code = 'POSTPROCESSING_FAILED'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(event_code, "POSTPROCESSING_FAILED");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn metadata_and_thumbnail_failures_are_persisted_per_target()
-> Result<(), Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Broken Metadata Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Broken.Metadata.Movie.2024.mp4"), b"fixture").await?;
    tokio::fs::write(movie_dir.join("Broken.Metadata.Movie.2024.nfo"), b"<movie").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone())
        .with_nfo_store(LocalNfoMetadataStore::new(database.clone()));
    let thumbnails =
        ThumbnailService::with_runner(database.clone(), "false", Duration::from_secs(5));
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion_with_metadata_and_thumbnails(&job.id, 100, None, None, Some(thumbnails))
        .await?;

    let states: (String, String, String) = sqlx::query_as(
        "SELECT status, metadata_state, thumbnail_state
         FROM scan_jobs sj
         JOIN scan_job_targets t ON t.job_id = sj.id
         WHERE sj.id = ? AND t.target_type = 'ITEM'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        states,
        (
            "COMPLETED".to_owned(),
            "FAILED".to_owned(),
            "FAILED".to_owned()
        )
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn scan_job_marks_inaccessible_root_unavailable_and_recovers_after_restore()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Recovery.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    assert_eq!(initial.total_count, 0);
    finish_scan(&jobs, &initial.id).await?;

    let mut permissions = tokio::fs::metadata(&root).await?.permissions();
    permissions.set_mode(0o000);
    tokio::fs::set_permissions(&root, permissions).await?;

    let unavailable = jobs.create_movie_scan_job(library.id).await?;
    assert_eq!(unavailable.total_count, 0);
    finish_scan(&jobs, &unavailable.id).await?;
    let root_available: i64 =
        sqlx::query_scalar("SELECT is_available FROM library_roots WHERE library_id = ?")
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(root_available, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_items")
            .fetch_one(database.pool())
            .await?,
        1
    );
    let unavailable_manifest_root_state: String = sqlx::query_scalar(
        "SELECT state FROM scan_manifest_roots
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&unavailable.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(unavailable_manifest_root_state, "UNAVAILABLE");

    let mut permissions = tokio::fs::metadata(&root).await?.permissions();
    permissions.set_mode(0o755);
    tokio::fs::set_permissions(&root, permissions).await?;

    let recovered = jobs.create_movie_scan_job(library.id).await?;
    assert_eq!(recovered.total_count, 0);
    finish_scan(&jobs, &recovered.id).await?;
    let recovered_manifest_root_state: String = sqlx::query_scalar(
        "SELECT state FROM scan_manifest_roots
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)",
    )
    .bind(&recovered.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(recovered_manifest_root_state, "COMPLETE");
    let recovered_available: i64 =
        sqlx::query_scalar("SELECT is_available FROM library_roots WHERE library_id = ?")
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(recovered_available, 1);
    Ok(())
}

#[tokio::test]
async fn scans_from_different_libraries_are_serialized() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let first_library = libraries
        .create_library("First Movies", LibraryKind::Movie, false)
        .await?;
    let second_library = libraries
        .create_library("Second Movies", LibraryKind::Movie, false)
        .await?;
    let first_root = temp_dir.path().join("first");
    let second_root = temp_dir.path().join("second");
    tokio::fs::create_dir_all(&first_root).await?;
    tokio::fs::create_dir_all(&second_root).await?;
    for index in 0..128 {
        tokio::fs::write(
            first_root.join(format!("First.Movie.{}.mkv", 2000 + index)),
            b"fixture",
        )
        .await?;
    }
    tokio::fs::write(second_root.join("Second.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(
            first_library.id,
            first_root.to_str().ok_or("non-utf8 first root")?,
        )
        .await?;
    libraries
        .add_root(
            second_library.id,
            second_root.to_str().ok_or("non-utf8 second root")?,
        )
        .await?;

    let scan_lock = Arc::new(Semaphore::new(1));
    let held_permit = scan_lock.clone().acquire_owned().await?;
    let first_jobs = ScanJobService::new(database.clone()).with_scan_lock(scan_lock.clone());
    let second_jobs = ScanJobService::new(database.clone()).with_scan_lock(scan_lock.clone());
    let first_job = first_jobs.create_movie_scan_job(first_library.id).await?;
    let second_job = second_jobs.create_movie_scan_job(second_library.id).await?;
    let first_job_id = first_job.id.clone();
    let second_job_id = second_job.id.clone();

    let first_worker =
        tokio::spawn(async move { first_jobs.run_to_completion(&first_job_id, 50, None).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second_worker = tokio::spawn(async move {
        second_jobs
            .run_to_completion(&second_job_id, 50, None)
            .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let first_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&first_job.id)
        .fetch_one(database.pool())
        .await?;
    let second_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&second_job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(first_status, "PENDING");
    assert_eq!(second_status, "PENDING");

    drop(held_permit);
    first_worker.await??;
    second_worker.await??;

    let statuses: Vec<(String, String)> =
        sqlx::query_as("SELECT id, status FROM scan_jobs WHERE id IN (?, ?) ORDER BY id")
            .bind(&first_job.id)
            .bind(&second_job.id)
            .fetch_all(database.pool())
            .await?;
    assert_eq!(statuses.len(), 2);
    assert!(statuses.iter().all(|(_, status)| status == "COMPLETED"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn postprocessing_does_not_hold_the_shared_scan_lock()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{fs, os::unix::fs::PermissionsExt};

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let first_library = libraries
        .create_library("First Movies", LibraryKind::Movie, false)
        .await?;
    let second_library = libraries
        .create_library("Second Movies", LibraryKind::Movie, false)
        .await?;
    let first_root = temp_dir.path().join("first");
    let second_root = temp_dir.path().join("second");
    tokio::fs::create_dir_all(&first_root).await?;
    tokio::fs::create_dir_all(&second_root).await?;
    tokio::fs::write(first_root.join("First.Movie.2024.mkv"), b"fixture").await?;
    tokio::fs::write(second_root.join("Second.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(
            first_library.id,
            first_root.to_str().ok_or("non-utf8 first root")?,
        )
        .await?;
    libraries
        .add_root(
            second_library.id,
            second_root.to_str().ok_or("non-utf8 second root")?,
        )
        .await?;

    let fake_ffprobe = temp_dir.path().join("slow-ffprobe");
    fs::write(
        &fake_ffprobe,
        r#"#!/bin/sh
sleep 1
printf '%s' '{"format":{"format_name":"mp4"},"streams":[]}'
"#,
    )?;
    let mut permissions = fs::metadata(&fake_ffprobe)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_ffprobe, permissions)?;

    let scan_lock = Arc::new(Semaphore::new(1));
    let first_jobs = ScanJobService::new(database.clone()).with_scan_lock(scan_lock.clone());
    let second_jobs = ScanJobService::new(database.clone()).with_scan_lock(scan_lock);
    let first_job = first_jobs.create_movie_scan_job(first_library.id).await?;
    let second_job = second_jobs.create_movie_scan_job(second_library.id).await?;
    let first_job_id = first_job.id.clone();
    let first_probe = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(fake_ffprobe, Duration::from_secs(5)),
    );
    let first_worker = tokio::spawn(async move {
        first_jobs
            .run_to_completion(&first_job_id, 100, Some(first_probe))
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let phase: String = sqlx::query_scalar("SELECT scan_phase FROM scan_jobs WHERE id = ?")
                .bind(&first_job.id)
                .fetch_one(database.pool())
                .await?;
            if phase == "POSTPROCESSING" {
                break Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;

    let second_job_id = second_job.id.clone();
    let second_worker = tokio::spawn(async move {
        second_jobs
            .run_to_completion(&second_job_id, 100, None)
            .await
    });
    tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
                .bind(&second_job.id)
                .fetch_one(database.pool())
                .await?;
            if status != "PENDING" {
                break Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;

    second_worker.await??;
    first_worker.await??;
    let first_phase: String = sqlx::query_scalar("SELECT scan_phase FROM scan_jobs WHERE id = ?")
        .bind(&first_job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(first_phase, "IDLE");
    Ok(())
}

async fn advance_manifest_to_applying(
    database: &Database,
    jobs: &ScanJobService,
    job_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query("UPDATE scan_manifests SET workflow_version = 1 WHERE job_id = ?")
        .bind(job_id)
        .execute(database.pool())
        .await?;
    for _ in 0..16 {
        let state: String = sqlx::query_scalar("SELECT state FROM scan_manifests WHERE job_id = ?")
            .bind(job_id)
            .fetch_one(database.pool())
            .await?;
        if state == "APPLYING" {
            return Ok(());
        }
        let report = jobs.run_batch(job_id, 100).await?;
        if report.completed {
            return Err("manifest completed before entering APPLYING".into());
        }
    }
    Err("manifest did not enter APPLYING within the test batch bound".into())
}

#[cfg(unix)]
async fn finish_scan(
    jobs: &ScanJobService,
    job_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    jobs.run_to_completion(job_id, 100, None).await?;
    Ok(())
}
