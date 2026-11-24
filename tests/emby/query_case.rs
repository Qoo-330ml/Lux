//! Emby 查询参数大小写兼容（LUX-914）。
//!
//! 标准 Emby 服务端是 ASP.NET，查询字符串绑定大小写不敏感。第三方客户端因此按自己的习惯拼参数：
//! Entryway 1.0.18 的 Emby 适配层（`data/provider/emby`）对 `Users/{userId}/Items` 系列一律发
//! camelCase（`parentId`、`includeItemTypes`、`limit`、`startIndex`、`fields`…），Hills/Yamby 发
//! PascalCase。Lux 的 `EmbyItemsQuery` 原先只认 PascalCase，serde 会把 camelCase 的键静默丢弃：
//! 媒体库列表退化成「全服所有库混在一起、类型过滤与分页全部失效」，而且不报错，客户端无从察觉。
//!
//! 这些用例把「camelCase 写法与 PascalCase 写法等价」固定成 Emby 适配层的合同。覆盖的是客户端
//! 实际会发的 camelCase 拼写，不是任意大小写。

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

const TICKS_PER_SECOND: i64 = 10_000_000;
const MOVIE_RUNTIME_SECONDS: i64 = 7_200;

struct TestServer {
    base_url: String,
    client: reqwest::Client,
    admin_token: String,
    admin_id: String,
    movie_library_id: String,
    series_library_id: String,
    database: Database,
    _temp_dir: tempfile::TempDir,
    _server: tokio::task::JoinHandle<Result<(), std::io::Error>>,
}

/// Emby 的条目 id 是 UUID 的十进制写法；测试里的自造 id 直接透传。
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
            r#"Emby Client="QueryCaseTest", Device="Mac", DeviceId="query-case-device", Version="1""#,
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
    let libraries = LibraryService::new(database.clone());
    let movie_library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let series_library = libraries
        .create_library("Shows", LibraryKind::Series, false)
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
    Ok(TestServer {
        base_url,
        client,
        admin_token,
        admin_id: admin.id.to_string(),
        movie_library_id: movie_library.id.to_string(),
        series_library_id: series_library.id.to_string(),
        database,
        _temp_dir: temp_dir,
        _server: server,
    })
}

async fn insert_item(
    server: &TestServer,
    library_id: &str,
    item_id: &str,
    item_type: &str,
    title: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO media_items
         (id, library_id, item_type, title, sort_title, identification_status,
          identity_key, has_available_source, runtime_ticks)
         VALUES (?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', ?, 1, ?)",
    )
    .bind(item_id)
    .bind(library_id)
    .bind(item_type)
    .bind(title)
    .bind(title)
    .bind(format!("emby-query-case:{item_id}"))
    .bind(MOVIE_RUNTIME_SECONDS * TICKS_PER_SECOND)
    .execute(server.database.pool())
    .await?;
    Ok(())
}

/// 三部电影 + 一部剧集：足以区分「按库过滤」「按类型过滤」和「分页」。
async fn seed_catalog(server: &TestServer) -> Result<(), Box<dyn std::error::Error>> {
    for (item_id, title) in [
        ("movie-alpha", "Alpha Movie"),
        ("movie-bravo", "Bravo Movie"),
        ("movie-charlie", "Charlie Movie"),
    ] {
        insert_item(server, &server.movie_library_id, item_id, "MOVIE", title).await?;
    }
    insert_item(
        server,
        &server.series_library_id,
        "series-one",
        "SERIES",
        "Delta Show",
    )
    .await?;
    Ok(())
}

