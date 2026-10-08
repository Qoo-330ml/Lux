use std::fs;

mod common;

use common::{TestScraper, TestScraperConfig};
use luxd::application::{
    chapter_detector::{ChapterDetectionService, DEFAULT_CHAPTER_DETECTOR_PLUGIN_ID},
    libraries::{LibraryService, LibrarySettingsPatch},
    plugins::{MEDIA_INFO_PLUGIN_ID, PluginService},
    reidentify::MetadataReidentifyService,
    scanner::{LibraryScanner, ScanJobService},
    schedule::DANMAKU_MATCH_TASK_TYPE,
    scheduled_tasks::{METADATA_TASK_TYPE, ScheduledTaskService},
    scraper::ScraperProvider,
    strm_probe::StrmProbeService,
};
use luxd::{config::Config, library::LibraryKind, storage::Database};
use serde_json::Map;
use serde_json::json;

#[tokio::test]
async fn scheduled_metadata_task_reports_dispatcher_shutdown_and_keeps_job_queued()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Scheduled metadata", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Scheduled Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Scheduled.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    libraries
        .update_settings(
            library.id,
            LibrarySettingsPatch {
                metadata_schedule: Some(Some("* * * * *".to_owned())),
                ..LibrarySettingsPatch::default()
            },
        )
        .await?;

    let metadata = MetadataReidentifyService::new(
        database.clone(),
        ScraperProvider::from_adapter(TestScraper::new(TestScraperConfig::default())?),
    );
    let plugins = PluginService::new(database.clone(), config.config_dir.clone());
    let scheduler = ScheduledTaskService::new(
        database.clone(),
        plugins.clone(),
        StrmProbeService::new(database.clone(), plugins),
        None,
    )
    .with_library_services(
        ScanJobService::new(database.clone()),
        Some(metadata.clone()),
        None,
        None,
    );

    let accepted = scheduler
        .run_task("LIBRARY", &library.id.to_string(), METADATA_TASK_TYPE)
        .await?;
    let accepted_job = match accepted {
        luxd::application::scheduled_tasks::ScheduledTaskRun::Metadata { job } => job,
        _ => return Err("metadata task returned a different job type".into()),
    };
    assert_eq!(accepted_job.mode, "FILL_MISSING");
    sqlx::query(
        "UPDATE metadata_reidentify_jobs
         SET status = 'FAILED', error = 'TEST_RETRY', finished_at = unixepoch()
         WHERE id = ?",
    )
    .bind(&accepted_job.id)
    .execute(database.pool())
    .await?;
    metadata.shutdown().await;
    let result = scheduler
        .run_task("LIBRARY", &library.id.to_string(), METADATA_TASK_TYPE)
        .await;
    assert!(
        result.is_err(),
        "a closed dispatcher must reject the scheduled job"
    );
    let queued_jobs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM metadata_reidentify_jobs
         WHERE library_id = ? AND mode = 'FILL_MISSING' AND status = 'QUEUED'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(queued_jobs, 1, "the persisted job must remain retryable");
    Ok(())
}

#[tokio::test]
async fn enabled_strm_task_runs_once_per_matching_cron_minute()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join("plugins/org.lux.strm-media-info");
    tokio::fs::create_dir_all(plugin_dir.join("binaries")).await?;
    fs::write(plugin_dir.join("binaries/plugin"), b"placeholder")?;
    tokio::fs::write(
        plugin_dir.join("manifest.json"),
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
                {"key": "existingInfoPolicy", "label": "已有媒体信息处理方式", "type": "select", "defaultValue": "SKIP", "options": [{"value": "SKIP", "label": "跳过已有媒体信息"}, {"value": "OVERWRITE", "label": "覆盖已有媒体信息"}]},
                {"key": "writeSidecars", "label": "写入旁车", "type": "toggle", "defaultValue": true},
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
    let library = luxd::application::libraries::LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let plugins = PluginService::new(database.clone(), config_dir);
    plugins.install(MEDIA_INFO_PLUGIN_ID).await?;
    plugins
        .update_dynamic_config(
            MEDIA_INFO_PLUGIN_ID,
            Map::from_iter([
                ("libraryIds".to_owned(), json!([library.id.to_string()])),
                ("concurrency".to_owned(), json!(2)),
                ("existingInfoPolicy".to_owned(), json!("SKIP")),
                ("writeSidecars".to_owned(), json!(true)),
                ("schedule".to_owned(), json!("* * * * *")),
            ]),
        )
        .await?;
    let scheduler = ScheduledTaskService::new(
        database.clone(),
        plugins.clone(),
        StrmProbeService::new(database.clone(), plugins),
        None,
    );

    scheduler.run_once().await;
    scheduler.run_once().await;

    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM strm_probe_jobs")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(jobs, 1);
    Ok(())
}

