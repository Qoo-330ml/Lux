use std::{path::Path, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use luxd::application::danmaku::{
    DanmakuService, DanmakuServiceError, DanmakuSidecarScope, MAX_DANMAKU_XML_BYTES,
    atomic_write_danmaku_xml, danmaku_sidecar_path, validate_danmaku_xml,
    validate_provider_base_url,
};
use luxd::application::{
    libraries::LibraryService,
    plugin_protocol::{
        DANMAKU_FETCH_CAPABILITY, DANMAKU_MATCH_CAPABILITY, DANMAKU_SEARCH_CAPABILITY,
    },
    plugin_runtime::PluginRuntimeError,
    plugins::{DANMAKU_PLUGIN_ID, PluginService, PluginServiceError},
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
    let replacement = br#"<i><d p="2,1,25,16777215,0,0,0,0">replacement</d></i>"#;
    atomic_write_danmaku_xml(&target, replacement, true).await?;
    assert_eq!(tokio::fs::read(&target).await?, replacement);
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
    assert!(database.schema_version().await? >= 190);
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

/// Bilibili XML the stub plugin returns for a matched item.
#[cfg(unix)]
const STUB_XML: &[u8] = b"<i><d p=\"1,1,25,16777215,0,0,0,0\">hello</d></i>";

/// Minimal `org.lux.danmaku` stand-in for the host-side end-to-end test.
///
/// The real plugin process is built, packaged and protocol-tested in the
/// `Lux-plugins` repository. This stub speaks the same JSON-lines protocol and
/// answers only the methods the host uses, so the host keeps coverage of the
/// RPC boundary, the job pipeline and the sidecar write without shipping the
/// plugin binary. It matches the two file names the test indexes and reports
/// `NO_MATCH` for anything else, which keeps the "the host sends the source
/// file name" expectation the provider mock used to assert.
#[cfg(unix)]
const DANMAKU_STUB_SCRIPT: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"plugin.hello"'*)
      printf '{"id":"%s","result":{"id":"org.lux.danmaku","name":"danmaku stub","apiVersion":1,"capabilities":["danmaku.match"],"supportedItemTypes":[]}}\n' "$id"
      ;;
    *'"danmaku.match"'*)
      case "$line" in
        *'"fileName":"Demo.Movie.2024.mkv"'*|*'"fileName":"Remote.Movie.2024.strm"'*)
          printf '{"id":"%s","result":{"status":"MATCHED","provider":"stub","episodeId":"34","xmlBase64":"__XML_BASE64__"}}\n' "$id"
          ;;
        *)
          printf '{"id":"%s","result":{"status":"NO_MATCH"}}\n' "$id"
          ;;
      esac
      ;;
    *'"plugin.shutdown"'*)
      printf '{"id":"%s","result":{"accepted":true}}\n' "$id"
      exit 0
      ;;
    *'"plugin.health"'*)
      printf '{"id":"%s","result":{"available":true,"configured":true}}\n' "$id"
      ;;
    *)
      printf '{"id":"%s","error":{"code":"PLUGIN_UNSUPPORTED_METHOD","message":"unsupported plugin method"}}\n' "$id"
      ;;
  esac
done
"#;

#[cfg(unix)]
fn write_danmaku_stub_plugin(plugin_root: &Path, script: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(plugin_root)?;
    let entrypoint = plugin_root.join("plugin");
    std::fs::write(&entrypoint, script)?;
    std::fs::set_permissions(entrypoint, std::fs::Permissions::from_mode(0o700))
}

