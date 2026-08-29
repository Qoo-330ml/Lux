use std::{path::Path, time::Duration};

use axum::{
    Router,
    extract::Json,
    http::StatusCode,
    routing::{get, post},
};
use luxd::application::danmaku::{
    DanmakuService, DanmakuServiceError, MAX_DANMAKU_XML_BYTES, atomic_write_danmaku_xml,
    danmaku_sidecar_path, validate_danmaku_xml, validate_provider_base_url,
};
use luxd::application::{
    libraries::LibraryService,
    plugins::{DANMAKU_PLUGIN_ID, PluginService},
    scanner::LibraryScanner,
    scheduled_tasks::ScheduledTaskService,
    strm_probe::StrmProbeService,
};
use luxd::domain::ids::LibraryId;
use luxd::library::LibraryKind;
use luxd::{config::Config, storage::Database};

#[test]
fn derives_same_basename_xml_without_escaping_media_directory() {
    let path = Path::new("Season 01/Episode 01.mkv");
    assert_eq!(
        danmaku_sidecar_path(path).expect("sidecar path"),
        Path::new("Season 01/Episode 01.xml")
    );
    assert!(danmaku_sidecar_path(Path::new("../Episode.mkv")).is_err());
}

#[test]
fn validates_dandanplay_base_and_redacts_token_path() {
    let base = validate_provider_base_url(" https://danmu.example/secret-token/ ")
        .expect("valid provider base");
    assert_eq!(base.normalized(), "https://danmu.example/secret-token");
    assert_ne!(base.redacted(), base.normalized());
    assert!(validate_provider_base_url("file:///tmp/danmu").is_err());
    assert!(validate_provider_base_url("https://user:pass@example.invalid/api").is_err());
}

#[test]
fn accepts_bilibili_xml_and_rejects_html_or_oversized_payload() {
    let valid = br#"<?xml version="1.0" encoding="UTF-8"?><i><chatserver>chat.bilibili.com</chatserver><chatid>1</chatid><mission>0</mission><maxlimit>1</maxlimit><state>0</state><real_name>0</real_name><source>k-v</source><d p="1,1,25,16777215,0,0,0,0">hello</d></i>"#;
    assert!(validate_danmaku_xml(valid).is_ok());
    assert!(validate_danmaku_xml(b"<html>not danmaku</html>").is_err());
    assert!(validate_danmaku_xml(&vec![b'x'; MAX_DANMAKU_XML_BYTES + 1]).is_err());
}

#[tokio::test]
async fn writes_xml_atomically_and_rejects_existing_file_by_default()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let target = directory.path().join("Episode 01.xml");
    let xml = br#"<i><d p="1,1,25,16777215,0,0,0,0">hello</d></i>"#;

    atomic_write_danmaku_xml(&target, xml, false).await?;
    assert_eq!(tokio::fs::read(&target).await?, xml);
    let error = atomic_write_danmaku_xml(&target, xml, false)
        .await
        .expect_err("overwrite must be explicit");
    assert!(error.to_string().contains("already exists"));
    Ok(())
}

#[tokio::test]
async fn database_migration_creates_danmaku_tables() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: directory.path().to_path_buf(),
    })
    .await?;

    for table in [
        "danmaku_tracks",
        "danmaku_match_jobs",
        "danmaku_match_job_items",
    ] {
        let exists: i64 = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?
            )",
        )
        .bind(table)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(exists, 1, "missing migration table {table}");
    }
    assert_eq!(database.schema_version().await?, 103);
    let active_job_index: i64 = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master
            WHERE type = 'index' AND name = 'idx_danmaku_match_jobs_one_active_library'
        )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(active_job_index, 1);
    Ok(())
}

#[tokio::test]
async fn danmaku_service_rejects_unsafe_concurrency() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: directory.path().to_path_buf(),
    })
    .await?;
    let service = DanmakuService::new(database);

    let error = service
        .create_job(LibraryId::new(), 65, false)
        .await
        .expect_err("concurrency above the limit must be rejected");
    assert!(matches!(error, DanmakuServiceError::InvalidConcurrency));
    Ok(())
}

