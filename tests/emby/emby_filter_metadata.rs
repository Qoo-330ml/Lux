//! Emby filter-panel metadata: `GET /OfficialRatings` and `GET /Tags`.
//!
//! Hills requests both endpoints when it opens a library filter panel. `OfficialRatings` is
//! served from the certification projection that the Emby `OfficialRating` field already renders;
//! `Tags` has no data source yet and deliberately answers an empty envelope instead of `404`.

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
    .bind(format!("filter-metadata:{id}"))
    .execute(database.pool())
    .await
    .map(|_| ())
}

async fn insert_rating(
    database: &Database,
    item_id: &str,
    certification: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO media_content_restriction_index (item_id, certification_code) VALUES (?, ?)",
    )
    .bind(item_id)
    .bind(certification)
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
            r#"Emby Client="FilterMetadataTest", Device="Mac", DeviceId="filter-metadata-device", Version="1""#,
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

struct Fixture {
    admin_token: String,
    viewer_token: String,
    viewer_id: String,
    movies: String,
    private: String,
    base_url: String,
    client: reqwest::Client,
    server: tokio::task::JoinHandle<Result<(), std::io::Error>>,
    _temp_dir: tempfile::TempDir,
}

impl Fixture {
    async fn start() -> Result<Self, Box<dyn std::error::Error>> {
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
        let private = libraries
            .create_library("Private", LibraryKind::Movie, false)
            .await?
            .id
            .to_string();
        let shows = libraries
            .create_library("Shows", LibraryKind::Series, false)
            .await?
            .id
            .to_string();
        insert_item(&database, "rating-movie-1", &movies, "MOVIE").await?;
        insert_item(&database, "rating-movie-2", &movies, "MOVIE").await?;
        insert_item(&database, "rating-movie-3", &movies, "MOVIE").await?;
        insert_item(&database, "rating-private-1", &private, "MOVIE").await?;
        insert_item(&database, "rating-series-1", &shows, "SERIES").await?;
        insert_rating(&database, "rating-movie-1", "PG-13").await?;
        insert_rating(&database, "rating-movie-2", "PG-13").await?;
        insert_rating(&database, "rating-movie-3", "R").await?;
        insert_rating(&database, "rating-private-1", "NC-17").await?;
        insert_rating(&database, "rating-series-1", "TV-14").await?;
        sqlx::query(
            "INSERT INTO user_library_access (user_id, library_id, can_view) VALUES (?, ?, 1)",
        )
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
        let admin_token = emby_login(&client, &base_url, "admin", "correct password").await?;
        let viewer_token = emby_login(&client, &base_url, "viewer", "viewer password").await?;
        Ok(Self {
            admin_token,
            viewer_token,
            viewer_id: viewer.id.to_string(),
            movies,
            private,
            base_url,
            client,
            server,
            _temp_dir: temp_dir,
        })
    }

    async fn get(
        &self,
        path: &str,
        token: &str,
    ) -> Result<(reqwest::StatusCode, Value), Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(format!("{}{path}", self.base_url))
            .header("X-Emby-Token", token)
            .send()
            .await?;
        let status = response.status();
        let body = if status.is_success() {
            response.json().await?
        } else {
            Value::Null
        };
        Ok((status, body))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = &self._temp_dir;
    }
}

#[tokio::test]
async fn official_ratings_are_distinct_and_scoped_to_the_visible_libraries()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::start().await?;

    let unauthorised = fixture
        .client
        .get(format!("{}/OfficialRatings", fixture.base_url))
        .send()
        .await?;
    assert_eq!(unauthorised.status(), reqwest::StatusCode::UNAUTHORIZED);

    let (status, all) = fixture
        .get("/OfficialRatings", &fixture.admin_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&all), ["NC-17", "PG-13", "R", "TV-14"]);
    assert_eq!(all["TotalRecordCount"], 4);
    assert_eq!(all["Items"][1]["Id"], "PG-13");
    assert_eq!(all["Items"][1]["Name"], "PG-13");

    let (status, viewer) = fixture
        .get(
            &format!(
                "/emby/OfficialRatings?UserId={}&Recursive=true",
                fixture.viewer_id
            ),
            &fixture.viewer_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&viewer), ["PG-13", "R"]);
    assert_eq!(viewer["TotalRecordCount"], 2);

    Ok(())
}

#[tokio::test]
async fn official_ratings_apply_parent_id_item_types_and_pagination()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::start().await?;

    let (status, movies) = fixture
        .get(
            &format!("/OfficialRatings?ParentId={}", fixture.movies),
            &fixture.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&movies), ["PG-13", "R"]);
    assert_eq!(movies["TotalRecordCount"], 2);

    let (status, private) = fixture
        .get(
            &format!("/OfficialRatings?ParentId={}", fixture.private),
            &fixture.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&private), ["NC-17"]);

    let (status, series) = fixture
        .get(
            "/OfficialRatings?IncludeItemTypes=Series",
            &fixture.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&series), ["TV-14"]);

    let (status, page) = fixture
        .get(
            "/OfficialRatings?StartIndex=1&Limit=1",
            &fixture.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&page), ["PG-13"]);
    assert_eq!(page["TotalRecordCount"], 4);

    let (status, empty_limit) = fixture
        .get("/OfficialRatings?Limit=&StartIndex=", &fixture.admin_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(empty_limit["TotalRecordCount"], 4);
    assert_eq!(names(&empty_limit).len(), 4);

    let (status, _) = fixture
        .get("/OfficialRatings?Limit=1001", &fixture.admin_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    let (status, _) = fixture
        .get("/OfficialRatings?StartIndex=-1", &fixture.admin_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);

    let (status, _) = fixture
        .get(
            &format!("/OfficialRatings?ParentId={}", fixture.private),
            &fixture.viewer_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);

    let (status, mixed_case) = fixture
        .get("/EMBY/officialratings", &fixture.admin_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(mixed_case["TotalRecordCount"], 4);

    Ok(())
}

#[tokio::test]
async fn tags_answer_an_empty_envelope_on_both_paths() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::start().await?;

    let unauthorised = fixture
        .client
        .get(format!("{}/Tags", fixture.base_url))
        .send()
        .await?;
    assert_eq!(unauthorised.status(), reqwest::StatusCode::UNAUTHORIZED);

    for path in [
        "/Tags",
        "/emby/Tags",
        "/Tags?ParentId=&Limit=&StartIndex=",
        "/EMBY/tags",
    ] {
        let (status, body) = fixture.get(path, &fixture.admin_token).await?;
        assert_eq!(status, reqwest::StatusCode::OK, "path {path}");
        assert_eq!(body["Items"], json!([]), "path {path}");
        assert_eq!(body["TotalRecordCount"], 0, "path {path}");
    }

    Ok(())
}
