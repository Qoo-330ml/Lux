//! Emby list endpoints treat empty optional numeric query values as absent (LUX-888).
//!
//! The Hills/Yamby capture in `docs/reviews/2026-11-06-hills-yamby-capture.md` (§2 problem 3)
//! records the media library filter panel requesting `Genres`/`Years`/`Studios` with an **empty**
//! `Limit=`, and Lux answering `400` with
//! `Failed to deserialize query string: Limit: cannot parse integer from empty string`, which left
//! the panel empty. The `IsFavorite=` regression fixed the same class of failure for optional
//! booleans by treating an empty value as "not provided"; these regressions pin the numeric
//! counterpart on every Emby list endpoint that accepted an optional number, and keep the existing
//! validation for non-empty values (unparsable values and values above the server-side cap stay
//! `400`).
//!
//! Both the root paths and the `/emby` prefix are covered because the router mounts the same tree
//! twice.

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// Query suffixes whose value is absent-or-blank and therefore must behave like a missing key.
const BLANK_SUFFIXES: [&str; 4] = [
    "?Limit=",
    "?StartIndex=",
    "?Limit=&StartIndex=",
    "?Limit=%20%20&StartIndex=%20",
];

/// Query suffixes that carry a value and must keep failing for list endpoints capped at 1000.
const INVALID_SUFFIXES: [&str; 5] = [
    "?Limit=abc",
    "?Limit=1.5",
    "?Limit=1001",
    "?StartIndex=abc",
    "?StartIndex=-1",
];

/// `Genres`, `Years`, `Studios`, `Persons`, `Library/MediaFolders` and `Users/Query` also declare
/// the camelCase aliases, so a blank alias needs the same treatment there. `/Items` does not
/// declare them (that is existing behaviour, not part of this regression).
const BLANK_ALIAS_SUFFIXES: [&str; 2] = ["?limit=", "?startIndex="];
const INVALID_ALIAS_SUFFIXES: [&str; 2] = ["?limit=abc", "?startIndex=abc"];

struct TestServer {
    base_url: String,
    client: reqwest::Client,
    admin_token: String,
    admin_id: String,
    _temp_dir: tempfile::TempDir,
    _server: tokio::task::JoinHandle<Result<(), std::io::Error>>,
}

impl TestServer {
    /// Performs one authenticated read and returns the status with the decoded JSON body (or
    /// `Value::Null` when the body is not JSON, which the `400` assertions rely on).
    async fn get(
        &self,
        path: &str,
    ) -> Result<(reqwest::StatusCode, Value), Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(format!("{}{path}", self.base_url))
            .header("X-Emby-Token", &self.admin_token)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        Ok((status, serde_json::from_str(&body).unwrap_or(Value::Null)))
    }
}

async fn emby_login(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="EmptyNumericParamsTest", Device="Mac", DeviceId="empty-numeric-device", Version="1""#,
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

/// Inserts one catalog item; `item_id` is used verbatim so the Emby id round trip stays trivial.
async fn insert_item(
    database: &Database,
    id: &str,
    library_id: &str,
    item_type: &str,
    year: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO media_items
         (id, library_id, item_type, title, sort_title, identification_status,
          identity_key, has_available_source, production_year)
         VALUES (?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', ?, 1, ?)",
    )
    .bind(id)
    .bind(library_id)
    .bind(item_type)
    .bind(id)
    .bind(id)
    .bind(format!("emby-empty-numeric:{id}"))
    .bind(year)
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

async fn spawn_server() -> Result<TestServer, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    // A second account keeps `/Users/Query` non-empty; the regression only reads it with the
    // administrator token.
    UserStore::new(database.clone())?
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
    insert_item(&database, "empty-num-movie-1", &movies, "MOVIE", 2019).await?;
    insert_item(&database, "empty-num-movie-2", &movies, "MOVIE", 2021).await?;
    insert_item(&database, "empty-num-series-1", &shows, "SERIES", 2019).await?;
    insert_facet(&database, "empty-num-movie-1", "GENRE", "Drama").await?;
    insert_facet(&database, "empty-num-movie-2", "GENRE", "Sci-Fi").await?;
    insert_facet(&database, "empty-num-movie-1", "STUDIO", "A24").await?;

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
    let admin_token = emby_login(&client, &base_url, "Admin", "correct password").await?;
    Ok(TestServer {
        base_url,
        client,
        admin_token,
        admin_id: admin.id.to_string(),
        _temp_dir: temp_dir,
        _server: server,
    })
}

/// A blank numeric value must answer exactly what the parameter-less request answers.
async fn assert_blank_values_match_absent(
    server: &TestServer,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (baseline_status, baseline) = server.get(path).await?;
    assert_eq!(
        baseline_status,
        reqwest::StatusCode::OK,
        "`{path}` baseline must stay 200"
    );
    assert!(
        baseline != Value::Null,
        "`{path}` baseline must be a JSON body"
    );
    for suffix in BLANK_SUFFIXES {
        let (status, body) = server.get(&format!("{path}{suffix}")).await?;
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "`{path}{suffix}` must treat the blank value as absent"
        );
        assert_eq!(
            body, baseline,
            "`{path}{suffix}` must return the same body as `{path}`"
        );
    }
    Ok(())
}

