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

struct Harness {
    client: reqwest::Client,
    base_url: String,
    admin_token: String,
    viewer_token: String,
    viewer_id: String,
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

async fn insert_image(
    database: &Database,
    id: &str,
    item_id: &str,
    image_type: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO item_images (id, item_id, image_type, image_index, local_path)
         VALUES (?, ?, ?, 0, ?)",
    )
    .bind(id)
    .bind(item_id)
    .bind(image_type)
    .bind(format!("/nonexistent/{id}.jpg"))
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
            r#"Emby Client="SimilarTest", Device="Mac", DeviceId="similar-device", Version="1""#,
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

/// Seeds one movie library, one series library and a second movie library the viewer cannot see.
///
/// The shared-genre counts are chosen so the ranked order differs from the alphabetical order in
/// some cases and matches it in others; a test can therefore tell "ranked by shared genres" from
/// "sorted by name".
async fn start_server() -> Result<Harness, Box<dyn std::error::Error>> {
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
    let hidden = libraries
        .create_library("Hidden", LibraryKind::Movie, false)
        .await?
        .id
        .to_string();

    // Movie source: Drama, Sci-Fi, Thriller.
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
    // A same-type item in a library the viewer cannot see.
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
    // Same genres as the series source, so it must not leak into the movie ranking.
    insert_item(
        &database,
        "sim-movie-mystery",
        &movies,
        "MOVIE",
        "Mystery Movie",
    )
    .await?;
    insert_genres(&database, "sim-movie-mystery", &["Mystery"]).await?;

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

    insert_image(&database, "sim-alpha-poster", "sim-movie-alpha", "POSTER").await?;
    insert_image(&database, "sim-alpha-fanart", "sim-movie-alpha", "FANART").await?;

    sqlx::query("INSERT INTO user_library_access (user_id, library_id, can_view) VALUES (?, ?, 1)")
        .bind(viewer.id.to_string())
        .bind(&movies)
        .execute(database.pool())
        .await?;
    sqlx::query("INSERT INTO user_library_access (user_id, library_id, can_view) VALUES (?, ?, 1)")
        .bind(viewer.id.to_string())
        .bind(&shows)
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
    Ok(Harness {
        client,
        base_url,
        admin_token,
        viewer_token,
        viewer_id: viewer.id.to_string(),
        server,
        _temp_dir: temp_dir,
    })
}

impl Harness {
    async fn similar(
        &self,
        prefix: &str,
        item_id: &str,
        query: &str,
        token: &str,
    ) -> Result<(reqwest::StatusCode, Value), Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(format!(
                "{}{}/Items/{item_id}/Similar{query}",
                self.base_url,
                prefix.trim_end_matches('/')
            ))
            .header("X-Emby-Token", token)
            .send()
            .await?;
        let status = response.status();
        // The Emby namespace answers some errors with an empty body; keep the status assertion
        // readable instead of failing on JSON decoding first.
        let bytes = response.bytes().await?;
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        };
        Ok((status, body))
    }
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
async fn movie_similar_ranks_shared_genres_excludes_self_and_other_types()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    let query = "?Limit=20&Fields=ProductionYear,CommunityRating&ImageTypeLimit=1\
                 &EnableImageTypes=Primary,Backdrop";
    let (status, body) = harness
        .similar("/emby", "sim-movie-source", query, &harness.admin_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    // More shared genres outrank fewer; a title tie-break keeps equal counts stable.
    assert_eq!(
        names(&body),
        [
            "Alpha Similar",
            "Hidden Similar",
            "Charlie Similar",
            "Bravo Similar"
        ]
    );
    assert_eq!(body["TotalRecordCount"], 4);
    // The source item is never part of its own recommendation list, and neither are items of
    // another type or without a shared genre.
    assert!(!names(&body).contains(&"Source Movie"));
    assert!(!names(&body).contains(&"Similar Series"));
    assert!(!names(&body).contains(&"Other Genre"));
    assert!(!names(&body).contains(&"Drama Mystery Movie"));
    // The list projection keeps user data unless the caller opts out.
    assert!(body["Items"][0].get("UserData").is_some());
    assert_eq!(body["Items"][0]["Id"], "sim-movie-alpha");

    let (status, limited) = harness
        .similar(
            "/emby",
            "sim-movie-source",
            "?Limit=1&Fields=ProductionYear",
            &harness.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&limited), ["Alpha Similar"]);
    assert_eq!(
        limited["TotalRecordCount"], 4,
        "the total reports every similar item, not just the page"
    );

    // The root mount answers exactly like the `/emby` prefix.
    let (status, root) = harness
        .similar(
            "/",
            "sim-movie-source",
            "?Limit=2&Fields=ProductionYear",
            &harness.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&root), ["Alpha Similar", "Hidden Similar"]);
    assert_eq!(root["TotalRecordCount"], 4);

    harness.server.abort();
    Ok(())
}

