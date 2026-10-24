use std::{collections::BTreeMap, ffi::OsStr, fs, path::Path};

use sha2::{Digest, Sha384};

use luxd::{
    application::{libraries::LibraryService, scanner::LibraryScanner},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use sqlx::{
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};

fn latest_migration_version(directory: &str) -> Result<i64, Box<dyn std::error::Error>> {
    let migration_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(directory);
    let mut latest_version = None;
    for entry in fs::read_dir(migration_dir)? {
        let path = entry?.path();
        if path.extension() != Some(OsStr::new("sql")) {
            continue;
        }
        let Some(version) = path
            .file_stem()
            .and_then(OsStr::to_str)
            .and_then(|stem| stem.split('_').next())
            .and_then(|version| version.parse::<i64>().ok())
        else {
            continue;
        };
        latest_version = Some(latest_version.map_or(version, |latest: i64| latest.max(version)));
    }
    latest_version.ok_or_else(|| "migration directory contains no numbered SQL files".into())
}

#[tokio::test]
async fn user_activity_events_migration_runs_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    assert!(database.schema_version().await? >= 196);

    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'user_activity_events'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(table_count, 1);

    let required_columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('user_activity_events')
         WHERE name IN (
             'id', 'user_id', 'actor_user_id', 'event_type', 'target_type',
             'target_id', 'metadata_json', 'occurred_at'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(required_columns, 8);

    for index_name in [
        "idx_user_activity_events_user_time",
        "idx_user_activity_events_user_type_time",
        "idx_user_activity_events_retention",
    ] {
        let index_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?",
        )
        .bind(index_name)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(index_count, 1, "missing index {index_name}");
    }

    let foreign_key_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_list('user_activity_events')")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(foreign_key_count, 0);

    sqlx::query(
        "INSERT INTO user_activity_events (id, user_id, event_type)
         VALUES ('activity-migration-login', 'deleted-user', 'AUTH_LOGIN')",
    )
    .execute(database.pool())
    .await?;
    let (actor_user_id, target_id, occurred_at): (Option<String>, Option<String>, i64) =
        sqlx::query_as(
            "SELECT actor_user_id, target_id, occurred_at
             FROM user_activity_events WHERE id = 'activity-migration-login'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(actor_user_id, None);
    assert_eq!(target_id, None);
    assert!(occurred_at > 0);

    let postgres_migration = include_str!("../migrations-postgres/0193_user_activity_events.sql");
    assert!(postgres_migration.contains("user_id TEXT NOT NULL"));
    assert!(postgres_migration.contains("actor_user_id TEXT"));
    assert!(postgres_migration.contains("occurred_at BIGINT NOT NULL"));
    assert!(postgres_migration.contains("idx_user_activity_events_user_time"));
    assert!(postgres_migration.contains("idx_user_activity_events_user_type_time"));
    assert!(postgres_migration.contains("idx_user_activity_events_retention"));
    assert!(!postgres_migration.contains("REFERENCES users"));

    Ok(())
}

#[tokio::test]
async fn user_invitations_migration_runs_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    assert!(database.schema_version().await? >= 201);

    let column_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info('user_invitations')")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(column_count, 14);
    let foreign_key_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_list('user_invitations')")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(foreign_key_count, 2);
    let delete_set_null_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('user_invitations')
         WHERE \"table\" = 'users' AND on_delete = 'SET NULL'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(delete_set_null_count, 2);
    let pending_email_index: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'ux_user_invitations_pending_email' AND sql LIKE '%UNIQUE%'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(pending_email_index, 1);

    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('invitation-owner', 'invitation-owner', 'Owner', 'test-hash'),
                ('invitation-user', 'invitation-user', 'Invited', 'test-hash')",
    )
    .execute(database.pool())
    .await?;
    let invite_insert = "INSERT INTO user_invitations (
            id, recipient_email, created_by_user_id, accepted_user_id, status,
            preset_id, preset_name, preset_version, permission_snapshot_json,
            expires_at, accepted_at, revoked_at, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
    let insert_invitation = |id: &'static str, status: &'static str| {
        sqlx::query(invite_insert)
            .bind(id)
            .bind("invitee@example.test")
            .bind("invitation-owner")
            .bind(if status == "ACCEPTED" {
                Some("invitation-user")
            } else {
                None
            })
            .bind(status)
            .bind("viewer")
            .bind("Viewer")
            .bind(1_i64)
            .bind("{}")
            .bind(10_000_i64)
            .bind(if status == "ACCEPTED" {
                Some(20_i64)
            } else {
                None
            })
            .bind(None::<i64>)
            .bind(1_i64)
            .bind(1_i64)
    };
    insert_invitation("invitation-pending", "PENDING")
        .execute(database.pool())
        .await?;
    assert!(
        insert_invitation("invitation-duplicate", "PENDING")
            .execute(database.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query(invite_insert)
            .bind("invitation-invalid")
            .bind("bad@example.test")
            .bind("invitation-owner")
            .bind(None::<&str>)
            .bind("UNKNOWN")
            .bind("viewer")
            .bind("Viewer")
            .bind(1_i64)
            .bind("[]")
            .bind(10_000_i64)
            .bind(None::<i64>)
            .bind(None::<i64>)
            .bind(1_i64)
            .bind(1_i64)
            .execute(database.pool())
            .await
            .is_err()
    );

    sqlx::query(
        "UPDATE user_invitations
         SET status = 'ACCEPTED', accepted_user_id = 'invitation-user', accepted_at = 20
         WHERE id = 'invitation-pending'",
    )
    .execute(database.pool())
    .await?;
    insert_invitation("invitation-next", "PENDING")
        .execute(database.pool())
        .await?;
    sqlx::query("DELETE FROM users WHERE id IN ('invitation-owner', 'invitation-user')")
        .execute(database.pool())
        .await?;
    let preserved_links: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT created_by_user_id, accepted_user_id FROM user_invitations
         WHERE id = 'invitation-pending'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(preserved_links, (None, None));

    let postgres_migration = fs::read_to_string("migrations-postgres/0201_user_invitations.sql")?;
    assert!(postgres_migration.contains("permission_snapshot_json TEXT NOT NULL"));
    assert!(postgres_migration.contains("ON DELETE SET NULL"));
    assert!(postgres_migration.contains("ux_user_invitations_pending_email"));
    assert!(!postgres_migration.contains("token"));
    assert!(!postgres_migration.contains("password"));
    Ok(())
}

#[tokio::test]
async fn user_device_inventory_migration_runs_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    assert!(database.schema_version().await? >= 196);

    let session_columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('web_sessions') WHERE name = 'device_id'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(session_columns, 1);

    let required_columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('user_devices')
         WHERE name IN (
             'user_id', 'device_id', 'device_name', 'device_type', 'client_name',
             'client_version', 'first_seen_at', 'last_seen_at', 'last_remote_ip'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(required_columns, 9);

    let index_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'idx_user_devices_user_activity'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(index_count, 1);

    let foreign_key_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('user_devices')
         WHERE \"table\" = 'users' AND on_delete = 'CASCADE'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(foreign_key_count, 1);

    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('device-owner', 'device-owner', 'Device Owner', 'test-hash')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO web_sessions (
             id, user_id, session_token_hash, csrf_token_hash, expires_at
         ) VALUES ('legacy-session', 'device-owner', x'01', x'02', unixepoch() + 3600)",
    )
    .execute(database.pool())
    .await?;
    let legacy_device_id: Option<String> =
        sqlx::query_scalar("SELECT device_id FROM web_sessions WHERE id = 'legacy-session'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(legacy_device_id, None);
    sqlx::query(
        "INSERT INTO user_devices (
             user_id, device_id, client_name, first_seen_at, last_seen_at
         ) VALUES ('device-owner', 'browser-device', 'Lux Web', 10, 20)",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("DELETE FROM users WHERE id = 'device-owner'")
        .execute(database.pool())
        .await?;
    let cascaded_devices: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_devices WHERE user_id = 'device-owner'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(cascaded_devices, 0);

    let postgres_migration = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations-postgres/0194_user_devices.sql"),
    )?;
    for expected in [
        "CREATE TABLE user_devices",
        "REFERENCES users(id) ON DELETE CASCADE",
        "idx_user_devices_user_activity",
        "INSERT INTO user_devices",
        "last_remote_ip",
    ] {
        assert!(postgres_migration.contains(expected), "missing {expected}");
    }
    assert_eq!(
        include_str!("../migrations/0195_web_session_device_id.sql"),
        include_str!("../migrations-postgres/0195_web_session_device_id.sql")
    );

    Ok(())
}

#[tokio::test]
async fn user_access_blacklist_rules_migration_runs_from_empty_and_existing_databases()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    assert!(database.schema_version().await? >= 196);

    let columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('user_access_blacklist_rules')
         WHERE name IN (
             'id', 'user_id', 'rule_type', 'match_value', 'is_enabled',
             'created_by', 'created_at', 'updated_at'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(columns, 8);
    for index_name in [
        "idx_user_access_blacklist_global_active",
        "idx_user_access_blacklist_user_active",
        "uq_user_access_blacklist_global_value",
        "uq_user_access_blacklist_user_value",
    ] {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?",
        )
        .bind(index_name)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(count, 1, "missing index {index_name}");
    }
    let foreign_keys: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('user_access_blacklist_rules')
         WHERE \"table\" = 'users'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(foreign_keys, 2);

    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('blacklist-owner', 'blacklist-owner', 'Blacklist Owner', 'test-hash'),
                ('blacklist-admin', 'blacklist-admin', 'Blacklist Admin', 'test-hash')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_access_blacklist_rules (
             id, user_id, rule_type, match_value, created_by
         ) VALUES ('user-device-rule', 'blacklist-owner', 'DEVICE', 'blocked-device', 'blacklist-admin'),
                  ('global-client-rule', NULL, 'CLIENT', 'infuse', 'blacklist-admin'),
                  ('global-ip-rule', NULL, 'IP_CIDR', '192.0.2.0/24', 'blacklist-admin')",
    )
    .execute(database.pool())
    .await?;
    assert!(
        sqlx::query(
            "INSERT INTO user_access_blacklist_rules (
             id, rule_type, match_value, created_by
         ) VALUES ('duplicate-global-rule', 'CLIENT', 'infuse', 'blacklist-admin')",
        )
        .execute(database.pool())
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO user_access_blacklist_rules (
             id, rule_type, match_value, created_by
         ) VALUES ('invalid-kind-rule', 'OPERATING_SYSTEM', 'macos', 'blacklist-admin')",
        )
        .execute(database.pool())
        .await
        .is_err()
    );
    sqlx::query("DELETE FROM users WHERE id = 'blacklist-owner'")
        .execute(database.pool())
        .await?;
    sqlx::query("DELETE FROM users WHERE id = 'blacklist-admin'")
        .execute(database.pool())
        .await?;
    let (user_scoped_rules, global_rules, attributed_rules): (i64, i64, i64) = sqlx::query_as(
        "SELECT
             SUM(CASE WHEN user_id IS NOT NULL THEN 1 ELSE 0 END),
             SUM(CASE WHEN user_id IS NULL THEN 1 ELSE 0 END),
             SUM(CASE WHEN created_by IS NOT NULL THEN 1 ELSE 0 END)
         FROM user_access_blacklist_rules",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(user_scoped_rules, 0);
    assert_eq!(global_rules, 2);
    assert_eq!(attributed_rules, 0);

    let migration_dir = temp_dir.path().join("migrations-v195");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let entry = entry?;
        let filename = entry.file_name();
        let version: i64 = filename
            .to_str()
            .ok_or("invalid migration filename")?
            .split('_')
            .next()
            .ok_or("missing migration version")?
            .parse()?;
        if version <= 195 {
            fs::copy(entry.path(), migration_dir.join(filename))?;
        }
    }
    let legacy_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(temp_dir.path().join("upgrade-v195.db"))
                .create_if_missing(true),
        )
        .await?;
    Migrator::new(migration_dir.clone())
        .await?
        .run(&legacy_pool)
        .await?;
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('legacy-owner', 'legacy-owner', 'Legacy Owner', 'test-hash')",
    )
    .execute(&legacy_pool)
    .await?;
    sqlx::query(
        "INSERT INTO user_devices (
             user_id, device_id, client_name, first_seen_at, last_seen_at
         ) VALUES ('legacy-owner', 'legacy-device', 'Lux Web', 10, 20)",
    )
    .execute(&legacy_pool)
    .await?;
    fs::copy(
        source_dir.join("0196_user_access_blacklist_rules.sql"),
        migration_dir.join("0196_user_access_blacklist_rules.sql"),
    )?;
    Migrator::new(migration_dir)
        .await?
        .run(&legacy_pool)
        .await?;
    let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&legacy_pool)
        .await?;
    let preserved_devices: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_devices WHERE user_id = 'legacy-owner' AND device_id = 'legacy-device'",
    )
    .fetch_one(&legacy_pool)
    .await?;
    assert_eq!(version, 196);
    assert_eq!(preserved_devices, 1);
    Ok(())
}

#[tokio::test]
async fn web_session_device_id_migration_preserves_existing_sessions()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("migrations-v194");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let entry = entry?;
        let filename = entry.file_name();
        let version: i64 = filename
            .to_str()
            .ok_or("invalid migration filename")?
            .split('_')
            .next()
            .ok_or("missing migration version")?
            .parse()?;
        if version <= 194 {
            fs::copy(entry.path(), migration_dir.join(filename))?;
        }
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('legacy-session-owner', 'legacy-session-owner', 'Legacy Session', 'test-hash')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO web_sessions (
             id, user_id, session_token_hash, csrf_token_hash, expires_at
         ) VALUES (
             'legacy-session', 'legacy-session-owner', x'01', x'02', unixepoch() + 3600
         )",
    )
    .execute(&pool)
    .await?;
    fs::copy(
        source_dir.join("0195_web_session_device_id.sql"),
        migration_dir.join("0195_web_session_device_id.sql"),
    )?;
    Migrator::new(migration_dir).await?.run(&pool).await?;

    let device_id: Option<String> =
        sqlx::query_scalar("SELECT device_id FROM web_sessions WHERE id = 'legacy-session'")
            .fetch_one(&pool)
            .await?;
    let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(device_id, None);
    assert_eq!(version, 195);
    Ok(())
}

#[tokio::test]
async fn user_device_inventory_migration_backfills_legacy_emby_device_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("migrations");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?;
        if version.is_some_and(|version| version <= 193) {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing migration filename")?),
            )?;
        }
    }

    let database_path = temp_dir.path().join("devices-upgrade.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;
    let schema_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(schema_version, 193);

    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('device-owner', 'device-owner', 'Device Owner', 'test-hash'),
                ('other-owner', 'other-owner', 'Other Owner', 'test-hash')",
    )
    .execute(&pool)
    .await?;
    for (
        id,
        token_hash,
        user_id,
        device_id,
        client_name,
        device_name,
        version,
        created_at,
        last_seen_at,
        revoked_at,
    ) in [
        (
            "token-old",
            vec![1_u8],
            "device-owner",
            "shared-device",
            "Old Client",
            "Old Name",
            "1.0",
            100_i64,
            Some(120_i64),
            None,
        ),
        (
            "token-latest",
            vec![2_u8],
            "device-owner",
            "shared-device",
            "Current Client",
            "Current Name",
            "2.0",
            110_i64,
            Some(150_i64),
            None,
        ),
        (
            "token-revoked",
            vec![3_u8],
            "device-owner",
            "shared-device",
            "Revoked Client",
            "Revoked Name",
            "0.9",
            90_i64,
            Some(95_i64),
            Some(300_i64),
        ),
        (
            "token-other-user",
            vec![4_u8],
            "other-owner",
            "shared-device",
            "Other Client",
            "Other Device",
            "3.0",
            200_i64,
            Some(220_i64),
            None,
        ),
        (
            "token-empty-device",
            vec![5_u8],
            "device-owner",
            "",
            "Unknown Client",
            "Unknown Device",
            "0.1",
            50_i64,
            Some(60_i64),
            None,
        ),
    ] {
        sqlx::query(
            "INSERT INTO access_tokens (
                 id, token_hash, user_id, device_id, client_name, device_name,
                 client_version, created_at, updated_at, last_seen_at, revoked_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(token_hash)
        .bind(user_id)
        .bind(device_id)
        .bind(client_name)
        .bind(device_name)
        .bind(version)
        .bind(created_at)
        .bind(created_at + 10)
        .bind(last_seen_at)
        .bind(revoked_at)
        .execute(&pool)
        .await?;
    }

    fs::copy(
        source_dir.join("0194_user_devices.sql"),
        migration_dir.join("0194_user_devices.sql"),
    )?;
    Migrator::new(migration_dir).await?.run(&pool).await?;

    let (
        device_name,
        device_type,
        client_name,
        client_version,
        first_seen_at,
        last_seen_at,
        last_remote_ip,
    ): (
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        i64,
        i64,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT device_name, device_type, client_name, client_version,
                first_seen_at, last_seen_at, last_remote_ip
         FROM user_devices WHERE user_id = 'device-owner' AND device_id = 'shared-device'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(device_name.as_deref(), Some("Current Name"));
    assert_eq!(device_type, None);
    assert_eq!(client_name, "Current Client");
    assert_eq!(client_version.as_deref(), Some("2.0"));
    assert_eq!(first_seen_at, 90);
    assert_eq!(last_seen_at, 150);
    assert_eq!(last_remote_ip, None);

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_devices")
        .fetch_one(&pool)
        .await?;
    assert_eq!(rows, 2);
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn scan_job_processed_media_storage_is_migrated_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    assert!(database.schema_version().await? >= 190);

    let processed_media_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_jobs')
         WHERE name = 'processed_media_count'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(processed_media_column, 1);
    let (not_null, default_value): (i64, Option<String>) = sqlx::query_as(
        "SELECT \"notnull\", dflt_value FROM pragma_table_info('scan_jobs')
         WHERE name = 'processed_media_count'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        not_null, 0,
        "pre-migration jobs have no reliable media count"
    );
    assert_eq!(default_value.as_deref(), Some("0"));

    let ledger_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'scan_job_processed_media_items'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(ledger_table, 1);

    let job_foreign_key: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('scan_job_processed_media_items')
         WHERE \"table\" = 'scan_jobs' AND on_delete = 'CASCADE'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(job_foreign_key, 1);

    let postgres_migration =
        include_str!("../migrations-postgres/0183_scan_job_processed_media.sql");
    assert!(postgres_migration.contains("processed_media_count BIGINT"));
    assert!(postgres_migration.contains("scan_job_processed_media_items"));
    assert!(postgres_migration.contains("ON DELETE CASCADE"));
    assert!(postgres_migration.contains("UPDATE scan_jobs SET processed_media_count = NULL"));

    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Progress migration", LibraryKind::Movie, false)
        .await?;
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('progress-migration-job', ?, 'RECONCILE_LIBRARY', 'COMPLETED', 'g')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_job_processed_media_items (job_id, media_item_id)
         VALUES ('progress-migration-job', 'movie-1')",
    )
    .execute(database.pool())
    .await?;
    let duplicate_insert = sqlx::query(
        "INSERT INTO scan_job_processed_media_items (job_id, media_item_id)
         VALUES ('progress-migration-job', 'movie-1') ON CONFLICT DO NOTHING",
    )
    .execute(database.pool())
    .await?;
    assert_eq!(duplicate_insert.rows_affected(), 0);
    sqlx::query("DELETE FROM scan_jobs WHERE id = 'progress-migration-job'")
        .execute(database.pool())
        .await?;
    let remaining_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_job_processed_media_items
         WHERE job_id = 'progress-migration-job'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(remaining_items, 0);
    Ok(())
}

