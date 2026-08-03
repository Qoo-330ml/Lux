use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn lux_and_emby_catalogs_list_page_and_show_movie_details()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let users = UserStore::new(database.clone())?;
    let viewer = users
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    let first_dir = media_root.join("Alpha Movie (2020)");
    let second_dir = media_root.join("Beta Movie (2021)");
    tokio::fs::create_dir_all(&first_dir).await?;
    tokio::fs::create_dir_all(&second_dir).await?;
    tokio::fs::write(first_dir.join("Alpha.Movie.2020.mkv"), b"alpha").await?;
    tokio::fs::write(second_dir.join("Beta.Movie.2021.mp4"), b"beta").await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE sort_title = 'alpha movie'")
            .fetch_one(database.pool())
            .await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config, database, setup, web_auth, emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let admin_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="LuxTest", Device="Mac", DeviceId="admin-device", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_token = admin_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing admin token")?
        .to_owned();

    let views = client
        .get(format!("{base_url}/Users/{}/Views", admin.id))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(views.status(), reqwest::StatusCode::OK);
    let views_body: Value = views.json().await?;
    assert_eq!(views_body["TotalRecordCount"], 1);
    assert_eq!(views_body["Items"][0]["CollectionType"], "movies");

    let emby_page = client
        .get(format!(
            "{base_url}/Users/{}/Items?ParentId={}&Limit=1",
            admin.id, library.id
        ))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(emby_page.status(), reqwest::StatusCode::OK);
    let emby_page_body: Value = emby_page.json().await?;
    assert_eq!(emby_page_body["TotalRecordCount"], 2);
    assert_eq!(emby_page_body["StartIndex"], 0);
    assert_eq!(emby_page_body["Items"].as_array().map(Vec::len), Some(1));
    assert_eq!(emby_page_body["Items"][0]["Type"], "Movie");
    assert_eq!(emby_page_body["Items"][0]["Name"], "Alpha Movie");
    assert_eq!(
        emby_page_body["Items"][0]["MediaSources"][0]["Container"],
        "mkv"
    );

    let detail = client
        .get(format!("{base_url}/Items/{item_id}?api_key={admin_token}"))
        .send()
        .await?;
    assert_eq!(detail.status(), reqwest::StatusCode::OK);
    let detail_body: Value = detail.json().await?;
    assert_eq!(detail_body["Id"], item_id);
    assert_eq!(detail_body["Name"], "Alpha Movie");
    assert_eq!(detail_body["ProductionYear"], 2020);
    assert_eq!(detail_body["ImageTags"], json!({}));

    let viewer_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="LuxTest", Device="Mac", DeviceId="viewer-device", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    let viewer_token = viewer_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let forbidden = client
        .get(format!("{base_url}/Users/{}/Items", admin.id))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    let web_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(web_login.headers(), "lux_session"),
        cookie_value(web_login.headers(), "lux_csrf")
    );
    let lux_page = client
        .get(format!(
            "{base_url}/api/v1/libraries/{}/items?pageSize=1",
            library.id
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(lux_page.status(), reqwest::StatusCode::OK);
    let lux_page_body: Value = lux_page.json().await?;
    assert_eq!(lux_page_body["total"], 2);
    assert_eq!(lux_page_body["pageSize"], 1);
    assert_eq!(lux_page_body["items"][0]["title"], "Alpha Movie");

    let home = client
        .get(format!("{base_url}/api/v1/home"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(home.status(), reqwest::StatusCode::OK);
    let home_body: Value = home.json().await?;
    assert_eq!(home_body["recentlyAddedTotal"], 2);
    assert_eq!(home_body["recentlyAdded"].as_array().map(Vec::len), Some(2));

    let lux_detail = client
        .get(format!("{base_url}/api/v1/items/{item_id}"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(lux_detail.status(), reqwest::StatusCode::OK);
    let lux_detail_body: Value = lux_detail.json().await?;
    assert_eq!(lux_detail_body["id"], item_id);
    assert_eq!(lux_detail_body["productionYear"], 2020);
    assert_eq!(
        lux_detail_body["mediaSources"].as_array().map(Vec::len),
        Some(1)
    );

    assert_ne!(admin.id, viewer.id);
    server.abort();
    Ok(())
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
