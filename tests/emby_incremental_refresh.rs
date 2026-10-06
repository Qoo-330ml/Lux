use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::ScanJobService, setup::SetupService},
    auth::{
        admin_api_key::AdminApiKeyService, emby::EmbyAuthService, sessions::WebAuthService,
        users::UserStore,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::StatusCode;
use serde_json::json;
use tokio::net::TcpListener;

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn start_server(
    config: Config,
    database: Database,
    setup: SetupService,
) -> Result<(String, AbortOnDrop), Box<dyn std::error::Error>> {
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database)?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((format!("http://{address}"), AbortOnDrop(task)))
}

fn emby_public_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
}

#[tokio::test]
async fn emby_media_folders_returns_concrete_folder_ids_and_refreshes_one_folder()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup
        .complete("Admin", "Administrator", "correct password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    let folder = media_root.join("Drama").join("Show");
    tokio::fs::create_dir_all(&folder).await?;
    tokio::fs::write(folder.join("Show (2024).mkv"), b"movie").await?;
    let root = libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;
    luxd::application::scanner::LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let folder_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items
         WHERE library_id = ? AND item_type = 'FOLDER' AND title = 'Show'
         LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let (base_url, _server) = start_server(config, database.clone(), setup).await?;
    let client = reqwest::Client::new();
    let library_id = emby_public_id(&library.id.to_string());

    let folders = client
        .get(format!("{base_url}/Library/MediaFolders"))
        .query(&[
            ("LibraryId", library_id.as_str()),
            ("Limit", "100"),
            ("api_key", key.as_str()),
        ])
        .send()
        .await?;
    assert_eq!(folders.status(), StatusCode::OK);
    let folders_body = folders.json::<serde_json::Value>().await?;
    let show = folders_body["Items"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["Name"] == "Show"))
        .ok_or("missing scanned folder")?;
    assert_eq!(show["Id"], emby_public_id(&folder_id));
    assert_eq!(show["Path"], folder.to_string_lossy().to_string());
    assert_eq!(folders_body["TotalRecordCount"], 2);

    let refresh = client
        .post(format!(
            "{base_url}/Items/{}/Refresh",
            show["Id"].as_str().ok_or("missing folder id")?
        ))
        .query(&[("api_key", key.as_str())])
        .send()
        .await?;
    assert_eq!(refresh.status(), StatusCode::ACCEPTED);
    assert_eq!(
        refresh.json::<serde_json::Value>().await?["scope"],
        "FOLDER"
    );

    let viewer = UserStore::new(database.clone())?
        .create_user("Viewer", "Viewer", "viewer password", false)
        .await?;
    let viewer_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .json(&json!({
            "Username": "viewer",
            "Pw": "viewer password"
        }))
        .send()
        .await?;
    assert_eq!(viewer_login.status(), StatusCode::OK);
    let viewer_key = viewer_login.json::<serde_json::Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let forbidden = client
        .post(format!(
            "{base_url}/Items/{}/Refresh",
            show["Id"].as_str().ok_or("missing folder id")?
        ))
        .header("X-Emby-Token", viewer_key)
        .send()
        .await?;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    assert_eq!(viewer.id.to_string().len(), 36);

    let _ = admin;
    let _ = root;
    Ok(())
}

