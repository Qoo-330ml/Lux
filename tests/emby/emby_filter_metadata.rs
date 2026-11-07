//! Emby filter-panel metadata: `GET /OfficialRatings` and `GET /Tags`.
//!
//! Hills requests both endpoints when it opens a library filter panel. `OfficialRatings` is
//! served from the certification projection that the Emby `OfficialRating` field already renders;
//! `Tags` is served from the `TAG` facet projection through `CatalogService::list_tags`, so both
//! endpoints share the same envelope, the same `ParentId` ACL and the same page bounds. The
//! fixture seeds one hidden library, one restricted item and two spellings of one tag, so a single
//! server proves the projection, the de-duplication, the ACL and the pagination through HTTP.

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

/// One `TAG` facet row, matching what the content-restriction index worker projects from the NFO
/// `<tag>` values: the lower-cased `name_key` is the de-duplication key, `name` the display value.
async fn insert_tag(database: &Database, item_id: &str, tag: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO media_item_facets (item_id, kind, name_key, name) VALUES (?, 'TAG', ?, ?)",
    )
    .bind(item_id)
    .bind(tag.to_lowercase())
    .bind(tag)
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

/// Signs in through the Lux Web API and returns the `Cookie` header value the session endpoints
/// accept. Used only by the parity test, which compares `/Tags` with `/api/v1/tags`.
async fn web_login(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": username, "password": password }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let cookies = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect::<Vec<_>>();
    let session = cookies
        .iter()
        .find_map(|value| value.strip_prefix("lux_session=")?.split(';').next())
        .ok_or("missing lux_session cookie")?;
    let csrf = cookies
        .iter()
        .find_map(|value| value.strip_prefix("lux_csrf=")?.split(';').next())
        .ok_or("missing lux_csrf cookie")?;
    Ok(format!("lux_session={session}; lux_csrf={csrf}"))
}

fn lux_tag_names(body: &Value) -> Vec<&str> {
    body["tags"]
        .as_array()
        .map(|tags| tags.iter().filter_map(|tag| tag["name"].as_str()).collect())
        .unwrap_or_default()
}

