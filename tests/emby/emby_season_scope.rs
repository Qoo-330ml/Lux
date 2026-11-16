//! Emby `Shows/{seriesId}/Episodes` 的季作用域（LUX-915）。
//!
//! 客户端会带 `SeasonId` / `seasonId` 选中某一季；Entryway 1.0.18 的 `zy5.E` 就是
//! `Shows/{seriesId}/Episodes?userId=&seasonId=&fields=&limit=&startIndex=` 这样的形状。
//! Lux 的季过滤走 `mi.parent_id = ?`，因此除了「别名大小写」之外还有两件事必须固定：
//!
//! 1. 合法季 id 必须真的把结果缩到该季，而不是返回整部剧；
//! 2. 已失效的季 id（剧集库重扫后客户端留着旧选择器）保持 LUX-914 之前的既有兼容行为——
//!    回退到整部剧的集列表，而不是把页面清空（VidHub 兼容，`src/api/emby_catalog.rs`
//!    的 `Err(CatalogError::LibraryNotFound) if season_id.is_some()` 分支）。

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::net::TcpListener;

const STATUS_CONFIRMED: &str = "LOCAL_CONFIRMED";

struct TestServer {
    base_url: String,
    client: reqwest::Client,
    admin_token: String,
    series_id: String,
    season_one_id: String,
    season_two_id: String,
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
            r#"Emby Client="SeasonScopeTest", Device="Mac", DeviceId="season-scope", Version="1""#,
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

/// 一部两季、每季两集的剧：`S01E01`/`S01E02`/`S02E01`/`S02E02`。
async fn spawn_server() -> Result<TestServer, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let library = LibraryService::new(database.clone())
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let library_id = library.id.to_string();
    let series_id = "series-scope".to_owned();
    let season_one_id = "season-scope-1".to_owned();
    let season_two_id = "season-scope-2".to_owned();

    sqlx::query(
        "INSERT INTO media_items
         (id, library_id, item_type, title, sort_title, identification_status,
          identity_key, has_available_source)
         VALUES (?, ?, 'SERIES', 'Scope Show', 'Scope Show', ?, 'season-scope:series', 1)",
    )
    .bind(&series_id)
    .bind(&library_id)
    .bind(STATUS_CONFIRMED)
    .execute(database.pool())
    .await?;
    for (season_id, season_number) in [(&season_one_id, 1_i64), (&season_two_id, 2_i64)] {
        sqlx::query(
            "INSERT INTO media_items
             (id, library_id, item_type, parent_id, series_id, season_number, title, sort_title,
              identification_status, identity_key, has_available_source)
             VALUES (?, ?, 'SEASON', ?, ?, ?, ?, ?, ?, ?, 1)",
        )
        .bind(season_id)
        .bind(&library_id)
        .bind(&series_id)
        .bind(&series_id)
        .bind(season_number)
        .bind(format!("Season {season_number:02}"))
        .bind(format!("Season {season_number:02}"))
        .bind(STATUS_CONFIRMED)
        .bind(format!("season-scope:season:{season_number}"))
        .execute(database.pool())
        .await?;
    }
    for (season_id, season_number) in [(&season_one_id, 1_i64), (&season_two_id, 2_i64)] {
        for episode_number in [1_i64, 2_i64] {
            let episode_id = format!("episode-scope-s{season_number}e{episode_number}");
            let title = format!("Scope S{season_number:02}E{episode_number:02}");
            sqlx::query(
                "INSERT INTO media_items
                 (id, library_id, item_type, parent_id, series_id, season_number,
                  episode_number, title, sort_title, identification_status, identity_key,
                  has_available_source)
                 VALUES (?, ?, 'EPISODE', ?, ?, ?, ?, ?, ?, ?, ?, 1)",
            )
            .bind(&episode_id)
            .bind(&library_id)
            .bind(season_id)
            .bind(&series_id)
            .bind(season_number)
            .bind(episode_number)
            .bind(&title)
            .bind(&title)
            .bind(STATUS_CONFIRMED)
            .bind(format!("season-scope:{episode_id}"))
            .execute(database.pool())
            .await?;
        }
    }

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
    Ok(TestServer {
        base_url,
        client,
        admin_token,
        series_id,
        season_one_id,
        season_two_id,
        library_id,
        database,
        _temp_dir: temp_dir,
        _server: server,
    })
}

