use std::fs;

use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        plugins::{MEDIA_INFO_PLUGIN_ID, PluginService},
        scanner::LibraryScanner,
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    domain::ids::LibraryId,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn admin_can_start_and_list_strm_probe_jobs() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_root = config_dir.join("plugins/org.lux.strm-media-info");
    tokio::fs::create_dir_all(plugin_root.join("binaries")).await?;
    fs::write(plugin_root.join("binaries/plugin"), b"placeholder")?;
    tokio::fs::write(
        plugin_root.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": "org.lux.strm-media-info",
            "name": "strm媒体信息提取",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "media_probe",
            "category": "MEDIA",
            "capabilities": ["media.probe"],
            "configFields": [
                {"key": "libraryIds", "label": "媒体库", "type": "select", "multiple": true, "required": true, "optionsSource": "media-libraries"},
                {"key": "concurrency", "label": "并发数", "type": "number", "required": true, "defaultValue": 2, "minimum": 1, "maximum": 64},
                {"key": "existingInfoPolicy", "label": "已有媒体信息处理方式", "type": "select", "defaultValue": "SKIP", "options": [{"value": "SKIP", "label": "跳过已有媒体信息"}, {"value": "OVERWRITE", "label": "覆盖已有媒体信息"}]},
                {"key": "writeSidecars", "label": "写入旁车", "type": "toggle", "defaultValue": true}
            ],
            "permissions": {"network": ["media-source"], "filesystem": []},
            "files": []
        }))?,
    )
    .await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir,
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
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

    client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cookie = cookie_pair(login.headers());
    let csrf = cookie_value(login.headers(), "lux_csrf");

    let library = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "name": "Movies", "kind": "MOVIE" }))
        .send()
        .await?;
    assert_eq!(library.status(), reqwest::StatusCode::CREATED);
    let library_id = library.json::<Value>().await?["library"]["id"]
        .as_str()
        .ok_or("missing library id")?
        .to_owned();
    let library_id_typed = library_id.parse::<LibraryId>()?;
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Api Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(
        movie_dir.join("Api.Movie.2024.strm"),
        "https://example.invalid/movie",
    )
    .await?;
    LibraryService::new(database.clone())
        .add_root(
            library_id_typed,
            media_root.to_str().ok_or("non-utf8 media root")?,
        )
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library_id_typed)
        .await?;

    let installed = client
        .post(format!(
            "{base_url}/api/v1/admin/plugins/org.lux.strm-media-info/install"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(installed.status(), reqwest::StatusCode::CREATED);

    let invalid = client
        .put(format!(
            "{base_url}/api/v1/admin/plugins/org.lux.strm-media-info/config"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "libraryIds": [library_id], "concurrency": 0 }))
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);

    let configured = client
        .put(format!(
            "{base_url}/api/v1/admin/plugins/org.lux.strm-media-info/config"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "libraryIds": [library_id],
            "concurrency": 2,
            "existingInfoPolicy": "SKIP",
            "writeSidecars": false
        }))
        .send()
        .await?;
    assert_eq!(configured.status(), reqwest::StatusCode::OK);

    let mut listed_body = Value::Null;
    for _ in 0..50 {
        let listed = client
            .get(format!("{base_url}/api/v1/admin/strm-probe-jobs"))
            .header(COOKIE, &cookie)
            .send()
            .await?;
        assert_eq!(listed.status(), reqwest::StatusCode::OK);
        listed_body = listed.json::<Value>().await?;
        if listed_body["jobs"]
            .as_array()
            .is_some_and(|jobs| !jobs.is_empty())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let jobs = listed_body["jobs"]
        .as_array()
        .ok_or("missing STRM probe jobs")?;
    assert_eq!(jobs.len(), 1);
    let job_id = jobs[0]["id"]
        .as_str()
        .ok_or("missing STRM probe job id")?
        .to_owned();

    let detail = client
        .get(format!("{base_url}/api/v1/admin/strm-probe-jobs/{job_id}"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(detail.status(), reqwest::StatusCode::OK);
    assert_eq!(detail.json::<Value>().await?["job"]["id"], job_id);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn only_authorized_strm_media_get_triggers_playback_probe()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_root = config_dir.join("plugins/org.lux.strm-media-info");
    tokio::fs::create_dir_all(plugin_root.join("binaries")).await?;
    fs::write(plugin_root.join("binaries/plugin"), b"placeholder")?;
    tokio::fs::write(
        plugin_root.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": MEDIA_INFO_PLUGIN_ID,
            "name": "strm媒体信息提取",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "media_probe",
            "category": "MEDIA",
            "capabilities": ["media.probe"],
            "configFields": [
                {"key": "libraryIds", "label": "媒体库", "type": "select", "multiple": true, "required": true, "optionsSource": "media-libraries"},
                {"key": "concurrency", "label": "并发数", "type": "number", "required": true, "defaultValue": 2, "minimum": 1, "maximum": 64},
                {"key": "mediaInfoEnabled", "label": "提取媒体信息", "type": "toggle", "defaultValue": true},
                {"key": "thumbnailEnabled", "label": "补全 STRM 缩略图", "type": "toggle", "defaultValue": false},
                {"key": "thumbnailPositionPercent", "label": "缩略图位置", "type": "number", "required": true, "defaultValue": 30, "minimum": 1, "maximum": 99},
                {"key": "existingInfoPolicy", "label": "已有媒体信息处理方式", "type": "select", "defaultValue": "SKIP", "options": [{"value": "SKIP", "label": "跳过已有媒体信息"}, {"value": "OVERWRITE", "label": "覆盖已有媒体信息"}]},
                {"key": "writeSidecars", "label": "写入旁车", "type": "toggle", "defaultValue": false},
                {"key": "schedule", "label": "执行计划", "type": "text", "required": true, "defaultValue": "0 3 * * *"}
            ],
            "permissions": {"network": ["media-source"], "filesystem": []},
            "files": []
        }))?,
    )
    .await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    sqlx::query("UPDATE users SET max_simultaneous_playbacks = 1 WHERE id = ?")
        .bind(admin.id.to_string())
        .execute(database.pool())
        .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", luxd::library::LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    tokio::fs::write(
        media_root.join("Playback.Movie.2024.strm"),
        "https://media.example.invalid/movie.mkv",
    )
    .await?;
    tokio::fs::write(
        media_root.join("Blocked.Movie.2024.strm"),
        "https://media.example.invalid/blocked.mkv",
    )
    .await?;
    LibraryService::new(database.clone())
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-utf8 media root")?,
        )
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let (item_id, source_id): (String, String) = sqlx::query_as(
        "SELECT mi.id, ms.id FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         WHERE ms.source_kind = 'STRM_URL' AND mi.title = 'Playback Movie'",
    )
    .fetch_one(database.pool())
    .await?;
    let (blocked_item_id, blocked_source_id): (String, String) = sqlx::query_as(
        "SELECT mi.id, ms.id FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         WHERE ms.source_kind = 'STRM_URL' AND mi.title = 'Blocked Movie'",
    )
    .fetch_one(database.pool())
    .await?;
    let plugins = PluginService::new(database.clone(), config_dir.clone());
    plugins.install(MEDIA_INFO_PLUGIN_ID).await?;
    plugins
        .update_dynamic_config(
            MEDIA_INFO_PLUGIN_ID,
            serde_json::Map::from_iter([
                ("libraryIds".to_owned(), json!([library.id.to_string()])),
                ("concurrency".to_owned(), json!(2)),
                ("mediaInfoEnabled".to_owned(), json!(true)),
                ("thumbnailEnabled".to_owned(), json!(false)),
                ("thumbnailPositionPercent".to_owned(), json!(30)),
                ("existingInfoPolicy".to_owned(), json!("OVERWRITE")),
                ("writeSidecars".to_owned(), json!(false)),
                ("schedule".to_owned(), json!("0 3 * * *")),
            ]),
        )
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
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let unauthorized = client
        .get(format!("{base_url}/Videos/{item_id}/{source_id}/stream"))
        .send()
        .await?;
    assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM strm_probe_jobs")
            .fetch_one(database.pool())
            .await?,
        0
    );
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="STRMProbeTest", Device="Mac", DeviceId="strm-probe-test", Version="1""#,
        )
        .json(&json!({"Username": "admin", "Pw": "correct password"}))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing Emby token")?
        .to_owned();

    let playback_info = client
        .get(format!("{base_url}/Items/{item_id}/PlaybackInfo"))
        .query(&[("api_key", token.as_str())])
        .send()
        .await?;
    assert_eq!(playback_info.status(), reqwest::StatusCode::OK);
    let direct_url = playback_info.json::<Value>().await?["MediaSources"][0]["DirectStreamUrl"]
        .as_str()
        .ok_or("missing STRM direct stream URL")?
        .to_owned();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM strm_probe_jobs")
            .fetch_one(database.pool())
            .await?,
        0
    );

    let head = client
        .head(format!("{base_url}{direct_url}"))
        .send()
        .await?;
    assert_eq!(head.status(), reqwest::StatusCode::FOUND);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM strm_probe_jobs")
            .fetch_one(database.pool())
            .await?,
        0
    );

    let blocked_playback_info = client
        .get(format!("{base_url}/Items/{blocked_item_id}/PlaybackInfo"))
        .query(&[("api_key", token.as_str())])
        .send()
        .await?;
    assert_eq!(blocked_playback_info.status(), reqwest::StatusCode::OK);
    let blocked_direct_url =
        blocked_playback_info.json::<Value>().await?["MediaSources"][0]["DirectStreamUrl"]
            .as_str()
            .ok_or("missing blocked STRM direct stream URL")?
            .to_owned();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM strm_probe_jobs")
            .fetch_one(database.pool())
            .await?,
        0
    );

    let stream = client.get(format!("{base_url}{direct_url}")).send().await?;
    assert_eq!(stream.status(), reqwest::StatusCode::FOUND);
    let mut target_source = None;
    for _ in 0..100 {
        target_source = sqlx::query_scalar::<_, Option<String>>(
            "SELECT target_source_id FROM strm_probe_jobs ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_optional(database.pool())
        .await?
        .flatten();
        if target_source.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(target_source.as_deref(), Some(source_id.as_str()));

    for _ in 0..100 {
        let active_jobs = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM strm_probe_jobs WHERE status IN ('PENDING', 'RUNNING')",
        )
        .fetch_one(database.pool())
        .await?;
        if active_jobs == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let denied_stream = client
        .get(format!("{base_url}{blocked_direct_url}"))
        .send()
        .await?;
    assert_eq!(
        denied_stream.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    for _ in 0..100 {
        let denied_source_jobs = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM strm_probe_jobs WHERE target_source_id = ?",
        )
        .bind(&blocked_source_id)
        .fetch_one(database.pool())
        .await?;
        if denied_source_jobs > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM strm_probe_jobs WHERE target_source_id = ?",
        )
        .bind(&blocked_source_id)
        .fetch_one(database.pool())
        .await?,
        0,
        "a media GET denied by playback admission must not enqueue a STRM probe"
    );

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
        .ok_or_else(|| format!("missing cookie {name}"))
        .expect("login should set cookie")
}

fn cookie_pair(headers: &reqwest::header::HeaderMap) -> String {
    format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(headers, "lux_session"),
        cookie_value(headers, "lux_csrf")
    )
}