async fn fetch(
    server: &TestServer,
    path: &str,
    params: &[(&str, &str)],
) -> Result<(reqwest::StatusCode, Value), Box<dyn std::error::Error>> {
    let response = server
        .client
        .get(format!("{}{path}", server.base_url))
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

fn item_titles(body: &Value) -> Vec<String> {
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

fn item_ids(body: &Value) -> Vec<String> {
    let mut ids: Vec<String> = body["Items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["Id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// 一项查询参数的三元组：camelCase 写法、PascalCase 写法、取值。
type ParamPair<'a> = (&'a str, &'a str, &'a str);

fn spellings<'a>(params: &'a [ParamPair<'a>], camel: bool) -> Vec<(&'a str, &'a str)> {
    params
        .iter()
        .map(|(camel_name, pascal_name, value)| {
            if camel {
                (*camel_name, *value)
            } else {
                (*pascal_name, *value)
            }
        })
        .collect()
}

/// `/Users/{userId}/Items` 的每一项 camelCase 参数都必须和 PascalCase 写法得到完全相同的响应。
///
/// 覆盖 Entryway 实际会发的键（`parentId`、`includeItemTypes`、`limit`、`startIndex`、`fields`、
/// `recursive`、`genres`、`years`、`isPlayed`、`isFavorite`、`enableTotalRecordCount`），根路径与
/// `/emby` 前缀各跑一遍：路由前缀不该改变参数解析。
#[tokio::test]
async fn item_list_accepts_camel_case_query_parameters() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    seed_catalog(&server).await?;
    // 客户端用的是登录响应里的 `User.Id`，Lux 返回的就是 UUID 原文。
    let user_id = server.admin_id.clone();
    let movies = emby_public_id(&server.movie_library_id);
    let shows = emby_public_id(&server.series_library_id);
    let cases: Vec<Vec<ParamPair<'_>>> = vec![
        vec![("parentId", "ParentId", movies.as_str())],
        vec![("includeItemTypes", "IncludeItemTypes", "Series")],
        vec![("includeItemTypes", "IncludeItemTypes", "Movie")],
        vec![("limit", "Limit", "2"), ("startIndex", "StartIndex", "1")],
        vec![("fields", "Fields", "MediaStreams")],
        vec![("recursive", "Recursive", "true")],
        vec![("recursive", "Recursive", "false")],
        vec![("isFavorite", "IsFavorite", "true")],
        vec![("isPlayed", "IsPlayed", "false")],
        vec![("genres", "Genres", "No Such Genre")],
        vec![("years", "Years", "1999")],
        vec![("enableTotalRecordCount", "EnableTotalRecordCount", "false")],
        vec![("groupItems", "GroupItems", "false")],
        vec![("excludeItemTypes", "ExcludeItemTypes", "Series")],
        vec![("ids", "Ids", "movie-alpha")],
        vec![
            ("parentId", "ParentId", shows.as_str()),
            ("includeItemTypes", "IncludeItemTypes", "Series"),
        ],
    ];
    for prefix in ["", "/emby"] {
        for (index, params) in cases.iter().enumerate() {
            let camel = spellings(params, true);
            let pascal = spellings(params, false);
            let (camel_status, camel_body) =
                fetch(&server, &format!("{prefix}/Users/{user_id}/Items"), &camel).await?;
            let (pascal_status, pascal_body) =
                fetch(&server, &format!("{prefix}/Users/{user_id}/Items"), &pascal).await?;
            assert_eq!(
                camel_status, pascal_status,
                "camelCase {camel:?} 与 PascalCase {pascal:?} 的状态码不同（prefix {prefix:?}, case #{index}）"
            );
            assert_eq!(
                camel_body, pascal_body,
                "camelCase {camel:?} 与 PascalCase {pascal:?} 的响应不同（prefix {prefix:?}, case #{index}）"
            );
        }
    }
    Ok(())
}

/// 只改大小写就等价还不够：`parentId` + `includeItemTypes` 必须真的把列表缩小到目标库和目标类型。
#[tokio::test]
async fn camel_case_parent_id_and_type_filter_scope_the_list()
-> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    seed_catalog(&server).await?;
    // 客户端用的是登录响应里的 `User.Id`，Lux 返回的就是 UUID 原文。
    let user_id = server.admin_id.clone();
    let movies = emby_public_id(&server.movie_library_id);
    let shows = emby_public_id(&server.series_library_id);

    let (status, all) = fetch(&server, &format!("/Users/{user_id}/Items"), &[]).await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(all["TotalRecordCount"], 4);
    assert_eq!(
        item_titles(&all),
        [
            "Alpha Movie".to_owned(),
            "Bravo Movie".to_owned(),
            "Charlie Movie".to_owned(),
            "Delta Show".to_owned()
        ]
    );

    let (status, movies_only) = fetch(
        &server,
        &format!("/Users/{user_id}/Items"),
        &[("parentId", movies.as_str())],
    )
    .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(movies_only["TotalRecordCount"], 3);
    assert_eq!(
        item_titles(&movies_only),
        [
            "Alpha Movie".to_owned(),
            "Bravo Movie".to_owned(),
            "Charlie Movie".to_owned()
        ]
    );

    let (status, scoped) = fetch(
        &server,
        &format!("/Users/{user_id}/Items"),
        &[("parentId", shows.as_str()), ("includeItemTypes", "Series")],
    )
    .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(scoped["TotalRecordCount"], 1);
    assert_eq!(item_titles(&scoped), ["Delta Show".to_owned()]);

    // 类型过滤单独生效：电影库里没有剧集，`includeItemTypes=Series` 必须是空页而不是整库。
    let (status, empty) = fetch(
        &server,
        &format!("/Users/{user_id}/Items"),
        &[
            ("parentId", movies.as_str()),
            ("includeItemTypes", "Series"),
        ],
    )
    .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(empty["TotalRecordCount"], 0);
    assert_eq!(item_titles(&empty), Vec::<String>::new());
    Ok(())
}