#[tokio::test]
async fn user_library_access_mode_migration_runs_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    assert!(database.schema_version().await? >= 190);
    let (not_null, default_value): (i64, Option<String>) = sqlx::query_as(
        "SELECT \"notnull\", dflt_value FROM pragma_table_info('users')
         WHERE name = 'library_access_mode'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(not_null, 1);
    assert_eq!(default_value.as_deref(), Some("'ALL'"));

    Ok(())
}

#[tokio::test]
async fn user_account_preferences_migration_runs_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    assert!(database.schema_version().await? >= 190);

    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'user_account_preferences'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(table_count, 1);
    let owner_cascade_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('user_account_preferences')
         WHERE \"table\" = 'users' AND on_delete = 'CASCADE'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(owner_cascade_count, 1);
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('account-preferences-owner', 'account-preferences-owner', 'Preferences', 'hash')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_account_preferences (user_id)
         VALUES ('account-preferences-owner')",
    )
    .execute(database.pool())
    .await?;
    let defaults: (String, String, i64, i64, String, String, i64, i64) = sqlx::query_as(
        "SELECT theme, accent_color, show_media_libraries, show_continue_watching,
                audio_language, subtitle_language, auto_play_next_episode, version
         FROM user_account_preferences WHERE user_id = 'account-preferences-owner'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        defaults,
        (
            "dark".to_owned(),
            "silver".to_owned(),
            1,
            1,
            "原始音轨".to_owned(),
            "简体中文".to_owned(),
            1,
            1,
        )
    );
    assert!(
        sqlx::query(
            "UPDATE user_account_preferences SET theme = 'sepia'
             WHERE user_id = 'account-preferences-owner'",
        )
        .execute(database.pool())
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE user_account_preferences SET accent_color = 'ultraviolet'
             WHERE user_id = 'account-preferences-owner'",
        )
        .execute(database.pool())
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE user_account_preferences SET version = 0
             WHERE user_id = 'account-preferences-owner'",
        )
        .execute(database.pool())
        .await
        .is_err()
    );
    sqlx::query("DELETE FROM users WHERE id = 'account-preferences-owner'")
        .execute(database.pool())
        .await?;
    let remaining_preferences: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_account_preferences
         WHERE user_id = 'account-preferences-owner'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(remaining_preferences, 0);
    Ok(())
}

#[tokio::test]
async fn user_content_restrictions_migration_runs_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    assert!(database.schema_version().await? >= 190);

    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('content-restriction-user', 'content-restriction-user', 'Content Restriction', 'test')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_content_restrictions
            (user_id, rating_system, maximum_rating, allow_unrated)
         VALUES ('content-restriction-user', 'MPAA', 'PG-13', 0)",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_content_restriction_tags (user_id, tag_kind, tag)
         VALUES ('content-restriction-user', 'ALLOW', 'family'),
                ('content-restriction-user', 'BLOCK', 'horror')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('invalid-rating-system', 'invalid-rating-system', 'Invalid Rating', 'test'),
                ('invalid-unrated-flag', 'invalid-unrated-flag', 'Invalid Unrated', 'test')",
    )
    .execute(database.pool())
    .await?;

    for invalid in [
        "INSERT INTO user_content_restrictions
            (user_id, rating_system, maximum_rating, allow_unrated)
         VALUES ('invalid-rating-system', '   ', NULL, 0)",
        "INSERT INTO user_content_restrictions
            (user_id, rating_system, maximum_rating, allow_unrated)
         VALUES ('invalid-unrated-flag', 'MPAA', NULL, 2)",
        "INSERT INTO user_content_restriction_tags (user_id, tag_kind, tag)
         VALUES ('content-restriction-user', 'NEUTRAL', 'other')",
        "INSERT INTO user_content_restriction_tags (user_id, tag_kind, tag)
         VALUES ('content-restriction-user', 'ALLOW', '   ')",
    ] {
        assert!(sqlx::query(invalid).execute(database.pool()).await.is_err());
    }
    assert!(
        sqlx::query(
            "INSERT INTO user_content_restriction_tags (user_id, tag_kind, tag)
             VALUES ('content-restriction-user', 'ALLOW', 'family')",
        )
        .execute(database.pool())
        .await
        .is_err()
    );

    let postgres_migration =
        include_str!("../migrations-postgres/0190_user_content_restrictions.sql");
    assert!(postgres_migration.contains("CREATE TABLE user_content_restrictions"));
    assert!(postgres_migration.contains("CREATE TABLE user_content_restriction_tags"));
    assert!(postgres_migration.contains("ON DELETE CASCADE"));

    sqlx::query("DELETE FROM users WHERE id = 'content-restriction-user'")
        .execute(database.pool())
        .await?;
    let remaining_policy_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_content_restrictions
         WHERE user_id = 'content-restriction-user'",
    )
    .fetch_one(database.pool())
    .await?;
    let remaining_tag_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_content_restriction_tags
         WHERE user_id = 'content-restriction-user'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(remaining_policy_rows, 0);
    assert_eq!(remaining_tag_rows, 0);
    Ok(())
}

#[tokio::test]
async fn media_content_restriction_index_migration_runs_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    assert!(database.schema_version().await? >= 192);

    let levels: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT rating_system, rating_code, rating_order
         FROM content_rating_levels ORDER BY rating_system, rating_order, rating_code",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(levels.len(), 18);
    assert_eq!(
        levels,
        [
            ("BBFC".to_owned(), "U".to_owned(), 0),
            ("BBFC".to_owned(), "PG".to_owned(), 1),
            ("BBFC".to_owned(), "12".to_owned(), 2),
            ("BBFC".to_owned(), "12A".to_owned(), 2),
            ("BBFC".to_owned(), "15".to_owned(), 3),
            ("BBFC".to_owned(), "18".to_owned(), 4),
            ("BBFC".to_owned(), "R18".to_owned(), 5),
            ("MPA".to_owned(), "G".to_owned(), 0),
            ("MPA".to_owned(), "PG".to_owned(), 1),
            ("MPA".to_owned(), "PG-13".to_owned(), 2),
            ("MPA".to_owned(), "R".to_owned(), 3),
            ("MPA".to_owned(), "NC-17".to_owned(), 4),
            ("TVPG".to_owned(), "TV-Y".to_owned(), 0),
            ("TVPG".to_owned(), "TV-G".to_owned(), 1),
            ("TVPG".to_owned(), "TV-Y7".to_owned(), 2),
            ("TVPG".to_owned(), "TV-PG".to_owned(), 3),
            ("TVPG".to_owned(), "TV-14".to_owned(), 4),
            ("TVPG".to_owned(), "TV-MA".to_owned(), 5),
        ]
    );
    let restriction_indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name IN (
             'idx_media_content_restriction_index_certification',
             'idx_media_content_restriction_tags_tag'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(restriction_indexes, 2);
    assert!(
        sqlx::query(
            "INSERT INTO content_rating_levels (rating_system, rating_code, rating_order)
             VALUES ('MPA', 'UNKNOWN', 5)",
        )
        .execute(database.pool())
        .await
        .is_err()
    );

    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('restriction-index-library', 'Restrictions', 'MOVIE')")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO media_items
            (id, library_id, item_type, title, sort_title, identification_status, nfo_metadata_json)
         VALUES ('restriction-index-item', 'restriction-index-library', 'MOVIE',
                 'Restriction', 'restriction', 'LOCAL_CONFIRMED', '{\"certification\":\"PG-13\"}')",
    )
    .execute(database.pool())
    .await?;
    let unindexed_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_content_restriction_index
         WHERE item_id = 'restriction-index-item'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(unindexed_rows, 0, "no row means the item is not ready");
    sqlx::query(
        "INSERT INTO media_content_restriction_index (item_id, certification_code)
         VALUES ('restriction-index-item', 'PG-13')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_content_restriction_tags (item_id, tag)
         VALUES ('restriction-index-item', 'family')",
    )
    .execute(database.pool())
    .await?;
    assert!(
        sqlx::query(
            "INSERT INTO media_content_restriction_tags (item_id, tag)
             VALUES ('restriction-index-item', '   ')",
        )
        .execute(database.pool())
        .await
        .is_err()
    );

    sqlx::query(
        "UPDATE media_items SET nfo_metadata_json = nfo_metadata_json
         WHERE id = 'restriction-index-item'",
    )
    .execute(database.pool())
    .await?;
    let unchanged_index_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_content_restriction_index
         WHERE item_id = 'restriction-index-item'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        unchanged_index_rows, 1,
        "unchanged NFO keeps the index ready"
    );

    sqlx::query(
        "UPDATE media_items SET nfo_metadata_json = '{\"certification\":\"R\"}'
         WHERE id = 'restriction-index-item'",
    )
    .execute(database.pool())
    .await?;
    let invalidated_index_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_content_restriction_index
         WHERE item_id = 'restriction-index-item'",
    )
    .fetch_one(database.pool())
    .await?;
    let invalidated_tag_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_content_restriction_tags
         WHERE item_id = 'restriction-index-item'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(invalidated_index_rows, 0);
    assert_eq!(invalidated_tag_rows, 0);

    sqlx::query(
        "INSERT INTO media_content_restriction_index (item_id, certification_code)
         VALUES ('restriction-index-item', 'R')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_content_restriction_tags (item_id, tag)
         VALUES ('restriction-index-item', 'family')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE media_items SET nfo_metadata_json = NULL WHERE id = 'restriction-index-item'",
    )
    .execute(database.pool())
    .await?;
    let cleared_index_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_content_restriction_index
         WHERE item_id = 'restriction-index-item'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        cleared_index_rows, 0,
        "clearing NFO removes the indexed row"
    );

    sqlx::query(
        "INSERT INTO media_content_restriction_index (item_id, certification_code)
         VALUES ('restriction-index-item', NULL)",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_content_restriction_tags (item_id, tag)
         VALUES ('restriction-index-item', 'family')",
    )
    .execute(database.pool())
    .await?;

    sqlx::query("DELETE FROM media_items WHERE id = 'restriction-index-item'")
        .execute(database.pool())
        .await?;
    let stale_index_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_content_restriction_index
         WHERE item_id = 'restriction-index-item'",
    )
    .fetch_one(database.pool())
    .await?;
    let stale_tag_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_content_restriction_tags
         WHERE item_id = 'restriction-index-item'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stale_index_rows, 0, "media deletion cascades to the index");
    assert_eq!(stale_tag_rows, 0, "index deletion cascades to indexed tags");
    Ok(())
}

#[tokio::test]
async fn legacy_user_and_library_access_upgrade_from_v183_to_current_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    fs::create_dir_all(&config_dir)?;
    let migration_prefix = temp_dir.path().join("migrations-before-account-profile");
    fs::create_dir(&migration_prefix)?;
    for entry in fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))? {
        let path = entry?.path();
        if path.extension() != Some(OsStr::new("sql")) {
            continue;
        }
        let Some(version) = path
            .file_stem()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split('_').next())
            .and_then(|value| value.parse::<i64>().ok())
        else {
            continue;
        };
        if version <= 183 {
            let file_name = path.file_name().ok_or("migration has no file name")?;
            fs::copy(&path, migration_prefix.join(file_name))?;
        }
    }

    let database_path = config_dir.join("lux.db");
    let legacy_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await?;
    Migrator::new(migration_prefix.as_path())
        .await?
        .run(&legacy_pool)
        .await?;
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash, is_disabled)
         VALUES ('legacy-enabled', 'legacy-enabled', 'Legacy Enabled', 'hash', 0),
                ('legacy-disabled', 'legacy-disabled', 'Legacy Disabled', 'hash', 1),
                ('legacy-denials-only', 'legacy-denials-only', 'Legacy Denials', 'hash', 0),
                ('legacy-selected', 'legacy-selected', 'Legacy Selected', 'hash', 0)",
    )
    .execute(&legacy_pool)
    .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind)
         VALUES ('legacy-library', 'Legacy Library', 'MOVIE')",
    )
    .execute(&legacy_pool)
    .await?;
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES ('legacy-denials-only', 'legacy-library', 0),
                ('legacy-selected', 'legacy-library', 1)",
    )
    .execute(&legacy_pool)
    .await?;
    legacy_pool.close().await;

    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir,
    })
    .await?;
    assert_eq!(
        database.schema_version().await?,
        latest_migration_version("migrations")?
    );
    let users: Vec<(String, Option<String>, Option<i64>, Option<i64>, i64, i64)> = sqlx::query_as(
        "SELECT id, email, email_verified_at, deletion_purge_at,
                is_disabled, is_disabled_before_delete
         FROM users WHERE id LIKE 'legacy-%' ORDER BY id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(users.len(), 4);
    assert_eq!(
        users[0],
        ("legacy-denials-only".to_owned(), None, None, None, 0, 0)
    );
    assert_eq!(
        users[1],
        ("legacy-disabled".to_owned(), None, None, None, 1, 0)
    );
    assert_eq!(
        users[2],
        ("legacy-enabled".to_owned(), None, None, None, 0, 0)
    );
    assert_eq!(
        users[3],
        ("legacy-selected".to_owned(), None, None, None, 0, 0)
    );

    let legacy_access_modes: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, library_access_mode FROM users WHERE id LIKE 'legacy-%' ORDER BY id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        legacy_access_modes,
        [
            ("legacy-denials-only".to_owned(), "ALL".to_owned()),
            ("legacy-disabled".to_owned(), "ALL".to_owned()),
            ("legacy-enabled".to_owned(), "ALL".to_owned()),
            ("legacy-selected".to_owned(), "SELECTED".to_owned()),
        ]
    );
    let legacy_access_rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT user_id, library_id, can_view FROM user_library_access
         WHERE user_id LIKE 'legacy-%' ORDER BY user_id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        legacy_access_rows,
        [
            (
                "legacy-denials-only".to_owned(),
                "legacy-library".to_owned(),
                0
            ),
            ("legacy-selected".to_owned(), "legacy-library".to_owned(), 1),
        ]
    );

    sqlx::query("UPDATE users SET email = 'legacy@example.com' WHERE id = 'legacy-enabled'")
        .execute(database.pool())
        .await?;
    let duplicate_email =
        sqlx::query("UPDATE users SET email = 'LEGACY@example.com' WHERE id = 'legacy-disabled'")
            .execute(database.pool())
            .await;
    assert!(
        duplicate_email.is_err(),
        "email uniqueness must ignore case"
    );

    Ok(())
}

