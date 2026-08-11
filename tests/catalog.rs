use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService, metadata::MetadataEnricher, nfo::MovieNfoMetadataStore,
        scanner::LibraryScanner, setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

#[tokio::test]
async fn lux_and_emby_catalogs_list_page_and_show_movie_details()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let users = UserStore::new(database.clone())?;
    let viewer = users
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let series_library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    let first_dir = media_root.join("Alpha Movie (2020)");
    let second_dir = media_root.join("Beta Movie (2021)");
    tokio::fs::create_dir_all(&first_dir).await?;
    tokio::fs::create_dir_all(&second_dir).await?;
    tokio::fs::write(first_dir.join("Alpha.Movie.2020.mkv"), b"alpha").await?;
    tokio::fs::write(first_dir.join("poster.jpg"), b"alpha-poster").await?;
    tokio::fs::write(first_dir.join("fanart.jpg"), b"alpha-fanart").await?;
    tokio::fs::write(
        first_dir.join("movie.nfo"),
        r#"<movie><rating>8.1</rating><votes>123</votes><tagline>本地标语</tagline><premiered>2020-01-02</premiered><runtime>126</runtime><status>Released</status><language>zh</language><mpaa>PG-13</mpaa><country>中国</country><genre>动作</genre><studio>本地影业</studio><tmdbid>12345</tmdbid><director tmdbid="88">导演甲</director><writer tmdbid="99">编剧甲</writer><trailer>https://example.com/trailer</trailer></movie>"#,
    )
    .await?;
    tokio::fs::write(second_dir.join("Beta.Movie.2021.mp4"), b"beta").await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    MetadataEnricher::new(database.clone())
        .with_movie_nfo_store(MovieNfoMetadataStore::new(database.clone()))
        .enrich_movie_library(library.id)
        .await?;
    let removed_version = first_dir.join("Alpha.Movie.2020.2160p.mkv");
    tokio::fs::write(&removed_version, b"alpha-2160p").await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    tokio::fs::remove_file(&removed_version).await?;
    let removal_scan = LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    assert_eq!(removal_scan.marked_missing, 1);
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE sort_title = 'alpha movie'")
            .fetch_one(database.pool())
            .await?;
    let beta_item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE sort_title = 'beta movie'")
            .fetch_one(database.pool())
            .await?;
    let alpha_source_id: String =
        sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    sqlx::query("UPDATE media_items SET added_at = ? WHERE id = ?")
        .bind(300_i64)
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_items SET added_at = ? WHERE id = ?")
        .bind(200_i64)
        .bind(&beta_item_id)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_items SET premiere_date = ?, rating = ? WHERE id = ?")
        .bind("2021-01-01")
        .bind(8.2_f64)
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_items SET premiere_date = ?, rating = ? WHERE id = ?")
        .bind("2020-01-01")
        .bind(6.5_f64)
        .bind(&beta_item_id)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_sources SET duration_ticks = ? WHERE item_id = ?")
        .bind(2_000_000_000_i64)
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_sources SET duration_ticks = ? WHERE item_id = ?")
        .bind(2_000_000_000_i64)
        .bind(&beta_item_id)
        .execute(database.pool())
        .await?;
    let alpha_poster_id: String = sqlx::query_scalar(
        "SELECT id FROM item_images WHERE item_id = ? AND image_type = 'POSTER'",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    let alpha_fanart_id: String = sqlx::query_scalar(
        "SELECT id FROM item_images WHERE item_id = ? AND image_type = 'FANART'",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, is_favorite)
         VALUES (?, ?, 1)
         ON CONFLICT(user_id, item_id) DO UPDATE SET is_favorite = 1",
    )
    .bind(admin.id.to_string())
    .bind(&beta_item_id)
    .execute(database.pool())
    .await?;

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
    let client = reqwest::Client::new();

    let admin_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="LuxTest", Device="Mac", DeviceId="admin-device", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_token = admin_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing admin token")?
        .to_owned();

    let cover_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cover_session = cookie_value(cover_login.headers(), "lux_session");
    let cover_csrf = cookie_value(cover_login.headers(), "lux_csrf");
    let cover_upload = client
        .put(format!(
            "{base_url}/api/v1/admin/libraries/{}/cover",
            library.id
        ))
        .header(
            COOKIE,
            format!("lux_session={cover_session}; lux_csrf={cover_csrf}"),
        )
        .header("X-CSRF-Token", &cover_csrf)
        .header("Content-Type", "image/png")
        .body(PNG_1X1)
        .send()
        .await?;
    assert_eq!(cover_upload.status(), reqwest::StatusCode::OK);

    let views = client
        .get(format!("{base_url}/Users/{}/Views", admin.id))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(views.status(), reqwest::StatusCode::OK);
    let views_body: Value = views.json().await?;
    assert_eq!(views_body["TotalRecordCount"], 2);
    let movie_view = views_body["Items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["Id"] == library.id.to_string())
        })
        .ok_or("missing movie library view")?;
    assert_eq!(movie_view["CollectionType"], "movies");
    assert_eq!(movie_view["ChildCount"], 2);
    assert_eq!(movie_view["PrimaryImageItemId"], library.id.to_string());
    let cover_tag = movie_view["ImageTags"]["Primary"]
        .as_str()
        .ok_or("missing movie library cover tag")?;
    assert!(!cover_tag.is_empty());
    let shows_view = views_body["Items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["Id"] == series_library.id.to_string())
        })
        .ok_or("missing series library view")?;
    assert_eq!(shows_view["CollectionType"], "tvshows");
    assert_eq!(shows_view["ChildCount"], 0);

    let root = client
        .get(format!("{base_url}/Users/{}/Items/Root", admin.id))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(root.status(), reqwest::StatusCode::OK);
    let root_body: Value = root.json().await?;
    assert_eq!(root_body["Id"], admin.id.to_string());
    assert_eq!(root_body["Type"], "Folder");
    assert_eq!(root_body["IsFolder"], true);
    assert_eq!(root_body["ChildCount"], 2);

    let modern_root = client
        .get(format!("{base_url}/Items/Root?userId={}", admin.id))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(modern_root.status(), reqwest::StatusCode::OK);
    assert_eq!(
        modern_root.json::<Value>().await?["Id"],
        admin.id.to_string()
    );

    let root_children = client
        .get(format!(
            "{base_url}/Users/{}/Items?ParentId={}&IncludeItemTypes=CollectionFolder&Limit=10",
            admin.id, admin.id
        ))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(root_children.status(), reqwest::StatusCode::OK);
    let root_children_body: Value = root_children.json().await?;
    assert_eq!(root_children_body["TotalRecordCount"], 2);
    assert_eq!(root_children_body["Items"][0]["Type"], "CollectionFolder");
    assert_eq!(root_children_body["Items"][0]["IsFolder"], true);

    let filtered_root_children = client
        .get(format!(
            "{base_url}/Users/{}/Items?IncludeItemTypes=CollectionFolder&Limit=10",
            admin.id
        ))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(filtered_root_children.status(), reqwest::StatusCode::OK);
    assert_eq!(
        filtered_root_children.json::<Value>().await?["TotalRecordCount"],
        2
    );

    let emby_library_detail = client
        .get(format!(
            "{base_url}/Users/{}/Items/{}?EnableUserData=true&Fields=CollectionType,ChildCount",
            admin.id, library.id
        ))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(emby_library_detail.status(), reqwest::StatusCode::OK);
    let emby_library_detail_body: Value = emby_library_detail.json().await?;
    assert_eq!(emby_library_detail_body["Id"], library.id.to_string());
    assert_eq!(emby_library_detail_body["Name"], "Movies");
    assert_eq!(emby_library_detail_body["Type"], "CollectionFolder");
    assert_eq!(emby_library_detail_body["IsFolder"], true);
    assert_eq!(emby_library_detail_body["CollectionType"], "movies");
    assert_eq!(emby_library_detail_body["ChildCount"], 2);

    let emby_cover = client
        .get(format!("{base_url}/Items/{}/Images/Primary", library.id))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(emby_cover.status(), reqwest::StatusCode::OK);
    assert_eq!(emby_cover.headers()["content-type"], "image/png");
    assert_eq!(emby_cover.bytes().await?.as_ref(), PNG_1X1);

    let emby_capability_cover = client
        .get(format!(
            "{base_url}/emby/Items/{}/Images/Primary?tag={cover_tag}",
            library.id
        ))
        .send()
        .await?;
    assert_eq!(emby_capability_cover.status(), reqwest::StatusCode::OK);
    assert_eq!(emby_capability_cover.bytes().await?.as_ref(), PNG_1X1);

    let emby_page = client
        .get(format!(
            "{base_url}/Users/{}/Items?ParentId={}&Limit=1",
            admin.id, library.id
        ))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(emby_page.status(), reqwest::StatusCode::OK);
    let emby_page_body: Value = emby_page.json().await?;
    assert_eq!(emby_page_body["TotalRecordCount"], 2);
    assert_eq!(emby_page_body["StartIndex"], 0);
    assert_eq!(emby_page_body["Items"].as_array().map(Vec::len), Some(1));
    assert_eq!(emby_page_body["Items"][0]["Type"], "Movie");
    assert_eq!(emby_page_body["Items"][0]["Name"], "Alpha Movie");
    assert_eq!(emby_page_body["Items"][0]["PrimaryImageItemId"], item_id);
    assert_eq!(
        emby_page_body["Items"][0]["ImageTags"]["Primary"],
        alpha_poster_id
    );
    assert_eq!(
        emby_page_body["Items"][0]["BackdropImageTags"][0],
        alpha_fanart_id
    );
    assert_eq!(
        emby_page_body["Items"][0]["ParentId"],
        library.id.to_string()
    );
    assert_eq!(
        emby_page_body["Items"][0]["MediaSources"][0]["Container"],
        "mkv"
    );

    let filtered_by_item_id = client
        .get(format!(
            "{base_url}/Items?Ids={item_id}&Fields=Path,MediaSources&Limit=1"
        ))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(filtered_by_item_id.status(), reqwest::StatusCode::OK);
    let filtered_by_item_id_body: Value = filtered_by_item_id.json().await?;
    assert_eq!(filtered_by_item_id_body["TotalRecordCount"], 1);
    assert_eq!(
        filtered_by_item_id_body["Items"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(filtered_by_item_id_body["Items"][0]["Id"], item_id);

    let filtered_by_multiple_item_ids = client
        .get(format!(
            "{base_url}/Items?Ids={item_id},{beta_item_id}&Limit=10"
        ))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(
        filtered_by_multiple_item_ids.status(),
        reqwest::StatusCode::OK
    );
    let filtered_by_multiple_item_ids_body: Value = filtered_by_multiple_item_ids.json().await?;
    assert_eq!(filtered_by_multiple_item_ids_body["TotalRecordCount"], 2);
    assert_eq!(
        filtered_by_multiple_item_ids_body["Items"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );

    let filtered_by_media_source_id = client
        .get(format!(
            "{base_url}/Items?Ids={alpha_source_id}&Fields=Path,MediaSources&Limit=1"
        ))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(
        filtered_by_media_source_id.status(),
        reqwest::StatusCode::OK
    );
    let filtered_by_media_source_id_body: Value = filtered_by_media_source_id.json().await?;
    assert_eq!(filtered_by_media_source_id_body["TotalRecordCount"], 1);
    assert_eq!(
        filtered_by_media_source_id_body["Items"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(filtered_by_media_source_id_body["Items"][0]["Id"], item_id);
    assert_eq!(
        filtered_by_media_source_id_body["Items"][0]["MediaSources"][0]["Id"],
        alpha_source_id
    );

    let filtered_by_unknown_id = client
        .get(format!(
            "{base_url}/Items?Ids=00000000-0000-0000-0000-000000000000&Limit=1"
        ))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(filtered_by_unknown_id.status(), reqwest::StatusCode::OK);
    let filtered_by_unknown_id_body: Value = filtered_by_unknown_id.json().await?;
    assert_eq!(filtered_by_unknown_id_body["TotalRecordCount"], 0);
    assert_eq!(
        filtered_by_unknown_id_body["Items"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    let emby_compact_page = client
        .get(format!(
            "{base_url}/Users/{}/Items?ParentId={}&Limit=1&Fields=BasicSyncInfo,Container",
            admin.id, library.id
        ))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(emby_compact_page.status(), reqwest::StatusCode::OK);
    let emby_compact_page_body: Value = emby_compact_page.json().await?;
    assert_eq!(
        emby_compact_page_body["Items"].as_array().map(Vec::len),
        Some(1)
    );
    assert!(
        emby_compact_page_body["Items"][0]
            .get("MediaSources")
            .is_none()
    );

    let emby_latest = client
        .get(format!(
            "{base_url}/Users/{}/Items/Latest?ParentId={}&Limit=2",
            admin.id, library.id
        ))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(emby_latest.status(), reqwest::StatusCode::OK);
    let emby_latest_body: Value = emby_latest.json().await?;
    let emby_latest_items = emby_latest_body
        .as_array()
        .ok_or("Emby latest items should be returned as a bare array")?;
    assert_eq!(emby_latest_items.len(), 2);
    assert_eq!(emby_latest_items[0]["Name"], "Alpha Movie");
    assert_eq!(emby_latest_items[0]["ParentId"], library.id.to_string());
    assert_eq!(emby_latest_items[0]["PrimaryImageItemId"], item_id);
    assert_eq!(
        emby_latest_items[0]["ImageTags"]["Primary"],
        alpha_poster_id
    );

    let detail = client
        .get(format!("{base_url}/Items/{item_id}?api_key={admin_token}"))
        .send()
        .await?;
    assert_eq!(detail.status(), reqwest::StatusCode::OK);
    let detail_body: Value = detail.json().await?;
    assert_eq!(detail_body["Id"], item_id);
    assert_eq!(detail_body["Name"], "Alpha Movie");
    assert_eq!(detail_body["ProductionYear"], 2020);
    assert_eq!(detail_body["PrimaryImageItemId"], item_id);
    assert_eq!(detail_body["ImageTags"]["Primary"], alpha_poster_id);

    let viewer_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="LuxTest", Device="Mac", DeviceId="viewer-device", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    let viewer_token = viewer_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let forbidden = client
        .get(format!("{base_url}/Users/{}/Items", admin.id))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    let viewer_web_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    let viewer_cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(viewer_web_login.headers(), "lux_session"),
        cookie_value(viewer_web_login.headers(), "lux_csrf")
    );
    let viewer_home = client
        .get(format!("{base_url}/api/v1/home"))
        .header(COOKIE, &viewer_cookies)
        .send()
        .await?;
    assert_eq!(viewer_home.status(), reqwest::StatusCode::OK);
    let viewer_home_body: Value = viewer_home.json().await?;
    assert_eq!(viewer_home_body["recentlyAddedTotal"], 0);
    assert_eq!(
        viewer_home_body["libraries"].as_array().map(Vec::len),
        Some(0)
    );

    let web_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(web_login.headers(), "lux_session"),
        cookie_value(web_login.headers(), "lux_csrf")
    );
    let lux_page = client
        .get(format!(
            "{base_url}/api/v1/libraries/{}/items?pageSize=1",
            library.id
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(lux_page.status(), reqwest::StatusCode::OK);
    let lux_page_body: Value = lux_page.json().await?;
    assert_eq!(lux_page_body["total"], 2);
    assert_eq!(lux_page_body["pageSize"], 1);
    assert_eq!(lux_page_body["items"][0]["title"], "Alpha Movie");

    let recent_page = client
        .get(format!(
            "{base_url}/api/v1/libraries/{}/items?pageSize=1&sort_by=DateCreated&sort_order=Descending",
            library.id
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(recent_page.status(), reqwest::StatusCode::OK);
    let recent_page_body: Value = recent_page.json().await?;
    assert_eq!(recent_page_body["items"][0]["title"], "Alpha Movie");

    let release_page = client
        .get(format!(
            "{base_url}/api/v1/libraries/{}/items?pageSize=1&sort_by=PremiereDate&sort_order=Descending",
            library.id
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(release_page.status(), reqwest::StatusCode::OK);
    let release_page_body: Value = release_page.json().await?;
    assert_eq!(release_page_body["items"][0]["title"], "Alpha Movie");

    let rating_page = client
        .get(format!(
            "{base_url}/api/v1/libraries/{}/items?pageSize=1&sort_by=CommunityRating&sort_order=Descending",
            library.id
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(rating_page.status(), reqwest::StatusCode::OK);
    let rating_page_body: Value = rating_page.json().await?;
    assert_eq!(rating_page_body["items"][0]["title"], "Alpha Movie");

    sqlx::query("UPDATE media_items SET rating = NULL WHERE id = ?")
        .bind(&beta_item_id)
        .execute(database.pool())
        .await?;
    let unrated_page = client
        .get(format!(
            "{base_url}/api/v1/libraries/{}/items?pageSize=2&sort_by=CommunityRating&sort_order=Descending",
            library.id
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(unrated_page.status(), reqwest::StatusCode::OK);
    let unrated_page_body: Value = unrated_page.json().await?;
    assert_eq!(unrated_page_body["items"][0]["title"], "Alpha Movie");
    assert_eq!(unrated_page_body["items"][1]["title"], "Beta Movie");

    let favorite_page = client
        .get(format!(
            "{base_url}/api/v1/libraries/{}/items?pageSize=1&is_favorite=true",
            library.id
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(favorite_page.status(), reqwest::StatusCode::OK);
    let favorite_page_body: Value = favorite_page.json().await?;
    assert_eq!(favorite_page_body["total"], 1);
    assert_eq!(favorite_page_body["items"][0]["title"], "Beta Movie");

    let favorites = client
        .get(format!("{base_url}/api/v1/favorites?pageSize=1"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(favorites.status(), reqwest::StatusCode::OK);
    let favorites_body: Value = favorites.json().await?;
    assert_eq!(favorites_body["total"], 1);
    assert_eq!(favorites_body["items"][0]["title"], "Beta Movie");

    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, position_ticks, last_played_at)
         VALUES (?, ?, ?, ?), (?, ?, ?, ?)
         ON CONFLICT(user_id, item_id) DO UPDATE SET
             position_ticks = excluded.position_ticks,
             is_played = 0,
             last_played_at = excluded.last_played_at",
    )
    .bind(admin.id.to_string())
    .bind(&item_id)
    .bind(1_700_000_000_i64)
    .bind(400_i64)
    .bind(admin.id.to_string())
    .bind(&beta_item_id)
    .bind(600_000_000_i64)
    .bind(500_i64)
    .execute(database.pool())
    .await?;
    let home = client
        .get(format!("{base_url}/api/v1/home"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(home.status(), reqwest::StatusCode::OK);
    let home_body: Value = home.json().await?;
    assert_eq!(home_body["recentlyAddedTotal"], 2);
    assert_eq!(home_body["continueWatchingTotal"], 1);
    assert_eq!(home_body["continueWatching"][0]["id"], item_id);
    assert_eq!(home_body["continueWatching"][0]["title"], "Alpha Movie");
    assert_eq!(
        home_body["continueWatching"][0]["imageTags"]["poster"],
        alpha_poster_id
    );
    assert_eq!(
        home_body["continueWatching"][0]["imageTags"]["fanart"],
        alpha_fanart_id
    );
    assert_eq!(
        home_body["continueWatching"][0]["userData"]["positionTicks"],
        1_700_000_000_i64
    );
    assert_eq!(home_body["recentlyAdded"].as_array().map(Vec::len), Some(2));
    assert_eq!(home_body["recentlyAdded"][0]["title"], "Alpha Movie");
    assert_eq!(home_body["recentlyAdded"][1]["title"], "Beta Movie");
    assert_eq!(home_body["libraries"].as_array().map(Vec::len), Some(2));
    let home_movie_library = home_body["libraries"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["id"] == library.id.to_string())
        })
        .ok_or("missing movie library in home")?;
    assert_eq!(
        home_movie_library["latest"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(home_movie_library["latest"][0]["title"], "Alpha Movie");
    assert_eq!(
        home_movie_library["latest"][0]["imageTags"]["poster"],
        alpha_poster_id
    );
    assert_eq!(home_movie_library["latest"][1]["title"], "Beta Movie");
    assert_eq!(home_body["recommended"].as_array().map(Vec::len), Some(2));
    assert!(
        home_body["recommended"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["title"] == "Beta Movie"))
    );

    let emby_resume = client
        .get(format!("{base_url}/Users/{}/Items/Resume", admin.id))
        .header("X-Emby-Token", &admin_token)
        .query(&[("Limit", "10")])
        .send()
        .await?;
    assert_eq!(emby_resume.status(), reqwest::StatusCode::OK);
    let emby_resume_body: Value = emby_resume.json().await?;
    assert_eq!(emby_resume_body["TotalRecordCount"], 1);
    assert_eq!(emby_resume_body["Items"][0]["Id"], item_id);
    assert_eq!(emby_resume_body["Items"][0]["PrimaryImageItemId"], item_id);
    assert_eq!(
        emby_resume_body["Items"][0]["ImageTags"]["Primary"],
        alpha_poster_id
    );

    let lux_detail = client
        .get(format!("{base_url}/api/v1/items/{item_id}"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(lux_detail.status(), reqwest::StatusCode::OK);
    let lux_detail_body: Value = lux_detail.json().await?;
    assert_eq!(lux_detail_body["id"], item_id);
    assert_eq!(lux_detail_body["productionYear"], 2020);
    assert_eq!(lux_detail_body["rating"], 8.1);
    assert_eq!(lux_detail_body["ratingSource"], "NFO");
    assert_eq!(lux_detail_body["providerIds"]["tmdb"], "12345");
    assert_eq!(lux_detail_body["nfo"]["tagline"], "本地标语");
    assert_eq!(lux_detail_body["nfo"]["genres"][0], "动作");
    assert_eq!(lux_detail_body["nfo"]["directors"][0]["name"], "导演甲");
    assert_eq!(lux_detail_body["nfo"]["writers"][0]["name"], "编剧甲");
    assert_eq!(
        lux_detail_body["nfo"]["trailers"][0],
        "https://example.com/trailer"
    );
    assert_eq!(
        lux_detail_body["mediaSources"].as_array().map(Vec::len),
        Some(1)
    );

    assert_ne!(admin.id, viewer.id);
    server.abort();
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
