use luxd::{
    api::{AppState, app_with_state},
    application::libraries::LibraryService,
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::net::TcpListener;

async fn insert_item(
    database: &Database,
    id: &str,
    library_id: &str,
    item_type: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO media_items
         (id, library_id, item_type, title, sort_title, identification_status,
          identity_key, has_available_source)
         VALUES (?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', ?, 1)",
    )
    .bind(id)
    .bind(library_id)
    .bind(item_type)
    .bind(id)
    .bind(id)
    .bind(format!("facets:{id}"))
    .execute(database.pool())
    .await
    .map(|_| ())
}

async fn insert_facet(
    database: &Database,
    item_id: &str,
    kind: &str,
    name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO media_item_facets (item_id, kind, name_key, name) VALUES (?, ?, ?, ?)")
        .bind(item_id)
        .bind(kind)
        .bind(name.to_lowercase())
        .bind(name)
        .execute(database.pool())
        .await
        .map(|_| ())
}

async fn emby_login(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base_url}/emby/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="FacetTest", Device="Mac", DeviceId="facet-device", Version="1""#,
        )
        .json(&json!({ "Username": username, "Pw": password }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await?;
    Ok(body["AccessToken"]
        .as_str()
        .ok_or("missing access token")?
        .to_owned())
}

fn names(body: &Value) -> Vec<&str> {
    body["Items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["Name"].as_str())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn emby_genres_and_studios_list_and_filter_items_within_user_scope()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let _admin = setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let movies = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?
        .id
        .to_string();
    let shows = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?
        .id
        .to_string();
    insert_item(&database, "facet-movie-1", &movies, "MOVIE").await?;
    insert_item(&database, "facet-movie-2", &movies, "MOVIE").await?;
    insert_item(&database, "facet-series-1", &shows, "SERIES").await?;
    insert_facet(&database, "facet-movie-1", "GENRE", "Sci-Fi").await?;
    insert_facet(&database, "facet-movie-1", "GENRE", "Drama").await?;
    insert_facet(&database, "facet-movie-1", "STUDIO", "A24").await?;
    insert_facet(&database, "facet-movie-2", "GENRE", "Drama").await?;
    insert_facet(&database, "facet-series-1", "GENRE", "Horror").await?;
    insert_facet(&database, "facet-series-1", "STUDIO", "HBO").await?;
    sqlx::query("INSERT INTO user_library_access (user_id, library_id, can_view) VALUES (?, ?, 1)")
        .bind(viewer.id.to_string())
        .bind(&movies)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE users SET library_access_mode = 'SELECTED' WHERE id = ?")
        .bind(viewer.id.to_string())
        .execute(database.pool())
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
    let client = reqwest::Client::builder().no_proxy().build()?;

    let unauthenticated = client.get(format!("{base_url}/emby/Genres")).send().await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let admin = emby_login(&client, &base_url, "admin", "correct password").await?;
    let genres: Value = client
        .get(format!("{base_url}/emby/Genres"))
        .header("X-Emby-Token", &admin)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(names(&genres), ["Drama", "Horror", "Sci-Fi"]);
    assert_eq!(genres["TotalRecordCount"], 3);
    assert_eq!(genres["Items"][0]["Type"], "Genre");
    assert!(
        genres["Items"][0]["Id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );

    let studios: Value = client
        .get(format!("{base_url}/Studios?StartIndex=1&Limit=1"))
        .header("X-Emby-Token", &admin)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(names(&studios), ["HBO"]);
    assert_eq!(studios["TotalRecordCount"], 2);
    assert_eq!(studios["Items"][0]["Type"], "Studio");

    let viewer_token = emby_login(&client, &base_url, "viewer", "viewer password").await?;
    let viewer_genres: Value = client
        .get(format!("{base_url}/emby/Genres?UserId={}", viewer.id))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(names(&viewer_genres), ["Drama", "Sci-Fi"]);

    let movie_genres: Value = client
        .get(format!("{base_url}/emby/Genres?IncludeItemTypes=Series"))
        .header("X-Emby-Token", &admin)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(names(&movie_genres), ["Horror"]);

    let drama: Value = client
        .get(format!(
            "{base_url}/emby/Items?Recursive=true&IncludeItemTypes=Movie&Genres=drama"
        ))
        .header("X-Emby-Token", &admin)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(drama["TotalRecordCount"], 2);
    let either: Value = client
        .get(format!(
            "{base_url}/emby/Items?Recursive=true&Genres=Sci-Fi%7CHorror"
        ))
        .header("X-Emby-Token", &admin)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(either["TotalRecordCount"], 2);
    let a24: Value = client
        .get(format!("{base_url}/emby/Items?Recursive=true&Studios=A24"))
        .header("X-Emby-Token", &admin)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(a24["TotalRecordCount"], 1);

    server.abort();
    Ok(())
}
