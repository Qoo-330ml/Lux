use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

struct Harness {
    client: reqwest::Client,
    base_url: String,
    admin_cookie: String,
    viewer_cookie: String,
    admin_emby_token: String,
    viewer_emby_token: String,
    viewer_id: String,
    database: Database,
    server: tokio::task::JoinHandle<Result<(), std::io::Error>>,
    _temp_dir: tempfile::TempDir,
}

async fn insert_item(
    database: &Database,
    id: &str,
    library_id: &str,
    item_type: &str,
    title: &str,
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
    .bind(title)
    .bind(title.to_lowercase())
    .bind(format!("similar:{id}"))
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

async fn insert_genres(
    database: &Database,
    item_id: &str,
    genres: &[&str],
) -> Result<(), sqlx::Error> {
    for genre in genres {
        sqlx::query(
            "INSERT INTO media_item_facets (item_id, kind, name_key, name) VALUES (?, 'GENRE', ?, ?)",
        )
        .bind(item_id)
        .bind(genre.to_lowercase())
        .bind(genre)
        .execute(database.pool())
        .await?;
    }
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
            r#"Emby Client="LuxSimilarTest", Device="Mac", DeviceId="lux-similar-device", Version="1""#,
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

/// Seeds the same movie/series neighbourhood the Emby adapter tests use: one visible movie
/// library, one visible series library and a movie library the viewer cannot access.
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

    // Movie source: Drama, Sci-Fi, Thriller. The shared-genre counts deliberately disagree with
    // the alphabetical order, so an assertion on ids proves ranking rather than title sorting.
    insert_item(
        &database,
        "sim-movie-source",
        &movies,
        "MOVIE",
        "Source Movie",
    )
    .await?;
    insert_genres(
        &database,
        "sim-movie-source",
        &["Drama", "Sci-Fi", "Thriller"],
    )
    .await?;
    insert_item(
        &database,
        "sim-movie-alpha",
        &movies,
        "MOVIE",
        "Alpha Similar",
    )
    .await?;
    insert_genres(
        &database,
        "sim-movie-alpha",
        &["Drama", "Sci-Fi", "Thriller"],
    )
    .await?;
    insert_item(
        &database,
        "sim-movie-bravo",
        &movies,
        "MOVIE",
        "Bravo Similar",
    )
    .await?;
    insert_genres(&database, "sim-movie-bravo", &["Drama"]).await?;
    insert_item(
        &database,
        "sim-movie-charlie",
        &movies,
        "MOVIE",
        "Charlie Similar",
    )
    .await?;
    insert_genres(&database, "sim-movie-charlie", &["Drama", "Sci-Fi"]).await?;
    insert_item(
        &database,
        "sim-movie-other",
        &movies,
        "MOVIE",
        "Other Genre",
    )
    .await?;
    insert_genres(&database, "sim-movie-other", &["Comedy"]).await?;
    insert_item(
        &database,
        "sim-movie-no-genre",
        &movies,
        "MOVIE",
        "No Genre",
    )
    .await?;
    // A same-type candidate with the same genres in a library the viewer cannot see.
    insert_item(
        &database,
        "sim-movie-hidden",
        &hidden,
        "MOVIE",
        "Hidden Similar",
    )
    .await?;
    insert_genres(
        &database,
        "sim-movie-hidden",
        &["Drama", "Sci-Fi", "Thriller"],
    )
    .await?;

    // Series source: Mystery, Fantasy.
    insert_item(
        &database,
        "sim-series-source",
        &shows,
        "SERIES",
        "Source Series",
    )
    .await?;
    insert_genres(&database, "sim-series-source", &["Mystery", "Fantasy"]).await?;
    insert_item(
        &database,
        "sim-series-alpha",
        &shows,
        "SERIES",
        "Similar Series",
    )
    .await?;
    insert_genres(&database, "sim-series-alpha", &["Mystery", "Fantasy"]).await?;
    insert_item(
        &database,
        "sim-series-drama",
        &shows,
        "SERIES",
        "Drama Series",
    )
    .await?;
    insert_genres(
        &database,
        "sim-series-drama",
        &["Drama", "Sci-Fi", "Thriller"],
    )
    .await?;

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
    let admin_emby_token = emby_login(&client, &base_url, "admin", "admin password").await?;
    let viewer_emby_token = emby_login(&client, &base_url, "viewer", "viewer password").await?;

    Ok(Harness {
        client,
        base_url,
        admin_cookie,
        viewer_cookie,
        admin_emby_token,
        viewer_emby_token,
        viewer_id,
        database: harness_database,
        server,
        _temp_dir: temp_dir,
    })
}

impl Harness {
    async fn lux_similar(
        &self,
        item_id: &str,
        query: &str,
        cookie: &str,
    ) -> Result<(reqwest::StatusCode, Value), Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(format!(
                "{}/api/v1/items/{item_id}/similar{query}",
                self.base_url
            ))
            .header(COOKIE, cookie)
            .send()
            .await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        };
        Ok((status, body))
    }

    async fn emby_similar(
        &self,
        item_id: &str,
        query: &str,
        token: &str,
    ) -> Result<(reqwest::StatusCode, Value), Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(format!(
                "{}/emby/Items/{item_id}/Similar{query}",
                self.base_url
            ))
            .header("X-Emby-Token", token)
            .send()
            .await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        };
        Ok((status, body))
    }
}