#[tokio::test]
async fn enabled_chapter_task_creates_one_detection_job_per_matching_minute()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join(format!("plugins/{DEFAULT_CHAPTER_DETECTOR_PLUGIN_ID}"));
    tokio::fs::create_dir_all(plugin_dir.join("binaries")).await?;
    fs::write(plugin_dir.join("binaries/plugin"), b"placeholder")?;
    tokio::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": DEFAULT_CHAPTER_DETECTOR_PLUGIN_ID,
            "name": "Intro/outro detector",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "chapter_detector",
            "category": "MEDIA",
            "supportedMediaSourceKinds": ["LOCAL_FILE"],
            "supportedItemTypes": ["Episode"],
            "capabilities": ["chapters.detect"],
            "configFields": [
                {"key": "concurrency", "label": "Concurrency", "type": "number", "required": true, "defaultValue": 2, "minimum": 1, "maximum": 16},
                {"key": "introWindowSeconds", "label": "Intro window", "type": "number", "required": true, "defaultValue": 180, "minimum": 15, "maximum": 300},
                {"key": "creditsWindowSeconds", "label": "Credits window", "type": "number", "required": true, "defaultValue": 180, "minimum": 15, "maximum": 600},
                {"key": "matchThreshold", "label": "Threshold", "type": "number", "required": true, "defaultValue": 80, "minimum": 1, "maximum": 100},
                {"key": "schedule", "label": "Schedule", "type": "text", "required": true, "defaultValue": "0 4 * * 0"}
            ],
            "permissions": {"network": [], "filesystem": []},
            "files": []
        }))?,
    )
    .await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let library = luxd::application::libraries::LibraryService::new(database.clone())
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let second_library = luxd::application::libraries::LibraryService::new(database.clone())
        .create_library("More Shows", LibraryKind::Series, false)
        .await?;
    let plugins = PluginService::new(database.clone(), config_dir);
    plugins.install(DEFAULT_CHAPTER_DETECTOR_PLUGIN_ID).await?;
    let libraries = luxd::application::libraries::LibraryService::new(database.clone());
    for selected_library in [library.id, second_library.id] {
        libraries
            .update_settings(
                selected_library,
                LibrarySettingsPatch {
                    chapter_source_id: Some(Some(DEFAULT_CHAPTER_DETECTOR_PLUGIN_ID.to_owned())),
                    ..Default::default()
                },
            )
            .await?;
    }
    plugins
        .update_dynamic_config(
            DEFAULT_CHAPTER_DETECTOR_PLUGIN_ID,
            Map::from_iter([
                ("concurrency".to_owned(), json!(2)),
                ("introWindowSeconds".to_owned(), json!(180)),
                ("creditsWindowSeconds".to_owned(), json!(180)),
                ("matchThreshold".to_owned(), json!(80)),
                ("schedule".to_owned(), json!("* * * * *")),
            ]),
        )
        .await?;
    let scheduler = ScheduledTaskService::new(
        database.clone(),
        plugins.clone(),
        StrmProbeService::new(database.clone(), plugins.clone()),
        Some(ChapterDetectionService::new(database.clone(), plugins)),
    );

    scheduler.run_once().await;
    scheduler.run_once().await;

    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chapter_detection_jobs")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(jobs, 2);
    Ok(())
}

