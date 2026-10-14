mod common;

use std::time::Duration;

use axum::{Json, Router, routing::get};
use common::{TestScraper, TestScraperConfig};
use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        metadata_paths::{MetadataObjectKind, metadata_object_directory},
        scanner::LibraryScanner,
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn emby_public_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
}

const GLOBAL_COLLECTION_LIBRARY_ID: &str = "00000000-0000-7000-8000-000000000538";

async fn tmdb_movie() -> Json<Value> {
    Json(json!({
        "id": 7,
        "title": "Movie A",
        "belongs_to_collection": { "id": 10, "name": "Stub Collection" }
    }))
}

async fn tmdb_collection() -> Json<Value> {
    Json(json!({
        "id": 10,
        "name": "Stub Collection",
        "overview": "Collection overview",
        "parts": [
            { "id": 7, "title": "Movie A", "release_date": "2020-01-01" },
            { "id": 8, "title": "Movie B", "release_date": "2021-01-01" }
        ]
    }))
}

#[tokio::test]
async fn tmdb_collection_refresh_is_idempotent_and_filters_members_by_acl()
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
    let first = libraries
        .create_library("Movies A", LibraryKind::Movie, false)
        .await?;
    let second = libraries
        .create_library("Movies B", LibraryKind::Movie, false)
        .await?;
    let first_root = temp_dir.path().join("MoviesA");
    let second_root = temp_dir.path().join("MoviesB");
    tokio::fs::create_dir_all(&first_root).await?;
    tokio::fs::create_dir_all(&second_root).await?;
    tokio::fs::write(first_root.join("Movie.A.2020.mkv"), b"a").await?;
    tokio::fs::write(second_root.join("Movie.B.2021.mkv"), b"b").await?;
    libraries
        .add_root(first.id, first_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    libraries
        .add_root(second.id, second_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_movie_library(first.id).await?;
    scanner.scan_movie_library(second.id).await?;

    let first_item: String = sqlx::query_scalar("SELECT id FROM media_items WHERE library_id = ?")
        .bind(first.id.to_string())
        .fetch_one(database.pool())
        .await?;
    let second_item: String = sqlx::query_scalar("SELECT id FROM media_items WHERE library_id = ?")
        .bind(second.id.to_string())
        .fetch_one(database.pool())
        .await?;
    sqlx::query("UPDATE media_items SET provider_ids_json = ? WHERE id = ?")
        .bind(r#"{"tmdb":"7"}"#)
        .bind(&first_item)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_items SET provider_ids_json = ? WHERE id = ?")
        .bind(r#"{"tmdb":"8"}"#)
        .bind(&second_item)
        .execute(database.pool())
        .await?;

    let tmdb_app = Router::new()
        .route("/3/movie/7", get(tmdb_movie))
        .route("/3/collection/10", get(tmdb_collection));
    let tmdb_listener = TcpListener::bind("127.0.0.1:0").await?;
    let tmdb_address = tmdb_listener.local_addr()?;
    let tmdb_server = tokio::spawn(async move { axum::serve(tmdb_listener, tmdb_app).await });
    let tmdb = TestScraper::new(TestScraperConfig {
        base_url: format!("http://{tmdb_address}"),
        proxy_url: None,
        api_key: None,
        read_access_token: Some("stub-token".to_owned()),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let config_dir = config.config_dir.clone();
    let state = AppState::ready(config, database.clone(), setup, web_auth, emby_auth)
        .with_scraper(tmdb.provider());
    let app = app_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    let refreshed = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{first_item}/collection/refresh"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(refreshed.status(), reqwest::StatusCode::OK);
    let refresh_body = refreshed.json::<Value>().await?;
    let collection_item_id = refresh_body["collectionItemId"]
        .as_str()
        .ok_or("missing collection item")?
        .to_owned();
    assert_eq!(refresh_body["memberCount"], 1);
    let collection_directory = metadata_object_directory(
        &config_dir,
        MetadataObjectKind::Collection,
        "Stub Collection",
        "tmdb",
        "10",
    )?;
    let collection_snapshot =
        tokio::fs::read_to_string(collection_directory.join("collection.json")).await?;
    let collection_snapshot: Value = serde_json::from_str(&collection_snapshot)?;
    assert_eq!(collection_snapshot["kind"], "collections");
    assert_eq!(collection_snapshot["displayName"], "Stub Collection");
    assert_eq!(collection_snapshot["provider"], "tmdb");
    assert_eq!(collection_snapshot["objectId"], "10");
    assert_eq!(collection_snapshot["memberCount"], 1);

    let refreshed_again = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{first_item}/collection/refresh"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(refreshed_again.status(), reqwest::StatusCode::OK);
    let refreshed_again_body = refreshed_again.json::<Value>().await?;
    assert_eq!(refreshed_again_body["collectionItemId"], collection_item_id);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM collections")
            .fetch_one(database.pool())
            .await?,
        1
    );
    sqlx::query(
        "INSERT INTO collection_items (collection_id, item_id, sort_order)
         SELECT c.id, ?, 1 FROM collections c WHERE c.item_id = ?",
    )
    .bind(&second_item)
    .bind(&collection_item_id)
    .execute(database.pool())
    .await?;

    let admin_collection = client
        .get(format!(
            "{base_url}/api/v1/collections/{collection_item_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(admin_collection.status(), reqwest::StatusCode::OK);
    assert_eq!(
        admin_collection.json::<Value>().await?["items"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );

    let grant = client
        .patch(format!(
            "{base_url}/api/v1/admin/users/{}/libraries/{}",
            viewer.id, first.id
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "canView": true }))
        .send()
        .await?;
    assert_eq!(grant.status(), reqwest::StatusCode::OK);
    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, is_favorite)
         VALUES (?, ?, 1)
         ON CONFLICT(user_id, item_id) DO UPDATE SET is_favorite = 1",
    )
    .bind(viewer.id.to_string())
    .bind(&first_item)
    .execute(database.pool())
    .await?;
    let viewer_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    let viewer_cookie = cookie_pair(viewer_login.headers());
    let collection = client
        .get(format!(
            "{base_url}/api/v1/collections/{collection_item_id}"
        ))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(collection.status(), reqwest::StatusCode::OK);
    let collection_body = collection.json::<Value>().await?;
    assert_eq!(collection_body["collection"]["itemType"], "BOX_SET");
    assert_eq!(collection_body["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(collection_body["items"][0]["id"], first_item);
    assert_eq!(collection_body["items"][0]["userData"]["isFavorite"], true);

    let viewer_emby_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="CollectionTest", Device="Mac", DeviceId="collection-viewer", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    let viewer_token = viewer_emby_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let children = client
        .get(format!("{base_url}/Items/{collection_item_id}/Children"))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(children.status(), reqwest::StatusCode::OK);
    let children_body = children.json::<Value>().await?;
    assert_eq!(children_body["Items"].as_array().map(Vec::len), Some(1));

    sqlx::query(
        "WITH RECURSIVE sequence(value) AS (
             SELECT 1
             UNION ALL
             SELECT value + 1 FROM sequence WHERE value < 5000
         )
         INSERT INTO media_items (
             id, library_id, item_type, title, sort_title,
             identification_status, identity_key, has_available_source
         )
         SELECT printf('bulk-collection-%05d', value), ?, 'MOVIE',
                printf('Bulk Collection %05d', value),
                printf('Bulk Collection %05d', value), 'LOCAL_CONFIRMED',
                printf('bulk-collection:%05d', value), 1
         FROM sequence",
    )
    .bind(first.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO collection_items (collection_id, item_id, sort_order)
         SELECT c.id, mi.id, 1 + CAST(substr(mi.id, -5) AS INTEGER)
         FROM collections c
         JOIN media_items mi ON mi.id LIKE 'bulk-collection-%'
         WHERE c.item_id = ?",
    )
    .bind(&collection_item_id)
    .execute(database.pool())
    .await?;
    let large_page = tokio::time::timeout(Duration::from_secs(1), async {
        client
            .get(format!(
                "{base_url}/Items/{collection_item_id}/Children?StartIndex=1&Limit=1"
            ))
            .header("X-Emby-Token", &viewer_token)
            .send()
            .await
    })
    .await
    .map_err(|_| "collection page loaded every member")??;
    assert_eq!(large_page.status(), reqwest::StatusCode::OK);
    let large_page_body = large_page.json::<Value>().await?;
    assert_eq!(large_page_body["TotalRecordCount"], 5001);
    assert_eq!(large_page_body["Items"].as_array().map(Vec::len), Some(1));
    assert_eq!(large_page_body["Items"][0]["Id"], "bulk-collection-00001");

    server.abort();
    tmdb_server.abort();
    assert_ne!(admin.id, viewer.id);
    Ok(())
}

#[tokio::test]
async fn emby_global_collection_views_children_counts_and_mutations_are_scoped()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let visible_library = libraries
        .create_library("Visible Movies", LibraryKind::Movie, false)
        .await?;
    let hidden_library = libraries
        .create_library("Hidden Movies", LibraryKind::Movie, false)
        .await?;
    let empty_library = libraries
        .create_library("Empty Movies", LibraryKind::Movie, false)
        .await?;
    let isolated_user = UserStore::new(database.clone())?
        .create_user("isolated", "Isolated", "isolated password", false)
        .await?;
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, ?, 1)",
    )
    .bind(viewer.id.to_string())
    .bind(visible_library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, ?, 1)",
    )
    .bind(isolated_user.id.to_string())
    .bind(empty_library.id.to_string())
    .execute(database.pool())
    .await?;

    for (item_id, library_id, title) in [
        (
            "visible-copy-one",
            visible_library.id.to_string(),
            "Movie Zeta",
        ),
        (
            "hidden-copy-one",
            hidden_library.id.to_string(),
            "Movie Zeta",
        ),
        (
            "hidden-copy-two",
            hidden_library.id.to_string(),
            "Movie Alpha",
        ),
        (
            "logical-work-one",
            GLOBAL_COLLECTION_LIBRARY_ID.to_owned(),
            "Movie Zeta",
        ),
        (
            "logical-work-two",
            GLOBAL_COLLECTION_LIBRARY_ID.to_owned(),
            "Movie Alpha",
        ),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title,
                 identification_status, identity_key, has_available_source
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'ONLINE_CONFIRMED', ?, 1)",
        )
        .bind(item_id)
        .bind(library_id)
        .bind(title)
        .bind(title.to_lowercase())
        .bind(format!("collection-test:{item_id}"))
        .execute(database.pool())
        .await?;
    }
    for (work_item_id, movie_provider_id) in [
        ("logical-work-one", "movie-one"),
        ("logical-work-two", "movie-two"),
    ] {
        sqlx::query(
            "INSERT INTO collection_work_items (work_item_id, movie_provider, movie_provider_id)
             VALUES (?, 'tmdb', ?)",
        )
        .bind(work_item_id)
        .bind(movie_provider_id)
        .execute(database.pool())
        .await?;
    }
    for (work_item_id, source_item_id, sort_order) in [
        ("logical-work-one", "visible-copy-one", 0),
        ("logical-work-one", "hidden-copy-one", 1),
        ("logical-work-two", "hidden-copy-two", 2),
    ] {
        sqlx::query(
            "INSERT INTO collection_work_sources (work_item_id, source_item_id, sort_order)
             VALUES (?, ?, ?)",
        )
        .bind(work_item_id)
        .bind(source_item_id)
        .bind(sort_order)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status,
             identity_key, has_available_source
         ) VALUES ('global-collection-item', ?, 'BOX_SET', 'Test Collection',
                   'test collection', 'ONLINE_CONFIRMED', 'collection-test:boxset', 1)",
    )
    .bind(GLOBAL_COLLECTION_LIBRARY_ID)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO collections (id, item_id, library_id, provider, provider_id, title)
         VALUES ('global-collection-row', 'global-collection-item', ?, 'tmdb',
                 'collection-one', 'Test Collection')",
    )
    .bind(GLOBAL_COLLECTION_LIBRARY_ID)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO collection_items (collection_id, item_id, sort_order)
         VALUES ('global-collection-row', 'logical-work-one', 1),
                ('global-collection-row', 'logical-work-two', 2)",
    )
    .execute(database.pool())
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
    let client = reqwest::Client::new();
    let admin_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="CollectionTest", Device="Mac", DeviceId="collection-admin", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_token = admin_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing admin token")?
        .to_owned();
    let viewer_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="CollectionTest", Device="Mac", DeviceId="collection-viewer", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    assert_eq!(viewer_login.status(), reqwest::StatusCode::OK);
    let viewer_token = viewer_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let global_library_public_id = emby_public_id(GLOBAL_COLLECTION_LIBRARY_ID);

    let views = client
        .get(format!("{base_url}/Users/{}/Views", viewer.id))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(views.status(), reqwest::StatusCode::OK);
    let views: Value = views.json().await?;
    let global_view = views["Items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["Id"] == global_library_public_id)
        })
        .ok_or("missing Emby global collection view")?;
    assert_eq!(global_view["Type"], "CollectionFolder");
    assert_eq!(global_view["CollectionType"], "boxsets");
    assert_eq!(global_view["ChildCount"], 1);

    let global_view_detail = client
        .get(format!(
            "{base_url}/Users/{}/Items/{global_library_public_id}",
            viewer.id
        ))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(global_view_detail.status(), reqwest::StatusCode::OK);
    let global_view_detail: Value = global_view_detail.json().await?;
    assert_eq!(global_view_detail["Type"], "CollectionFolder");
    assert_eq!(global_view_detail["ChildCount"], 1);

    let root_items = client
        .get(format!(
            "{base_url}/Users/{}/Items?ParentId={global_library_public_id}&IncludeItemTypes=BoxSet&StartIndex=0&Limit=10",
            viewer.id
        ))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(root_items.status(), reqwest::StatusCode::OK);
    let root_items: Value = root_items.json().await?;
    assert_eq!(root_items["TotalRecordCount"], 1);
    assert_eq!(root_items["StartIndex"], 0);
    assert_eq!(root_items["Items"][0]["Id"], "global-collection-item");
    assert_eq!(root_items["Items"][0]["Type"], "BoxSet");
    assert_eq!(root_items["Items"][0]["ChildCount"], 1);

    let collection_detail = client
        .get(format!(
            "{base_url}/Users/{}/Items/global-collection-item",
            viewer.id
        ))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(collection_detail.status(), reqwest::StatusCode::OK);
    let collection_detail: Value = collection_detail.json().await?;
    assert_eq!(collection_detail["Type"], "BoxSet");
    assert_eq!(collection_detail["ChildCount"], 1);

    let children = client
        .get(format!(
            "{base_url}/Items/global-collection-item/Children?UserId={}&StartIndex=0&Limit=1",
            viewer.id
        ))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(children.status(), reqwest::StatusCode::OK);
    let children: Value = children.json().await?;
    assert_eq!(children["TotalRecordCount"], 1);
    assert_eq!(children["StartIndex"], 0);
    assert_eq!(children["Items"][0]["Id"], "logical-work-one");
    assert_eq!(children["Items"][0]["Type"], "Movie");

    let generic_children = client
        .get(format!(
            "{base_url}/Users/{}/Items?ParentId=global-collection-item&StartIndex=0&Limit=1",
            viewer.id
        ))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(generic_children.status(), reqwest::StatusCode::OK);
    let generic_children: Value = generic_children.json().await?;
    assert_eq!(generic_children["TotalRecordCount"], 1);
    assert_eq!(generic_children["Items"][0]["Id"], "logical-work-one");

    let counts = client
        .get(format!("{base_url}/Items/Counts?UserId={}", viewer.id))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(counts.status(), reqwest::StatusCode::OK);
    let counts: Value = counts.json().await?;
    assert_eq!(counts["MovieCount"], 1);
    assert_eq!(counts["BoxSetCount"], 1);
    assert_eq!(counts["ItemCount"], 2);

    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, is_favorite)
         VALUES (?, 'global-collection-item', 1)",
    )
    .bind(viewer.id.to_string())
    .execute(database.pool())
    .await?;
    let favorite_counts = client
        .get(format!(
            "{base_url}/Items/Counts?UserId={}&IsFavorite=true",
            viewer.id
        ))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(favorite_counts.status(), reqwest::StatusCode::OK);
    let favorite_counts: Value = favorite_counts.json().await?;
    assert_eq!(favorite_counts["BoxSetCount"], 1);
    assert_eq!(favorite_counts["ItemCount"], 1);
    let non_favorite_counts = client
        .get(format!(
            "{base_url}/Items/Counts?UserId={}&IsFavorite=false",
            viewer.id
        ))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(non_favorite_counts.status(), reqwest::StatusCode::OK);
    let non_favorite_counts: Value = non_favorite_counts.json().await?;
    assert_eq!(non_favorite_counts["BoxSetCount"], 0);
    assert_eq!(non_favorite_counts["MovieCount"], 1);
    assert_eq!(non_favorite_counts["ItemCount"], 1);

    let isolated_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="CollectionTest", Device="Mac", DeviceId="collection-isolated", Version="1""#,
        )
        .json(&json!({ "Username": "isolated", "Pw": "isolated password" }))
        .send()
        .await?;
    assert_eq!(isolated_login.status(), reqwest::StatusCode::OK);
    let isolated_token = isolated_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing isolated token")?
        .to_owned();
    let isolated_views = client
        .get(format!("{base_url}/Users/{}/Views", isolated_user.id))
        .header("X-Emby-Token", &isolated_token)
        .send()
        .await?;
    assert_eq!(isolated_views.status(), reqwest::StatusCode::OK);
    let isolated_views: Value = isolated_views.json().await?;
    assert_eq!(isolated_views["Items"].as_array().map(Vec::len), Some(1));
    assert!(isolated_views["Items"].as_array().is_some_and(|items| {
        items
            .iter()
            .all(|item| item["Id"] != global_library_public_id)
    }));

    for mutation_url in [
        format!("{base_url}/Collections/global-collection-item/Items?Ids=hidden-copy-two"),
        format!("{base_url}/Collections/global-collection-item/Items/Delete?Ids=hidden-copy-two"),
        format!("{base_url}/Collections/{global_library_public_id}/Items?Ids=hidden-copy-two"),
    ] {
        let mutation = client
            .post(mutation_url)
            .header("X-Emby-Token", &admin_token)
            .send()
            .await?;
        assert_eq!(mutation.status(), reqwest::StatusCode::NOT_FOUND);
    }

    let admin_children = client
        .get(format!(
            "{base_url}/Items/global-collection-item/Children?StartIndex=0&Limit=1"
        ))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(admin_children.status(), reqwest::StatusCode::OK);
    let admin_children: Value = admin_children.json().await?;
    assert_eq!(admin_children["TotalRecordCount"], 2);
    assert_eq!(admin_children["Items"].as_array().map(Vec::len), Some(1));
    assert_eq!(admin_children["StartIndex"], 0);
    assert_eq!(admin_children["Items"][0]["Id"], "logical-work-one");
    let admin_children_page_two = client
        .get(format!(
            "{base_url}/Items/global-collection-item/Children?StartIndex=1&Limit=1"
        ))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(admin_children_page_two.status(), reqwest::StatusCode::OK);
    let admin_children_page_two: Value = admin_children_page_two.json().await?;
    assert_eq!(admin_children_page_two["TotalRecordCount"], 2);
    assert_eq!(admin_children_page_two["StartIndex"], 1);
    assert_eq!(
        admin_children_page_two["Items"][0]["Id"],
        "logical-work-two"
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn emby_manual_collections_and_library_cover_follow_qmby_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Movie.A.2020.mkv"), b"a").await?;
    tokio::fs::write(root.join("Movie.B.2021.mkv"), b"b").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY id",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(item_ids.len(), 2);

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
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="QmbyContract", Device="test", DeviceId="qmby-contract", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing Emby token")?
        .to_owned();
    let first_id = emby_public_id(&item_ids[0]);
    let second_id = emby_public_id(&item_ids[1]);
    let created = client
        .post(format!("{base_url}/Collections"))
        .query(&[("Name", "Qmby Collection"), ("Ids", first_id.as_str())])
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::OK);
    let collection = created.json::<Value>().await?;
    assert_eq!(collection["Name"], "Qmby Collection");
    assert_eq!(collection["Type"], "BoxSet");
    let collection_id = collection["Id"].as_str().ok_or("missing collection id")?;

    let members = client
        .get(format!("{base_url}/Items"))
        .query(&[("ParentId", collection_id), ("Limit", "1000")])
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(members.status(), reqwest::StatusCode::OK);
    assert_eq!(members.json::<Value>().await?["TotalRecordCount"], 1);

    let added = client
        .post(format!("{base_url}/Collections/{collection_id}/Items"))
        .query(&[("Ids", second_id.as_str())])
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(added.status(), reqwest::StatusCode::NO_CONTENT);
    let removed = client
        .post(format!(
            "{base_url}/Collections/{collection_id}/Items/Delete"
        ))
        .query(&[("Ids", first_id.as_str())])
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(removed.status(), reqwest::StatusCode::NO_CONTENT);

    let cover_bytes = {
        use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
        let mut bytes = Vec::new();
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, Rgba([1, 2, 3, 255])))
            .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)?;
        bytes
    };
    let uploaded = client
        .post(format!(
            "{base_url}/Items/{}/Images/Primary",
            emby_public_id(&library.id.to_string())
        ))
        .header("X-Emby-Token", &token)
        .header("Content-Type", "image/png")
        .body(cover_bytes)
        .send()
        .await?;
    assert_eq!(uploaded.status(), reqwest::StatusCode::NO_CONTENT);
    let cover = client
        .get(format!(
            "{base_url}/Items/{}/Images/Primary",
            emby_public_id(&library.id.to_string())
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(cover.status(), reqwest::StatusCode::OK);
    assert_eq!(cover.headers()["content-type"], "image/png");

    server.abort();
    Ok(())
}

#[tokio::test]
async fn global_collections_are_visible_only_through_accessible_source_libraries()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8099".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let isolated = UserStore::new(database.clone())?
        .create_user("isolated", "Isolated", "isolated password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let visible_library = libraries
        .create_library("Visible Movies", LibraryKind::Movie, false)
        .await?;
    let hidden_library = libraries
        .create_library("Hidden Movies", LibraryKind::Movie, false)
        .await?;
    let empty_library = libraries
        .create_library("Empty Movies", LibraryKind::Movie, false)
        .await?;

    for (item_id, library_id, movie_id, year) in [
        (
            "viewer-visible-movie",
            visible_library.id.to_string(),
            "viewer-movie-one",
            2020,
        ),
        (
            "viewer-hidden-copy",
            hidden_library.id.to_string(),
            "viewer-movie-one",
            2020,
        ),
        (
            "viewer-hidden-movie",
            hidden_library.id.to_string(),
            "viewer-movie-two",
            2021,
        ),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, production_year,
                 identification_status, has_available_source
             ) VALUES (?, ?, 'MOVIE', ?, ?, ?, 'ONLINE_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(library_id)
        .bind(item_id)
        .bind(item_id.to_lowercase())
        .bind(year)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO collection_item_references (
                 item_id, movie_provider, movie_provider_id, collection_provider,
                 collection_provider_id, collection_title, sort_order
             ) VALUES (?, 'tmdb', ?, 'tmdb', 'viewer-series', 'Viewer Series', ?)",
        )
        .bind(item_id)
        .bind(movie_id)
        .bind(year)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO media_sources (id, item_id, source_kind, container, is_default)
         VALUES ('viewer-visible-version', 'viewer-visible-movie', 'LOCAL_FILE', 'mkv', 1),
                ('viewer-hidden-version', 'viewer-hidden-copy', 'LOCAL_FILE', 'mkv', 1)",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO item_images (id, item_id, image_type, image_index, local_path)
         VALUES ('viewer-visible-poster', 'viewer-visible-movie', 'POSTER', 0, '/visible.jpg'),
                ('viewer-hidden-poster', 'viewer-hidden-movie', 'POSTER', 0, '/hidden.jpg')",
    )
    .execute(database.pool())
    .await?;

    let state = AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    );
    let app = app_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    for (user_id, library_id) in [
        (viewer.id, visible_library.id),
        (isolated.id, empty_library.id),
    ] {
        let grant = client
            .patch(format!(
                "{base_url}/api/v1/admin/users/{user_id}/libraries/{library_id}"
            ))
            .header(COOKIE, &admin_cookie)
            .header("x-csrf-token", &admin_csrf)
            .json(&json!({ "canView": true }))
            .send()
            .await?;
        assert_eq!(grant.status(), reqwest::StatusCode::OK);
    }

    let admin_libraries = client
        .get(format!("{base_url}/api/v1/libraries"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(admin_libraries.status(), reqwest::StatusCode::OK);
    let admin_libraries: Value = admin_libraries.json().await?;
    let collection_library = admin_libraries["libraries"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["name"] == "合集"))
        .ok_or("admin did not see the visible collections library")?;
    let collection_library_id = collection_library["id"]
        .as_str()
        .ok_or("missing global collection library id")?
        .to_owned();
    assert!(
        admin_libraries["libraries"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["name"] == "Hidden Movies"))
    );

    let viewer_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    let viewer_cookie = cookie_pair(viewer_login.headers());
    let viewer_csrf = cookie_value(viewer_login.headers(), "lux_csrf");
    let viewer_libraries = client
        .get(format!("{base_url}/api/v1/libraries"))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(viewer_libraries.status(), reqwest::StatusCode::OK);
    let viewer_libraries: Value = viewer_libraries.json().await?;
    let viewer_library_names = viewer_libraries["libraries"]
        .as_array()
        .ok_or("missing library list")?
        .iter()
        .filter_map(|library| library["name"].as_str())
        .collect::<Vec<_>>();
    assert!(viewer_library_names.contains(&"Visible Movies"));
    assert!(viewer_library_names.contains(&"合集"));
    assert!(!viewer_library_names.contains(&"Hidden Movies"));

    let global_page = client
        .get(format!(
            "{base_url}/api/v1/libraries/{collection_library_id}/items?page=1&pageSize=10"
        ))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(global_page.status(), reqwest::StatusCode::OK);
    let global_page: Value = global_page.json().await?;
    assert_eq!(global_page["total"], 1);
    assert_eq!(global_page["items"][0]["title"], "Viewer Series");
    assert_eq!(global_page["items"][0]["memberCount"], 1);
    assert_eq!(
        global_page["items"][0]["posterItemId"],
        "viewer-visible-movie"
    );

    let collection_item_id = global_page["items"][0]["id"]
        .as_str()
        .ok_or("missing collection item id")?;
    let collection_detail = client
        .get(format!(
            "{base_url}/api/v1/collections/{collection_item_id}?page=1&pageSize=10"
        ))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(collection_detail.status(), reqwest::StatusCode::OK);
    let collection_detail: Value = collection_detail.json().await?;
    assert_eq!(collection_detail["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        collection_detail["items"][0]["sources"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        collection_detail["items"][0]["sources"][0]["itemId"],
        "viewer-visible-movie"
    );
    assert_eq!(
        collection_detail["items"][0]["sources"][0]["versions"][0]["id"],
        "viewer-visible-version"
    );
    let work_item_id = collection_detail["items"][0]["id"]
        .as_str()
        .ok_or("missing logical work item id")?;

    let select_source = client
        .put(format!(
            "{base_url}/api/v1/collections/{collection_item_id}/items/{work_item_id}/source"
        ))
        .header(COOKIE, &viewer_cookie)
        .header("x-csrf-token", &viewer_csrf)
        .json(&json!({
            "sourceItemId": "viewer-visible-movie",
            "mediaSourceId": "viewer-visible-version"
        }))
        .send()
        .await?;
    assert_eq!(select_source.status(), reqwest::StatusCode::NO_CONTENT);

    let hidden_source = client
        .put(format!(
            "{base_url}/api/v1/collections/{collection_item_id}/items/{work_item_id}/source"
        ))
        .header(COOKIE, &viewer_cookie)
        .header("x-csrf-token", &viewer_csrf)
        .json(&json!({ "sourceItemId": "viewer-hidden-copy" }))
        .send()
        .await?;
    assert_eq!(hidden_source.status(), reqwest::StatusCode::NOT_FOUND);

    let mismatched_version = client
        .put(format!(
            "{base_url}/api/v1/collections/{collection_item_id}/items/{work_item_id}/source"
        ))
        .header(COOKIE, &viewer_cookie)
        .header("x-csrf-token", &viewer_csrf)
        .json(&json!({
            "sourceItemId": "viewer-visible-movie",
            "mediaSourceId": "viewer-hidden-version"
        }))
        .send()
        .await?;
    assert_eq!(mismatched_version.status(), reqwest::StatusCode::NOT_FOUND);

    let selected_collection_detail = client
        .get(format!(
            "{base_url}/api/v1/collections/{collection_item_id}?page=1&pageSize=10"
        ))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(selected_collection_detail.status(), reqwest::StatusCode::OK);
    let selected_collection_detail: Value = selected_collection_detail.json().await?;
    assert_eq!(
        selected_collection_detail["items"][0]["preferredSourceItemId"],
        "viewer-visible-movie"
    );
    assert_eq!(
        selected_collection_detail["items"][0]["preferredMediaSourceId"],
        "viewer-visible-version"
    );

    let movie_library_page = client
        .get(format!(
            "{base_url}/api/v1/libraries/{}/items?page=1&pageSize=10",
            visible_library.id
        ))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(movie_library_page.status(), reqwest::StatusCode::OK);
    let movie_library_page: Value = movie_library_page.json().await?;
    assert_eq!(movie_library_page["total"], 1);
    assert_eq!(movie_library_page["items"][0]["id"], "viewer-visible-movie");

    let isolated_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "isolated", "password": "isolated password" }))
        .send()
        .await?;
    let isolated_cookie = cookie_pair(isolated_login.headers());
    let isolated_libraries = client
        .get(format!("{base_url}/api/v1/libraries"))
        .header(COOKIE, &isolated_cookie)
        .send()
        .await?;
    assert_eq!(isolated_libraries.status(), reqwest::StatusCode::OK);
    let isolated_libraries: Value = isolated_libraries.json().await?;
    assert!(
        !isolated_libraries["libraries"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["name"] == "合集"))
    );

    server.abort();
    assert_ne!(admin.id, viewer.id);
    Ok(())
}

fn cookie_pair(headers: &reqwest::header::HeaderMap) -> String {
    format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(headers, "lux_session"),
        cookie_value(headers, "lux_csrf")
    )
}

fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> String {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            value
                .strip_prefix(&format!("{name}="))
                .and_then(|value| value.split(';').next())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}
