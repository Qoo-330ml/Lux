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
    database: Database,
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
            config,
            database.clone(),
            setup,
            web_auth,
            emby_auth,
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let base_url = format!("http://{address}");
        let client = reqwest::Client::builder().no_proxy().build()?;
        let admin_token = emby_login(&client, &base_url, "admin", "correct password").await?;
        let viewer_token = emby_login(&client, &base_url, "viewer", "viewer password").await?;
        Ok(Self {
            database,
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

    async fn restrict_viewer_to_max_rating(
        &self,
        rating_system: &str,
        maximum_rating: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO user_content_restrictions
             (user_id, rating_system, maximum_rating, allow_unrated)
             VALUES (?, ?, ?, 0)",
        )
        .bind(&self.viewer_id)
        .bind(rating_system)
        .bind(maximum_rating)
        .execute(self.database.pool())
        .await
        .map(|_| ())
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

    // Content restrictions narrow the same list: an MPA PG-13 viewer no longer sees the R rating
    // even though the item carrying it is in a library they can open.
    fixture
        .restrict_viewer_to_max_rating("MPA", "PG-13")
        .await?;
    let (status, restricted) = fixture
        .get(
            &format!("/OfficialRatings?UserId={}", fixture.viewer_id),
            &fixture.viewer_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&restricted), ["PG-13"]);
    assert_eq!(restricted["TotalRecordCount"], 1);

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

    // An entirely unrecognized type list must not degrade into "no type filter" (`/Items` uses the
    // same rule), while a mixed list keeps only the types it recognizes.
    let (status, unknown_type) = fixture
        .get(
            "/OfficialRatings?IncludeItemTypes=Photo",
            &fixture.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(names(&unknown_type).is_empty());
    assert_eq!(unknown_type["TotalRecordCount"], 0);

    let (status, mixed_types) = fixture
        .get(
            "/OfficialRatings?IncludeItemTypes=Photo,Series",
            &fixture.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&mixed_types), ["TV-14"]);

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

    // `Limit<=0` keeps the `/Genres` behavior: the default page size applies.
    let (status, zero_limit) = fixture
        .get("/OfficialRatings?Limit=0", &fixture.admin_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(zero_limit["TotalRecordCount"], 4);
    assert_eq!(names(&zero_limit).len(), 4);

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
async fn tags_answer_an_empty_envelope_and_scope_the_parent_library()
-> Result<(), Box<dyn std::error::Error>> {
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

    // The library scope is enforced even though the list is empty: a future tag projection must
    // not inherit an ACL shortcut just because today's answer has no rows.
    let (status, body) = fixture
        .get(
            &format!("/Tags?ParentId={}", fixture.movies),
            &fixture.viewer_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["Items"], json!([]));
    assert_eq!(body["TotalRecordCount"], 0);

    for path in [
        format!("/Tags?ParentId={}", fixture.private),
        "/Tags?ParentId=00000000-0000-0000-0000-000000000000".to_owned(),
    ] {
        let (status, _) = fixture.get(&path, &fixture.viewer_token).await?;
        assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "path {path}");
    }

    // The admin can open every library, so the same scoped request still succeeds.
    let (status, body) = fixture
        .get(
            &format!("/Tags?ParentId={}", fixture.private),
            &fixture.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["TotalRecordCount"], 0);

    Ok(())
}