/// Minimal stand-in for a danmaku plugin that also answers the optional manual-match methods
/// (`danmaku.search` / `danmaku.fetch`, ADR-052 P4).
///
/// The query text selects the contract under test: `slow` blocks past the host deadline,
/// `blank-title` returns a candidate the host must reject, and every other query returns one
/// candidate with two episodes. Fetch always answers with the same XML the match stub uses, so the
/// host-side decode and `validate_danmaku_xml` path is covered without a real provider.
#[cfg(unix)]
const DANMAKU_MANUAL_STUB_SCRIPT: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/^{"id":"\([^"]*\)".*/\1/p')
  case "$line" in
    *'"plugin.hello"'*)
      printf '{"id":"%s","result":{"id":"org.lux.danmaku","name":"danmaku stub","apiVersion":1,"capabilities":["danmaku.match","danmaku.search","danmaku.fetch"],"supportedItemTypes":[]}}\n' "$id"
      ;;
    *'"danmaku.match"'*)
      printf '{"id":"%s","result":{"status":"MATCHED","provider":"stub","episodeId":"ep-1","xmlBase64":"__XML_BASE64__"}}\n' "$id"
      ;;
    *'"danmaku.search"'*)
      case "$line" in
        *'"query":"huge"'*)
          filler=$(head -c 4300000 /dev/zero | tr '\0' 'a')
          printf '{"id":"%s","result":{"candidates":[{"animeId":"a1","animeTitle":"%s","episodes":[]}]}}\n' "$id" "$filler"
          ;;
        *'"query":"slow"'*)
          sleep 5
          printf '{"id":"%s","result":{"candidates":[]}}\n' "$id"
          ;;
        *'"query":"blank-title"'*)
          printf '{"id":"%s","result":{"candidates":[{"provider":"stub","animeId":"anime-1","animeTitle":"   ","episodes":[{"episodeId":"ep-1","title":"Episode 1"}]}]}}\n' "$id"
          ;;
        *)
          printf '{"id":"%s","result":{"candidates":[{"provider":"stub","animeId":"anime-1","animeTitle":"Demo Anime","episodes":[{"episodeId":"ep-1","title":"Episode 1"},{"episodeId":"ep-2","title":"Episode 2"}]}]}}\n' "$id"
          ;;
      esac
      ;;
    *'"danmaku.fetch"'*)
      case "$line" in
        *'"episodeId":"huge"'*)
          filler=$(head -c 4300000 /dev/zero | tr '\0' 'a')
          printf '{"id":"%s","result":{"xmlBase64":"%s"}}\n' "$id" "$filler"
          ;;
        *'"episodeId":"bad-xml"'*)
          printf '{"id":"%s","result":{"xmlBase64":"PGh0bWwvPg=="}}\n' "$id"
          ;;
        *)
          printf '{"id":"%s","result":{"xmlBase64":"__XML_BASE64__"}}\n' "$id"
          ;;
      esac
      ;;
    *'"plugin.shutdown"'*)
      printf '{"id":"%s","result":{"accepted":true}}\n' "$id"
      exit 0
      ;;
    *'"plugin.health"'*)
      printf '{"id":"%s","result":{"available":true,"configured":true}}\n' "$id"
      ;;
    *)
      printf '{"id":"%s","error":{"code":"PLUGIN_UNSUPPORTED_METHOD","message":"unsupported plugin method"}}\n' "$id"
      ;;
  esac
done
"#;

/// The `org.lux.danmaku` manifest the host validates, with a caller-selected capability set.
#[cfg(unix)]
fn danmaku_manifest_value(capabilities: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "formatVersion": 1,
        "id": DANMAKU_PLUGIN_ID,
        "name": "弹幕匹配",
        "version": "1.0.0",
        "apiVersion": 1,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "danmaku",
        "category": "MEDIA",
        "capabilities": capabilities,
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
    })
}

/// Installs the stub danmaku plugin with `capabilities` and returns a ready service.
#[cfg(unix)]
async fn danmaku_plugin_service(
    directory: &Path,
    capabilities: &[&str],
    script: &str,
) -> Result<PluginService, Box<dyn std::error::Error>> {
    let config_dir = directory.join("config");
    let plugin_dir = config_dir.join("plugins/org.lux.danmaku");
    std::fs::create_dir_all(plugin_dir.join("binaries"))?;
    std::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec(&danmaku_manifest_value(capabilities))?,
    )?;
    write_danmaku_stub_plugin(&plugin_dir.join("binaries"), script)?;
    std::fs::create_dir_all(config_dir.join("plugin-config"))?;
    std::fs::write(
        config_dir.join("plugin-config/org.lux.danmaku.json"),
        serde_json::to_vec(&serde_json::json!({
            "providerBaseUrl": "https://danmu.example.test",
            "schedule": "* * * * *"
        }))?,
    )?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    })
    .await?;
    let plugins = PluginService::new(database, config_dir);
    plugins.install(DANMAKU_PLUGIN_ID).await?;
    Ok(plugins)
}

