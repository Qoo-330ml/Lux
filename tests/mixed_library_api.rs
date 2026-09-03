use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn emby_public_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
}

async fn emby_login(
    client: &reqwest::Client,
    base_url: &str,
    client_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            format!(
                "Emby Client=\"{client_name}\", Device=\"Mac\", DeviceId=\"mixed-{client_name}\", Version=\"test\""
            ),
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    Ok(response.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing access token")?
        .to_owned())
}

#[tokio::test]
async fn all_emby_clients_use_the_standard_mixed_library_shape()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let library = LibraryService::new(database.clone())
        .create_library("Mixed", LibraryKind::Mixed, false)
        .await?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let emby_library_id = emby_public_id(&library.id.to_string());

    let emby_token = emby_login(&client, &base_url, "Emby").await?;
    let emby_views = client
        .get(format!("{base_url}/Users/{}/Views", admin.id))
        .header("X-Emby-Token", &emby_token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert_eq!(emby_views["Items"][0]["Id"], emby_library_id);
    assert!(emby_views["Items"][0]["CollectionType"].is_null());

    let vidhub_token = emby_login(&client, &base_url, "VidHub").await?;
    let vidhub_views = client
        .get(format!("{base_url}/Users/{}/Views", admin.id))
        .header("X-Emby-Token", &vidhub_token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert_eq!(vidhub_views["Items"][0]["Id"], emby_library_id);
    assert!(vidhub_views["Items"][0]["CollectionType"].is_null());

    let vidhub_detail = client
        .get(format!(
            "{base_url}/Users/{}/Items/{}",
            admin.id, emby_library_id
        ))
        .header("X-Emby-Token", &vidhub_token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert!(vidhub_detail["CollectionType"].is_null());

    let vidhub_virtual_folders = client
        .get(format!("{base_url}/Library/VirtualFolders"))
        .header("X-Emby-Token", &vidhub_token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert!(vidhub_virtual_folders[0]["CollectionType"].is_null());
    assert!(vidhub_virtual_folders[0]["LibraryOptions"]["ContentType"].is_null());
    assert_eq!(
        vidhub_virtual_folders[0]["LibraryOptions"]["TypeOptions"]
            .as_array()
            .map(|items| items
                .iter()
                .map(|item| item["Type"].clone())
                .collect::<Vec<_>>()),
        Some(vec![json!("Movie"), json!("Series")])
    );

    let yamby_token = emby_login(&client, &base_url, "Yamby").await?;
    let yamby_views = client
        .get(format!("{base_url}/Users/{}/Views", admin.id))
        .header("X-Emby-Token", &yamby_token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert_eq!(yamby_views["Items"][0]["Id"], emby_library_id);
    assert!(yamby_views["Items"][0]["CollectionType"].is_null());

    let yamby_detail = client
        .get(format!(
            "{base_url}/Users/{}/Items/{}",
            admin.id, emby_library_id
        ))
        .header("X-Emby-Token", &yamby_token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert!(yamby_detail["CollectionType"].is_null());

    let yamby_virtual_folders = client
        .get(format!("{base_url}/Library/VirtualFolders"))
        .header("X-Emby-Token", &yamby_token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert!(yamby_virtual_folders[0]["CollectionType"].is_null());
    assert!(yamby_virtual_folders[0]["LibraryOptions"]["ContentType"].is_null());
    assert_eq!(
        yamby_virtual_folders[0]["LibraryOptions"]["TypeOptions"]
            .as_array()
            .map(|items| items
                .iter()
                .map(|item| item["Type"].clone())
                .collect::<Vec<_>>()),
        Some(vec![json!("Movie"), json!("Series")])
    );

    server.abort();
    Ok(())
}