#[tokio::test]
async fn registered_danmaku_task_uses_the_danmaku_service() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join("plugins/org.lux.danmaku");
    tokio::fs::create_dir_all(plugin_dir.join("binaries")).await?;
    fs::write(plugin_dir.join("binaries/plugin"), b"placeholder")?;
    tokio::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": "org.lux.danmaku",
            "name": "弹幕匹配",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "danmaku",
            "category": "MEDIA",
            "capabilities": ["danmaku.match"],
            "configFields": [
                {"key": "providerBaseUrl", "label": "弹幕 API 地址", "type": "text", "required": true, "sensitive": true},
                {"key": "libraryIds", "label": "媒体库", "type": "select", "multiple": true, "optionsSource": "media-libraries", "defaultValue": []},
                {"key": "schedule", "label": "执行计划", "type": "text", "required": true, "defaultValue": "0 6 * * *"}
            ],
            "scheduledTasks": [{
                "taskType": "DANMAKU_MATCH",
                "ownerType": "GLOBAL",
                "name": "弹幕匹配",
                "description": "按计划为选定媒体库匹配并下载 Bilibili XML 弹幕旁车。",
                "scheduleConfigKey": "schedule",
                "defaultSchedule": "0 6 * * *",
                "requiredConfigKeys": ["providerBaseUrl", "libraryIds"],
                "resourceLimit": {"concurrency": 2, "overwrite": false}
            }],
            "permissions": {"network": ["*"], "filesystem": []},
            "files": []
        }))?,
    )
    .await?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    })
    .await?;
    let plugins = PluginService::new(database.clone(), config_dir);
    plugins.install("org.lux.danmaku").await?;
    let scheduler = ScheduledTaskService::new(
        database.clone(),
        plugins.clone(),
        StrmProbeService::new(database, plugins.clone()),
        None,
    );
    let error = scheduler
        .run_task("GLOBAL", "global", DANMAKU_MATCH_TASK_TYPE)
        .await
        .expect_err("danmaku task must be dispatched to its service");
    assert!(matches!(
        error,
        luxd::application::scheduled_tasks::ScheduledTaskError::ServiceUnavailable
    ));
    Ok(())
}

#[tokio::test]
async fn scheduled_plan_dispatches_each_library_once_per_cron_minute()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    })
    .await?;
    let libraries = luxd::application::libraries::LibraryService::new(database.clone());
    libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .create_library("More Movies", LibraryKind::Movie, false)
        .await?;
    sqlx::query(
        "UPDATE scheduled_task_plans
         SET cron_or_interval = '* * * * *', is_enabled = 1
         WHERE task_type = 'RECONCILIATION_SCAN' AND is_default = 1",
    )
    .execute(database.pool())
    .await?;
    let plugins = PluginService::new(database.clone(), config_dir);
    let scheduler = ScheduledTaskService::new(
        database.clone(),
        plugins.clone(),
        StrmProbeService::new(database.clone(), plugins),
        None,
    )
    .with_library_services(ScanJobService::new(database.clone()), None, None, None);

    scheduler.run_once().await;
    scheduler.run_once().await;
    let job_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_jobs WHERE job_type = 'RECONCILE_LIBRARY'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(job_count, 2);
    Ok(())
}

#[tokio::test]
async fn unplanned_task_dispatch_reuses_the_loaded_task_config()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    })
    .await?;
    let library = luxd::application::libraries::LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    sqlx::query(
        "UPDATE scheduled_task_configs
         SET plan_id = NULL, cron_or_interval = '* * * * *', is_enabled = 1
         WHERE owner_type = 'LIBRARY' AND owner_id = ?
           AND task_type = 'RECONCILIATION_SCAN'",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    let plugins = PluginService::new(database.clone(), config_dir);
    let scheduler = ScheduledTaskService::new(
        database.clone(),
        plugins.clone(),
        StrmProbeService::new(database.clone(), plugins),
        None,
    )
    .with_library_services(ScanJobService::new(database.clone()), None, None, None);

    scheduler.run_once().await;

    let job_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_jobs
         WHERE library_id = ? AND job_type = 'RECONCILE_LIBRARY'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(job_count, 1);
    Ok(())
}
