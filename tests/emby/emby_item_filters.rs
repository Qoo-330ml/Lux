//! Emby `Filters` compatibility for item lists (LUX-886).
//!
//! Standard Emby accepts `IsResumable` and `IsFavorite` inside the comma-separated `Filters`
//! parameter. Lux used to read only `IsPlayed`/`IsUnplayed`, so `Filters=IsResumable` silently
//! returned the full list. These regressions pin the filter semantics on both `/emby`-prefixed and
//! root paths, including per-user scoping, pagination/totals and the combination rules.

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

const TICKS_PER_SECOND: i64 = 10_000_000;
const MOVIE_RUNTIME_SECONDS: i64 = 7_200;

struct TestServer {
    base_url: String,
    client: reqwest::Client,
    admin_token: String,
    viewer_token: String,
    admin_id: String,
    viewer_id: String,
    library_id: String,
    database: Database,
    _temp_dir: tempfile::TempDir,
    _server: tokio::task::JoinHandle<Result<(), std::io::Error>>,
}

fn emby_public_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
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
            r#"Emby Client="ItemFiltersTest", Device="Mac", DeviceId="filters-device", Version="1""#,
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

async fn spawn_server() -> Result<TestServer, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder().no_proxy().build()?;
    let admin_token = emby_login(&client, &base_url, "Admin", "correct password").await?;
    let viewer_token = emby_login(&client, &base_url, "viewer", "viewer password").await?;
    Ok(TestServer {
        base_url,
        client,
        admin_token,
        viewer_token,
        admin_id: admin.id.to_string(),
        viewer_id: viewer.id.to_string(),
        library_id: library.id.to_string(),
        database,
        _temp_dir: temp_dir,
        _server: server,
    })
}

async fn insert_movie(
    server: &TestServer,
    item_id: &str,
    title: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO media_items
         (id, library_id, item_type, title, sort_title, identification_status,
          identity_key, has_available_source, runtime_ticks)
         VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?, 1, ?)",
    )
    .bind(item_id)
    .bind(&server.library_id)
    .bind(title)
    .bind(title)
    .bind(format!("emby-filters:{item_id}"))
    .bind(MOVIE_RUNTIME_SECONDS * TICKS_PER_SECOND)
    .execute(server.database.pool())
    .await?;
    Ok(())
}

/// Upserts one `user_item_state` row; `position_seconds` is stored in Emby ticks.
async fn set_user_state(
    server: &TestServer,
    user_id: &str,
    item_id: &str,
    position_seconds: i64,
    played: bool,
    favorite: bool,
    hidden_from_resume: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO user_item_state
         (user_id, item_id, position_ticks, is_played, is_favorite, play_count,
          hide_from_resume, last_played_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(user_id, item_id) DO UPDATE SET
             position_ticks = excluded.position_ticks,
             is_played = excluded.is_played,
             is_favorite = excluded.is_favorite,
             play_count = excluded.play_count,
             hide_from_resume = excluded.hide_from_resume,
             last_played_at = excluded.last_played_at",
    )
    .bind(user_id)
    .bind(item_id)
    .bind(position_seconds * TICKS_PER_SECOND)
    .bind(i64::from(played))
    .bind(i64::from(favorite))
    .bind(i64::from(played))
    .bind(i64::from(hidden_from_resume))
    .bind(1_700_000_000_i64)
    .execute(server.database.pool())
    .await?;
    Ok(())
}

async fn get_items(
    server: &TestServer,
    token: &str,
    path: String,
    params: &[(&str, &str)],
) -> Result<Value, Box<dyn std::error::Error>> {
    let response = server
        .client
        .get(format!("{}{path}", server.base_url))
        .query(params)
        .header("X-Emby-Token", token)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    Ok(response.json().await?)
}