#[tokio::test]
async fn user_permission_preset_source_migration_runs_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    assert!(database.schema_version().await? >= 190);
    let columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('user_permission_preset_sources')
         WHERE name IN ('user_id', 'preset_id', 'preset_name', 'preset_version', 'applied_at')",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(columns, 5);
    let preset_foreign_key: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('user_permission_preset_sources')
         WHERE \"table\" = 'users' AND \"from\" = 'user_id' AND on_delete = 'CASCADE'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(preset_foreign_key, 1);

    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('preset-source-user', 'preset-source-user', 'Preset Source', 'hash')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_permission_preset_sources
         (user_id, preset_id, preset_name, preset_version)
         VALUES ('preset-source-user', 'custom-id', 'Family', 2)",
    )
    .execute(database.pool())
    .await?;
    let source: (String, String, i64, i64) = sqlx::query_as(
        "SELECT preset_id, preset_name, preset_version, applied_at
         FROM user_permission_preset_sources WHERE user_id = 'preset-source-user'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(source.0, "custom-id");
    assert_eq!(source.1, "Family");
    assert_eq!(source.2, 2);
    assert!(source.3 > 0);

    sqlx::query("DELETE FROM users WHERE id = 'preset-source-user'")
        .execute(database.pool())
        .await?;
    let remaining_sources: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_permission_preset_sources
         WHERE user_id = 'preset-source-user'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(remaining_sources, 0);

    let postgres_migration =
        fs::read_to_string("migrations-postgres/0188_user_permission_preset_sources.sql")?;
    assert!(postgres_migration.contains("user_permission_preset_sources"));
    assert!(postgres_migration.contains("REFERENCES users(id) ON DELETE CASCADE"));
    assert!(
        postgres_migration.contains("preset_version BIGINT NOT NULL CHECK (preset_version > 0)")
    );
    Ok(())
}

#[tokio::test]
async fn global_movie_collection_storage_is_migrated_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    assert!(database.schema_version().await? >= 190);

    let system_library: (String, String) = sqlx::query_as(
        "SELECT kind, system_kind FROM libraries
         WHERE system_kind = 'COLLECTIONS'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        system_library,
        ("MIXED".to_owned(), "COLLECTIONS".to_owned())
    );

    let root_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM library_roots
         WHERE library_id = '00000000-0000-7000-8000-000000000538'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(root_count, 0);

    for table in [
        "collection_item_references",
        "collection_work_items",
        "collection_work_sources",
    ] {
        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(table)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(table_count, 1, "missing {table} table");
    }

    Ok(())
}

#[tokio::test]
async fn global_movie_collection_migration_preserves_existing_library_and_manual_collections()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("migrations");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?;
        if version.is_some_and(|version| version <= 169) {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing migration filename")?),
            )?;
        }
    }

    let database_path = temp_dir.path().join("collections-upgrade.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('movie-library', 'Movies', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('movie-a', 'movie-library', 'MOVIE', 'Movie A', 'movie a', 'LOCAL_CONFIRMED')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('manual-boxset', 'movie-library', 'BOX_SET', 'Favorites', 'favorites', 'LOCAL_CONFIRMED')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO collections (id, item_id, library_id, provider, provider_id, title)
         VALUES ('manual-collection', 'manual-boxset', 'movie-library', 'EMBY', 'manual-1', 'Favorites')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO collection_items (collection_id, item_id, sort_order)
         VALUES ('manual-collection', 'movie-a', 1)",
    )
    .execute(&pool)
    .await?;

    let migration = source_dir.join("0170_global_movie_collections.sql");
    fs::copy(
        migration,
        migration_dir.join("0170_global_movie_collections.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;

    let manual_members: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM collection_items
         WHERE collection_id = 'manual-collection' AND item_id = 'movie-a'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(manual_members, 1);
    let system_library_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM libraries WHERE system_kind = 'COLLECTIONS'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(system_library_count, 1);

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn nfo_collection_identity_migration_adds_reference_index_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("migrations-v172");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?;
        if version.is_some_and(|version| version <= 172) {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing migration filename")?),
            )?;
        }
    }

    let database_path = temp_dir.path().join("nfo-reindex-upgrade.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;
    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('library', 'Mixed', 'MIXED')")
        .execute(&pool)
        .await?;

    let prior_fingerprint = vec![7_u8; 32];
    for (item_id, item_type) in [("movie", "MOVIE"), ("series", "SERIES")] {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status,
                 metadata_fingerprint
             ) VALUES (?, 'library', ?, ?, ?, 'LOCAL_CONFIRMED', ?)",
        )
        .bind(item_id)
        .bind(item_type)
        .bind(item_id)
        .bind(item_id)
        .bind(&prior_fingerprint)
        .execute(&pool)
        .await?;
    }

    let reindex_migration = source_dir.join("0173_nfo_collection_reference_key.sql");
    fs::copy(
        reindex_migration,
        migration_dir.join("0173_nfo_collection_reference_key.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;

    let movie_state: (Option<Vec<u8>>, Option<String>) = sqlx::query_as(
        "SELECT metadata_fingerprint, nfo_collection_reference_key
         FROM media_items WHERE id = 'movie'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(movie_state, (Some(prior_fingerprint.clone()), None));
    let series_state: (Option<Vec<u8>>, Option<String>) = sqlx::query_as(
        "SELECT metadata_fingerprint, nfo_collection_reference_key
         FROM media_items WHERE id = 'series'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(series_state, (Some(prior_fingerprint), None));

    pool.close().await;
    Ok(())
}

#[derive(Debug, Eq, PartialEq, sqlx::FromRow)]
struct FillRequestSnapshotRow {
    item_id: String,
    status: String,
    request_fingerprint: Option<Vec<u8>>,
    request_capabilities_json: String,
    claimed_request_fingerprint: Option<Vec<u8>>,
    claimed_request_capabilities_json: String,
}

#[test]
fn migration_versions_are_unique_per_backend() -> Result<(), Box<dyn std::error::Error>> {
    for directory in ["migrations", "migrations-postgres"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(directory);
        let mut versions = BTreeMap::<i64, Vec<String>>::new();

        for entry in fs::read_dir(path)? {
            let path = entry?.path();
            if path.extension() != Some(OsStr::new("sql")) {
                continue;
            }
            let file_name = path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or("migration filename is not valid UTF-8")?;
            let version = file_name
                .split_once('_')
                .ok_or("migration filename has no version separator")?
                .0
                .parse::<i64>()?;
            versions
                .entry(version)
                .or_default()
                .push(file_name.to_owned());
        }

        let duplicates: Vec<_> = versions
            .into_iter()
            .filter(|(_, files)| files.len() > 1)
            .collect();
        assert!(
            duplicates.is_empty(),
            "{directory} has duplicate migration versions: {duplicates:?}"
        );
    }
    Ok(())
}

#[test]
fn scan_job_file_changes_migration_matches_both_backends() -> Result<(), Box<dyn std::error::Error>>
{
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sqlite = fs::read_to_string(repository.join("migrations/0174_scan_job_file_changes.sql"))?;
    let postgres =
        fs::read_to_string(repository.join("migrations-postgres/0174_scan_job_file_changes.sql"))?;

    for migration in [&sqlite, &postgres] {
        assert!(migration.contains("CREATE TABLE scan_job_file_changes"));
        assert!(migration.contains("PRIMARY KEY (job_id, library_root_id, relative_path)"));
        assert!(migration.contains("idx_scan_job_file_changes_pending"));
        for state in [
            "ADD", "CHANGE", "REMOVE", "PENDING", "RUNNING", "DONE", "FAILED", "SKIPPED",
        ] {
            assert!(
                migration.contains(state),
                "missing {state} in migration: {migration}"
            );
        }
    }
    assert!(sqlite.contains("unixepoch()"));
    assert!(postgres.contains("EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)"));
    assert!(!postgres.contains("unixepoch()"));
    Ok(())
}

#[test]
fn global_collection_reference_reconciliation_index_matches_both_backends()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sqlite = fs::read_to_string(
        repository.join("migrations/0175_global_collection_reference_reconciliation_index.sql"),
    )?;
    let postgres = fs::read_to_string(
        repository
            .join("migrations-postgres/0175_global_collection_reference_reconciliation_index.sql"),
    )?;
    assert_eq!(sqlite, postgres);
    assert!(sqlite.contains("idx_media_items_nfo_collection_reference_pending"));
    assert!(sqlite.contains("item_type = 'MOVIE'"));
    assert!(sqlite.contains("nfo_metadata_json IS NOT NULL"));
    assert!(sqlite.contains("nfo_collection_reference_key IS NULL"));
    Ok(())
}

#[tokio::test]
async fn sqlite_media_item_timestamps_upgrade_handles_legacy_and_new_rows()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("migrations");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?;
        if version.is_some_and(|version| version <= 125) {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing migration filename")?),
            )?;
        }
    }

    let database_path = temp_dir.path().join("media-item-timestamps-upgrade.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;

    let schema_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(schema_version, 125);

    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('timestamp-library', 'Legacy', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('legacy-item', 'timestamp-library', 'MOVIE', 'Legacy', 'legacy', 'LOCAL_CONFIRMED')",
    )
    .execute(&pool)
    .await?;
    let migration_started_at: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(&pool)
        .await?;

    fs::copy(
        source_dir.join("0126_emby_item_timestamps.sql"),
        migration_dir.join("0126_emby_item_timestamps.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;

    let legacy_updated_at: i64 =
        sqlx::query_scalar("SELECT updated_at FROM media_items WHERE id = 'legacy-item'")
            .fetch_one(&pool)
            .await?;
    let migration_finished_at: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(&pool)
        .await?;
    assert!((migration_started_at..=migration_finished_at).contains(&legacy_updated_at));

    let index_exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_media_items_updated_at'
         )",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(index_exists, 1);

    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('new-item', 'timestamp-library', 'MOVIE', 'New', 'new', 'LOCAL_CONFIRMED')",
    )
    .execute(&pool)
    .await?;
    let new_updated_at: i64 =
        sqlx::query_scalar("SELECT updated_at FROM media_items WHERE id = 'new-item'")
            .fetch_one(&pool)
            .await?;
    assert!(new_updated_at >= migration_started_at);

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn sqlite_upgrade_accepts_the_original_applied_timestamp_migration_checksum()
-> Result<(), Box<dyn std::error::Error>> {
    const ORIGINAL_MIGRATION: &str = "ALTER TABLE media_items\nADD COLUMN updated_at INTEGER NOT NULL DEFAULT (unixepoch());\n\nCREATE INDEX idx_media_items_updated_at\nON media_items(updated_at, id);\n";

    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    fs::create_dir_all(&config_dir)?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let migration_dir = temp_dir.path().join("legacy-migrations");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?;
        if version.is_some_and(|version| version <= 125) {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing migration filename")?),
            )?;
        }
    }
    fs::write(
        migration_dir.join("0126_emby_item_timestamps.sql"),
        ORIGINAL_MIGRATION,
    )?;

    let database_path = config_dir.join("lux.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('timestamp-library', 'Legacy', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('legacy-item', 'timestamp-library', 'MOVIE', 'Legacy', 'legacy', 'LOCAL_CONFIRMED')",
    )
    .execute(&pool)
    .await?;
    let original_checksum: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 126")
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        original_checksum,
        Sha384::digest(ORIGINAL_MIGRATION.as_bytes()).to_vec()
    );
    pool.close().await;

    let database = Database::connect(&config).await?;
    assert!(database.schema_version().await? >= 190);
    let preserved_checksum: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 126")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(preserved_checksum, original_checksum);
    let updated_at: i64 =
        sqlx::query_scalar("SELECT updated_at FROM media_items WHERE id = 'legacy-item'")
            .fetch_one(database.pool())
            .await?;
    assert!(updated_at > 0);

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn sqlite_upgrade_and_missing_transitions_clear_file_fingerprints()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database_path = temp_dir.path().join("missing-fingerprint-upgrade.db");
    let migration_dir = temp_dir.path().join("migrations");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?
            .ok_or("migration file has no version")?;
        if version <= 179 {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing migration filename")?),
            )?;
        }
    }

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('missing-library', 'Missing', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('missing-root', 'missing-library', '/missing', '/missing', 1, 0)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO filesystem_entries (
             id, library_root_id, relative_path, entry_kind, size, modified_at,
             fingerprint, last_seen_generation, is_missing
         ) VALUES
             ('old-missing', 'missing-root', 'gone.mkv', 'FILE', 10, 20, zeroblob(32), 'old', 1),
             ('old-live', 'missing-root', 'live.mkv', 'FILE', 10, 20, zeroblob(32), 'old', 0),
             ('old-strm', 'missing-root', 'remote.strm', 'FILE', 10, 20, zeroblob(32), 'old', 0),
             ('old-local', 'missing-root', 'local.mkv', 'FILE', 10, 20, zeroblob(32), 'old', 0),
             ('old-ready', 'missing-root', 'ready.strm', 'FILE', 10, 20, zeroblob(32), 'old', 0)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES
             ('strm-item', 'missing-library', 'MOVIE', 'STRM', 'strm', 'LOCAL_CONFIRMED'),
             ('local-item', 'missing-library', 'MOVIE', 'Local', 'local', 'LOCAL_CONFIRMED'),
             ('ready-item', 'missing-library', 'MOVIE', 'Ready', 'ready', 'LOCAL_CONFIRMED')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO media_sources (
             id, item_id, source_kind, filesystem_entry_id, probe_status
         ) VALUES
             ('strm-source', 'strm-item', 'STRM_URL', 'old-strm', 'PENDING'),
             ('local-source', 'local-item', 'LOCAL_FILE', 'old-local', 'PENDING'),
             ('ready-source', 'ready-item', 'STRM_URL', 'old-ready', 'READY')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_fingerprint_migration_state (library_id, completed_at)
         VALUES ('missing-library', 123)",
    )
    .execute(&pool)
    .await?;

    fs::copy(
        source_dir.join("0180_clear_missing_file_fingerprints.sql"),
        migration_dir.join("0180_clear_missing_file_fingerprints.sql"),
    )?;
    fs::copy(
        source_dir.join("0181_strm_probe_status_not_configured.sql"),
        migration_dir.join("0181_strm_probe_status_not_configured.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;

    let upgraded_missing: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT fingerprint FROM filesystem_entries WHERE id = 'old-missing'")
            .fetch_one(&pool)
            .await?;
    let upgraded_live: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT fingerprint FROM filesystem_entries WHERE id = 'old-live'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(upgraded_missing, Some(vec![0; 32]));
    assert_eq!(upgraded_live, Some(vec![0; 32]));
    let reset_completion: Option<i64> = sqlx::query_scalar(
        "SELECT completed_at FROM scan_fingerprint_migration_state
         WHERE library_id = 'missing-library'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(reset_completion, None);
    let probe_statuses: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, probe_status FROM media_sources
         WHERE id IN ('strm-source', 'local-source', 'ready-source') ORDER BY id",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        probe_statuses,
        vec![
            ("local-source".to_owned(), "PENDING".to_owned()),
            ("ready-source".to_owned(), "READY".to_owned()),
            ("strm-source".to_owned(), "NOT_CONFIGURED".to_owned()),
        ]
    );

    sqlx::query("UPDATE filesystem_entries SET is_missing = 1 WHERE id = 'old-live'")
        .execute(&pool)
        .await?;
    let newly_missing: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT fingerprint FROM filesystem_entries WHERE id = 'old-live'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(newly_missing, None);

    sqlx::query(
        "UPDATE filesystem_entries SET fingerprint = zeroblob(32)
         WHERE id = 'old-live'",
    )
    .execute(&pool)
    .await?;
    let still_missing: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT fingerprint FROM filesystem_entries WHERE id = 'old-live'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(still_missing, None);

    sqlx::query(
        "INSERT INTO filesystem_entries (
             id, library_root_id, relative_path, entry_kind, size, modified_at,
             fingerprint, last_seen_generation, is_missing
         ) VALUES ('new-missing', 'missing-root', 'new-gone.mkv', 'FILE', 10, 20,
                   zeroblob(32), 'new', 1)",
    )
    .execute(&pool)
    .await?;
    let inserted_missing: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT fingerprint FROM filesystem_entries WHERE id = 'new-missing'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(inserted_missing, None);

    pool.close().await;
    Ok(())
}

#[test]
fn postgres_migration_0158_preserves_legacy_media_chapters_version() {
    let legacy = include_str!("../migrations-postgres/0158_media_info_chapters.sql");
    let additive = include_str!(
        "../migrations-postgres/0160_scan_local_metadata_backfill_non_retryable_items.sql"
    );

    assert!(legacy.contains("CREATE TABLE media_info_chapters"));
    assert!(additive.contains("non_retryable_item_ids_json"));
}

#[test]
fn person_index_relation_checksum_migration_is_nullable_on_both_backends() {
    let sqlite = include_str!("../migrations/0166_person_index_relation_checksum.sql");
    let postgres = include_str!("../migrations-postgres/0166_person_index_relation_checksum.sql");

    for migration in [sqlite, postgres] {
        let normalized = migration.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalized
                .contains("ALTER TABLE person_index_item_state ADD COLUMN relation_checksum TEXT;")
        );
        assert!(!normalized.contains("relation_checksum TEXT NOT NULL"));
    }
}

