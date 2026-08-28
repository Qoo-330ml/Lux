use std::fs;

use luxd::{
    application::{
        danmaku::{DanmakuService, DanmakuServiceError},
        libraries::LibraryService,
        plugins::PluginService,
        schedule::{DANMAKU_MATCH_TASK_TYPE, DEFAULT_DANMAKU_MATCH_SCHEDULE},
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use serde_json::{Map, json};

const PLUGIN_ID: &str = "org.lux.danmaku";

#[tokio::test]
async fn danmaku_plugin_config_exposes_library_scope_and_match_preferences()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join(format!("plugins/{PLUGIN_ID}/binaries"));
    tokio::fs::create_dir_all(&plugin_dir).await?;
    fs::write(plugin_dir.join("plugin"), b"placeholder")?;
    tokio::fs::write(
        config_dir.join(format!("plugins/{PLUGIN_ID}/manifest.json")),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": PLUGIN_ID,
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
                {"key": "matchOriginalFilename", "label": "使用原始文件名", "type": "toggle", "defaultValue": true},
                {"key": "matchSimplifiedTraditionalTitles", "label": "尝试简繁标题", "type": "toggle", "defaultValue": true},
                {"key": "matchEnglishTitle", "label": "尝试英文标题", "type": "toggle", "defaultValue": false},
                {"key": "schedule", "label": "执行计划", "type": "text", "required": true, "defaultValue": "0 6 * * *"}
            ],
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
    let library = LibraryService::new(database.clone())
        .create_library("番剧", LibraryKind::Series, false)
        .await?;
    let other_library = LibraryService::new(database.clone())
        .create_library("其他", LibraryKind::Movie, false)
        .await?;
    let plugins = PluginService::new(database.clone(), config_dir);
    plugins.install(PLUGIN_ID).await?;

    let registered: (Option<String>, i64, String, Option<String>) = sqlx::query_as(
        "SELECT cron_or_interval, is_enabled, source_type, plugin_id
         FROM scheduled_task_configs
         WHERE owner_type = 'GLOBAL' AND owner_id = 'global' AND task_type = ?",
    )
    .bind(DANMAKU_MATCH_TASK_TYPE)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        registered.0.as_deref(),
        Some(DEFAULT_DANMAKU_MATCH_SCHEDULE)
    );
    assert_eq!(registered.1, 0);
    assert_eq!(registered.2, "PLUGIN");
    assert_eq!(registered.3.as_deref(), Some(PLUGIN_ID));

    let page = plugins.list_installed(0, 20).await?;
    let plugin = page
        .plugins
        .iter()
        .find(|plugin| plugin.id == PLUGIN_ID)
        .ok_or("danmaku plugin missing")?;
    let library_field = plugin
        .config_fields
        .iter()
        .find(|field| field.key == "libraryIds")
        .ok_or("libraryIds field missing")?;
    assert!(
        library_field
            .options
            .iter()
            .any(|option| option.value == library.id.to_string())
    );
    assert_eq!(plugin.config_values["libraryIds"], json!([]));
    assert_eq!(plugin.config_values["matchOriginalFilename"], true);
    assert_eq!(
        plugin.config_values["matchSimplifiedTraditionalTitles"],
        true
    );
    assert_eq!(plugin.config_values["matchEnglishTitle"], false);
    assert!(!plugin.configured);

    let values = Map::from_iter([
        (
            "providerBaseUrl".to_owned(),
            json!("https://danmu.example/secret"),
        ),
        ("libraryIds".to_owned(), json!([library.id.to_string()])),
        ("matchOriginalFilename".to_owned(), json!(true)),
        ("matchSimplifiedTraditionalTitles".to_owned(), json!(false)),
        ("matchEnglishTitle".to_owned(), json!(true)),
    ]);
    let updated = plugins.update_dynamic_config(PLUGIN_ID, values).await?;
    assert!(updated.configured);
    assert_eq!(
        updated.config_values["libraryIds"],
        json!([library.id.to_string()])
    );
    assert_eq!(
        updated.config_values["matchSimplifiedTraditionalTitles"],
        false
    );
    assert_eq!(updated.config_values["matchEnglishTitle"], true);

    let enabled: (Option<String>, i64) = sqlx::query_as(
        "SELECT cron_or_interval, is_enabled
         FROM scheduled_task_configs
         WHERE owner_type = 'GLOBAL' AND owner_id = 'global' AND task_type = ?",
    )
    .bind(DANMAKU_MATCH_TASK_TYPE)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        enabled,
        (Some(DEFAULT_DANMAKU_MATCH_SCHEDULE.to_owned()), 1)
    );

    plugins.set_enabled(PLUGIN_ID, false).await?;
    let disabled: i64 = sqlx::query_scalar(
        "SELECT is_enabled FROM scheduled_task_configs
         WHERE owner_type = 'GLOBAL' AND owner_id = 'global' AND task_type = ?",
    )
    .bind(DANMAKU_MATCH_TASK_TYPE)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(disabled, 0);
    plugins.set_enabled(PLUGIN_ID, true).await?;

    let settings = plugins.danmaku_settings().await?;
    assert_eq!(settings.library_ids, vec![library.id.to_string()]);
    assert!(settings.match_original_filename);
    assert!(!settings.match_simplified_traditional_titles);
    assert!(settings.match_english_title);

    LibraryService::new(database.clone())
        .delete_library(library.id)
        .await?;
    plugins.prune_danmaku_library_ids().await?;
    assert!(plugins.danmaku_settings().await?.library_ids.is_empty());

    let danmaku = DanmakuService::new(database).with_plugins(plugins);
    let error = danmaku
        .create_job(other_library.id, 2, false)
        .await
        .expect_err("unselected library must not accept a danmaku job");
    assert!(matches!(error, DanmakuServiceError::LibraryNotSelected));
    Ok(())
}