struct Fixture {
    database: Database,
    admin_token: String,
    viewer_token: String,
    admin_cookie: String,
    viewer_cookie: String,
    viewer_id: String,
    movies: String,
    private: String,
    shows: String,
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
        // One tag per item plus a second spelling of the movies' `Star Wars` key: the projection
        // must collapse the two spellings into one entry and keep the byte-order smallest name.
        insert_tag(&database, "rating-movie-1", "Star Wars").await?;
        insert_tag(&database, "rating-movie-2", "star wars").await?;
        insert_tag(&database, "rating-movie-3", "Gore").await?;
        insert_tag(&database, "rating-private-1", "Elsewhere").await?;
        insert_tag(&database, "rating-series-1", "Space").await?;

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
        let admin_cookie = web_login(&client, &base_url, "admin", "correct password").await?;
        let viewer_cookie = web_login(&client, &base_url, "viewer", "viewer password").await?;
        Ok(Self {
            database,
            admin_token,
            viewer_token,
            admin_cookie,
            viewer_cookie,
            viewer_id: viewer.id.to_string(),
            movies,
            private,
            shows,
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

    /// `GET /api/v1/tags` with the Lux Web session, used by the parity test.
    async fn lux_tags(
        &self,
        query: &str,
        cookie: &str,
    ) -> Result<(reqwest::StatusCode, Value), Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(format!("{}/api/v1/tags{query}", self.base_url))
            .header(reqwest::header::COOKIE, cookie)
            .send()
            .await?;
        let status = response.status();
        let body = response.json().await?;
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
async fn tags_list_the_visible_tags_and_scope_the_parent_library()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::start().await?;

    let unauthorised = fixture
        .client
        .get(format!("{}/Tags", fixture.base_url))
        .send()
        .await?;
    assert_eq!(unauthorised.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Root path, `/emby` prefix, mixed case and the filter panel's empty `ParentId=`/`Limit=` all
    // answer the same list: an administrator skips the content restrictions and opens every
    // enabled library, and the two `star wars` spellings collapse into one `Star Wars` entry.
    for path in [
        "/Tags",
        "/emby/Tags",
        "/Tags?ParentId=&Limit=&StartIndex=",
        "/EMBY/tags",
    ] {
        let (status, body) = fixture.get(path, &fixture.admin_token).await?;
        assert_eq!(status, reqwest::StatusCode::OK, "path {path}");
        assert_eq!(
            names(&body),
            ["Elsewhere", "Gore", "Space", "Star Wars"],
            "path {path}"
        );
        assert_eq!(body["TotalRecordCount"], 4, "path {path}");
        assert_eq!(body["StartIndex"], 0, "path {path}");
    }

    // The envelope is the `NameIdPair` shape of `/OfficialRatings`: `Id` repeats the tag name and
    // the row carries the server id. There is no facet id, item count or `name_key`.
    let (status, all) = fixture.get("/Tags", &fixture.admin_token).await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    let first = &all["Items"][0];
    assert_eq!(first["Name"], "Elsewhere");
    assert_eq!(first["Id"], "Elsewhere");
    assert!(first["ServerId"].is_string());
    let row_keys = first
        .as_object()
        .map(|row| row.keys().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(row_keys, Some(vec!["Id", "Name", "ServerId"]));

    // The viewer only holds `Movies`, so an empty `ParentId` lists that one library instead of
    // answering 404 or leaking the `Private` tag.
    let (status, viewer) = fixture.get("/Tags", &fixture.viewer_token).await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&viewer), ["Gore", "Star Wars"]);
    assert_eq!(viewer["TotalRecordCount"], 2);

    // Naming the one library the viewer can open, or sending the filter panel's empty
    // `ParentId=`, returns exactly the same rows.
    for path in [
        format!("/Tags?ParentId={}", fixture.movies),
        "/Tags?ParentId=".to_owned(),
    ] {
        let (status, body) = fixture.get(&path, &fixture.viewer_token).await?;
        assert_eq!(status, reqwest::StatusCode::OK, "path {path}");
        assert_eq!(names(&body), ["Gore", "Star Wars"], "path {path}");
        assert_eq!(body["TotalRecordCount"], 2, "path {path}");
    }

    // A library the viewer cannot open, or one that does not exist, answers 404 like
    // `/OfficialRatings`: the ACL is checked by the service, not by the empty answer.
    for path in [
        format!("/Tags?ParentId={}", fixture.private),
        "/Tags?ParentId=00000000-0000-0000-0000-000000000000".to_owned(),
    ] {
        let (status, _) = fixture.get(&path, &fixture.viewer_token).await?;
        assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "path {path}");
    }

    // The admin can open every library, so the same scoped request lists that library's tag.
    let (status, body) = fixture
        .get(
            &format!("/Tags?ParentId={}", fixture.private),
            &fixture.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&body), ["Elsewhere"]);
    assert_eq!(body["TotalRecordCount"], 1);

    // Content restrictions narrow the list the way they narrow `/Items`: the R-rated item's tag
    // disappears for a viewer capped at PG-13.
    fixture
        .restrict_viewer_to_max_rating("MPA", "PG-13")
        .await?;
    let (status, restricted) = fixture.get("/Tags", &fixture.viewer_token).await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&restricted), ["Star Wars"]);
    assert_eq!(restricted["TotalRecordCount"], 1);

    Ok(())
}

#[tokio::test]
async fn tags_apply_include_item_types_and_pagination() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::start().await?;

    let (status, movies) = fixture
        .get(
            &format!("/Tags?ParentId={}", fixture.movies),
            &fixture.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&movies), ["Gore", "Star Wars"]);
    assert_eq!(movies["TotalRecordCount"], 2);

    let (status, shows) = fixture
        .get(
            &format!("/Tags?ParentId={}", fixture.shows),
            &fixture.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&shows), ["Space"]);

    // `IncludeItemTypes` goes through the `/OfficialRatings` parser: a recognized value narrows
    // the tag list, an entirely unrecognized list matches nothing, and a mixed list keeps only the
    // types it recognizes.
    let (status, series) = fixture
        .get("/Tags?IncludeItemTypes=Series", &fixture.admin_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&series), ["Space"]);
    assert_eq!(series["TotalRecordCount"], 1);

    let (status, movie_types) = fixture
        .get("/Tags?IncludeItemTypes=Movie", &fixture.admin_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&movie_types), ["Elsewhere", "Gore", "Star Wars"]);

    let (status, unknown_type) = fixture
        .get("/Tags?IncludeItemTypes=Photo", &fixture.admin_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(names(&unknown_type).is_empty());
    assert_eq!(unknown_type["TotalRecordCount"], 0);

    let (status, mixed_types) = fixture
        .get("/Tags?IncludeItemTypes=Photo,Series", &fixture.admin_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&mixed_types), ["Space"]);

    // `StartIndex`/`Limit` page the `name_key` order and keep the total of the whole list.
    let (status, page) = fixture
        .get("/Tags?StartIndex=1&Limit=2", &fixture.admin_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&page), ["Gore", "Space"]);
    assert_eq!(page["TotalRecordCount"], 4);
    assert_eq!(page["StartIndex"], 1);

    let (status, past_end) = fixture
        .get("/Tags?StartIndex=4&Limit=2", &fixture.admin_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(names(&past_end).is_empty());
    assert_eq!(past_end["TotalRecordCount"], 4);

    // Empty `StartIndex=`/`Limit=` and `Limit=0` keep the `/OfficialRatings` defaults.
    let (status, empty_params) = fixture
        .get("/Tags?Limit=&StartIndex=", &fixture.admin_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&empty_params).len(), 4);

    let (status, zero_limit) = fixture.get("/Tags?Limit=0", &fixture.admin_token).await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&zero_limit).len(), 4);

    // The server-side upper bound and a negative offset are rejected, not clamped.
    for path in ["/Tags?Limit=1001", "/Tags?StartIndex=-1"] {
        let (status, _) = fixture.get(path, &fixture.admin_token).await?;
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "path {path}");
    }

    Ok(())
}