#[test]
fn metadata_migrations_preserve_historical_version_sequence()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));

    for directory in ["migrations", "migrations-postgres"] {
        let migrations = repository.join(directory);
        for name in [
            "0158_media_info_chapters.sql",
            "0160_scan_local_metadata_backfill_non_retryable_items.sql",
            "0161_reconcile_cancelled_metadata_job_items.sql",
            "0162_filesystem_entry_directory_prefix_index.sql",
            "0163_scan_job_failed_count_index.sql",
            "0164_metadata_fill_request_snapshots.sql",
            "0165_metadata_fill_missing_retry_backoff.sql",
            "0166_person_index_relation_checksum.sql",
            "0169_metadata_reidentify_dispatch_source.sql",
        ] {
            assert!(
                migrations.join(name).is_file(),
                "{directory} must preserve the historical migration sequence at {name}"
            );
        }

        let chapters = fs::read_to_string(migrations.join("0158_media_info_chapters.sql"))?;
        let backfill = fs::read_to_string(
            migrations.join("0160_scan_local_metadata_backfill_non_retryable_items.sql"),
        )?;
        let dispatch_source =
            fs::read_to_string(migrations.join("0169_metadata_reidentify_dispatch_source.sql"))?;
        assert!(chapters.contains("CREATE TABLE media_info_chapters"));
        assert!(backfill.contains("non_retryable_item_ids_json"));
        assert!(dispatch_source.contains("dispatch_source"));
        assert!(dispatch_source.contains("idx_metadata_reidentify_jobs_automatic_queue"));
    }

    let sqlite_retry = fs::read_to_string(
        repository.join("migrations/0165_metadata_fill_missing_retry_backoff.sql"),
    )?;
    let postgres_retry = fs::read_to_string(
        repository.join("migrations-postgres/0165_metadata_fill_missing_retry_backoff.sql"),
    )?;
    for migration in [&sqlite_retry, &postgres_retry] {
        assert!(migration.contains("automatic_retry_count"));
        assert!(migration.contains("automatic_retry_after"));
        assert!(migration.contains("automatic_retry_consumed"));
        assert!(migration.contains("metadata_fill_missing_legacy_retry_after"));
        assert!(migration.contains("+ 300"));
        assert!(!migration.contains("UPDATE metadata_reidentify_job_items"));
    }
    assert!(!postgres_retry.contains("unixepoch()"));
    assert!(postgres_retry.contains("EXTRACT(EPOCH FROM CURRENT_TIMESTAMP"));

    Ok(())
}

#[tokio::test]
async fn sqlite_person_index_relation_checksum_migration_preserves_existing_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let migration_dir = temp_dir.path().join("migrations-v165");
    fs::create_dir(&migration_dir)?;
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?
            .ok_or("migration file has no version")?;
        if version <= 165 {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing migration name")?),
            )?;
        }
    }

    let database_path = temp_dir.path().join("person-index-upgrade.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('relation-library', 'Relation', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('relation-item', 'relation-library', 'MOVIE', 'Relation', 'relation', 'LOCAL_CONFIRMED')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO person_index_item_state (
             item_id, source_fingerprint, relation_schema_version, updated_at
         ) VALUES ('relation-item', 'nfo-fingerprint', 7, 123456789)",
    )
    .execute(&pool)
    .await?;
    let checksum_before_migration: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('person_index_item_state')
         WHERE name = 'relation_checksum'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(checksum_before_migration, 0);

    fs::copy(
        source_dir.join("0166_person_index_relation_checksum.sql"),
        migration_dir.join("0166_person_index_relation_checksum.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;

    let preserved_state: (Option<String>, i64, i64, Option<String>) = sqlx::query_as(
        "SELECT source_fingerprint, relation_schema_version, updated_at, relation_checksum
         FROM person_index_item_state WHERE item_id = 'relation-item'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        preserved_state,
        (Some("nfo-fingerprint".to_owned()), 7, 123456789, None)
    );
    let schema_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(schema_version, 166);
    pool.close().await;

    let empty_database_path = temp_dir.path().join("person-index-empty.db");
    let empty_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&empty_database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(source_dir)
        .await?
        .run(&empty_pool)
        .await?;
    let empty_database_has_checksum: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('person_index_item_state')
         WHERE name = 'relation_checksum'",
    )
    .fetch_one(&empty_pool)
    .await?;
    assert_eq!(empty_database_has_checksum, 1);
    empty_pool.close().await;
    Ok(())
}

#[test]
fn metadata_migrations_keep_deployed_checksums() -> Result<(), Box<dyn std::error::Error>> {
    for (path, checksum) in [
        (
            "migrations/0158_media_info_chapters.sql",
            "7f467f8bcf880efea58d07fdd434f3e4bcf0e64671effa68f03c21d359c2460cbc5996ca6ce7cb21ba729e82c62df4ce",
        ),
        (
            "migrations-postgres/0158_media_info_chapters.sql",
            "765fd88f82535e66cca0b46bb746f66ca30ecdd75f30886e89b8650d3a9c477ac3b19881f88e11053fda953665f8d969",
        ),
        (
            "migrations/0160_scan_local_metadata_backfill_non_retryable_items.sql",
            "3522909b3fa8daaab26a6acf1fb43bde5f6a79359b670a7dd19b9a406c949976305ae9f691133e9127b702fc8619b786",
        ),
        (
            "migrations-postgres/0160_scan_local_metadata_backfill_non_retryable_items.sql",
            "3522909b3fa8daaab26a6acf1fb43bde5f6a79359b670a7dd19b9a406c949976305ae9f691133e9127b702fc8619b786",
        ),
    ] {
        let migration = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))?;
        assert_eq!(
            format!("{:x}", Sha384::digest(migration.as_bytes())),
            checksum
        );
    }

    Ok(())
}

#[test]
fn historical_media_catalog_migration_keeps_its_original_checksum() {
    let migration = include_str!("../migrations/0006_media_catalog.sql");

    assert_eq!(
        format!("{:x}", Sha384::digest(migration.as_bytes())),
        "4c0ce4f36416069631e85e66ad25c6808cf42b9ea60287db68e945e67f32860ab07bf9706dadc5a120fda8eba5bae323"
    );
}

#[test]
fn postgres_bootstrap_migration_keeps_its_original_checksum() {
    let migration = include_str!("../migrations-postgres/0001_bootstrap.sql");

    assert_eq!(
        format!("{:x}", Sha384::digest(migration.as_bytes())),
        "81fb302801af162714b21496d70ca696af6f710145070a1b69b31c229d879806a2e36acd9aca651c0c96867d1bd5d4ca"
    );
}

#[test]
fn postgres_derived_index_refresh_uses_statement_level_triggers() {
    let migration = include_str!("../migrations-postgres/0137_statement_derived_index_refresh.sql");

    for trigger in [
        "media_items_search_ai",
        "media_items_search_au",
        "media_items_search_ad",
        "item_aliases_search_ai",
        "item_aliases_search_au",
        "item_aliases_search_ad",
        "media_sources_availability_ai",
        "media_sources_availability_au",
        "media_sources_availability_ad",
        "filesystem_entries_availability_au",
    ] {
        assert!(
            migration.contains(&format!("DROP TRIGGER IF EXISTS {trigger}")),
            "migration must replace the row trigger {trigger}"
        );
    }

    assert!(migration.contains("FOR EACH STATEMENT"));
    assert!(!migration.contains("FOR EACH ROW"));
    assert!(migration.contains("REFERENCING NEW TABLE"));
}

#[test]
fn postgres_provider_index_refresh_uses_statement_level_triggers() {
    let migration =
        include_str!("../migrations-postgres/0141_statement_provider_index_refresh.sql");

    for trigger in ["media_item_provider_ids_ai", "media_item_provider_ids_au"] {
        assert!(
            migration.contains(&format!("DROP TRIGGER IF EXISTS {trigger}")),
            "migration must replace the row trigger {trigger}"
        );
    }

    assert!(migration.contains("FOR EACH STATEMENT"));
    assert!(!migration.contains("FOR EACH ROW"));
    assert!(migration.contains("REFERENCING NEW TABLE"));
}

#[test]
fn postgres_media_search_refresh_owns_provider_index_refresh() {
    let migration =
        include_str!("../migrations-postgres/0143_merge_provider_refresh_into_media_search.sql");

    assert!(migration.contains("lux_refresh_media_search_items_insert_stmt"));
    assert!(migration.contains("lux_refresh_media_search_items_update_stmt"));
    assert!(migration.contains("INSERT INTO media_item_provider_ids"));
    assert!(migration.contains("DELETE FROM media_item_provider_ids"));
    assert!(migration.contains("DROP TRIGGER IF EXISTS media_item_provider_ids_ai"));
    assert!(migration.contains("DROP TRIGGER IF EXISTS media_item_provider_ids_au"));
    assert!(migration.contains("DROP INDEX IF EXISTS idx_media_search_title"));
    assert!(migration.contains("DROP INDEX IF EXISTS idx_media_search_sort_title"));
    assert!(!migration.contains("CREATE TRIGGER media_item_provider_ids_ai"));
    assert!(!migration.contains("CREATE TRIGGER media_item_provider_ids_au"));
    assert!(!migration.contains("FOR EACH ROW"));
    assert!(migration.contains("FROM new_rows"));
    assert!(migration.contains("USING old_rows"));
    assert!(migration.contains("JOIN old_rows o"));
    assert!(migration.contains("n.title IS DISTINCT FROM o.title"));
    assert!(migration.contains("existing.item_id IS NULL"));
}

#[test]
fn postgres_provider_insert_refresh_filters_empty_provider_documents() {
    let migration =
        include_str!("../migrations-postgres/0146_skip_empty_provider_index_expansion.sql");

    assert!(migration.contains("WITH provider_rows AS MATERIALIZED"));
    assert!(migration.contains("WHERE n.provider_ids_json IS NOT NULL"));
    assert!(migration.contains("AND n.provider_ids_json <> '{}'"));
    assert!(migration.contains("FROM provider_rows n"));
    assert!(migration.contains("json_each_text(n.provider_ids_json::json)"));
}

#[test]
fn sqlite_media_search_triggers_skip_only_ascii_case_equivalent_sort_titles() {
    let migration = include_str!("../migrations/0147_skip_redundant_sort_title_fts_tokens.sql");
    let legacy_rebuild = include_str!("../src/storage/migration.rs");

    assert_eq!(
        migration
            .matches("NEW.sort_title = NEW.title COLLATE NOCASE")
            .count(),
        2,
        "both insert and update triggers should omit only ASCII-case-equivalent sort titles"
    );
    assert!(migration.contains("ELSE NEW.sort_title"));
    assert!(migration.contains("CREATE TRIGGER media_items_search_ai"));
    assert!(migration.contains("CREATE TRIGGER media_items_search_au"));
    assert_eq!(
        legacy_rebuild
            .matches("CASE WHEN NEW.sort_title = NEW.title COLLATE NOCASE THEN '' ELSE NEW.sort_title END")
            .count(),
        2,
        "legacy table rebuild must keep insert and update triggers in sync"
    );
}

#[tokio::test]
async fn sqlite_fts_columnsize_upgrade_preserves_existing_search_fields()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("migrations");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?;
        if version.is_some_and(|version| version <= 147) {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing migration filename")?),
            )?;
        }
    }

    let database_path = temp_dir.path().join("fts-columnsize-upgrade.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;
    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('fts-library', 'FTS', 'MOVIE')")
        .execute(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, original_title,
             identification_status
         ) VALUES (
             'fts-existing-item', 'fts-library', 'MOVIE', 'Visible Movie',
             'Canonical Sort Key', 'Original Feature', 'LOCAL_CONFIRMED'
         )",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO item_aliases (id, item_id, alias, alias_normalized)
         VALUES ('fts-existing-alias', 'fts-existing-item', 'Search Alias', 'search alias')",
    )
    .execute(&pool)
    .await?;

    fs::copy(
        source_dir.join("0148_fts_columnsize_zero.sql"),
        migration_dir.join("0148_fts_columnsize_zero.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;

    let schema_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(schema_version, 148);

    for term in ["Visible", "Canonical", "Original", "Alias"] {
        let matches: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM media_search
             WHERE media_search MATCH ? AND item_id = 'fts-existing-item'",
        )
        .bind(term)
        .fetch_one(&pool)
        .await?;
        assert_eq!(matches, 1, "search field {term:?} was not preserved");
    }

    let table_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'media_search'",
    )
    .fetch_one(&pool)
    .await?;
    assert!(table_sql.contains("columnsize=0"));
    let docsize_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'media_search_docsize'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(docsize_table_count, 0);

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn sqlite_media_search_rowid_map_upgrade_preserves_existing_rows()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("migrations");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?;
        if version.is_some_and(|version| version <= 166) {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing migration filename")?),
            )?;
        }
    }

    let database_path = temp_dir.path().join("fts-rowid-map-upgrade.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('fts-map-library', 'FTS', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, original_title,
             identification_status
         ) VALUES (
             'fts-map-existing-item', 'fts-map-library', 'MOVIE', 'Visible Movie',
             'Canonical Sort Key', 'Original Feature', 'LOCAL_CONFIRMED'
         )",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO item_aliases (id, item_id, alias, alias_normalized)
         VALUES ('fts-map-existing-alias', 'fts-map-existing-item', 'Search Alias', 'search alias')",
    )
    .execute(&pool)
    .await?;
    let original_rowid: i64 = sqlx::query_scalar(
        "SELECT rowid FROM media_search WHERE item_id = 'fts-map-existing-item'",
    )
    .fetch_one(&pool)
    .await?;

    fs::copy(
        source_dir.join("0167_index_media_search_row_ids.sql"),
        migration_dir.join("0167_index_media_search_row_ids.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;

    let mapped_row: (i64, String) = sqlx::query_as(
        "SELECT map.fts_rowid, media_search.item_id
         FROM media_search_map map
         JOIN media_search ON media_search.rowid = map.fts_rowid
         WHERE map.item_id = 'fts-map-existing-item'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        mapped_row,
        (original_rowid, "fts-map-existing-item".to_owned())
    );
    for term in ["Visible", "Canonical", "Original", "Alias"] {
        let matches: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM media_search
             WHERE media_search MATCH ? AND item_id = 'fts-map-existing-item'",
        )
        .bind(term)
        .fetch_one(&pool)
        .await?;
        assert_eq!(matches, 1, "search field {term:?} was not preserved");
    }
    pool.close().await;
    Ok(())
}

#[test]
fn postgres_media_search_refresh_does_not_rescan_aliases_per_item() {
    let migration =
        include_str!("../migrations-postgres/0138_avoid_alias_rescan_on_media_item_refresh.sql");

    assert!(migration.contains("lux_refresh_media_search_items_insert_stmt"));
    assert!(migration.contains("           ''"));
    assert!(migration.contains("LEFT JOIN media_search existing"));
    assert!(!migration.contains("FROM item_aliases"));
}

#[test]
fn postgres_media_source_insert_refresh_only_promotes_unavailable_items() {
    let migration =
        include_str!("../migrations-postgres/0139_promote_media_availability_on_source_insert.sql");

    assert!(migration.contains("SET has_available_source = 1"));
    assert!(migration.contains("has_available_source = 0"));
    assert!(migration.contains("JOIN filesystem_entries"));
    assert!(!migration.contains("CASE WHEN EXISTS"));
}

#[test]
fn postgres_media_source_insert_filters_available_items_before_filesystem_lookup() {
    let migration =
        include_str!("../migrations-postgres/0142_filter_available_source_promotions.sql");

    assert!(migration.contains("JOIN media_items candidate"));
    assert!(migration.contains("candidate.has_available_source = 0"));
    assert!(migration.contains("JOIN filesystem_entries entry"));
}

#[test]
fn postgres_media_search_insert_refresh_avoids_redundant_upsert() {
    let migration =
        include_str!("../migrations-postgres/0140_media_search_insert_without_conflict_probe.sql");

    assert!(migration.contains("INSERT INTO media_search"));
    assert!(migration.contains("lux_refresh_media_search_items_insert_stmt"));
    assert!(!migration.contains("ON CONFLICT"));
}

#[test]
fn postgres_media_search_drops_the_trigger_redundant_item_foreign_key() {
    let migration = include_str!("../migrations-postgres/0147_drop_media_search_item_fk.sql");

    assert!(migration.contains("ALTER TABLE media_search"));
    assert!(migration.contains("DROP CONSTRAINT IF EXISTS media_search_item_id_fkey"));
}

#[tokio::test]
async fn postgres_scan_job_migration_allows_one_active_job_per_type()
-> Result<(), Box<dyn std::error::Error>> {
    let pool = sqlx::SqlitePool::connect(":memory:").await?;
    sqlx::query(
        "CREATE TABLE scan_jobs (
            id TEXT PRIMARY KEY,
            library_id TEXT NOT NULL,
            job_type TEXT NOT NULL,
            status TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE UNIQUE INDEX idx_scan_jobs_one_active
         ON scan_jobs(library_id)
         WHERE status IN ('PENDING', 'RUNNING')",
    )
    .execute(&pool)
    .await?;

    for statement in include_str!("../migrations-postgres/0109_scan_job_active_by_type.sql")
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        sqlx::query(statement).execute(&pool).await?;
    }

    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status)
         VALUES ('full', 'library', 'RECONCILE_LIBRARY', 'RUNNING'),
                ('incremental', 'library', 'INCREMENTAL_SCAN', 'PENDING')",
    )
    .execute(&pool)
    .await?;
    let active_jobs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_jobs
         WHERE library_id = 'library' AND status IN ('PENDING', 'RUNNING')",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(active_jobs, 2);

    let duplicate_incremental = sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status)
         VALUES ('incremental-2', 'library', 'INCREMENTAL_SCAN', 'RUNNING')",
    )
    .execute(&pool)
    .await;
    assert!(duplicate_incremental.is_err());
    Ok(())
}

