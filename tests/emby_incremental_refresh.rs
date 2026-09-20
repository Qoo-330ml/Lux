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
    assert_eq!(serde_json::from_str::<serde_json::Value>(&response_body)?["scope"], "PATH");
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