#[tokio::test]
async fn emby_media_updated_queues_incremental_scan_for_absolute_path()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup
        .complete("Admin", "Administrator", "correct password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    let new_folder = media_root.join("NewMovie");
    tokio::fs::create_dir_all(&new_folder).await?;
    let root = libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;
    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let (base_url, _server) = start_server(config, database.clone(), setup).await?;

    let response = reqwest::Client::new()
        .post(format!("{base_url}/Library/Media/Updated"))
        .query(&[("api_key", key.as_str())])
        .json(&json!({
            "Updates": [{
                "Path": new_folder.to_string_lossy(),
                "UpdateType": "Created"
            }]
        }))
        .send()
        .await?;

    let response_status = response.status();
    let response_body = response.text().await?;
    assert_eq!(response_status, StatusCode::ACCEPTED, "{response_body}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&response_body)?["scope"],
        "PATH"
    );
    let job_type: String = sqlx::query_scalar("SELECT job_type FROM scan_jobs LIMIT 1")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(job_type, "INCREMENTAL_SCAN");
    let queued_path: String =
        sqlx::query_scalar("SELECT relative_path FROM scan_job_paths LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(queued_path, "NewMovie");
    assert_eq!(root.library_id, library.id);
    Ok(())
}

#[tokio::test]
async fn emby_scheduled_tasks_reports_library_scan_state() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup
        .complete("Admin", "Administrator", "correct password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let scan = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await?;
    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let (base_url, _server) = start_server(config, database, setup).await?;

    let response = reqwest::Client::new()
        .get(format!("{base_url}/emby/ScheduledTasks"))
        .query(&[("api_key", key.as_str())])
        .send()
        .await?;
    let response_status = response.status();
    let response_body = response.text().await?;
    assert_eq!(response_status, StatusCode::OK, "{response_body}");
    let tasks = serde_json::from_str::<Vec<serde_json::Value>>(&response_body)?;
    let refresh_task = tasks
        .iter()
        .find(|task| task["Key"] == "RefreshMediaLibrary")
        .ok_or("missing RefreshMediaLibrary task")?;
    assert_eq!(refresh_task["State"], "Running");
    assert_eq!(refresh_task["IsHidden"], false);
    assert_eq!(scan.status, "PENDING");
    Ok(())
}

#[tokio::test]
async fn emby_library_refresh_queues_all_enabled_libraries_and_reuses_active_jobs()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup
        .complete("Admin", "Administrator", "correct password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let movies = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let series = libraries
        .create_library("Series", LibraryKind::Series, false)
        .await?;
    let movies_root = temp_dir.path().join("Movies");
    let series_root = temp_dir.path().join("Series");
    tokio::fs::create_dir_all(&movies_root).await?;
    tokio::fs::create_dir_all(&series_root).await?;
    libraries
        .add_root(
            movies.id,
            movies_root.to_str().ok_or("non-utf8 movies root")?,
        )
        .await?;
    libraries
        .add_root(
            series.id,
            series_root.to_str().ok_or("non-utf8 series root")?,
        )
        .await?;

    let existing = ScanJobService::new(database.clone())
        .create_movie_scan_job(movies.id)
        .await?;
    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("Viewer", "Viewer", "viewer password", false)
        .await?;
    let (base_url, _server) = start_server(config, database.clone(), setup).await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base_url}/emby/Library/Refresh"))
        .header("X-Emby-Token", &key)
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = response.json::<serde_json::Value>().await?;
    let jobs = body["jobs"].as_array().ok_or("missing refresh jobs")?;
    assert_eq!(jobs.len(), 2);
    assert!(jobs.iter().any(|job| job["id"] == existing.id));
    assert!(jobs.iter().all(|job| job["jobType"] == "RECONCILE_LIBRARY"));

    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .json(&json!({
            "Username": "Viewer",
            "Pw": "viewer password"
        }))
        .send()
        .await?;
    assert_eq!(login.status(), StatusCode::OK);
    let viewer_token = login.json::<serde_json::Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let forbidden = client
        .post(format!("{base_url}/Library/Refresh"))
        .header("X-Emby-Token", viewer_token)
        .send()
        .await?;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    assert_eq!(viewer.id.to_string().len(), 36);
    Ok(())
}