async fn fetch_episodes(
    server: &TestServer,
    params: &[(&str, &str)],
) -> Result<(reqwest::StatusCode, Value), Box<dyn std::error::Error>> {
    let response = server
        .client
        .get(format!(
            "{}/Shows/{}/Episodes",
            server.base_url,
            emby_public_id(&server.series_id)
        ))
        .query(params)
        .header("X-Emby-Token", &server.admin_token)
        .send()
        .await?;
    let status = response.status();
    let body = if status.is_success() {
        response.json().await?
    } else {
        Value::Null
    };
    Ok((status, body))
}

fn episode_titles(body: &Value) -> Vec<String> {
    let mut titles: Vec<String> = body["Items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["Name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    titles.sort();
    titles
}

/// 不带季选择器时返回整部剧的四集，作为下面每个用例的基准。
#[tokio::test]
async fn episodes_without_a_season_selector_return_the_whole_show()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    let (status, body) = fetch_episodes(&server, &[]).await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["TotalRecordCount"], 4);
    assert_eq!(
        episode_titles(&body),
        [
            "Scope S01E01".to_owned(),
            "Scope S01E02".to_owned(),
            "Scope S02E01".to_owned(),
            "Scope S02E02".to_owned()
        ]
    );
    Ok(())
}

/// `SeasonId` 是 Emby 的规范写法：它必须真的把结果缩到该季（不是返回整部剧）。
#[tokio::test]
async fn season_id_scopes_the_episode_list() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    for (season_id, expected) in [
        (
            &server.season_one_id,
            ["Scope S01E01".to_owned(), "Scope S01E02".to_owned()],
        ),
        (
            &server.season_two_id,
            ["Scope S02E01".to_owned(), "Scope S02E02".to_owned()],
        ),
    ] {
        let (status, body) =
            fetch_episodes(&server, &[("SeasonId", &emby_public_id(season_id))]).await?;
        assert_eq!(status, reqwest::StatusCode::OK);
        assert_eq!(
            body["TotalRecordCount"], 2,
            "SeasonId={season_id} 必须只返回该季的两集"
        );
        assert_eq!(episode_titles(&body), expected);
    }
    Ok(())
}

/// Entryway 发的是 camelCase 的 `seasonId`，两种拼写必须给出同一份结果。
#[tokio::test]
async fn season_id_accepts_camel_case() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    let season_id = emby_public_id(&server.season_two_id);
    let (camel_status, camel) = fetch_episodes(&server, &[("seasonId", &season_id)]).await?;
    let (pascal_status, pascal) = fetch_episodes(&server, &[("SeasonId", &season_id)]).await?;
    assert_eq!(camel_status, reqwest::StatusCode::OK);
    assert_eq!(pascal_status, reqwest::StatusCode::OK);
    assert_eq!(camel, pascal, "seasonId 与 SeasonId 必须返回同一份集列表");
    assert_eq!(
        episode_titles(&camel),
        ["Scope S02E01".to_owned(), "Scope S02E02".to_owned()],
        "camelCase 的 seasonId 必须真的过滤"
    );
    Ok(())
}

/// 空的或 `null`/`undefined` 的季选择器按「没选季」处理，两种拼写一致。
#[tokio::test]
async fn blank_season_selector_means_every_season() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    for (name, value) in [
        ("SeasonId", ""),
        ("seasonId", ""),
        ("SeasonId", "null"),
        ("seasonId", "undefined"),
    ] {
        let (status, body) = fetch_episodes(&server, &[(name, value)]).await?;
        assert_eq!(status, reqwest::StatusCode::OK, "{name}={value}");
        assert_eq!(
            body["TotalRecordCount"], 4,
            "{name}={value} 必须回退到整部剧，而不是空页"
        );
    }
    Ok(())
}

/// 已失效的季 id（库重扫后客户端还留着旧选择器）维持既有 VidHub 兼容行为：回退到整部剧。
///
/// 这是 `emby_show_episodes` 的 `Err(CatalogError::LibraryNotFound) if season_id.is_some()`
/// 分支，不是季过滤失效——上面两条用例已经固定了合法 id 的行为。
#[tokio::test]
async fn stale_season_id_falls_back_to_the_whole_show() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    for (name, value) in [
        ("SeasonId", "999999999999999999999999999999999999"),
        ("seasonId", "999999999999999999999999999999999999"),
    ] {
        let (status, body) = fetch_episodes(&server, &[(name, value)]).await?;
        assert_eq!(status, reqwest::StatusCode::OK, "{name}={value}");
        assert_eq!(
            body["TotalRecordCount"], 4,
            "{name}={value} 必须回退到整部剧，而不是 404 或空页"
        );
    }
    Ok(())
}

