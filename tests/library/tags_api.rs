//! Contract tests for the Lux native `GET /api/v1/tags` endpoint.
//!
//! The endpoint lists the distinct TAG facets of the items the caller can see. The fixture puts
//! one hidden library, one R-rated item and one tag spelled in two cases behind the same catalog,
//! so a single server proves the library ACL, the content restriction and the `name_key`
//! de-duplication of the storage projection through the HTTP layer.

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

struct Harness {
    client: reqwest::Client,
    base_url: String,
    admin_cookie: String,
    viewer_cookie: String,
    database: Database,
    server: tokio::task::JoinHandle<Result<(), std::io::Error>>,
    _temp_dir: tempfile::TempDir,
}

/// One item with one TAG facet and one content restriction rating, matching what the scanner
/// projects into `media_item_facets` / `media_content_restriction_index`.
async fn insert_tag_item(
    database: &Database,
    id: &str,
    library_id: &str,
    item_type: &str,
    tag: &str,
    certification: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO media_items
         (id, library_id, item_type, title, sort_title, identification_status, has_available_source)
         VALUES (?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', 1)",
    )
    .bind(id)
    .bind(library_id)
    .bind(item_type)
    .bind(id)
    .bind(id.to_lowercase())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_item_facets (item_id, kind, name_key, name) VALUES (?, 'TAG', ?, ?)",
    )
    .bind(id)
    .bind(tag.to_lowercase())
    .bind(tag)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_content_restriction_index (item_id, certification_code) VALUES (?, ?)",
    )
    .bind(id)
    .bind(certification)
    .execute(database.pool())
    .await?;
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

fn session_cookies(headers: &reqwest::header::HeaderMap) -> String {
    format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(headers, "lux_session"),
        cookie_value(headers, "lux_csrf")
    )
}

/// Seeds three libraries (two visible, one not granted), five tagged items and the viewer's
/// PG-13 content restriction.
async fn start_server() -> Result<Harness, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let _admin = setup.complete("Admin", "Admin", "admin password").await?;
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
    let hidden = libraries
        .create_library("Hidden", LibraryKind::Movie, false)
        .await?
        .id
        .to_string();

    // `Star Wars` and `star wars` share the `star wars` name key, so they must collapse into one
    // tag whose display name is the byte-order smallest spelling.
    insert_tag_item(
        &database,
        "tag-movie-one",
        &movies,
        "MOVIE",
        "Star Wars",
        "PG",
    )
    .await?;
    insert_tag_item(
        &database,
        "tag-movie-one-b",
        &movies,
        "MOVIE",
        "Space",
        "PG",
    )
    .await?;
    insert_tag_item(
        &database,
        "tag-movie-two",
        &movies,
        "MOVIE",
        "star wars",
        "PG",
    )
    .await?;
    insert_tag_item(
        &database,
        "tag-movie-two-b",
        &movies,
        "MOVIE",
        "Adventure",
        "PG",
    )
    .await?;
    insert_tag_item(&database, "tag-series-one", &shows, "SERIES", "Space", "PG").await?;
    insert_tag_item(
        &database,
        "tag-series-one-b",
        &shows,
        "SERIES",
        "Mystery",
        "PG",
    )
    .await?;
    // Above the viewer's PG-13 limit: its tag never reaches the viewer's list.
    insert_tag_item(&database, "tag-restricted", &movies, "MOVIE", "Gore", "R").await?;
    // In a library the viewer was not granted.
    insert_tag_item(&database, "tag-hidden", &hidden, "MOVIE", "Elsewhere", "PG").await?;

    let viewer_id = viewer.id.to_string();
    for library_id in [&movies, &shows] {
        sqlx::query(
            "INSERT INTO user_library_access (user_id, library_id, can_view) VALUES (?, ?, 1)",
        )
        .bind(&viewer_id)
        .bind(library_id)
        .execute(database.pool())
        .await?;
    }
    sqlx::query("UPDATE users SET library_access_mode = 'SELECTED' WHERE id = ?")
        .bind(&viewer_id)
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO user_content_restrictions
         (user_id, rating_system, maximum_rating, allow_unrated)
         VALUES (?, 'MPA', 'PG-13', 0)",
    )
    .bind(&viewer_id)
    .execute(database.pool())
    .await?;

    let harness_database = database.clone();
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

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "admin password" }))
        .send()
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_cookie = session_cookies(admin_login.headers());
    let viewer_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    assert_eq!(viewer_login.status(), reqwest::StatusCode::OK);
    let viewer_cookie = session_cookies(viewer_login.headers());

    Ok(Harness {
        client,
        base_url,
        admin_cookie,
        viewer_cookie,
        database: harness_database,
        server,
        _temp_dir: temp_dir,
    })
}

