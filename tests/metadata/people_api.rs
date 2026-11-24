use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        metadata_paths::{library_item_directory, lux_person_directory, people_directory},
        people::{ActorCredit, PeopleService, PersonMetadata},
        scanner::LibraryScanner,
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, HeaderMap, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::{Duration, sleep};

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

const JPEG_SIGNATURE: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, b'J', b'F', b'I', b'F'];

#[tokio::test]
async fn lux_people_search_and_items_enforce_acl_and_aggregate_series_episodes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = luxd::auth::users::UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let denied = luxd::auth::users::UserStore::new(database.clone())?
        .create_user("denied", "Denied", "denied password", false)
        .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Mixed", LibraryKind::Mixed, false)
        .await?;
    let unrelated_library = LibraryService::new(database.clone())
        .create_library("Unrelated", LibraryKind::Movie, false)
        .await?;
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, ?, 1)",
    )
    .bind(viewer.id.to_string())
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE users SET library_access_mode = 'SELECTED' WHERE id IN (?, ?)")
        .bind(viewer.id.to_string())
        .bind(denied.id.to_string())
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, ?, 1)",
    )
    .bind(denied.id.to_string())
    .bind(unrelated_library.id.to_string())
    .execute(database.pool())
    .await?;

    for (id, item_type, title, parent_id, series_id, premiere_date) in [
        (
            "movie-1",
            "MOVIE",
            "演员甲电影",
            None,
            None,
            Some("2024-01-01"),
        ),
        (
            "series-1",
            "SERIES",
            "演员甲剧集",
            None,
            None,
            Some("2023-01-01"),
        ),
        (
            "season-1",
            "SEASON",
            "第一季",
            Some("series-1"),
            Some("series-1"),
            None,
        ),
        (
            "episode-1",
            "EPISODE",
            "第一集",
            Some("season-1"),
            Some("series-1"),
            Some("2023-01-02"),
        ),
        (
            "episode-2",
            "EPISODE",
            "第二集",
            Some("season-1"),
            None,
            Some("2023-01-03"),
        ),
        (
            "denied-movie",
            "MOVIE",
            "隐藏电影",
            None,
            None,
            Some("2022-01-01"),
        ),
        (
            "unavailable-movie",
            "MOVIE",
            "无源电影",
            None,
            None,
            Some("2021-01-01"),
        ),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, parent_id, series_id,
                premiere_date, identification_status, has_available_source
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(id)
        .bind(library.id.to_string())
        .bind(item_type)
        .bind(title)
        .bind(title.to_lowercase())
        .bind(parent_id)
        .bind(series_id)
        .bind(premiere_date)
        .execute(database.pool())
        .await?;
    }
    sqlx::query("UPDATE media_items SET has_available_source = 0 WHERE id = ?")
        .bind("unavailable-movie")
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_items SET has_available_source = 0 WHERE id = ?")
        .bind("series-1")
        .execute(database.pool())
        .await?;
    PeopleService::new(config.config_dir.clone())
        .with_database(database.clone())
        .persist_item_actors(
            "movie-1",
            "tmdb",
            &[
                ActorCredit {
                    id: "42".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员甲".to_owned(),
                    character: Some("角色甲".to_owned()),
                    order: Some(0),
                    profile_url: None,
                    person: None,
                },
                ActorCredit {
                    id: "43".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "John Actor".to_owned(),
                    character: Some("Supporting Role".to_owned()),
                    order: Some(1),
                    profile_url: None,
                    person: None,
                },
            ],
        )
        .await?;
    for item_id in ["episode-1", "episode-2"] {
        PeopleService::new(config.config_dir.clone())
            .with_database(database.clone())
            .persist_item_actors(
                item_id,
                "tmdb",
                &[ActorCredit {
                    id: "42".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员甲".to_owned(),
                    character: Some("角色甲".to_owned()),
                    order: Some(0),
                    profile_url: None,
                    person: None,
                }],
            )
            .await?;
    }
    PeopleService::new(config.config_dir.clone())
        .with_database(database.clone())
        .persist_item_actors(
            "series-1",
            "tmdb",
            &[ActorCredit {
                id: "44".to_owned(),
                provider: None,
                identities: Vec::new(),
                name: "Series Actor".to_owned(),
                character: Some("Series Role".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;
    for (item_id, title, premiere_date) in [
        ("cross-provider-movie-1", "跨来源电影一", "2020-01-01"),
        ("cross-provider-movie-2", "跨来源电影二", "2019-01-01"),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, premiere_date,
                identification_status, has_available_source
             ) VALUES (?, ?, 'MOVIE', ?, ?, ?, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(library.id.to_string())
        .bind(title)
        .bind(title)
        .bind(premiere_date)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO person_credits (
            item_id, person_id, person_type, person_name, provider, lux_person_id
         ) VALUES
            ('cross-provider-movie-1', '50', 'Actor', '跨来源演员', 'tmdb', 'lux-999999'),
            ('cross-provider-movie-2', 'nm50', 'Actor', '跨来源演员', 'imdb', 'lux-999999')",
    )
    .execute(database.pool())
    .await?;
    PeopleService::new(config.config_dir.clone())
        .with_database(database.clone())
        .persist_item_actors(
            "denied-movie",
            "tmdb",
            &[ActorCredit {
                id: "99".to_owned(),
                provider: None,
                identities: Vec::new(),
                name: "隐藏演员".to_owned(),
                character: None,
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;
    PeopleService::new(config.config_dir.clone())
        .with_database(database.clone())
        .persist_item_actors(
            "unavailable-movie",
            "tmdb",
            &[ActorCredit {
                id: "100".to_owned(),
                provider: None,
                identities: Vec::new(),
                name: "无源演员".to_owned(),
                character: None,
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config, database, setup, web_auth, emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let base_url = format!("http://{address}");
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({"username": "viewer", "password": "viewer password"}))
        .send()
        .await?;
    let cookies = cookie_pair(login.headers());

    let search = client
        .get(format!(
            "{base_url}/api/v1/people?q=演员甲&page=1&pageSize=12"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(search.status(), reqwest::StatusCode::OK);
    let search_body: Value = search.json().await?;
    assert_eq!(search_body["total"], 1);
    assert_eq!(search_body["items"][0]["id"], "42");

    let english_search = client
        .get(format!(
            "{base_url}/api/v1/people?q=John&page=1&pageSize=12"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(english_search.status(), reqwest::StatusCode::OK);
    let english_search_body: Value = english_search.json().await?;
    assert_eq!(english_search_body["total"], 1);
    assert_eq!(english_search_body["items"][0]["name"], "John Actor");

    let series_actor_search = client
        .get(format!(
            "{base_url}/api/v1/people?q=Series%20Actor&page=1&pageSize=12"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(series_actor_search.status(), reqwest::StatusCode::OK);
    assert_eq!(series_actor_search.json::<Value>().await?["total"], 1);

    let series_actor_works = client
        .get(format!(
            "{base_url}/api/v1/people/44/items?page=1&pageSize=12"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(series_actor_works.status(), reqwest::StatusCode::OK);
    let series_actor_works_body: Value = series_actor_works.json().await?;
    assert_eq!(series_actor_works_body["total"], 1);
    assert_eq!(series_actor_works_body["items"][0]["id"], "series-1");

    let cross_provider_search = client
        .get(format!(
            "{base_url}/api/v1/people?q=%E8%B7%A8%E6%9D%A5%E6%BA%90%E6%BC%94%E5%91%98&page=1&pageSize=12"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(cross_provider_search.status(), reqwest::StatusCode::OK);
    let cross_provider_search_body: Value = cross_provider_search.json().await?;
    assert_eq!(cross_provider_search_body["total"], 1);
    assert_eq!(cross_provider_search_body["items"][0]["id"], "50");

    let cross_provider_works = client
        .get(format!(
            "{base_url}/api/v1/people/50/items?page=1&pageSize=12"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(cross_provider_works.status(), reqwest::StatusCode::OK);
    assert_eq!(cross_provider_works.json::<Value>().await?["total"], 2);

    let works = client
        .get(format!(
            "{base_url}/api/v1/people/42/items?page=1&pageSize=1"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(works.status(), reqwest::StatusCode::OK);
    let works_body: Value = works.json().await?;
    assert_eq!(works_body["total"], 2);
    assert_eq!(works_body["items"][0]["id"], "movie-1");

    let works_page_two = client
        .get(format!(
            "{base_url}/api/v1/people/42/items?page=2&pageSize=1"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(works_page_two.status(), reqwest::StatusCode::OK);
    assert_eq!(
        works_page_two.json::<Value>().await?["items"][0]["id"],
        "series-1"
    );

    let denied_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({"username": "denied", "password": "denied password"}))
        .send()
        .await?;
    let denied_cookies = cookie_pair(denied_login.headers());
    let denied_search = client
        .get(format!(
            "{base_url}/api/v1/people?q=隐藏演员&page=1&pageSize=12"
        ))
        .header(COOKIE, &denied_cookies)
        .send()
        .await?;
    assert_eq!(denied_search.status(), reqwest::StatusCode::OK);
    assert_eq!(denied_search.json::<Value>().await?["total"], 0);

    let unavailable_search = client
        .get(format!(
            "{base_url}/api/v1/people?q=无源演员&page=1&pageSize=12"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(unavailable_search.status(), reqwest::StatusCode::OK);
    assert_eq!(unavailable_search.json::<Value>().await?["total"], 0);

    let unavailable_items = client
        .get(format!(
            "{base_url}/api/v1/people/100/items?page=1&pageSize=12"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(unavailable_items.status(), reqwest::StatusCode::OK);
    assert_eq!(unavailable_items.json::<Value>().await?["total"], 0);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn lux_person_favorites_are_user_scoped_and_require_person_acl_and_csrf()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = luxd::auth::users::UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let denied = luxd::auth::users::UserStore::new(database.clone())?
        .create_user("denied", "Denied", "denied password", false)
        .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let unrelated_library = LibraryService::new(database.clone())
        .create_library("Unrelated", LibraryKind::Series, false)
        .await?;
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('item-person-favorite', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, ?, 1)",
    )
    .bind(denied.id.to_string())
    .bind(unrelated_library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, ?, 1)",
    )
    .bind(viewer.id.to_string())
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE users SET library_access_mode = 'SELECTED' WHERE id IN (?, ?)")
        .bind(viewer.id.to_string())
        .bind(denied.id.to_string())
        .execute(database.pool())
        .await?;
    PeopleService::new(config.config_dir.clone())
        .with_database(database.clone())
        .persist_item_actors(
            "item-person-favorite",
            "tmdb",
            &[ActorCredit {
                id: "101".to_owned(),
                provider: None,
                identities: Vec::new(),
                name: "演员甲".to_owned(),
                character: Some("角色甲".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config, database, setup, web_auth, emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let base_url = format!("http://{address}");

    let viewer_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    let viewer_cookie = cookie_pair(viewer_login.headers());
    let viewer_csrf = cookie_value(viewer_login.headers(), "lux_csrf");
    let initial = client
        .get(format!("{base_url}/api/v1/people/101"))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(initial.status(), reqwest::StatusCode::OK);
    assert_eq!(initial.json::<Value>().await?["isFavorite"], false);

    let missing_csrf = client
        .put(format!("{base_url}/api/v1/people/101/favorite"))
        .header(COOKIE, &viewer_cookie)
        .json(&json!({ "favorite": true }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    for _ in 0..2 {
        let favorite = client
            .put(format!("{base_url}/api/v1/people/101/favorite"))
            .header(COOKIE, &viewer_cookie)
            .header("x-csrf-token", &viewer_csrf)
            .json(&json!({ "favorite": true }))
            .send()
            .await?;
        assert_eq!(favorite.status(), reqwest::StatusCode::NO_CONTENT);
    }
    let favorited = client
        .get(format!("{base_url}/api/v1/people/101"))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(favorited.status(), reqwest::StatusCode::OK);
    assert_eq!(favorited.json::<Value>().await?["isFavorite"], true);

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_person = client
        .get(format!("{base_url}/api/v1/people/101"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(admin_person.status(), reqwest::StatusCode::OK);
    assert_eq!(admin_person.json::<Value>().await?["isFavorite"], false);

    let denied_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "denied", "password": "denied password" }))
        .send()
        .await?;
    let denied_cookie = cookie_pair(denied_login.headers());
    let denied_person = client
        .get(format!("{base_url}/api/v1/people/101"))
        .header(COOKIE, &denied_cookie)
        .send()
        .await?;
    assert_eq!(denied_person.status(), reqwest::StatusCode::NOT_FOUND);

    let unfavorite = client
        .put(format!("{base_url}/api/v1/people/101/favorite"))
        .header(COOKIE, &viewer_cookie)
        .header("x-csrf-token", &viewer_csrf)
        .json(&json!({ "favorite": false }))
        .send()
        .await?;
    assert_eq!(unfavorite.status(), reqwest::StatusCode::NO_CONTENT);

    server.abort();
    let _ = (admin, denied);
    Ok(())
}

#[tokio::test]
async fn rebuilding_people_does_not_clear_index_when_metadata_library_root_is_missing()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('item-metadata-root-missing', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    service
        .persist_item_actors(
            "item-metadata-root-missing",
            "tmdb",
            &[ActorCredit {
                id: "101".to_owned(),
                provider: None,
                identities: Vec::new(),
                name: "演员甲".to_owned(),
                character: Some("角色甲".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;
    let before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM person_credits WHERE item_id = 'item-metadata-root-missing'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(before, 1);

    tokio::fs::remove_dir_all(config.config_dir.join("metadata/library")).await?;

    assert_eq!(service.rebuild_person_credit_index().await?, 0);
    let after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM person_credits WHERE item_id = 'item-metadata-root-missing'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(after, 1);
    Ok(())
}

#[tokio::test]
async fn rebuilding_people_does_not_rewrite_unchanged_relations_on_restart()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Example Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2024.mkv"), b"movie").await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;

    let people = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    people
        .persist_item_actors(
            &item_id,
            "tmdb",
            &[ActorCredit {
                id: "101".to_owned(),
                provider: None,
                identities: Vec::new(),
                name: "演员甲".to_owned(),
                character: Some("角色甲".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;
    people.rebuild_person_credit_index().await?;
    sqlx::query("UPDATE person_index_item_state SET updated_at = 1 WHERE item_id = ?")
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    sqlx::query(
        "UPDATE person_index_rebuild_jobs
         SET status = 'COMPLETED', cursor_id = ?, processed_count = 1,
             total_count = 1, cancel_requested = 0, run_token = NULL
         WHERE library_id = ?",
    )
    .bind(&item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    people.rebuild_person_credit_index().await?;

    let updated_at: i64 =
        sqlx::query_scalar("SELECT updated_at FROM person_index_item_state WHERE item_id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(updated_at, 1);
    Ok(())
}

#[tokio::test]
async fn rebuilding_people_does_not_restore_relation_snapshot_when_index_state_is_missing()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Example Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2024.mkv"), b"movie").await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;

    let people = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    people
        .persist_item_actors(
            &item_id,
            "tmdb",
            &[ActorCredit {
                id: "101".to_owned(),
                provider: None,
                identities: Vec::new(),
                name: "演员甲".to_owned(),
                character: Some("角色甲".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;
    people.rebuild_person_credit_index().await?;
    sqlx::query("DELETE FROM person_credits")
        .execute(database.pool())
        .await?;
    sqlx::query("DELETE FROM person_index_item_state")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "UPDATE person_index_rebuild_jobs
         SET status = 'COMPLETED', cursor_id = ?, processed_count = 1,
             total_count = 1, cancel_requested = 0, run_token = NULL
         WHERE library_id = ?",
    )
    .bind(&item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    people.rebuild_person_credit_index().await?;

    let restored_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_credits WHERE item_id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(restored_count, 0);
    Ok(())
}

#[tokio::test]
async fn rebuilding_people_clears_index_for_missing_item_metadata_directory()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    for item_id in ["item-metadata-missing", "item-metadata-kept"] {
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(library.id.to_string())
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await?;
    }
    let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    for item_id in ["item-metadata-missing", "item-metadata-kept"] {
        service
            .persist_item_actors(
                item_id,
                "tmdb",
                &[ActorCredit {
                    id: if item_id == "item-metadata-missing" {
                        "101"
                    } else {
                        "102"
                    }
                    .to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员".to_owned(),
                    character: Some("角色".to_owned()),
                    order: Some(0),
                    profile_url: None,
                    person: None,
                }],
            )
            .await?;
    }

    tokio::fs::remove_dir_all(library_item_directory(
        &config.config_dir,
        "item-metadata-missing",
    )?)
    .await?;

    assert_eq!(service.rebuild_person_credit_index().await?, 2);
    let missing_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM person_credits WHERE item_id = 'item-metadata-missing'",
    )
    .fetch_one(database.pool())
    .await?;
    let kept_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM person_credits WHERE item_id = 'item-metadata-kept'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(missing_count, 0);
    assert_eq!(kept_count, 1);
    Ok(())
}

#[tokio::test]
async fn rebuilding_people_keeps_quarantined_relation_after_source_restore()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Example Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2024.mkv"), b"movie").await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let source_entry_id: String = sqlx::query_scalar(
        "SELECT filesystem_entry_id FROM media_sources WHERE item_id = ? LIMIT 1",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    let original_fingerprint: Vec<u8> =
        sqlx::query_scalar("SELECT fingerprint FROM filesystem_entries WHERE id = ?")
            .bind(&source_entry_id)
            .fetch_one(database.pool())
            .await?;

    let people = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    people
        .persist_item_actors(
            &item_id,
            "tmdb",
            &[ActorCredit {
                id: "101".to_owned(),
                provider: None,
                identities: Vec::new(),
                name: "演员甲".to_owned(),
                character: Some("角色甲".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;
    let relation_path = library_item_directory(&config.config_dir, &item_id)?.join("people.json");
    assert!(relation_path.exists());
    sqlx::query("UPDATE filesystem_entries SET fingerprint = ? WHERE id = ?")
        .bind(b"changed-fingerprint".to_vec())
        .bind(&source_entry_id)
        .execute(database.pool())
        .await?;

    people.rebuild_person_credit_index().await?;
    assert!(!relation_path.exists());
    let quarantine_root = config
        .config_dir
        .join("metadata/quarantine/people-relations");
    let mut quarantined = tokio::fs::read_dir(&quarantine_root).await?;
    assert!(quarantined.next_entry().await?.is_some());
    let quarantined_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_credits WHERE item_id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(quarantined_count, 0);

    sqlx::query("UPDATE filesystem_entries SET fingerprint = ? WHERE id = ?")
        .bind(original_fingerprint)
        .bind(&source_entry_id)
        .execute(database.pool())
        .await?;
    sqlx::query(
        "UPDATE person_index_rebuild_jobs
         SET status = 'QUEUED', cursor_id = NULL, processed_count = 0,
             total_count = 0, cancel_requested = 0, run_token = NULL, error = NULL
         WHERE library_id = ?",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    let setup = SetupService::new(database.clone())?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app_state = AppState::ready(config.clone(), database.clone(), setup, web_auth, emby_auth);
    app_state.rebuild_people_index().await;

    let mut startup_status = String::new();
    for _ in 0..200 {
        startup_status =
            sqlx::query_scalar("SELECT status FROM person_index_rebuild_jobs WHERE library_id = ?")
                .bind(library.id.to_string())
                .fetch_one(database.pool())
                .await?;
        if startup_status == "COMPLETED" {
            break;
        }
        sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(startup_status, "COMPLETED");
    assert!(!relation_path.exists());
    let restored_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_credits WHERE item_id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(restored_count, 0);
    let mut remaining = tokio::fs::read_dir(&quarantine_root).await?;
    assert!(remaining.next_entry().await?.is_some());
    Ok(())
}

#[tokio::test]
async fn emby_persons_lists_library_actors_with_shared_admin_key()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup
        .complete("Admin", "Administrator", "correct password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Example Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2024.mkv"), b"movie").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;

    let second_movie_dir = root.join("Second Movie (2023)");
    tokio::fs::create_dir_all(&second_movie_dir).await?;
    tokio::fs::write(
        second_movie_dir.join("Second.Movie.2023.mkv"),
        b"second movie",
    )
    .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let second_item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items
         WHERE library_id = ? AND item_type = 'MOVIE' AND id <> ?
         LIMIT 1",
    )
    .bind(library.id.to_string())
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    sqlx::query("UPDATE media_items SET added_at = ? WHERE id = ?")
        .bind(100_i64)
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_items SET added_at = ? WHERE id = ?")
        .bind(200_i64)
        .bind(&second_item_id)
        .execute(database.pool())
        .await?;

    PeopleService::new(config.config_dir.clone())
        .with_database(database.clone())
        .persist_item_actors(
            &item_id,
            "tmdb",
            &[
                ActorCredit {
                    id: "101".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员甲".to_owned(),
                    character: Some("角色甲".to_owned()),
                    order: Some(0),
                    profile_url: None,
                    person: None,
                },
                ActorCredit {
                    id: "102".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员乙".to_owned(),
                    character: None,
                    order: Some(1),
                    profile_url: None,
                    person: None,
                },
            ],
        )
        .await?;
    PeopleService::new(config.config_dir.clone())
        .with_database(database.clone())
        .persist_item_actors(
            &second_item_id,
            "tmdb",
            &[
                ActorCredit {
                    id: "101".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员甲".to_owned(),
                    character: Some("角色乙".to_owned()),
                    order: Some(0),
                    profile_url: None,
                    person: None,
                },
                ActorCredit {
                    id: "104".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员丁".to_owned(),
                    character: None,
                    order: Some(1),
                    profile_url: None,
                    person: Some(PersonMetadata {
                        biography: Some("演员丁简介".to_owned()),
                        birthday: None,
                        deathday: None,
                        known_for_department: None,
                        place_of_birth: None,
                        provider_ids: std::collections::BTreeMap::new(),
                        genres: Vec::new(),
                        tags: Vec::new(),
                        production_locations: Vec::new(),
                        premiere_date: None,
                        production_year: None,
                        taglines: Vec::new(),
                    }),
                },
            ],
        )
        .await?;
    let relation_path = library_item_directory(&config.config_dir, &item_id)?.join("people.json");
    tokio::fs::write(
        &relation_path,
        serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "actors": [
                {
                    "id": "101",
                    "name": "演员甲",
                    "provider": "tmdb",
                    "character": "角色甲",
                    "order": 0
                },
                {
                    "id": null,
                    "name": "演员乙",
                    "provider": "",
                    "identities": [{"provider": "tmdb", "id": "102"}],
                    "order": 1
                },
                {
                    "id": null,
                    "name": "演员丙",
                    "provider": "",
                    "identities": [{"provider": "imdb", "id": "nm103"}],
                    "order": 2
                }
            ]
        }))?,
    )
    .await?;
    sqlx::query("DELETE FROM person_credits")
        .execute(database.pool())
        .await?;
    sqlx::query("DELETE FROM person_index_item_state")
        .execute(database.pool())
        .await?;
    let people = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    assert!(people.rebuild_person_credit_index().await? > 0);
    assert_eq!(people.rebuild_person_credit_index().await?, 0);

    let key = luxd::auth::admin_api_key::AdminApiKeyService::new(
        config.config_dir.clone(),
        database.clone(),
    )
    .rotate()
    .await?;
    sqlx::query(
        "UPDATE person_index_rebuild_jobs
         SET status = 'RUNNING', cancel_requested = 0
         WHERE library_id = ?",
    )
    .bind(library.id.to_string())
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
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let rebuilds = client
        .get(format!(
            "http://{address}/api/v1/admin/people/index-rebuild"
        ))
        .header("X-Lux-Api-Key", &key)
        .send()
        .await?;
    assert_eq!(rebuilds.status(), reqwest::StatusCode::OK);
    assert_eq!(rebuilds.json::<serde_json::Value>().await?["total"], 1);
    let cancel = client
        .post(format!(
            "http://{address}/api/v1/admin/people/index-rebuild/{}/cancel",
            library.id
        ))
        .header("X-Lux-Api-Key", &key)
        .send()
        .await?;
    assert_eq!(cancel.status(), reqwest::StatusCode::ACCEPTED);
    let query = format!(
        "ParentId={}&PersonTypes=Actor&StartIndex=0&Limit=10&api_key={key}",
        library.id
    );

    let response = client
        .get(format!("http://{address}/Persons?{query}"))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await?;
    assert_eq!(body["TotalRecordCount"], 4);
    assert!(body.get("StartIndex").is_none());
    let actors = body["Items"].as_array().ok_or("missing actor items")?;
    let actor_a = actors
        .iter()
        .find(|actor| actor["Id"] == "101")
        .ok_or("missing actor 101")?;
    assert_eq!(
        actors.iter().filter(|actor| actor["Id"] == "101").count(),
        1
    );
    assert_eq!(actor_a["Name"], "演员甲");
    assert!(matches!(
        actor_a["Role"].as_str(),
        Some("角色甲") | Some("角色乙")
    ));
    assert_eq!(actor_a["Type"], "Person");
    assert!(
        actor_a["ServerId"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert!(actor_a["ImageTags"].is_object());
    assert_eq!(actor_a["BackdropImageTags"], json!([]));
    let actor_b = actors
        .iter()
        .find(|actor| actor["Id"] == "102")
        .ok_or("missing actor 102")?;
    assert_eq!(actor_b["Name"], "演员乙");
    assert!(actor_b.get("Role").is_none());
    let actor_c = actors
        .iter()
        .find(|actor| actor["Id"] == "nm103")
        .ok_or("missing actor nm103")?;
    assert_eq!(actor_c["Name"], "演员丙");
    assert!(actors.iter().any(|actor| actor["Id"] == "104"));

    let avdb_persons = client
        .get(format!(
            "http://{address}/Persons?ParentId={}&Recursive=true&StartIndex=0&Limit=48&Fields=PrimaryImageAspectRatio,ImageTags,ChildCount,RecursiveItemCount,ServerId&api_key={key}&SortBy=SortName&SortOrder=Ascending&PersonTypes=Actor,GuestStar",
            library.id
        ))
        .send()
        .await?;
    assert_eq!(avdb_persons.status(), reqwest::StatusCode::OK);
    let avdb_persons_body: serde_json::Value = avdb_persons.json().await?;
    assert_eq!(avdb_persons_body["TotalRecordCount"], 4);
    assert_eq!(avdb_persons_body["Items"].as_array().map(Vec::len), Some(4));

    let full_query = format!(
        "ParentId={}&Recursive=true&PersonTypes=Actor&StartIndex=0&Limit=1000&Fields=DateCreated,Overview&SortBy=DateCreated&SortOrder=Descending&userid={}&api_key={key}",
        library.id, admin.id
    );
    let full_response = client
        .get(format!("http://{address}/emby/Persons?{full_query}&&"))
        .send()
        .await?;
    assert_eq!(full_response.status(), reqwest::StatusCode::OK);
    let full_body: serde_json::Value = full_response.json().await?;
    assert_eq!(full_body["TotalRecordCount"], 4);
    assert!(full_body.get("StartIndex").is_none());
    assert_eq!(full_body["Items"].as_array().map(Vec::len), Some(4));
    assert_eq!(full_body["Items"][0]["Id"], "104");
    assert!(full_body["Items"][0]["DateCreated"].is_string());
    assert!(full_body["Items"][0].get("Overview").is_some());
    assert!(full_body["Items"][0].get("Role").is_none());

    let ascending = client
        .get(format!(
            "http://{address}/Persons?ParentId={}&Recursive=true&PersonTypes=Actor&Limit=1000&SortBy=DateCreated&SortOrder=Ascending&api_key={key}",
            library.id
        ))
        .send()
        .await?;
    assert_eq!(ascending.status(), reqwest::StatusCode::OK);
    let ascending_body: serde_json::Value = ascending.json().await?;
    let ascending_items = ascending_body["Items"]
        .as_array()
        .ok_or("missing ascending items")?;
    assert_eq!(
        ascending_items.last().ok_or("empty ascending items")?["Id"],
        "104"
    );

    let prefixed = client
        .get(format!("http://{address}/emby/Persons?{query}"))
        .send()
        .await?;
    assert_eq!(prefixed.status(), reqwest::StatusCode::OK);
    assert_eq!(
        prefixed.json::<serde_json::Value>().await?["TotalRecordCount"],
        4
    );

    let non_recursive = client
        .get(format!(
            "http://{address}/Persons?ParentId={}&Recursive=false&PersonTypes=Actor&Limit=1000&api_key={key}",
            library.id
        ))
        .send()
        .await?;
    assert_eq!(non_recursive.status(), reqwest::StatusCode::OK);
    assert_eq!(
        non_recursive.json::<serde_json::Value>().await?["TotalRecordCount"],
        0
    );

    let directors = client
        .get(format!(
            "http://{address}/Persons?ParentId={}&PersonTypes=Director&api_key={key}",
            library.id
        ))
        .send()
        .await?;
    assert_eq!(directors.status(), reqwest::StatusCode::OK);
    assert_eq!(
        directors.json::<serde_json::Value>().await?["Items"],
        json!([])
    );

    let person_detail = client
        .get(format!(
            "http://{address}/emby/Persons/104?Fields=Overview,Role,DateCreated&api_key={key}"
        ))
        .send()
        .await?;
    assert_eq!(person_detail.status(), reqwest::StatusCode::OK);
    let person_detail_body: serde_json::Value = person_detail.json().await?;
    assert_eq!(person_detail_body["Id"], "104");
    assert_eq!(person_detail_body["Type"], "Person");
    assert_eq!(person_detail_body["Overview"], "演员丁简介");
    assert!(person_detail_body["DateCreated"].is_string());
    assert!(person_detail_body["ImageTags"].is_object());
    assert_eq!(person_detail_body["BackdropImageTags"], json!([]));
    assert!(person_detail_body.get("BirthDate").is_none());

    let person_name_detail = client
        .get(format!(
            "http://{address}/emby/Persons/演员丁?Fields=Overview,Role,DateCreated&api_key={key}"
        ))
        .send()
        .await?;
    assert_eq!(person_name_detail.status(), reqwest::StatusCode::OK);
    let person_name_detail_body: serde_json::Value = person_name_detail.json().await?;
    assert_eq!(person_name_detail_body, person_detail_body);

    let root_person_name_detail = client
        .get(format!(
            "http://{address}/Persons/演员丁?Fields=Overview,Role,DateCreated&api_key={key}"
        ))
        .send()
        .await?;
    assert_eq!(root_person_name_detail.status(), reqwest::StatusCode::OK);
    let root_person_name_detail_body: serde_json::Value = root_person_name_detail.json().await?;
    assert_eq!(root_person_name_detail_body, person_detail_body);

    let root_person_detail = client
        .get(format!("http://{address}/Persons/104?api_key={key}"))
        .send()
        .await?;
    assert_eq!(root_person_detail.status(), reqwest::StatusCode::OK);

    let person_update = client
        .post(format!("http://{address}/emby/Items/104?api_key={key}"))
        .json(&json!({
            "Name": "演员丁",
            "ServerId": "ignored-by-lux",
            "Id": "104",
            "Type": "Person",
            "Overview": "MDC 更新后的演员简介",
            "BirthDate": "1990-01-02",
            "Genres": ["Drama"],
            "Tags": ["MDC"],
            "ProviderIds": {
                "Tmdb": "123456",
                "Imdb": "nm1234567"
            },
            "ProductionLocations": ["日本"],
            "PremiereDate": "2000-01-02",
            "ProductionYear": 2000,
            "KnownForDepartment": "Acting",
            "PlaceOfBirth": "北京",
            "Taglines": ["MDC 标语"]
        }))
        .send()
        .await?;
    assert_eq!(person_update.status(), reqwest::StatusCode::OK);
    let person_update_body: serde_json::Value = person_update.json().await?;
    assert_eq!(person_update_body["Id"], "104");
    assert_eq!(person_update_body["Type"], "Person");
    assert_eq!(person_update_body["Overview"], "MDC 更新后的演员简介");
    assert_eq!(person_update_body["BirthDate"], "1990-01-02");
    assert_eq!(person_update_body["KnownForDepartment"], "Acting");
    assert_eq!(person_update_body["PlaceOfBirth"], "北京");
    assert_eq!(person_update_body["Genres"], json!(["Drama"]));
    assert_eq!(person_update_body["Tags"], json!(["MDC"]));
    assert_eq!(person_update_body["ProductionLocations"], json!(["日本"]));
    assert_eq!(person_update_body["PremiereDate"], "2000-01-02");
    assert_eq!(person_update_body["ProductionYear"], 2000);
    assert_eq!(person_update_body["Taglines"], json!(["MDC 标语"]));
    assert_eq!(person_update_body["ProviderIds"]["Imdb"], "nm1234567");

    let updated_relation_path =
        library_item_directory(&temp_dir.path().join("config"), &second_item_id)?
            .join("people.json");
    let updated_relation: serde_json::Value =
        serde_json::from_slice(&tokio::fs::read(updated_relation_path).await?)?;
    let person_key = updated_relation["actors"]
        .as_array()
        .and_then(|actors| actors.iter().find(|actor| actor["id"] == "104"))
        .and_then(|actor| actor["personKey"].as_str())
        .ok_or("missing updated person key")?;
    let person_nfo = lux_person_directory(&temp_dir.path().join("config"), "演员丁", person_key)?
        .join("person.nfo");
    let person_nfo_body = tokio::fs::read_to_string(person_nfo).await?;
    assert!(person_nfo_body.contains("<name>演员丁</name>"));
    assert!(person_nfo_body.contains("<biography>演员丁简介</biography>"));
    assert!(!person_nfo_body.contains("MDC 更新后的演员简介"));
    assert!(person_nfo_body.contains("<birthday>1990-01-02</birthday>"));
    assert!(person_nfo_body.contains("<knownfor>Acting</knownfor>"));
    assert!(person_nfo_body.contains("<placeofbirth>北京</placeofbirth>"));
    assert!(person_nfo_body.contains("<uniqueid type=\"tmdb\">123456</uniqueid>"));
    assert!(person_nfo_body.contains("<uniqueid type=\"imdb\">nm1234567</uniqueid>"));
    assert!(person_nfo_body.contains("<genre>Drama</genre>"));
    assert!(person_nfo_body.contains("<tag>MDC</tag>"));
    assert!(person_nfo_body.contains("<country>日本</country>"));
    assert!(person_nfo_body.contains("<premiered>2000-01-02</premiered>"));
    assert!(person_nfo_body.contains("<year>2000</year>"));
    assert!(person_nfo_body.contains("<tagline>MDC 标语</tagline>"));

    let person_image_upload = client
        .post(format!(
            "http://{address}/emby/Items/104/Images/Primary?api_key={key}"
        ))
        .header("Content-Type", "image/png")
        .body(PNG_1X1)
        .send()
        .await?;
    assert_eq!(
        person_image_upload.status(),
        reqwest::StatusCode::NO_CONTENT
    );

    let person_image = client
        .get(format!(
            "http://{address}/emby/Items/104/Images/Primary?api_key={key}"
        ))
        .send()
        .await?;
    assert_eq!(person_image.status(), reqwest::StatusCode::OK);
    assert_eq!(person_image.headers()["content-type"], "image/png");
    assert_eq!(person_image.bytes().await?.as_ref(), PNG_1X1);

    let encoded_person_image_upload = client
        .post(format!(
            "http://{address}/emby/Items/104/Images/Primary?api_key={key}"
        ))
        .header("Content-Type", "image/png")
        .body(BASE64.encode(PNG_1X1))
        .send()
        .await?;
    assert_eq!(
        encoded_person_image_upload.status(),
        reqwest::StatusCode::NO_CONTENT
    );

    let mismatched_content_type_upload = client
        .post(format!(
            "http://{address}/emby/Items/104/Images/Primary?api_key={key}"
        ))
        .header("Content-Type", "image/png")
        .body(BASE64.encode(JPEG_SIGNATURE))
        .send()
        .await?;
    assert_eq!(
        mismatched_content_type_upload.status(),
        reqwest::StatusCode::NO_CONTENT
    );

    let mismatched_content_type_image = client
        .get(format!(
            "http://{address}/emby/Items/104/Images/Primary?api_key={key}"
        ))
        .send()
        .await?;
    assert_eq!(
        mismatched_content_type_image.headers()["content-type"],
        "image/jpeg"
    );

    let updated_person_detail = client
        .get(format!(
            "http://{address}/emby/Persons/104?Fields=Overview,BirthDate&api_key={key}"
        ))
        .send()
        .await?;
    assert_eq!(updated_person_detail.status(), reqwest::StatusCode::OK);
    let updated_person_detail_body: serde_json::Value = updated_person_detail.json().await?;
    assert_eq!(
        updated_person_detail_body["Overview"],
        "MDC 更新后的演员简介"
    );
    assert_eq!(updated_person_detail_body["BirthDate"], "1990-01-02");

    let lux_person_detail = client
        .get(format!("http://{address}/api/v1/people/104"))
        .header("X-Lux-Api-Key", &key)
        .send()
        .await?;
    assert_eq!(lux_person_detail.status(), reqwest::StatusCode::OK);
    let lux_person_detail_body: serde_json::Value = lux_person_detail.json().await?;
    assert_eq!(lux_person_detail_body["id"], "104");
    assert_eq!(lux_person_detail_body["name"], "演员丁");
    assert_eq!(lux_person_detail_body["biography"], "MDC 更新后的演员简介");

    let lux_person_update = client
        .patch(format!("http://{address}/api/v1/people/104"))
        .header("X-Lux-Api-Key", &key)
        .json(&json!({
            "name": "演员丁（已编辑）",
            "biography": "编辑后的简介",
            "birthday": "1991-02-03",
            "knownForDepartment": "Directing",
            "placeOfBirth": "上海",
            "providerIds": {"Imdb": "nm7654321"},
            "genres": ["Comedy"],
            "tags": ["已编辑"],
            "productionLocations": ["中国"],
            "premiereDate": "2001-02-03",
            "productionYear": 2001,
            "taglines": ["编辑标语"]
        }))
        .send()
        .await?;
    assert_eq!(lux_person_update.status(), reqwest::StatusCode::OK);
    let lux_person_update_body: serde_json::Value = lux_person_update.json().await?;
    assert_eq!(lux_person_update_body["name"], "演员丁（已编辑）");
    assert_eq!(lux_person_update_body["biography"], "编辑后的简介");
    assert_eq!(lux_person_update_body["birthday"], "1991-02-03");
    assert_eq!(lux_person_update_body["knownForDepartment"], "Directing");
    assert_eq!(lux_person_update_body["placeOfBirth"], "上海");
    assert_eq!(lux_person_update_body["providerIds"]["Imdb"], "nm7654321");
    assert_eq!(lux_person_update_body["genres"], json!(["Comedy"]));
    assert_eq!(lux_person_update_body["tags"], json!(["已编辑"]));
    assert_eq!(
        lux_person_update_body["productionLocations"],
        json!(["中国"])
    );
    assert_eq!(lux_person_update_body["premiereDate"], "2001-02-03");
    assert_eq!(lux_person_update_body["productionYear"], 2001);
    assert_eq!(lux_person_update_body["taglines"], json!(["编辑标语"]));

    let edited_person_nfo = tokio::fs::read_to_string(
        lux_person_directory(
            &temp_dir.path().join("config"),
            "演员丁（已编辑）",
            person_key,
        )?
        .join("person.nfo"),
    )
    .await?;
    assert!(edited_person_nfo.contains("<name>演员丁（已编辑）</name>"));
    assert!(edited_person_nfo.contains("<biography>编辑后的简介</biography>"));
    assert!(edited_person_nfo.contains("<birthday>1991-02-03</birthday>"));
    assert!(edited_person_nfo.contains("<knownfor>Directing</knownfor>"));
    assert!(edited_person_nfo.contains("<placeofbirth>上海</placeofbirth>"));
    assert!(edited_person_nfo.contains("<uniqueid type=\"imdb\">nm7654321</uniqueid>"));
    assert!(edited_person_nfo.contains("<genre>Comedy</genre>"));
    assert!(edited_person_nfo.contains("<tag>已编辑</tag>"));
    assert!(edited_person_nfo.contains("<country>中国</country>"));
    assert!(edited_person_nfo.contains("<premiered>2001-02-03</premiered>"));
    assert!(edited_person_nfo.contains("<year>2001</year>"));
    assert!(edited_person_nfo.contains("<tagline>编辑标语</tagline>"));

    let missing_person = client
        .get(format!("http://{address}/Persons/missing?api_key={key}"))
        .send()
        .await?;
    assert_eq!(missing_person.status(), reqwest::StatusCode::NOT_FOUND);

    let missing_lux_person = client
        .get(format!("http://{address}/api/v1/people/missing"))
        .header("X-Lux-Api-Key", &key)
        .send()
        .await?;
    assert_eq!(missing_lux_person.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn people_index_rebuild_admin_api_supports_csrf_pagination_cancel_and_requeue()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let base_url = format!("http://{address}");

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({"username": "admin", "password": "correct password"}))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookies = cookie_pair(login.headers());
    let csrf = cookie_value(login.headers(), "lux_csrf");

    let initial = client
        .get(format!(
            "{base_url}/api/v1/admin/people/index-rebuild?page=1&pageSize=20"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(initial.status(), reqwest::StatusCode::OK);
    assert_eq!(initial.json::<Value>().await?["jobs"], json!([]));

    let missing_csrf = client
        .post(format!(
            "{base_url}/api/v1/admin/people/index-rebuild/{}",
            library.id
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let queued = client
        .post(format!(
            "{base_url}/api/v1/admin/people/index-rebuild/{}",
            library.id
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(queued.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(queued.json::<Value>().await?["job"]["status"], "QUEUED");

    let listed = client
        .get(format!(
            "{base_url}/api/v1/admin/people/index-rebuild?page=1&pageSize=1"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let listed_body = listed.json::<Value>().await?;
    assert_eq!(listed_body["total"], 1);
    assert_eq!(listed_body["jobs"][0]["libraryId"], library.id.to_string());
    assert_eq!(listed_body["jobs"][0]["status"], "QUEUED");

    let cancelled = client
        .post(format!(
            "{base_url}/api/v1/admin/people/index-rebuild/{}/cancel",
            library.id
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(cancelled.status(), reqwest::StatusCode::ACCEPTED);
    let cancelled_body = cancelled.json::<Value>().await?;
    assert_eq!(cancelled_body["job"]["status"], "CANCELLED");
    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json::<Value>()
        .await?;
    let cancel_event = audit["events"]
        .as_array()
        .and_then(|events| {
            events
                .iter()
                .find(|event| event["eventType"] == "PEOPLE_INDEX_REBUILD_CANCEL_REQUESTED")
        })
        .ok_or("missing PEOPLE_INDEX_REBUILD_CANCEL_REQUESTED")?;
    assert_eq!(cancel_event["metadata"]["status"], "CANCELLED");

    let requeued = client
        .post(format!(
            "{base_url}/api/v1/admin/people/index-rebuild/{}",
            library.id
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(requeued.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(requeued.json::<Value>().await?["job"]["status"], "QUEUED");

    server.abort();
    Ok(())
}

fn cookie_pair(headers: &HeaderMap) -> String {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .collect::<Vec<_>>()
        .join("; ")
}

fn cookie_value(headers: &HeaderMap, name: &str) -> String {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .find_map(|value| value.strip_prefix(&format!("{name}=")))
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn emby_item_update_accepts_local_only_actor() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup
        .complete("Admin", "Administrator", "correct password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Local Actor Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Local.Actor.Movie.2024.mkv"), b"movie").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;

    // NFO actors without any provider ID are indexed under a `local-*` ID and
    // never receive a Lux person manifest.
    let relation_path = library_item_directory(&config.config_dir, &item_id)?.join("people.json");
    tokio::fs::create_dir_all(relation_path.parent().ok_or("missing relation dir")?).await?;
    tokio::fs::write(
        &relation_path,
        serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "actors": [
                {
                    "id": null,
                    "name": "本地演员",
                    "provider": "",
                    "order": 0
                }
            ]
        }))?,
    )
    .await?;
    let people = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    assert!(people.rebuild_person_credit_index().await? > 0);
    let person_id: String = sqlx::query_scalar(
        "SELECT person_id FROM person_credits WHERE item_id = ? AND person_name = ?",
    )
    .bind(&item_id)
    .bind("本地演员")
    .fetch_one(database.pool())
    .await?;
    assert!(person_id.starts_with("local-"));

    let key = luxd::auth::admin_api_key::AdminApiKeyService::new(
        config.config_dir.clone(),
        database.clone(),
    )
    .rotate()
    .await?;
    let config_dir = config.config_dir.clone();
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config, database, setup, web_auth, emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

    let person_update = client
        .post(format!(
            "http://{address}/emby/Items/{person_id}?api_key={key}"
        ))
        .json(&json!({
            "Name": "本地演员",
            "Id": person_id,
            "Type": "Person",
            "Overview": "MDC 补全的演员简介",
            "BirthDate": "1995-05-06",
            "ProviderIds": {
                "Javdb": "abc123"
            }
        }))
        .send()
        .await?;
    assert_eq!(person_update.status(), reqwest::StatusCode::OK);
    let person_update_body: serde_json::Value = person_update.json().await?;
    assert_eq!(person_update_body["Id"], person_id.as_str());
    assert_eq!(person_update_body["Overview"], "MDC 补全的演员简介");
    assert_eq!(person_update_body["BirthDate"], "1995-05-06");

    let person_nfo =
        people_directory(&config_dir, "本地演员", "local", &person_id)?.join("person.nfo");
    let person_nfo_body = tokio::fs::read_to_string(person_nfo).await?;
    assert!(person_nfo_body.contains("<name>本地演员</name>"));
    assert!(person_nfo_body.contains("<biography>MDC 补全的演员简介</biography>"));
    assert!(person_nfo_body.contains("<birthday>1995-05-06</birthday>"));

    // Renaming must keep the name-derived local ID stable.
    let person_rename = client
        .post(format!(
            "http://{address}/emby/Items/{person_id}?api_key={key}"
        ))
        .json(&json!({
            "Name": "本地演员新名",
            "Id": person_id,
            "Type": "Person",
            "Overview": "改名后的演员简介"
        }))
        .send()
        .await?;
    assert_eq!(person_rename.status(), reqwest::StatusCode::OK);
    let person_rename_body: serde_json::Value = person_rename.json().await?;
    assert_eq!(person_rename_body["Id"], person_id.as_str());
    assert_eq!(person_rename_body["Name"], "本地演员新名");
    assert_eq!(person_rename_body["Overview"], "改名后的演员简介");

    let renamed_detail = client
        .get(format!(
            "http://{address}/emby/Items/{person_id}?api_key={key}"
        ))
        .send()
        .await?;
    assert_eq!(renamed_detail.status(), reqwest::StatusCode::OK);
    assert_eq!(
        renamed_detail.json::<serde_json::Value>().await?["Name"],
        "本地演员新名"
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn local_actors_without_provider_id_merge_across_roles_and_rebuild_backfills_identities()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;

    for (item_id, title) in [
        ("local-role-movie-1", "本地一"),
        ("local-role-movie-2", "本地二"),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status,
                has_available_source
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(library.id.to_string())
        .bind(title)
        .bind(title)
        .execute(database.pool())
        .await?;
    }

    let people = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    people
        .persist_nfo_item_actors(
            "local-role-movie-1",
            "tmdb",
            &[ActorCredit {
                id: String::new(),
                provider: None,
                identities: Vec::new(),
                name: "本地张飞".to_owned(),
                character: Some("角色A".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
            &[1],
        )
        .await?;
    people
        .persist_nfo_item_actors(
            "local-role-movie-2",
            "tmdb",
            &[ActorCredit {
                id: String::new(),
                provider: None,
                identities: Vec::new(),
                name: "本地张飞".to_owned(),
                character: Some("角色B".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
            &[2],
        )
        .await?;

    let (results, total) = people
        .search_actors(&[library.id.to_string()], "本地张飞", 0, 20)
        .await?;
    assert_eq!(
        total, 1,
        "local actor across different roles should merge into one search result"
    );
    assert_eq!(results.len(), 1);
    assert!(results[0].id.starts_with("local-"));

    // Legacy people.json with `id` + `provider` but empty `identities` should backfill `lux_person_id`
    // from existing `person_identities` during rebuild.
    for (item_id, title) in [
        ("seed-id-movie", "种子电影"),
        ("legacy-id-movie", "旧映射电影"),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status,
                has_available_source
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(library.id.to_string())
        .bind(title)
        .bind(title)
        .execute(database.pool())
        .await?;
    }
    people
        .persist_item_actors(
            "seed-id-movie",
            "tmdb",
            &[ActorCredit {
                id: "424242".to_owned(),
                provider: Some("tmdb".to_owned()),
                identities: Vec::new(),
                name: "已知演员".to_owned(),
                character: None,
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;
    let expected_lux_id: String = sqlx::query_scalar(
        "SELECT person_id FROM person_identities WHERE provider = 'tmdb' AND provider_id = '424242'",
    )
    .fetch_one(database.pool())
    .await?;
    let legacy_relation_path =
        library_item_directory(&config.config_dir, "legacy-id-movie")?.join("people.json");
    tokio::fs::create_dir_all(
        legacy_relation_path
            .parent()
            .ok_or("missing legacy relation parent")?,
    )
    .await?;
    tokio::fs::write(
        &legacy_relation_path,
        serde_json::to_vec(&json!({
            "schemaVersion": 4,
            "actors": [
                {
                    "id": "424242",
                    "name": "已知演员",
                    "provider": "tmdb",
                    "order": 0
                }
            ]
        }))?,
    )
    .await?;
    people.rebuild_person_credit_index().await?;
    let backfilled_lux_id: Option<String> = sqlx::query_scalar(
        "SELECT lux_person_id FROM person_credits WHERE item_id = 'legacy-id-movie' AND person_name = '已知演员'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(backfilled_lux_id.as_deref(), Some(expected_lux_id.as_str()));
    Ok(())
}

#[tokio::test]
async fn multi_identity_actor_collision_auto_merges_canonical_people_on_persist_and_rebuild()
-> Result<(), Box<dyn std::error::Error>> {
    use luxd::application::people::PersonIdentity;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;

    for (item_id, title) in [
        ("merge-tmdb-only", "纯TMDb电影"),
        ("merge-imdb-only", "纯IMDb电影"),
        ("merge-both-ids", "双ID电影"),
        ("rebuild-tmdb-only", "重建TMDb电影"),
        ("rebuild-imdb-only", "重建IMDb电影"),
        ("rebuild-both-ids", "重建双ID电影"),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status,
                has_available_source
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(library.id.to_string())
        .bind(title)
        .bind(title)
        .execute(database.pool())
        .await?;
    }

    let people = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    people
        .persist_item_actors(
            "merge-tmdb-only",
            "tmdb",
            &[ActorCredit {
                id: "1519026".to_owned(),
                provider: Some("tmdb".to_owned()),
                identities: Vec::new(),
                name: "沈腾".to_owned(),
                character: Some("夏洛".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;
    people
        .persist_item_actors(
            "merge-imdb-only",
            "imdb",
            &[ActorCredit {
                id: "nm7613067".to_owned(),
                provider: Some("imdb".to_owned()),
                identities: Vec::new(),
                name: "沈腾".to_owned(),
                character: Some("张扬".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;

    let (_, before_total) = people
        .search_actors(&[library.id.to_string()], "沈腾", 0, 20)
        .await?;
    assert_eq!(before_total, 2);

    let tmdb_lux_id: String = sqlx::query_scalar(
        "SELECT person_id FROM person_identities WHERE provider = 'tmdb' AND provider_id = '1519026'",
    )
    .fetch_one(database.pool())
    .await?;
    let imdb_lux_id: String = sqlx::query_scalar(
        "SELECT person_id FROM person_identities WHERE provider = 'imdb' AND provider_id = 'nm7613067'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_ne!(tmdb_lux_id, imdb_lux_id);

    // Persisting a cast member carrying both IDs must automatically merge the IMDb person into
    // the TMDb-backed canonical person.
    people
        .persist_item_actors(
            "merge-both-ids",
            "tmdb",
            &[ActorCredit {
                id: "1519026".to_owned(),
                provider: Some("tmdb".to_owned()),
                identities: vec![
                    PersonIdentity {
                        provider: "tmdb".to_owned(),
                        id: "1519026".to_owned(),
                    },
                    PersonIdentity {
                        provider: "imdb".to_owned(),
                        id: "nm7613067".to_owned(),
                    },
                ],
                name: "沈腾".to_owned(),
                character: Some("张驰".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;

    let (after_results, after_total) = people
        .search_actors(&[library.id.to_string()], "沈腾", 0, 20)
        .await?;
    assert_eq!(after_total, 1);
    assert_eq!(after_results.len(), 1);

    let imdb_owner: String = sqlx::query_scalar(
        "SELECT person_id FROM person_identities WHERE provider = 'imdb' AND provider_id = 'nm7613067'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(imdb_owner, tmdb_lux_id);

    let merged_status: String = sqlx::query_scalar("SELECT status FROM people WHERE id = ?")
        .bind(&imdb_lux_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(merged_status, "MERGED");

    let unmerged_credits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM person_credits
         WHERE item_id IN ('merge-tmdb-only', 'merge-imdb-only', 'merge-both-ids')
           AND lux_person_id <> ?",
    )
    .bind(&tmdb_lux_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(unmerged_credits, 0);

    // Rebuild path: pre-existing people.json carrying conflicting mapped identities also converges.
    people
        .persist_item_actors(
            "rebuild-tmdb-only",
            "tmdb",
            &[ActorCredit {
                id: "777001".to_owned(),
                provider: Some("tmdb".to_owned()),
                identities: Vec::new(),
                name: "马丽".to_owned(),
                character: None,
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;
    people
        .persist_item_actors(
            "rebuild-imdb-only",
            "imdb",
            &[ActorCredit {
                id: "nm777001".to_owned(),
                provider: Some("imdb".to_owned()),
                identities: Vec::new(),
                name: "马丽".to_owned(),
                character: None,
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;
    let rebuild_tmdb_lux: String = sqlx::query_scalar(
        "SELECT person_id FROM person_identities WHERE provider = 'tmdb' AND provider_id = '777001'",
    )
    .fetch_one(database.pool())
    .await?;
    let rebuild_relation_path =
        library_item_directory(&config.config_dir, "rebuild-both-ids")?.join("people.json");
    tokio::fs::create_dir_all(
        rebuild_relation_path
            .parent()
            .ok_or("missing rebuild relation parent")?,
    )
    .await?;
    tokio::fs::write(
        &rebuild_relation_path,
        serde_json::to_vec(&json!({
            "schemaVersion": 4,
            "actors": [
                {
                    "id": "777001",
                    "name": "马丽",
                    "provider": "tmdb",
                    "identities": [
                        { "provider": "imdb", "id": "nm777001" },
                        { "provider": "tmdb", "id": "777001" }
                    ],
                    "order": 0
                }
            ]
        }))?,
    )
    .await?;
    people.rebuild_person_credit_index().await?;
    let (_, mali_total) = people
        .search_actors(&[library.id.to_string()], "马丽", 0, 20)
        .await?;
    assert_eq!(mali_total, 1);
    let mali_credit_lux: Option<String> = sqlx::query_scalar(
        "SELECT lux_person_id FROM person_credits WHERE item_id = 'rebuild-both-ids' AND person_name = '马丽'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(mali_credit_lux.as_deref(), Some(rebuild_tmdb_lux.as_str()));
    Ok(())
}

#[tokio::test]
async fn admin_people_merge_endpoint_merges_identities_favorites_assets_and_survives_restart()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = luxd::auth::users::UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;

    for (item_id, title) in [("adm-merge-1", "手动合并一"), ("adm-merge-2", "手动合并二")]
    {
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status,
                has_available_source
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(library.id.to_string())
        .bind(title)
        .bind(title)
        .execute(database.pool())
        .await?;
    }

    let people = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    people
        .persist_item_actors(
            "adm-merge-1",
            "tmdb",
            &[ActorCredit {
                id: "888001".to_owned(),
                provider: Some("tmdb".to_owned()),
                identities: Vec::new(),
                name: "艾伦".to_owned(),
                character: None,
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;
    people
        .persist_item_actors(
            "adm-merge-2",
            "imdb",
            &[ActorCredit {
                id: "nm888001".to_owned(),
                provider: Some("imdb".to_owned()),
                identities: Vec::new(),
                name: "艾伦".to_owned(),
                character: None,
                order: Some(0),
                profile_url: None,
                person: Some(PersonMetadata {
                    biography: Some("来自 IMDb 的补充简介".to_owned()),
                    ..PersonMetadata::default()
                }),
            }],
        )
        .await?;
    people
        .update_person_image("nm888001", "艾伦", Some("imdb"), Some("image/png"), PNG_1X1)
        .await?;

    let target_id: String = sqlx::query_scalar(
        "SELECT person_id FROM person_identities WHERE provider = 'tmdb' AND provider_id = '888001'",
    )
    .fetch_one(database.pool())
    .await?;
    let source_id: String = sqlx::query_scalar(
        "SELECT person_id FROM person_identities WHERE provider = 'imdb' AND provider_id = 'nm888001'",
    )
    .fetch_one(database.pool())
    .await?;

    sqlx::query("INSERT INTO user_person_state (user_id, person_id, is_favorite) VALUES (?, ?, 1)")
        .bind(viewer.id.to_string())
        .bind(&source_id)
        .execute(database.pool())
        .await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let base_url = format!("http://{address}");

    let viewer_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({"username": "viewer", "password": "viewer password"}))
        .send()
        .await?;
    let viewer_cookies = cookie_pair(viewer_login.headers());
    let viewer_csrf = cookie_value(viewer_login.headers(), "lux_csrf");
    let forbidden = client
        .post(format!("{base_url}/api/v1/admin/people/merge"))
        .header(COOKIE, &viewer_cookies)
        .header("x-csrf-token", &viewer_csrf)
        .json(&json!({"sourcePersonId": source_id, "targetPersonId": target_id}))
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({"username": "admin", "password": "correct password"}))
        .send()
        .await?;
    let admin_cookies = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");

    let missing_csrf = client
        .post(format!("{base_url}/api/v1/admin/people/merge"))
        .header(COOKIE, &admin_cookies)
        .json(&json!({"sourcePersonId": source_id, "targetPersonId": target_id}))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let self_merge = client
        .post(format!("{base_url}/api/v1/admin/people/merge"))
        .header(COOKIE, &admin_cookies)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({"sourcePersonId": target_id, "targetPersonId": target_id}))
        .send()
        .await?;
    assert_eq!(self_merge.status(), reqwest::StatusCode::CONFLICT);

    let missing_person = client
        .post(format!("{base_url}/api/v1/admin/people/merge"))
        .header(COOKIE, &admin_cookies)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({"sourcePersonId": "lux-999999", "targetPersonId": target_id}))
        .send()
        .await?;
    assert_eq!(missing_person.status(), reqwest::StatusCode::NOT_FOUND);

    let merged = client
        .post(format!("{base_url}/api/v1/admin/people/merge"))
        .header(COOKIE, &admin_cookies)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "sourcePersonId": source_id,
            "targetPersonId": target_id,
            "evidence": { "reason": "manual merge" }
        }))
        .send()
        .await?;
    assert_eq!(merged.status(), reqwest::StatusCode::OK);
    let merged_body: Value = merged.json().await?;
    assert_eq!(merged_body["status"], "MERGED");
    assert_eq!(merged_body["sourcePersonId"], source_id);
    assert_eq!(merged_body["targetPersonId"], target_id);

    let duplicate_merge = client
        .post(format!("{base_url}/api/v1/admin/people/merge"))
        .header(COOKIE, &admin_cookies)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({"sourcePersonId": source_id, "targetPersonId": target_id}))
        .send()
        .await?;
    assert_eq!(duplicate_merge.status(), reqwest::StatusCode::CONFLICT);

    let search = client
        .get(format!(
            "{base_url}/api/v1/people?q=艾伦&page=1&pageSize=12"
        ))
        .header(COOKIE, &viewer_cookies)
        .send()
        .await?;
    assert_eq!(search.status(), reqwest::StatusCode::OK);
    let search_body: Value = search.json().await?;
    assert_eq!(search_body["total"], 1);
    assert!(search_body["items"][0]["imageUrl"].is_string());

    let detail = client
        .get(format!("{base_url}/api/v1/people/{target_id}"))
        .header(COOKIE, &viewer_cookies)
        .send()
        .await?;
    assert_eq!(detail.status(), reqwest::StatusCode::OK);
    let detail_body: Value = detail.json().await?;
    assert_eq!(detail_body["isFavorite"], true);

    let items = client
        .get(format!(
            "{base_url}/api/v1/people/{target_id}/items?page=1&pageSize=20"
        ))
        .header(COOKIE, &viewer_cookies)
        .send()
        .await?;
    assert_eq!(items.status(), reqwest::StatusCode::OK);
    let items_body: Value = items.json().await?;
    assert_eq!(items_body["total"], 2);

    // Simulating manifest restore after restart must keep the merged source MERGED.
    sqlx::query("UPDATE person_manifest_restore_state SET status = 'PENDING'")
        .execute(database.pool())
        .await?;
    people.rebuild_person_credit_index().await?;
    let source_status: String = sqlx::query_scalar("SELECT status FROM people WHERE id = ?")
        .bind(&source_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(source_status, "MERGED");

    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit"))
        .header(COOKIE, &admin_cookies)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert!(audit["events"].as_array().is_some_and(|events| {
        events
            .iter()
            .any(|event| event["eventType"] == "PEOPLE_MERGED")
    }));

    server.abort();
    Ok(())
}

#[tokio::test]
async fn rebuilding_people_migrates_legacy_local_actor_favorites_and_assets_after_id_algorithm_change()
-> Result<(), Box<dyn std::error::Error>> {
    use sha2::{Digest, Sha256};

    fn legacy_local_id(name: &str, character: Option<&str>) -> String {
        let mut hasher = Sha256::new();
        hasher.update(name.trim().as_bytes());
        hasher.update([0]);
        hasher.update(character.unwrap_or_default().trim().as_bytes());
        let digest = hasher.finalize();
        let hex = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!("local-{hex}")
    }

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = luxd::auth::users::UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    sqlx::query("INSERT INTO user_library_access (user_id, library_id, can_view) VALUES (?, ?, 1)")
        .bind(viewer.id.to_string())
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;

    for (item_id, title) in [
        ("legacy-local-a", "旧本地角色甲"),
        ("legacy-local-b", "旧本地角色乙"),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status,
                has_available_source
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(library.id.to_string())
        .bind(title)
        .bind(title)
        .execute(database.pool())
        .await?;
    }

    let people = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    people
        .persist_item_actors(
            "legacy-local-a",
            "tmdb",
            &[ActorCredit {
                id: String::new(),
                provider: None,
                identities: Vec::new(),
                name: "群演甲".to_owned(),
                character: Some("路人A".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;
    people
        .persist_item_actors(
            "legacy-local-b",
            "tmdb",
            &[ActorCredit {
                id: String::new(),
                provider: None,
                identities: Vec::new(),
                name: "群演甲".to_owned(),
                character: Some("路人B".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;

    // Simulate a pre-LUX-918 library state where movie A's local actor had `sha256(name|role)`,
    // a user favorite, and an uploaded avatar under that old ID, and an older rebuild schema ran.
    let old_local_a = legacy_local_id("群演甲", Some("路人A"));
    let old_local_b = legacy_local_id("群演甲", Some("路人B"));
    sqlx::query("UPDATE person_credits SET person_id = ? WHERE item_id = 'legacy-local-a'")
        .bind(&old_local_a)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE person_credits SET person_id = ? WHERE item_id = 'legacy-local-b'")
        .bind(&old_local_b)
        .execute(database.pool())
        .await?;
    sqlx::query("DELETE FROM person_index_item_state")
        .execute(database.pool())
        .await?;

    people
        .update_person_image(&old_local_a, "群演甲", None, Some("image/png"), PNG_1X1)
        .await?;
    sqlx::query("INSERT INTO user_person_state (user_id, person_id, is_favorite) VALUES (?, ?, 1)")
        .bind(viewer.id.to_string())
        .bind(&old_local_a)
        .execute(database.pool())
        .await?;

    // Upgrading and running rebuild should converge credits AND migrate the old favorite & avatar.
    people.rebuild_person_credit_index().await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let base_url = format!("http://{address}");

    let viewer_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({"username": "viewer", "password": "viewer password"}))
        .send()
        .await?;
    let viewer_cookies = cookie_pair(viewer_login.headers());

    let search = client
        .get(format!(
            "{base_url}/api/v1/people?q=群演甲&page=1&pageSize=12"
        ))
        .header(COOKIE, &viewer_cookies)
        .send()
        .await?;
    assert_eq!(search.status(), reqwest::StatusCode::OK);
    let search_body: Value = search.json().await?;
    assert_eq!(search_body["total"], 1);
    let converged_id = search_body["items"][0]["id"]
        .as_str()
        .expect("converged local id")
        .to_owned();
    assert_ne!(converged_id, old_local_a);
    assert_ne!(converged_id, old_local_b);

    let detail = client
        .get(format!("{base_url}/api/v1/people/{converged_id}"))
        .header(COOKIE, &viewer_cookies)
        .send()
        .await?;
    assert_eq!(detail.status(), reqwest::StatusCode::OK);
    let detail_body: Value = detail.json().await?;
    assert_eq!(detail_body["isFavorite"], true);
    let image_url = detail_body["imageUrl"]
        .as_str()
        .expect("migrated imageUrl")
        .to_owned();

    let image = client
        .get(format!("{base_url}{image_url}"))
        .header(COOKIE, &viewer_cookies)
        .send()
        .await?;
    assert_eq!(image.status(), reqwest::StatusCode::OK);
    assert_eq!(image.bytes().await?.as_ref(), PNG_1X1);

    let items = client
        .get(format!(
            "{base_url}/api/v1/people/{converged_id}/items?page=1&pageSize=20"
        ))
        .header(COOKIE, &viewer_cookies)
        .send()
        .await?;
    assert_eq!(items.status(), reqwest::StatusCode::OK);
    assert_eq!(items.json::<Value>().await?["total"], 2);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn rebuild_preserves_and_recovers_legacy_v1_relations_after_v2_device_drift()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let preserved_dir = root.join("Preserved (2024)");
    let recovered_dir = root.join("Recovered (2024)");
    let stale_dir = root.join("Stale (2024)");
    tokio::fs::create_dir_all(&preserved_dir).await?;
    tokio::fs::create_dir_all(&recovered_dir).await?;
    tokio::fs::create_dir_all(&stale_dir).await?;
    tokio::fs::write(preserved_dir.join("Preserved.2024.mkv"), b"preserved").await?;
    tokio::fs::write(recovered_dir.join("Recovered.2024.mkv"), b"recovered").await?;
    tokio::fs::write(stale_dir.join("Stale.2024.mkv"), b"stale").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let (preserved_id, preserved_entry_id, preserved_rel, preserved_size, preserved_mtime): (
        String,
        String,
        String,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT mi.id, fe.id, fe.relative_path, fe.size, fe.modified_at
         FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE mi.library_id = ? AND fe.relative_path LIKE 'Preserved%' LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let (recovered_id, recovered_rel, recovered_size, recovered_mtime): (String, String, i64, i64) =
        sqlx::query_as(
            "SELECT mi.id, fe.relative_path, fe.size, fe.modified_at
             FROM media_items mi
             JOIN media_sources ms ON ms.item_id = mi.id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             WHERE mi.library_id = ? AND fe.relative_path LIKE 'Recovered%' LIMIT 1",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?;
    let (stale_id, stale_rel, stale_size, stale_mtime): (String, String, i64, i64) =
        sqlx::query_as(
            "SELECT mi.id, fe.relative_path, fe.size, fe.modified_at
             FROM media_items mi
             JOIN media_sources ms ON ms.item_id = mi.id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             WHERE mi.library_id = ? AND fe.relative_path LIKE 'Stale%' LIMIT 1",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?;

    let encode_hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let preserved_v1_fp = luxd::application::scanner::compute_file_fingerprint(
        &preserved_rel,
        preserved_size,
        preserved_mtime,
        Some(56),
        Some(10_001),
    );
    let recovered_v1_fp = luxd::application::scanner::compute_file_fingerprint(
        &recovered_rel,
        recovered_size,
        recovered_mtime,
        Some(56),
        Some(10_002),
    );
    let stale_other_v2_fp = luxd::application::scanner::compute_scan_fingerprint_v2(
        &stale_rel,
        stale_size,
        stale_mtime,
        Some(99),
        Some(99_999),
    );

    let canonical_root: String =
        sqlx::query_scalar("SELECT canonical_path FROM library_roots WHERE library_id = ?")
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
    let preserved_v2_fp = luxd::application::scanner::compute_scan_fingerprint_v2(
        &preserved_rel,
        preserved_size,
        preserved_mtime,
        Some(57),
        Some(10_001),
    );
    sqlx::query(
        "UPDATE filesystem_entries SET device = 57, inode = 10001, fingerprint = ? WHERE id = ?",
    )
    .bind(preserved_v2_fp)
    .bind(&preserved_entry_id)
    .execute(database.pool())
    .await?;

    let preserved_relation_path =
        library_item_directory(&config.config_dir, &preserved_id)?.join("people.json");
    tokio::fs::create_dir_all(
        preserved_relation_path
            .parent()
            .ok_or("missing preserved relation parent")?,
    )
    .await?;
    tokio::fs::write(
        &preserved_relation_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 4,
            "generation": 1,
            "itemId": preserved_id,
            "sourceRoot": canonical_root,
            "sourceRelativePath": preserved_rel,
            "mediaFingerprint": encode_hex(&preserved_v1_fp),
            "mediaSize": preserved_size,
            "mediaModifiedAt": preserved_mtime,
            "mediaTitle": "Preserved",
            "mediaProductionYear": 2024,
            "actors": [{
                "id": "1519026",
                "name": "沈腾",
                "provider": "tmdb",
                "identities": [{"provider": "tmdb", "id": "1519026"}],
                "order": 0
            }]
        }))?,
    )
    .await?;

    let quarantine_dir = config
        .config_dir
        .join("metadata/quarantine/people-relations");
    tokio::fs::create_dir_all(&quarantine_dir).await?;
    let recovered_quarantine = quarantine_dir.join("relation-recovered.json");
    tokio::fs::write(
        &recovered_quarantine,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 4,
            "generation": 2,
            "itemId": recovered_id,
            "sourceRoot": canonical_root,
            "sourceRelativePath": recovered_rel,
            "mediaFingerprint": encode_hex(&recovered_v1_fp),
            "mediaSize": recovered_size,
            "mediaModifiedAt": recovered_mtime,
            "mediaTitle": "Recovered",
            "mediaProductionYear": 2024,
            "actors": [{
                "id": "1519026",
                "name": "沈腾",
                "provider": "tmdb",
                "identities": [{"provider": "tmdb", "id": "1519026"}],
                "order": 0
            }]
        }))?,
    )
    .await?;
    let stale_quarantine = quarantine_dir.join("relation-stale-v2.json");
    tokio::fs::write(
        &stale_quarantine,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 4,
            "generation": 1,
            "itemId": stale_id,
            "sourceRoot": canonical_root,
            "sourceRelativePath": stale_rel,
            "mediaFingerprint": encode_hex(&stale_other_v2_fp),
            "mediaSize": stale_size,
            "mediaModifiedAt": stale_mtime,
            "mediaTitle": "Stale",
            "mediaProductionYear": 2024,
            "actors": [{
                "id": "999001",
                "name": "失效演员",
                "provider": "tmdb",
                "order": 0
            }]
        }))?,
    )
    .await?;

    let people = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    people.rebuild_person_credit_index().await?;

    assert!(preserved_relation_path.exists());
    assert!(!recovered_quarantine.exists());
    let recovered_relation_path =
        library_item_directory(&config.config_dir, &recovered_id)?.join("people.json");
    assert!(recovered_relation_path.exists());
    assert!(stale_quarantine.exists());

    let preserved_credits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_credits WHERE item_id = ?")
            .bind(&preserved_id)
            .fetch_one(database.pool())
            .await?;
    let recovered_credits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_credits WHERE item_id = ?")
            .bind(&recovered_id)
            .fetch_one(database.pool())
            .await?;
    let stale_credits: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_credits WHERE item_id = ?")
            .bind(&stale_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(preserved_credits, 1);
    assert_eq!(recovered_credits, 1);
    assert_eq!(stale_credits, 0);
    Ok(())
}

#[tokio::test]
async fn startup_backfill_reparses_pre_upgrade_dual_nfos_and_merges_existing_people()
-> Result<(), Box<dyn std::error::Error>> {
    use luxd::application::nfo::{LocalNfoDetails, LocalNfoMetadataStore};
    use luxd::application::scanner::ScanJobService;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let pegasus2 = root.join("飞驰人生2 (2024)");
    let extra_imdb = root.join("你好李焕英外传 (2024)");
    tokio::fs::create_dir_all(&pegasus2).await?;
    tokio::fs::create_dir_all(&extra_imdb).await?;
    tokio::fs::write(pegasus2.join("飞驰人生2 (2024) - 1080p.mkv"), b"pegasus2").await?;
    tokio::fs::write(
        pegasus2.join("movie.nfo"),
        r#"<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<movie>
  <title>飞驰人生2</title>
  <year>2024</year>
  <actor>
    <name>沈腾</name>
    <role>Zhang Chi</role>
    <type>Actor</type>
  </actor>
</movie>"#,
    )
    .await?;
    tokio::fs::write(
        pegasus2.join("飞驰人生2 (2024) - 1080p.nfo"),
        r#"<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<movie>
  <title>飞驰人生2</title>
  <year>2024</year>
  <actor>
    <name>沈腾</name>
    <role>Zhang Chi</role>
    <type>Actor</type>
    <tmdbid>1519026</tmdbid>
    <imdbid>nm7613067</imdbid>
  </actor>
</movie>"#,
    )
    .await?;
    tokio::fs::write(extra_imdb.join("你好李焕英外传 (2024).mkv"), b"extra").await?;
    tokio::fs::write(
        extra_imdb.join("你好李焕英外传 (2024).nfo"),
        r#"<?xml version="1.0" encoding="utf-8" standalone="yes"?>
<movie>
  <title>你好李焕英外传</title>
  <year>2024</year>
  <actor>
    <name>沈腾</name>
    <role>客串</role>
    <type>Actor</type>
    <imdbid>nm7613067</imdbid>
  </actor>
</movie>"#,
    )
    .await?;

    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let pegasus2_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND title LIKE '飞驰人生2%' LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let extra_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND title LIKE '你好李焕英外传%' LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;

    let people = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    people
        .persist_item_actors(
            &extra_id,
            "imdb",
            &[ActorCredit {
                id: "nm7613067".to_owned(),
                provider: Some("imdb".to_owned()),
                identities: Vec::new(),
                name: "沈腾".to_owned(),
                character: Some("客串".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;
    let seed_tmdb_dir = root.join("西虹市首富 (2018)");
    tokio::fs::create_dir_all(&seed_tmdb_dir).await?;
    tokio::fs::write(seed_tmdb_dir.join("西虹市首富 (2018).mkv"), b"tmdb-seed").await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let tmdb_seed_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND title LIKE '西虹市首富%' LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    people
        .persist_item_actors(
            &tmdb_seed_id,
            "tmdb",
            &[ActorCredit {
                id: "1519026".to_owned(),
                provider: Some("tmdb".to_owned()),
                identities: Vec::new(),
                name: "沈腾".to_owned(),
                character: Some("王多鱼".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
        )
        .await?;
    let tmdb_person_id: String = sqlx::query_scalar(
        "SELECT person_id FROM person_identities WHERE provider = 'tmdb' AND provider_id = '1519026'",
    )
    .fetch_one(database.pool())
    .await?;
    let imdb_person_id: String = sqlx::query_scalar(
        "SELECT person_id FROM person_identities WHERE provider = 'imdb' AND provider_id = 'nm7613067'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_ne!(tmdb_person_id, imdb_person_id);

    let pegasus2_nfo_bytes = tokio::fs::read(pegasus2.join("飞驰人生2 (2024) - 1080p.nfo")).await?;
    let pegasus2_movie_nfo_bytes = tokio::fs::read(pegasus2.join("movie.nfo")).await?;
    let old_combined_fp = {
        use sha2::{Digest, Sha256};
        let p = Sha256::digest(&pegasus2_nfo_bytes);
        let c = Sha256::digest(&pegasus2_movie_nfo_bytes);
        let mut h = Sha256::new();
        h.update(&p);
        h.update(b"\0movie.nfo\0");
        h.update(&c);
        h.finalize().to_vec()
    };
    let encode_hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let pegasus2_rel_path =
        library_item_directory(&config.config_dir, &pegasus2_id)?.join("people.json");
    tokio::fs::create_dir_all(pegasus2_rel_path.parent().ok_or("missing parent")?).await?;
    tokio::fs::write(
        &pegasus2_rel_path,
        serde_json::to_vec_pretty(&json!({
            "schemaVersion": 4,
            "generation": 1,
            "sourceFingerprint": encode_hex(&old_combined_fp),
            "itemId": pegasus2_id,
            "actors": [{
                "id": null,
                "name": "沈腾",
                "provider": "",
                "character": "Zhang Chi",
                "order": 0
            }]
        }))?,
    )
    .await?;

    let stale_v1_cache = serde_json::json!({
        "schemaVersion": 1,
        "semanticFingerprint": vec![1_u8; 32],
        "relationFingerprint": old_combined_fp,
        "details": serde_json::to_value(LocalNfoDetails::default())?,
    })
    .to_string();
    sqlx::query(
        "INSERT INTO media_item_nfo_cache (item_id, fingerprint, json, bytes) VALUES (?, ?, ?, ?)
         ON CONFLICT(item_id) DO UPDATE SET fingerprint = excluded.fingerprint, json = excluded.json, bytes = excluded.bytes",
    )
    .bind(&pegasus2_id)
    .bind(&old_combined_fp)
    .bind(&stale_v1_cache)
    .bind(i64::try_from(stale_v1_cache.len())?)
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE media_items SET metadata_fingerprint = ? WHERE id = ?")
        .bind(&old_combined_fp)
        .bind(&pegasus2_id)
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO scan_local_metadata_backfills (library_root_id, status)
         SELECT id, 'COMPLETED' FROM library_roots WHERE library_id = ?
         ON CONFLICT(library_root_id) DO UPDATE SET status = 'COMPLETED'",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    let jobs = ScanJobService::new(database.clone())
        .with_nfo_store(LocalNfoMetadataStore::new(database.clone()))
        .with_people(people.clone());
    jobs.start_local_metadata_outbox_worker().await?;

    let wait_result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status: String = sqlx::query_scalar(
                "SELECT status FROM scan_local_metadata_backfills
                 WHERE library_root_id = (SELECT id FROM library_roots WHERE library_id = ?)",
            )
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
            let (_, total) = people
                .search_actors(&[library.id.to_string()], "沈腾", 0, 20)
                .await?;
            let credit_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM person_credits WHERE item_id IN (?, ?, ?) AND person_name = '沈腾'",
            )
            .bind(&pegasus2_id)
            .bind(&extra_id)
            .bind(&tmdb_seed_id)
            .fetch_one(database.pool())
            .await?;
            if status == "COMPLETED" && total == 1 && credit_count == 3 {
                return Ok::<(), Box<dyn std::error::Error>>(());
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    if wait_result.is_err() {
        let backfill_row: (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT status, cursor_entry_id, error FROM scan_local_metadata_backfills
             WHERE library_root_id = (SELECT id FROM library_roots WHERE library_id = ?)",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?;
        let (_, total) = people
            .search_actors(&[library.id.to_string()], "沈腾", 0, 20)
            .await?;
        let credits: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT item_id, person_id, provider, lux_person_id FROM person_credits WHERE person_name = '沈腾'",
        )
        .fetch_all(database.pool())
        .await?;
        let people_rows: Vec<(String, String)> = sqlx::query_as("SELECT id, status FROM people")
            .fetch_all(database.pool())
            .await?;
        panic!(
            "timed out: backfill={backfill_row:?}, total={total}, credits={credits:?}, people={people_rows:?}"
        );
    }
    wait_result??;

    let (_, total) = people
        .search_actors(&[library.id.to_string()], "沈腾", 0, 20)
        .await?;
    assert_eq!(total, 1);
    let converged_person: String = sqlx::query_scalar(
        "SELECT person_id FROM person_identities WHERE provider = 'imdb' AND provider_id = 'nm7613067'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(converged_person, tmdb_person_id);
    Ok(())
}
