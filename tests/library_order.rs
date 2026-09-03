use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn emby_public_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
}

fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> String {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _) = value.split_once(';')?;
            let (cookie_name, cookie_value) = pair.split_once('=')?;
            (cookie_name == name).then(|| cookie_value.to_owned())
        })
        .expect("expected cookie")
}

#[tokio::test]
async fn library_order_is_persisted_and_used_by_web_and_emby_views()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let first = libraries
        .create_library("Movies A", LibraryKind::Movie, false)
        .await?;
    let second = libraries
        .create_library("Movies B", LibraryKind::Movie, false)
        .await?;
    let emby_first_id = emby_public_id(&first.id.to_string());
    let emby_second_id = emby_public_id(&second.id.to_string());
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

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(login.headers(), "lux_session"),
        cookie_value(login.headers(), "lux_csrf")
    );
    let csrf = cookie_value(login.headers(), "lux_csrf");

    let defaults = client
        .get(format!("{base_url}/api/v1/auth/library-order"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(defaults.status(), reqwest::StatusCode::OK);
    assert_eq!(defaults.json::<Value>().await?["libraryOrder"], json!([]));

    let updated = client
        .patch(format!("{base_url}/api/v1/auth/library-order"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "libraryOrder": [second.id, first.id] }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    assert_eq!(
        updated.json::<Value>().await?["libraryOrder"],
        json!([second.id.to_string(), first.id.to_string()])
    );

    let web_libraries = client
        .get(format!("{base_url}/api/v1/libraries"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert_eq!(
        web_libraries["libraries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|library| library["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![second.id.to_string(), first.id.to_string()]
    );

    let emby_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            "Emby Client=\"Test\", Device=\"Test\", DeviceId=\"library-order\", Version=\"1\"",
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let emby_login_body = emby_login.json::<Value>().await?;
    assert_eq!(
        emby_login_body["User"]["Configuration"]["OrderedViews"],
        json!([emby_second_id, emby_first_id])
    );
    let emby_token = emby_login_body["AccessToken"]
        .as_str()
        .ok_or("missing Emby token")?
        .to_owned();
    let views = client
        .get(format!("{base_url}/Users/{}/Views", admin.id))
        .header("X-Emby-Token", &emby_token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert_eq!(
        views["Items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|library| library["Id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![emby_second_id, emby_first_id]
    );

    server.abort();
    Ok(())
}