fn item_ids(body: &Value) -> Vec<String> {
    body["Items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["Id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// `Filters=IsResumable` is a strict subset of `IsUnplayed`: it keeps only the current user's
/// unfinished progress and reuses the continue-watching decision (minimum progress, played
/// percentage and `HideFromResume`), so items with no progress, finished items, items above the
/// watched percentage, items below the minimum progress and hidden items never appear.
#[tokio::test]
async fn emby_filters_is_resumable_matches_continue_watching_and_scopes_users()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    for (item_id, title) in [
        ("resume-watched", "Resume Watched"),
        ("resume-played", "Resume Played"),
        ("resume-fresh", "Resume Fresh"),
        ("resume-almost-done", "Resume Almost Done"),
        ("resume-barely-started", "Resume Barely Started"),
        ("resume-hidden", "Resume Hidden"),
        ("resume-viewer-only", "Resume Viewer Only"),
    ] {
        insert_movie(&server, item_id, title).await?;
    }
    // The admin has progress on one item only; the rest pin one rejection reason each.
    set_user_state(
        &server,
        &server.admin_id,
        "resume-watched",
        1_800,
        false,
        false,
        false,
    )
    .await?;
    set_user_state(
        &server,
        &server.admin_id,
        "resume-played",
        1_800,
        true,
        false,
        false,
    )
    .await?;
    // 7_000s of 7_200s is above the default 95% watched percentage.
    set_user_state(
        &server,
        &server.admin_id,
        "resume-almost-done",
        7_000,
        false,
        false,
        false,
    )
    .await?;
    // 60s is below the default 120s minimum resume progress.
    set_user_state(
        &server,
        &server.admin_id,
        "resume-barely-started",
        60,
        false,
        false,
        false,
    )
    .await?;
    set_user_state(
        &server,
        &server.admin_id,
        "resume-hidden",
        1_800,
        false,
        false,
        true,
    )
    .await?;
    // Only the viewer has progress on this item.
    set_user_state(
        &server,
        &server.viewer_id,
        "resume-viewer-only",
        1_800,
        false,
        false,
        false,
    )
    .await?;
    let library_public_id = emby_public_id(&server.library_id);

    let admin_resumable = get_items(
        &server,
        &server.admin_token,
        format!("/emby/Users/{}/Items", server.admin_id),
        &[
            ("ParentId", library_public_id.as_str()),
            ("Recursive", "true"),
            ("IncludeItemTypes", "Movie"),
            ("Filters", "IsResumable"),
            ("Limit", "50"),
        ],
    )
    .await?;
    assert_eq!(admin_resumable["TotalRecordCount"], 1);
    assert_eq!(item_ids(&admin_resumable), vec!["resume-watched"]);
    assert_eq!(
        admin_resumable["Items"][0]["UserData"]["PlaybackPositionTicks"],
        1_800 * TICKS_PER_SECOND
    );
    assert_eq!(admin_resumable["Items"][0]["UserData"]["Played"], false);

    // Root path, lower-case value and the other user's token: only the viewer's own progress is
    // visible, so the admin's progress never leaks into this response.
    let viewer_resumable = get_items(
        &server,
        &server.viewer_token,
        format!("/Users/{}/Items", server.viewer_id),
        &[
            ("ParentId", library_public_id.as_str()),
            ("Recursive", "true"),
            ("IncludeItemTypes", "Movie"),
            ("Filters", "isresumable"),
            ("Limit", "50"),
        ],
    )
    .await?;
    assert_eq!(viewer_resumable["TotalRecordCount"], 1);
    assert_eq!(item_ids(&viewer_resumable), vec!["resume-viewer-only"]);

    // Baseline: `IsUnplayed` alone keeps every unplayed item, so `IsResumable` is a strict subset.
    let admin_unplayed = get_items(
        &server,
        &server.admin_token,
        format!("/emby/Users/{}/Items", server.admin_id),
        &[
            ("ParentId", library_public_id.as_str()),
            ("Recursive", "true"),
            ("IncludeItemTypes", "Movie"),
            ("Filters", "IsUnplayed"),
            ("Limit", "50"),
        ],
    )
    .await?;
    assert_eq!(admin_unplayed["TotalRecordCount"], 6);
    assert!(!item_ids(&admin_unplayed).contains(&"resume-played".to_owned()));

    // Combination rule: `IsResumable` implies "not watched", so pairing it with `IsPlayed` is a
    // contradiction and both filters are applied (AND) instead of one silently overriding the
    // other - the result is empty rather than "all played items".
    let contradictory = get_items(
        &server,
        &server.admin_token,
        format!("/emby/Users/{}/Items", server.admin_id),
        &[
            ("ParentId", library_public_id.as_str()),
            ("Recursive", "true"),
            ("IncludeItemTypes", "Movie"),
            ("Filters", "IsResumable,IsPlayed"),
            ("Limit", "50"),
        ],
    )
    .await?;
    assert_eq!(contradictory["TotalRecordCount"], 0);
    assert!(item_ids(&contradictory).is_empty());

    // The compatible pairing keeps the same rows, and combination values are case-insensitive.
    let consistent = get_items(
        &server,
        &server.admin_token,
        format!("/emby/Users/{}/Items", server.admin_id),
        &[
            ("ParentId", library_public_id.as_str()),
            ("Recursive", "true"),
            ("IncludeItemTypes", "Movie"),
            ("Filters", "ISRESUMABLE,IsUnplayed"),
            ("Limit", "50"),
        ],
    )
    .await?;
    assert_eq!(consistent["TotalRecordCount"], 1);
    assert_eq!(item_ids(&consistent), vec!["resume-watched"]);

    Ok(())
}

/// Resumable pages keep the server-side paging and the filtered total, and `Filters=IsFavorite`
/// behaves like `IsFavorite=true` on the root `/Items` path.
#[tokio::test]
async fn emby_filters_is_resumable_paginates_and_is_favorite_matches_the_query_parameter()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    for (item_id, title) in [
        ("resume-a", "Resume A"),
        ("resume-b", "Resume B"),
        ("resume-c", "Resume C"),
        ("favorite-fresh", "Favorite Fresh"),
        ("plain-fresh", "Plain Fresh"),
    ] {
        insert_movie(&server, item_id, title).await?;
    }
    for item_id in ["resume-a", "resume-b", "resume-c"] {
        set_user_state(&server, &server.admin_id, item_id, 1_800, false, false, false).await?;
    }
    // A favorite without any playback progress, and a favorite with progress.
    set_user_state(
        &server,
        &server.admin_id,
        "resume-b",
        1_800,
        false,
        true,
        false,
    )
    .await?;
    set_user_state(
        &server,
        &server.admin_id,
        "favorite-fresh",
        0,
        false,
        true,
        false,
    )
    .await?;

    let first_page = get_items(
        &server,
        &server.admin_token,
        "/emby/Items".to_owned(),
        &[
            ("UserId", server.admin_id.as_str()),
            ("Recursive", "true"),
            ("IncludeItemTypes", "Movie"),
            ("Filters", "IsResumable"),
            ("Limit", "2"),
            ("StartIndex", "0"),
        ],
    )
    .await?;
    assert_eq!(first_page["TotalRecordCount"], 3);
    assert_eq!(item_ids(&first_page), vec!["resume-a", "resume-b"]);

    let second_page = get_items(
        &server,
        &server.admin_token,
        "/emby/Items".to_owned(),
        &[
            ("UserId", server.admin_id.as_str()),
            ("Recursive", "true"),
            ("IncludeItemTypes", "Movie"),
            ("Filters", "IsResumable"),
            ("Limit", "2"),
            ("StartIndex", "2"),
        ],
    )
    .await?;
    // The total is the filtered total, not the page size.
    assert_eq!(second_page["TotalRecordCount"], 3);
    assert_eq!(item_ids(&second_page), vec!["resume-c"]);

    let library_public_id = emby_public_id(&server.library_id);
    let favorite_filter = get_items(
        &server,
        &server.admin_token,
        format!("/Users/{}/Items", server.admin_id),
        &[
            ("ParentId", library_public_id.as_str()),
            ("Recursive", "true"),
            ("IncludeItemTypes", "Movie"),
            ("Filters", "IsFavorite"),
            ("Limit", "50"),
        ],
    )
    .await?;
    assert_eq!(favorite_filter["TotalRecordCount"], 2);
    assert_eq!(
        item_ids(&favorite_filter),
        vec!["favorite-fresh", "resume-b"]
    );

    let favorite_parameter = get_items(
        &server,
        &server.admin_token,
        format!("/Users/{}/Items", server.admin_id),
        &[
            ("ParentId", library_public_id.as_str()),
            ("Recursive", "true"),
            ("IncludeItemTypes", "Movie"),
            ("IsFavorite", "true"),
            ("Limit", "50"),
        ],
    )
    .await?;
    assert_eq!(
        favorite_parameter["TotalRecordCount"],
        favorite_filter["TotalRecordCount"]
    );
    assert_eq!(item_ids(&favorite_parameter), item_ids(&favorite_filter));

    // Combined values are intersected: only the favorite item with progress remains.
    let favorite_and_resumable = get_items(
        &server,
        &server.admin_token,
        format!("/Users/{}/Items", server.admin_id),
        &[
            ("ParentId", library_public_id.as_str()),
            ("Recursive", "true"),
            ("IncludeItemTypes", "Movie"),
            ("Filters", "IsFavorite,IsResumable"),
            ("Limit", "50"),
        ],
    )
    .await?;
    assert_eq!(favorite_and_resumable["TotalRecordCount"], 1);
    assert_eq!(item_ids(&favorite_and_resumable), vec!["resume-b"]);

    Ok(())
}
