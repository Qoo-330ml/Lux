use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn spawn_test_proxy()
-> Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                loop {
                    let Ok(read) = stream.read(&mut chunk).await else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                    if request.len() > 16 * 1024 {
                        return;
                    }
                }
                let request = String::from_utf8_lossy(&request);
                let forwards_credentials = request.lines().any(|line| {
                    let line = line.to_ascii_lowercase();
                    line.starts_with("authorization:") || line.starts_with("cookie:")
                });
                if forwards_credentials {
                    let _ = stream
                        .write_all(
                            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                    return;
                }
                let method = request.split_whitespace().next().unwrap_or_default();
                let has_range = request
                    .lines()
                    .any(|line| line.eq_ignore_ascii_case("range: bytes=0-5"));
                let (status, body, content_range) = if has_range {
                    (
                        "206 Partial Content",
                        b"remote".as_slice(),
                        Some("bytes 0-5/17"),
                    )
                } else {
                    ("200 OK", b"remote video data".as_slice(), None)
                };
                let body_length = body.len();
                let headers = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: video/x-matroska\r\nContent-Length: {body_length}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n{}\r\n",
                    content_range
                        .map(|value| format!("Content-Range: {value}\r\n"))
                        .unwrap_or_default()
                );
                if stream.write_all(headers.as_bytes()).await.is_err() {
                    return;
                }
                if method != "HEAD" {
                    let _ = stream.write_all(body).await;
                }
            });
        }
    });
    Ok((address, task))
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

#[tokio::test]
async fn download_returns_selected_local_and_strm_sources_without_archiving()
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
    let movie_dir = root.join("二毛 (2019)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    let selected = "二毛 (2019) - 2160p - H.265 - AAC - test.mkv";
    tokio::fs::write(movie_dir.join(selected), b"selected video").await?;
    tokio::fs::write(
        movie_dir.join("二毛 (2019) - 2160p - H.265 - AAC - test.zh.ass"),
        b"subtitle",
    )
    .await?;
    tokio::fs::write(
        movie_dir.join("二毛 (2019) - 1080p - H.264 - AAC.mkv"),
        b"other video",
    )
    .await?;
    tokio::fs::write(
        root.join("Remote.Movie.2024.strm"),
        b"http://example.com/Remote.Movie.2024.mkv\nignored\n",
    )
    .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar(
        "SELECT mi.id
         FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE fe.relative_path = ?",
    )
    .bind(format!("二毛 (2019)/{selected}"))
    .fetch_one(database.pool())
    .await?;
    let (remote_item_id, remote_source_id): (String, String) = sqlx::query_as(
        "SELECT mi.id, ms.id
         FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE fe.relative_path = ?",
    )
    .bind("Remote.Movie.2024.strm")
    .fetch_one(database.pool())
    .await?;
    sqlx::query("UPDATE users SET can_download = 1 WHERE username_normalized = 'admin'")
        .execute(database.pool())
        .await?;
    let source_id: String = sqlx::query_scalar(
        "SELECT ms.id
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE ms.item_id = ? AND fe.relative_path = ?",
    )
    .bind(&item_id)
    .bind(format!("二毛 (2019)/{selected}"))
    .fetch_one(database.pool())
    .await?;

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let (proxy_address, proxy_task) = spawn_test_proxy().await?;
    let app = app_with_state(AppState::ready_with_proxy(
        config,
        database,
        setup,
        auth,
        emby_auth,
        Some(format!("http://{proxy_address}")),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cookie = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(login.headers(), "lux_session"),
        cookie_value(login.headers(), "lux_csrf")
    );

    let response = client
        .head(format!(
            "{base_url}/api/v1/items/{item_id}/download?sourceId={source_id}"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "video/x-matroska");
    let content_disposition = response.headers()["content-disposition"].to_str()?;
    assert!(content_disposition.contains("filename*=UTF-8''"));
    assert!(content_disposition.contains(".mkv"));
    assert!(!content_disposition.contains("filename=\"download\""));

    let response = client
        .get(format!(
            "{base_url}/api/v1/items/{item_id}/download?sourceId={source_id}"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "video/x-matroska");
    assert_eq!(response.bytes().await?.as_ref(), b"selected video");

    let response = client
        .get(format!(
            "{base_url}/api/v1/items/{remote_item_id}/download?sourceId={remote_source_id}"
        ))
        .header(COOKIE, &cookie)
        .header("Range", "bytes=0-5")
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()["content-type"], "video/x-matroska");
    assert_eq!(response.headers()["content-range"], "bytes 0-5/17");
    assert!(
        response.headers()["content-disposition"]
            .to_str()?
            .contains("Remote.Movie.2024.mkv")
    );
    assert_eq!(response.bytes().await?.as_ref(), b"remote");

    let response = client
        .head(format!(
            "{base_url}/api/v1/items/{remote_item_id}/download?sourceId={remote_source_id}"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "video/x-matroska");
    assert_eq!(response.headers()["content-length"], "17");
    assert!(response.bytes().await?.is_empty());

    let emby_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="DownloadTest", Device="Mac", DeviceId="download-test", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let emby_token = emby_login.json::<serde_json::Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing Emby token")?
        .to_owned();
    let response = client
        .get(format!("{base_url}/Items/{item_id}/Download"))
        .query(&[
            ("api_key", emby_token.as_str()),
            ("mediaSourceId", source_id.as_str()),
        ])
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "video/x-matroska");
    let content_disposition = response.headers()["content-disposition"].to_str()?;
    assert!(content_disposition.contains("filename*=UTF-8''"));
    assert!(content_disposition.contains(".mkv"));
    assert!(!content_disposition.contains("filename=\"download\""));
    assert_eq!(response.bytes().await?.as_ref(), b"selected video");

    let response = client
        .head(format!("{base_url}/Items/{item_id}/Download"))
        .query(&[
            ("api_key", emby_token.as_str()),
            ("mediaSourceId", source_id.as_str()),
        ])
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "video/x-matroska");

    let response = client
        .get(format!("{base_url}/Items/{remote_item_id}/Download"))
        .query(&[
            ("api_key", emby_token.as_str()),
            ("mediaSourceId", remote_source_id.as_str()),
        ])
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "video/x-matroska");
    assert!(
        response.headers()["content-disposition"]
            .to_str()?
            .contains("Remote.Movie.2024.mkv")
    );
    assert_eq!(response.bytes().await?.as_ref(), b"remote video data");

    server.abort();
    proxy_task.abort();
    Ok(())
}