fn lux_ids(body: &Value) -> Vec<&str> {
    body["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["id"].as_str())
                .collect()
        })
        .unwrap_or_default()
}

fn emby_ids(body: &Value) -> Vec<&str> {
    body["Items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["Id"].as_str())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn lux_similar_items_match_the_emby_ranking_and_respect_library_acl()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    let (status, body) = harness
        .lux_similar("sim-movie-source", "?limit=20", &harness.viewer_cookie)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    // More shared genres outrank fewer; a title tie-break keeps equal counts stable.
    assert_eq!(
        lux_ids(&body),
        ["sim-movie-alpha", "sim-movie-charlie", "sim-movie-bravo"]
    );
    assert_eq!(
        body["total"], 3,
        "the total is the visible candidate count, not just the page"
    );
    assert_eq!(body["page"], 1);
    assert_eq!(body["pageSize"], 20);
    assert_eq!(body["hasNext"], false);
    // The page is built from the Lux item DTO (camelCase `userData`/`mediaSources`), never from
    // the Emby `Items` projection (`Name`/`Id`/`UserData`).
    let first = &body["items"][0];
    assert_eq!(first["title"], "Alpha Similar");
    assert!(first.get("userData").is_some());
    assert!(first.get("mediaSources").is_some());
    assert!(first.get("Id").is_none());
    assert!(first.get("Name").is_none());
    assert!(first.get("UserData").is_none());

    // The Emby compatibility route is only a protocol adapter over the same service, so the same
    // user and library must see the same ids in the same order.
    let (emby_status, emby_body) = harness
        .emby_similar("sim-movie-source", "?Limit=20", &harness.viewer_emby_token)
        .await?;
    assert_eq!(emby_status, reqwest::StatusCode::OK);
    assert_eq!(lux_ids(&body), emby_ids(&emby_body));
    assert_eq!(emby_body["TotalRecordCount"], 3);
    assert!(!emby_ids(&emby_body).contains(&"sim-movie-hidden"));

    // An administrator sees the third library; both protocols still agree on its ranked position.
    let (status, admin_body) = harness
        .lux_similar("sim-movie-source", "?limit=20", &harness.admin_cookie)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        lux_ids(&admin_body),
        [
            "sim-movie-alpha",
            "sim-movie-hidden",
            "sim-movie-charlie",
            "sim-movie-bravo"
        ]
    );
    assert_eq!(admin_body["total"], 4);
    let (emby_status, admin_emby_body) = harness
        .emby_similar("sim-movie-source", "?Limit=20", &harness.admin_emby_token)
        .await?;
    assert_eq!(emby_status, reqwest::StatusCode::OK);
    assert_eq!(lux_ids(&admin_body), emby_ids(&admin_emby_body));
    assert_eq!(admin_emby_body["TotalRecordCount"], 4);

    // The source item itself, items of another type, a different genre and an ACL-hidden item
    // never enter the Lux page.
    assert!(!lux_ids(&body).contains(&"sim-movie-source"));
    assert!(!lux_ids(&body).contains(&"sim-movie-other"));
    assert!(!lux_ids(&body).contains(&"sim-movie-no-genre"));

    harness.server.abort();
    Ok(())
}

