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

async fn wait_for_local_metadata_batches(
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let pending = pending_local_metadata_batches(database).await?;
            if pending == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        Ok::<(), sqlx::Error>(())
    })
    .await??;
    Ok(())
}

async fn pending_local_metadata_batches(database: &Database) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_local_metadata_batches WHERE status IN ('PENDING', 'RUNNING')",
    )
    .fetch_one(database.pool())
    .await
}

struct Fixture {
    base_url: String,
    key: String,
    public_item_id: String,
    _server: AbortOnDrop,
    _temp: tempfile::TempDir,
}

async fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(
        root.join("Admin.Media.2020.strm"),
        "https://example.invalid/admin-media",
    )
    .await?;
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
    wait_for_local_metadata_batches(&database).await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE'")
            .fetch_one(database.pool())
            .await?;
    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let (base_url, server) = start_server(config, database.clone(), setup).await?;
    Ok(Fixture {
        base_url,
        key,
        public_item_id: emby_public_id(&item_id),
        _server: server,
        _temp: temp_dir,
    })
}

async fn viewer_token(
    base_url: &str,
    database: &Database,
) -> Result<String, Box<dyn std::error::Error>> {
    UserStore::new(database.clone())?
        .create_user("Viewer", "Viewer", "viewer password", false)
        .await?;
    let response = test_client()
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .json(&json!({ "Username": "Viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<serde_json::Value>().await?;
    Ok(body["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned())
}

#[tokio::test]
async fn remote_search_validates_the_client_and_the_item() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = fixture().await?;
    let client = test_client();
    let search_body = json!({
        "ItemId": fixture.public_item_id,
        "SearchInfo": {
            "ProviderIds": { "Tmdb": "550" },
            "Name": "Admin Media",
            "Year": 2020
        }
    });

    // The library has no scraper configured, so the search reports the configuration gap (409)
    // instead of an empty list that looks like "no results".
    let response = client
        .post(format!(
            "{base_url}/emby/Items/RemoteSearch/Movie",
            base_url = fixture.base_url
        ))
        .query(&[("api_key", fixture.key.as_str())])
        .json(&search_body)
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    let unknown_item = client
        .post(format!(
            "{base_url}/emby/Items/RemoteSearch/Movie",
            base_url = fixture.base_url
        ))
        .query(&[("api_key", fixture.key.as_str())])
        .json(&json!({
            "ItemId": emby_public_id(&uuid::Uuid::now_v7().to_string()),
            "SearchInfo": { "Name": "Admin Media" }
        }))
        .send()
        .await?;
    assert_eq!(unknown_item.status(), StatusCode::NOT_FOUND);

    let blank_name = client
        .post(format!(
            "{base_url}/emby/Items/RemoteSearch/Movie",
            base_url = fixture.base_url
        ))
        .query(&[("api_key", fixture.key.as_str())])
        .json(&json!({
            "ItemId": fixture.public_item_id,
            "SearchInfo": { "Name": "   " }
        }))
        .send()
        .await?;
    assert_eq!(blank_name.status(), StatusCode::BAD_REQUEST);

    let wrong_type = client
        .post(format!(
            "{base_url}/emby/Items/RemoteSearch/Person",
            base_url = fixture.base_url
        ))
        .query(&[("api_key", fixture.key.as_str())])
        .json(&search_body)
        .send()
        .await?;
    assert_eq!(wrong_type.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn remote_search_rejects_non_administrators() -> Result<(), Box<dyn std::error::Error>> {
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
    let (base_url, _server) = start_server(config, database.clone(), setup).await?;
    let token = viewer_token(&base_url, &database).await?;
    let client = test_client();

    for path in [
        "/emby/Items/RemoteSearch/Movie",
        "/emby/Items/RemoteSearch/Apply/any",
    ] {
        let response = client
            .post(format!("{base_url}{path}"))
            .header("X-Emby-Token", &token)
            .json(&json!({ "ItemId": "1", "SearchInfo": { "Name": "x" } }))
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
    }
    let images = client
        .get(format!("{base_url}/emby/Items/1/RemoteImages"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(images.status(), StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn remote_search_apply_rejects_unknown_candidates() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = fixture().await?;
    let client = test_client();

    let unknown_candidate = client
        .post(format!(
            "{base_url}/emby/Items/RemoteSearch/Apply/{item_id}",
            base_url = fixture.base_url,
            item_id = fixture.public_item_id
        ))
        .query(&[("api_key", fixture.key.as_str())])
        .json(&json!({ "Id": "not-a-candidate", "ProviderIds": { "Tmdb": "999999" } }))
        .send()
        .await?;
    assert_eq!(unknown_candidate.status(), StatusCode::NOT_FOUND);

    let unknown_item = client
        .post(format!(
            "{base_url}/emby/Items/RemoteSearch/Apply/{item_id}",
            base_url = fixture.base_url,
            item_id = emby_public_id(&uuid::Uuid::now_v7().to_string())
        ))
        .query(&[("api_key", fixture.key.as_str())])
        .json(&json!({ "Id": "x" }))
        .send()
        .await?;
    assert_eq!(unknown_item.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn remote_images_validate_type_and_item() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture().await?;
    let client = test_client();

    let invalid_type = client
        .get(format!(
            "{base_url}/emby/Items/{item_id}/RemoteImages?Type=Nope",
            base_url = fixture.base_url,
            item_id = fixture.public_item_id
        ))
        .query(&[("api_key", fixture.key.as_str())])
        .send()
        .await?;
    assert_eq!(invalid_type.status(), StatusCode::BAD_REQUEST);

    // The item has no provider id yet, and the library has no scraper: both are reported as a
    // conflict, never as an empty image list.
    let unresolved = client
        .get(format!(
            "{base_url}/emby/Items/{item_id}/RemoteImages?Type=Primary",
            base_url = fixture.base_url,
            item_id = fixture.public_item_id
        ))
        .query(&[("api_key", fixture.key.as_str())])
        .send()
        .await?;
    let status = unresolved.status();
    assert!(
        status == StatusCode::CONFLICT || status == StatusCode::SERVICE_UNAVAILABLE,
        "unexpected status {status}"
    );
    Ok(())
}

#[tokio::test]
async fn remote_image_download_validates_the_body() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture().await?;
    let client = test_client();

    let missing_url = client
        .post(format!(
            "{base_url}/emby/Items/{item_id}/RemoteImages/Download",
            base_url = fixture.base_url,
            item_id = fixture.public_item_id
        ))
        .query(&[("api_key", fixture.key.as_str())])
        .json(&json!({ "Type": "Primary" }))
        .send()
        .await?;
    assert_eq!(missing_url.status(), StatusCode::BAD_REQUEST);

    let invalid_type = client
        .post(format!(
            "{base_url}/emby/Items/{item_id}/RemoteImages/Download",
            base_url = fixture.base_url,
            item_id = fixture.public_item_id
        ))
        .query(&[("api_key", fixture.key.as_str())])
        .json(&json!({ "Type": "Nope", "ImageUrl": "https://img.example/x.jpg" }))
        .send()
        .await?;
    assert_eq!(invalid_type.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn image_delete_maps_type_and_index() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture().await?;
    let client = test_client();

    let invalid_index = client
        .delete(format!(
            "{base_url}/emby/Items/{item_id}/Images/Primary/abc/Delete",
            base_url = fixture.base_url,
            item_id = fixture.public_item_id
        ))
        .query(&[("api_key", fixture.key.as_str())])
        .send()
        .await?;
    assert_eq!(invalid_index.status(), StatusCode::BAD_REQUEST);

    let invalid_type = client
        .delete(format!(
            "{base_url}/emby/Items/{item_id}/Images/Nope/0/Delete",
            base_url = fixture.base_url,
            item_id = fixture.public_item_id
        ))
        .query(&[("api_key", fixture.key.as_str())])
        .send()
        .await?;
    assert_eq!(invalid_type.status(), StatusCode::BAD_REQUEST);

    // A scanned STRM item has no stored images, so the delete is a clean 404.
    let missing = client
        .delete(format!(
            "{base_url}/emby/Items/{item_id}/Images/Primary/0/Delete",
            base_url = fixture.base_url,
            item_id = fixture.public_item_id
        ))
        .query(&[("api_key", fixture.key.as_str())])
        .send()
        .await?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    Ok(())
}