#[tokio::test]
async fn similar_respects_the_viewers_library_acl() -> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    let (status, body) = harness
        .similar(
            "/emby",
            "sim-movie-source",
            "?Limit=20&Fields=ProductionYear",
            &harness.viewer_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        names(&body),
        ["Alpha Similar", "Charlie Similar", "Bravo Similar"]
    );
    assert_eq!(body["TotalRecordCount"], 3);
    assert!(!names(&body).contains(&"Hidden Similar"));

    // A `UserId` query parameter must not widen the viewer's scope either.
    let query = format!("?Limit=20&UserId={}", harness.viewer_id);
    let (status, body) = harness
        .similar("/emby", "sim-movie-source", &query, &harness.viewer_token)
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["TotalRecordCount"], 3);
    assert!(!names(&body).contains(&"Hidden Similar"));

    harness.server.abort();
    Ok(())
}

#[tokio::test]
async fn series_similar_only_returns_series() -> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    let (status, body) = harness
        .similar(
            "/emby",
            "sim-series-source",
            "?Limit=20&Fields=ProductionYear",
            &harness.viewer_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(names(&body), ["Similar Series"]);
    assert_eq!(body["TotalRecordCount"], 1);
    assert!(!names(&body).contains(&"Drama Series"));
    assert!(!names(&body).contains(&"Mystery Movie"));

    // An item whose genres nobody shares is not an error: it is an empty page.
    let (status, empty) = harness
        .similar(
            "/emby",
            "sim-movie-no-genre",
            "?Limit=20&Fields=ProductionYear",
            &harness.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(names(&empty).is_empty());
    assert_eq!(empty["TotalRecordCount"], 0);

    harness.server.abort();
    Ok(())
}

#[tokio::test]
async fn similar_returns_404_for_unknown_ids_and_401_without_a_token()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    let (status, _) = harness
        .similar(
            "/emby",
            "sim-does-not-exist",
            "?Limit=20",
            &harness.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);

    let unauthenticated = harness
        .client
        .get(format!(
            "{}/emby/Items/sim-movie-source/Similar?Limit=20",
            harness.base_url
        ))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    harness.server.abort();
    Ok(())
}

#[tokio::test]
async fn similar_limit_defaults_and_rejects_values_above_the_page_cap()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    // An empty value is the same as an absent parameter (the query-string behaviour LUX-888
    // requests for the other Emby list endpoints).
    for query in ["?Limit=", "?Limit=0", "?Limit=1000"] {
        let (status, body) = harness
            .similar("/emby", "sim-movie-source", query, &harness.admin_token)
            .await?;
        assert_eq!(status, reqwest::StatusCode::OK, "query {query}");
        assert_eq!(body["TotalRecordCount"], 4, "query {query}");
    }
    for query in ["?Limit=abc", "?Limit=-1", "?Limit=1001"] {
        let response = harness
            .client
            .get(format!(
                "{}/emby/Items/sim-movie-source/Similar{query}",
                harness.base_url
            ))
            .header("X-Emby-Token", &harness.admin_token)
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "query {query}"
        );
    }

    harness.server.abort();
    Ok(())
}

#[tokio::test]
async fn similar_honours_user_data_and_image_type_selection()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = start_server().await?;
    let (status, filtered) = harness
        .similar(
            "/emby",
            "sim-movie-source",
            "?Limit=1&EnableUserData=false&ImageTypeLimit=1&EnableImageTypes=Primary",
            &harness.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    let item = &filtered["Items"][0];
    assert_eq!(item["Name"], "Alpha Similar");
    assert!(item.get("UserData").is_none());
    assert_eq!(item["ImageTags"]["Primary"], "sim-alpha-poster");
    assert!(item["ImageTags"].get("Logo").is_none());
    assert_eq!(item["BackdropImageTags"], json!([]));

    let (status, unrestricted) = harness
        .similar(
            "/emby",
            "sim-movie-source",
            "?Limit=1&Fields=ProductionYear",
            &harness.admin_token,
        )
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        unrestricted["Items"][0]["BackdropImageTags"],
        json!(["sim-alpha-fanart"])
    );

    harness.server.abort();
    Ok(())
}