impl Harness {
    async fn lux_tags(
        &self,
        query: &str,
        cookie: Option<&str>,
    ) -> Result<(reqwest::StatusCode, Value), Box<dyn std::error::Error>> {
        let mut request = self
            .client
            .get(format!("{}/api/v1/tags{query}", self.base_url));
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie);
        }
        let response = request.send().await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        };
        Ok((status, body))
    }

    async fn library_id(&self, name: &str) -> Result<String, sqlx::Error> {
        sqlx::query_scalar("SELECT id FROM libraries WHERE name = ?")
            .bind(name)
            .fetch_one(self.database.pool())
            .await
    }
}

fn tag_names(body: &Value) -> Vec<&str> {
    body["tags"]
        .as_array()
        .map(|tags| tags.iter().filter_map(|tag| tag["name"].as_str()).collect())
        .unwrap_or_default()
}

/// Asserts the documented envelope: exactly `tags`, `page`, `pageSize` and `total`, and every
/// entry carries only `name` (no `nameKey`, no item count).
fn assert_tags_page(body: &Value, names: &[&str], page: i64, page_size: i64, total: i64) {
    assert_eq!(tag_names(body), names);
    assert_eq!(body["page"], page);
    assert_eq!(body["pageSize"], page_size);
    assert_eq!(body["total"], total);
    let keys = body
        .as_object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    assert_eq!(
        keys,
        ["page", "pageSize", "tags", "total"],
        "the envelope must not leak the internal item counts or a `name_key`"
    );
    for tag in body["tags"].as_array().into_iter().flatten() {
        let keys = tag
            .as_object()
            .map(|object| object.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        assert_eq!(keys, ["name"], "a tag entry exposes only its name");
    }
}

#[tokio::test]
async fn lux_tags_lists_the_distinct_visible_tags() -> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    let (status, body) = harness.lux_tags("", Some(&harness.viewer_cookie)).await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    // Sorted by `name_key`; the hidden library, the R-rated `Gore` tag and the duplicate
    // `star wars` spelling never surface.
    assert_tags_page(
        &body,
        &["Adventure", "Mystery", "Space", "Star Wars"],
        1,
        50,
        4,
    );

    // An administrator skips the content restrictions and sees every enabled library.
    let (status, admin_body) = harness.lux_tags("", Some(&harness.admin_cookie)).await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_tags_page(
        &admin_body,
        &[
            "Adventure",
            "Elsewhere",
            "Gore",
            "Mystery",
            "Space",
            "Star Wars",
        ],
        1,
        50,
        6,
    );

    harness.server.abort();
    Ok(())
}

#[tokio::test]
async fn lux_tags_pages_and_rejects_invalid_page_parameters()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    let (status, first) = harness
        .lux_tags("?pageSize=2", Some(&harness.viewer_cookie))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_tags_page(&first, &["Adventure", "Mystery"], 1, 2, 4);

    let (status, second) = harness
        .lux_tags("?pageSize=2&page=2", Some(&harness.viewer_cookie))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_tags_page(&second, &["Space", "Star Wars"], 2, 2, 4);

    // A page past the end is a 200 with an empty list and the unchanged total.
    let (status, beyond) = harness
        .lux_tags("?pageSize=2&page=3", Some(&harness.viewer_cookie))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_tags_page(&beyond, &[], 3, 2, 4);

    let assembled = [&first, &second]
        .into_iter()
        .flat_map(tag_names)
        .collect::<Vec<_>>();
    assert_eq!(
        assembled
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        assembled.len(),
        "pagination must not repeat a tag"
    );

    // `pageSize` defaults to 50 and the documented upper bound is 1000.
    let (status, capped) = harness
        .lux_tags("?pageSize=1000", Some(&harness.viewer_cookie))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_tags_page(
        &capped,
        &["Adventure", "Mystery", "Space", "Star Wars"],
        1,
        1000,
        4,
    );

    for query in [
        "?page=0",
        "?page=-1",
        "?page=abc",
        "?page=1.5",
        "?pageSize=0",
        "?pageSize=-1",
        "?pageSize=1001",
        "?pageSize=abc",
        "?pageSize=1.5",
    ] {
        let (status, body) = harness
            .lux_tags(query, Some(&harness.viewer_cookie))
            .await?;
        assert_eq!(
            status,
            reqwest::StatusCode::BAD_REQUEST,
            "query {query} must be rejected"
        );
        assert_eq!(body["error"]["code"], "INVALID_REQUEST", "query {query}");
    }

    harness.server.abort();
    Ok(())
}