/// `limit`/`startIndex` 必须真的翻页，并且和 PascalCase 一样接受服务端的边界校验。
#[tokio::test]
async fn camel_case_paging_parameters_page_and_validate() -> Result<(), Box<dyn std::error::Error>>
{
    let server = spawn_server().await?;
    seed_catalog(&server).await?;
    // 客户端用的是登录响应里的 `User.Id`，Lux 返回的就是 UUID 原文。
    let user_id = server.admin_id.clone();
    let movies = emby_public_id(&server.movie_library_id);

    let mut paged = Vec::new();
    for start in ["0", "1", "2"] {
        let (status, body) = fetch(
            &server,
            &format!("/Users/{user_id}/Items"),
            &[
                ("parentId", movies.as_str()),
                ("limit", "1"),
                ("startIndex", start),
            ],
        )
        .await?;
        assert_eq!(status, reqwest::StatusCode::OK);
        assert_eq!(body["TotalRecordCount"], 3);
        assert_eq!(
            item_ids(&body).len(),
            1,
            "limit=1 必须只返回一条（startIndex={start}）"
        );
        paged.extend(item_ids(&body));
    }
    paged.sort();
    paged.dedup();
    assert_eq!(paged.len(), 3, "三页必须覆盖三部电影而不是重复同一页");

    // 超出服务端上限与非法偏移和 PascalCase 一样是 400，不能被静默忽略。
    for params in [
        vec![("limit", "2001")],
        vec![("startIndex", "-1")],
        vec![("startIndex", "abc")],
    ] {
        let (status, _) = fetch(&server, &format!("/Users/{user_id}/Items"), &params).await?;
        assert_eq!(
            status,
            reqwest::StatusCode::BAD_REQUEST,
            "camelCase {params:?} 必须和 PascalCase 一样被拒绝"
        );
    }
    Ok(())
}

