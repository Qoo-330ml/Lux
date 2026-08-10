use std::fs;

use luxd::application::{
    plugins::{MEDIA_INFO_PLUGIN_ID, PluginService},
    scheduled_tasks::{ScheduledTaskError, ScheduledTaskService},
    strm_probe::StrmProbeService,
};
use luxd::{config::Config, library::LibraryKind, storage::Database};
use serde_json::Map;
use serde_json::json;

#[tokio::test]
async fn enabled_strm_task_runs_once_until_its_interval_is_due()
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
                {"key": "schedule", "label": "执行间隔", "type": "text", "required": true, "defaultValue": "24h"}
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
                ("schedule".to_owned(), json!("1m")),
            ]),
        )
        .await?;
    let scheduler = ScheduledTaskService::new(
        database.clone(),
        plugins.clone(),
        StrmProbeService::new(database.clone(), plugins),
    );

    scheduler
        .run_task("GLOBAL", "global", "STRM_MEDIA_INFO")
        .await?;
    assert!(matches!(
        scheduler
            .run_task("GLOBAL", "global", "STRM_MEDIA_INFO")
            .await,
        Err(ScheduledTaskError::Strm(
            luxd::application::strm_probe::StrmProbeError::AlreadyActive
        ))
    ));

    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM strm_probe_jobs")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(jobs, 1);
    Ok(())
}
