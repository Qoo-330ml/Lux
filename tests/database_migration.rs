use std::collections::HashSet;

use luxd::{
    config::Config,
    storage::{
        Database,
        migration::{
            MIGRATION_TABLES, MigrationOptions, connection_from_environment,
            is_excluded_sqlite_table, normalized_job_tables, validate_batch_size,
        },
    },
};

#[test]
fn migration_plan_is_unique_and_orders_core_dependencies() {
    let names: Vec<_> = MIGRATION_TABLES.iter().map(|table| table.name).collect();
    let unique: HashSet<_> = names.iter().copied().collect();
    assert_eq!(unique.len(), names.len());

    assert_before(&names, "users", "web_sessions");
    assert_before(&names, "libraries", "library_roots");
    assert_before(&names, "library_roots", "filesystem_entries");
    assert_before(&names, "media_items", "media_sources");
    assert_before(&names, "media_sources", "media_streams");
    assert_before(&names, "scan_jobs", "scan_job_events");
    assert_before(&names, "collections", "collection_items");
}

#[test]
fn migration_options_reject_unsafe_batch_sizes() {
    assert!(validate_batch_size(0).is_err());
    assert!(validate_batch_size(10_001).is_err());
    assert_eq!(validate_batch_size(500).unwrap(), 500);

    let options = MigrationOptions::new("/config/lux.db".into(), 500).unwrap();
    assert_eq!(options.source.display().to_string(), "/config/lux.db");
    assert_eq!(options.batch_size, 500);
}

#[test]
fn postgres_migration_environment_requires_every_field_without_echoing_values() {
    let result = connection_from_environment(|name| match name {
        "LUX_MIGRATE_POSTGRES_HOST" => Some("postgres.internal".to_owned()),
        "LUX_MIGRATE_POSTGRES_PORT" => Some("5432".to_owned()),
        "LUX_MIGRATE_POSTGRES_DATABASE" => Some("lux".to_owned()),
        "LUX_MIGRATE_POSTGRES_USER" => Some("lux".to_owned()),
        "LUX_MIGRATE_POSTGRES_PASSWORD" => None,
        "LUX_MIGRATE_POSTGRES_SSL_MODE" => Some("require".to_owned()),
        _ => None,
    });
    let error = result.unwrap_err().to_string();
    assert!(error.contains("LUX_MIGRATE_POSTGRES_PASSWORD"));
    assert!(!error.contains("postgres.internal"));
}

#[test]
fn migration_plan_excludes_migrations_and_sqlite_search_tables() {
    for table in [
        "_sqlx_migrations",
        "media_search",
        "media_search_data",
        "media_search_idx",
        "media_search_content",
        "media_search_docsize",
        "media_search_config",
        "sqlite_sequence",
    ] {
        assert!(is_excluded_sqlite_table(table), "{table}");
        assert!(!MIGRATION_TABLES.iter().any(|entry| entry.name == table));
    }
}

#[test]
fn all_persistent_job_families_have_state_normalization() {
    let tables = normalized_job_tables();
    for expected in [
        "scan_jobs",
        "metadata_reidentify_jobs",
        "strm_probe_jobs",
        "danmaku_match_jobs",
    ] {
        assert!(tables.contains(&expected), "{expected}");
    }
}

#[tokio::test]
async fn migration_plan_covers_every_sqlite_business_table()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:0".parse()?,
        config_dir: temp.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let actual: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )
    .fetch_all(database.pool())
    .await?;

    let planned: HashSet<_> = MIGRATION_TABLES.iter().map(|table| table.name).collect();
    let missing: Vec<_> = actual
        .iter()
        .filter(|name| !is_excluded_sqlite_table(name) && !planned.contains(name.as_str()))
        .collect();
    assert!(missing.is_empty(), "unplanned SQLite tables: {missing:?}");

    for table in MIGRATION_TABLES {
        let primary_key_columns: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info(?) WHERE pk > 0")
                .bind(table.name)
                .fetch_one(database.pool())
                .await?;
        assert!(
            primary_key_columns > 0,
            "migration table has no primary key: {}",
            table.name
        );
    }
    database.close().await;
    Ok(())
}

fn assert_before(names: &[&str], parent: &str, child: &str) {
    let parent_index = names.iter().position(|name| *name == parent).unwrap();
    let child_index = names.iter().position(|name| *name == child).unwrap();
    assert!(parent_index < child_index, "{parent} must precede {child}");
}
