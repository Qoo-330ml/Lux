use std::fs;

use luxd::{
    application::{
        libraries::LibraryService,
        plugins::{MEDIA_INFO_PLUGIN_ID, PluginService},
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use serde_json::{Map, Value, json};

#[tokio::test]
async fn media_info_plugin_config_exposes_libraries_and_drives_settings()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join("plugins/org.lux.strm-media-info/binaries");
    tokio::fs::create_dir_all(&plugin_dir).await?;
    let entrypoint = plugin_dir.join("plugin");
    fs::write(&entrypoint, b"placeholder")?;
    let manifest = json!({
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
    });
    tokio::fs::write(
        config_dir.join("plugins/org.lux.strm-media-info/manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
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
    let plugins = PluginService::new(database.clone(), config_dir);
    plugins.install(MEDIA_INFO_PLUGIN_ID).await?;

    let task: (String, Option<String>, i64, String, Option<String>) = sqlx::query_as(
        "SELECT task_type, cron_or_interval, is_enabled, source_type, plugin_id
         FROM scheduled_task_configs
         WHERE owner_type = 'GLOBAL' AND owner_id = 'global' AND task_type = 'STRM_MEDIA_INFO'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        task,
        (
            "STRM_MEDIA_INFO".to_owned(),
            Some("24h".to_owned()),
            0,
            "PLUGIN".to_owned(),
            Some(MEDIA_INFO_PLUGIN_ID.to_owned())
        )
    );

    let page = plugins.list_installed(0, 20).await?;
    let plugin = page
        .plugins
        .iter()
        .find(|plugin| plugin.id == MEDIA_INFO_PLUGIN_ID)
        .ok_or("media info plugin missing")?;
    let library_field = plugin
        .config_fields
        .iter()
        .find(|field| field.key == "libraryIds")
        .ok_or("libraryIds config field missing")?;
    assert_eq!(library_field.options[0].value, library.id.to_string());
    assert!(!plugin.configured);

    let values = Map::from_iter([
        ("libraryIds".to_owned(), json!([library.id.to_string()])),
        ("concurrency".to_owned(), json!(4)),
        (
            "existingInfoPolicy".to_owned(),
            Value::String("OVERWRITE".to_owned()),
        ),
        ("writeSidecars".to_owned(), Value::Bool(false)),
        ("schedule".to_owned(), Value::String("6h".to_owned())),
    ]);
    let updated = plugins
        .update_dynamic_config(MEDIA_INFO_PLUGIN_ID, values)
        .await?;
    assert!(updated.configured);
    assert_eq!(updated.config_values["concurrency"], 4);
    assert_eq!(updated.config_values["writeSidecars"], false);

    let enabled: (Option<String>, i64) = sqlx::query_as(
        "SELECT cron_or_interval, is_enabled
         FROM scheduled_task_configs
         WHERE owner_type = 'GLOBAL' AND owner_id = 'global' AND task_type = 'STRM_MEDIA_INFO'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(enabled, (Some("6h".to_owned()), 1));

    plugins.set_enabled(MEDIA_INFO_PLUGIN_ID, false).await?;
    let disabled: i64 = sqlx::query_scalar(
        "SELECT is_enabled FROM scheduled_task_configs
         WHERE owner_type = 'GLOBAL' AND owner_id = 'global' AND task_type = 'STRM_MEDIA_INFO'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(disabled, 0);

    plugins.set_enabled(MEDIA_INFO_PLUGIN_ID, true).await?;
    let reenabled: i64 = sqlx::query_scalar(
        "SELECT is_enabled FROM scheduled_task_configs
         WHERE owner_type = 'GLOBAL' AND owner_id = 'global' AND task_type = 'STRM_MEDIA_INFO'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(reenabled, 1);

    let settings = plugins.media_info_settings().await?;
    assert_eq!(settings.library_ids, vec![library.id]);
    assert_eq!(settings.concurrency, 4);
    assert!(settings.include_ready);
    assert!(!settings.write_sidecars);
    assert_eq!(settings.schedule, "6h");

    let invalid = Map::from_iter([
        ("libraryIds".to_owned(), json!([library.id.to_string()])),
        ("concurrency".to_owned(), json!(4)),
        ("existingInfoPolicy".to_owned(), json!("SKIP")),
        ("writeSidecars".to_owned(), json!(true)),
        ("schedule".to_owned(), json!("59s")),
    ]);
    assert!(
        plugins
            .update_dynamic_config(MEDIA_INFO_PLUGIN_ID, invalid)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn media_info_plugin_migrates_legacy_include_ready_configuration()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join("plugins/org.lux.strm-media-info/binaries");
    tokio::fs::create_dir_all(&plugin_dir).await?;
    fs::write(plugin_dir.join("plugin"), b"placeholder")?;
    let manifest = json!({
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
            {"key": "writeSidecars", "label": "写入旁车", "type": "toggle", "defaultValue": true}
        ],
        "permissions": {"network": ["media-source"], "filesystem": []},
        "files": []
    });
    tokio::fs::write(
        config_dir.join("plugins/org.lux.strm-media-info/manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
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
    let plugins = PluginService::new(database, config_dir.clone());
    plugins.install(MEDIA_INFO_PLUGIN_ID).await?;
    tokio::fs::create_dir_all(config_dir.join("plugin-config")).await?;
    tokio::fs::write(
        config_dir.join("plugin-config/org.lux.media-info.json"),
        serde_json::to_vec(&json!({
            "libraryIds": [library.id.to_string()],
            "concurrency": 3,
            "includeReady": true,
            "writeSidecars": false
        }))?,
    )
    .await?;

    let page = plugins.list_installed(0, 20).await?;
    let plugin = page
        .plugins
        .iter()
        .find(|plugin| plugin.id == MEDIA_INFO_PLUGIN_ID)
        .ok_or("media info plugin missing")?;
    assert_eq!(plugin.config_values["existingInfoPolicy"], "OVERWRITE");
    assert!(
        config_dir
            .join("plugin-config/org.lux.strm-media-info.json")
            .is_file()
    );

    let settings = plugins.media_info_settings().await?;
    assert_eq!(settings.library_ids, vec![library.id]);
    assert!(settings.include_ready);
    assert!(!settings.write_sidecars);
    assert_eq!(settings.schedule, "24h");
    Ok(())
}