#[tokio::test]
async fn lux_similar_limit_defaults_caps_and_rejects_invalid_values()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    for query in ["?limit=", ""] {
        let (status, body) = harness
            .lux_similar("sim-movie-source", query, &harness.viewer_cookie)
            .await?;
        assert_eq!(status, reqwest::StatusCode::OK, "query {query}");
        assert_eq!(body["pageSize"], 50, "query {query}");
        assert_eq!(body["total"], 3, "query {query}");
    }

    let (status, limited) = harness
        .lux_similar("sim-movie-source", "?limit=1", &harness.viewer_cookie)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(lux_ids(&limited), ["sim-movie-alpha"]);
    assert_eq!(
        limited["total"], 3,
        "limit only trims the page, not the total"
    );
    assert_eq!(
        limited["hasNext"], false,
        "the endpoint has no paging parameter, so it never promises a next page"
    );

    // `total > limit` is the case that would otherwise advertise a next page; the endpoint can
    // only serve the first page of its bounded candidate pool.
    let (status, partial) = harness
        .lux_similar("sim-movie-source", "?limit=2", &harness.viewer_cookie)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(lux_ids(&partial), ["sim-movie-alpha", "sim-movie-charlie"]);
    assert_eq!(partial["total"], 3);
    assert_eq!(partial["pageSize"], 2);
    assert_eq!(partial["hasNext"], false);

    let (status, capped) = harness
        .lux_similar("sim-movie-source", "?limit=1000", &harness.viewer_cookie)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(capped["pageSize"], 1000);
    assert_eq!(capped["total"], 3);
    assert_eq!(capped["hasNext"], false);

    for query in [
        "?limit=0",
        "?limit=-1",
        "?limit=1001",
        "?limit=abc",
        "?limit=1.5",
    ] {
        let (status, _) = harness
            .lux_similar("sim-movie-source", query, &harness.viewer_cookie)
            .await?;
        assert_eq!(
            status,
            reqwest::StatusCode::BAD_REQUEST,
            "query {query} must be rejected"
        );
    }

    harness.server.abort();
    Ok(())
}

#[tokio::test]
async fn lux_similar_excludes_items_blocked_by_content_restrictions()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    // The viewer may watch up to PG-13 and unrated items are not allowed; every relevant item has
    // an index row, and Charlie is the only one above the limit.
    sqlx::query(
        "INSERT INTO user_content_restrictions
         (user_id, rating_system, maximum_rating, allow_unrated)
         VALUES (?, 'MPA', 'PG-13', 0)",
    )
    .bind(&harness.viewer_id)
    .execute(harness.database.pool())
    .await?;
    for (item_id, certification) in [
        ("sim-movie-source", "PG"),
        ("sim-movie-alpha", "PG"),
        ("sim-movie-bravo", "PG"),
        ("sim-movie-charlie", "R"),
    ] {
        insert_rating(&harness.database, item_id, certification).await?;
    }

    let (status, body) = harness
        .lux_similar("sim-movie-source", "?limit=20", &harness.viewer_cookie)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(lux_ids(&body), ["sim-movie-alpha", "sim-movie-bravo"]);
    assert_eq!(body["total"], 2);
    assert!(!lux_ids(&body).contains(&"sim-movie-charlie"));

    // The Emby adapter shares the same restriction predicate and also drops Charlie from its
    // TotalRecordCount.
    let (emby_status, emby_body) = harness
        .emby_similar("sim-movie-source", "?Limit=20", &harness.viewer_emby_token)
        .await?;
    assert_eq!(emby_status, reqwest::StatusCode::OK);
    assert_eq!(emby_ids(&emby_body), ["sim-movie-alpha", "sim-movie-bravo"]);
    assert_eq!(emby_body["TotalRecordCount"], 2);

    harness.server.abort();
    Ok(())
}

#[tokio::test]
async fn lux_similar_answers_404_for_unknown_or_invisible_items_and_401_without_a_session()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    let (status, body) = harness
        .lux_similar("sim-does-not-exist", "?limit=20", &harness.viewer_cookie)
        .await?;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "NOT_FOUND");

    // The item exists but belongs to a library the viewer cannot access: same 404, no leak.
    let (status, _) = harness
        .lux_similar("sim-movie-hidden", "?limit=20", &harness.viewer_cookie)
        .await?;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);

    let unauthenticated = harness
        .client
        .get(format!(
            "{}/api/v1/items/sim-movie-source/similar?limit=20",
            harness.base_url
        ))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);
    let body: Value = unauthenticated.json().await?;
    assert_eq!(body["error"]["code"], "AUTHENTICATION_REQUIRED");

    harness.server.abort();
    Ok(())
}

#[tokio::test]
async fn lux_similar_only_returns_the_same_type_and_handles_an_empty_result()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    let (status, body) = harness
        .lux_similar("sim-series-source", "?limit=20", &harness.viewer_cookie)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(lux_ids(&body), ["sim-series-alpha"]);
    assert_eq!(body["total"], 1);
    assert!(!lux_ids(&body).contains(&"sim-series-drama"));
    assert!(!lux_ids(&body).contains(&"sim-movie-alpha"));

    let (status, empty) = harness
        .lux_similar("sim-movie-no-genre", "?limit=20", &harness.viewer_cookie)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(lux_ids(&empty).is_empty());
    assert_eq!(empty["total"], 0);
    assert_eq!(empty["hasNext"], false);

    harness.server.abort();
    Ok(())
}