/// `fields` 决定列表里做什么投影：`MediaStreams` 只有点名时才出现在条目顶层。
#[tokio::test]
async fn camel_case_fields_selects_the_list_projection() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    seed_catalog(&server).await?;
    // 客户端用的是登录响应里的 `User.Id`，Lux 返回的就是 UUID 原文。
    let user_id = server.admin_id.clone();
    let movies = emby_public_id(&server.movie_library_id);

    let (status, plain) = fetch(
        &server,
        &format!("/Users/{user_id}/Items"),
        &[("parentId", movies.as_str())],
    )
    .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    for item in plain["Items"].as_array().ok_or("missing Items")? {
        assert!(
            item.get("MediaStreams").is_none(),
            "没有 Fields 时列表不应带上顶层 MediaStreams：{item}"
        );
    }

    let (status, projected) = fetch(
        &server,
        &format!("/Users/{user_id}/Items"),
        &[("parentId", movies.as_str()), ("fields", "MediaStreams")],
    )
    .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    let items = projected["Items"].as_array().ok_or("missing Items")?;
    assert_eq!(items.len(), 3);
    for item in items {
        assert!(
            item.get("MediaStreams").is_some(),
            "fields=MediaStreams 必须带上顶层 MediaStreams：{item}"
        );
    }
    Ok(())
}

/// 详情端点（`/Items/{id}` 与 `/Users/{userId}/Items/{id}`）的 `fields` 同样不区分大小写。
#[tokio::test]
async fn item_detail_accepts_camel_case_fields() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    seed_catalog(&server).await?;
    // 客户端用的是登录响应里的 `User.Id`，Lux 返回的就是 UUID 原文。
    let user_id = server.admin_id.clone();
    let item_id = emby_public_id("movie-alpha");

    for path in [
        format!("/Items/{item_id}"),
        format!("/Users/{user_id}/Items/{item_id}"),
    ] {
        let (status, camel) = fetch(&server, &path, &[("fields", "MediaStreams")]).await?;
        assert_eq!(status, reqwest::StatusCode::OK);
        let (status, pascal) = fetch(&server, &path, &[("Fields", "MediaStreams")]).await?;
        assert_eq!(status, reqwest::StatusCode::OK);
        assert_eq!(
            camel, pascal,
            "{path}: camelCase fields 必须和 PascalCase 得到同一条目"
        );
    }
    Ok(())
}

/// `Items/{id}/Similar` 的 `limit`/`fields` 同样大小写不敏感：负 `limit` 是 400，而不是被忽略。
#[tokio::test]
async fn similar_accepts_camel_case_limit_and_fields() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    seed_catalog(&server).await?;
    let item_id = emby_public_id("movie-alpha");

    let (status, _) = fetch(
        &server,
        &format!("/Items/{item_id}/Similar"),
        &[("limit", "-1")],
    )
    .await?;
    assert_eq!(
        status,
        reqwest::StatusCode::BAD_REQUEST,
        "camelCase limit=-1 必须和 PascalCase 一样被拒绝"
    );

    let (status, camel) = fetch(
        &server,
        &format!("/Items/{item_id}/Similar"),
        &[("limit", "1"), ("fields", "MediaStreams")],
    )
    .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    let (status, pascal) = fetch(
        &server,
        &format!("/Items/{item_id}/Similar"),
        &[("Limit", "1"), ("Fields", "MediaStreams")],
    )
    .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(camel, pascal, "Similar 的 limit/fields 两种写法必须等价");
    Ok(())
}

/// `HideFromResume` 原先只认 `Hide=`；小写 `hide=` 会被当成「没给」，直接 400。
#[tokio::test]
async fn hide_from_resume_accepts_camel_case_hide() -> Result<(), Box<dyn std::error::Error>> {
    let server = spawn_server().await?;
    seed_catalog(&server).await?;
    // 客户端用的是登录响应里的 `User.Id`，Lux 返回的就是 UUID 原文。
    let user_id = server.admin_id.clone();
    let item_id = emby_public_id("movie-alpha");

    for hide in ["hide", "Hide"] {
        let response = server
            .client
            .post(format!(
                "{}/Users/{user_id}/Items/{item_id}/HideFromResume",
                server.base_url
            ))
            .query(&[(hide, "true")])
            .header("X-Emby-Token", &server.admin_token)
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::OK,
            "`{hide}=true` 必须被识别为隐藏"
        );
        let body: Value = response.json().await?;
        assert_eq!(
            body["HideFromResume"],
            json!(true),
            "`{hide}=true` 的响应体"
        );
    }
    Ok(())
}
