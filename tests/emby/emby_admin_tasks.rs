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

fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client")
}

async fn scan_movie_library(
    database: &Database,
    root: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::fs::create_dir_all(root).await?;
    tokio::fs::write(
        root.join("Admin.Refresh.2020.strm"),
        "https://example.invalid/admin-refresh",
    )
    .await?;
    let database = database.clone();
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;
    Ok(())
}

async fn only_movie_item_id(database: &Database) -> Result<String, Box<dyn std::error::Error>> {
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE'")
            .fetch_one(database.pool())
            .await?;
    Ok(item_id)
}

async fn wait_for_local_metadata_batches(
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let (pending, failed): (i64, i64) = sqlx::query_as(
                "SELECT COUNT(*) FILTER (WHERE status IN ('PENDING', 'RUNNING')),
                        COUNT(*) FILTER (WHERE status = 'FAILED')
                 FROM scan_local_metadata_batches",
            )
            .fetch_one(database.pool())
            .await?;
            if failed > 0 {
                return Err(sqlx::Error::Protocol(format!(
                    "{failed} local metadata batch(es) failed"
                )));
            }
            if pending == 0 {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn scan_task_key_is_startable_and_queues_every_enabled_library()
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
    for (name, kind) in [
        ("Movies", LibraryKind::Movie),
        ("Series", LibraryKind::Series),
    ] {
        let library = libraries.create_library(name, kind, false).await?;
        let root = temp_dir.path().join(name);
        tokio::fs::create_dir_all(&root).await?;
        libraries
            .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
            .await?;
    }
    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let (base_url, _server) = start_server(config, database.clone(), setup).await?;
    let client = test_client();

    let listed = client
        .get(format!("{base_url}/emby/ScheduledTasks"))
        .query(&[("isHidden", "false"), ("api_key", key.as_str())])
        .send()
        .await?;
    assert_eq!(listed.status(), StatusCode::OK);
    let tasks = listed.json::<Vec<serde_json::Value>>().await?;
    let task = tasks.first().ok_or("missing scheduled task")?;
    // Yamby keeps only these keys; anything else never reaches its task panel.
    assert_eq!(task["Key"], "RefreshLibrary");
    let task_id = task["Id"].as_str().ok_or("missing task id")?.to_owned();

    let started = client
        .post(format!("{base_url}/emby/ScheduledTasks/Running/{task_id}"))
        .query(&[("api_key", key.as_str())])
        .send()
        .await?;
    assert_eq!(started.status(), StatusCode::NO_CONTENT);

    let running = client
        .get(format!("{base_url}/emby/ScheduledTasks"))
        .query(&[("api_key", key.as_str())])
        .send()
        .await?;
    let running = running.json::<Vec<serde_json::Value>>().await?;
    assert_eq!(running[0]["State"], "Running");

    let queued: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_jobs WHERE job_type = 'RECONCILE_LIBRARY'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(queued, 2);

    // Starting the same task twice reuses the active jobs instead of queueing a second pair.
    let again = client
        .post(format!("{base_url}/emby/ScheduledTasks/Running/{task_id}"))
        .query(&[("api_key", key.as_str())])
        .send()
        .await?;
    assert_eq!(again.status(), StatusCode::NO_CONTENT);
    let queued_again: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_jobs WHERE job_type = 'RECONCILE_LIBRARY'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(queued_again, 2);
    Ok(())
}

#[tokio::test]
async fn scan_task_start_rejects_unknown_tasks_and_non_administrators()
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
    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("Viewer", "Viewer", "viewer password", false)
        .await?;
    let (base_url, _server) = start_server(config, database.clone(), setup).await?;
    let client = test_client();

    let unknown = client
        .post(format!("{base_url}/emby/ScheduledTasks/Running/not-a-task"))
        .query(&[("api_key", key.as_str())])
        .send()
        .await?;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

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
        .post(format!(
            "{base_url}/emby/ScheduledTasks/Running/lux-refresh-media-library"
        ))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    assert_eq!(viewer.id.to_string().len(), 36);
    Ok(())
}

