use luxd::{
    application::{libraries::LibraryService, scanner::ScanJobService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[tokio::test]
async fn library_deletion_cancels_jobs_and_fences_late_workers()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let deletion_state_column: Option<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info('libraries') WHERE name = 'deletion_state'",
    )
    .fetch_optional(database.pool())
    .await?;
    assert_eq!(deletion_state_column.as_deref(), Some("deletion_state"));

    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let media_file = root.join("Movie.2024.mkv");
    tokio::fs::write(&media_file, b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-UTF-8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job_id = "postprocessing-job";
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, scan_phase)
         VALUES (?, ?, 'RECONCILE_LIBRARY', 'COMPLETED', 'generation', 'POSTPROCESSING')",
    )
    .bind(job_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    let _deletion_guard = jobs.prepare_library_deletion(library.id).await?;
    let status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(status, "CANCELLED");

    let late_job_id = "queued-during-library-deletion";
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, scan_phase)
         VALUES (?, ?, 'RECONCILE_LIBRARY', 'PENDING', 'generation', 'IDLE')",
    )
    .bind(late_job_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    jobs.run_to_completion(late_job_id, 100, None).await?;
    let late_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(late_job_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(late_status, "CANCELLED");

    libraries.delete_library(library.id).await?;
    let remaining_libraries: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM libraries WHERE id = ?")
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_libraries, 0);
    assert!(
        media_file.exists(),
        "library deletion must keep media files"
    );

    Ok(())
}