#[cfg(unix)]
fn danmaku_manual_plugin_script() -> String {
    let base64 = BASE64.encode(STUB_XML);
    DANMAKU_MANUAL_STUB_SCRIPT.replace("__XML_BASE64__", &base64)
}

#[cfg(unix)]
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

    write_danmaku_stub_plugin(
        &config_dir.join("plugins/org.lux.danmaku/binaries"),
        &DANMAKU_STUB_SCRIPT.replace("__XML_BASE64__", &BASE64.encode(STUB_XML)),
    )?;
    tokio::fs::write(
        config_dir.join("plugins/org.lux.danmaku/manifest.json"),
        serde_json::to_vec(&danmaku_manifest_value(&["danmaku.match"]))?,
    )
    .await?;
    tokio::fs::create_dir_all(config_dir.join("plugin-config")).await?;
    tokio::fs::write(
        config_dir.join("plugin-config/org.lux.danmaku.json"),
        serde_json::to_vec(&serde_json::json!({
            "providerBaseUrl": "https://danmu.example.test",
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
    assert_eq!(completed.status, "COMPLETED", "{completed:?}");
    assert_eq!(completed.success_count, 2);
    assert_eq!(
        tokio::fs::read(media_root.join("Demo.Movie.2024.xml")).await?,
        STUB_XML
    );
    assert_eq!(
        tokio::fs::read(media_root.join("Remote.Movie.2024.xml")).await?,
        STUB_XML
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
        .read_for_playback(
            &strm_item_id,
            Some(&strm_source_id),
            DanmakuSidecarScope::RegisteredTrack,
        )
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

    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_manual_match_plugin_serves_search_and_fetch_inside_the_host_bounds()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let plugins = danmaku_plugin_service(
        directory.path(),
        &[
            DANMAKU_MATCH_CAPABILITY,
            DANMAKU_SEARCH_CAPABILITY,
            DANMAKU_FETCH_CAPABILITY,
        ],
        &danmaku_manual_plugin_script(),
    )
    .await?;

    assert!(plugins.danmaku_manual_match_supported().await);

    let search = plugins.search_danmaku("Demo", 50).await?;
    assert_eq!(search.candidates.len(), 1);
    assert_eq!(search.candidates[0].provider.as_deref(), Some("stub"));
    assert_eq!(search.candidates[0].anime_id, "anime-1");
    assert_eq!(search.candidates[0].anime_title, "Demo Anime");
    assert_eq!(search.candidates[0].episodes.len(), 2);
    assert_eq!(search.candidates[0].episodes[1].episode_id, "ep-2");

    let fetched = plugins.fetch_danmaku(Some("stub"), "ep-1").await?;
    assert_eq!(fetched.decode_xml()?, STUB_XML);
    let without_provider = plugins.fetch_danmaku(None, "ep-2").await?;
    assert_eq!(without_provider.decode_xml()?, STUB_XML);
    // "No provider" also arrives as an empty or blank label from a form field; neither may fail a
    // fetch the host can already serve.
    assert_eq!(
        plugins
            .fetch_danmaku(Some(""), "ep-1")
            .await?
            .decode_xml()?,
        STUB_XML
    );
    assert_eq!(
        plugins
            .fetch_danmaku(Some("   "), "ep-2")
            .await?
            .decode_xml()?,
        STUB_XML
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn unusable_plugin_payloads_become_an_invalid_response()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let plugins = danmaku_plugin_service(
        directory.path(),
        &[
            DANMAKU_MATCH_CAPABILITY,
            DANMAKU_SEARCH_CAPABILITY,
            DANMAKU_FETCH_CAPABILITY,
        ],
        &danmaku_manual_plugin_script(),
    )
    .await?;

    // A candidate whose title trims to empty is a broken plugin response, not extra data.
    assert!(matches!(
        plugins.search_danmaku("blank-title", 10).await,
        Err(PluginServiceError::InvalidResponse)
    ));
    // The fetch layer still applies the host XML rules before handing bytes to its caller.
    assert!(matches!(
        plugins.fetch_danmaku(None, "bad-xml").await,
        Err(PluginServiceError::InvalidResponse)
    ));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_match_only_plugin_keeps_working_and_reports_manual_search_as_unsupported()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let plugins = danmaku_plugin_service(
        directory.path(),
        &[DANMAKU_MATCH_CAPABILITY],
        &DANMAKU_STUB_SCRIPT.replace("__XML_BASE64__", &BASE64.encode(STUB_XML)),
    )
    .await?;

    assert!(!plugins.danmaku_manual_match_supported().await);
    assert!(matches!(
        plugins.search_danmaku("Demo", 10).await,
        Err(PluginServiceError::Unsupported(DANMAKU_SEARCH_CAPABILITY))
    ));
    assert!(matches!(
        plugins.fetch_danmaku(None, "ep-1").await,
        Err(PluginServiceError::Unsupported(DANMAKU_SEARCH_CAPABILITY))
    ));
    // The automatic contract must not change for plugins that never declared search or fetch.
    let matched = plugins.match_danmaku("Demo.Movie.2024.mkv").await?;
    assert_eq!(matched.episode_id.as_deref(), Some("34"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn oversized_plugin_answers_are_an_invalid_response_not_a_runtime_outage()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let plugins = danmaku_plugin_service(
        directory.path(),
        &[
            DANMAKU_MATCH_CAPABILITY,
            DANMAKU_SEARCH_CAPABILITY,
            DANMAKU_FETCH_CAPABILITY,
        ],
        &danmaku_manual_plugin_script(),
    )
    .await?;

    // The transport caps one plugin line at 4 MiB. A plugin that answers larger than that answered
    // with an unusable payload, not with a crashed runtime, so both manual methods must report the
    // invalid-response class (`PLUGIN_INVALID_RESPONSE`) instead of the runtime/unavailable one.
    assert!(matches!(
        plugins.search_danmaku("huge", 10).await,
        Err(PluginServiceError::InvalidResponse)
    ));
    assert!(matches!(
        plugins.fetch_danmaku(None, "huge").await,
        Err(PluginServiceError::InvalidResponse)
    ));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn manual_search_reports_the_host_plugin_timeout() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let plugins = danmaku_plugin_service(
        directory.path(),
        &[
            DANMAKU_MATCH_CAPABILITY,
            DANMAKU_SEARCH_CAPABILITY,
            DANMAKU_FETCH_CAPABILITY,
        ],
        &danmaku_manual_plugin_script(),
    )
    .await?;
    let plugins = plugins.with_danmaku_call_timeout(Duration::from_millis(300));

    assert!(matches!(
        plugins.search_danmaku("slow", 10).await,
        Err(PluginServiceError::Runtime(PluginRuntimeError::Timeout))
    ));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn manual_requests_are_validated_before_the_plugin_is_consulted()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let config_dir = directory.path().join("config");
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    })
    .await?;
    let plugins = PluginService::new(database, config_dir);

    assert!(!plugins.danmaku_manual_match_supported().await);
    for (query, limit) in [("   ", 10), ("Demo", 0), ("Demo", 51)] {
        assert!(
            matches!(
                plugins.search_danmaku(query, limit).await,
                Err(PluginServiceError::InvalidRequest(_))
            ),
            "query {query:?} limit {limit} must be rejected before the plugin is consulted"
        );
    }
    assert!(matches!(
        plugins.fetch_danmaku(None, "").await,
        Err(PluginServiceError::InvalidRequest(_))
    ));
    // Valid input without an installed plugin keeps a specific variant instead of panicking.
    assert!(matches!(
        plugins.search_danmaku("Demo", 10).await,
        Err(PluginServiceError::UnknownPlugin(_))
    ));
    assert!(matches!(
        plugins.fetch_danmaku(None, "ep-1").await,
        Err(PluginServiceError::UnknownPlugin(_))
    ));
    Ok(())
}