#[tokio::test]
async fn item_refresh_full_refresh_queues_a_metadata_job() -> Result<(), Box<dyn std::error::Error>>
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
    scan_movie_library(&database, &temp_dir.path().join("Movies")).await?;
    let item_id = only_movie_item_id(&database).await?;
    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let (base_url, _server) = start_server(config, database.clone(), setup).await?;
    let client = test_client();

    let refresh = client
        .post(format!(
            "{base_url}/emby/Items/{}/Refresh",
            emby_public_id(&item_id)
        ))
        .query(&[
            ("api_key", key.as_str()),
            ("Recursive", "true"),
            ("MetadataRefreshMode", "FullRefresh"),
            ("ImageRefreshMode", "FullRefresh"),
            ("ReplaceAllMetadata", "true"),
            ("ReplaceAllImages", "true"),
        ])
        .send()
        .await?;
    let status = refresh.status();
    let body = refresh.text().await?;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let body = serde_json::from_str::<serde_json::Value>(&body)?;
    assert_eq!(body["scope"], "ITEM_METADATA");
    assert_eq!(body["job"]["mode"], "FULL_REFRESH");

    let modes: Vec<String> = sqlx::query_scalar("SELECT mode FROM metadata_reidentify_jobs")
        .fetch_all(database.pool())
        .await?;
    assert_eq!(modes, vec!["FULL_REFRESH".to_owned()]);
    Ok(())
}

#[tokio::test]
async fn item_refresh_default_mode_scans_the_item_folder() -> Result<(), Box<dyn std::error::Error>>
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
    scan_movie_library(&database, &temp_dir.path().join("Movies")).await?;
    let item_id = only_movie_item_id(&database).await?;
    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let (base_url, _server) = start_server(config, database.clone(), setup).await?;
    let client = test_client();

    let refresh = client
        .post(format!(
            "{base_url}/emby/Items/{}/Refresh",
            emby_public_id(&item_id)
        ))
        .query(&[
            ("api_key", key.as_str()),
            ("Recursive", "true"),
            ("MetadataRefreshMode", "Default"),
        ])
        .send()
        .await?;
    let status = refresh.status();
    let body = refresh.text().await?;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let body = serde_json::from_str::<serde_json::Value>(&body)?;
    assert_eq!(body["scope"], "ITEM_FOLDER");
    assert_eq!(body["job"]["jobType"], "INCREMENTAL_SCAN");

    let metadata_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metadata_reidentify_jobs")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(metadata_jobs, 0);
    Ok(())
}

#[tokio::test]
async fn item_delete_accepts_the_emby_delete_suffix() -> Result<(), Box<dyn std::error::Error>> {
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
    let movies_root = temp_dir.path().join("Movies");
    scan_movie_library(&database, &movies_root).await?;
    let item_id = only_movie_item_id(&database).await?;
    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    UserStore::new(database.clone())?
        .create_user("Viewer", "Viewer", "viewer password", false)
        .await?;
    let (base_url, _server) = start_server(config, database.clone(), setup).await?;
    let client = test_client();

    // The scanner keeps registering local NFO/images after run_to_completion returns, and a
    // delete racing those writes surfaces as 503; wait for the batches to drain first.
    wait_for_local_metadata_batches(&database).await?;

    let deleted = client
        .delete(format!(
            "{base_url}/emby/Items/{}/Delete",
            emby_public_id(&item_id)
        ))
        .query(&[("api_key", key.as_str())])
        .send()
        .await?;
    let deleted_status = deleted.status();
    let deleted_body = deleted.text().await?;
    assert_eq!(deleted_status, StatusCode::NO_CONTENT, "{deleted_body}");
    assert!(!movies_root.join("Admin.Refresh.2020.strm").exists());

    let missing = client
        .delete(format!(
            "{base_url}/emby/Items/{}/Delete",
            emby_public_id(&uuid::Uuid::now_v7().to_string())
        ))
        .query(&[("api_key", key.as_str())])
        .send()
        .await?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

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
        .delete(format!(
            "{base_url}/emby/Items/{}/Delete",
            emby_public_id(&item_id)
        ))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    Ok(())
}