#[tokio::test]
async fn metadata_candidate_identity_migration_collapses_pending_duplicates()
-> Result<(), Box<dyn std::error::Error>> {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
    sqlx::query(
        "CREATE TABLE metadata_candidates (
            id TEXT PRIMARY KEY,
            item_id TEXT NOT NULL,
            provider TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            candidate_json TEXT NOT NULL,
            score REAL NOT NULL,
            status TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await?;
    for (id, score, created_at) in [
        ("older", 90.0, 10_i64),
        ("best", 95.0, 11),
        ("newest", 95.0, 12),
    ] {
        sqlx::query(
            "INSERT INTO metadata_candidates (
                id, item_id, provider, provider_id, candidate_json,
                score, status, created_at, updated_at
             ) VALUES (?, 'item', 'TMDB', '603', '{}', ?, 'PENDING', ?, ?)",
        )
        .bind(id)
        .bind(score)
        .bind(created_at)
        .bind(created_at)
        .execute(&pool)
        .await?;
    }

    for statement in include_str!("../migrations/0095_metadata_candidate_identity.sql")
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        sqlx::query(statement).execute(&pool).await?;
    }

    let pending: (String, f64) =
        sqlx::query_as("SELECT id, score FROM metadata_candidates WHERE status = 'PENDING'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(pending.0, "newest");
    assert_eq!(pending.1, 95.0);
    let rejected: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM metadata_candidates WHERE status = 'REJECTED'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(rejected, 2);
    let index_name: String = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'index' AND name = 'idx_metadata_candidates_pending_identity'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(index_name, "idx_metadata_candidates_pending_identity");
    Ok(())
}

#[tokio::test]
async fn empty_config_dir_runs_migrations_and_configures_sqlite()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };

    let database = Database::connect(&config).await?;

    assert!(database.schema_version().await? >= 190);
    assert!(config_dir.join("lux.db").is_file());

    let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(database.pool())
        .await?;
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(database.pool())
        .await?;
    let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(database.pool())
        .await?;

    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
    assert_eq!(foreign_keys, 1);
    assert_eq!(busy_timeout, 5_000);

    database.close().await;

    let second_database = Database::connect(&config).await?;
    assert!(second_database.schema_version().await? >= 190);
    second_database.close().await;
    Ok(())
}

#[tokio::test]
async fn sqlite_fill_request_snapshot_migration_preserves_queued_jobs()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    fs::create_dir(&config_dir)?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let migration_dir = temp_dir.path().join("migrations-v163");
    fs::create_dir(&migration_dir)?;
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?
            .ok_or("migration file has no version")?;
        if version <= 163 {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing name")?),
            )?;
        }
    }

    let database_path = config_dir.join("lux.db");
    let old_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&old_pool)
        .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('snapshot-library', 'Snapshot', 'MOVIE')",
    )
    .execute(&old_pool)
    .await?;
    for (item_id, title) in [
        ("snapshot-pending", "Pending"),
        ("snapshot-completed", "Completed"),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, 'snapshot-library', 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(title)
        .bind(title.to_ascii_lowercase())
        .execute(&old_pool)
        .await?;
    }
    sqlx::query(
        "INSERT INTO metadata_reidentify_jobs (
             id, status, processed_count, total_count, mode, library_id, job_scope
         ) VALUES ('snapshot-job', 'QUEUED', 1, 2, 'FILL_MISSING', 'snapshot-library', 'ITEMS')",
    )
    .execute(&old_pool)
    .await?;
    sqlx::query(
        "INSERT INTO metadata_reidentify_job_items (job_id, item_id, status)
         VALUES ('snapshot-job', 'snapshot-pending', 'PENDING'),
                ('snapshot-job', 'snapshot-completed', 'COMPLETED')",
    )
    .execute(&old_pool)
    .await?;
    old_pool.close().await;

    let database = Database::connect(&config).await?;
    assert!(database.schema_version().await? >= 190);
    let job_state: (String, i64, i64) = sqlx::query_as(
        "SELECT status, processed_count, total_count
         FROM metadata_reidentify_jobs WHERE id = 'snapshot-job'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(job_state, ("QUEUED".to_owned(), 1, 2));
    let item_snapshots: Vec<FillRequestSnapshotRow> = sqlx::query_as(
        "SELECT item_id, status, request_fingerprint, request_capabilities_json,
                    claimed_request_fingerprint, claimed_request_capabilities_json
             FROM metadata_reidentify_job_items WHERE job_id = 'snapshot-job'
             ORDER BY item_id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        item_snapshots,
        vec![
            FillRequestSnapshotRow {
                item_id: "snapshot-completed".to_owned(),
                status: "COMPLETED".to_owned(),
                request_fingerprint: None,
                request_capabilities_json: "[]".to_owned(),
                claimed_request_fingerprint: None,
                claimed_request_capabilities_json: "[]".to_owned(),
            },
            FillRequestSnapshotRow {
                item_id: "snapshot-pending".to_owned(),
                status: "PENDING".to_owned(),
                request_fingerprint: None,
                request_capabilities_json: "[]".to_owned(),
                claimed_request_fingerprint: None,
                claimed_request_capabilities_json: "[]".to_owned(),
            },
        ]
    );
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn sqlite_fill_missing_retry_migration_uses_single_legacy_cooldown_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    fs::create_dir(&config_dir)?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let migration_dir = temp_dir.path().join("migrations-v164");
    fs::create_dir(&migration_dir)?;
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?
            .ok_or("migration file has no version")?;
        if version <= 164 {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing name")?),
            )?;
        }
    }

    let database_path = config_dir.join("lux.db");
    let old_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&old_pool)
        .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('retry-library', 'Retry', 'MOVIE')",
    )
    .execute(&old_pool)
    .await?;
    for item_id in ["retry-automatic", "retry-other-error", "retry-no-snapshot"] {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, 'retry-library', 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(item_id)
        .bind(item_id)
        .execute(&old_pool)
        .await?;
    }
    for (job_id, item_id) in [
        ("retry-automatic-job", "retry-automatic"),
        ("retry-other-error-job", "retry-other-error"),
        ("retry-no-snapshot-job", "retry-no-snapshot"),
    ] {
        sqlx::query(
            "INSERT INTO metadata_reidentify_jobs (
                 id, status, processed_count, total_count, mode, library_id, job_scope
             ) VALUES (?, 'DEFERRED', 1, 1, 'FILL_MISSING', 'retry-library', 'ITEMS')",
        )
        .bind(job_id)
        .execute(&old_pool)
        .await?;
        sqlx::query(
            "INSERT INTO metadata_reidentify_job_items (
                 job_id, item_id, status, error, request_fingerprint,
                 request_capabilities_json
             ) VALUES (?, ?, 'FAILED', ?, ?, '[]')",
        )
        .bind(job_id)
        .bind(item_id)
        .bind(if item_id == "retry-other-error" {
            "ITEM_NOT_FOUND"
        } else {
            "SCRAPER_UNAVAILABLE"
        })
        .bind((item_id != "retry-no-snapshot").then_some(b"retry-fingerprint".as_slice()))
        .execute(&old_pool)
        .await?;
    }
    sqlx::query("CREATE TABLE migration_retry_item_updates (count INTEGER NOT NULL)")
        .execute(&old_pool)
        .await?;
    sqlx::query("INSERT INTO migration_retry_item_updates (count) VALUES (0)")
        .execute(&old_pool)
        .await?;
    sqlx::query(
        "CREATE TRIGGER count_legacy_retry_item_updates
         AFTER UPDATE ON metadata_reidentify_job_items
         BEGIN
             UPDATE migration_retry_item_updates SET count = count + 1;
         END",
    )
    .execute(&old_pool)
    .await?;
    old_pool.close().await;

    let database = Database::connect(&config).await?;
    assert!(database.schema_version().await? >= 190);
    // 0165 stamps the legacy cooldown relative to the migration time. Read it right away: the
    // assertions below can take seconds under parallel test load and would shrink the window.
    let legacy_retry_cooldown: i64 = sqlx::query_scalar(
        "SELECT CAST(value AS INTEGER) - unixepoch() FROM server_settings
         WHERE key = 'metadata_fill_missing_legacy_retry_after'",
    )
    .fetch_one(database.pool())
    .await?;
    let jobs: Vec<(String, String, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT jobs.id, jobs.status, jobs.processed_count, jobs.total_count,
                items.automatic_retry_count, items.automatic_retry_consumed
         FROM metadata_reidentify_jobs jobs
         JOIN metadata_reidentify_job_items items ON items.job_id = jobs.id
         ORDER BY jobs.id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        jobs.iter()
            .map(
                |(id, status, processed, total, retry_count, retry_consumed)| {
                    (
                        id.as_str(),
                        status.as_str(),
                        *processed,
                        *total,
                        *retry_count,
                        *retry_consumed,
                    )
                }
            )
            .collect::<Vec<_>>(),
        vec![
            ("retry-automatic-job", "DEFERRED", 1, 1, 0, 0),
            ("retry-no-snapshot-job", "DEFERRED", 1, 1, 0, 0),
            ("retry-other-error-job", "DEFERRED", 1, 1, 0, 0),
        ]
    );
    let legacy_dispatch_sources: Vec<(String, String)> =
        sqlx::query_as("SELECT id, dispatch_source FROM metadata_reidentify_jobs ORDER BY id")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(
        legacy_dispatch_sources,
        vec![
            ("retry-automatic-job".to_owned(), "EXPLICIT".to_owned()),
            ("retry-no-snapshot-job".to_owned(), "EXPLICIT".to_owned()),
            ("retry-other-error-job".to_owned(), "EXPLICIT".to_owned()),
        ]
    );
    let retry_deadlines: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT item_id, automatic_retry_after - unixepoch()
         FROM metadata_reidentify_job_items ORDER BY item_id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(retry_deadlines.len(), 3);
    assert!(
        retry_deadlines
            .iter()
            .all(|(_, seconds_until_retry)| seconds_until_retry.is_none())
    );
    let item_update_count: i64 =
        sqlx::query_scalar("SELECT count FROM migration_retry_item_updates")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        item_update_count, 0,
        "0165 must not rewrite historical job items"
    );
    // 0165 records one shared cooldown of 300 seconds. Creating and migrating the database can
    // take seconds while other tests run in parallel, so allow for elapsed time instead of
    // pinning the remaining seconds to 299..=300.
    assert!(
        (280..=300).contains(&legacy_retry_cooldown),
        "cooldown={legacy_retry_cooldown}"
    );
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn sqlite_library_deletion_foreign_key_lookups_have_indexes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let indexes: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'index'")
            .fetch_all(database.pool())
            .await?;

    // Deletes must efficiently check every referencing FK, including NO ACTION references.
    for expected in [
        "idx_danmaku_match_job_items_media_source_id",
        "idx_chapter_detection_job_items_source_id",
        "idx_chapter_detection_job_items_item_id",
        "idx_chapter_detection_job_items_season_id",
        "idx_reconciliation_scan_entries_library_root_id",
        "idx_scan_job_paths_library_root_id",
        "idx_emby_migration_import_records_lux_item_id",
        "idx_user_item_state_item_id",
        "idx_playback_sessions_item_id",
        "idx_chapter_detection_jobs_library_id",
        "idx_strm_probe_jobs_library_id",
        "idx_web_playback_sessions_item_id",
        "idx_danmaku_match_jobs_library_id",
        "idx_scan_manifest_roots_library_root_id",
        "idx_library_cover_jobs_library_id",
        "idx_user_library_order_library_id",
        "idx_scan_local_metadata_batches_library_root_id",
        "idx_scan_manifest_deltas_manifest_root_id",
        "idx_emby_migration_item_matches_lux_item_id",
        "idx_metadata_reidentify_jobs_library_id",
        "idx_playback_sessions_media_source_id",
        "idx_web_playback_sessions_media_source_id",
        "idx_scan_manifest_deltas_parent_key",
    ] {
        assert!(
            indexes.iter().any(|index| index == expected),
            "missing library-delete foreign-key lookup index {expected}"
        );
    }
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn progressive_scan_metadata_schema_is_created_for_new_sqlite_databases()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database_path = temp_dir.path().join("progressive-empty.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    let migration_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    sqlx::migrate::Migrator::new(migration_dir.as_path())
        .await?
        .run(&pool)
        .await?;

    let schema_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    assert!(schema_version >= 190);
    for table in ["scan_local_metadata_batches", "item_metadata_completeness"] {
        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(table)
        .fetch_one(&pool)
        .await?;
        assert_eq!(table_count, 1, "missing progressive metadata table {table}");
    }

    let policy_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('libraries')
         WHERE name = 'scan_missing_metadata_auto_match_enabled'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(policy_column, 1);
    let image_stage_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_local_metadata_batches')
         WHERE name = 'images_completed_at'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(image_stage_column, 1);
    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('progressive-new', 'New', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    let default_policy: i64 = sqlx::query_scalar(
        "SELECT scan_missing_metadata_auto_match_enabled
         FROM libraries WHERE id = 'progressive-new'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(default_policy, 1);
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn progressive_scan_metadata_upgrade_preserves_existing_policy_intent()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let migration_dir = temp_dir.path().join("migrations");
    fs::create_dir(&migration_dir)?;
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?
            .ok_or("migration file has no version")?;
        if version <= 148 {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing name")?),
            )?;
        }
    }

    let database_path = temp_dir.path().join("progressive-upgrade.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind, realtime_metadata_auto_match_enabled)
         VALUES ('progressive-off', 'Progressive off', 'MOVIE', 0),
                ('progressive-on', 'Progressive on', 'MOVIE', 1)",
    )
    .execute(&pool)
    .await?;
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await?;

    let migration = source_dir.join("0151_progressive_scan_metadata.sql");
    fs::copy(
        &migration,
        migration_dir.join("0151_progressive_scan_metadata.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;

    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('workflow-two-job', 'progressive-off', 'RECONCILE_LIBRARY', 'COMPLETED', 'g2')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifests (
             id, job_id, library_id, state, workflow_version, resume_state
         ) VALUES (
             'workflow-two-manifest', 'workflow-two-job', 'progressive-off', 'FAILED', 2,
             'DISCOVERING'
         )",
    )
    .execute(&pool)
    .await?;

    fs::copy(
        source_dir.join("0152_scan_manifest_workflow_three.sql"),
        migration_dir.join("0152_scan_manifest_workflow_three.sql"),
    )?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('progressive-root', 'progressive-off', '/off', '/off', 1, 1)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_local_metadata_batches (
             id, job_id, library_root_id, batch_sequence, source_refs_json, source_count
         ) VALUES (
             'progressive-batch-before-image-stage', 'workflow-two-job', 'progressive-root', 0,
             '[\"old-source\"]', 1
         )",
    )
    .execute(&pool)
    .await?;
    fs::copy(
        source_dir.join("0153_scan_local_metadata_image_stage.sql"),
        migration_dir.join("0153_scan_local_metadata_image_stage.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;

    let legacy_manifest: (i64, Option<String>) = sqlx::query_as(
        "SELECT workflow_version, resume_state FROM scan_manifests
         WHERE id = 'workflow-two-manifest'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(legacy_manifest, (2, Some("DISCOVERING".to_owned())));
    let old_batch_image_stage: Option<i64> = sqlx::query_scalar(
        "SELECT images_completed_at FROM scan_local_metadata_batches
         WHERE id = 'progressive-batch-before-image-stage'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(old_batch_image_stage, None);
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('workflow-three-job', 'progressive-off', 'RECONCILE_LIBRARY', 'PENDING', 'g3')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifests (id, job_id, library_id, state, workflow_version)
         VALUES ('workflow-three-manifest', 'workflow-three-job', 'progressive-off', 'DISCOVERING', 3)",
    )
    .execute(&pool)
    .await?;

    let existing_policy: Vec<(String, i64)> = sqlx::query_as(
        "SELECT id, scan_missing_metadata_auto_match_enabled
         FROM libraries WHERE id IN ('progressive-off', 'progressive-on') ORDER BY id",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        existing_policy,
        vec![
            ("progressive-off".to_owned(), 0),
            ("progressive-on".to_owned(), 1)
        ]
    );

    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('progressive-new', 'New', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    let new_policy: i64 = sqlx::query_scalar(
        "SELECT scan_missing_metadata_auto_match_enabled
         FROM libraries WHERE id = 'progressive-new'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(new_policy, 1);

    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES (
             'progressive-item', 'progressive-on', 'MOVIE', 'Progressive', 'progressive',
             'LOCAL_CONFIRMED'
         )",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_local_metadata_batches (
             id, job_id, library_root_id, batch_sequence, source_refs_json, source_count
         ) VALUES ('progressive-batch', 'cleaned-scan-job', 'progressive-root', 0, '[\"source-1\"]', 1)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO item_metadata_completeness (
             item_id, capability, local_state, is_missing
         ) VALUES ('progressive-item', 'POSTER', 'READY', 1)",
    )
    .execute(&pool)
    .await?;
    assert!(
        sqlx::query(
            "INSERT INTO item_metadata_completeness (
                 item_id, capability, local_state, is_missing
             ) VALUES ('progressive-item', 'POSTER', 'READY', 0)",
        )
        .execute(&pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO item_metadata_completeness (
                 item_id, capability, local_state, is_missing
             ) VALUES ('progressive-item', 'BACKDROP', 'FAILED', 1)",
        )
        .execute(&pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO scan_local_metadata_batches (
                 id, job_id, library_root_id, batch_sequence, source_refs_json, source_count
             ) VALUES ('progressive-empty', 'cleaned-scan-job', 'progressive-root', 2, '[\"source-1\"]', 0)",
        )
        .execute(&pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO scan_local_metadata_batches (
                 id, job_id, library_root_id, batch_sequence, source_refs_json, source_count
             ) VALUES ('progressive-oversized', 'cleaned-scan-job', 'progressive-root', 1, '[]', 257)",
        )
        .execute(&pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO scan_local_metadata_batches (
                 id, job_id, library_root_id, batch_sequence, source_refs_json, source_count
             ) VALUES ('progressive-duplicate', 'cleaned-scan-job', 'progressive-root', 0, '[]', 1)",
        )
        .execute(&pool)
        .await
        .is_err()
    );

    sqlx::query("DELETE FROM libraries WHERE id = 'progressive-off'")
        .execute(&pool)
        .await?;
    sqlx::query("DELETE FROM media_items WHERE id = 'progressive-item'")
        .execute(&pool)
        .await?;
    let remaining_batches: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_local_metadata_batches WHERE id = 'progressive-batch'",
    )
    .fetch_one(&pool)
    .await?;
    let remaining_completeness: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM item_metadata_completeness WHERE item_id = 'progressive-item'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(remaining_batches, 0);
    assert_eq!(remaining_completeness, 0);
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn progressive_scan_policy_survives_sqlite_catalog_rebuild()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    fs::create_dir_all(&config_dir)?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let migration_dir = temp_dir.path().join("migrations-v148");
    fs::create_dir(&migration_dir)?;
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?
            .ok_or("migration file has no version")?;
        if version <= 148 {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing name")?),
            )?;
        }
    }

    let database_path = config_dir.join("lux.db");
    let old_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&old_pool)
        .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind, realtime_metadata_auto_match_enabled)
         VALUES ('rebuild-off', 'Rebuild off', 'MOVIE', 0),
                ('rebuild-on', 'Rebuild on', 'MOVIE', 1)",
    )
    .execute(&old_pool)
    .await?;
    old_pool.close().await;

    let database = Database::connect(&config).await?;
    let policies: Vec<(String, i64)> = sqlx::query_as(
        "SELECT id, scan_missing_metadata_auto_match_enabled
         FROM libraries WHERE id IN ('rebuild-off', 'rebuild-on') ORDER BY id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        policies,
        vec![("rebuild-off".to_owned(), 0), ("rebuild-on".to_owned(), 1)]
    );
    let schema_version = database.schema_version().await?;
    assert!(schema_version >= 190);
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn sqlite_homevideos_and_video_types_migrate_from_empty_and_existing_databases()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let old_config_dir = temp_dir.path().join("old-config");
    fs::create_dir(&old_config_dir)?;
    let old_database_path = old_config_dir.join("lux.db");
    let old_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&old_database_path)
                .create_if_missing(true),
        )
        .await?;
    let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    sqlx::migrate::Migrator::new(migrations.as_path())
        .await?
        .run(&old_pool)
        .await?;
    sqlx::query("INSERT INTO libraries (id, name, kind, scraper_id) VALUES ('old-library', 'Old', 'MIXED', 'tmdb')")
        .execute(&old_pool)
        .await?;
    sqlx::query(
        "INSERT INTO library_scrapers (library_id, scraper_id, position, role)
         VALUES ('old-library', 'tmdb', 0, 'PRIMARY')",
    )
    .execute(&old_pool)
    .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('old-root', 'old-library', '/media', '/media', 1, 1)",
    )
    .execute(&old_pool)
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('old-item', 'old-library', 'MOVIE', 'Old Movie', 'old movie', 'LOCAL_CONFIRMED')",
    )
    .execute(&old_pool)
    .await?;
    old_pool.close().await;

    let old_database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: old_config_dir,
    })
    .await?;
    let old_library_kind: String =
        sqlx::query_scalar("SELECT kind FROM libraries WHERE id = 'old-library'")
            .fetch_one(old_database.pool())
            .await?;
    let old_item_type: String =
        sqlx::query_scalar("SELECT item_type FROM media_items WHERE id = 'old-item'")
            .fetch_one(old_database.pool())
            .await?;
    let old_root_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM library_roots WHERE id = 'old-root'")
            .fetch_one(old_database.pool())
            .await?;
    let old_scraper_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM library_scrapers WHERE library_id = 'old-library'",
    )
    .fetch_one(old_database.pool())
    .await?;
    assert_eq!(old_library_kind, "MIXED");
    assert_eq!(old_item_type, "MOVIE");
    assert_eq!(old_root_count, 1);
    assert_eq!(old_scraper_count, 1);
    assert_homevideos_video_types_are_accepted(&old_database, "upgraded").await?;
    let old_foreign_key_violations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
            .fetch_one(old_database.pool())
            .await?;
    assert_eq!(old_foreign_key_violations, 0);
    old_database.close().await;

    let fresh_database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("fresh-config"),
    })
    .await?;
    assert_homevideos_video_types_are_accepted(&fresh_database, "fresh").await?;
    let fresh_foreign_key_violations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
            .fetch_one(fresh_database.pool())
            .await?;
    assert_eq!(fresh_foreign_key_violations, 0);
    fresh_database.close().await;
    Ok(())
}