/// 分页与季作用域叠加：`limit` 在季内生效，`TotalRecordCount` 是该季的集数。
#[tokio::test]
async fn season_scope_and_paging_combine() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    let season_id = emby_public_id(&server.season_one_id);
    let (status, first_page) = fetch_episodes(
        &server,
        &[
            ("SeasonId", &season_id),
            ("limit", "1"),
            ("startIndex", "0"),
        ],
    )
    .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(first_page["TotalRecordCount"], 2, "总数是季内集数");
    assert_eq!(episode_titles(&first_page), ["Scope S01E01".to_owned()]);

    let (status, second_page) = fetch_episodes(
        &server,
        &[
            ("SeasonId", &season_id),
            ("limit", "1"),
            ("startIndex", "1"),
        ],
    )
    .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(episode_titles(&second_page), ["Scope S01E02".to_owned()]);
    Ok(())
}

/// 季 id 指向另一部剧的季时，不能泄漏那个季的集，只能回退到本剧。
#[tokio::test]
async fn season_id_from_another_show_does_not_leak_episodes()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    let other_series = "series-other";
    let other_season = "season-other-1";
    let other_episode = "episode-other-1";
    sqlx::query(
        "INSERT INTO media_items
         (id, library_id, item_type, title, sort_title, identification_status,
          identity_key, has_available_source)
         VALUES (?, ?, 'SERIES', 'Other Show', 'Other Show', ?, 'season-scope:other-series', 1)",
    )
    .bind(other_series)
    .bind(&server.library_id)
    .bind(STATUS_CONFIRMED)
    .execute(server.database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_items
         (id, library_id, item_type, parent_id, series_id, season_number, title, sort_title,
          identification_status, identity_key, has_available_source)
         VALUES (?, ?, 'SEASON', ?, ?, 1, 'Season 01', 'Season 01', ?, 'season-scope:other-season', 1)",
    )
    .bind(other_season)
    .bind(&server.library_id)
    .bind(other_series)
    .bind(other_series)
    .bind(STATUS_CONFIRMED)
    .execute(server.database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_items
         (id, library_id, item_type, parent_id, series_id, season_number, episode_number,
          title, sort_title, identification_status, identity_key, has_available_source)
         VALUES (?, ?, 'EPISODE', ?, ?, 1, 1, 'Other S01E01', 'Other S01E01', ?,
                 'season-scope:other-episode', 1)",
    )
    .bind(other_episode)
    .bind(&server.library_id)
    .bind(other_season)
    .bind(other_series)
    .bind(STATUS_CONFIRMED)
    .execute(server.database.pool())
    .await?;

    let (status, body) =
        fetch_episodes(&server, &[("SeasonId", &emby_public_id(other_season))]).await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    let titles = episode_titles(&body);
    assert_eq!(
        titles,
        [
            "Scope S01E01".to_owned(),
            "Scope S01E02".to_owned(),
            "Scope S02E01".to_owned(),
            "Scope S02E02".to_owned()
        ],
        "别的剧的季 id 只能回退到本剧，不能把那个季的集带进来"
    );
    Ok(())
}

/// `/emby` 前缀与根路径的行为一致。
#[tokio::test]
async fn season_scope_is_the_same_under_the_emby_prefix() -> Result<(), Box<dyn std::error::Error>>
{
    let server = spawn_server().await?;
    let season_id = emby_public_id(&server.season_two_id);
    let response = server
        .client
        .get(format!(
            "{}/emby/Shows/{}/Episodes",
            server.base_url,
            emby_public_id(&server.series_id)
        ))
        .query(&[("seasonId", season_id.as_str())])
        .header("X-Emby-Token", &server.admin_token)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await?;
    assert_eq!(
        episode_titles(&body),
        ["Scope S02E01".to_owned(), "Scope S02E02".to_owned()]
    );
    Ok(())
}
