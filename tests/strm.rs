use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService, scanner::LibraryScanner, setup::SetupService,
        strm_playback::StrmPlaybackResolver,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn strm_sources_store_first_non_empty_line_and_returns_url_to_the_client()
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
    let remote_target =
        "http://media.example.test/video/剧集?id=7&title=第1集&token=fixture".to_owned();
    tokio::fs::write(
        root.join("Remote.Movie.2024.strm"),
        format!("\u{feff}\n  \n {remote_target} \nignored\n"),
    )
    .await?;
    tokio::fs::write(root.join("Empty.Movie.2025.strm"), b"\n \n").await?;
    tokio::fs::write(
        root.join("Path.Movie.2026.strm"),
        "targets/movie (4K).target\nignored\n",
    )
    .await?;
    tokio::fs::create_dir_all(root.join("targets")).await?;
    tokio::fs::write(root.join("targets/movie (4K).target"), b"local path media").await?;
    tokio::fs::write(
        root.join("Opaque.Movie.2027.strm"),
        "media-provider://library/item/7\n",
    )
    .await?;
    LibraryService::new(database.clone())
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    let report = LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    assert_eq!(report.discovered_files, 4);

    let stored: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT mi.title, ms.source_kind, ms.external_url, ms.strm_target_kind
         FROM media_items mi JOIN media_sources ms ON ms.item_id = mi.id
         ORDER BY mi.title",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(stored.len(), 4);
    let remote = stored
        .iter()
        .find(|row| row.0 == "Remote Movie")
        .ok_or("missing remote source")?;
    assert_eq!(remote.1, "STRM_URL");
    assert_eq!(remote.3.as_deref(), Some("URL"));
    assert_eq!(remote.2.as_deref(), Some(remote_target.as_str()));
    let path = stored
        .iter()
        .find(|row| row.0 == "Path Movie")
        .ok_or("missing path source")?;
    assert_eq!(path.1, "STRM_URL");
    assert_eq!(path.2.as_deref(), Some("targets/movie (4K).target"));
    assert_eq!(path.3.as_deref(), Some("PATH"));
    let opaque = stored
        .iter()
        .find(|row| row.0 == "Opaque Movie")
        .ok_or("missing opaque source")?;
    assert_eq!(opaque.1, "STRM_URL");
    assert_eq!(opaque.2.as_deref(), Some("media-provider://library/item/7"));
    assert_eq!(opaque.3.as_deref(), Some("OPAQUE"));
    let empty = stored
        .iter()
        .find(|row| row.0 == "Empty Movie")
        .ok_or("missing empty source")?;
    assert_eq!(empty.1, "STRM_URL");
    assert_eq!(empty.2, None);
    assert_eq!(empty.3.as_deref(), Some("EMPTY"));

    let remote_item_id: String =
        sqlx::query_scalar("SELECT mi.id FROM media_items mi WHERE mi.title = 'Remote Movie'")
            .fetch_one(database.pool())
            .await?;
    let remote_source_id: String =
        sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
            .bind(&remote_item_id)
            .fetch_one(database.pool())
            .await?;
    let path_item_id: String =
        sqlx::query_scalar("SELECT mi.id FROM media_items mi WHERE mi.title = 'Path Movie'")
            .fetch_one(database.pool())
            .await?;
    let path_source_id: String =
        sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
            .bind(&path_item_id)
            .fetch_one(database.pool())
            .await?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let proxy_listener = TcpListener::bind("127.0.0.1:0").await?;
    let proxy_address = proxy_listener.local_addr()?;
    let forwarded_user_agents = Arc::new(Mutex::new(Vec::new()));
    let forwarded_user_agents_for_proxy = Arc::clone(&forwarded_user_agents);
    let proxy_server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = proxy_listener.accept().await else {
                break;
            };
            let user_agents = Arc::clone(&forwarded_user_agents_for_proxy);
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = stream.read(&mut buffer).await.ok()?;
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).ok()?;
                if let Some(user_agent) = request.lines().find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
                        .map(|(_, value)| value.trim().to_owned())
                }) {
                    user_agents.lock().ok()?.push(user_agent);
                }
                let response = if request.contains("/video/") {
                    "HTTP/1.1 302 Found\r\nLocation: http://media.example.test/cdn.mkv\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nContent-Range: bytes 0-0/1\r\nConnection: close\r\n\r\nX"
                };
                stream.write_all(response.as_bytes()).await.ok()?;
                Some(())
            });
        }
    });
    let resolver =
        StrmPlaybackResolver::new_with_proxy_for_tests(format!("http://{proxy_address}"))?;
    let app = app_with_state(
        AppState::ready(config, database.clone(), setup, auth, emby_auth)
            .with_strm_playback_resolver(resolver),
    );
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
    let login_body = login.json::<Value>().await?;
    let token = login_body["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();
    let user_id = login_body["User"]["Id"]
        .as_str()
        .ok_or("missing user id")?
        .to_owned();

    let popcorn_detail = client
        .get(format!(
            "http://{address}/emby/Users/{user_id}/Items/{remote_item_id}?Fields=ShareLevel"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(popcorn_detail.status(), reqwest::StatusCode::OK);
    let popcorn_detail_body = popcorn_detail.json::<Value>().await?;
    assert!(popcorn_detail_body["MediaSources"].is_array());
    assert_eq!(
        popcorn_detail_body["MediaSources"][0]["Id"],
        remote_source_id
    );

    let popcorn_playback = client
        .post(format!(
            "http://{address}/emby/Items/{remote_item_id}/PlaybackInfo"
        ))
        .header("X-Emby-Token", &token)
        .query(&[
            ("UserId", user_id.as_str()),
            ("MediaSourceId", remote_source_id.as_str()),
            ("IsPlayback", "true"),
        ])
        .send()
        .await?;
    assert_eq!(popcorn_playback.status(), reqwest::StatusCode::OK);
    let popcorn_playback_body = popcorn_playback.json::<Value>().await?;
    assert_eq!(
        popcorn_playback_body["MediaSources"][0]["Id"],
        remote_source_id
    );
    assert_eq!(
        popcorn_playback_body["MediaSources"][0]["SupportsDirectPlay"],
        true
    );
    assert_eq!(
        popcorn_playback_body["MediaSources"][0]["DirectStreamUrl"],
        format!("/Videos/{remote_item_id}/{remote_source_id}/stream")
    );

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
    assert_eq!(body["MediaSources"][0]["SupportsDirectStream"], true);
    assert_eq!(
        body["MediaSources"][0]["DirectStreamUrl"],
        format!("/Videos/{remote_item_id}/{remote_source_id}/stream")
    );

    let path_playback = client
        .get(format!(
            "http://{address}/Items/{path_item_id}/PlaybackInfo"
        ))
        .query(&[("api_key", token.as_str())])
        .send()
        .await?;
    assert_eq!(path_playback.status(), reqwest::StatusCode::OK);
    let path_body = path_playback.json::<Value>().await?;
    assert_eq!(path_body["MediaSources"][0]["Protocol"], "File");
    assert_eq!(path_body["MediaSources"][0]["IsRemote"], false);
    assert_eq!(path_body["MediaSources"][0]["SupportsDirectPlay"], true);
    assert_eq!(
        path_body["MediaSources"][0]["DirectStreamUrl"],
        format!("/Videos/{path_item_id}/{path_source_id}/stream")
    );

    let no_redirect_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let player_user_agent = "VidHub/9.0 (iPhone; iOS 18.0)";
    let senplayer_stream = no_redirect_client
        .get(format!(
            "http://{address}/emby/videos/{remote_item_id}/stream.mkv%3FMediaSourceId={remote_source_id}&X-Emby-Token={token}"
        ))
        .header(reqwest::header::USER_AGENT, player_user_agent)
        .send()
        .await?;
    assert_eq!(
        senplayer_stream.status(),
        reqwest::StatusCode::TEMPORARY_REDIRECT
    );
    assert_eq!(
        senplayer_stream.headers()[reqwest::header::LOCATION],
        "http://media.example.test/cdn.mkv"
    );

    let duplicate_source_query_stream = no_redirect_client
        .get(format!(
            "http://{address}/emby/videos/{remote_item_id}/stream.mkv"
        ))
        .query(&[
            ("MediaSourceId", remote_source_id.as_str()),
            ("MediaSourceId", "00000000-0000-0000-0000-000000000000"),
            ("api_key", token.as_str()),
            ("api_key", "invalid-second-token"),
        ])
        .header(reqwest::header::USER_AGENT, player_user_agent)
        .send()
        .await?;
    assert_eq!(
        duplicate_source_query_stream.status(),
        reqwest::StatusCode::TEMPORARY_REDIRECT
    );
    assert_eq!(
        duplicate_source_query_stream.headers()[reqwest::header::LOCATION],
        "http://media.example.test/cdn.mkv"
    );

    let path_stream = no_redirect_client
        .get(format!(
            "http://{address}/Videos/{path_item_id}/original.strm"
        ))
        .query(&[
            ("MediaSourceId", path_source_id.as_str()),
            ("api_key", token.as_str()),
        ])
        .send()
        .await?;
    assert_eq!(path_stream.status(), reqwest::StatusCode::OK);
    assert_eq!(path_stream.bytes().await?, "local path media");

    let unmatched_video_path = no_redirect_client
        .get(format!(
            "http://{address}/Videos/{remote_item_id}/original.strm"
        ))
        .query(&[
            ("MediaSourceId", remote_source_id.as_str()),
            ("api_key", token.as_str()),
        ])
        .header(reqwest::header::USER_AGENT, player_user_agent)
        .send()
        .await?;
    assert_eq!(
        unmatched_video_path.status(),
        reqwest::StatusCode::TEMPORARY_REDIRECT
    );
    assert_eq!(
        unmatched_video_path.headers()[reqwest::header::LOCATION],
        "http://media.example.test/cdn.mkv"
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
        remote_target
    );
    tokio::fs::write(
        root.join("Path.Movie.2026.strm"),
        "https://media.example.test/path-movie.mkv\n",
    )
    .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let updated_path: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT ms.external_url, ms.strm_target_kind
         FROM media_sources ms WHERE ms.id = ?",
    )
    .bind(&path_source_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        updated_path.0.as_deref(),
        Some("https://media.example.test/path-movie.mkv")
    );
    assert_eq!(updated_path.1.as_deref(), Some("URL"));
    let forwarded_user_agents = forwarded_user_agents
        .lock()
        .map_err(|_| "proxy mutex poisoned")?;
    assert!(!forwarded_user_agents.is_empty());
    assert!(
        forwarded_user_agents
            .iter()
            .all(|user_agent| user_agent == player_user_agent)
    );
    server.abort();
    proxy_server.abort();
    Ok(())
}