async fn assert_homevideos_video_types_are_accepted(
    database: &Database,
    id_prefix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let library_id = format!("{id_prefix}-library");
    let item_id = format!("{id_prefix}-video");
    let preserved_media_indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name IN (
             'idx_media_items_library_added_visible',
             'idx_media_items_parent_available',
             'idx_media_items_series_available',
             'idx_media_items_home_unavailable_series',
             'idx_media_items_updated_at'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(preserved_media_indexes, 5);
    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES (?, ?, 'HOMEVIDEOS')")
        .bind(&library_id)
        .bind(format!("{id_prefix} home videos"))
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES (?, ?, 'VIDEO', 'Clip', 'clip', 'LOCAL_CONFIRMED')",
    )
    .bind(&item_id)
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    Ok(())
}

#[tokio::test]
async fn full_scan_manifest_schema_is_created_for_sqlite() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name LIKE 'scan_manifest_%'
         ORDER BY name",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        tables,
        vec![
            "scan_manifest_deltas",
            "scan_manifest_directories",
            "scan_manifest_entries",
            "scan_manifest_roots",
            "scan_manifest_seen_paths",
            "scan_manifests",
        ]
    );

    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'index' AND name LIKE 'idx_scan_manifest_%'
         ORDER BY name",
    )
    .fetch_all(database.pool())
    .await?;
    assert!(indexes.contains(&"idx_scan_manifest_directories_frontier".to_owned()));
    assert!(
        !indexes.contains(&"idx_scan_manifest_entries_path".to_owned()),
        "the entries primary key already has the same column order"
    );
    assert!(indexes.contains(&"idx_scan_manifest_deltas_pending".to_owned()));
    let entry_lookup_plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN
         SELECT device, inode FROM scan_manifest_entries
         WHERE manifest_id = 'manifest' AND library_root_id = 'root'
           AND relative_path = ''
         ORDER BY observation_sequence DESC LIMIT 1",
    )
    .fetch_all(database.pool())
    .await?;
    assert!(
        entry_lookup_plan
            .iter()
            .any(|(_, _, _, detail)| detail.contains("sqlite_autoindex_scan_manifest_entries_1")),
        "the primary-key index should retain the root-observation lookup: {entry_lookup_plan:?}"
    );
    let workflow_version_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifests')
         WHERE name = 'workflow_version'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(workflow_version_column, 1);
    let discovery_format_version_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifests')
         WHERE name = 'discovery_format_version'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(discovery_format_version_column, 1);
    let discovery_mode_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifests')
         WHERE name = 'discovery_mode'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(discovery_mode_column, 1);
    let sequence_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifest_roots')
         WHERE name = 'next_observation_sequence'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(sequence_column, 1);
    let last_seen_change_kind_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('filesystem_entries')
         WHERE name = 'last_seen_change_kind'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(last_seen_change_kind_column, 1);

    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('manifest-library', 'Manifest', 'MOVIE')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('manifest-root', 'manifest-library', '/media', '/media', 1, 1)",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('manifest-job', 'manifest-library', 'RECONCILE_LIBRARY', 'PENDING', 'generation-1')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifests (id, job_id, library_id, state)
         VALUES ('manifest-job', 'manifest-job', 'manifest-library', 'DISCOVERING')",
    )
    .execute(database.pool())
    .await?;
    let default_discovery_format_version: i64 = sqlx::query_scalar(
        "SELECT discovery_format_version FROM scan_manifests WHERE id = 'manifest-job'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(default_discovery_format_version, 2);
    let default_discovery_mode: String =
        sqlx::query_scalar("SELECT discovery_mode FROM scan_manifests WHERE id = 'manifest-job'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(default_discovery_mode, "PERSISTED");
    let default_targets_ready: i64 = sqlx::query_scalar(
        "SELECT postprocessing_targets_ready FROM scan_manifests WHERE id = 'manifest-job'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(default_targets_ready, 1);
    sqlx::query(
        "INSERT INTO scan_manifest_roots (manifest_id, library_root_id, state)
         VALUES ('manifest-job', 'manifest-root', 'PENDING')",
    )
    .execute(database.pool())
    .await?;
    let default_target_stage: String = sqlx::query_scalar(
        "SELECT postprocessing_target_stage FROM scan_manifest_roots
         WHERE manifest_id = 'manifest-job' AND library_root_id = 'manifest-root'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(default_target_stage, "DONE");
    let first_observation = sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at, fingerprint
         ) VALUES ('manifest-job', 'manifest-root', 'movie.mkv', 1, 'FILE', 123, 456, ?)",
    )
    .bind(vec![1_u8, 2, 3])
    .execute(database.pool())
    .await?;
    assert_eq!(first_observation.rows_affected(), 1);
    let duplicate_observation = sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at
         ) VALUES ('manifest-job', 'manifest-root', 'movie.mkv', 1, 'FILE', 999, 999)",
    )
    .execute(database.pool())
    .await;
    assert!(duplicate_observation.is_err());
    sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at, fingerprint
         ) VALUES ('manifest-job', 'manifest-root', 'movie.mkv', 2, 'FILE', 124, 457, ?)",
    )
    .bind(vec![4_u8, 5, 6])
    .execute(database.pool())
    .await?;
    let observation_versions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_entries
         WHERE manifest_id = 'manifest-job' AND library_root_id = 'manifest-root'
           AND relative_path = 'movie.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(observation_versions, 2);
    let invalid_state = sqlx::query(
        "UPDATE scan_manifest_roots SET state = 'DONE'
         WHERE manifest_id = 'manifest-job' AND library_root_id = 'manifest-root'",
    )
    .execute(database.pool())
    .await;
    assert!(invalid_state.is_err());

    let manifest_schema: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master
         WHERE type = 'table' AND name = 'scan_manifests'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(manifest_schema.contains("'DISCOVERING'"));
    assert!(manifest_schema.contains("'POSTPROCESSING'"));
    let manifest_entry_device: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifest_entries')
         WHERE name = 'device'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(manifest_entry_device, 1);
    let manifest_resume_state: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifests')
         WHERE name = 'resume_state'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(manifest_resume_state, 1);
    assert!(database.schema_version().await? >= 190);

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn completed_manifest_payload_cleanup_is_batched_and_preserves_checkpoints()
-> Result<(), Box<dyn std::error::Error>> {
    const OBSERVATION_COUNT: i64 = 1_001;

    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Manifest cleanup", LibraryKind::Movie, false)
        .await?;
    let root_path = temp_dir.path().join("Movies");
    fs::create_dir_all(&root_path)?;
    let root = libraries
        .add_root(library.id, root_path.to_str().ok_or("non-UTF-8 root path")?)
        .await?
        .root;
    let library_id = library.id.to_string();
    let root_id = root.id.to_string();
    for (job_id, status) in [
        ("cleanup-completed-manifest-job", "COMPLETED"),
        ("cleanup-failed-manifest-job", "FAILED"),
    ] {
        sqlx::query(
            "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, scan_phase)
             VALUES (?, ?, 'RECONCILE_LIBRARY', ?, ?, 'IDLE')",
        )
        .bind(job_id)
        .bind(&library_id)
        .bind(status)
        .bind(format!("generation-{job_id}"))
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO scan_manifests (
             id, job_id, library_id, state, resume_state, root_count,
             observed_file_count, add_count, applied_delta_count, completed_at
         ) VALUES
             ('cleanup-completed-manifest', 'cleanup-completed-manifest-job', ?,
              'COMPLETED', NULL, 1, ?, ?, ?, unixepoch()),
             ('cleanup-failed-manifest', 'cleanup-failed-manifest-job', ?,
              'FAILED', 'APPLYING', 1, 1, 1, 0, NULL)",
    )
    .bind(&library_id)
    .bind(OBSERVATION_COUNT)
    .bind(OBSERVATION_COUNT)
    .bind(OBSERVATION_COUNT)
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_roots (manifest_id, library_root_id, state)
         VALUES ('cleanup-completed-manifest', ?, 'COMPLETE'),
                ('cleanup-failed-manifest', ?, 'INCOMPLETE')",
    )
    .bind(&root_id)
    .bind(&root_id)
    .execute(database.pool())
    .await?;

    let mut transaction = database.pool().begin().await?;
    for index in 0..OBSERVATION_COUNT {
        let relative_path = format!("movie-{index:04}.mkv");
        let directory_path = format!("folder-{index:04}");
        sqlx::query(
            "INSERT INTO scan_manifest_directories (
                 manifest_id, library_root_id, relative_path, state
             ) VALUES ('cleanup-completed-manifest', ?, ?, 'COMPLETE')",
        )
        .bind(&root_id)
        .bind(directory_path)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO scan_manifest_entries (
                 manifest_id, library_root_id, relative_path, observation_sequence,
                 entry_kind, size, modified_at, fingerprint
             ) VALUES ('cleanup-completed-manifest', ?, ?, 1, 'FILE', 1, 1, X'01')",
        )
        .bind(&root_id)
        .bind(&relative_path)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO scan_manifest_deltas (
                 id, manifest_id, library_root_id, relative_path,
                 observation_sequence, delta_kind, state
             ) VALUES (?, 'cleanup-completed-manifest', ?, ?, 1, 'ADD', 'APPLIED')",
        )
        .bind(format!("delta-{index:04}"))
        .bind(&root_id)
        .bind(relative_path)
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query(
        "INSERT INTO scan_manifest_directories (
             manifest_id, library_root_id, relative_path, state
         ) VALUES ('cleanup-failed-manifest', ?, 'retained', 'PENDING')",
    )
    .bind(&root_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at
         ) VALUES ('cleanup-failed-manifest', ?, 'retained.mkv', 1, 'FILE', 1, 1)",
    )
    .bind(&root_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_deltas (
             id, manifest_id, library_root_id, relative_path,
             observation_sequence, delta_kind, state
         ) VALUES ('retained-delta', 'cleanup-failed-manifest', ?, 'retained.mkv',
                  1, 'ADD', 'PENDING')",
    )
    .bind(&root_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_seen_paths (
             manifest_id, library_root_id, relative_path
         ) VALUES ('cleanup-completed-manifest', ?, 'seen.mkv'),
                  ('cleanup-failed-manifest', ?, 'retained.mkv')",
    )
    .bind(&root_id)
    .bind(&root_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;

    let report = database
        .run_database_lifecycle_cleanup()
        .await?
        .ok_or("cleanup should report completed manifest payloads")?;
    assert_eq!(
        report.scan_manifest_entries_deleted,
        OBSERVATION_COUNT as u64 + 1
    );
    assert_eq!(
        report.scan_manifest_directories_deleted,
        OBSERVATION_COUNT as u64
    );
    assert_eq!(
        report.scan_manifest_deltas_deleted,
        OBSERVATION_COUNT as u64
    );

    let completed_summary: (String, i64, i64) = sqlx::query_as(
        "SELECT state, observed_file_count, add_count FROM scan_manifests
         WHERE id = 'cleanup-completed-manifest'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        completed_summary,
        ("COMPLETED".to_owned(), OBSERVATION_COUNT, OBSERVATION_COUNT)
    );
    let retained_checkpoint: (String, String, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT m.state, m.resume_state,
                (SELECT COUNT(*) FROM scan_manifest_entries e WHERE e.manifest_id = m.id),
                (SELECT COUNT(*) FROM scan_manifest_directories d WHERE d.manifest_id = m.id),
                (SELECT COUNT(*) FROM scan_manifest_deltas x WHERE x.manifest_id = m.id),
                (SELECT COUNT(*) FROM scan_manifest_seen_paths s WHERE s.manifest_id = m.id)
         FROM scan_manifests m WHERE m.id = 'cleanup-failed-manifest'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        retained_checkpoint,
        ("FAILED".to_owned(), "APPLYING".to_owned(), 1, 1, 1, 1)
    );
    assert!(database.run_database_lifecycle_cleanup().await?.is_none());
    Ok(())
}