/// Asserts that one Emby `/Tags` request and one `/api/v1/tags` request name the same rows in the
/// same order and report the same total, and returns the names.
async fn assert_tag_names_match(
    fixture: &Fixture,
    emby_path: &str,
    lux_query: &str,
    cookie: &str,
    token: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let (status, emby) = fixture.get(emby_path, token).await?;
    assert_eq!(status, reqwest::StatusCode::OK, "emby path {emby_path}");
    let (status, lux) = fixture.lux_tags(lux_query, cookie).await?;
    assert_eq!(status, reqwest::StatusCode::OK, "lux query {lux_query}");
    let emby_names = names(&emby)
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let lux_names = lux_tag_names(&lux)
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(emby_names, lux_names, "{emby_path} vs {lux_query}");
    assert_eq!(
        emby["TotalRecordCount"].as_i64(),
        lux["total"].as_i64(),
        "{emby_path} vs {lux_query}"
    );
    Ok(emby_names)
}

/// `/Tags` and `/api/v1/tags` read the same `TAG` projection through `CatalogService::list_tags`,
/// so for one library and one user they must answer the same name set; only the envelope differs.
#[tokio::test]
async fn tags_match_the_lux_tags_endpoint() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::start().await?;

    let movies = assert_tag_names_match(
        &fixture,
        &format!("/Tags?ParentId={}", fixture.movies),
        &format!("?libraryId={}", fixture.movies),
        &fixture.admin_cookie,
        &fixture.admin_token,
    )
    .await?;
    assert_eq!(movies, ["Gore", "Star Wars"]);

    // The viewer's library ACL and the content restrictions apply on both routes.
    let viewer = assert_tag_names_match(
        &fixture,
        "/Tags",
        "",
        &fixture.viewer_cookie,
        &fixture.viewer_token,
    )
    .await?;
    assert_eq!(viewer, ["Gore", "Star Wars"]);

    // `IncludeItemTypes` and `itemType` share one parser, so a type filter selects the same page.
    let series = assert_tag_names_match(
        &fixture,
        "/Tags?IncludeItemTypes=Series",
        "?itemType=SERIES",
        &fixture.admin_cookie,
        &fixture.admin_token,
    )
    .await?;
    assert_eq!(series, ["Space"]);

    // The second page keeps the same order and total on both routes (`StartIndex` is an offset,
    // `page` is 1-based).
    let second_page = assert_tag_names_match(
        &fixture,
        "/Tags?StartIndex=2&Limit=2",
        "?page=2&pageSize=2",
        &fixture.admin_cookie,
        &fixture.admin_token,
    )
    .await?;
    assert_eq!(second_page, ["Space", "Star Wars"]);

    // The administrator can address the library the viewer cannot open.
    let private = assert_tag_names_match(
        &fixture,
        &format!("/Tags?ParentId={}", fixture.private),
        &format!("?libraryId={}", fixture.private),
        &fixture.admin_cookie,
        &fixture.admin_token,
    )
    .await?;
    assert_eq!(private, ["Elsewhere"]);

    Ok(())
}