#[tokio::test]
async fn danmaku_service_matches_local_and_strm_sources_and_writes_sidecars()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let config_dir = directory.path().join("config");
    let media_root = directory.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    tokio::fs::write(media_root.join("Demo.Movie.2024.mkv"), b"video").await?;
    tokio::fs::write(
        media_root.join("Remote.Movie.2024.strm"),
        "https://media.example.test/Remote.Movie.2024.mkv\n",
    )
    .await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
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

    let app = Router::new()
        .route(
            "/api/v2/match",
            post(|Json(body): Json<serde_json::Value>| async move {
                assert!(matches!(
                    body["fileName"].as_str(),
                    Some("Demo.Movie.2024.mkv") | Some("Remote.Movie.2024.strm")
                ));
                Json(serde_json::json!({
                    "matches": [{"animeId": 12, "episodeId": 34}]
                }))
            }),
        )
        .route(
            "/api/v2/comment/34",
            get(|| async {
                (
                    StatusCode::OK,
                    [("content-type", "application/xml")],
                    "<i><d p=\"1,1,25,16777215,0,0,0,0\">hello</d></i>",
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .map_err(Box::<dyn std::error::Error + Send + Sync>::from)
    });
    let plugin_root = config_dir.join("plugins/org.lux.danmaku/binaries");
    tokio::fs::create_dir_all(&plugin_root).await?;
    tokio::fs::copy(
        env!("CARGO_BIN_EXE_lux-plugin-danmaku"),
        plugin_root.join("plugin"),
    )
    .await?;
    tokio::fs::write(
        config_dir.join("plugins/org.lux.danmaku/manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "formatVersion": 1,
            "id": DANMAKU_PLUGIN_ID,
            "name": "弹幕匹配",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "danmaku",
            "category": "MEDIA",
            "capabilities": ["danmaku.match"],
            "configFields": [
                {"key": "providerBaseUrl", "label": "弹幕接口地址", "type": "text", "required": true, "sensitive": true},
                {"key": "libraryIds", "label": "媒体库", "type": "select", "multiple": true, "optionsSource": "media-libraries", "defaultValue": []},
                {"key": "matchOriginalFilename", "label": "使用原始文件名", "type": "toggle", "defaultValue": true},
                {"key": "matchSimplifiedTraditionalTitles", "label": "尝试简繁标题", "type": "toggle", "defaultValue": true},
                {"key": "matchEnglishTitle", "label": "尝试英文标题", "type": "toggle", "defaultValue": false},
                {"key": "schedule", "label": "执行计划", "type": "text", "required": true, "defaultValue": "* * * * *"}
            ],
            "scheduledTasks": [{
                "taskType": "DANMAKU_MATCH",
                "ownerType": "GLOBAL",
                "name": "弹幕匹配",
                "description": "按计划为选定媒体库匹配并下载 Bilibili XML 弹幕旁车。",
                "scheduleConfigKey": "schedule",
                "defaultSchedule": "* * * * *",
                "requiredConfigKeys": ["providerBaseUrl", "libraryIds"],
                "resourceLimit": {"concurrency": 2, "overwrite": false}
            }],
            "permissions": {"network": ["*"], "filesystem": []},
            "files": []
        }))?,
    )
    .await?;
    tokio::fs::create_dir_all(config_dir.join("plugin-config")).await?;
    tokio::fs::write(
        config_dir.join("plugin-config/org.lux.danmaku.json"),
        serde_json::to_vec(&serde_json::json!({
            "providerBaseUrl": format!("http://127.0.0.1:{}", address.port()),
            "libraryIds": [library.id.to_string()],
            "schedule": "* * * * *"
        }))?,
    )
    .await?;
    let plugins = PluginService::new(database.clone(), config_dir);
    plugins.install(DANMAKU_PLUGIN_ID).await?;
    let service = DanmakuService::new(database.clone()).with_plugins(plugins.clone());
    let scheduler = ScheduledTaskService::new(
        database.clone(),
        plugins.clone(),
        StrmProbeService::new(database.clone(), plugins),
        None,
    )
    .with_danmaku(service.clone());
    scheduler.run_once().await;
    scheduler.run_once().await;
    let job_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM danmaku_match_jobs WHERE library_id = ?")
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(job_count, 1);
    let job_id: String = sqlx::query_scalar(
        "SELECT id FROM danmaku_match_jobs WHERE library_id = ? ORDER BY created_at DESC LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let completed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let job = service.get(&job_id).await?;
            if !matches!(job.status.as_str(), "PENDING" | "RUNNING") {
                return Ok::<_, DanmakuServiceError>(job);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| std::io::Error::other("timed out waiting for scheduled danmaku job"))??;
    assert_eq!(completed.status, "COMPLETED");
    assert_eq!(completed.success_count, 2);
    assert_eq!(
        tokio::fs::read(media_root.join("Demo.Movie.2024.xml")).await?,
        b"<i><d p=\"1,1,25,16777215,0,0,0,0\">hello</d></i>"
    );
    assert_eq!(
        tokio::fs::read(media_root.join("Remote.Movie.2024.xml")).await?,
        b"<i><d p=\"1,1,25,16777215,0,0,0,0\">hello</d></i>"
    );
    let (strm_item_id, strm_source_id): (String, String) = sqlx::query_as(
        "SELECT mi.id, ms.id
         FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE fe.relative_path = 'Remote.Movie.2024.strm'",
    )
    .fetch_one(database.pool())
    .await?;
    let strm_danmaku = service
        .read_registered_sidecar_for_source(&strm_item_id, Some(&strm_source_id))
        .await?;
    assert!(strm_danmaku.is_some());
    let track_status: String = sqlx::query_scalar("SELECT status FROM danmaku_tracks LIMIT 1")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(track_status, "READY");

    let root_id: String = sqlx::query_scalar("SELECT id FROM library_roots WHERE library_id = ?")
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?;
    sqlx::query(
        "WITH RECURSIVE sequence(value) AS (
             SELECT 1
             UNION ALL
             SELECT value + 1 FROM sequence WHERE value < 10000
         )
         INSERT INTO media_items (
             id, library_id, item_type, title, sort_title,
             identification_status, identity_key
         )
         SELECT printf('bulk-danmaku-item-%05d', value), ?, 'MOVIE',
                printf('Bulk Danmaku %05d', value),
                printf('Bulk Danmaku %05d', value), 'LOCAL_CONFIRMED',
                printf('bulk-danmaku:%05d', value)
         FROM sequence",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "WITH RECURSIVE sequence(value) AS (
             SELECT 1
             UNION ALL
             SELECT value + 1 FROM sequence WHERE value < 10000
         )
         INSERT INTO filesystem_entries (
             id, library_root_id, relative_path, entry_kind, size,
             modified_at, last_seen_generation, is_missing
         )
         SELECT printf('bulk-danmaku-entry-%05d', value), ?,
                printf('Bulk.Danmaku.%05d.mkv', value), 'FILE', 1, 1,
                'bulk-danmaku', 0
         FROM sequence",
    )
    .bind(root_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "WITH RECURSIVE sequence(value) AS (
             SELECT 1
             UNION ALL
             SELECT value + 1 FROM sequence WHERE value < 10000
         )
         INSERT INTO media_sources (
             id, item_id, source_kind, filesystem_entry_id,
             container, size, is_default, probe_status
         )
         SELECT printf('bulk-danmaku-source-%05d', value),
                printf('bulk-danmaku-item-%05d', value), 'LOCAL_FILE',
                printf('bulk-danmaku-entry-%05d', value),
                'mkv', 1, 1, 'PENDING'
         FROM sequence",
    )
    .execute(database.pool())
    .await?;
    let large_job = tokio::time::timeout(
        Duration::from_millis(250),
        service.create_job(library.id, 64, false),
    )
    .await
    .map_err(|_| "danmaku job creation materialized every source")??;
    assert_eq!(large_job.total_count, 10002);

    server.abort();
    Ok(())
}