#[tokio::test]
async fn full_scan_manifest_upgrade_preserves_existing_scan_data()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("migrations");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?;
        if version.is_some_and(|version| version <= 127) {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing filename")?),
            )?;
        }
    }

    let database_path = temp_dir.path().join("manifest-upgrade.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('legacy-library', 'Legacy', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('legacy-root', 'legacy-library', '/media', '/media', 1, 1)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('legacy-job', 'legacy-library', 'RECONCILE_LIBRARY', 'RUNNING', 'legacy-generation')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO reconciliation_scan_entries (
             job_id, library_root_id, relative_path, entry_type
         ) VALUES ('legacy-job', 'legacy-root', 'movie.mkv', 'FILE')",
    )
    .execute(&pool)
    .await?;

    fs::copy(
        source_dir.join("0128_full_scan_manifest.sql"),
        migration_dir.join("0128_full_scan_manifest.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;

    sqlx::query(
        "INSERT INTO scan_manifests (id, job_id, library_id, state)
         VALUES ('legacy-manifest', 'legacy-job', 'legacy-library', 'DISCOVERING')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_roots (manifest_id, library_root_id, state)
         VALUES ('legacy-manifest', 'legacy-root', 'COMPLETE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at, inode, fingerprint
         ) VALUES ('legacy-manifest', 'legacy-root', 'movie.mkv', 1,
                   'FILE', 1234, 100, 42, X'010203')",
    )
    .execute(&pool)
    .await?;

    fs::copy(
        source_dir.join("0129_login_background_plugin_cache.sql"),
        migration_dir.join("0129_login_background_plugin_cache.sql"),
    )?;
    fs::copy(
        source_dir.join("0130_scan_manifest_entry_device.sql"),
        migration_dir.join("0130_scan_manifest_entry_device.sql"),
    )?;
    fs::copy(
        source_dir.join("0131_scan_manifest_resume_state.sql"),
        migration_dir.join("0131_scan_manifest_resume_state.sql"),
    )?;
    fs::copy(
        source_dir.join("0132_streamed_manifest_indexing.sql"),
        migration_dir.join("0132_streamed_manifest_indexing.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;

    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('v2-library', 'V2', 'MOVIE')")
        .execute(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('v2-root', 'v2-library', '/v2-root', '/v2-root', 1, 1)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('active-v2-job', 'v2-library', 'RECONCILE_LIBRARY', 'RUNNING',
                 'active-v2-generation')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifests (
             id, job_id, library_id, state, workflow_version, root_count
         ) VALUES ('active-v2-manifest', 'active-v2-job', 'v2-library',
                  'DISCOVERING', 2, 1)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_roots (manifest_id, library_root_id, state)
         VALUES ('active-v2-manifest', 'v2-root', 'SCANNING')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at, device, inode, fingerprint
         ) VALUES ('active-v2-manifest', 'v2-root', 'still-present.mkv', 1,
                   'FILE', 123, 456, 1, 2, X'010203')",
    )
    .execute(&pool)
    .await?;
    fs::copy(
        source_dir.join("0133_skip_redundant_source_availability_update.sql"),
        migration_dir.join("0133_skip_redundant_source_availability_update.sql"),
    )?;
    fs::copy(
        source_dir.join("0134_drop_redundant_manifest_entry_index.sql"),
        migration_dir.join("0134_drop_redundant_manifest_entry_index.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;
    fs::copy(
        source_dir.join("0135_manifest_discovery_format_and_seen_paths.sql"),
        migration_dir.join("0135_manifest_discovery_format_and_seen_paths.sql"),
    )?;
    fs::copy(
        source_dir.join("0136_manifest_postprocessing_target_checkpoint.sql"),
        migration_dir.join("0136_manifest_postprocessing_target_checkpoint.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;

    let schema_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(schema_version, 136);
    let legacy_workflow_version: i64 = sqlx::query_scalar(
        "SELECT workflow_version FROM scan_manifests WHERE id = 'legacy-manifest'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(legacy_workflow_version, 1);
    let legacy_discovery_format_version: i64 = sqlx::query_scalar(
        "SELECT discovery_format_version FROM scan_manifests WHERE id = 'legacy-manifest'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(legacy_discovery_format_version, 2);
    let active_v2_versions: (i64, i64, i64) = sqlx::query_as(
        "SELECT manifest.workflow_version, manifest.discovery_format_version,
                (SELECT COUNT(*) FROM scan_manifest_entries entry
                 WHERE entry.manifest_id = manifest.id AND entry.entry_kind = 'FILE')
         FROM scan_manifests manifest WHERE manifest.id = 'active-v2-manifest'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(active_v2_versions, (2, 2, 1));
    let sequence_allocator: i64 = sqlx::query_scalar(
        "SELECT next_observation_sequence FROM scan_manifest_roots
         WHERE manifest_id = 'legacy-manifest' AND library_root_id = 'legacy-root'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(sequence_allocator, 0);
    let legacy_status: String =
        sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = 'legacy-job'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(legacy_status, "RUNNING");
    let legacy_entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries WHERE job_id = 'legacy-job'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(legacy_entries, 1);
    let manifests: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scan_manifests")
        .fetch_one(&pool)
        .await?;
    assert_eq!(manifests, 2);
    let manifest_entry: (i64, i64, Option<i64>) = sqlx::query_as(
        "SELECT size, inode, device FROM scan_manifest_entries
         WHERE manifest_id = 'legacy-manifest' AND relative_path = 'movie.mkv'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(manifest_entry, (1234, 42, None));
    let resume_state_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifests')
         WHERE name = 'resume_state'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(resume_state_column, 1);

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn postgres_manifest_migration_uses_only_supported_schema_constructs()
-> Result<(), Box<dyn std::error::Error>> {
    let postgres_migration = include_str!("../migrations-postgres/0128_full_scan_manifest.sql");
    for forbidden in ["COPY ", "UPDATE ", "FOR UPDATE", "TEMP TABLE"] {
        assert!(
            !postgres_migration.contains(forbidden),
            "manifest migration contains unsupported core SQL construct {forbidden}"
        );
    }

    let sqlite_compatible_migration = postgres_migration
        .replace("BYTEA", "BLOB")
        .replace("BIGINT", "INTEGER")
        .replace(
            "DEFAULT (EXTRACT(EPOCH FROM NOW())::INTEGER)",
            "DEFAULT (unixepoch())",
        );
    let postgres_device_migration =
        include_str!("../migrations-postgres/0130_scan_manifest_entry_device.sql");
    assert!(!postgres_device_migration.contains("BIGSERIAL"));
    let postgres_resume_state_migration =
        include_str!("../migrations-postgres/0131_scan_manifest_resume_state.sql");
    assert!(!postgres_resume_state_migration.contains("BIGSERIAL"));
    let postgres_streamed_manifest_migration =
        include_str!("../migrations-postgres/0132_streamed_manifest_indexing.sql");
    assert!(!postgres_streamed_manifest_migration.contains("BIGSERIAL"));
    let postgres_availability_migration =
        include_str!("../migrations-postgres/0133_skip_redundant_source_availability_update.sql");
    assert!(postgres_availability_migration.contains("has_available_source = 0"));
    assert_eq!(
        include_str!("../migrations-postgres/0134_drop_redundant_manifest_entry_index.sql"),
        include_str!("../migrations/0134_drop_redundant_manifest_entry_index.sql")
    );
    let postgres_seen_paths_migration =
        include_str!("../migrations-postgres/0135_manifest_discovery_format_and_seen_paths.sql");
    assert_eq!(
        postgres_seen_paths_migration.replace("BIGINT", "INTEGER"),
        include_str!("../migrations/0135_manifest_discovery_format_and_seen_paths.sql")
    );
    let postgres_target_checkpoint_migration =
        include_str!("../migrations-postgres/0136_manifest_postprocessing_target_checkpoint.sql");
    assert_eq!(
        postgres_target_checkpoint_migration.replace("BIGINT", "INTEGER"),
        include_str!("../migrations/0136_manifest_postprocessing_target_checkpoint.sql")
    );
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("migrations");
    fs::create_dir(&migration_dir)?;
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    sqlx::query("CREATE TABLE libraries (id TEXT PRIMARY KEY)")
        .execute(&pool)
        .await?;
    sqlx::query("CREATE TABLE library_roots (id TEXT PRIMARY KEY)")
        .execute(&pool)
        .await?;
    sqlx::query("CREATE TABLE scan_jobs (id TEXT PRIMARY KEY)")
        .execute(&pool)
        .await?;
    fs::write(
        migration_dir.join("0128_full_scan_manifest.sql"),
        sqlite_compatible_migration,
    )?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;

    sqlx::query("INSERT INTO libraries (id) VALUES ('library')")
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO library_roots (id) VALUES ('root')")
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO scan_jobs (id) VALUES ('job')")
        .execute(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO scan_manifests (id, job_id, library_id, state)
         VALUES ('manifest', 'job', 'library', 'DISCOVERING')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_roots (manifest_id, library_root_id, state)
         VALUES ('manifest', 'root', 'COMPLETE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at, inode, fingerprint
         ) VALUES ('manifest', 'root', 'movie.mkv', 1, 'FILE', 987, 100, 42, X'010203')",
    )
    .execute(&pool)
    .await?;

    fs::write(
        migration_dir.join("0130_scan_manifest_entry_device.sql"),
        postgres_device_migration.replace("BIGINT", "INTEGER"),
    )?;
    fs::write(
        migration_dir.join("0131_scan_manifest_resume_state.sql"),
        postgres_resume_state_migration,
    )?;
    fs::write(
        migration_dir.join("0132_streamed_manifest_indexing.sql"),
        postgres_streamed_manifest_migration.replace("BIGINT", "INTEGER"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;
    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'
         AND name IN (
             'scan_manifests', 'scan_manifest_roots', 'scan_manifest_directories',
             'scan_manifest_entries', 'scan_manifest_deltas'
         )",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(tables, 5);
    let device_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifest_entries')
         WHERE name = 'device'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(device_column, 1);
    let resume_state_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifests')
         WHERE name = 'resume_state'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(resume_state_column, 1);
    let manifest_entry: (i64, i64, Option<i64>) = sqlx::query_as(
        "SELECT size, inode, device FROM scan_manifest_entries
         WHERE manifest_id = 'manifest' AND relative_path = 'movie.mkv'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(manifest_entry, (987, 42, None));
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn filesystem_entry_path_keeps_only_the_unique_index()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    let duplicate_index: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'idx_filesystem_entries_root_path'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(duplicate_index, 0);
    let filesystem_schema: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master
         WHERE type = 'table' AND name = 'filesystem_entries'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(filesystem_schema.contains("UNIQUE (library_root_id, relative_path)"));
    Ok(())
}

#[tokio::test]
async fn redundant_child_indexes_are_removed_after_migration()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    let redundant_indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name IN (
             'idx_item_images_item_id',
             'idx_media_streams_source_id',
             'idx_person_credits_item',
             'idx_person_credits_person'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(redundant_indexes, 0);
    Ok(())
}

#[tokio::test]
async fn scan_indexes_keep_only_required_rows_and_lookup_order()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    let reconciliation_schema: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master
         WHERE type = 'table' AND name = 'reconciliation_scan_entries'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(
        reconciliation_schema
            .contains("PRIMARY KEY (job_id, entry_type, library_root_id, relative_path)")
    );

    let wide_reconciliation_index: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'idx_reconciliation_scan_entries_pending'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(wide_reconciliation_index, 0);

    let scan_target_indexes: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, sql FROM sqlite_master
         WHERE type = 'index' AND name IN (
             'idx_scan_job_targets_probe',
             'idx_scan_job_targets_metadata',
             'idx_scan_job_targets_thumbnail'
         )
         ORDER BY name",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(scan_target_indexes.len(), 3);
    for (_, sql) in scan_target_indexes {
        assert!(
            sql.replace(' ', "").contains("IN('PENDING','FAILED')"),
            "unexpected index: {sql}"
        );
    }

    let external_stream_index: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'idx_media_streams_external_path'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(external_stream_index, 0);
    assert!(database.schema_version().await? >= 190);
    Ok(())
}

#[tokio::test]
async fn scan_index_compaction_preserves_existing_rows_during_upgrade()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("migrations");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?;
        if version.is_some_and(|version| version <= 117) {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing filename")?),
            )?;
        }
    }

    let database_path = temp_dir.path().join("upgrade.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;

    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('migration-library', 'Migration', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('migration-root', 'migration-library', '/media', '/media', 1, 1)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('migration-job', 'migration-library', 'RECONCILE_LIBRARY', 'RUNNING', 'generation-1')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO reconciliation_scan_entries (
             job_id, library_root_id, relative_path, entry_type, created_at
         ) VALUES
             ('migration-job', 'migration-root', '', 'DIRECTORY', 101),
             ('migration-job', 'migration-root', 'movie.mkv', 'FILE', 102)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_job_targets (
             job_id, target_type, target_id, item_id, change_kind,
             probe_state, metadata_state, thumbnail_state
         ) VALUES
             ('migration-job', 'ITEM', 'target-pending', 'item-pending', 'NEW',
              'SKIPPED', 'PENDING', 'PENDING'),
             ('migration-job', 'ITEM', 'target-done', 'item-done', 'CHANGED',
              'SKIPPED', 'DONE', 'DONE')",
    )
    .execute(&pool)
    .await?;

    fs::copy(
        source_dir.join("0118_scan_index_compaction.sql"),
        migration_dir.join("0118_scan_index_compaction.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;

    let entries: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT job_id, library_root_id, relative_path, created_at
         FROM reconciliation_scan_entries
         ORDER BY entry_type, relative_path",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        entries,
        vec![
            (
                "migration-job".to_owned(),
                "migration-root".to_owned(),
                "".to_owned(),
                101,
            ),
            (
                "migration-job".to_owned(),
                "migration-root".to_owned(),
                "movie.mkv".to_owned(),
                102,
            ),
        ]
    );

    let target_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets")
        .fetch_one(&pool)
        .await?;
    assert_eq!(target_count, 2);
    let foreign_key_violations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
            .fetch_one(&pool)
            .await?;
    assert_eq!(foreign_key_violations, 0);
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&pool)
        .await?;
    assert_eq!(foreign_keys, 1);
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn recommendation_query_indexes_are_created_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    let indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name IN (
             'idx_user_item_state_recommendation_last_played',
             'idx_playback_sessions_recommendation_last_event',
             'idx_user_item_state_recommendation_favorites',
             'idx_item_images_recommendation_lookup'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(indexes, 4);
    Ok(())
}

#[tokio::test]
async fn scan_job_targets_schema_is_available_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let table_name: String = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'scan_job_targets'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(table_name, "scan_job_targets");
    assert!(database.schema_version().await? >= 190);
    Ok(())
}

#[tokio::test]
async fn login_background_plugin_cache_schema_is_available_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'login_background_plugin_cache'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(table_count, 1);

    sqlx::query("INSERT INTO installed_plugins (plugin_id) VALUES ('org.lux.background-test')")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO login_background_plugin_cache (plugin_id, payload_json, refreshed_at)
         VALUES ('org.lux.background-test', '{}', 100)",
    )
    .execute(database.pool())
    .await?;

    let oversized_payload = "x".repeat(262_145);
    let oversized_insert = sqlx::query(
        "INSERT INTO login_background_plugin_cache (plugin_id, payload_json, refreshed_at)
         VALUES ('org.lux.background-test', ?, 101)",
    )
    .bind(oversized_payload)
    .execute(database.pool())
    .await;
    assert!(oversized_insert.is_err());

    sqlx::query("DELETE FROM installed_plugins WHERE plugin_id = 'org.lux.background-test'")
        .execute(database.pool())
        .await?;
    let remaining_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM login_background_plugin_cache")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_rows, 0);
    Ok(())
}

#[tokio::test]
async fn emby_migration_migration_creates_state_and_history_tables()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    for table in [
        "emby_migration_jobs",
        "emby_migration_user_links",
        "emby_migration_item_matches",
        "emby_migration_import_records",
        "emby_migration_handled_items",
        "emby_migration_user_bindings",
        "emby_migration_person_favorites",
        "playback_history_events",
        "legacy_person_migration_state",
        "person_manifest_restore_state",
        "person_manifest_index_state",
        "media_item_provider_ids",
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
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn metadata_job_scope_migration_adds_explicit_scope_and_summary_indexes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    let columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('metadata_reidentify_jobs')
         WHERE name IN ('library_id', 'job_scope', 'dispatch_source')",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(columns, 3);
    let indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name IN (
             'idx_metadata_reidentify_items_item_job',
             'idx_metadata_reidentify_jobs_scope_status',
             'idx_metadata_reidentify_jobs_automatic_queue'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(indexes, 3);
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn empty_sqlite_database_creates_people_index_tables_and_indexes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name IN (
             'person_index_rebuild_jobs',
             'person_index_item_state',
             'person_credits'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(tables, 3);
    let indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name IN (
             'idx_person_index_rebuild_jobs_status',
             'idx_person_credits_person_item',
             'idx_media_items_people_visible'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(indexes, 3);

    database.close().await;

    let database = Database::connect(&config).await?;
    sqlx::query("DROP INDEX idx_media_items_people_visible")
        .execute(database.pool())
        .await?;
    database.close().await;

    let database = Database::connect(&config).await?;
    let restored_indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'idx_media_items_people_visible'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(restored_indexes, 1);
    Ok(())
}

#[tokio::test]
async fn sqlite_media_item_search_triggers_follow_rebuilt_table()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    let trigger_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master
         WHERE type = 'trigger' AND name = 'media_items_search_ai'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(!trigger_sql.contains("media_items_legacy"));
    assert!(
        !trigger_sql.contains("FROM item_aliases"),
        "a newly inserted media item cannot have aliases yet; alias triggers index them after insertion"
    );
    let stale_references: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE sql LIKE '%media_items_legacy%'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stale_references, 0);

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn media_chapter_migration_creates_source_scoped_table()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    assert!(database.schema_version().await? >= 190);
    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'media_chapters'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(table_count, 1);
    let job_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name IN ('chapter_detection_jobs', 'chapter_detection_job_items')",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(job_table_count, 2);

    let create_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'media_chapters'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(create_sql.contains("REFERENCES media_sources(id) ON DELETE CASCADE"));
    assert!(create_sql.contains("INTRO_START"));
    assert!(create_sql.contains("CREDITS_START"));
    assert!(!create_sql.contains("'CHAPTER'"));

    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Marker migration", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&media_root).await?;
    tokio::fs::write(media_root.join("Marker.Movie.2026.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources")
        .fetch_one(database.pool())
        .await?;

    sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, 10000000, 'INTRO_START', 0, 'org.lux.detector', 0.95)",
    )
    .bind("intro-start")
    .bind(&source_id)
    .execute(database.pool())
    .await?;
    let ordinary_chapter = sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, 20000000, 'CHAPTER', 1, 'org.lux.detector', 0.95)",
    )
    .bind("ordinary-chapter")
    .bind(&source_id)
    .execute(database.pool())
    .await;
    assert!(ordinary_chapter.is_err());
    let duplicate_marker = sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, 30000000, 'INTRO_START', 1, 'org.lux.detector', 0.90)",
    )
    .bind("duplicate-intro-start")
    .bind(&source_id)
    .execute(database.pool())
    .await;
    assert!(duplicate_marker.is_err());
    let negative_start = sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, -1, 'INTRO_END', 1, 'org.lux.detector', 0.90)",
    )
    .bind("negative-intro-end")
    .bind(&source_id)
    .execute(database.pool())
    .await;
    assert!(negative_start.is_err());
    let invalid_confidence = sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, 40000000, 'CREDITS_START', 2, 'org.lux.detector', 1.1)",
    )
    .bind("invalid-credits-start")
    .bind(&source_id)
    .execute(database.pool())
    .await;
    assert!(invalid_confidence.is_err());
    let blank_provider = sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, 40000000, 'CREDITS_START', 2, '   ', 0.90)",
    )
    .bind("blank-provider")
    .bind(&source_id)
    .execute(database.pool())
    .await;
    assert!(blank_provider.is_err());

    sqlx::query("DELETE FROM media_sources WHERE id = ?")
        .bind(&source_id)
        .execute(database.pool())
        .await?;
    let remaining_markers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_chapters")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(remaining_markers, 0);

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn strm_probe_scan_job_reference_prevents_scan_job_deletion()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Migration test", LibraryKind::Movie, false)
        .await?;

    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES (?, ?, 'INCREMENTAL_SCAN', 'COMPLETED', 'migration-test')",
    )
    .bind("migration-scan-job")
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO strm_probe_jobs (
            id, operation_id, library_id, status, concurrency, target_scan_job_id
         ) VALUES (?, ?, ?, 'COMPLETED', 1, ?)",
    )
    .bind("migration-probe-job")
    .bind("migration-operation")
    .bind(library.id.to_string())
    .bind("migration-scan-job")
    .execute(database.pool())
    .await?;

    let deletion = sqlx::query("DELETE FROM scan_jobs WHERE id = ?")
        .bind("migration-scan-job")
        .execute(database.pool())
        .await;
    assert!(deletion.is_err());

    let target_scan_job_id: Option<String> =
        sqlx::query_scalar("SELECT target_scan_job_id FROM strm_probe_jobs WHERE id = ?")
            .bind("migration-probe-job")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(target_scan_job_id.as_deref(), Some("migration-scan-job"));

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn sqlite_write_probe_succeeds_and_only_persists_reserved_marker()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    database.probe_write().await?;
    assert!(database.schema_version().await? >= 190);
    let probe_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM lux_meta WHERE key = '__lux_write_probe__'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(probe_rows, 1);

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn library_registers_reconciliation_and_metadata_tasks_only()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, true)
        .await?;

    let task_types: Vec<String> = sqlx::query_scalar(
        "SELECT task_type FROM scheduled_task_configs
         WHERE owner_type = 'LIBRARY' AND owner_id = ?
         ORDER BY task_type",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        task_types,
        vec![
            "METADATA_PARSE".to_owned(),
            "RECONCILIATION_SCAN".to_owned()
        ]
    );

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn scheduled_task_plans_group_new_library_registrations()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let first = libraries
        .create_library("Movies", LibraryKind::Movie, true)
        .await?;
    let second = libraries
        .create_library("More Movies", LibraryKind::Movie, true)
        .await?;

    let plan_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scheduled_task_plans WHERE task_type = 'RECONCILIATION_SCAN'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(plan_count, 1);
    let membership_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scheduled_task_plan_libraries l
         JOIN scheduled_task_plans p ON p.id = l.plan_id
         WHERE p.task_type = 'RECONCILIATION_SCAN'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(membership_count, 2);
    let plan_ids: Vec<String> = sqlx::query_scalar(
        "SELECT plan_id FROM scheduled_task_configs
         WHERE task_type = 'RECONCILIATION_SCAN' AND owner_id IN (?, ?)
         ORDER BY owner_id",
    )
    .bind(first.id.to_string())
    .bind(second.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(plan_ids.len(), 2);
    assert_eq!(plan_ids[0], plan_ids[1]);
    Ok(())
}

