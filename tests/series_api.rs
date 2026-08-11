use std::time::Duration;

use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService, metadata::MetadataEnricher, scanner::LibraryScanner,
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

#[tokio::test]
async fn emby_series_seasons_episodes_and_next_up_return_hierarchy_and_user_state()
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
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let root = temp_dir.path().join("Shows");
    let season_dir = root.join("Example Show/Season 01");
    tokio::fs::create_dir_all(&season_dir).await?;
    for episode in 1..=3 {
        tokio::fs::write(
            season_dir.join(format!("Example.Show.S01E0{episode}.mkv")),
            b"episode",
        )
        .await?;
    }
    tokio::fs::write(
        season_dir.join("Example.Show.S01E01-thumb.jpg"),
        b"episode-thumbnail",
    )
    .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;
    MetadataEnricher::new(database.clone())
        .enrich_series_library(library.id)
        .await?;
    let series_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'SERIES'")
            .fetch_one(database.pool())
            .await?;
    let season_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'SEASON'")
            .fetch_one(database.pool())
            .await?;
    let episode_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'EPISODE' AND episode_number = 1",
    )
    .fetch_one(database.pool())
    .await?;
    let episode_thumb_id: String =
        sqlx::query_scalar("SELECT id FROM item_images WHERE item_id = ? AND image_type = 'THUMB'")
            .bind(&episode_id)
            .fetch_one(database.pool())
            .await?;
    let played_episode_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'EPISODE' AND episode_number = 2",
    )
    .fetch_one(database.pool())
    .await?;
    sqlx::query(
        "UPDATE media_items
         SET original_title = ?, premiere_date = ?, last_air_date = ?, status = ?,
             original_language = ?, provider_ids_json = ?
         WHERE id = ?",
    )
    .bind("Rick and Morty")
    .bind("2013-12-02")
    .bind("2025-05-25")
    .bind("Ended")
    .bind("en")
    .bind(r#"{"tmdb":"60625"}"#)
    .bind(&series_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_item_state
         (user_id, item_id, position_ticks, is_played, is_favorite, play_count, last_played_at)
         VALUES (?, ?, 12345, 0, 1, 2, 200)",
    )
    .bind(admin.id.to_string())
    .bind(&episode_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_item_state
         (user_id, item_id, position_ticks, is_played, is_favorite, play_count, last_played_at)
         VALUES (?, ?, 999, 1, 0, 4, 100)",
    )
    .bind(admin.id.to_string())
    .bind(&played_episode_id)
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
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="SeriesTest", Device="Mac", DeviceId="series-admin", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();
    let headers = [("X-Emby-Token", token.as_str())];

    let emby_series_detail = client
        .get(format!("{base_url}/Users/{}/Items/{series_id}", admin.id))
        .header(headers[0].0, headers[0].1)
        .send()
        .await?;
    assert_eq!(emby_series_detail.status(), reqwest::StatusCode::OK);
    let emby_series_detail_body: Value = emby_series_detail.json().await?;
    assert_eq!(emby_series_detail_body["SortName"], "example show");
    assert_eq!(emby_series_detail_body["PremiereDate"], "2013-12-02");
    assert_eq!(emby_series_detail_body["ProviderIds"]["Tmdb"], "60625");

    let seasons = client
        .get(format!("{base_url}/Shows/{series_id}/Seasons?Limit=10"))
        .header(headers[0].0, headers[0].1)
        .send()
        .await?;
    assert_eq!(seasons.status(), reqwest::StatusCode::OK);
    let seasons_body: Value = seasons.json().await?;
    assert_eq!(seasons_body["TotalRecordCount"], 1);
    assert_eq!(seasons_body["Items"][0]["Type"], "Season");
    assert_eq!(seasons_body["Items"][0]["IsFolder"], true);
    assert_eq!(seasons_body["Items"][0]["ParentId"], series_id);
    assert_eq!(seasons_body["Items"][0]["IndexNumber"], 1);
    assert_eq!(seasons_body["Items"][0]["ChildCount"], 3);

    let children = client
        .get(format!(
            "{base_url}/Users/{}/Items?ParentId={series_id}&IncludeItemTypes=Season&Limit=10",
            admin.id
        ))
        .header(headers[0].0, headers[0].1)
        .send()
        .await?;
    assert_eq!(children.status(), reqwest::StatusCode::OK);
    let children_body: Value = children.json().await?;
    assert_eq!(children_body["TotalRecordCount"], 1);
    assert_eq!(children_body["Items"][0]["Id"], season_id);

    let episodes_by_parent = client
        .get(format!(
            "{base_url}/Items?ParentId={series_id}&IncludeItemTypes=Episode&Recursive=true&Limit=10"
        ))
        .header(headers[0].0, headers[0].1)
        .send()
        .await?;
    assert_eq!(episodes_by_parent.status(), reqwest::StatusCode::OK);
    let episodes_by_parent_body: Value = episodes_by_parent.json().await?;
    assert_eq!(episodes_by_parent_body["TotalRecordCount"], 3);
    assert_eq!(episodes_by_parent_body["Items"][0]["Type"], "Episode");

    let grouped_latest = client
        .get(format!(
            "{base_url}/Users/{}/Items/Latest?IncludeItemTypes=Episode&GroupItems=true&Limit=10",
            admin.id
        ))
        .header(headers[0].0, headers[0].1)
        .send()
        .await?;
    assert_eq!(grouped_latest.status(), reqwest::StatusCode::OK);
    let grouped_latest_body: Value = grouped_latest.json().await?;
    assert_eq!(grouped_latest_body.as_array().map(Vec::len), Some(1));
    assert_eq!(grouped_latest_body[0]["Id"], series_id);
    assert_eq!(grouped_latest_body[0]["Type"], "Series");
    assert_eq!(grouped_latest_body[0]["ChildCount"], 3);

    let default_latest = client
        .get(format!(
            "{base_url}/Users/{}/Items/Latest?Limit=10",
            admin.id
        ))
        .header(headers[0].0, headers[0].1)
        .send()
        .await?;
    assert_eq!(default_latest.status(), reqwest::StatusCode::OK);
    let default_latest_body: Value = default_latest.json().await?;
    assert!(default_latest_body.as_array().is_some_and(|items| {
        items
            .iter()
            .all(|item| matches!(item["Type"].as_str(), Some("Movie" | "Series")))
    }));

    let homepage_items = client
        .get(format!(
            "{base_url}/Users/{}/Items?ExcludeItemTypes=Audio,Book,MusicVideo,Game,MusicAlbum,Photo&StartIndex=0&Limit=50&Fields=PremiereDate,ProductionYear,CommunityRating,ChildCount,CanDownload,Chapters",
            admin.id
        ))
        .header(headers[0].0, headers[0].1)
        .send()
        .await?;
    assert_eq!(homepage_items.status(), reqwest::StatusCode::OK);
    let homepage_items_body: Value = homepage_items.json().await?;
    assert_eq!(homepage_items_body["TotalRecordCount"], 1);
    assert!(
        homepage_items_body["Items"]
            .as_array()
            .is_some_and(|items| {
                !items.is_empty()
                    && items.iter().all(|item| item["Type"] == "CollectionFolder")
                    && items
                        .iter()
                        .all(|item| item["Id"] == library.id.to_string())
                    && items
                        .iter()
                        .all(|item| item["RecursiveItemCount"] == item["ChildCount"])
            })
    );

    let recursive_filtered_items = client
        .get(format!(
            "{base_url}/Users/{}/Items?Recursive=true&ExcludeItemTypes=Season,Episode&StartIndex=0&Limit=50",
            admin.id
        ))
        .header(headers[0].0, headers[0].1)
        .send()
        .await?;
    assert_eq!(recursive_filtered_items.status(), reqwest::StatusCode::OK);
    let recursive_filtered_body: Value = recursive_filtered_items.json().await?;
    assert_eq!(recursive_filtered_body["TotalRecordCount"], 1);
    assert!(
        recursive_filtered_body["Items"]
            .as_array()
            .is_some_and(|items| { items.iter().all(|item| item["Type"] == "Series") })
    );

    let library_latest = client
        .get(format!(
            "{base_url}/Users/{}/Items/Latest?ParentId={}&Limit=10",
            admin.id, library.id
        ))
        .header(headers[0].0, headers[0].1)
        .send()
        .await?;
    assert_eq!(library_latest.status(), reqwest::StatusCode::OK);
    let library_latest_body: Value = library_latest.json().await?;
    assert!(library_latest_body.as_array().is_some_and(|items| {
        !items.is_empty()
            && items
                .iter()
                .all(|item| matches!(item["Type"].as_str(), Some("Movie" | "Series")))
    }));

    let series_latest = client
        .get(format!(
            "{base_url}/Users/{}/Items/Latest?ParentId={series_id}&Limit=10",
            admin.id
        ))
        .header(headers[0].0, headers[0].1)
        .send()
        .await?;
    assert_eq!(series_latest.status(), reqwest::StatusCode::OK);
    let series_latest_body: Value = series_latest.json().await?;
    assert_eq!(series_latest_body.as_array().map(Vec::len), Some(1));
    assert_eq!(series_latest_body[0]["Type"], "Season");

    let latest_children = client
        .get(format!(
            "{base_url}/Users/{}/Items/Latest?ParentId={series_id}&IncludeItemTypes=Episode&GroupItems=false&Limit=10",
            admin.id
        ))
        .header(headers[0].0, headers[0].1)
        .send()
        .await?;
    assert_eq!(latest_children.status(), reqwest::StatusCode::OK);
    let latest_children_body: Value = latest_children.json().await?;
    assert_eq!(latest_children_body.as_array().map(Vec::len), Some(3));
    assert_eq!(latest_children_body[0]["Type"], "Episode");

    let episodes = client
        .get(format!(
            "{base_url}/Shows/{series_id}/Episodes?StartIndex=1&Limit=1"
        ))
        .header(headers[0].0, headers[0].1)
        .send()
        .await?;
    assert_eq!(episodes.status(), reqwest::StatusCode::OK);
    let episodes_body: Value = episodes.json().await?;
    assert_eq!(episodes_body["TotalRecordCount"], 3);
    assert_eq!(episodes_body["Items"].as_array().map(Vec::len), Some(1));
    assert_eq!(episodes_body["Items"][0]["Index"], 2);
    assert_eq!(episodes_body["Items"][0]["IndexNumber"], 2);
    assert_eq!(episodes_body["Items"][0]["ParentIndexNumber"], 1);
    assert_eq!(episodes_body["Items"][0]["ParentId"], season_id);
    assert_eq!(episodes_body["Items"][0]["SeasonId"], season_id);
    assert_eq!(episodes_body["Items"][0]["SeriesId"], series_id);
    assert_eq!(episodes_body["Items"][0]["UserData"]["Played"], true);
    assert_eq!(episodes_body["Items"][0]["UserData"]["PlayCount"], 4);

    let next_up = client
        .get(format!("{base_url}/Users/{}/Items/NextUp", admin.id))
        .header(headers[0].0, headers[0].1)
        .send()
        .await?;
    assert_eq!(next_up.status(), reqwest::StatusCode::OK);
    let next_up_body: Value = next_up.json().await?;
    assert_eq!(next_up_body["TotalRecordCount"], 1);
    assert_eq!(next_up_body["Items"][0]["Id"], episode_id);
    assert_eq!(
        next_up_body["Items"][0]["UserData"]["PlaybackPositionTicks"],
        12345
    );
    assert_eq!(next_up_body["Items"][0]["UserData"]["IsFavorite"], true);

    let shows_next_up = client
        .get(format!(
            "{base_url}/Shows/NextUp?UserId={}&Limit=10",
            admin.id
        ))
        .header(headers[0].0, headers[0].1)
        .send()
        .await?;
    assert_eq!(shows_next_up.status(), reqwest::StatusCode::OK);
    let shows_next_up_body: Value = shows_next_up.json().await?;
    assert_eq!(shows_next_up_body["TotalRecordCount"], 1);
    assert_eq!(shows_next_up_body["Items"][0]["Id"], episode_id);

    let web_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(web_login.status(), reqwest::StatusCode::OK);
    let web_cookie = cookie_pair(web_login.headers());
    let web_series = client
        .get(format!("{base_url}/api/v1/items/{series_id}"))
        .header(COOKIE, &web_cookie)
        .send()
        .await?;
    assert_eq!(web_series.status(), reqwest::StatusCode::OK);
    let web_series_body = web_series.json::<Value>().await?;
    assert_eq!(web_series_body["originalTitle"], "Rick and Morty");
    assert_eq!(web_series_body["premiereDate"], "2013-12-02");
    assert_eq!(web_series_body["lastAirDate"], "2025-05-25");
    assert_eq!(web_series_body["status"], "Ended");
    assert_eq!(web_series_body["originalLanguage"], "en");
    assert_eq!(web_series_body["providerIds"]["tmdb"], "60625");
    assert_eq!(web_series_body["seasonCount"], 1);
    assert_eq!(web_series_body["episodeCount"], 3);
    let web_library_items = client
        .get(format!(
            "{base_url}/api/v1/libraries/{}/items?itemType=SERIES&pageSize=24",
            library.id
        ))
        .header(COOKIE, &web_cookie)
        .send()
        .await?;
    assert_eq!(web_library_items.status(), reqwest::StatusCode::OK);
    let web_library_items_body = web_library_items.json::<Value>().await?;
    assert_eq!(web_library_items_body["items"][0]["episodeCount"], 3);
    let web_seasons = client
        .get(format!(
            "{base_url}/api/v1/items/{series_id}/children?itemType=SEASON"
        ))
        .header(COOKIE, &web_cookie)
        .send()
        .await?;
    assert_eq!(web_seasons.status(), reqwest::StatusCode::OK);
    let web_seasons_body = web_seasons.json::<Value>().await?;
    assert_eq!(web_seasons_body["total"], 1);
    assert_eq!(web_seasons_body["items"][0]["parentId"], series_id);
    assert_eq!(web_seasons_body["items"][0]["seriesId"], series_id);
    assert_eq!(web_seasons_body["items"][0]["parentIndexNumber"], 1);
    assert_eq!(web_seasons_body["items"][0]["episodeCount"], 3);
    let web_episodes = client
        .get(format!(
            "{base_url}/api/v1/items/{series_id}/children?itemType=EPISODE&seasonId={season_id}"
        ))
        .header(COOKIE, &web_cookie)
        .send()
        .await?;
    assert_eq!(web_episodes.status(), reqwest::StatusCode::OK);
    let web_episodes_body = web_episodes.json::<Value>().await?;
    assert_eq!(web_episodes_body["total"], 3);
    assert_eq!(web_episodes_body["items"][0]["id"], episode_id);
    assert_eq!(web_episodes_body["items"][0]["parentId"], season_id);
    assert_eq!(web_episodes_body["items"][0]["seriesId"], series_id);
    assert_eq!(web_episodes_body["items"][0]["parentIndexNumber"], 1);
    assert_eq!(web_episodes_body["items"][0]["indexNumber"], 1);
    assert_eq!(
        web_episodes_body["items"][0]["imageTags"]["thumb"],
        episode_thumb_id
    );
    assert_eq!(
        web_episodes_body["items"][0]["userData"]["isFavorite"],
        true
    );
    assert_eq!(web_episodes_body["items"][0]["userData"]["isPlayed"], false);
    assert_eq!(web_episodes_body["items"][1]["userData"]["isPlayed"], true);

    sqlx::query(
        "INSERT INTO media_items
         (id, library_id, item_type, parent_id, series_id, season_number, episode_number,
          title, sort_title, original_title, identification_status, identity_key)
         SELECT ?, library_id, item_type, parent_id, series_id, season_number, episode_number,
                title, sort_title || ' [4K]', original_title, identification_status,
                identity_key || ':4k'
         FROM media_items
         WHERE id = ?",
    )
    .bind("episode-1-4k")
    .bind(&episode_id)
    .execute(database.pool())
    .await?;

    let web_seasons_after_variant = client
        .get(format!(
            "{base_url}/api/v1/items/{series_id}/children?itemType=SEASON"
        ))
        .header(COOKIE, &web_cookie)
        .send()
        .await?;
    assert_eq!(web_seasons_after_variant.status(), reqwest::StatusCode::OK);
    let web_seasons_after_variant_body = web_seasons_after_variant.json::<Value>().await?;
    assert_eq!(
        web_seasons_after_variant_body["items"][0]["episodeCount"],
        3
    );

    let web_home = client
        .get(format!("{base_url}/api/v1/home"))
        .header(COOKIE, &web_cookie)
        .send()
        .await?;
    assert_eq!(web_home.status(), reqwest::StatusCode::OK);
    let web_home_body = web_home.json::<Value>().await?;
    assert_eq!(
        web_home_body["libraries"][0]["latest"][0]["episodeCount"],
        3
    );

    let csrf = request_cookie(&web_cookie, "lux_csrf");
    let missing_csrf = client
        .put(format!("{base_url}/api/v1/items/{episode_id}/played"))
        .header(COOKIE, &web_cookie)
        .json(&json!({ "played": true }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);
    let missing_favorite_csrf = client
        .put(format!("{base_url}/api/v1/items/{episode_id}/favorite"))
        .header(COOKIE, &web_cookie)
        .json(&json!({ "favorite": true }))
        .send()
        .await?;
    assert_eq!(
        missing_favorite_csrf.status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let mark_played = client
        .put(format!("{base_url}/api/v1/items/{episode_id}/played"))
        .header(COOKIE, &web_cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "played": true }))
        .send()
        .await?;
    assert_eq!(mark_played.status(), reqwest::StatusCode::NO_CONTENT);
    let playback = client
        .get(format!("{base_url}/api/v1/items/{episode_id}/playback"))
        .header(COOKIE, &web_cookie)
        .send()
        .await?;
    assert_eq!(playback.status(), reqwest::StatusCode::OK);
    assert_eq!(playback.json::<Value>().await?["isPlayed"], true);

    let viewer_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="SeriesTest", Device="Mac", DeviceId="series-viewer", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    let viewer_token = viewer_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let denied = client
        .get(format!("{base_url}/Shows/{series_id}/Seasons"))
        .header("X-Emby-Token", viewer_token)
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::NOT_FOUND);

    sqlx::query(
        "WITH RECURSIVE sequence(value) AS (
             SELECT 1
             UNION ALL
             SELECT value + 1 FROM sequence WHERE value < 20000
         )
         INSERT INTO media_items (
             id, library_id, item_type, parent_id, series_id,
             season_number, episode_number, title, sort_title,
             identification_status, identity_key, has_available_source
         )
         SELECT printf('bulk-episode-%05d', value), ?, 'EPISODE', ?, ?,
                2, value, printf('Bulk Episode %05d', value),
                printf('Bulk Episode %05d', value), 'LOCAL_CONFIRMED',
                printf('bulk-episode:%05d', value), 1
         FROM sequence",
    )
    .bind(library.id.to_string())
    .bind(&season_id)
    .bind(&series_id)
    .execute(database.pool())
    .await?;
    let large_page = tokio::time::timeout(Duration::from_secs(1), async {
        client
            .get(format!(
                "{base_url}/Shows/{series_id}/Episodes?StartIndex=3&Limit=1"
            ))
            .header("X-Emby-Token", &token)
            .send()
            .await
    })
    .await
    .map_err(|_| "episode page materialized the complete series")??;
    assert_eq!(large_page.status(), reqwest::StatusCode::OK);
    let large_page_body = large_page.json::<Value>().await?;
    assert_eq!(large_page_body["TotalRecordCount"], 20003);
    assert_eq!(large_page_body["Items"].as_array().map(Vec::len), Some(1));
    assert_eq!(large_page_body["Items"][0]["Id"], "bulk-episode-00001");

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

fn request_cookie(cookie: &str, name: &str) -> String {
    cookie
        .split("; ")
        .find_map(|part| {
            let (key, value) = part.split_once('=')?;
            (key == name).then(|| value.to_owned())
        })
        .unwrap_or_default()
}