#[tokio::test]
async fn admin_path_scan_queues_only_the_requested_relative_path()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup
        .complete("Admin", "Administrator", "correct password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(media_root.join("New")).await?;
    let root = libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;
    let root_id = root.id.to_string();

    let job = ScanJobService::new(database.clone())
        .create_path_scan_job(library.id, Some(&root_id), "New")
        .await?;
    let job_type: String = sqlx::query_scalar("SELECT job_type FROM scan_jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    let queued_path: String =
        sqlx::query_scalar("SELECT relative_path FROM scan_job_paths WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(job_type, "INCREMENTAL_SCAN");
    assert_eq!(queued_path, "New");

    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let (base_url, _server) = start_server(config, database.clone(), setup).await?;
    let client = reqwest::Client::new();
    let valid_request = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/{}/scan-path",
            library.id
        ))
        .header("X-Lux-Api-Key", &key)
        .json(&json!({
            "rootId": root_id,
            "path": "New",
            "recursive": true
        }))
        .send()
        .await?;
    assert_eq!(valid_request.status(), StatusCode::ACCEPTED);
    assert_eq!(
        valid_request.json::<serde_json::Value>().await?["scope"],
        "PATH"
    );

    for invalid in ["", ".", "../outside", "/absolute", r"C:\outside"] {
        let result = ScanJobService::new(database.clone())
            .create_path_scan_job(library.id, Some(&root_id), invalid)
            .await;
        assert!(
            matches!(
                result,
                Err(luxd::application::scanner::ScanJobError::Scanner(
                    luxd::application::scanner::ScannerError::InvalidRelativePath(_)
                ))
            ),
            "unexpected result for {invalid:?}: {result:?}"
        );
    }
    for invalid in [".", "../outside", "/absolute"] {
        let response = client
            .post(format!(
                "{base_url}/api/v1/admin/libraries/{}/scan-path",
                library.id
            ))
            .header("X-Lux-Api-Key", &key)
            .json(&json!({
                "rootId": root_id,
                "path": invalid,
                "recursive": true
            }))
            .send()
            .await?;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{invalid}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn emby_media_folders_applies_pagination_across_accessible_libraries()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup
        .complete("Admin", "Administrator", "correct password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let first_library = libraries
        .create_library("First", LibraryKind::Movie, false)
        .await?;
    let second_library = libraries
        .create_library("Second", LibraryKind::Movie, false)
        .await?;

    for (library, name) in [
        (first_library, "FirstMovie"),
        (second_library, "SecondMovie"),
    ] {
        let root = temp_dir.path().join(name);
        let movie_directory = root.join(name);
        tokio::fs::create_dir_all(&movie_directory).await?;
        tokio::fs::write(movie_directory.join(format!("{name}.mkv")), b"movie").await?;
        libraries
            .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
            .await?;
        luxd::application::scanner::LibraryScanner::new(database.clone())
            .scan_movie_library(library.id)
            .await?;
    }

    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let (base_url, _server) = start_server(config, database, setup).await?;
    let response = reqwest::Client::new()
        .get(format!("{base_url}/Library/MediaFolders"))
        .query(&[
            ("StartIndex", "0"),
            ("Limit", "1"),
            ("api_key", key.as_str()),
        ])
        .send()
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await?;
    assert_eq!(body["TotalRecordCount"], 2);
    assert_eq!(body["StartIndex"], 0);
    assert_eq!(body["Items"].as_array().map(Vec::len), Some(1));
    Ok(())
}

#[tokio::test]
async fn modified_notification_and_admin_endpoint_refresh_local_metadata_precisely()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup
        .complete("Admin", "Administrator", "correct password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    for (directory, file) in [
        ("Alpha (2020)", "Alpha.2020.mkv"),
        ("Beta (2021)", "Beta.2021.mkv"),
    ] {
        let movie_dir = media_root.join(directory);
        tokio::fs::create_dir_all(&movie_dir).await?;
        tokio::fs::write(movie_dir.join(file), b"movie").await?;
        tokio::fs::write(movie_dir.join("poster.jpg"), b"jpg").await?;
    }
    let root = libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;
    let wait_for_posters = |expected: i64| {
        let database = database.clone();
        async move {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let posters: i64 = sqlx::query_scalar(
                        "SELECT COUNT(DISTINCT item_id) FROM item_images
                         WHERE image_type = 'POSTER'",
                    )
                    .fetch_one(database.pool())
                    .await?;
                    if posters == expected {
                        return Ok::<_, sqlx::Error>(());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
        }
    };
    wait_for_posters(2).await??;
    // Items that lost their images although nothing changed on disk.
    sqlx::query("DELETE FROM item_images")
        .execute(database.pool())
        .await?;

    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let (base_url, _server) = start_server(config, database.clone(), setup).await?;
    let client = reqwest::Client::new();

    // An Emby-compatible "Modified" notification for one directory re-indexes just that one.
    let response = client
        .post(format!("{base_url}/Library/Media/Updated"))
        .query(&[("api_key", key.as_str())])
        .json(&json!({
            "Updates": [{
                "Path": media_root.join("Alpha (2020)").to_string_lossy(),
                "UpdateType": "Modified"
            }]
        }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = response.json::<serde_json::Value>().await?;
    assert_eq!(body["localMetadataEntries"], 1);
    wait_for_posters(1).await??;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    wait_for_posters(1).await??;

    // "Created" only queues the incremental scan; it must not trigger the extra refresh.
    let response = client
        .post(format!("{base_url}/Library/Media/Updated"))
        .query(&[("api_key", key.as_str())])
        .json(&json!({
            "Updates": [{
                "Path": media_root.join("Beta (2021)").to_string_lossy(),
                "UpdateType": "Created"
            }]
        }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        response.json::<serde_json::Value>().await?["localMetadataEntries"],
        0
    );

    // The admin endpoint takes library-relative directories.
    let response = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/{}/refresh-local-metadata",
            library.id
        ))
        .header("X-Lux-Api-Key", &key)
        .json(&json!({ "rootId": root.id, "paths": ["Beta (2021)"] }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = response.json::<serde_json::Value>().await?;
    assert_eq!(body["scope"], "LOCAL_METADATA");
    assert_eq!(body["entries"], 1);
    wait_for_posters(2).await??;

    for paths in [json!([]), json!(["."]), json!(["../outside"])] {
        let response = client
            .post(format!(
                "{base_url}/api/v1/admin/libraries/{}/refresh-local-metadata",
                library.id
            ))
            .header("X-Lux-Api-Key", &key)
            .json(&json!({ "paths": paths }))
            .send()
            .await?;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{paths}"
        );
    }
    Ok(())
}