#[tokio::test]
async fn library_persists_chapter_source_selection() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library_with_scraper_and_chapter_source(
            "Shows",
            LibraryKind::Series,
            false,
            None,
            None,
            false,
        )
        .await?;
    assert_eq!(library.chapter_source_id, None);

    let updated = libraries
        .update_settings(
            library.id,
            luxd::application::libraries::LibrarySettingsPatch {
                chapter_source_id: Some(Some("org.lux.intro-outro-detector".to_owned())),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(
        updated.library.chapter_source_id.as_deref(),
        Some("org.lux.intro-outro-detector")
    );

    let reopened = libraries.get_library(library.id).await?;
    assert_eq!(
        reopened.chapter_source_id.as_deref(),
        Some("org.lux.intro-outro-detector")
    );
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn read_only_config_dir_returns_a_clear_database_error()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("readonly");
    fs::create_dir(&config_dir)?;
    let mut permissions = fs::metadata(&config_dir)?.permissions();
    permissions.set_mode(0o500);
    fs::set_permissions(&config_dir, permissions)?;

    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let result = Database::connect(&config).await;

    let mut permissions = fs::metadata(&config_dir)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&config_dir, permissions)?;

    let error = match result {
        Ok(database) => {
            database.close().await;
            return Err("read-only config directory unexpectedly opened".into());
        }
        Err(error) => error,
    };
    assert!(error.to_string().contains("lux.db"));
    Ok(())
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn resume_visibility_upgrade_preserves_existing_playback_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("old-migrations");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let entry = entry?;
        let filename = entry.file_name();
        let version: i64 = filename
            .to_str()
            .ok_or("invalid filename")?
            .split('_')
            .next()
            .ok_or("missing migration version")?
            .parse()?;
        if version <= 175 {
            fs::copy(entry.path(), migration_dir.join(filename))?;
        }
    }
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
                 VALUES ('resume-user', 'resume-user', 'Resume User', 'test-only')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('resume-library', 'Resume', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query("INSERT INTO media_items (id, library_id, item_type, title, sort_title, identification_status)
                 VALUES ('resume-item', 'resume-library', 'MOVIE', 'Resume', 'resume', 'LOCAL_CONFIRMED')")
        .execute(&pool).await?;
    sqlx::query("INSERT INTO user_item_state (user_id, item_id, position_ticks, is_favorite,
                 play_count, last_played_at, version) VALUES ('resume-user', 'resume-item', 1300000000, 1, 2, 12345, 7)")
        .execute(&pool).await?;
    sqlx::migrate::Migrator::new(source_dir)
        .await?
        .run(&pool)
        .await?;
    let row: (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT position_ticks, is_played, is_favorite, play_count, last_played_at,
                version, hide_from_resume FROM user_item_state WHERE item_id = 'resume-item'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(row, (1300000000, 0, 1, 2, 12345, 7, 0));
    assert!(
        sqlx::query("UPDATE user_item_state SET hide_from_resume = 2")
            .execute(&pool)
            .await
            .is_err()
    );
    let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(version, latest_migration_version("migrations")?);
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn user_registration_requests_migration_runs_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(temp.path().join("registration-empty.db"))
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await?;
    Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await?
        .run(&pool)
        .await?;
    assert_sqlite_registration_request_schema(&pool).await?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn user_registration_requests_migration_upgrades_existing_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let selected = temp.path().join("migrations");
    fs::create_dir(&selected)?;
    for entry in fs::read_dir(&source)? {
        let entry = entry?;
        let filename = entry.file_name();
        let version: i64 = filename
            .to_str()
            .ok_or("invalid filename")?
            .split('_')
            .next()
            .ok_or("missing version")?
            .parse()?;
        if version <= 202 {
            fs::copy(entry.path(), selected.join(filename))?;
        }
    }
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(temp.path().join("registration-upgrade.db"))
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await?;
    Migrator::new(selected.clone()).await?.run(&pool).await?;
    sqlx::query("INSERT INTO users (id, username_normalized, display_name, password_hash) VALUES ('preserved-registration-owner', 'preserved-registration-owner', 'Preserved', 'test-hash')").execute(&pool).await?;
    fs::copy(
        source.join("0203_user_registration_requests.sql"),
        selected.join("0203_user_registration_requests.sql"),
    )?;
    Migrator::new(selected).await?.run(&pool).await?;
    assert_sqlite_registration_request_schema(&pool).await?;
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT display_name FROM users WHERE id = 'preserved-registration-owner'"
        )
        .fetch_one(&pool)
        .await?,
        "Preserved"
    );
    pool.close().await;
    Ok(())
}

async fn assert_sqlite_registration_request_schema(
    pool: &sqlx::SqlitePool,
) -> Result<(), Box<dyn std::error::Error>> {
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('user_registration_requests')")
            .fetch_all(pool)
            .await?;
    assert_eq!(columns.len(), 15);
    assert!(
        !columns
            .iter()
            .any(|column| column.contains("token") || column == "password")
    );
    for column in [
        "recipient_email",
        "username_normalized",
        "display_name",
        "password_hash",
        "permission_snapshot_json",
        "status",
        "expires_at",
        "email_verified_at",
        "reviewed_at",
        "reviewed_by_user_id",
        "approved_user_id",
        "last_error_code",
        "created_at",
        "updated_at",
    ] {
        assert!(
            columns.iter().any(|existing| existing == column),
            "missing {column}"
        );
    }
    for name in [
        "ux_user_registration_requests_pending_email",
        "idx_user_registration_requests_status_created",
        "idx_user_registration_requests_expiry",
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?"
            )
            .bind(name)
            .fetch_one(pool)
            .await?,
            1
        );
    }
    sqlx::query("INSERT INTO users (id, username_normalized, display_name, password_hash) VALUES ('registration-reviewer', 'registration-reviewer', 'Reviewer', 'test-hash')").execute(pool).await?;
    let insert = "INSERT INTO user_registration_requests (id, recipient_email, username_normalized, display_name, password_hash, permission_snapshot_json, expires_at, created_at, updated_at) VALUES (?, 'registration@example.test', 'registration-user', 'Registration User', '$argon2id$test-hash', '{}', 20, 10, 10)";
    sqlx::query(insert)
        .bind("registration-main")
        .execute(pool)
        .await?;
    assert!(
        sqlx::query(insert)
            .bind("registration-duplicate")
            .execute(pool)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM user_registration_requests WHERE id = 'registration-main'"
        )
        .fetch_one(pool)
        .await?,
        "PENDING_EMAIL"
    );
    for (query, value) in [
        (
            "UPDATE user_registration_requests SET id = ? WHERE id = 'registration-main'",
            "x".repeat(129),
        ),
        (
            "UPDATE user_registration_requests SET username_normalized = ? WHERE id = 'registration-main'",
            " ".to_owned(),
        ),
        (
            "UPDATE user_registration_requests SET password_hash = ? WHERE id = 'registration-main'",
            "$ARGON2ID$test-hash".to_owned(),
        ),
        (
            "UPDATE user_registration_requests SET permission_snapshot_json = ? WHERE id = 'registration-main'",
            format!(r#"{{"x":"{}"}}"#, "界".repeat(12_000)),
        ),
        (
            "UPDATE user_registration_requests SET recipient_email = ? WHERE id = 'registration-main'",
            "x".repeat(255),
        ),
        (
            "UPDATE user_registration_requests SET recipient_email = ? WHERE id = 'registration-main'",
            "UPPER@example.test".to_owned(),
        ),
        (
            "UPDATE user_registration_requests SET username_normalized = ? WHERE id = 'registration-main'",
            "x".repeat(129),
        ),
        (
            "UPDATE user_registration_requests SET display_name = ? WHERE id = 'registration-main'",
            "x".repeat(129),
        ),
        (
            "UPDATE user_registration_requests SET password_hash = ? WHERE id = 'registration-main'",
            "plaintext".to_owned(),
        ),
        (
            "UPDATE user_registration_requests SET password_hash = ? WHERE id = 'registration-main'",
            format!("$argon2id${}", "x".repeat(2048)),
        ),
        (
            "UPDATE user_registration_requests SET permission_snapshot_json = ? WHERE id = 'registration-main'",
            "invalid".to_owned(),
        ),
        (
            "UPDATE user_registration_requests SET permission_snapshot_json = ? WHERE id = 'registration-main'",
            "[]".to_owned(),
        ),
        (
            "UPDATE user_registration_requests SET permission_snapshot_json = ? WHERE id = 'registration-main'",
            format!(r#"{{"x":"{}"}}"#, "x".repeat(32768)),
        ),
        (
            "UPDATE user_registration_requests SET status = ? WHERE id = 'registration-main'",
            "ACTIVE".to_owned(),
        ),
        (
            "UPDATE user_registration_requests SET last_error_code = ? WHERE id = 'registration-main'",
            "arbitrary private error".to_owned(),
        ),
    ] {
        assert!(
            sqlx::query(query).bind(&value).execute(pool).await.is_err(),
            "accepted invalid update: {query}"
        );
    }
    for query in [
        "UPDATE user_registration_requests SET password_hash = NULL WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET expires_at = 10 WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET updated_at = 9 WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET email_verified_at = 9 WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET status = 'PENDING_APPROVAL' WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET status = 'APPROVED', reviewed_at = 12, password_hash = NULL WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET reviewed_by_user_id = 'unknown' WHERE id = 'registration-main'",
    ] {
        assert!(
            sqlx::query(query).execute(pool).await.is_err(),
            "accepted invalid transition: {query}"
        );
    }
    sqlx::query("UPDATE user_registration_requests SET status = 'PENDING_APPROVAL', email_verified_at = 11, updated_at = 11 WHERE id = 'registration-main'").execute(pool).await?;
    assert!(
        sqlx::query(insert)
            .bind("registration-approval-duplicate")
            .execute(pool)
            .await
            .is_err()
    );
    sqlx::query("UPDATE user_registration_requests SET status = 'REJECTED', reviewed_at = 12, updated_at = 12, reviewed_by_user_id = 'registration-reviewer', password_hash = NULL WHERE id = 'registration-main'").execute(pool).await?;
    sqlx::query(insert)
        .bind("registration-next")
        .execute(pool)
        .await?;
    sqlx::query("UPDATE user_registration_requests SET status = 'APPROVED', email_verified_at = 11, reviewed_at = 12, updated_at = 12, reviewed_by_user_id = 'registration-reviewer', approved_user_id = 'registration-reviewer', password_hash = NULL WHERE id = 'registration-next'").execute(pool).await?;
    assert!(sqlx::query("UPDATE user_registration_requests SET approved_user_id = 'unknown' WHERE id = 'registration-next'").execute(pool).await.is_err());
    sqlx::query(insert)
        .bind("registration-expired")
        .execute(pool)
        .await?;
    sqlx::query("UPDATE user_registration_requests SET status = 'EXPIRED', password_hash = NULL, updated_at = 21 WHERE id = 'registration-expired'").execute(pool).await?;
    sqlx::query("DELETE FROM users WHERE id = 'registration-reviewer'")
        .execute(pool)
        .await?;
    let (reviewer, approved, password): (Option<String>, Option<String>, Option<String>) = sqlx::query_as("SELECT reviewed_by_user_id, approved_user_id, password_hash FROM user_registration_requests WHERE id = 'registration-next'").fetch_one(pool).await?;
    assert_eq!((reviewer, approved, password), (None, None, None));
    Ok(())
}