/// A present but invalid value keeps answering `400`.
async fn assert_invalid_values_are_rejected(
    server: &TestServer,
    path: &str,
    suffixes: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    for suffix in suffixes {
        let (status, _) = server.get(&format!("{path}{suffix}")).await?;
        assert_eq!(
            status,
            reqwest::StatusCode::BAD_REQUEST,
            "`{path}{suffix}` must stay 400"
        );
    }
    Ok(())
}

/// Endpoints that also declare the camelCase aliases must treat a blank alias like a missing key.
async fn assert_blank_aliases_match_absent(
    server: &TestServer,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (baseline_status, baseline) = server.get(path).await?;
    assert_eq!(
        baseline_status,
        reqwest::StatusCode::OK,
        "`{path}` baseline must stay 200"
    );
    for suffix in BLANK_ALIAS_SUFFIXES {
        let (status, body) = server.get(&format!("{path}{suffix}")).await?;
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "`{path}{suffix}` must treat the blank alias as absent"
        );
        assert_eq!(
            body, baseline,
            "`{path}{suffix}` must return the same body as `{path}`"
        );
    }
    assert_invalid_values_are_rejected(server, path, &INVALID_ALIAS_SUFFIXES).await
}

#[tokio::test]
async fn emby_facets_treat_blank_limit_and_start_index_as_absent()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    for endpoint in ["/Genres", "/Years", "/Studios"] {
        // The capture recorded the blank `Limit=` on the `/emby`-prefixed path; the root mount
        // serves the same tree, so both must behave identically.
        for prefix in ["", "/emby"] {
            let path = format!("{prefix}{endpoint}");
            assert_blank_values_match_absent(&server, &path).await?;
            assert_invalid_values_are_rejected(&server, &path, &INVALID_SUFFIXES).await?;
            assert_blank_aliases_match_absent(&server, &path).await?;
        }
    }
    // A non-empty value still works and is not silently dropped.
    let (status, body) = server.get("/emby/Genres?Limit=1").await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["Items"].as_array().map(Vec::len), Some(1));
    Ok(())
}

#[tokio::test]
async fn emby_item_lists_treat_blank_paging_parameters_as_absent()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    let admin_id = server.admin_id.clone();
    let endpoints = [
        "/Items".to_owned(),
        format!("/Users/{admin_id}/Items"),
        format!("/Users/{admin_id}/Items/Resume"),
        format!("/Users/{admin_id}/Items/Latest"),
        format!("/Users/{admin_id}/Items/NextUp"),
        format!("/Users/{admin_id}/FavoriteItems"),
        // `Shows/*` shares `EmbyItemsQuery` with the item lists.
        "/Shows/NextUp".to_owned(),
        "/Shows/empty-num-series-1/Seasons".to_owned(),
        "/Shows/empty-num-series-1/Episodes".to_owned(),
    ];
    for endpoint in &endpoints {
        for prefix in ["", "/emby"] {
            let path = format!("{prefix}{endpoint}");
            assert_blank_values_match_absent(&server, &path).await?;
            assert_invalid_values_are_rejected(&server, &path, &INVALID_SUFFIXES).await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn emby_person_folder_and_user_lists_treat_blank_paging_parameters_as_absent()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    for path in ["/Persons", "/Library/MediaFolders"] {
        for prefix in ["", "/emby"] {
            let path = format!("{prefix}{path}");
            assert_blank_values_match_absent(&server, &path).await?;
            assert_invalid_values_are_rejected(&server, &path, &INVALID_SUFFIXES).await?;
            assert_blank_aliases_match_absent(&server, &path).await?;
        }
    }
    // `Users/Query` shares the same paging fields but caps `Limit` at 100.
    for path in ["/Users/Query", "/emby/Users/Query"] {
        assert_blank_values_match_absent(&server, path).await?;
        assert_invalid_values_are_rejected(
            &server,
            path,
            &[
                "?Limit=abc",
                "?Limit=101",
                "?StartIndex=abc",
                "?StartIndex=-1",
            ],
        )
        .await?;
        assert_blank_aliases_match_absent(&server, path).await?;
    }
    Ok(())
}

#[tokio::test]
async fn emby_sessions_treat_blank_active_within_seconds_as_absent()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    // `EmbyTokenQuery` carries `ActiveWithinSeconds` instead of paging fields, so it needs its own
    // suffixes. The 30-day window is the server-side cap for this filter.
    const BLANK: [&str; 3] = [
        "?ActiveWithinSeconds=",
        "?ActiveWithinSeconds=%20%20",
        "?activeWithinSeconds=",
    ];
    const INVALID: [&str; 4] = [
        "?ActiveWithinSeconds=abc",
        "?ActiveWithinSeconds=0",
        "?ActiveWithinSeconds=2592001",
        "?activeWithinSeconds=abc",
    ];
    for path in ["/Sessions", "/emby/Sessions"] {
        let (baseline_status, baseline) = server.get(path).await?;
        assert_eq!(
            baseline_status,
            reqwest::StatusCode::OK,
            "`{path}` baseline must stay 200"
        );
        for suffix in BLANK {
            let (status, body) = server.get(&format!("{path}{suffix}")).await?;
            assert_eq!(
                status,
                reqwest::StatusCode::OK,
                "`{path}{suffix}` must treat the blank value as absent"
            );
            assert_eq!(
                body, baseline,
                "`{path}{suffix}` must return the same body as `{path}`"
            );
        }
        assert_invalid_values_are_rejected(&server, path, &INVALID).await?;
    }
    Ok(())
}
