use std::collections::HashMap;

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
    device_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base_url}/emby/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            format!(
                r#"Emby Client="EnhancementTest", Device="Mac", DeviceId="{device_id}", Version="1""#
            ),
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

#[allow(clippy::too_many_arguments)]
async fn insert_item(
    database: &Database,
    id: &str,
    library_id: &str,
    item_type: &str,
    parent_id: Option<&str>,
    series_id: Option<&str>,
    season_number: Option<i64>,
    episode_number: Option<i64>,
    title: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, parent_id, series_id, season_number, episode_number,
             title, sort_title, identification_status, identity_key, has_available_source
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', ?, 1)",
    )
    .bind(id)
    .bind(library_id)
    .bind(item_type)
    .bind(parent_id)
    .bind(series_id)
    .bind(season_number)
    .bind(episode_number)
    .bind(title)
    .bind(title.to_ascii_lowercase())
    .bind(format!("emby-enh:{id}"))
    .execute(database.pool())
    .await
    .map(|_| ())
}

#[tokio::test]
async fn emby_scoped_search_respects_parent_id_library_series_season_folder_and_acl()
-> Result<(), Box<dyn std::error::Error>> {
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

    let libraries = LibraryService::new(database.clone());
    let movie_lib_a = libraries
        .create_library("Movies A", LibraryKind::Movie, false)
        .await?;
    let movie_lib_b = libraries
        .create_library("Movies B", LibraryKind::Movie, false)
        .await?;
    let series_lib = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let home_videos_lib = libraries
        .create_library("Home Videos", LibraryKind::HomeVideos, false)
        .await?;

    // Grant viewer access only to Movies A and Shows (not Movies B or Home Videos).
    sqlx::query("UPDATE users SET library_access_mode = 'SELECTED' WHERE id = ?")
        .bind(viewer.id.to_string())
        .execute(database.pool())
        .await?;
    for allowed_lib in [movie_lib_a.id, series_lib.id] {
        sqlx::query(
            "INSERT INTO user_library_access (user_id, library_id, can_view) VALUES (?, ?, 1)",
        )
        .bind(viewer.id.to_string())
        .bind(allowed_lib.to_string())
        .execute(database.pool())
        .await?;
    }

    let lib_a_id = movie_lib_a.id.to_string();
    let lib_b_id = movie_lib_b.id.to_string();
    let series_lib_id = series_lib.id.to_string();
    let hv_lib_id = home_videos_lib.id.to_string();

    insert_item(
        &database,
        "movie-a-1",
        &lib_a_id,
        "MOVIE",
        None,
        None,
        None,
        None,
        "Alpha Signal",
    )
    .await?;
    insert_item(
        &database,
        "movie-a-2",
        &lib_a_id,
        "MOVIE",
        None,
        None,
        None,
        None,
        "Shared Signal",
    )
    .await?;
    insert_item(
        &database,
        "movie-b-1",
        &lib_b_id,
        "MOVIE",
        None,
        None,
        None,
        None,
        "Bravo Signal",
    )
    .await?;
    insert_item(
        &database,
        "movie-b-2",
        &lib_b_id,
        "MOVIE",
        None,
        None,
        None,
        None,
        "Shared Signal",
    )
    .await?;

    // Two series with matching episodes to prove series and season ParentId scoping.
    insert_item(
        &database,
        "series-alpha",
        &series_lib_id,
        "SERIES",
        None,
        None,
        None,
        None,
        "Show Alpha",
    )
    .await?;
    insert_item(
        &database,
        "season-alpha-1",
        &series_lib_id,
        "SEASON",
        Some("series-alpha"),
        Some("series-alpha"),
        Some(1),
        None,
        "Season 1",
    )
    .await?;
    insert_item(
        &database,
        "season-alpha-2",
        &series_lib_id,
        "SEASON",
        Some("series-alpha"),
        Some("series-alpha"),
        Some(2),
        None,
        "Season 2",
    )
    .await?;
    insert_item(
        &database,
        "ep-alpha-s1e1",
        &series_lib_id,
        "EPISODE",
        Some("season-alpha-1"),
        Some("series-alpha"),
        Some(1),
        Some(1),
        "Signal Pilot",
    )
    .await?;
    insert_item(
        &database,
        "ep-alpha-s1e2",
        &series_lib_id,
        "EPISODE",
        Some("season-alpha-1"),
        Some("series-alpha"),
        Some(1),
        Some(2),
        "Signal Finale",
    )
    .await?;
    insert_item(
        &database,
        "ep-alpha-s2e1",
        &series_lib_id,
        "EPISODE",
        Some("season-alpha-2"),
        Some("series-alpha"),
        Some(2),
        Some(1),
        "Signal Return",
    )
    .await?;
    insert_item(
        &database,
        "series-beta",
        &series_lib_id,
        "SERIES",
        None,
        None,
        None,
        None,
        "Show Beta",
    )
    .await?;
    insert_item(
        &database,
        "season-beta-1",
        &series_lib_id,
        "SEASON",
        Some("series-beta"),
        Some("series-beta"),
        Some(1),
        None,
        "Season 1",
    )
    .await?;
    insert_item(
        &database,
        "ep-beta-s1e1",
        &series_lib_id,
        "EPISODE",
        Some("season-beta-1"),
        Some("series-beta"),
        Some(1),
        Some(1),
        "Signal Other",
    )
    .await?;

    // Nested folder hierarchy inside HomeVideos.
    insert_item(
        &database,
        "folder-trips",
        &hv_lib_id,
        "FOLDER",
        None,
        None,
        None,
        None,
        "Trips",
    )
    .await?;
    insert_item(
        &database,
        "folder-2024",
        &hv_lib_id,
        "FOLDER",
        Some("folder-trips"),
        None,
        None,
        None,
        "2024",
    )
    .await?;
    insert_item(
        &database,
        "video-trip-1",
        &hv_lib_id,
        "VIDEO",
        Some("folder-2024"),
        None,
        None,
        None,
        "Signal Trip",
    )
    .await?;
    insert_item(
        &database,
        "folder-other",
        &hv_lib_id,
        "FOLDER",
        None,
        None,
        None,
        None,
        "Other",
    )
    .await?;
    insert_item(
        &database,
        "video-other-1",
        &hv_lib_id,
        "VIDEO",
        Some("folder-other"),
        None,
        None,
        None,
        "Signal Unrelated",
    )
    .await?;

    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder().no_proxy().build()?;

    let admin_token = emby_login(&client, &base_url, "Admin", "correct password", "dev-a").await?;
    let viewer_token = emby_login(&client, &base_url, "viewer", "viewer password", "dev-v").await?;

    let lib_a_public = emby_public_id(&lib_a_id);
    let lib_b_public = emby_public_id(&lib_b_id);

    // 1. Unscoped search preserves full-library behavior (4 matching movies across lib A + lib B).
    let unscoped = client
        .get(format!("{base_url}/emby/Items"))
        .header("X-Emby-Token", &admin_token)
        .query(&[("SearchTerm", "Signal"), ("Recursive", "true")])
        .send()
        .await?;
    assert_eq!(unscoped.status(), reqwest::StatusCode::OK);
    let unscoped_body: Value = unscoped.json().await?;
    assert_eq!(unscoped_body["TotalRecordCount"], 4);

    // 2. Scoped to Movies A via /emby/Items, /Users/{id}/Items, and camelCase query keys.
    for (path, parent_key, term_key) in [
        ("/emby/Items".to_owned(), "ParentId", "SearchTerm"),
        (
            format!("/Users/{}/Items", admin.id),
            "ParentId",
            "SearchTerm",
        ),
        (
            format!("/emby/Users/{}/Items", admin.id),
            "parentId",
            "searchTerm",
        ),
    ] {
        let resp = client
            .get(format!("{base_url}{path}"))
            .header("X-Emby-Token", &admin_token)
            .query(&[
                (parent_key, lib_a_public.as_str()),
                (term_key, "Signal"),
                ("Recursive", "true"),
            ])
            .send()
            .await?;
        assert_eq!(resp.status(), reqwest::StatusCode::OK, "path={path}");
        let body: Value = resp.json().await?;
        assert_eq!(body["TotalRecordCount"], 2, "path={path}");
        let ids: Vec<&str> = body["Items"]
            .as_array()
            .expect("Items array")
            .iter()
            .filter_map(|item| item["Id"].as_str())
            .collect();
        assert_eq!(ids, vec!["movie-a-1", "movie-a-2"], "path={path}");
    }

    // 3. /Search/Hints also scopes by ParentId.
    let hints = client
        .get(format!("{base_url}/Search/Hints"))
        .header("X-Emby-Token", &admin_token)
        .query(&[
            ("ParentId", lib_a_public.as_str()),
            ("SearchTerm", "Signal"),
        ])
        .send()
        .await?;
    assert_eq!(hints.status(), reqwest::StatusCode::OK);
    let hints_body: Value = hints.json().await?;
    assert_eq!(hints_body["TotalRecordCount"], 2);
    let hint_ids: Vec<&str> = hints_body["SearchHints"]
        .as_array()
        .expect("SearchHints array")
        .iter()
        .filter_map(|item| item["Id"].as_str())
        .collect();
    assert_eq!(hint_ids, vec!["movie-a-1", "movie-a-2"]);

    // 4. ACL enforcement: viewer cannot search inside unauthorized Movies B or unknown ParentId.
    let forbidden = client
        .get(format!("{base_url}/emby/Items"))
        .header("X-Emby-Token", &viewer_token)
        .query(&[
            ("ParentId", lib_b_public.as_str()),
            ("SearchTerm", "Signal"),
        ])
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    let missing_parent = client
        .get(format!("{base_url}/emby/Items"))
        .header("X-Emby-Token", &viewer_token)
        .query(&[
            ("ParentId", "00000000-0000-0000-0000-000000000000"),
            ("SearchTerm", "Signal"),
        ])
        .send()
        .await?;
    assert_eq!(missing_parent.status(), reqwest::StatusCode::NOT_FOUND);

    // 5. Series ParentId + IncludeItemTypes=Episode returns only that series's episodes.
    let series_eps = client
        .get(format!("{base_url}/emby/Items"))
        .header("X-Emby-Token", &admin_token)
        .query(&[
            ("ParentId", "series-alpha"),
            ("IncludeItemTypes", "Episode"),
            ("SearchTerm", "Signal"),
            ("Recursive", "true"),
        ])
        .send()
        .await?;
    assert_eq!(series_eps.status(), reqwest::StatusCode::OK);
    let series_eps_body: Value = series_eps.json().await?;
    assert_eq!(series_eps_body["TotalRecordCount"], 3);
    let ep_ids: Vec<&str> = series_eps_body["Items"]
        .as_array()
        .expect("Items array")
        .iter()
        .filter_map(|item| item["Id"].as_str())
        .collect();
    assert_eq!(
        ep_ids,
        vec!["ep-alpha-s1e2", "ep-alpha-s1e1", "ep-alpha-s2e1"]
    );

    // Pagination keeps accurate TotalRecordCount under ParentId.
    let paged_eps = client
        .get(format!("{base_url}/emby/Items"))
        .header("X-Emby-Token", &admin_token)
        .query(&[
            ("ParentId", "series-alpha"),
            ("IncludeItemTypes", "Episode"),
            ("SearchTerm", "Signal"),
            ("StartIndex", "1"),
            ("Limit", "1"),
        ])
        .send()
        .await?;
    assert_eq!(paged_eps.status(), reqwest::StatusCode::OK);
    let paged_eps_body: Value = paged_eps.json().await?;
    assert_eq!(paged_eps_body["TotalRecordCount"], 3);
    assert_eq!(paged_eps_body["Items"].as_array().map(Vec::len), Some(1));

    // 6. Season ParentId scopes strictly to that season's episodes even without IncludeItemTypes.
    let season_eps = client
        .get(format!("{base_url}/emby/Items"))
        .header("X-Emby-Token", &admin_token)
        .query(&[("ParentId", "season-alpha-1"), ("SearchTerm", "Signal")])
        .send()
        .await?;
    assert_eq!(season_eps.status(), reqwest::StatusCode::OK);
    let season_eps_body: Value = season_eps.json().await?;
    assert_eq!(season_eps_body["TotalRecordCount"], 2);
    let season_ep_ids: Vec<&str> = season_eps_body["Items"]
        .as_array()
        .expect("Items array")
        .iter()
        .filter_map(|item| item["Id"].as_str())
        .collect();
    assert_eq!(season_ep_ids, vec!["ep-alpha-s1e2", "ep-alpha-s1e1"]);

    // 7. Folder ParentId walks nested subdirectories recursively inside HomeVideos.
    let folder_videos = client
        .get(format!("{base_url}/emby/Items"))
        .header("X-Emby-Token", &admin_token)
        .query(&[("ParentId", "folder-trips"), ("SearchTerm", "Signal")])
        .send()
        .await?;
    assert_eq!(folder_videos.status(), reqwest::StatusCode::OK);
    let folder_body: Value = folder_videos.json().await?;
    assert_eq!(folder_body["TotalRecordCount"], 1);
    assert_eq!(folder_body["Items"][0]["Id"], "video-trip-1");

    // 8. Viewer without access to HomeVideos gets 404 when scoping by a folder inside HomeVideos.
    let denied_folder = client
        .get(format!("{base_url}/emby/Items"))
        .header("X-Emby-Token", &viewer_token)
        .query(&[("ParentId", "folder-trips"), ("SearchTerm", "Signal")])
        .send()
        .await?;
    assert_eq!(denied_folder.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn emby_item_ancestors_returns_parent_chain_and_enforces_acl()
-> Result<(), Box<dyn std::error::Error>> {
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

    let libraries = LibraryService::new(database.clone());
    let movie_lib = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let series_lib = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let home_videos_lib = libraries
        .create_library("Home Videos", LibraryKind::HomeVideos, false)
        .await?;

    // Grant viewer access only to Movies.
    sqlx::query("UPDATE users SET library_access_mode = 'SELECTED' WHERE id = ?")
        .bind(viewer.id.to_string())
        .execute(database.pool())
        .await?;
    sqlx::query("INSERT INTO user_library_access (user_id, library_id, can_view) VALUES (?, ?, 1)")
        .bind(viewer.id.to_string())
        .bind(movie_lib.id.to_string())
        .execute(database.pool())
        .await?;

    let movie_lib_id = movie_lib.id.to_string();
    let series_lib_id = series_lib.id.to_string();
    let hv_lib_id = home_videos_lib.id.to_string();

    insert_item(
        &database,
        "anc-movie",
        &movie_lib_id,
        "MOVIE",
        None,
        None,
        None,
        None,
        "Inception",
    )
    .await?;
    insert_item(
        &database,
        "anc-series",
        &series_lib_id,
        "SERIES",
        None,
        None,
        None,
        None,
        "Andor",
    )
    .await?;
    insert_item(
        &database,
        "anc-season",
        &series_lib_id,
        "SEASON",
        Some("anc-series"),
        Some("anc-series"),
        Some(1),
        None,
        "Season 1",
    )
    .await?;
    insert_item(
        &database,
        "anc-episode",
        &series_lib_id,
        "EPISODE",
        Some("anc-season"),
        Some("anc-series"),
        Some(1),
        Some(1),
        "Kassa",
    )
    .await?;
    insert_item(
        &database,
        "anc-folder-trips",
        &hv_lib_id,
        "FOLDER",
        None,
        None,
        None,
        None,
        "Trips",
    )
    .await?;
    // Simulate an intermediate directory with no direct playable child (has_available_source = 0).
    sqlx::query("UPDATE media_items SET has_available_source = 0 WHERE id = 'anc-folder-trips'")
        .execute(database.pool())
        .await?;
    insert_item(
        &database,
        "anc-folder-2024",
        &hv_lib_id,
        "FOLDER",
        Some("anc-folder-trips"),
        None,
        None,
        None,
        "2024",
    )
    .await?;
    insert_item(
        &database,
        "anc-video",
        &hv_lib_id,
        "VIDEO",
        Some("anc-folder-2024"),
        None,
        None,
        None,
        "Family Trip",
    )
    .await?;

    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder().no_proxy().build()?;

    let admin_token = emby_login(&client, &base_url, "Admin", "correct password", "anc-a").await?;
    let viewer_token = emby_login(&client, &base_url, "viewer", "viewer password", "anc-v").await?;

    let movie_lib_public = emby_public_id(&movie_lib_id);
    let series_lib_public = emby_public_id(&series_lib_id);
    let hv_lib_public = emby_public_id(&hv_lib_id);

    // Unauthenticated request -> 401.
    let unauth = client
        .get(format!("{base_url}/emby/Items/anc-movie/Ancestors"))
        .send()
        .await?;
    assert_eq!(unauth.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Fetch /Views to cross-check CollectionFolder node shape.
    let views: Value = client
        .get(format!("{base_url}/Users/{}/Views", admin.id))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?
        .json()
        .await?;
    let view_by_id: HashMap<String, Value> = views["Items"]
        .as_array()
        .expect("Views items")
        .iter()
        .filter_map(|item| Some((item["Id"].as_str()?.to_owned(), item.clone())))
        .collect();

    // Movie -> [CollectionFolder(Movies)] on both /Items and /emby/Items with api_key / header.
    for (path, use_query_key) in [
        ("/Items/anc-movie/Ancestors", false),
        ("/emby/Items/anc-movie/Ancestors", true),
    ] {
        let req = client.get(format!("{base_url}{path}"));
        let req = if use_query_key {
            req.query(&[
                ("api_key", admin_token.as_str()),
                ("userId", &admin.id.to_string()),
            ])
        } else {
            req.header("X-Emby-Token", &admin_token)
                .query(&[("UserId", &admin.id.to_string())])
        };
        let resp = req.send().await?;
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: Value = resp.json().await?;
        let arr = body.as_array().expect("Ancestors JSON array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["Id"], movie_lib_public);
        assert_eq!(arr[0]["Name"], "Movies");
        assert_eq!(arr[0]["Type"], "CollectionFolder");
        assert_eq!(arr[0]["CollectionType"], "movies");
        assert_eq!(arr[0]["Id"], view_by_id[&movie_lib_public]["Id"]);
        assert_eq!(arr[0]["Type"], view_by_id[&movie_lib_public]["Type"]);
    }

    // Series -> [CollectionFolder(Shows)].
    let series_anc: Value = client
        .get(format!("{base_url}/emby/Items/anc-series/Ancestors"))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?
        .json()
        .await?;
    let series_arr = series_anc.as_array().expect("series ancestors");
    assert_eq!(series_arr.len(), 1);
    assert_eq!(series_arr[0]["Id"], series_lib_public);
    assert_eq!(series_arr[0]["Type"], "CollectionFolder");
    assert_eq!(series_arr[0]["CollectionType"], "tvshows");

    // Season -> [Series, CollectionFolder(Shows)].
    let season_anc: Value = client
        .get(format!("{base_url}/emby/Items/anc-season/Ancestors"))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?
        .json()
        .await?;
    let season_arr = season_anc.as_array().expect("season ancestors");
    assert_eq!(season_arr.len(), 2);
    assert_eq!(season_arr[0]["Id"], "anc-series");
    assert_eq!(season_arr[0]["Type"], "Series");
    assert_eq!(season_arr[1]["Id"], series_lib_public);
    assert_eq!(season_arr[1]["Type"], "CollectionFolder");

    // Episode -> [Season, Series, CollectionFolder(Shows)].
    let ep_anc: Value = client
        .get(format!("{base_url}/emby/Items/anc-episode/Ancestors"))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?
        .json()
        .await?;
    let ep_arr = ep_anc.as_array().expect("episode ancestors");
    assert_eq!(ep_arr.len(), 3);
    assert_eq!(ep_arr[0]["Id"], "anc-season");
    assert_eq!(ep_arr[0]["Type"], "Season");
    assert_eq!(ep_arr[1]["Id"], "anc-series");
    assert_eq!(ep_arr[1]["Type"], "Series");
    assert_eq!(ep_arr[2]["Id"], series_lib_public);
    assert_eq!(ep_arr[2]["Type"], "CollectionFolder");

    // Nested HomeVideos Video -> [Folder("2024"), Folder("Trips"), CollectionFolder("Home Videos")].
    let video_anc: Value = client
        .get(format!("{base_url}/emby/Items/anc-video/Ancestors"))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?
        .json()
        .await?;
    let video_arr = video_anc.as_array().expect("video ancestors");
    assert_eq!(video_arr.len(), 3);
    assert_eq!(video_arr[0]["Id"], "anc-folder-2024");
    assert_eq!(video_arr[0]["Type"], "Folder");
    assert_eq!(video_arr[1]["Id"], "anc-folder-trips");
    assert_eq!(video_arr[1]["Type"], "Folder");
    assert_eq!(video_arr[2]["Id"], hv_lib_public);
    assert_eq!(video_arr[2]["Type"], "CollectionFolder");
    assert_eq!(video_arr[2]["CollectionType"], "homevideos");

    // Library itself -> [] for authorized user, 404 for unauthorized user.
    let lib_anc = client
        .get(format!(
            "{base_url}/emby/Items/{movie_lib_public}/Ancestors"
        ))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(lib_anc.status(), reqwest::StatusCode::OK);
    assert_eq!(lib_anc.json::<Value>().await?, json!([]));

    let denied_lib_anc = client
        .get(format!(
            "{base_url}/emby/Items/{series_lib_public}/Ancestors"
        ))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(denied_lib_anc.status(), reqwest::StatusCode::NOT_FOUND);

    // ACL enforcement: viewer cannot read ancestors of items in unauthorized libraries -> 404.
    for denied_id in ["anc-series", "anc-season", "anc-episode", "anc-video"] {
        let resp = client
            .get(format!("{base_url}/emby/Items/{denied_id}/Ancestors"))
            .header("X-Emby-Token", &viewer_token)
            .send()
            .await?;
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NOT_FOUND,
            "denied_id={denied_id}"
        );
    }

    // Viewer attempting to impersonate admin via userId is rejected with 403.
    let viewer_as_admin = client
        .get(format!("{base_url}/emby/Items/anc-episode/Ancestors"))
        .header("X-Emby-Token", &viewer_token)
        .query(&[("userId", &admin.id.to_string())])
        .send()
        .await?;
    assert_eq!(viewer_as_admin.status(), reqwest::StatusCode::FORBIDDEN);

    // Admin specifying an unknown target userId gets 404.
    let unknown_target_user = client
        .get(format!("{base_url}/emby/Items/anc-movie/Ancestors"))
        .header("X-Emby-Token", &admin_token)
        .query(&[("userId", "00000000-0000-0000-0000-000000000000")])
        .send()
        .await?;
    assert_eq!(unknown_target_user.status(), reqwest::StatusCode::NOT_FOUND);

    // Viewer reading ancestors of an authorized movie with explicit userId succeeds.
    let viewer_movie = client
        .get(format!("{base_url}/emby/Items/anc-movie/Ancestors"))
        .header("X-Emby-Token", &viewer_token)
        .query(&[("userId", &viewer.id.to_string())])
        .send()
        .await?;
    assert_eq!(viewer_movie.status(), reqwest::StatusCode::OK);
    assert_eq!(
        viewer_movie.json::<Value>().await?[0]["Id"],
        movie_lib_public
    );

    // Unknown item ID -> 404.
    let unknown = client
        .get(format!("{base_url}/emby/Items/does-not-exist/Ancestors"))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn emby_virtual_folders_include_accurate_item_count_without_n_plus_one()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let _admin = setup.complete("Admin", "Admin", "correct password").await?;

    let libraries = LibraryService::new(database.clone());
    let movie_lib = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let series_lib = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let mixed_lib = libraries
        .create_library("Mixed", LibraryKind::Mixed, false)
        .await?;
    let hv_lib = libraries
        .create_library("Home Videos", LibraryKind::HomeVideos, false)
        .await?;
    let empty_lib = libraries
        .create_library("Empty Movies", LibraryKind::Movie, false)
        .await?;

    let movie_lib_id = movie_lib.id.to_string();
    let series_lib_id = series_lib.id.to_string();
    let mixed_lib_id = mixed_lib.id.to_string();
    let hv_lib_id = hv_lib.id.to_string();

    // Movies: 2 active + 1 soft-deleted + 1 merged => ItemCount = 2.
    insert_item(
        &database,
        "vf-movie-1",
        &movie_lib_id,
        "MOVIE",
        None,
        None,
        None,
        None,
        "Movie One",
    )
    .await?;
    insert_item(
        &database,
        "vf-movie-2",
        &movie_lib_id,
        "MOVIE",
        None,
        None,
        None,
        None,
        "Movie Two",
    )
    .await?;
    insert_item(
        &database,
        "vf-movie-removed",
        &movie_lib_id,
        "MOVIE",
        None,
        None,
        None,
        None,
        "Movie Removed",
    )
    .await?;
    sqlx::query("UPDATE media_items SET removed_at = 1 WHERE id = 'vf-movie-removed'")
        .execute(database.pool())
        .await?;
    insert_item(
        &database,
        "vf-movie-merged",
        &movie_lib_id,
        "MOVIE",
        None,
        None,
        None,
        None,
        "Movie Merged",
    )
    .await?;
    sqlx::query(
        "UPDATE media_items SET merged_into_item_id = 'vf-movie-1' WHERE id = 'vf-movie-merged'",
    )
    .execute(database.pool())
    .await?;

    // Shows: 2 active series (with season + episode children) + 1 soft-deleted series => ItemCount = 2.
    insert_item(
        &database,
        "vf-series-1",
        &series_lib_id,
        "SERIES",
        None,
        None,
        None,
        None,
        "Show One",
    )
    .await?;
    insert_item(
        &database,
        "vf-season-1",
        &series_lib_id,
        "SEASON",
        Some("vf-series-1"),
        Some("vf-series-1"),
        Some(1),
        None,
        "Season 1",
    )
    .await?;
    insert_item(
        &database,
        "vf-ep-1",
        &series_lib_id,
        "EPISODE",
        Some("vf-season-1"),
        Some("vf-series-1"),
        Some(1),
        Some(1),
        "Episode 1",
    )
    .await?;
    insert_item(
        &database,
        "vf-series-2",
        &series_lib_id,
        "SERIES",
        None,
        None,
        None,
        None,
        "Show Two",
    )
    .await?;
    insert_item(
        &database,
        "vf-series-removed",
        &series_lib_id,
        "SERIES",
        None,
        None,
        None,
        None,
        "Show Removed",
    )
    .await?;
    sqlx::query("UPDATE media_items SET removed_at = 1 WHERE id = 'vf-series-removed'")
        .execute(database.pool())
        .await?;

    // Mixed: 1 movie + 1 series (+ episode) => ItemCount = 2.
    insert_item(
        &database,
        "vf-mixed-movie",
        &mixed_lib_id,
        "MOVIE",
        None,
        None,
        None,
        None,
        "Mixed Movie",
    )
    .await?;
    insert_item(
        &database,
        "vf-mixed-series",
        &mixed_lib_id,
        "SERIES",
        None,
        None,
        None,
        None,
        "Mixed Series",
    )
    .await?;
    insert_item(
        &database,
        "vf-mixed-ep",
        &mixed_lib_id,
        "EPISODE",
        Some("vf-mixed-series"),
        Some("vf-mixed-series"),
        Some(1),
        Some(1),
        "Mixed Ep",
    )
    .await?;

    // HomeVideos: 1 folder + 2 videos => ItemCount = 2.
    insert_item(
        &database,
        "vf-hv-folder",
        &hv_lib_id,
        "FOLDER",
        None,
        None,
        None,
        None,
        "Clips",
    )
    .await?;
    insert_item(
        &database,
        "vf-hv-video-1",
        &hv_lib_id,
        "VIDEO",
        None,
        None,
        None,
        None,
        "Root Clip",
    )
    .await?;
    insert_item(
        &database,
        "vf-hv-video-2",
        &hv_lib_id,
        "VIDEO",
        Some("vf-hv-folder"),
        None,
        None,
        None,
        "Nested Clip",
    )
    .await?;

    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder().no_proxy().build()?;

    let admin_token = emby_login(&client, &base_url, "Admin", "correct password", "vf-a").await?;

    let resp = client
        .get(format!("{base_url}/emby/Library/VirtualFolders"))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let folders: Vec<Value> = resp.json().await?;
    assert_eq!(folders.len(), 5);

    let by_name: HashMap<String, Value> = folders
        .into_iter()
        .filter_map(|folder| Some((folder["Name"].as_str()?.to_owned(), folder)))
        .collect();

    let expected = [
        ("Movies", &movie_lib_id, 2_i64, "Movie,Series"),
        ("Shows", &series_lib_id, 2_i64, "Movie,Series"),
        ("Mixed", &mixed_lib_id, 2_i64, "Movie,Series"),
        ("Home Videos", &hv_lib_id, 2_i64, "Video"),
        (
            "Empty Movies",
            &empty_lib.id.to_string(),
            0_i64,
            "Movie,Series",
        ),
    ];

    for (name, internal_id, expected_count, include_types) in expected {
        let folder = &by_name[name];
        let expected_item_id = emby_public_id(internal_id);
        assert_eq!(folder["ItemId"], expected_item_id, "library={name}");
        assert_eq!(folder["ItemCount"], expected_count, "library={name}");
        assert!(folder["Locations"].is_array(), "library={name}");
        assert!(folder["LibraryOptions"].is_object(), "library={name}");
        assert_eq!(folder["RefreshStatus"], "Idle", "library={name}");

        // Verify exact parity with GET /emby/Items?ParentId=<ItemId>&Recursive=true&IncludeItemTypes=...
        let items_resp: Value = client
            .get(format!("{base_url}/emby/Items"))
            .header("X-Emby-Token", &admin_token)
            .query(&[
                ("ParentId", expected_item_id.as_str()),
                ("Recursive", "true"),
                ("IncludeItemTypes", include_types),
            ])
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(
            folder["ItemCount"], items_resp["TotalRecordCount"],
            "ItemCount must match TotalRecordCount for {name}"
        );
    }

    // Add 5 more libraries and verify the root route (/Library/VirtualFolders) reports all 10
    // with accurate zero counts for the new empty libraries.
    for idx in 1..=5 {
        libraries
            .create_library(&format!("Extra Lib {idx}"), LibraryKind::Movie, false)
            .await?;
    }
    let resp_ten = client
        .get(format!("{base_url}/Library/VirtualFolders"))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(resp_ten.status(), reqwest::StatusCode::OK);
    let folders_ten: Vec<Value> = resp_ten.json().await?;
    assert_eq!(folders_ten.len(), 10);
    for idx in 1..=5 {
        let name = format!("Extra Lib {idx}");
        let folder = folders_ten
            .iter()
            .find(|entry| entry["Name"] == name)
            .expect("extra virtual folder");
        assert_eq!(folder["ItemCount"], 0);
    }

    server.abort();
    database.close().await;
    Ok(())
}