#[tokio::test]
async fn lux_tags_filters_by_library_and_item_type() -> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    let movies = harness.library_id("Movies").await?;
    let shows = harness.library_id("Shows").await?;

    let viewer = Some(harness.viewer_cookie.as_str());
    for (query, names, total) in [
        (
            "?itemType=MOVIE".to_owned(),
            vec!["Adventure", "Space", "Star Wars"],
            3,
        ),
        ("?itemType=SERIES".to_owned(), vec!["Mystery", "Space"], 2),
        (
            "?itemType=MOVIE,SERIES".to_owned(),
            vec!["Adventure", "Mystery", "Space", "Star Wars"],
            4,
        ),
        // The parameter may repeat instead of using commas.
        (
            "?itemType=movie&itemType=series".to_owned(),
            vec!["Adventure", "Mystery", "Space", "Star Wars"],
            4,
        ),
        (
            "?itemType=movie,series".to_owned(),
            vec!["Adventure", "Mystery", "Space", "Star Wars"],
            4,
        ),
        // An empty value means every type, like the other Lux list endpoints.
        (
            "?itemType=".to_owned(),
            vec!["Adventure", "Mystery", "Space", "Star Wars"],
            4,
        ),
        // Nothing recognized matches no rows instead of dropping the restriction.
        ("?itemType=UNKNOWN".to_owned(), Vec::new(), 0),
        (
            format!("?libraryId={movies}"),
            vec!["Adventure", "Space", "Star Wars"],
            3,
        ),
        (format!("?libraryId={shows}"), vec!["Mystery", "Space"], 2),
        (format!("?libraryId={shows}&itemType=MOVIE"), Vec::new(), 0),
        (
            "?libraryId=".to_owned(),
            vec!["Adventure", "Mystery", "Space", "Star Wars"],
            4,
        ),
    ] {
        let (status, body) = harness.lux_tags(&query, viewer).await?;
        assert_eq!(status, reqwest::StatusCode::OK, "query {query}");
        assert_tags_page(&body, &names, 1, 50, total);
    }

    harness.server.abort();
    Ok(())
}

#[tokio::test]
async fn lux_tags_answers_404_for_unknown_or_invisible_libraries_and_401_without_a_session()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    let hidden = harness.library_id("Hidden").await?;
    for query in [
        format!("?libraryId={hidden}"),
        "?libraryId=00000000-0000-7000-8000-000000000999".to_owned(),
        "?libraryId=not-a-library-id".to_owned(),
    ] {
        let (status, body) = harness
            .lux_tags(&query, Some(&harness.viewer_cookie))
            .await?;
        assert_eq!(
            status,
            reqwest::StatusCode::NOT_FOUND,
            "query {query} must not leak the library"
        );
        assert_eq!(body["error"]["code"], "NOT_FOUND", "query {query}");
    }

    // The administrator can address the same hidden library explicitly.
    let (status, body) = harness
        .lux_tags(&format!("?libraryId={hidden}"), Some(&harness.admin_cookie))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_tags_page(&body, &["Elsewhere"], 1, 50, 1);

    let (status, body) = harness.lux_tags("", None).await?;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "AUTHENTICATION_REQUIRED");

    harness.server.abort();
    Ok(())
}
