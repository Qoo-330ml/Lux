use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
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

#[tokio::test]
async fn fts_search_matches_chinese_titles_and_aliases_with_acl()
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
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let unrelated_library = libraries
        .create_library("Unrelated", LibraryKind::Series, false)
        .await?;
    sqlx::query("UPDATE libraries SET is_enabled = 0 WHERE id = ?")
        .bind(unrelated_library.id.to_string())
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, ?, 1)",
    )
    .bind(viewer.id.to_string())
    .bind(unrelated_library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE users SET library_access_mode = 'SELECTED' WHERE id = ?")
        .bind(viewer.id.to_string())
        .execute(database.pool())
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("银河护卫队.2024.mkv"), b"one").await?;
    tokio::fs::write(root.join("Hidden.Movie.2025.mkv"), b"two").await?;
    tokio::fs::write(root.join("Movie.2026.mkv"), b"three").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let chinese_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE title = '银河护卫队'")
            .fetch_one(database.pool())
            .await?;
    let collections_library_id: String =
        sqlx::query_scalar("SELECT id FROM libraries WHERE system_kind = 'COLLECTIONS'")
            .fetch_one(database.pool())
            .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status,
             identity_key, has_available_source
         ) VALUES ('logical-search-work', ?, 'MOVIE', '银河护卫队', '银河护卫队',
                   'ONLINE_CONFIRMED', 'global-work:tmdb:search-test', 1)",
    )
    .bind(&collections_library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO collection_work_items (work_item_id, movie_provider, movie_provider_id)
         VALUES ('logical-search-work', 'tmdb', 'search-test')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO collection_work_sources (work_item_id, source_item_id, sort_order)
         VALUES ('logical-search-work', ?, 0)",
    )
    .bind(&chinese_id)
    .execute(database.pool())
    .await?;
    let hidden_movie_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE title = 'Hidden Movie'")
            .fetch_one(database.pool())
            .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status,
             identity_key, has_available_source
         ) VALUES ('logical-partial-search-work', ?, 'MOVIE', 'Hidden Movie', 'hidden movie',
                   'ONLINE_CONFIRMED', 'global-work:tmdb:partial-search-test', 1)",
    )
    .bind(&collections_library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO collection_work_items (work_item_id, movie_provider, movie_provider_id)
         VALUES ('logical-partial-search-work', 'tmdb', 'partial-search-test')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO collection_work_sources (work_item_id, source_item_id, sort_order)
         VALUES ('logical-partial-search-work', ?, 0)",
    )
    .bind(&hidden_movie_id)
    .execute(database.pool())
    .await?;
    let initial_fts_sort_title: String =
        sqlx::query_scalar("SELECT sort_title FROM media_search WHERE item_id = ?")
            .bind(&hidden_movie_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(initial_fts_sort_title, "");

    sqlx::query("UPDATE media_items SET sort_title = 'Canonical Hidden' WHERE id = ?")
        .bind(&hidden_movie_id)
        .execute(database.pool())
        .await?;
    let canonical_sort_title_matches: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_search
         WHERE media_search MATCH 'Canonical' AND item_id = ?",
    )
    .bind(&hidden_movie_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(canonical_sort_title_matches, 1);
    let emby_chinese_id = emby_public_id(&chinese_id);
    sqlx::query(
        "INSERT INTO item_aliases (id, item_id, alias, language, alias_normalized)
         VALUES (?, ?, '星际守护者', 'zh-CN', '星际守护者')",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(&chinese_id)
    .execute(database.pool())
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
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="SearchTest", Device="Mac", DeviceId="search-admin", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();
    let hints = client
        .get(format!("{base_url}/Search/Hints"))
        .query(&[("api_key", token.as_str()), ("SearchTerm", "星际守护者")])
        .send()
        .await?;
    assert_eq!(hints.status(), reqwest::StatusCode::OK);
    let hints_body = hints.json::<Value>().await?;
    assert_eq!(hints_body["TotalRecordCount"], 1);
    assert_eq!(hints_body["SearchHints"][0]["Id"], emby_chinese_id);

    let item_search = client
        .get(format!("{base_url}/Users/{}/Items", admin.id))
        .query(&[
            ("api_key", token.as_str()),
            ("SearchTerm", "星际守护者"),
            ("IncludeItemTypes", "Movie,Series"),
            ("Recursive", "true"),
        ])
        .send()
        .await?;
    assert_eq!(item_search.status(), reqwest::StatusCode::OK);
    let item_search_body = item_search.json::<Value>().await?;
    assert_eq!(item_search_body["TotalRecordCount"], 1);
    assert_eq!(item_search_body["Items"][0]["Id"], emby_chinese_id);

    let lux_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let session = lux_login
        .headers()
        .get_all("set-cookie")
        .iter()
        .find_map(|value| {
            let value = value.to_str().ok()?;
            value.strip_prefix("lux_session=")?.split(';').next()
        })
        .ok_or("missing session")?;
    let lux = client
        .get(format!("{base_url}/api/v1/search?q=银河"))
        .header("Cookie", format!("lux_session={session}"))
        .send()
        .await?;
    assert_eq!(lux.status(), reqwest::StatusCode::OK);
    let lux_body = lux.json::<Value>().await?;
    assert_eq!(lux_body["total"], 1);
    assert_eq!(lux_body["items"][0]["id"], chinese_id);

    let multi_word = client
        .get(format!("{base_url}/api/v1/search?q=Hidden%20Movie"))
        .header("Cookie", format!("lux_session={session}"))
        .send()
        .await?;
    assert_eq!(multi_word.status(), reqwest::StatusCode::OK);
    let multi_word_body = multi_word.json::<Value>().await?;
    assert_eq!(multi_word_body["total"], 1);
    assert_eq!(multi_word_body["items"][0]["id"], hidden_movie_id);

    let punctuation = client
        .get(format!("{base_url}/api/v1/search?q=Hidden.Movie"))
        .header("Cookie", format!("lux_session={session}"))
        .send()
        .await?;
    assert_eq!(punctuation.status(), reqwest::StatusCode::OK);
    let punctuation_body = punctuation.json::<Value>().await?;
    assert_eq!(punctuation_body["total"], 1);
    assert_eq!(punctuation_body["items"][0]["id"], hidden_movie_id);

    let substring = client
        .get(format!("{base_url}/api/v1/search?q=idden"))
        .header("Cookie", format!("lux_session={session}"))
        .send()
        .await?;
    assert_eq!(substring.status(), reqwest::StatusCode::OK);
    let substring_body = substring.json::<Value>().await?;
    assert_eq!(substring_body["total"], 1);
    assert_eq!(substring_body["items"][0]["id"], hidden_movie_id);

    let viewer_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="SearchTest", Device="Mac", DeviceId="search-viewer", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    let viewer_token = viewer_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let denied = client
        .get(format!("{base_url}/Search/Hints"))
        .query(&[("api_key", viewer_token.as_str()), ("SearchTerm", "银河")])
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::OK);
    assert_eq!(denied.json::<Value>().await?["TotalRecordCount"], 0);
    assert_ne!(admin.id, viewer.id);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn search_returns_series_by_default_but_preserves_explicit_episode_search()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let root = temp_dir.path().join("Shows");
    let season_dir = root.join("Target Show/Season 01");
    tokio::fs::create_dir_all(&season_dir).await?;
    for episode in 1..=2 {
        tokio::fs::write(
            season_dir.join(format!("Target.Show.S01E0{episode}.mkv")),
            b"episode",
        )
        .await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;

    // Make every hierarchy level match the same query so the regression test
    // proves that filtering happens before pagination, not in presentation.
    sqlx::query(
        "UPDATE media_items
         SET title = 'Target Show', sort_title = 'target show', original_title = 'Target Show'
         WHERE library_id = ?",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
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
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="SearchTest", Device="Mac", DeviceId="search-series", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();

    let emby_items = client
        .get(format!("{base_url}/Users/{}/Items", admin.id))
        .query(&[
            ("api_key", token.as_str()),
            ("SearchTerm", "Target Show"),
            ("Recursive", "true"),
        ])
        .send()
        .await?;
    assert_eq!(emby_items.status(), reqwest::StatusCode::OK);
    let emby_items_body = emby_items.json::<Value>().await?;
    assert_eq!(emby_items_body["TotalRecordCount"], 1);
    assert_eq!(emby_items_body["Items"][0]["Type"], "Series");

    let hints = client
        .get(format!("{base_url}/Search/Hints"))
        .query(&[("api_key", token.as_str()), ("SearchTerm", "Target Show")])
        .send()
        .await?;
    assert_eq!(hints.status(), reqwest::StatusCode::OK);
    let hints_body = hints.json::<Value>().await?;
    assert_eq!(hints_body["TotalRecordCount"], 1);
    assert_eq!(hints_body["SearchHints"][0]["Type"], "Series");

    let lux_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let session = lux_login
        .headers()
        .get_all("set-cookie")
        .iter()
        .find_map(|value| {
            let value = value.to_str().ok()?;
            value.strip_prefix("lux_session=")?.split(';').next()
        })
        .ok_or("missing session")?;
    let lux = client
        .get(format!("{base_url}/api/v1/search?q=Target%20Show"))
        .header("Cookie", format!("lux_session={session}"))
        .send()
        .await?;
    assert_eq!(lux.status(), reqwest::StatusCode::OK);
    let lux_body = lux.json::<Value>().await?;
    assert_eq!(lux_body["total"], 1);
    assert_eq!(lux_body["items"][0]["itemType"], "SERIES");

    let episodes = client
        .get(format!("{base_url}/Users/{}/Items", admin.id))
        .query(&[
            ("api_key", token.as_str()),
            ("SearchTerm", "Target Show"),
            ("IncludeItemTypes", "Episode"),
            ("Recursive", "true"),
        ])
        .send()
        .await?;
    assert_eq!(episodes.status(), reqwest::StatusCode::OK);
    let episodes_body = episodes.json::<Value>().await?;
    assert_eq!(episodes_body["TotalRecordCount"], 2);
    assert!(
        episodes_body["Items"]
            .as_array()
            .is_some_and(|items| items.iter().all(|item| item["Type"] == "Episode"))
    );

    server.abort();
    Ok(())
}

/// Case matrix for the search contract (LUX-869): Chinese 2/3/4-character queries, Latin prefixes
/// and substrings, hyphens, multi-token input and aliases in both scripts. It pins the behaviour a
/// unified implementation has to keep on every backend.
#[tokio::test]
async fn search_matrix_matches_chinese_prefix_alias_and_punctuation()
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
    tokio::fs::write(root.join("银河护卫队.2024.mkv"), b"one").await?;
    tokio::fs::write(root.join("Hidden.Movie.2025.mkv"), b"two").await?;
    tokio::fs::write(root.join("Spider-Man.2024.mkv"), b"three").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let chinese_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE title = '银河护卫队'")
            .fetch_one(database.pool())
            .await?;
    let hidden_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE title = 'Hidden Movie'")
            .fetch_one(database.pool())
            .await?;
    let spider_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE title LIKE 'Spider%'")
            .fetch_one(database.pool())
            .await?;
    for (alias, language) in [("Guardians", "en"), ("星际守护者", "zh-CN")] {
        sqlx::query(
            "INSERT INTO item_aliases (id, item_id, alias, language, alias_normalized)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(&chinese_id)
        .bind(alias)
        .bind(language)
        .bind(alias)
        .execute(database.pool())
        .await?;
    }
    let alias_fts_row: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_search WHERE media_search MATCH 'Guardians' AND item_id = ?",
    )
    .bind(&chinese_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(alias_fts_row, 1);

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
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let session = login
        .headers()
        .get_all("set-cookie")
        .iter()
        .find_map(|value| {
            let value = value.to_str().ok()?;
            value.strip_prefix("lux_session=")?.split(';').next()
        })
        .ok_or("missing session")?
        .to_owned();

    // (query, expected item, case label)
    let cases: [(&str, &str, &str); 10] = [
        ("银河", "chinese", "two-character Chinese"),
        ("银河护", "chinese", "three-character Chinese"),
        ("银河护卫", "chinese", "four-character Chinese"),
        ("护卫队", "chinese", "Chinese suffix inside one token"),
        ("星际守护者", "chinese", "Chinese alias"),
        ("Guardians", "chinese", "Latin alias"),
        ("Hid", "hidden", "Latin prefix"),
        ("idden", "hidden", "Latin substring"),
        ("Spider-Man", "spider", "Latin hyphenated title"),
        ("Spider Man", "spider", "multi-token input"),
    ];
    for (query, expected, label) in cases {
        let response = client
            .get(format!(
                "{base_url}/api/v1/search?q={}",
                percent_encode(query)
            ))
            .header("Cookie", format!("lux_session={session}"))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK, "query {label}");
        let body = response.json::<Value>().await?;
        let ids = body["items"]
            .as_array()
            .ok_or("missing items")?
            .iter()
            .filter_map(|item| item["id"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        let expected = match expected {
            "chinese" => &chinese_id,
            "hidden" => &hidden_id,
            _ => &spider_id,
        };
        assert_eq!(body["total"], 1, "query {label}");
        assert_eq!(&ids, &[expected.clone()], "query {label}");
    }

    let empty = client
        .get(format!("{base_url}/api/v1/search?q=nothing-matches-this"))
        .header("Cookie", format!("lux_session={session}"))
        .send()
        .await?;
    assert_eq!(empty.status(), reqwest::StatusCode::OK);
    assert_eq!(empty.json::<Value>().await?["total"], 0);

    server.abort();
    Ok(())
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}
