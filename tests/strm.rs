use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn strm_sources_store_first_non_empty_line_without_network_access()
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
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(
        root.join("Remote.Movie.2024.strm"),
        "\u{feff}\n  \n https://media.example.test/video?id=7&token=secret \nignored\n",
    )
    .await?;
    tokio::fs::write(root.join("Empty.Movie.2025.strm"), b"\n \n").await?;
    LibraryService::new(database.clone())
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    let report = LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    assert_eq!(report.discovered_files, 2);

    let stored: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT mi.title, ms.source_kind, ms.external_url
         FROM media_items mi JOIN media_sources ms ON ms.item_id = mi.id
         ORDER BY mi.title",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[1].1, "STRM_URL");
    assert_eq!(
        stored[1].2.as_deref(),
        Some("https://media.example.test/video?id=7&token=secret")
    );
    assert_eq!(stored[0].2, None);

    let remote_item_id: String =
        sqlx::query_scalar("SELECT mi.id FROM media_items mi WHERE mi.title = 'Remote Movie'")
            .fetch_one(database.pool())
            .await?;
    let remote_source_id: String =
        sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
            .bind(&remote_item_id)
            .fetch_one(database.pool())
            .await?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::new();
    let login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="StrmTest", Device="Mac", DeviceId="strm-admin", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();
    let playback = client
        .get(format!(
            "http://{address}/Items/{remote_item_id}/PlaybackInfo"
        ))
        .query(&[("api_key", token.as_str())])
        .send()
        .await?;
    assert_eq!(playback.status(), reqwest::StatusCode::OK);
    let body = playback.json::<Value>().await?;
    assert_eq!(body["MediaSources"][0]["Protocol"], "Http");
    assert_eq!(body["MediaSources"][0]["IsRemote"], true);
    assert_eq!(body["MediaSources"][0]["SupportsDirectPlay"], true);
    assert_eq!(
        body["MediaSources"][0]["DirectStreamUrl"],
        "https://media.example.test/video?id=7&token=secret"
    );

    let no_redirect_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let senplayer_stream = no_redirect_client
        .get(format!(
            "http://{address}/emby/videos/{remote_item_id}/stream.mkv%3FMediaSourceId={remote_source_id}&X-Emby-Token={token}"
        ))
        .send()
        .await?;
    assert_eq!(
        senplayer_stream.status(),
        reqwest::StatusCode::TEMPORARY_REDIRECT
    );
    assert_eq!(
        senplayer_stream.headers()[reqwest::header::LOCATION],
        "https://media.example.test/video?id=7&token=secret"
    );

    let unmatched_video_path = no_redirect_client
        .get(format!(
            "http://{address}/Videos/{remote_item_id}/original.strm"
        ))
        .query(&[
            ("MediaSourceId", remote_source_id.as_str()),
            ("api_key", token.as_str()),
        ])
        .send()
        .await?;
    assert_eq!(
        unmatched_video_path.status(),
        reqwest::StatusCode::TEMPORARY_REDIRECT
    );
    assert_eq!(
        unmatched_video_path.headers()[reqwest::header::LOCATION],
        "https://media.example.test/video?id=7&token=secret"
    );

    let missing_source_video_path = no_redirect_client
        .get(format!(
            "http://{address}/Videos/{remote_item_id}/original.strm"
        ))
        .query(&[
            ("MediaSourceId", "00000000-0000-0000-0000-000000000000"),
            ("api_key", token.as_str()),
        ])
        .send()
        .await?;
    assert_eq!(
        missing_source_video_path.status(),
        reqwest::StatusCode::NOT_FOUND
    );

    let source_id_items = no_redirect_client
        .get(format!("http://{address}/Items"))
        .query(&[
            ("Ids", remote_source_id.as_str()),
            ("Fields", "Path,MediaSources"),
            ("Limit", "1"),
            ("api_key", token.as_str()),
        ])
        .send()
        .await?;
    assert_eq!(source_id_items.status(), reqwest::StatusCode::OK);
    let source_id_body = source_id_items.json::<Value>().await?;
    assert_eq!(source_id_body["TotalRecordCount"], 1);
    assert_eq!(source_id_body["Items"][0]["Id"], remote_item_id);
    assert_eq!(
        source_id_body["Items"][0]["MediaSources"][0]["Id"],
        remote_source_id
    );
    assert_eq!(
        source_id_body["Items"][0]["MediaSources"][0]["Path"],
        "https://media.example.test/video?id=7&token=secret"
    );
    server.abort();
    Ok(())
}
