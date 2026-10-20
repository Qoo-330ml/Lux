use super::*;
use crate::domain::content_restrictions::ContentRestrictionIndexProjection;
use crate::storage::{
    ApplyUserPermissionPreset, CONTENT_RESTRICTION_INDEX_BATCH_LIMIT,
    MediaContentRestrictionIndexCandidate, NewEmbyDirectPlaybackTicket, NewMailChallenge,
    NewMailOutboxMessage, NewStoredUserPermissions, NewUserAccessBlacklistRule,
    NewUserActivityEvent, NewUserContentRestrictions, NewUserDeviceObservation, NewUserInvitation,
    PlaybackAdmissionDecision, PlaybackAdmissionDenialReason, StoredUserActivityEvent,
    UserAccessBlacklistRuleMatchQuery, UserAccessBlacklistRulePageQuery,
    UserAccessBlacklistRuleScope, UserActivityEventQuery, UserContentRestrictionsCasResult,
    UserDevicePageQuery, UserInvitationCreateResult, UserInvitationStatus,
    UserPermissionCopyGroups, UserPermissionCopyResult, UserPlaybackPolicy,
};

#[tokio::test]
async fn web_session_device_association_is_optional_and_only_read_from_active_sessions()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{application::setup::SetupService, config::Config};

    let temp = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp.path().join("config"),
    })
    .await?;
    let user = SetupService::new(database.clone())?
        .complete("Session Device", "Session Device", "password")
        .await?;
    let user_id = user.id.to_string();
    let token_hash = [1_u8; 32];
    database
        .create_web_session_with_device(
            "device-session",
            &user_id,
            &token_hash,
            &[2_u8; 32],
            Some("browser-device-id"),
            3600,
        )
        .await?;
    let stored = database
        .find_web_session(&token_hash)
        .await?
        .ok_or("active device session was not found")?;
    assert_eq!(stored.device_id.as_deref(), Some("browser-device-id"));

    let legacy_token_hash = [3_u8; 32];
    database
        .create_web_session(
            "legacy-session",
            &user_id,
            &legacy_token_hash,
            &[4_u8; 32],
            3600,
        )
        .await?;
    let legacy = database
        .find_web_session(&legacy_token_hash)
        .await?
        .ok_or("active legacy session was not found")?;
    assert_eq!(legacy.device_id, None);

    let expired_token_hash = [5_u8; 32];
    database
        .create_web_session_with_device(
            "expired-session",
            &user_id,
            &expired_token_hash,
            &[6_u8; 32],
            Some("expired-device"),
            -1,
        )
        .await?;
    assert!(
        database
            .find_web_session(&expired_token_hash)
            .await?
            .is_none()
    );
    database.revoke_web_session(&token_hash).await?;
    assert!(database.find_web_session(&token_hash).await?.is_none());
    assert!(database.find_web_session(&[7_u8; 32]).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn user_access_blacklist_rules_are_scoped_stable_and_bounded()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp.path().join("config"),
    })
    .await?;
    for (user_id, username) in [
        ("blacklist-user-a", "blacklist-user-a"),
        ("blacklist-user-b", "blacklist-user-b"),
        ("blacklist-creator", "blacklist-creator"),
    ] {
        sqlx::query(
            "INSERT INTO users (id, username_normalized, display_name, password_hash)
             VALUES (?, ?, ?, 'test-hash')",
        )
        .bind(user_id)
        .bind(username)
        .bind(username)
        .execute(database.pool())
        .await?;
    }

    for (id, user_id, rule_type, match_value, created_at) in [
        ("global-client", None, "CLIENT", "infuse", 200_i64),
        ("global-device", None, "DEVICE", "living-room-tv", 200_i64),
        (
            "user-a-client",
            Some("blacklist-user-a"),
            "CLIENT",
            "infuse",
            250_i64,
        ),
        (
            "user-a-ip",
            Some("blacklist-user-a"),
            "IP_CIDR",
            "192.0.2.0/24",
            150_i64,
        ),
        (
            "user-b-device",
            Some("blacklist-user-b"),
            "DEVICE",
            "living-room-tv",
            300_i64,
        ),
    ] {
        database
            .insert_user_access_blacklist_rule(NewUserAccessBlacklistRule {
                id,
                user_id,
                rule_type,
                match_value,
                is_enabled: true,
                created_by: Some("blacklist-creator"),
                created_at,
            })
            .await?;
    }

    let (global_page_1, global_total) = database
        .list_user_access_blacklist_rules_page(UserAccessBlacklistRulePageQuery {
            scope: UserAccessBlacklistRuleScope::Global,
            offset: 0,
            limit: 1,
        })
        .await?;
    assert_eq!(global_total, 2);
    assert_eq!(global_page_1[0].id, "global-client");
    let (global_page_2, _) = database
        .list_user_access_blacklist_rules_page(UserAccessBlacklistRulePageQuery {
            scope: UserAccessBlacklistRuleScope::Global,
            offset: 1,
            limit: 1,
        })
        .await?;
    assert_eq!(global_page_2[0].id, "global-device");

    let (user_a_page, user_a_total) = database
        .list_user_access_blacklist_rules_page(UserAccessBlacklistRulePageQuery {
            scope: UserAccessBlacklistRuleScope::User("blacklist-user-a"),
            offset: -1,
            limit: 0,
        })
        .await?;
    assert_eq!(user_a_total, 2);
    assert_eq!(user_a_page.len(), 1);
    assert_eq!(user_a_page[0].id, "user-a-client");
    assert_eq!(user_a_page[0].user_id.as_deref(), Some("blacklist-user-a"));
    assert!(user_a_page[0].is_enabled);

    let (user_b_page, user_b_total) = database
        .list_user_access_blacklist_rules_page(UserAccessBlacklistRulePageQuery {
            scope: UserAccessBlacklistRuleScope::User("blacklist-user-b"),
            offset: 0,
            limit: 101,
        })
        .await?;
    assert_eq!(user_b_total, 1);
    assert_eq!(user_b_page.len(), 1);
    assert_eq!(user_b_page[0].id, "user-b-device");

    assert!(
        database
            .insert_user_access_blacklist_rule(NewUserAccessBlacklistRule {
                id: "duplicate-global-client",
                user_id: None,
                rule_type: "CLIENT",
                match_value: "infuse",
                is_enabled: true,
                created_by: Some("blacklist-creator"),
                created_at: 300,
            })
            .await
            .is_err()
    );
    assert!(
        database
            .insert_user_access_blacklist_rule(NewUserAccessBlacklistRule {
                id: "duplicate-user-device",
                user_id: Some("blacklist-user-b"),
                rule_type: "DEVICE",
                match_value: "living-room-tv",
                is_enabled: true,
                created_by: Some("blacklist-creator"),
                created_at: 300,
            })
            .await
            .is_err()
    );

    assert!(
        database
            .set_user_access_blacklist_rule_enabled(
                UserAccessBlacklistRuleScope::Global,
                "global-client",
                false,
            )
            .await?
    );
    assert!(
        !database
            .set_user_access_blacklist_rule_enabled(
                UserAccessBlacklistRuleScope::User("blacklist-user-a"),
                "global-client",
                false,
            )
            .await?
    );
    let (disabled_page, _) = database
        .list_user_access_blacklist_rules_page(UserAccessBlacklistRulePageQuery {
            scope: UserAccessBlacklistRuleScope::Global,
            offset: 0,
            limit: 10,
        })
        .await?;
    let disabled = disabled_page
        .iter()
        .find(|rule| rule.id == "global-client")
        .ok_or("disabled global rule was not listed")?;
    assert!(!disabled.is_enabled);
    assert!(disabled.updated_at >= disabled.created_at);

    assert!(
        !database
            .delete_user_access_blacklist_rule(
                UserAccessBlacklistRuleScope::User("blacklist-user-a"),
                "global-client",
            )
            .await?
    );
    assert!(
        database
            .delete_user_access_blacklist_rule(
                UserAccessBlacklistRuleScope::User("blacklist-user-a"),
                "user-a-ip",
            )
            .await?
    );
    let (_, user_a_after_delete) = database
        .list_user_access_blacklist_rules_page(UserAccessBlacklistRulePageQuery {
            scope: UserAccessBlacklistRuleScope::User("blacklist-user-a"),
            offset: 0,
            limit: 100,
        })
        .await?;
    assert_eq!(user_a_after_delete, 1);
    Ok(())
}

#[tokio::test]
async fn user_access_blacklist_matching_is_scoped_enabled_and_type_specific()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp.path().join("config"),
    })
    .await?;
    for (user_id, username) in [
        ("blacklist-match-a", "blacklist-match-a"),
        ("blacklist-match-b", "blacklist-match-b"),
        ("blacklist-match-creator", "blacklist-match-creator"),
    ] {
        sqlx::query(
            "INSERT INTO users (id, username_normalized, display_name, password_hash)
             VALUES (?, ?, ?, 'test-hash')",
        )
        .bind(user_id)
        .bind(username)
        .bind(username)
        .execute(database.pool())
        .await?;
    }

    for (id, user_id, rule_type, match_value, is_enabled) in [
        ("match-global-device", None, "DEVICE", "shared-tv", true),
        ("match-global-client", None, "CLIENT", "lux web", true),
        (
            "match-user-a-device",
            Some("blacklist-match-a"),
            "DEVICE",
            "a-phone",
            true,
        ),
        (
            "match-user-a-client",
            Some("blacklist-match-a"),
            "CLIENT",
            "plex",
            true,
        ),
        (
            "match-user-b-device",
            Some("blacklist-match-b"),
            "DEVICE",
            "b-phone",
            true,
        ),
        (
            "match-disabled-device",
            None,
            "DEVICE",
            "disabled-tv",
            false,
        ),
        (
            "match-ip-same-value",
            None,
            "IP_CIDR",
            "203.0.113.5/32",
            true,
        ),
    ] {
        database
            .insert_user_access_blacklist_rule(NewUserAccessBlacklistRule {
                id,
                user_id,
                rule_type,
                match_value,
                is_enabled,
                created_by: Some("blacklist-match-creator"),
                created_at: 100,
            })
            .await?;
    }

    for (user_id, device_id, client_name) in [
        ("blacklist-match-a", Some("shared-tv"), None),
        ("blacklist-match-b", Some("shared-tv"), None),
        ("blacklist-match-a", Some("a-phone"), None),
        ("blacklist-match-a", None, Some("lux web")),
        ("blacklist-match-a", None, Some("plex")),
    ] {
        assert!(
            database
                .user_access_blacklist_matches(UserAccessBlacklistRuleMatchQuery {
                    user_id,
                    device_id,
                    client_name,
                })
                .await?
        );
    }

    for (user_id, device_id, client_name) in [
        ("blacklist-match-b", Some("a-phone"), None),
        ("blacklist-match-a", Some("a-phone-extra"), None),
        ("blacklist-match-a", Some("disabled-tv"), None),
        ("blacklist-match-a", None, Some("203.0.113.5/32")),
        ("blacklist-match-a", None, None),
    ] {
        assert!(
            !database
                .user_access_blacklist_matches(UserAccessBlacklistRuleMatchQuery {
                    user_id,
                    device_id,
                    client_name,
                })
                .await?
        );
    }
    Ok(())
}

#[tokio::test]
async fn user_access_ip_blacklist_rules_are_bounded_enabled_and_scoped()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp.path().join("config"),
    })
    .await?;
    for (user_id, username) in [
        ("ip-rules-a", "ip-rules-a"),
        ("ip-rules-b", "ip-rules-b"),
        ("ip-rules-creator", "ip-rules-creator"),
    ] {
        sqlx::query(
            "INSERT INTO users (id, username_normalized, display_name, password_hash)
             VALUES (?, ?, ?, 'test-hash')",
        )
        .bind(user_id)
        .bind(username)
        .bind(username)
        .execute(database.pool())
        .await?;
    }

    for (id, user_id, rule_type, value, enabled) in [
        ("ip-global-a", None, "IP_CIDR", "192.0.2.0/24", true),
        (
            "ip-global-disabled",
            None,
            "IP_CIDR",
            "198.51.100.0/24",
            false,
        ),
        (
            "ip-user-a",
            Some("ip-rules-a"),
            "IP_CIDR",
            "2001:db8::/32",
            true,
        ),
        (
            "ip-user-b",
            Some("ip-rules-b"),
            "IP_CIDR",
            "203.0.113.0/24",
            true,
        ),
        ("ip-not-ip", None, "CLIENT", "192.0.2.0/24", true),
    ] {
        database
            .insert_user_access_blacklist_rule(NewUserAccessBlacklistRule {
                id,
                user_id,
                rule_type,
                match_value: value,
                is_enabled: enabled,
                created_by: Some("ip-rules-creator"),
                created_at: 100,
            })
            .await?;
    }

    let rules = database
        .list_user_access_ip_blacklist_rule_values(UserAccessBlacklistIpRuleQuery {
            user_id: "ip-rules-a",
            limit: 10,
        })
        .await?;
    assert_eq!(rules, ["192.0.2.0/24", "2001:db8::/32"]);

    let limited = database
        .list_user_access_ip_blacklist_rule_values(UserAccessBlacklistIpRuleQuery {
            user_id: "ip-rules-a",
            limit: 1,
        })
        .await?;
    assert_eq!(limited.len(), 1);

    let other_user = database
        .list_user_access_ip_blacklist_rule_values(UserAccessBlacklistIpRuleQuery {
            user_id: "ip-rules-b",
            limit: 10,
        })
        .await?;
    assert_eq!(other_user, ["192.0.2.0/24", "203.0.113.0/24"]);

    for index in 0..205 {
        let id = format!("ip-overflow-{index}");
        let match_value = format!("203.0.113.{index}/32");
        database
            .insert_user_access_blacklist_rule(NewUserAccessBlacklistRule {
                id: &id,
                user_id: None,
                rule_type: "IP_CIDR",
                match_value: &match_value,
                is_enabled: true,
                created_by: Some("ip-rules-creator"),
                created_at: 101,
            })
            .await?;
    }
    let bounded = database
        .list_user_access_ip_blacklist_rule_values(UserAccessBlacklistIpRuleQuery {
            user_id: "ip-rules-a",
            limit: 500,
        })
        .await?;
    assert_eq!(bounded.len(), 201);
    Ok(())
}

#[tokio::test]
async fn scan_job_retry_diagnostics_cover_history_and_filter_before_pagination()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp.path().join("config"),
    })
    .await?;
    assert_scan_job_retry_diagnostics(&database, temp.path()).await
}

async fn assert_scan_job_retry_diagnostics(
    database: &Database,
    temp: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Diagnostics", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let cases = [
        ("normal", "RECONCILE_LIBRARY", "COMPLETED", "IDLE", None),
        ("done", "RECONCILE_LIBRARY", "COMPLETED", "IDLE", None),
        ("pending", "RECONCILE_LIBRARY", "COMPLETED", "IDLE", None),
        ("failed", "RECONCILE_LIBRARY", "COMPLETED", "IDLE", None),
        ("unready-v2", "RECONCILE_LIBRARY", "COMPLETED", "IDLE", None),
        ("unready-v3", "RECONCILE_LIBRARY", "COMPLETED", "IDLE", None),
        (
            "marker",
            "RECONCILE_LIBRARY",
            "COMPLETED",
            "IDLE",
            Some("POSTPROCESSING_FAILED"),
        ),
        (
            "processing",
            "RECONCILE_LIBRARY",
            "COMPLETED",
            "POSTPROCESSING",
            None,
        ),
        ("incremental", "INCREMENTAL_SCAN", "COMPLETED", "IDLE", None),
        ("active", "RECONCILE_LIBRARY", "RUNNING", "DISCOVERY", None),
    ];
    for (index, (id, kind, status, phase, error)) in cases.iter().enumerate() {
        database.query("INSERT INTO scan_jobs (id, library_id, job_type, status, scan_phase, error, generation, created_at) VALUES (?, ?, ?, ?, ?, ?, 'generation', ?)")
            .bind(id).bind(&library_id).bind(kind).bind(status).bind(phase).bind(error)
            .bind(index as i64).execute(database.pool()).await?;
    }
    for (id, state) in [
        ("done", "DONE"),
        ("pending", "PENDING"),
        ("failed", "FAILED"),
        ("processing", "PENDING"),
        ("incremental", "PENDING"),
    ] {
        database.query("INSERT INTO scan_job_targets (job_id, target_type, target_id, item_id, change_kind, metadata_state) VALUES (?, 'ITEM', 'item', 'item', 'NEW', ?)")
            .bind(id).bind(state).execute(database.pool()).await?;
    }
    for (id, workflow) in [("unready-v2", 2_i64), ("unready-v3", 3_i64)] {
        database.query("INSERT INTO scan_manifests (id, job_id, library_id, state, workflow_version, discovery_format_version, postprocessing_targets_ready) VALUES (?, ?, ?, 'POSTPROCESSING', ?, 3, 0)")
            .bind(id)
            .bind(id).bind(&library_id).bind(workflow).execute(database.pool()).await?;
    }
    let ids: Vec<String> = cases.iter().map(|case| case.0.to_owned()).collect();
    database.reset_query_count();
    let states = database.scan_job_retry_states(&ids).await?;
    assert_eq!(
        database.query_count(),
        1,
        "one batched query, not one per task"
    );
    for id in ["pending", "failed", "unready-v2", "unready-v3", "marker"] {
        assert!(states[id].has_postprocessing_issues, "{id}");
    }
    for id in ["normal", "done", "processing", "incremental", "active"] {
        assert!(!states[id].has_postprocessing_issues, "{id}");
    }
    assert!(states["pending"].has_unfinished_targets);
    assert!(!states["done"].has_unfinished_targets);
    assert!(states["unready-v3"].targets_need_materialization);
    assert!(states["normal"].has_active_job);
    assert!(!states["incremental"].has_active_job);
    assert!(!states["pending"].has_legacy_work);
    let first = database
        .list_admin_scan_jobs(None, Some(true), 0, 2)
        .await?;
    assert_eq!(
        first.iter().map(|job| job.id.as_str()).collect::<Vec<_>>(),
        ["marker", "unready-v3"]
    );
    let second = database
        .list_admin_scan_jobs(Some("COMPLETED"), Some(true), 2, 2)
        .await?;
    assert_eq!(
        second.iter().map(|job| job.id.as_str()).collect::<Vec<_>>(),
        ["unready-v2", "failed"]
    );
    let clean = database
        .list_admin_scan_jobs(None, Some(false), 0, 20)
        .await?;
    assert_eq!(clean.len(), 5);
    assert!(database.scan_job_retry_states(&[]).await?.is_empty());
    let root_path = temp.join("media");
    tokio::fs::create_dir_all(&root_path).await?;
    let root = libraries
        .add_root(library.id, root_path.to_str().ok_or("root path")?)
        .await?
        .root;
    database.query("INSERT INTO reconciliation_scan_entries (job_id, library_root_id, relative_path, entry_type) VALUES ('pending', ?, '', 'DIRECTORY')")
        .bind(root.id.to_string()).execute(database.pool()).await?;
    let ids = vec!["pending".to_owned(); SCAN_DML_CHUNK_SIZE + 1];
    database.reset_query_count();
    let states = database.scan_job_retry_states(&ids).await?;
    assert_eq!(database.query_count(), 2);
    assert!(states["pending"].has_legacy_work);
    Ok(())
}
use crate::{
    application::{
        access::MediaAccessService,
        candidates::MetadataCandidateService,
        catalog::CatalogService,
        libraries::LibraryService,
        metadata::MetadataEnricher,
        scanner::{LibraryScanner, ScanJobService},
        setup::SetupService,
    },
    config::{Config, DatabaseBackend, DatabaseConfiguration, PostgresConnection},
    library::LibraryKind,
    storage::{
        ItemImageBatchInsert, ItemImageInsert, LocalNfoDefaultsRepair, MetadataAutoMatchPolicy,
        MetadataCapabilityResult, MetadataImageUnavailable, NewFilesystemEntry,
        NewItemMetadataCompletenessCheck, NewItemMetadataCompletenessResult, NewMediaChapterMarker,
        NewMetadataCandidate, NewNotificationDestination, NewNotificationEvent,
    },
};

#[tokio::test]
async fn sqlite_media_search_rowid_map_tracks_fts_rows_and_aliases() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    for item_id in ["fts-first", "fts-second"] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(library.id.to_string())
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    let original_rowid: i64 =
        sqlx::query_scalar("SELECT rowid FROM media_search WHERE item_id = 'fts-first'")
            .fetch_one(database.pool())
            .await
            .expect("initial FTS row");
    let mapped_rowid: i64 =
        sqlx::query_scalar("SELECT fts_rowid FROM media_search_map WHERE item_id = 'fts-first'")
            .fetch_one(database.pool())
            .await
            .expect("indexed FTS row mapping");
    assert_eq!(mapped_rowid, original_rowid);
    let unique_index_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_index_list('media_search_map') WHERE \"unique\" = 1",
    )
    .fetch_one(database.pool())
    .await
    .expect("mapping uniqueness index");
    assert!(unique_index_count > 0);

    sqlx::query("UPDATE media_items SET overview = 'new overview' WHERE id = 'fts-first'")
        .execute(database.pool())
        .await
        .expect("non-search metadata update");
    sqlx::query("UPDATE media_items SET title = 'renamed media' WHERE id = 'fts-first'")
        .execute(database.pool())
        .await
        .expect("search metadata update");
    let renamed_rowid: i64 =
        sqlx::query_scalar("SELECT rowid FROM media_search WHERE item_id = 'fts-first'")
            .fetch_one(database.pool())
            .await
            .expect("renamed FTS row");
    assert_eq!(renamed_rowid, original_rowid);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM media_search WHERE item_id = 'fts-first' AND title MATCH 'renamed'",
        )
        .fetch_one(database.pool())
        .await
        .expect("updated FTS content"),
        1
    );

    sqlx::query(
        "INSERT INTO item_aliases (id, item_id, alias, alias_normalized)
         VALUES ('fts-alias', 'fts-first', 'first alias', 'first alias')",
    )
    .execute(database.pool())
    .await
    .expect("insert alias");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM media_search WHERE item_id = 'fts-first' AND aliases MATCH 'alias'",
        )
        .fetch_one(database.pool())
        .await
        .expect("inserted alias search"),
        1
    );
    sqlx::query(
        "UPDATE item_aliases SET alias = 'second alias', alias_normalized = 'second alias'
         WHERE id = 'fts-alias'",
    )
    .execute(database.pool())
    .await
    .expect("update alias");
    sqlx::query("UPDATE item_aliases SET item_id = 'fts-second' WHERE id = 'fts-alias'")
        .execute(database.pool())
        .await
        .expect("move alias to another item");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM media_search WHERE item_id = 'fts-first' AND aliases MATCH 'alias'",
        )
        .fetch_one(database.pool())
        .await
        .expect("old item's alias search"),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM media_search WHERE item_id = 'fts-second' AND aliases MATCH 'alias'",
        )
        .fetch_one(database.pool())
        .await
        .expect("new item's alias search"),
        1
    );
    sqlx::query("DELETE FROM item_aliases WHERE id = 'fts-alias'")
        .execute(database.pool())
        .await
        .expect("delete alias");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM media_search WHERE item_id = 'fts-second' AND aliases MATCH 'alias'",
        )
        .fetch_one(database.pool())
        .await
        .expect("deleted alias search"),
        0
    );

    sqlx::query("DELETE FROM media_items WHERE id = 'fts-first'")
        .execute(database.pool())
        .await
        .expect("delete media item");
    let remaining_mappings: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_search_map WHERE item_id = 'fts-first'")
            .fetch_one(database.pool())
            .await
            .expect("deleted row mapping");
    let remaining_search_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_search WHERE item_id = 'fts-first'")
            .fetch_one(database.pool())
            .await
            .expect("deleted FTS row");
    assert_eq!(remaining_mappings, 0);
    assert_eq!(remaining_search_rows, 0);
    database.close().await;
}

#[tokio::test]
async fn incremental_strm_count_uses_exact_file_paths_and_preserves_directory_scopes() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("STRM count", LibraryKind::Movie, false)
        .await
        .expect("library");
    let media_root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&media_root_path)
        .await
        .expect("media root");
    let root = libraries
        .add_root(
            library.id,
            media_root_path.to_str().expect("UTF-8 media root"),
        )
        .await
        .expect("library root")
        .root;
    let library_id = library.id.to_string();
    let root_id = root.id.to_string();

    for (entry_id, path) in [
        ("strm-count-nfo-entry", "Movie/Movie.nfo"),
        ("strm-count-video-entry", "Movie/Movie.strm"),
    ] {
        database
            .insert_filesystem_entry(NewFilesystemEntry {
                id: entry_id,
                library_root_id: &root_id,
                relative_path: path,
                entry_kind: "FILE",
                size: 1,
                modified_at: 1,
                device: None,
                inode: None,
                fingerprint: b"fingerprint",
                last_seen_generation: "strm-count-generation",
            })
            .await
            .expect("filesystem entry");
    }
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('strm-count-item', ?, 'MOVIE', 'STRM Movie', 'strm-movie', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("media item");
    sqlx::query(
        "INSERT INTO media_sources (id, item_id, source_kind, filesystem_entry_id)
         VALUES ('strm-count-source', 'strm-count-item', 'STRM_URL', 'strm-count-video-entry')",
    )
    .execute(database.pool())
    .await
    .expect("STRM media source");

    database
        .create_scan_job(
            "strm-count-nfo",
            &library_id,
            "INCREMENTAL_SCAN",
            "nfo",
            0,
            false,
        )
        .await
        .expect("NFO scan job");
    database
        .enqueue_incremental_scan_path("strm-count-nfo", &root_id, "Movie/Movie.nfo", "MODIFY")
        .await
        .expect("enqueue NFO scan path");
    sqlx::query("UPDATE scan_job_paths SET processed_at = 1 WHERE job_id = 'strm-count-nfo'")
        .execute(database.pool())
        .await
        .expect("mark NFO scan path processed");
    database.reset_query_count();
    assert_eq!(
        database
            .count_strm_media_sources_for_incremental_scan("strm-count-nfo")
            .await
            .expect("count NFO scan STRM sources"),
        0
    );
    assert_eq!(
        database.query_count(),
        2,
        "file-only scans use the narrow path"
    );
    sqlx::query("UPDATE scan_jobs SET status = 'COMPLETED' WHERE id = 'strm-count-nfo'")
        .execute(database.pool())
        .await
        .expect("complete NFO scan job");

    database
        .create_scan_job(
            "strm-count-strm",
            &library_id,
            "INCREMENTAL_SCAN",
            "strm",
            0,
            false,
        )
        .await
        .expect("STRM scan job");
    database
        .enqueue_incremental_scan_path("strm-count-strm", &root_id, "Movie/Movie.strm", "MODIFY")
        .await
        .expect("enqueue STRM scan path");
    sqlx::query("UPDATE scan_job_paths SET processed_at = 1 WHERE job_id = 'strm-count-strm'")
        .execute(database.pool())
        .await
        .expect("mark STRM scan path processed");
    database.reset_query_count();
    assert_eq!(
        database
            .count_strm_media_sources_for_incremental_scan("strm-count-strm")
            .await
            .expect("count STRM scan sources"),
        1
    );
    assert_eq!(
        database.query_count(),
        2,
        "STRM file scans use the narrow path"
    );
    sqlx::query("UPDATE scan_jobs SET status = 'COMPLETED' WHERE id = 'strm-count-strm'")
        .execute(database.pool())
        .await
        .expect("complete STRM scan job");

    database
        .create_scan_job(
            "strm-count-directory",
            &library_id,
            "INCREMENTAL_SCAN",
            "directory",
            0,
            false,
        )
        .await
        .expect("directory scan job");
    database
        .enqueue_incremental_scan_path("strm-count-directory", &root_id, ".", "MODIFY")
        .await
        .expect("enqueue directory scan path");
    sqlx::query("UPDATE scan_job_paths SET processed_at = 1 WHERE job_id = 'strm-count-directory'")
        .execute(database.pool())
        .await
        .expect("mark directory scan path processed");
    database.reset_query_count();
    assert_eq!(
        database
            .count_strm_media_sources_for_incremental_scan("strm-count-directory")
            .await
            .expect("count directory STRM sources"),
        1
    );
    assert_eq!(
        database.query_count(),
        2,
        "directory scans retain recursive counting"
    );

    database.close().await;
}

async fn refresh_recommendation_stats(database: &Database) {
    sqlx::query(
        "UPDATE recommendation_stats_state
         SET batch_key = batch_key - 1
         WHERE id = 1",
    )
    .execute(database.pool())
    .await
    .expect("invalidate recommendation stats batch");
    assert!(
        database
            .refresh_recommendation_stats_if_needed()
            .await
            .expect("refresh recommendation stats")
    );
}

#[tokio::test]
async fn scan_job_metadata_target_selection_uses_one_query_without_a_local_source()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await?;
    database
        .query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('metadata-target-without-source', ?, 'MOVIE', 'Movie', 'movie', 'PENDING')",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_job_targets (
                 job_id, target_type, target_id, item_id, change_kind, metadata_state
             ) VALUES (?, 'ITEM', 'metadata-target-without-source',
                       'metadata-target-without-source', 'NEW', 'PENDING')",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;

    database.reset_query_count();
    let report = MetadataEnricher::new(database.clone())
        .enrich_scan_job_targets(&job.id, 32)
        .await?;

    assert_eq!(report.items_processed, 0);
    assert_eq!(
        database.query_count(),
        1,
        "pending state and the unavailable-source result should share one bounded page query"
    );
    Ok(())
}

#[tokio::test]
async fn retrying_failed_scan_probe_keeps_unconfigured_strm_status()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&media_root).await?;
    let root = LibraryService::new(database.clone())
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-utf8 media root")?,
        )
        .await?
        .root;
    database
        .query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 last_seen_generation, is_missing
             ) VALUES ('unselected-strm-entry', ?, 'Unselected.strm', 'FILE', 1, 1,
                       'generation', 0)",
        )
        .bind(root.id.to_string())
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('unselected-strm-item', ?, 'MOVIE', 'Unselected', 'unselected',
                       'LOCAL_CONFIRMED')",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO media_sources (
                 id, item_id, source_kind, filesystem_entry_id, external_url, probe_status
             ) VALUES ('unselected-strm-source', 'unselected-strm-item', 'STRM_URL',
                       'unselected-strm-entry', 'https://example.invalid/movie',
                       'NOT_CONFIGURED')",
        )
        .execute(database.pool())
        .await?;
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await?;
    database
        .query(
            "INSERT INTO scan_job_targets (
                 job_id, target_type, target_id, source_id, item_id, change_kind,
                 probe_state, updated_at
             ) VALUES (?, 'SOURCE', 'unselected-strm-source', 'unselected-strm-source',
                       'unselected-strm-item', 'NEW', 'FAILED', 1)",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;

    database.retry_failed_scan_job_targets(&job.id).await?;

    let probe_status: String = database
        .query_scalar("SELECT probe_status FROM media_sources WHERE id = 'unselected-strm-source'")
        .fetch_one(database.pool())
        .await?;
    let probe_state: String = database
        .query_scalar(
            "SELECT probe_state FROM scan_job_targets
             WHERE job_id = ? AND target_id = 'unselected-strm-source'",
        )
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(probe_status, "NOT_CONFIGURED");
    assert_eq!(probe_state, "PENDING");
    Ok(())
}

#[tokio::test]
async fn local_metadata_completion_wait_state_uses_one_query_and_stops_for_cancelled_jobs()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await?;
    database
        .query(
            "INSERT INTO scan_job_targets (
                 job_id, target_type, target_id, item_id, change_kind, metadata_state
             ) VALUES (?, 'ITEM', 'completion-wait-target', 'completion-wait-target',
                       'NEW', 'PENDING')",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;

    database.reset_query_count();
    assert_eq!(
        database
            .local_metadata_completion_wait_state(&job.id)
            .await?,
        (true, false)
    );
    assert_eq!(
        database.query_count(),
        1,
        "pending state and job cancellation must be observed by one storage query"
    );

    database.request_scan_job_cancel(&job.id).await?;
    assert_eq!(
        database
            .local_metadata_completion_wait_state(&job.id)
            .await?,
        (true, true)
    );
    assert_eq!(
        database
            .local_metadata_completion_wait_state("missing-completion-wait-job")
            .await?,
        (false, false)
    );

    database
        .query(
            "UPDATE scan_job_targets SET metadata_state = 'DONE'
             WHERE job_id = ? AND target_id = 'completion-wait-target'",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    assert_eq!(
        database
            .local_metadata_completion_wait_state(&job.id)
            .await?,
        (false, false)
    );
    Ok(())
}

#[tokio::test]
async fn local_nfo_metadata_batch_rolls_back_all_updates_on_a_batch_error()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    for item_id in ["nfo-batch-first", "nfo-batch-second"] {
        database
            .query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', 'Old title', 'old title', 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .execute(database.pool())
            .await?;
    }
    sqlx::query(
        "CREATE TRIGGER reject_second_local_nfo_update
         BEFORE UPDATE OF title ON media_items
         WHEN OLD.id = 'nfo-batch-second'
         BEGIN SELECT RAISE(ABORT, 'injected metadata failure'); END",
    )
    .execute(database.pool())
    .await?;

    let first_fingerprint = [1_u8; 32];
    let second_fingerprint = [2_u8; 32];
    let updates = [
        MediaMetadataUpdate {
            item_id: "nfo-batch-first",
            title: "First title",
            original_title: None,
            overview: None,
            production_year: None,
            premiere_date: None,
            rating: None,
            rating_source: None,
            provider_ids_json: None,
            metadata_fingerprint: &first_fingerprint,
            provenance_json: "{}",
            locked_fields_json: "{}",
        },
        MediaMetadataUpdate {
            item_id: "nfo-batch-second",
            title: "Second title",
            original_title: None,
            overview: None,
            production_year: None,
            premiere_date: None,
            rating: None,
            rating_source: None,
            provider_ids_json: None,
            metadata_fingerprint: &second_fingerprint,
            provenance_json: "{}",
            locked_fields_json: "{}",
        },
    ];

    assert!(
        database
            .commit_local_nfo_state_batch(&updates, &[])
            .await
            .is_err()
    );
    let titles: Vec<String> = sqlx::query_scalar(
        "SELECT title FROM media_items
         WHERE id IN ('nfo-batch-first', 'nfo-batch-second') ORDER BY id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(titles, ["Old title", "Old title"]);
    Ok(())
}

#[tokio::test]
async fn local_nfo_state_batch_rolls_back_metadata_and_fills_only_missing_defaults()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    database
        .query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status,
                 provider_ids_json
             ) VALUES ('nfo-state-metadata', ?, 'MOVIE', 'Old title', 'old title',
                       'LOCAL_CONFIRMED', '{}'),
                      ('nfo-state-defaults', ?, 'MOVIE', 'Keep title', 'keep title',
                       'LOCAL_CONFIRMED', '{\"tmdb\":\"99\"}')",
        )
        .bind(library.id.to_string())
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
    sqlx::query(
        "CREATE TRIGGER reject_local_nfo_default_repair
         BEFORE UPDATE OF premiere_date ON media_items
         WHEN OLD.id = 'nfo-state-defaults'
         BEGIN SELECT RAISE(ABORT, 'injected defaults failure'); END",
    )
    .execute(database.pool())
    .await?;

    let fingerprint = [7_u8; 32];
    let updates = [MediaMetadataUpdate {
        item_id: "nfo-state-metadata",
        title: "Updated title",
        original_title: None,
        overview: None,
        production_year: None,
        premiere_date: None,
        rating: None,
        rating_source: None,
        provider_ids_json: None,
        metadata_fingerprint: &fingerprint,
        provenance_json: "{}",
        locked_fields_json: "{}",
    }];
    let provider_ids = std::collections::BTreeMap::from([
        ("tmdb".to_owned(), "42".to_owned()),
        ("imdb".to_owned(), "tt123".to_owned()),
    ]);
    let repairs = [LocalNfoDefaultsRepair {
        item_id: "nfo-state-defaults",
        provider_ids: &provider_ids,
        premiere_date: Some("2026-01-02"),
    }];

    assert!(
        database
            .commit_local_nfo_state_batch(&updates, &repairs)
            .await
            .is_err()
    );
    let unchanged: (String, Option<String>) = sqlx::query_as(
        "SELECT metadata.title, defaults.premiere_date
         FROM media_items metadata
         CROSS JOIN media_items defaults
         WHERE metadata.id = 'nfo-state-metadata'
           AND defaults.id = 'nfo-state-defaults'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(unchanged, ("Old title".to_owned(), None));

    sqlx::query("DROP TRIGGER reject_local_nfo_default_repair")
        .execute(database.pool())
        .await?;
    database
        .commit_local_nfo_state_batch(&updates, &repairs)
        .await?;
    let title: String =
        sqlx::query_scalar("SELECT title FROM media_items WHERE id = 'nfo-state-metadata'")
            .fetch_one(database.pool())
            .await?;
    let repaired: (String, Option<String>) = sqlx::query_as(
        "SELECT provider_ids_json, premiere_date FROM media_items
         WHERE id = 'nfo-state-defaults'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(title, "Updated title");
    assert_eq!(
        repaired,
        (
            r#"{"imdb":"tt123","tmdb":"99"}"#.to_owned(),
            Some("2026-01-02".to_owned())
        )
    );

    sqlx::query(
        "CREATE TRIGGER reject_unchanged_local_nfo_default_repair
         BEFORE UPDATE OF premiere_date ON media_items
         WHEN OLD.id = 'nfo-state-defaults'
         BEGIN SELECT RAISE(ABORT, 'unchanged defaults must not update'); END",
    )
    .execute(database.pool())
    .await?;
    database.commit_local_nfo_state_batch(&[], &repairs).await?;
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn scan_local_metadata_sources_batch_nested_series_directories() {
    const DIRECTORY_COUNT: usize = 205;
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Local metadata sources", LibraryKind::Series, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    let root = LibraryService::new(database.clone())
        .add_root(library.id, root_path.to_str().expect("media root"))
        .await
        .expect("root");
    let root_id = root.root.id.to_string();
    let library_id = library.id.to_string();
    let mut poster_entry_ids = Vec::with_capacity(DIRECTORY_COUNT);
    for index in 0..DIRECTORY_COUNT {
        let series_id = format!("local-source-series-{index:03}");
        let season_id = format!("local-source-season-{index:03}");
        let episode_id = format!("local-source-episode-{index:03}");
        let poster_entry_id = format!("local-source-poster-entry-{index:03}");
        let episode_entry_id = format!("local-source-episode-entry-{index:03}");
        let directory = format!("Series {index:03}");
        sqlx::query(
            "INSERT INTO media_items
                 (id, library_id, item_type, title, sort_title, identification_status)
             VALUES (?, ?, 'SERIES', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(&series_id)
        .bind(&library_id)
        .bind(&series_id)
        .bind(&series_id)
        .execute(database.pool())
        .await
        .expect("series");
        sqlx::query(
            "INSERT INTO media_items
                 (id, library_id, item_type, parent_id, series_id, season_number,
                  title, sort_title, identification_status)
             VALUES (?, ?, 'SEASON', ?, ?, 1, ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(&season_id)
        .bind(&library_id)
        .bind(&series_id)
        .bind(&series_id)
        .bind(&season_id)
        .bind(&season_id)
        .execute(database.pool())
        .await
        .expect("season");
        sqlx::query(
            "INSERT INTO media_items
                 (id, library_id, item_type, parent_id, series_id, season_number,
                  episode_number, title, sort_title, identification_status)
             VALUES (?, ?, 'EPISODE', ?, ?, 1, 1, ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(&episode_id)
        .bind(&library_id)
        .bind(&season_id)
        .bind(&series_id)
        .bind(&episode_id)
        .bind(&episode_id)
        .execute(database.pool())
        .await
        .expect("episode");
        database
            .insert_filesystem_entry(NewFilesystemEntry {
                id: &poster_entry_id,
                library_root_id: &root_id,
                relative_path: &format!("{directory}/poster.jpg"),
                entry_kind: "FILE",
                size: 1,
                modified_at: 1,
                device: None,
                inode: None,
                fingerprint: b"poster",
                last_seen_generation: "generation",
            })
            .await
            .expect("poster entry");
        database
            .insert_filesystem_entry(NewFilesystemEntry {
                id: &episode_entry_id,
                library_root_id: &root_id,
                relative_path: &format!("{directory}/Season 1/Episode.mkv"),
                entry_kind: "FILE",
                size: 1,
                modified_at: 1,
                device: None,
                inode: None,
                fingerprint: b"episode",
                last_seen_generation: "generation",
            })
            .await
            .expect("episode entry");
        sqlx::query(
            "INSERT INTO media_sources (id, item_id, source_kind, filesystem_entry_id, is_default)
             VALUES (?, ?, 'LOCAL_FILE', ?, 1)",
        )
        .bind(format!("local-source-media-source-{index:03}"))
        .bind(&episode_id)
        .bind(&episode_entry_id)
        .execute(database.pool())
        .await
        .expect("episode source");
        poster_entry_ids.push(poster_entry_id);
    }

    database.reset_query_count();
    let sources = database
        .list_scan_local_metadata_sources(&poster_entry_ids)
        .await
        .expect("local metadata sources");
    assert_eq!(sources.len(), DIRECTORY_COUNT);
    assert_eq!(database.query_count(), 4);
}

#[tokio::test]
async fn manifest_unresolved_files_use_bounded_batch_writes()
-> Result<(), Box<dyn std::error::Error>> {
    const FILE_COUNT: usize = 205;
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Manifest unresolved", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path).await?;
    let root = LibraryService::new(database.clone())
        .add_root(
            library.id,
            root_path.to_str().ok_or("non-UTF-8 media root")?,
        )
        .await?;
    let root_id = root.root.id.to_string();
    let mut files = Vec::with_capacity(FILE_COUNT);
    for index in 0..FILE_COUNT {
        let relative_path = format!("folder-{index}/video-{index}.mkv");
        let filesystem_entry_id = format!("unresolved-entry-{index}");
        database
            .insert_filesystem_entry(NewFilesystemEntry {
                id: &filesystem_entry_id,
                library_root_id: &root_id,
                relative_path: &relative_path,
                entry_kind: "FILE",
                size: 1,
                modified_at: 1,
                device: None,
                inode: None,
                fingerprint: b"fixture",
                last_seen_generation: "generation",
            })
            .await?;
        files.push(NewScanManifestUnresolvedFile {
            item_id: format!("unresolved-item-{index}"),
            filesystem_entry_id,
            source_id: format!("unresolved-source-{index}"),
            relative_path,
            size: 1,
            modified_at: 1,
            inode: None,
            fingerprint: b"fixture".to_vec(),
            title: format!("Video {index}"),
            identity_key: format!("unresolved:{root_id}:video-{index}"),
            source_kind: "LOCAL_FILE".to_owned(),
            container: "mkv".to_owned(),
            external_url: None,
            strm_target_kind: None,
            home_video: false,
        });
    }

    let mut transaction = database.pool().begin().await?;
    database.reset_query_count();
    database
        .materialize_manifest_unresolved_files_after_filesystem_insert_in_transaction(
            &mut transaction,
            &library_id,
            &root_id,
            &files,
        )
        .await?;
    assert_eq!(database.query_count(), 12);
    transaction.commit().await?;

    let item_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items WHERE library_id = ? AND item_type = 'UNRESOLVED'",
    )
    .bind(&library_id)
    .fetch_one(database.pool())
    .await?;
    let source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources WHERE source_kind = 'LOCAL_FILE'
         AND item_id IN (SELECT id FROM media_items WHERE library_id = ?)",
    )
    .bind(&library_id)
    .fetch_one(database.pool())
    .await?;
    let parent_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items item
         JOIN media_items parent ON item.parent_id = parent.id
         WHERE item.library_id = ? AND item.item_type = 'UNRESOLVED'
           AND parent.item_type = 'FOLDER'",
    )
    .bind(&library_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(item_count, FILE_COUNT as i64);
    assert_eq!(source_count, FILE_COUNT as i64);
    assert_eq!(parent_count, FILE_COUNT as i64);
    Ok(())
}

#[tokio::test]
async fn scan_job_metadata_page_preserves_kind_priority_and_item_order()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Mixed", LibraryKind::Mixed, false)
        .await?;
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path).await?;
    let root_path = root_path.to_str().ok_or("non-UTF8 test root")?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('metadata-page-root', ?, ?, ?, 1, 0)",
        )
        .bind(library.id.to_string())
        .bind(root_path)
        .bind(root_path)
        .execute(database.pool())
        .await?;
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await?;
    database
        .query("UPDATE scan_jobs SET auto_metadata_match = 1 WHERE id = ?")
        .bind(&job.id)
        .execute(database.pool())
        .await?;

    for (item_id, item_type, parent_id, series_id, season_number) in [
        ("movie-z", "MOVIE", None, None, None),
        ("movie-a", "MOVIE", None, None, None),
        ("video-a", "VIDEO", None, None, None),
        ("series-a", "SERIES", None, None, None),
        (
            "season-a",
            "SEASON",
            Some("series-a"),
            Some("series-a"),
            Some(1_i64),
        ),
        (
            "episode-a",
            "EPISODE",
            Some("season-a"),
            Some("series-a"),
            Some(1_i64),
        ),
    ] {
        database
            .query(
                "INSERT INTO media_items (
                     id, library_id, item_type, parent_id, series_id, season_number,
                     title, sort_title, identification_status
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .bind(item_type)
            .bind(parent_id)
            .bind(series_id)
            .bind(season_number)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
    }
    for item_id in ["movie-z", "movie-a", "video-a", "episode-a"] {
        let entry_id = format!("entry-{item_id}");
        let source_id = format!("source-{item_id}");
        let relative_path = format!("{item_id}.mkv");
        database
            .query(
                "INSERT INTO filesystem_entries (
                     id, library_root_id, relative_path, entry_kind, size, modified_at,
                     last_seen_generation
                 ) VALUES (?, 'metadata-page-root', ?, 'FILE', 1, 1, 'generation')",
            )
            .bind(&entry_id)
            .bind(&relative_path)
            .execute(database.pool())
            .await?;
        database
            .query(
                "INSERT INTO media_sources (
                     id, item_id, source_kind, filesystem_entry_id, is_default
                 ) VALUES (?, ?, 'LOCAL_FILE', ?, 1)",
            )
            .bind(&source_id)
            .bind(item_id)
            .bind(&entry_id)
            .execute(database.pool())
            .await?;
        database
            .query(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, item_id, change_kind, metadata_state
                 ) VALUES (?, 'ITEM', ?, ?, 'NEW', 'PENDING')",
            )
            .bind(&job.id)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
    }
    database.reset_query_count();
    let page = database.load_scan_job_metadata_page(&job.id, 1).await?;
    assert!(page.has_pending);
    assert_eq!(page.job_type.as_deref(), Some(job.job_type.as_str()));
    assert_eq!(page.job_status.as_deref(), Some(job.status.as_str()));
    assert!(page.auto_metadata_match);
    let StoredScanJobMetadataSources::Movies(movies) = page.sources else {
        return Err("movie targets should have priority over other media types".into());
    };
    assert_eq!(movies.len(), 1);
    assert_eq!(movies[0].item_id, "movie-a");
    assert_eq!(database.query_count(), 1);

    database
        .query(
            "UPDATE scan_job_targets SET metadata_state = 'DONE'
             WHERE job_id = ? AND target_id IN ('movie-a', 'movie-z')",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    database
        .query("DELETE FROM media_sources WHERE item_id = 'movie-z'")
        .execute(database.pool())
        .await?;
    database
        .query(
            "UPDATE scan_job_targets SET metadata_state = 'PENDING'
             WHERE job_id = ? AND target_id = 'movie-z'",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    database.reset_query_count();
    let page = database.load_scan_job_metadata_page(&job.id, 8).await?;
    let StoredScanJobMetadataSources::HomeVideos(videos) = page.sources else {
        return Err("home video targets should follow movies".into());
    };
    assert_eq!(
        videos
            .iter()
            .map(|source| source.item_id.as_str())
            .collect::<Vec<_>>(),
        ["video-a"]
    );
    assert_eq!(database.query_count(), 1);

    database
        .query(
            "UPDATE scan_job_targets SET metadata_state = 'DONE'
             WHERE job_id = ? AND target_id = 'video-a'",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    database.reset_query_count();
    let page = database.load_scan_job_metadata_page(&job.id, 8).await?;
    let StoredScanJobMetadataSources::Episodes(episodes) = page.sources else {
        return Err("episode targets should follow home videos".into());
    };
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0].episode_id, "episode-a");
    assert_eq!(episodes[0].series_id, "series-a");
    assert_eq!(database.query_count(), 1);

    database
        .query(
            "UPDATE scan_job_targets SET metadata_state = 'DONE'
             WHERE job_id = ? AND target_id = 'episode-a'",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    database.reset_query_count();
    let page = database.load_scan_job_metadata_page(&job.id, 8).await?;
    assert!(
        page.has_pending,
        "an unavailable movie source remains pending"
    );
    assert!(matches!(page.sources, StoredScanJobMetadataSources::None));
    assert_eq!(database.query_count(), 1);

    database
        .query(
            "UPDATE scan_job_targets SET metadata_state = 'FAILED'
             WHERE job_id = ? AND target_id = 'movie-z'",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    database.reset_query_count();
    let page = database.load_scan_job_metadata_page(&job.id, 8).await?;
    assert!(!page.has_pending);
    assert!(matches!(page.sources, StoredScanJobMetadataSources::None));
    assert_eq!(database.query_count(), 1);

    database
        .query("UPDATE scan_jobs SET status = 'FAILED' WHERE id = ?")
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
             VALUES ('metadata-page-empty-job', ?, 'RECONCILE_LIBRARY', 'RUNNING', 'generation')",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
    database.reset_query_count();
    let empty_job_page = database
        .load_scan_job_metadata_page("metadata-page-empty-job", 8)
        .await?;
    assert_eq!(
        empty_job_page.job_type.as_deref(),
        Some("RECONCILE_LIBRARY")
    );
    assert_eq!(empty_job_page.job_status.as_deref(), Some("RUNNING"));
    assert!(!empty_job_page.auto_metadata_match);
    assert!(!empty_job_page.has_pending);
    assert!(matches!(
        empty_job_page.sources,
        StoredScanJobMetadataSources::None
    ));
    assert_eq!(database.query_count(), 1);

    database.reset_query_count();
    let failed_job_page = database.load_scan_job_metadata_page(&job.id, 8).await?;
    assert_eq!(
        failed_job_page.job_type.as_deref(),
        Some(job.job_type.as_str())
    );
    assert_eq!(failed_job_page.job_status.as_deref(), Some("FAILED"));
    assert_eq!(database.query_count(), 1);

    database.reset_query_count();
    let missing_job_page = database
        .load_scan_job_metadata_page("metadata-page-job-does-not-exist", 8)
        .await?;
    assert_eq!(missing_job_page.job_type, None);
    assert_eq!(missing_job_page.job_status, None);
    assert!(!missing_job_page.auto_metadata_match);
    assert!(!missing_job_page.has_pending);
    assert_eq!(database.query_count(), 1);
    Ok(())
}

#[tokio::test]
async fn scan_manifest_postprocessing_state_loads_roots_in_one_query()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path).await?;
    let root_path = root_path.to_str().ok_or("non-UTF8 test root")?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('manifest-state-root', ?, ?, ?, 1, 0)",
        )
        .bind(library.id.to_string())
        .bind(root_path)
        .bind(root_path)
        .execute(database.pool())
        .await?;
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await?;
    database
        .query(
            "UPDATE scan_manifests SET state = 'POSTPROCESSING', workflow_version = 3,
                 discovery_format_version = 3, discovery_mode = 'LITE'
             WHERE job_id = ?",
        )
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    let manifest_id: String = sqlx::query_scalar("SELECT id FROM scan_manifests WHERE job_id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_manifest_roots (
                 manifest_id, library_root_id, state, postprocessing_target_stage
             ) VALUES (?, 'manifest-state-root', 'COMPLETE', 'NEW')
             ON CONFLICT(manifest_id, library_root_id) DO UPDATE SET
                 state = 'COMPLETE', postprocessing_target_stage = 'NEW'",
        )
        .bind(&manifest_id)
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_manifest_entries (
                 manifest_id, library_root_id, relative_path, observation_sequence,
                 entry_kind, size, modified_at, device, inode
             ) VALUES (?, 'manifest-state-root', '', 1,
                       'DIRECTORY', 0, 1, 11, 22)
             ON CONFLICT(manifest_id, library_root_id, relative_path, observation_sequence)
             DO UPDATE SET device = 11, inode = 22",
        )
        .bind(&manifest_id)
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 last_seen_generation, last_seen_change_kind
             ) VALUES ('manifest-positive-entry', 'manifest-state-root', 'movie.mkv',
                       'FILE', 1, 1, ?, 'NEW')",
        )
        .bind(&job.generation)
        .execute(database.pool())
        .await?;

    database.reset_query_count();
    let state = database
        .get_scan_manifest_postprocessing_state_by_job(&job.id)
        .await?
        .ok_or("scan manifest state should be present")?;
    assert_eq!(state.manifest_id, manifest_id);
    assert_eq!(state.workflow_version, 3);
    assert_eq!(state.discovery_format_version, 3);
    assert_eq!(state.roots.len(), 1);
    assert_eq!(state.roots[0].expected_device, Some(11));
    assert_eq!(state.roots[0].expected_inode, Some(22));
    assert!(state.roots[0].has_positive_rows);
    assert!(state.roots[0].has_stage_rows);
    assert_eq!(database.query_count(), 1);

    database.reset_query_count();
    assert!(
        database
            .get_scan_manifest_postprocessing_state_by_job("missing-manifest-job")
            .await?
            .is_none()
    );
    assert_eq!(database.query_count(), 1);
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_metadata_batch_writes_preserve_upsert_and_retry_semantics()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration =
        crate::config::DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(
        &config,
        &crate::config::DatabaseConfiguration::Postgres(PostgresConnection {
            database: database_name.clone(),
            ..admin_connection
        }),
    )
    .await?;

    let assertions = async {
        let library = LibraryService::new(database.clone())
            .create_library("Metadata", LibraryKind::Movie, false)
            .await?;
        database
            .query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES ('postgres-metadata-batch-item', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
            )
            .bind(library.id.to_string())
            .execute(database.pool())
            .await?;

        let candidates = [
            NewMetadataCandidate {
                id: "candidate-first",
                item_id: "postgres-metadata-batch-item",
                provider: "tmdb",
                provider_id: "movie-1",
                candidate_json: r#"{"title":"lower"}"#,
                score: 70.0,
                expires_at: Some(1_000),
            },
            NewMetadataCandidate {
                id: "candidate-second",
                item_id: "postgres-metadata-batch-item",
                provider: "tmdb",
                provider_id: "movie-1",
                candidate_json: r#"{"title":"higher"}"#,
                score: 90.0,
                expires_at: Some(2_000),
            },
            NewMetadataCandidate {
                id: "candidate-other",
                item_id: "postgres-metadata-batch-item",
                provider: "tmdb",
                provider_id: "movie-2",
                candidate_json: r#"{"title":"other"}"#,
                score: 60.0,
                expires_at: Some(3_000),
            },
        ];
        database.reset_query_count();
        database.insert_metadata_candidates(&candidates).await?;
        assert_eq!(database.query_count(), 1);
        let rows: Vec<(String, String, f64)> = sqlx::query_as(
            "SELECT id, candidate_json, score FROM metadata_candidates
             WHERE item_id = 'postgres-metadata-batch-item' AND provider_id = 'movie-1'",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(rows, vec![(
            "candidate-first".to_owned(),
            r#"{"title":"higher"}"#.to_owned(),
            90.0,
        )]);

        let capabilities = [MetadataCapabilityResult {
            capability: "EXTERNAL_IDS",
            has_data: false,
        }];
        database.reset_query_count();
        database
            .record_metadata_capability_results(
                "postgres-metadata-batch-item",
                "tmdb",
                "movie-1",
                &capabilities,
                1_000,
            )
            .await?;
        assert_eq!(database.query_count(), 1);
        database.reset_query_count();
        database
            .record_metadata_capability_failures(
                "postgres-metadata-batch-item",
                "tmdb",
                "movie-1",
                &["EXTERNAL_IDS", "TRAILERS"],
                2_000,
            )
            .await?;
        assert_eq!(database.query_count(), 2);

        let unavailable = [
            MetadataImageUnavailable {
                image_type: "POSTER",
                candidate_key: "tmdb:movie-1:POSTER",
            },
            MetadataImageUnavailable {
                image_type: "FANART",
                candidate_key: "tmdb:movie-1:FANART",
            },
        ];
        database.reset_query_count();
        database
            .mark_metadata_images_unavailable(
                "postgres-metadata-batch-item",
                &unavailable,
                3_000,
            )
            .await?;
        assert_eq!(database.query_count(), 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    assertions?;
    drop_database?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_scan_write_transaction_uses_local_async_commit()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration =
        crate::config::DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER DATABASE {database_name} SET synchronous_commit = on"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let connection = PostgresConnection {
        database: database_name.clone(),
        ..admin_connection
    };
    let database = Database::connect_with_configuration(
        &config,
        &crate::config::DatabaseConfiguration::Postgres(connection),
    )
    .await?;
    let mut metadata_transaction = database.begin_metadata_write_transaction().await?;
    let metadata_setting: String = sqlx::query_scalar("SHOW synchronous_commit")
        .fetch_one(&mut *metadata_transaction)
        .await?;
    metadata_transaction.commit().await?;

    let mut transaction = database.begin_scan_write_transaction().await?;
    let transaction_setting: String = sqlx::query_scalar("SHOW synchronous_commit")
        .fetch_one(&mut *transaction)
        .await?;
    transaction.commit().await?;
    let session_setting: String = sqlx::query_scalar("SHOW synchronous_commit")
        .fetch_one(database.pool())
        .await?;

    database.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    admin_pool.close().await;

    assert_eq!(transaction_setting, "off");
    assert_eq!(metadata_setting, "on");
    assert_eq!(session_setting, "on");
    Ok(())
}

#[tokio::test]
async fn recommendation_stats_are_refreshed_once_per_batch_and_deduplicate_users() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let admin = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Recommendations", LibraryKind::Movie, false)
        .await
        .expect("library");
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await
        .expect("current timestamp");
    let admin_id = admin.id.to_string();
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO users (
                id, username_normalized, display_name, password_hash
             ) VALUES ('recommendation-user-2', 'recommendation-user-2',
                       'Recommendation User 2', 'test')",
    )
    .execute(database.pool())
    .await
    .expect("second user");
    for item_id in ["playback-item", "favorite-item", "expired-item"] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, last_played_at)
         VALUES (?, 'playback-item', ?),
                ('recommendation-user-2', 'playback-item', ?),
                (?, 'expired-item', ?),
                (?, 'favorite-item', 0),
                ('recommendation-user-2', 'favorite-item', 0)",
    )
    .bind(&admin_id)
    .bind(now)
    .bind(now)
    .bind(&admin_id)
    .bind(now - 180 * 86_400)
    .bind(&admin_id)
    .execute(database.pool())
    .await
    .expect("user item states");
    sqlx::query(
        "UPDATE user_item_state
         SET is_favorite = 1
         WHERE item_id = 'favorite-item'",
    )
    .execute(database.pool())
    .await
    .expect("favorite states");
    sqlx::query(
        "INSERT INTO playback_sessions (
                id, user_id, item_id, play_session_id, device_id, state, last_event_at
             ) VALUES ('recommendation-session', ?, 'playback-item',
                       'recommendation-play-session', 'test', 'PLAYING', ?)",
    )
    .bind(&admin_id)
    .bind(now)
    .execute(database.pool())
    .await
    .expect("playback session");

    refresh_recommendation_stats(&database).await;
    let scores = sqlx::query(
        "SELECT item_id, recent_playback_score, favorite_score
         FROM recommendation_item_stats
         WHERE item_id IN ('playback-item', 'favorite-item', 'expired-item')
         ORDER BY item_id",
    )
    .fetch_all(database.pool())
    .await
    .expect("recommendation scores");
    assert_eq!(scores.len(), 3);
    assert_eq!(scores[0].get::<String, _>("item_id"), "expired-item");
    assert_eq!(scores[0].get::<i64, _>("recent_playback_score"), 0);
    assert_eq!(scores[1].get::<String, _>("item_id"), "favorite-item");
    assert_eq!(scores[1].get::<i64, _>("favorite_score"), 10);
    assert_eq!(scores[2].get::<String, _>("item_id"), "playback-item");
    assert_eq!(scores[2].get::<i64, _>("recent_playback_score"), 2);
    assert!(
        !database
            .refresh_recommendation_stats_if_needed()
            .await
            .expect("same-batch stats refresh")
    );

    sqlx::query(
        "UPDATE media_items
         SET removed_at = ?
         WHERE id = 'expired-item'",
    )
    .bind(now)
    .execute(database.pool())
    .await
    .expect("remove recommendation item");
    refresh_recommendation_stats(&database).await;
    let remaining_stats: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM recommendation_item_stats
         WHERE item_id IN ('playback-item', 'favorite-item', 'expired-item')",
    )
    .fetch_one(database.pool())
    .await
    .expect("remaining recommendation stats");
    assert_eq!(remaining_stats, 2);
}

#[tokio::test]
async fn played_container_state_sync_batches_parent_queries_and_preserves_state() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let admin = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Shows", LibraryKind::Series, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    let user_id = admin.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, parent_id, series_id, title, sort_title,
             identification_status, has_available_source
         ) VALUES
            ('played-parent-series', ?, 'SERIES', NULL, NULL, 'Series', 'series', 'LOCAL_CONFIRMED', 1),
            ('played-parent-season', ?, 'SEASON', 'played-parent-series', 'played-parent-series', 'Season', 'season', 'LOCAL_CONFIRMED', 1),
            ('played-episode-1', ?, 'EPISODE', 'played-parent-season', 'played-parent-series', 'Episode 1', 'episode 1', 'LOCAL_CONFIRMED', 1),
            ('played-episode-2', ?, 'EPISODE', 'played-parent-season', 'played-parent-series', 'Episode 2', 'episode 2', 'LOCAL_CONFIRMED', 1)",
    )
    .bind(&library_id)
    .bind(&library_id)
    .bind(&library_id)
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("media hierarchy");
    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, is_played, play_count)
         VALUES (?, 'played-episode-1', 1, 1), (?, 'played-episode-2', 1, 1)",
    )
    .bind(&user_id)
    .bind(&user_id)
    .execute(database.pool())
    .await
    .expect("episode states");

    database.reset_query_count();
    database
        .sync_played_container_states(&user_id, "played-episode-1")
        .await
        .expect("played parent states");
    assert_eq!(database.query_count(), 3);

    let played_states: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT item_id, is_played, play_count FROM user_item_state
         WHERE user_id = ? AND item_id IN ('played-parent-season', 'played-parent-series')
         ORDER BY item_id",
    )
    .bind(&user_id)
    .fetch_all(database.pool())
    .await
    .expect("played parent state rows");
    assert_eq!(
        played_states,
        vec![
            ("played-parent-season".to_owned(), 1, 1),
            ("played-parent-series".to_owned(), 1, 1),
        ]
    );

    sqlx::query(
        "UPDATE user_item_state SET is_played = 0 WHERE user_id = ? AND item_id = 'played-episode-2'",
    )
    .bind(&user_id)
    .execute(database.pool())
    .await
    .expect("unmark episode");
    database.reset_query_count();
    database
        .sync_played_container_states(&user_id, "played-episode-2")
        .await
        .expect("unplayed parent states");
    assert_eq!(database.query_count(), 3);

    let unplayed_states: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT item_id, is_played, version FROM user_item_state
         WHERE user_id = ? AND item_id IN ('played-parent-season', 'played-parent-series')
         ORDER BY item_id",
    )
    .bind(&user_id)
    .fetch_all(database.pool())
    .await
    .expect("unplayed parent state rows");
    assert_eq!(
        unplayed_states,
        vec![
            ("played-parent-season".to_owned(), 0, 1),
            ("played-parent-series".to_owned(), 0, 1),
        ]
    );
}

#[tokio::test]
async fn home_resume_page_returns_items_and_total_with_one_query() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let admin = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Home resume", LibraryKind::Movie, false)
        .await
        .expect("library");
    let user_id = admin.id.to_string();
    let library_id = library.id.to_string();
    for item_id in ["resume-one", "resume-two"] {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, runtime_ticks,
                 identification_status, has_available_source
             ) VALUES (?, ?, 'MOVIE', ?, ?, 2000000000, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
        sqlx::query(
            "INSERT INTO user_item_state (
                 user_id, item_id, is_played, position_ticks, last_played_at
             ) VALUES (?, ?, 0, 1000000000, 1)",
        )
        .bind(&user_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("resume state");
    }

    database.reset_query_count();
    let (rows, total) = database
        .list_resume_items_with_total(
            &ResumeItemsQuery {
                user_id: &user_id,
                library_ids: std::slice::from_ref(&library_id),
                item_types: &["MOVIE"],
                content_restrictions: None,
                played_percent: 90,
                minimum_ticks: 0,
                offset: 0,
                limit: 1,
            },
            true,
        )
        .await
        .expect("home resume page");

    assert_eq!(database.query_count(), 1);
    assert_eq!(total, 2);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].item_id, "resume-one");

    database.reset_query_count();
    let (past_end, total) = database
        .list_resume_items_with_total(
            &ResumeItemsQuery {
                user_id: &user_id,
                library_ids: std::slice::from_ref(&library_id),
                item_types: &["MOVIE"],
                content_restrictions: None,
                played_percent: 90,
                minimum_ticks: 0,
                offset: 10,
                limit: 1,
            },
            true,
        )
        .await
        .expect("out-of-range home resume page");

    assert!(past_end.is_empty());
    assert_eq!(total, 2);
    assert_eq!(database.query_count(), 2);
}

#[tokio::test]
async fn resume_items_apply_content_restrictions_before_ranking()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp.path().join("config"),
    })
    .await?;
    let admin = SetupService::new(database.clone())?
        .complete("Resume Admin", "resume-admin", "test password")
        .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Restricted resume", LibraryKind::Movie, false)
        .await?;
    let user_id = admin.id.to_string();
    let library_id = library.id.to_string();
    for (item_id, certification) in [("resume-visible", "PG"), ("resume-hidden", "R")] {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, runtime_ticks,
                 identification_status, has_available_source
             ) VALUES (?, ?, 'MOVIE', ?, ?, 2000000000, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO user_item_state (
                 user_id, item_id, is_played, position_ticks, last_played_at
             ) VALUES (?, ?, 0, 1000000000, 1)",
        )
        .bind(&user_id)
        .bind(item_id)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_content_restriction_index (item_id, certification_code)
             VALUES (?, ?)",
        )
        .bind(item_id)
        .bind(certification)
        .execute(database.pool())
        .await?;
    }
    let restrictions = match database
        .compare_and_swap_user_content_restrictions(
            &user_id,
            0,
            NewUserContentRestrictions {
                rating_system: "MPA",
                maximum_rating: Some("PG-13"),
                allow_unrated: false,
                allowed_tags: &[],
                blocked_tags: &[],
            },
        )
        .await?
    {
        UserContentRestrictionsCasResult::Updated(value) => value,
        other => return Err(format!("expected policy update, got {other:?}").into()),
    };
    let query = ResumeItemsQuery {
        user_id: &user_id,
        library_ids: std::slice::from_ref(&library_id),
        item_types: &["MOVIE"],
        content_restrictions: Some(&restrictions),
        played_percent: 90,
        minimum_ticks: 0,
        offset: 0,
        limit: 10,
    };

    let (rows, total) = database.list_resume_items_with_total(&query, true).await?;

    assert_eq!(total, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].item_id, "resume-visible");
    let past_end_query = ResumeItemsQuery {
        offset: 1,
        limit: 1,
        ..query
    };
    let (past_end, total) = database
        .list_resume_items_with_total(&past_end_query, true)
        .await?;
    assert!(past_end.is_empty());
    assert_eq!(total, 1);
    Ok(())
}

#[tokio::test]
async fn home_resume_settings_are_read_with_one_query() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let admin = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let user_id = admin.id.to_string();
    database
        .set_user_played_percent(&user_id, 85)
        .await
        .expect("played percent");
    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES ('resume_min_ticks', '300000000')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .execute(database.pool())
    .await
    .expect("minimum resume ticks");

    database.reset_query_count();
    let settings = database
        .home_resume_settings(&user_id)
        .await
        .expect("home resume settings");

    assert_eq!(database.query_count(), 1);
    assert_eq!(settings, (85, 300_000_000));
}

#[tokio::test]
async fn thumbnail_scraper_retries_are_persisted_and_claimed_at_due_times() {
    const SIX_HOURS: i64 = 6 * 60 * 60;
    const ONE_DAY: i64 = 24 * 60 * 60;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Retry Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir)
        .await
        .expect("movie directory");
    tokio::fs::write(movie_dir.join("Retry.Movie.2024.mkv"), b"video")
        .await
        .expect("movie file");
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("index movie");
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE'")
            .fetch_one(database.pool())
            .await
            .expect("indexed item");

    let first_attempt_at = 1_000;
    assert!(
        database
            .ensure_thumbnail_scraper_retry(
                &item_id,
                first_attempt_at,
                first_attempt_at + SIX_HOURS,
            )
            .await
            .expect("seed first retry")
    );
    assert!(
        !database
            .ensure_thumbnail_scraper_retry(
                &item_id,
                first_attempt_at + 1,
                first_attempt_at + SIX_HOURS + 1,
            )
            .await
            .expect("do not reset retry history")
    );
    assert!(
        database
            .claim_due_thumbnail_scraper_retries(
                first_attempt_at + SIX_HOURS - 1,
                first_attempt_at + SIX_HOURS + 300,
                10,
            )
            .await
            .expect("check before due")
            .is_empty()
    );

    let second_attempt = database
        .claim_due_thumbnail_scraper_retries(
            first_attempt_at + SIX_HOURS,
            first_attempt_at + SIX_HOURS + 300,
            10,
        )
        .await
        .expect("claim six-hour retry");
    assert_eq!(second_attempt.len(), 1);
    assert_eq!(second_attempt[0].attempt_count, 1);
    assert!(
        database
            .finish_thumbnail_scraper_retry(
                &item_id,
                2,
                Some(first_attempt_at + ONE_DAY),
                first_attempt_at + SIX_HOURS,
            )
            .await
            .expect("schedule twenty-four-hour retry")
    );

    let third_attempt = database
        .claim_due_thumbnail_scraper_retries(
            first_attempt_at + ONE_DAY,
            first_attempt_at + ONE_DAY + 300,
            10,
        )
        .await
        .expect("claim twenty-four-hour retry");
    assert_eq!(third_attempt.len(), 1);
    assert_eq!(third_attempt[0].attempt_count, 2);
    assert!(
        database
            .finish_thumbnail_scraper_retry(&item_id, 3, None, first_attempt_at + ONE_DAY,)
            .await
            .expect("complete retry sequence")
    );

    let completed = database
        .find_thumbnail_scraper_retry(&item_id)
        .await
        .expect("read completed retry")
        .expect("retry state exists");
    assert_eq!(completed.status, "COMPLETE");
    assert_eq!(completed.attempt_count, 3);
    assert_eq!(completed.first_attempt_at, first_attempt_at);
    assert_eq!(completed.next_retry_at, None);
}

#[tokio::test]
async fn thumbnail_scraper_retry_claims_use_one_bounded_update() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('retry-lib', 'Retries', 'MOVIE')")
        .execute(database.pool())
        .await
        .expect("retry library");
    sqlx::query(
        "WITH RECURSIVE numbers(n) AS (
             SELECT 0 UNION ALL SELECT n + 1 FROM numbers WHERE n < 204
         )
         INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         )
         SELECT 'retry-item-' || printf('%03d', n), 'retry-lib', 'MOVIE',
                'Retry ' || n, 'retry ' || n, 'LOCAL_CONFIRMED'
         FROM numbers",
    )
    .execute(database.pool())
    .await
    .expect("retry items");
    for index in 0..205 {
        assert!(
            database
                .ensure_thumbnail_scraper_retry(&format!("retry-item-{index:03}"), 1, 10)
                .await
                .expect("seed retry")
        );
    }

    database.reset_query_count();
    let claimed = database
        .claim_due_thumbnail_scraper_retries(10, 20, 128)
        .await
        .expect("claim retries");
    assert_eq!(claimed.len(), 100);
    assert_eq!(database.query_count(), 2);
    assert!(
        claimed
            .windows(2)
            .all(|pair| pair[0].item_id < pair[1].item_id)
    );
    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM thumbnail_scraper_retries WHERE status = 'PENDING'",
    )
    .fetch_one(database.pool())
    .await
    .expect("pending retries");
    assert_eq!(pending, 105);
    let claimed_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM thumbnail_scraper_retries
         WHERE status = 'RUNNING' AND claimed_until = 20 AND updated_at = 10",
    )
    .fetch_one(database.pool())
    .await
    .expect("claim lease");
    assert_eq!(claimed_count, 100);
    let remaining = database
        .claim_due_thumbnail_scraper_retries(19, 30, 100)
        .await
        .expect("claim other pending retries");
    assert_eq!(remaining.len(), 100);
    assert_eq!(remaining[0].item_id, "retry-item-100");
    assert_eq!(
        database
            .claim_due_thumbnail_scraper_retries(19, 30, 100)
            .await
            .expect("claim tail")
            .len(),
        5
    );
    database.reset_query_count();
    assert!(
        database
            .claim_due_thumbnail_scraper_retries(19, 30, 100)
            .await
            .expect("no early reclaim")
            .is_empty()
    );
    assert_eq!(database.query_count(), 1);
    database.reset_query_count();
    let reclaimed = database
        .claim_due_thumbnail_scraper_retries(20, 40, 100)
        .await
        .expect("reclaim expired lease");
    assert_eq!(reclaimed.len(), 100);
    assert_eq!(database.query_count(), 2);
    assert_eq!(reclaimed[0].item_id, "retry-item-000");
    assert!(reclaimed.iter().all(|retry| retry.status == "RUNNING"
        && retry.attempt_count == 1
        && retry.first_attempt_at == 1
        && retry.next_retry_at == Some(10)));
}

#[tokio::test]
async fn recommendation_daily_batch_is_stable_until_the_next_batch() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Recommendations", LibraryKind::Movie, false)
        .await
        .expect("library");
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await
        .expect("current timestamp");
    let library_id = library.id.to_string();
    let user_id = user.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, added_at, has_available_source
             ) VALUES ('daily-item-1', ?, 'MOVIE', 'Daily item 1', 'daily item 1',
                       'LOCAL_CONFIRMED', ?, 1)",
    )
    .bind(&library_id)
    .bind(now - 15 * 86_400)
    .execute(database.pool())
    .await
    .expect("initial media item");
    refresh_recommendation_stats(&database).await;

    let service = CatalogService::new(database.clone(), MediaAccessService::new(database.clone()));
    let first = service
        .list_recommended_for_library_ids(std::slice::from_ref(&library_id), &user_id, 7)
        .await
        .expect("first daily recommendation");
    assert_eq!(first.len(), 1);
    let first_ids = first.iter().map(|item| item.id.clone()).collect::<Vec<_>>();

    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, added_at, has_available_source, rating
             ) VALUES ('daily-item-2', ?, 'MOVIE', 'Daily item 2', 'daily item 2',
                       'LOCAL_CONFIRMED', ?, 1, 10.0)",
    )
    .bind(&library_id)
    .bind(now)
    .execute(database.pool())
    .await
    .expect("new media item");
    let same_batch = service
        .list_recommended_for_library_ids(std::slice::from_ref(&library_id), &user_id, 7)
        .await
        .expect("same daily recommendation");
    assert_eq!(
        same_batch
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>(),
        first_ids
    );

    sqlx::query(
        "UPDATE recommendation_daily_batches
         SET batch_key = batch_key - 1
         WHERE user_id = ?",
    )
    .bind(&user_id)
    .execute(database.pool())
    .await
    .expect("advance recommendation batch");
    let next_batch = service
        .list_recommended_for_library_ids(&[library_id], &user_id, 7)
        .await
        .expect("next daily recommendation");
    assert_eq!(next_batch.len(), 2);
    assert!(next_batch.iter().any(|item| item.id == "daily-item-2"));
}

#[tokio::test]
async fn listing_recommendations_does_not_refresh_global_stats_synchronously() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Recommendations", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, has_available_source
             ) VALUES ('home-recommendation-item', ?, 'MOVIE',
                       'Home recommendation item', 'home recommendation item',
                       'LOCAL_CONFIRMED', 1)",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    sqlx::query(
        "UPDATE recommendation_stats_state
         SET batch_key = -1, refreshed_at = 0
         WHERE id = 1",
    )
    .execute(database.pool())
    .await
    .expect("invalidate recommendation stats batch");

    let library_id = library.id.to_string();
    let user_id = user.id.to_string();
    let service = CatalogService::new(database.clone(), MediaAccessService::new(database.clone()));
    service
        .list_recommended_for_library_ids(std::slice::from_ref(&library_id), &user_id, 7)
        .await
        .expect("recommendations");

    let batch_key: i64 =
        sqlx::query_scalar("SELECT batch_key FROM recommendation_stats_state WHERE id = 1")
            .fetch_one(database.pool())
            .await
            .expect("recommendation stats state");
    assert_eq!(batch_key, -1);
}

#[tokio::test]
async fn cancelled_recommendation_listing_still_saves_the_daily_batch() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Recommendations", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, has_available_source
             ) VALUES ('cancel-recommendation-item', ?, 'MOVIE',
                       'Cancel recommendation item', 'cancel recommendation item',
                       'LOCAL_CONFIRMED', 1)",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");

    let library_id = library.id.to_string();
    let user_id = user.id.to_string();
    let service = CatalogService::new(database.clone(), MediaAccessService::new(database.clone()));

    // Hold the scoring lock so the detached task is parked after it has been spawned, then drop
    // the caller the way a home-cache invalidation does.
    let compute_lock = service.recommendation_compute_lock();
    let held = compute_lock.lock().await;
    let caller = {
        let service = service.clone();
        let library_id = library_id.clone();
        let user_id = user_id.clone();
        tokio::spawn(async move {
            service
                .list_recommended_for_library_ids(&[library_id], &user_id, 7)
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    caller.abort();
    let _ = caller.await;
    drop(held);

    let mut saved = 0_i64;
    for _ in 0..100 {
        saved = sqlx::query_scalar("SELECT COUNT(*) FROM recommendation_daily_batches")
            .fetch_one(database.pool())
            .await
            .expect("daily batch count");
        if saved > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(saved, 1, "the detached scoring task must persist its batch");
}

#[test]
fn database_pool_max_connections_uses_backend_defaults() {
    assert_eq!(
        resolve_database_pool_max_connections(DatabaseBackend::Sqlite, None)
            .expect("SQLite default pool size"),
        8
    );
    assert_eq!(
        resolve_database_pool_max_connections(DatabaseBackend::Postgres, None)
            .expect("PostgreSQL default pool size"),
        20
    );
}

#[test]
fn database_pool_max_connections_accepts_a_bounded_override() {
    assert_eq!(
        resolve_database_pool_max_connections(DatabaseBackend::Sqlite, Some(""))
            .expect("empty pool override uses the default"),
        8
    );
    assert_eq!(
        resolve_database_pool_max_connections(DatabaseBackend::Sqlite, Some("12"))
            .expect("configured pool size"),
        12
    );
    assert_eq!(
        resolve_database_pool_max_connections(DatabaseBackend::Postgres, Some(" 24 "))
            .expect("trimmed configured pool size"),
        24
    );
}

#[test]
fn database_pool_max_connections_rejects_invalid_overrides() {
    for value in ["0", "101", "not-a-number"] {
        assert!(
            resolve_database_pool_max_connections(DatabaseBackend::Sqlite, Some(value)).is_err()
        );
    }
}

#[tokio::test]
async fn scan_job_file_change_queue_is_path_scoped_coalesced_and_retryable() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Example Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir)
        .await
        .expect("movie directory");
    tokio::fs::write(movie_dir.join("Example.Movie.2020.mkv"), b"video")
        .await
        .expect("movie file");

    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let second_root_path = temp_dir.path().join("Other Movies");
    tokio::fs::create_dir_all(&second_root_path)
        .await
        .expect("second root directory");
    let second_root = libraries
        .add_root(
            library.id,
            second_root_path.to_str().expect("second media root"),
        )
        .await
        .expect("second library root")
        .root;
    let root = libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root")
        .root;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("initial index");

    let jobs = ScanJobService::new(database.clone());
    let job = jobs
        .create_movie_scan_job(library.id)
        .await
        .expect("scan job");
    sqlx::query(
        "INSERT INTO scan_job_targets (
             job_id, target_type, target_id, item_id, change_kind,
             probe_state, metadata_state, thumbnail_state
         )
         SELECT ?, 'ITEM', mi.id, mi.id, 'CHANGED', 'DONE', 'DONE', 'DONE'
         FROM media_items mi WHERE mi.library_id = ?",
    )
    .bind(&job.id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("completed item target");
    sqlx::query("UPDATE media_sources SET probe_status = 'READY'")
        .execute(database.pool())
        .await
        .expect("ready media source");
    sqlx::query("UPDATE media_items SET identification_status = 'LOCAL_CONFIRMED'")
        .execute(database.pool())
        .await
        .expect("confirmed media metadata");

    let root_id = root.id.to_string();
    let second_root_id = second_root.id.to_string();
    assert!(
        database
            .record_scan_job_file_change(&job.id, &root_id, "Example Movie (2020)/Zulu.srt", "ADD",)
            .await
            .expect("record subtitle change")
    );
    assert!(database
        .record_scan_job_file_change(
            &job.id,
            &root_id,
            "Example Movie (2020)/Alpha.nfo",
            "ADD",
        )
        .await
        .expect("record NFO change"));
    assert!(
        database
            .record_scan_job_file_change(
                &job.id,
                &second_root_id,
                "Example Movie (2020)/Alpha.nfo",
                "ADD",
            )
            .await
            .expect("record same relative path in another root")
    );
    assert!(!database
        .record_scan_job_file_change(
            &job.id,
            &root_id,
            "Example Movie (2020)/Alpha.nfo",
            "ADD",
        )
        .await
        .expect("deduplicate pending NFO change"));

    let page = database
        .list_pending_scan_job_file_changes(&job.id, None, 1)
        .await
        .expect("first pending page");
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].relative_path, "Example Movie (2020)/Alpha.nfo");
    let next_page = database
        .list_pending_scan_job_file_changes(&job.id, Some(&page[0]), 10)
        .await
        .expect("next pending page");
    assert_eq!(next_page.len(), 2);
    let all_rows = [&page[0], &next_page[0], &next_page[1]];
    assert!(all_rows.windows(2).all(|pair| {
        pair[0].relative_path < pair[1].relative_path
            || (pair[0].relative_path == pair[1].relative_path
                && pair[0].library_root_id < pair[1].library_root_id)
    }));
    let alpha_change = all_rows
        .iter()
        .find(|change| {
            change.library_root_id == root_id && change.relative_path.ends_with("Alpha.nfo")
        })
        .expect("first root NFO event");
    let alpha_revision = alpha_change
        .file_change_revision
        .expect("file queue revision");

    assert!(
        database
            .set_scan_job_file_change_state(
                &job.id,
                &root_id,
                "Example Movie (2020)/Alpha.nfo",
                alpha_revision,
                "RUNNING",
                None,
            )
            .await
            .expect("claim file event")
    );
    assert!(database
        .record_scan_job_file_change(
            &job.id,
            &root_id,
            "Example Movie (2020)/Alpha.nfo",
            "ADD",
        )
        .await
        .expect("new observation during processing"));
    assert!(
        !database
            .set_scan_job_file_change_state(
                &job.id,
                &root_id,
                "Example Movie (2020)/Alpha.nfo",
                alpha_revision,
                "DONE",
                None,
            )
            .await
            .expect("stale completion must not win")
    );
    let current_alpha_revision = database
        .list_pending_scan_job_file_changes(&job.id, None, 10)
        .await
        .expect("current pending event")
        .into_iter()
        .find(|change| {
            change.library_root_id == root_id && change.relative_path.ends_with("Alpha.nfo")
        })
        .and_then(|change| change.file_change_revision)
        .expect("requeued file event revision");
    database
        .set_scan_job_file_change_state(
            &job.id,
            &root_id,
            "Example Movie (2020)/Alpha.nfo",
            current_alpha_revision,
            "FAILED",
            Some("test failure"),
        )
        .await
        .expect("mark file event failed");
    assert!(
        database
            .record_scan_job_file_change(
                &job.id,
                &root_id,
                "Example Movie (2020)/Alpha.nfo",
                "REMOVE",
            )
            .await
            .expect("new observation requeues failed event")
    );
    let requeued = database
        .list_pending_scan_job_file_changes(&job.id, None, 10)
        .await
        .expect("requeued events");
    assert_eq!(
        requeued
            .iter()
            .find(|change| {
                change.library_root_id == root_id && change.relative_path.ends_with("Alpha.nfo")
            })
            .expect("requeued NFO event")
            .change_kind,
        "REMOVE"
    );
    let zulu_revision = requeued
        .iter()
        .find(|change| change.relative_path.ends_with("Zulu.srt"))
        .and_then(|change| change.file_change_revision)
        .expect("pending subtitle revision");
    assert!(
        database
            .set_scan_job_file_change_state(
                &job.id,
                &root_id,
                "Example Movie (2020)/Zulu.srt",
                zulu_revision,
                "RUNNING",
                None,
            )
            .await
            .expect("claim subtitle event")
    );
    database
        .requeue_running_scan_job_file_changes(&job.id)
        .await
        .expect("recover interrupted file event");
    assert!(
        database
            .list_pending_scan_job_file_changes(&job.id, None, 10)
            .await
            .expect("recovered events")
            .iter()
            .any(|change| change.relative_path.ends_with("Zulu.srt"))
    );

    let target_states: (String, String, String) = sqlx::query_as(
        "SELECT probe_state, metadata_state, thumbnail_state
         FROM scan_job_targets WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await
    .expect("item target states");
    assert_eq!(
        target_states,
        ("DONE".to_owned(), "DONE".to_owned(), "DONE".to_owned())
    );
    let probe_status: String = sqlx::query_scalar("SELECT probe_status FROM media_sources LIMIT 1")
        .fetch_one(database.pool())
        .await
        .expect("probe status");
    assert_eq!(probe_status, "READY");
    let identification_status: String =
        sqlx::query_scalar("SELECT identification_status FROM media_items LIMIT 1")
            .fetch_one(database.pool())
            .await
            .expect("metadata status");
    assert_eq!(identification_status, "LOCAL_CONFIRMED");

    sqlx::query("DELETE FROM scan_jobs WHERE id = ?")
        .bind(&job.id)
        .execute(database.pool())
        .await
        .expect("delete scan job");
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_file_changes WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await
            .expect("cascaded file queue rows");
    assert_eq!(remaining, 0);
}

#[tokio::test]
async fn changed_sidecar_target_requeues_completed_local_metadata() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Example Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir)
        .await
        .expect("movie directory");
    tokio::fs::write(movie_dir.join("Example.Movie.2020.mkv"), b"video")
        .await
        .expect("movie file");

    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root = libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root")
        .root;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("initial index");

    let jobs = ScanJobService::new(database.clone());
    let job = jobs
        .create_movie_scan_job(library.id)
        .await
        .expect("scan job");
    let root_id = root.id.to_string();
    let media_path = "Example Movie (2020)/Example.Movie.2020.mkv".to_owned();
    database
        .record_scan_job_targets(&job.id, &root_id, &[media_path], "NEW")
        .await
        .expect("record media target");
    sqlx::query(
        "UPDATE scan_job_targets
         SET metadata_state = 'DONE'
         WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&job.id)
    .execute(database.pool())
    .await
    .expect("complete local metadata target");

    let changed = database
        .record_scan_job_sidecar_targets(
            &job.id,
            &root_id,
            &["Example Movie (2020)/poster.jpg".to_owned()],
        )
        .await
        .expect("record changed sidecar target");
    assert!(changed);
    let state: String = sqlx::query_scalar(
        "SELECT metadata_state
         FROM scan_job_targets
         WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await
    .expect("local metadata target state");
    assert_eq!(state, "PENDING");

    sqlx::query(
        "UPDATE scan_job_targets
         SET updated_at = 1
         WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&job.id)
    .execute(database.pool())
    .await
    .expect("set stable sidecar timestamp");
    let changed = database
        .record_scan_job_sidecar_targets(
            &job.id,
            &root_id,
            &["Example Movie (2020)/poster.jpg".to_owned()],
        )
        .await
        .expect("repeat unchanged sidecar target");
    assert!(!changed);
    let updated_at: i64 = sqlx::query_scalar(
        "SELECT updated_at
         FROM scan_job_targets
         WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await
    .expect("unchanged sidecar target timestamp");
    assert_eq!(updated_at, 1);
}

#[tokio::test]
async fn progressive_scan_metadata_batches_are_bounded_idempotent_and_recoverable() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root)
        .await
        .expect("media root");
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Progressive metadata", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root = libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root");
    let root_id = root.root.id.to_string();
    let sources = vec!["source-a".to_owned()];

    for invalid_sources in [
        Vec::new(),
        vec!["".to_owned()],
        vec!["same".to_owned(), "same".to_owned()],
    ] {
        let invalid = NewScanLocalMetadataBatch {
            id: "invalid-batch",
            job_id: "scan-job",
            library_root_id: &root_id,
            batch_sequence: 0,
            source_ids: &invalid_sources,
        };
        assert!(
            database
                .enqueue_scan_local_metadata_batch(invalid)
                .await
                .is_err()
        );
    }
    let oversized_sources = (0..257)
        .map(|index| format!("source-{index}"))
        .collect::<Vec<_>>();
    let maximum_sources = (0..256)
        .map(|index| format!("max-source-{index}"))
        .collect::<Vec<_>>();
    assert!(
        database
            .enqueue_scan_local_metadata_batch(NewScanLocalMetadataBatch {
                id: "oversized-batch",
                job_id: "scan-job",
                library_root_id: &root_id,
                batch_sequence: 0,
                source_ids: &oversized_sources,
            })
            .await
            .is_err()
    );
    assert!(
        database
            .enqueue_scan_local_metadata_batch(NewScanLocalMetadataBatch {
                id: "max-size-batch",
                job_id: "limit-job",
                library_root_id: &root_id,
                batch_sequence: 0,
                source_ids: &maximum_sources,
            })
            .await
            .expect("256 sources are within the limit")
    );
    assert_eq!(
        database
            .cancel_scan_local_metadata_batches("limit-job")
            .await
            .expect("clean up boundary batch"),
        1
    );

    let first = NewScanLocalMetadataBatch {
        id: "batch-a",
        job_id: "scan-job",
        library_root_id: &root_id,
        batch_sequence: 0,
        source_ids: &sources,
    };
    assert!(
        database
            .enqueue_scan_local_metadata_batch(first)
            .await
            .expect("enqueue")
    );
    assert!(
        !database
            .enqueue_scan_local_metadata_batch(first)
            .await
            .expect("idempotent enqueue")
    );
    let changed_payload = vec!["source-b".to_owned()];
    assert!(
        database
            .enqueue_scan_local_metadata_batch(NewScanLocalMetadataBatch {
                id: "batch-a",
                job_id: "scan-job",
                library_root_id: &root_id,
                batch_sequence: 0,
                source_ids: &changed_payload,
            })
            .await
            .is_err()
    );
    for (id, sequence) in [("batch-b", 1), ("batch-c", 2), ("batch-d", 3)] {
        database
            .enqueue_scan_local_metadata_batch(NewScanLocalMetadataBatch {
                id,
                job_id: "scan-job",
                library_root_id: &root_id,
                batch_sequence: sequence,
                source_ids: &sources,
            })
            .await
            .expect("enqueue batch");
    }

    let page = database
        .list_scan_local_metadata_batches(None, None, 1)
        .await
        .expect("first page");
    assert_eq!(page.len(), 1);
    assert!(
        database
            .list_scan_local_metadata_batches(None, None, 101)
            .await
            .is_err()
    );
    assert!(
        database
            .list_scan_local_metadata_batches(Some(page[0].created_at), None, 1)
            .await
            .is_err(),
        "a cursor must provide both timestamp and id"
    );
    let second_page = database
        .list_scan_local_metadata_batches(Some(page[0].created_at), Some(&page[0].id), 1)
        .await
        .expect("second page");
    assert_eq!(second_page.len(), 1);
    assert_ne!(page[0].id, second_page[0].id);

    let (claimed_a, claimed_b) = tokio::join!(
        database.claim_next_scan_local_metadata_batch(),
        database.claim_next_scan_local_metadata_batch(),
    );
    let claimed_a = claimed_a.expect("claim a").expect("batch a");
    let claimed_b = claimed_b.expect("claim b").expect("batch b");
    assert_ne!(
        claimed_a.id, claimed_b.id,
        "concurrent claims must be distinct"
    );
    assert_eq!(claimed_a.status, "RUNNING");
    assert_eq!(claimed_a.attempts, 1);
    assert_eq!(claimed_a.job_id, "scan-job");
    assert_eq!(claimed_a.library_root_id, root_id);
    assert_eq!(claimed_a.source_count, 1);
    assert_eq!(claimed_a.source_refs_json, r#"["source-a"]"#);
    assert_eq!(claimed_a.batch_sequence, 0);
    assert_eq!(claimed_a.next_attempt_at, None);
    assert_eq!(claimed_a.error, None);
    assert!(claimed_a.updated_at >= claimed_a.created_at);
    assert!(
        database
            .has_pending_scan_local_metadata_images("scan-job")
            .await
            .expect("image stage is pending")
    );
    assert!(
        database
            .mark_scan_local_metadata_images_complete(&claimed_a.id)
            .await
            .expect("mark local images complete")
    );
    let claimed_a_images_completed_at: Option<i64> = sqlx::query_scalar(
        "SELECT images_completed_at FROM scan_local_metadata_batches WHERE id = ?",
    )
    .bind(&claimed_a.id)
    .fetch_one(database.pool())
    .await
    .expect("read image completion marker");
    assert!(claimed_a_images_completed_at.is_some());
    assert!(
        database
            .has_pending_scan_local_metadata_images("scan-job")
            .await
            .expect("another batch image stage remains pending")
    );
    assert!(
        database
            .complete_scan_local_metadata_batch(&claimed_a.id)
            .await
            .expect("complete")
    );
    assert!(
        !database
            .complete_scan_local_metadata_batch(&claimed_a.id)
            .await
            .expect("CAS complete")
    );

    let cancel_count = database
        .cancel_scan_local_metadata_batches("scan-job")
        .await
        .expect("cancel pending and running batches");
    assert_eq!(cancel_count, 3);
    assert!(
        !database
            .fail_scan_local_metadata_batch(&claimed_b.id, "temporary failure", Some(i64::MAX),)
            .await
            .expect("cancelled batch failure CAS"),
        "a cancelled running batch must reject later completion"
    );
    database
        .enqueue_scan_local_metadata_batch(NewScanLocalMetadataBatch {
            id: "retry-batch",
            job_id: "retry-job",
            library_root_id: &root_id,
            batch_sequence: 0,
            source_ids: &sources,
        })
        .await
        .expect("enqueue retry batch");
    let retryable = database
        .claim_next_scan_local_metadata_batch()
        .await
        .expect("claim retryable batch")
        .expect("retryable batch");
    assert_eq!(retryable.id, "retry-batch");
    assert!(
        database
            .mark_scan_local_metadata_images_complete(&retryable.id)
            .await
            .expect("mark image stage complete before NFO work")
    );
    assert!(
        database
            .fail_scan_local_metadata_batch(&retryable.id, "temporary failure", Some(i64::MAX))
            .await
            .expect("fail with delayed retry")
    );
    assert!(
        database
            .claim_next_scan_local_metadata_batch()
            .await
            .expect("deferred retry check")
            .is_none(),
        "a retry must not be claimable before next_attempt_at"
    );
    sqlx::query("UPDATE scan_local_metadata_batches SET next_attempt_at = 0 WHERE id = ?")
        .bind(&retryable.id)
        .execute(database.pool())
        .await
        .expect("make delayed retry due");
    let retried = database
        .claim_next_scan_local_metadata_batch()
        .await
        .expect("claim retry")
        .expect("due retry");
    assert_eq!(retried.id, retryable.id);
    assert_eq!(retried.attempts, 2);
    assert!(
        retried.images_completed_at.is_some(),
        "a retry retains its completed image stage"
    );
    assert!(
        !database
            .has_pending_scan_local_metadata_images("retry-job")
            .await
            .expect("completed image stage stays out of the pending queue")
    );
    assert!(
        database
            .complete_scan_local_metadata_batch(&retried.id)
            .await
            .expect("complete retry")
    );

    assert_eq!(
        database
            .cancel_scan_local_metadata_batches("retry-job")
            .await
            .expect("cancel completed job remainder"),
        0
    );
    let interrupted = database
        .enqueue_scan_local_metadata_batch(NewScanLocalMetadataBatch {
            id: "batch-e",
            job_id: "interrupted-job",
            library_root_id: &root_id,
            batch_sequence: 0,
            source_ids: &sources,
        })
        .await
        .expect("enqueue interrupted batch");
    assert!(interrupted);
    let running = database
        .claim_next_scan_local_metadata_batch()
        .await
        .expect("claim interrupted batch")
        .expect("running batch");
    assert_eq!(running.id, "batch-e");
    assert!(
        database
            .mark_scan_local_metadata_images_complete(&running.id)
            .await
            .expect("mark interrupted images complete")
    );
    assert!(
        !database
            .has_pending_scan_local_metadata_images("interrupted-job")
            .await
            .expect("persisted image completion")
    );
    assert_eq!(
        database
            .requeue_interrupted_scan_local_metadata_batches()
            .await
            .expect("recover interrupted batch"),
        1
    );
    let recovered = database
        .claim_next_scan_local_metadata_batch()
        .await
        .expect("claim recovered batch")
        .expect("recovered batch");
    assert_eq!(recovered.id, "batch-e");
    assert_eq!(recovered.attempts, 2);
    assert!(
        recovered.images_completed_at.is_some(),
        "interrupted work retains its completed image stage"
    );
    assert!(
        !database
            .has_pending_scan_local_metadata_images("interrupted-job")
            .await
            .expect("recovered batch does not repeat the image stage")
    );

    database.close().await;
}

#[tokio::test]
async fn scan_local_metadata_backfill_roots_register_in_one_query()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Backfill roots", LibraryKind::Movie, false)
        .await?;

    database.reset_query_count();
    assert_eq!(
        database.ensure_scan_local_metadata_backfill_roots().await?,
        0
    );
    assert_eq!(
        database.query_count(),
        1,
        "an empty root list uses one query"
    );

    for index in 0..4 {
        let root_path = temp_dir.path().join(format!("root-{index}"));
        std::fs::create_dir_all(&root_path)?;
        libraries
            .add_root(library.id, root_path.to_str().ok_or("non-UTF-8 root path")?)
            .await?;
    }

    database.reset_query_count();
    assert_eq!(
        database.ensure_scan_local_metadata_backfill_roots().await?,
        4
    );
    assert_eq!(database.query_count(), 1, "four roots use one query");
    let row_count: i64 = database
        .query_scalar("SELECT COUNT(*) FROM scan_local_metadata_backfills")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(row_count, 4);

    database.reset_query_count();
    assert_eq!(
        database.ensure_scan_local_metadata_backfill_roots().await?,
        0
    );
    assert_eq!(
        database.query_count(),
        1,
        "idempotent registration uses one query"
    );
    Ok(())
}

#[tokio::test]
async fn notification_deliveries_are_written_in_bounded_batches()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let mut destination_ids = Vec::new();
    for index in 0..205 {
        let id = format!("notification-destination-{index:03}");
        database
            .create_notification_destination(NewNotificationDestination {
                id: &id,
                name: &id,
                url: "https://example.test/webhook",
                enabled: true,
                allow_private_network: false,
                event_types_json: "[]",
                payload_format: "LUX",
                provider_plugin_id: "builtin.webhook",
                provider_config_json: "{}",
            })
            .await?;
        destination_ids.push(id);
    }

    let event_id = "notification-event-batch";
    let event = || NewNotificationEvent {
        id: event_id,
        event_type: "ITEM_ADDED",
        schema_version: 1,
        occurred_at: 1_800_000_000,
        dedupe_key: "notification-event-batch-key",
        payload_json: "{}",
    };
    sqlx::query(
        "CREATE TRIGGER reject_notification_delivery_batch
         BEFORE INSERT ON notification_deliveries
         WHEN NEW.destination_id = 'notification-destination-100'
         BEGIN
             SELECT RAISE(ABORT, 'forced delivery batch failure');
         END",
    )
    .execute(database.pool())
    .await?;
    assert!(
        database
            .insert_notification_event_with_deliveries(event(), &destination_ids)
            .await
            .is_err()
    );
    let rolled_back_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notification_events WHERE id = ?")
            .bind(event_id)
            .fetch_one(database.pool())
            .await?;
    let rolled_back_deliveries: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notification_deliveries WHERE event_id = ?")
            .bind(event_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(rolled_back_events, 0);
    assert_eq!(rolled_back_deliveries, 0);
    sqlx::query("DROP TRIGGER reject_notification_delivery_batch")
        .execute(database.pool())
        .await?;

    database.reset_query_count();
    assert!(
        database
            .insert_notification_event_with_deliveries(event(), &destination_ids)
            .await?
    );
    assert_eq!(
        database.query_count(),
        4,
        "one event plus three batches of at most 100 deliveries"
    );

    let delivery_rows: i64 = database
        .query_scalar("SELECT COUNT(*) FROM notification_deliveries WHERE event_id = ?")
        .bind(event_id)
        .fetch_one(database.pool())
        .await?;
    let pending_rows: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM notification_deliveries
             WHERE event_id = ? AND status = 'PENDING'",
        )
        .bind(event_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(delivery_rows, 205);
    assert_eq!(pending_rows, 205);

    database.reset_query_count();
    assert!(
        !database
            .insert_notification_event_with_deliveries(event(), &destination_ids)
            .await?
    );
    assert_eq!(
        database.query_count(),
        1,
        "deduped events skip delivery inserts"
    );

    database.reset_query_count();
    assert!(
        database
            .insert_notification_event_with_deliveries(
                NewNotificationEvent {
                    id: "notification-event-empty",
                    event_type: "ITEM_ADDED",
                    schema_version: 1,
                    occurred_at: 1_800_000_001,
                    dedupe_key: "notification-event-empty-key",
                    payload_json: "{}",
                },
                &[],
            )
            .await?
    );
    assert_eq!(
        database.query_count(),
        1,
        "empty destination list only inserts event"
    );

    let event_count: i64 = database
        .query_scalar("SELECT COUNT(*) FROM notification_events")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(event_count, 2);
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_notification_deliveries_are_written_in_bounded_batches()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_url = crate::config::DatabaseConfiguration::Postgres(admin_connection.clone())
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(
        &config,
        &DatabaseConfiguration::Postgres(PostgresConnection {
            database: database_name.clone(),
            ..admin_connection
        }),
    )
    .await?;
    let assertions = async {
        let mut destination_ids = Vec::new();
        for index in 0..205 {
            let id = format!("postgres-notification-destination-{index:03}");
            database
                .create_notification_destination(NewNotificationDestination {
                    id: &id,
                    name: &id,
                    url: "https://example.test/webhook",
                    enabled: true,
                    allow_private_network: false,
                    event_types_json: "[]",
                    payload_format: "LUX",
                    provider_plugin_id: "builtin.webhook",
                    provider_config_json: "{}",
                })
                .await?;
            destination_ids.push(id);
        }

        let event_id = "postgres-notification-event-batch";
        let event = || NewNotificationEvent {
            id: event_id,
            event_type: "ITEM_ADDED",
            schema_version: 1,
            occurred_at: 1_800_000_000,
            dedupe_key: "postgres-notification-event-batch-key",
            payload_json: "{}",
        };
        database.reset_query_count();
        assert!(
            database
                .insert_notification_event_with_deliveries(event(), &destination_ids)
                .await?
        );
        assert_eq!(database.query_count(), 4);
        let delivery_rows: i64 = database
            .query_scalar("SELECT COUNT(*) FROM notification_deliveries WHERE event_id = ?")
            .bind(event_id)
            .fetch_one(database.pool())
            .await?;
        assert_eq!(delivery_rows, 205);

        database.reset_query_count();
        assert!(
            !database
                .insert_notification_event_with_deliveries(event(), &destination_ids)
                .await?
        );
        assert_eq!(database.query_count(), 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    assertions?;
    drop_database?;
    Ok(())
}

#[tokio::test]
async fn progressive_scan_metadata_backfill_is_bounded_recoverable_and_root_scoped() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root)
        .await
        .expect("media root");
    for name in [
        "First.Movie.2023.mkv",
        "Second.Movie.2024.mkv",
        "Third.Movie.2025.mkv",
        "Fourth.Movie.2026.mkv",
    ] {
        tokio::fs::write(media_root.join(name), b"video")
            .await
            .expect("movie file");
    }
    tokio::fs::write(media_root.join("notes.txt"), b"not media")
        .await
        .expect("non-media file");

    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Backfill", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root = libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root");
    let root_id = root.root.id.to_string();
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("index media files");

    let expected_ids = database
        .query_scalar::<String>(
            "SELECT entry.id
             FROM filesystem_entries entry
             JOIN media_sources source ON source.filesystem_entry_id = entry.id
             JOIN media_items item ON item.id = source.item_id
             WHERE entry.library_root_id = ? AND entry.entry_kind = 'FILE'
               AND entry.is_missing = 0 AND item.removed_at IS NULL
             ORDER BY entry.id",
        )
        .bind(&root_id)
        .fetch_all(database.pool())
        .await
        .expect("read media source entry ids");
    assert_eq!(expected_ids.len(), 4, "the text file is not a media source");
    sqlx::query("UPDATE filesystem_entries SET is_missing = 1 WHERE id = ?")
        .bind(&expected_ids[0])
        .execute(database.pool())
        .await
        .expect("mark one entry missing");
    sqlx::query(
        "UPDATE media_items SET removed_at = unixepoch()
         WHERE id = (SELECT item_id FROM media_sources WHERE filesystem_entry_id = ?)",
    )
    .bind(&expected_ids[1])
    .execute(database.pool())
    .await
    .expect("remove one source item");

    assert_eq!(
        database
            .ensure_scan_local_metadata_backfill_roots()
            .await
            .expect("register existing roots"),
        1
    );
    assert!(
        !database
            .ensure_scan_local_metadata_backfill_root(&root_id)
            .await
            .expect("register existing root again"),
        "root registration is idempotent"
    );
    assert_eq!(
        database
            .ensure_scan_local_metadata_backfill_roots()
            .await
            .expect("register roots again"),
        0
    );
    assert!(
        database
            .claim_next_scan_local_metadata_backfill_page(0)
            .await
            .is_err(),
        "a zero-sized page is invalid"
    );
    assert!(
        database
            .claim_next_scan_local_metadata_backfill_page(17)
            .await
            .is_err(),
        "the public storage limit caps backfill windows"
    );

    let first = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await
        .expect("claim first page")
        .expect("first page exists");
    assert_eq!(first.library_root_id, root_id);
    assert_eq!(first.entry_ids.len(), 1);
    assert!(first.has_more);
    assert_eq!(first.cursor_entry_id, None);
    assert_eq!(first.attempts, 1);
    assert_eq!(first.entry_ids[0], expected_ids[2]);

    assert!(
        database
            .fail_scan_local_metadata_backfill_page(
                &first,
                "temporary local failure",
                Some(i64::MAX),
            )
            .await
            .expect("persist page failure")
    );
    assert!(
        database
            .claim_next_scan_local_metadata_backfill_page(1)
            .await
            .expect("check retry delay")
            .is_none(),
        "failed page remains deferred"
    );
    sqlx::query(
        "UPDATE scan_local_metadata_backfills SET next_attempt_at = 0 WHERE library_root_id = ?",
    )
    .bind(&root_id)
    .execute(database.pool())
    .await
    .expect("make retry due");
    let retried = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await
        .expect("claim retry")
        .expect("retry page exists");
    assert_eq!(retried.entry_ids, first.entry_ids);
    assert_eq!(retried.cursor_entry_id, first.cursor_entry_id);
    assert_eq!(retried.attempts, 2);
    assert!(
        !database
            .complete_scan_local_metadata_backfill_page(&first)
            .await
            .expect("reject stale worker completion"),
        "an old attempt cannot advance the retry cursor"
    );
    database
        .query(
            "UPDATE scan_local_metadata_backfills SET cursor_entry_id = ?
             WHERE library_root_id = ?",
        )
        .bind(&expected_ids[0])
        .bind(&root_id)
        .execute(database.pool())
        .await
        .expect("simulate a cursor change during the claimed attempt");
    assert!(
        !database
            .complete_scan_local_metadata_backfill_page(&retried)
            .await
            .expect("reject mismatched cursor CAS"),
        "a page cannot advance a cursor that changed during its attempt"
    );
    database
        .query(
            "UPDATE scan_local_metadata_backfills SET cursor_entry_id = NULL
             WHERE library_root_id = ?",
        )
        .bind(&root_id)
        .execute(database.pool())
        .await
        .expect("restore the claimed page cursor");
    assert!(
        database
            .complete_scan_local_metadata_backfill_page(&retried)
            .await
            .expect("advance first page")
    );

    let second = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await
        .expect("claim second page")
        .expect("second page exists");
    assert_eq!(
        second.cursor_entry_id.as_deref(),
        Some(first.entry_ids[0].as_str())
    );
    assert_eq!(second.entry_ids, vec![expected_ids[3].clone()]);
    assert!(!second.has_more);
    assert_eq!(
        database
            .requeue_interrupted_scan_local_metadata_backfills()
            .await
            .expect("recover interrupted page"),
        1
    );
    let recovered = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await
        .expect("claim recovered page")
        .expect("recovered page exists");
    assert_eq!(recovered.entry_ids, second.entry_ids);
    assert_eq!(recovered.cursor_entry_id, second.cursor_entry_id);
    assert_eq!(recovered.attempts, 4);
    assert!(
        database
            .complete_scan_local_metadata_backfill_page(&recovered)
            .await
            .expect("complete backfill")
    );
    let completed_status: String = database
        .query_scalar("SELECT status FROM scan_local_metadata_backfills WHERE library_root_id = ?")
        .bind(&root_id)
        .fetch_one(database.pool())
        .await
        .expect("read completed status");
    assert_eq!(completed_status, "COMPLETED");
    assert!(
        database
            .claim_next_scan_local_metadata_backfill_page(1)
            .await
            .expect("check completed root")
            .is_none()
    );

    let empty_root_path = temp_dir.path().join("Empty");
    tokio::fs::create_dir_all(&empty_root_path)
        .await
        .expect("empty root directory");
    let empty_library = libraries
        .create_library("Empty backfill", LibraryKind::Movie, false)
        .await
        .expect("empty library");
    let empty_root = libraries
        .add_root(
            empty_library.id,
            empty_root_path.to_str().expect("empty root path"),
        )
        .await
        .expect("empty library root");
    let empty_root_id = empty_root.root.id.to_string();
    assert!(
        database
            .ensure_scan_local_metadata_backfill_root(&empty_root_id)
            .await
            .expect("register empty root")
    );

    let populated_root_path = temp_dir.path().join("Populated");
    tokio::fs::create_dir_all(&populated_root_path)
        .await
        .expect("populated root directory");
    tokio::fs::write(populated_root_path.join("Later.Movie.2026.mkv"), b"video")
        .await
        .expect("populated root media file");
    let populated_library = libraries
        .create_library("Populated backfill", LibraryKind::Movie, false)
        .await
        .expect("populated library");
    let populated_root = libraries
        .add_root(
            populated_library.id,
            populated_root_path.to_str().expect("populated root path"),
        )
        .await
        .expect("populated library root");
    let populated_root_id = populated_root.root.id.to_string();
    LibraryScanner::new(database.clone())
        .scan_movie_library(populated_library.id)
        .await
        .expect("index populated root");
    assert!(
        database
            .ensure_scan_local_metadata_backfill_root(&populated_root_id)
            .await
            .expect("register populated root")
    );
    database
        .query(
            "UPDATE scan_local_metadata_backfills SET updated_at = 0
             WHERE library_root_id = ?",
        )
        .bind(&empty_root_id)
        .execute(database.pool())
        .await
        .expect("order empty root first");
    database
        .query(
            "UPDATE scan_local_metadata_backfills SET updated_at = 1
             WHERE library_root_id = ?",
        )
        .bind(&populated_root_id)
        .execute(database.pool())
        .await
        .expect("order populated root second");
    let populated_page = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await
        .expect("skip empty root and keep claiming")
        .expect("populated root page remains claimable");
    assert_eq!(populated_page.library_root_id, populated_root_id);
    assert_eq!(populated_page.entry_ids.len(), 1);
    assert!(
        database
            .complete_scan_local_metadata_backfill_page(&populated_page)
            .await
            .expect("complete populated root")
    );
    let empty_status: String = database
        .query_scalar("SELECT status FROM scan_local_metadata_backfills WHERE library_root_id = ?")
        .bind(&empty_root_id)
        .fetch_one(database.pool())
        .await
        .expect("read empty-root status");
    assert_eq!(empty_status, "COMPLETED");

    database
        .query("DELETE FROM library_roots WHERE id = ?")
        .bind(&empty_root_id)
        .execute(database.pool())
        .await
        .expect("delete empty root");
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM scan_local_metadata_backfills WHERE library_root_id = ?",
            )
            .bind(&empty_root_id)
            .fetch_one(database.pool())
            .await
            .expect("check root cascade"),
        0
    );

    database.close().await;
}

#[tokio::test]
async fn progressive_scan_metadata_completeness_is_versioned_and_paged() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Versioned Movie (2025)");
    let second_movie_dir = media_root.join("Second Movie (2025)");
    tokio::fs::create_dir_all(&movie_dir)
        .await
        .expect("movie directory");
    tokio::fs::create_dir_all(&second_movie_dir)
        .await
        .expect("second movie directory");
    tokio::fs::write(movie_dir.join("Versioned.Movie.2025.mkv"), b"video")
        .await
        .expect("movie file");
    tokio::fs::write(second_movie_dir.join("Second.Movie.2025.mkv"), b"video")
        .await
        .expect("second movie file");
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Completeness", LibraryKind::Movie, false)
        .await
        .expect("library");
    libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("index movie");
    let item_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE'",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await
    .expect("indexed items");
    assert_eq!(item_ids.len(), 2);
    let item_id = item_ids[0].clone();
    let second_item_id = item_ids[1].clone();

    let old_fingerprint = b"source-version-1";
    let current_fingerprint = b"source-version-2";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await
            .expect("prepare poster check")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await
            .expect("claim poster check")
    );
    assert_eq!(
        database
            .requeue_interrupted_item_metadata_completeness_checks()
            .await
            .expect("recover interrupted check"),
        1
    );
    assert_eq!(
        database
            .requeue_interrupted_item_metadata_completeness_checks()
            .await
            .expect("recovery is idempotent"),
        0
    );
    assert!(
        !database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                old_fingerprint,
                true,
                999,
            )
            .await
            .expect("recovered old worker CAS")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await
            .expect("reclaim after restart")
    );
    assert!(
        !database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await
            .expect("claim once")
    );
    assert!(
        !database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await
            .expect("same running input is idempotent")
    );

    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", current_fingerprint)
            .await
            .expect("replace stale input version")
    );
    let pending = database
        .find_item_metadata_completeness(&item_id, "POSTER")
        .await
        .expect("read reset status")
        .expect("completeness row");
    assert_eq!(pending.local_state, "PENDING");
    assert_eq!(pending.is_missing, None);
    assert_eq!(
        pending.input_fingerprint.as_deref(),
        Some(current_fingerprint.as_slice())
    );
    assert!(
        !database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                old_fingerprint,
                true,
                1_000,
            )
            .await
            .expect("stale worker result CAS")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", current_fingerprint)
            .await
            .expect("claim current input")
    );
    assert!(
        database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                current_fingerprint,
                true,
                1_001,
            )
            .await
            .expect("confirm missing poster")
    );
    let ready = database
        .find_item_metadata_completeness(&item_id, "POSTER")
        .await
        .expect("read ready state")
        .expect("ready completeness");
    assert_eq!(ready.local_state, "READY");
    assert_eq!(ready.is_missing, Some(true));
    assert_eq!(ready.checked_at, Some(1_001));
    assert!(
        !database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", current_fingerprint)
            .await
            .expect("same ready input is idempotent")
    );

    let missing = database
        .list_confirmed_missing_metadata("POSTER", None, 1)
        .await
        .expect("list confirmed missing posters");
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].item_id, item_id);
    assert!(
        database
            .list_confirmed_missing_metadata("POSTER", Some(&item_id), 1)
            .await
            .expect("page after item")
            .is_empty()
    );
    assert!(
        database
            .list_confirmed_missing_metadata("POSTER", None, 101)
            .await
            .is_err()
    );

    let failed_fingerprint = b"source-version-3";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", failed_fingerprint)
            .await
            .expect("prepare next version")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", failed_fingerprint)
            .await
            .expect("claim next version")
    );
    assert!(
        database
            .fail_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                failed_fingerprint,
                Some(4_000_000_000),
                "local read failed",
            )
            .await
            .expect("record local read failure")
    );
    assert!(
        !database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", failed_fingerprint,)
            .await
            .expect("respect completeness retry backoff")
    );
    database
        .query(
            "UPDATE item_metadata_completeness
             SET retry_after = 0
             WHERE item_id = ? AND capability = 'POSTER'",
        )
        .bind(&item_id)
        .execute(database.pool())
        .await
        .expect("expire completeness retry backoff");
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", failed_fingerprint,)
            .await
            .expect("claim completeness after retry backoff")
    );
    assert!(
        database
            .list_confirmed_missing_metadata("POSTER", None, 1)
            .await
            .expect("failed item must not be reported missing")
            .is_empty()
    );

    let backdrop_fingerprint = b"backdrop-input";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "BACKDROP", backdrop_fingerprint)
            .await
            .expect("prepare backdrop")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "BACKDROP", backdrop_fingerprint)
            .await
            .expect("claim backdrop")
    );
    assert!(
        database
            .cancel_item_metadata_completeness_check(&item_id, "BACKDROP", backdrop_fingerprint)
            .await
            .expect("cancel backdrop")
    );
    assert!(
        !database
            .finish_item_metadata_completeness_check(
                &item_id,
                "BACKDROP",
                backdrop_fingerprint,
                true,
                1_003,
            )
            .await
            .expect("cancelled result CAS")
    );

    let current_fingerprint = b"source-version-4";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", current_fingerprint)
            .await
            .expect("prepare current poster state")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", current_fingerprint)
            .await
            .expect("claim current poster state")
    );
    assert!(
        database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                current_fingerprint,
                true,
                1_004,
            )
            .await
            .expect("confirm first missing poster")
    );
    assert!(
        database
            .prepare_item_metadata_completeness_check(&second_item_id, "POSTER", b"second-poster")
            .await
            .expect("prepare second poster")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&second_item_id, "POSTER", b"second-poster")
            .await
            .expect("claim second poster")
    );
    assert!(
        database
            .finish_item_metadata_completeness_check(
                &second_item_id,
                "POSTER",
                b"second-poster",
                true,
                1_005,
            )
            .await
            .expect("confirm second missing poster")
    );
    assert!(
        database
            .prepare_item_metadata_completeness_check(&second_item_id, "BACKDROP", b"backdrop")
            .await
            .expect("prepare available backdrop")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&second_item_id, "BACKDROP", b"backdrop")
            .await
            .expect("claim available backdrop")
    );
    assert!(
        database
            .finish_item_metadata_completeness_check(
                &second_item_id,
                "BACKDROP",
                b"backdrop",
                false,
                1_006,
            )
            .await
            .expect("confirm available backdrop")
    );

    let poster_page_one = database
        .list_confirmed_missing_metadata("POSTER", None, 1)
        .await
        .expect("first missing poster page");
    assert_eq!(poster_page_one.len(), 1);
    let poster_page_two = database
        .list_confirmed_missing_metadata("POSTER", Some(&poster_page_one[0].item_id), 1)
        .await
        .expect("second missing poster page");
    assert_eq!(poster_page_two.len(), 1);
    assert_ne!(poster_page_one[0].item_id, poster_page_two[0].item_id);
    assert!(
        database
            .list_confirmed_missing_metadata("POSTER", Some(&poster_page_two[0].item_id), 1)
            .await
            .expect("end of missing poster pages")
            .is_empty()
    );
    assert!(
        database
            .list_confirmed_missing_metadata("BACKDROP", None, 1)
            .await
            .expect("available backdrop is not missing")
            .is_empty()
    );

    database.close().await;
}

#[tokio::test]
async fn local_metadata_completeness_claim_result_and_enqueue_are_atomic() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Atomic Completeness (2025)");
    tokio::fs::create_dir_all(&movie_dir)
        .await
        .expect("movie directory");
    tokio::fs::write(movie_dir.join("Atomic.Completeness.2025.mkv"), b"video")
        .await
        .expect("movie file");

    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Atomic completeness", LibraryKind::Movie, false)
        .await
        .expect("library");
    libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("index movie");
    let library_id = library.id.to_string();
    let item_id = database
        .query_scalar::<String>(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE'",
        )
        .bind(&library_id)
        .fetch_one(database.pool())
        .await
        .expect("movie item");
    let results = [
        NewItemMetadataCompletenessResult {
            item_id: &item_id,
            capability: "POSTER",
            input_fingerprint: b"atomic-completeness-v1",
            is_missing: true,
            checked_at: 1,
        },
        NewItemMetadataCompletenessResult {
            item_id: &item_id,
            capability: "METADATA",
            input_fingerprint: b"atomic-completeness-v1",
            is_missing: false,
            checked_at: 1,
        },
    ];
    let eligible_item_ids = [item_id.clone()];

    sqlx::query(
        "CREATE TRIGGER reject_atomic_fill_missing_item
         BEFORE INSERT ON metadata_reidentify_job_items
         WHEN EXISTS (
             SELECT 1 FROM metadata_reidentify_jobs
             WHERE id = NEW.job_id AND mode = 'FILL_MISSING'
         )
         BEGIN
             SELECT RAISE(ABORT, 'forced fill-missing enqueue failure');
         END",
    )
    .execute(database.pool())
    .await
    .expect("install enqueue failure trigger");

    assert!(
        database
            .complete_local_metadata_completeness_batch_with_policy(
                &library_id,
                &results,
                &eligible_item_ids,
                MetadataAutoMatchPolicy::Enabled,
            )
            .await
            .is_err(),
        "enqueue failure should abort the combined transaction"
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM item_metadata_completeness
                 WHERE item_id = ?",
            )
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("count completeness rows after rollback"),
        0,
        "failed transaction must not leave a RUNNING or READY claim"
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM metadata_reidentify_jobs
                 WHERE library_id = ? AND mode = 'FILL_MISSING'",
            )
            .bind(&library_id)
            .fetch_one(database.pool())
            .await
            .expect("count fill-missing jobs after rollback"),
        0
    );

    sqlx::query("DROP TRIGGER reject_atomic_fill_missing_item")
        .execute(database.pool())
        .await
        .expect("remove enqueue failure trigger");
    let independent_database = Database::connect(&config)
        .await
        .expect("independent database handle");
    let (retry_a, retry_b) = tokio::join!(
        database.complete_local_metadata_completeness_batch_with_policy(
            &library_id,
            &results,
            &eligible_item_ids,
            MetadataAutoMatchPolicy::Enabled,
        ),
        independent_database.complete_local_metadata_completeness_batch_with_policy(
            &library_id,
            &results,
            &eligible_item_ids,
            MetadataAutoMatchPolicy::Enabled,
        ),
    );
    let retry_a = retry_a.expect("first retry should commit");
    let retry_b = retry_b.expect("duplicate retry should commit without changes");
    assert_eq!(retry_a.updated_count + retry_b.updated_count, 2);
    assert_eq!(
        retry_a.scheduled_job_ids.len() + retry_b.scheduled_job_ids.len(),
        1
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM item_metadata_completeness
                 WHERE item_id = ? AND local_state = 'READY'",
            )
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("read completed completeness states"),
        2
    );
    let job_id = retry_a
        .scheduled_job_ids
        .first()
        .or_else(|| retry_b.scheduled_job_ids.first())
        .expect("one concurrent request schedules a job");
    fail_fill_missing_job_with_unavailable_provider(&database, job_id, &item_id)
        .await
        .expect("mark the first job deferred for provider unavailability");
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET automatic_retry_after = unixepoch() - 1
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(job_id)
    .bind(&item_id)
    .execute(database.pool())
    .await
    .expect("make the provider retry due");
    let disabled_retry = database
        .complete_local_metadata_completeness_batch_with_policy(
            &library_id,
            &results,
            &eligible_item_ids,
            MetadataAutoMatchPolicy::Disabled,
        )
        .await
        .expect("disabled auto-match must preserve due retry without enqueueing");
    assert_eq!(disabled_retry.updated_count, 0);
    assert!(disabled_retry.scheduled_job_ids.is_empty());
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
                 WHERE job_id = ? AND item_id = ?",
            )
            .bind(job_id)
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("read retry state after disabled policy"),
        0
    );
    sqlx::query(
        "CREATE TRIGGER reject_atomic_fill_missing_retry_item
         BEFORE INSERT ON metadata_reidentify_job_items
         WHEN EXISTS (
             SELECT 1 FROM metadata_reidentify_jobs
             WHERE id = NEW.job_id AND mode = 'FILL_MISSING'
         )
         BEGIN
             SELECT RAISE(ABORT, 'forced retried fill-missing enqueue failure');
         END",
    )
    .execute(database.pool())
    .await
    .expect("install retry enqueue failure trigger");
    assert!(
        database
            .complete_local_metadata_completeness_batch_with_policy(
                &library_id,
                &results,
                &eligible_item_ids,
                MetadataAutoMatchPolicy::Enabled,
            )
            .await
            .is_err(),
        "retry enqueue failure should abort retry consumption"
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
                 WHERE job_id = ? AND item_id = ?",
            )
            .bind(job_id)
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("read retry state after rollback"),
        0,
        "failed retry enqueue must restore the deferred retry"
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM metadata_reidentify_jobs
                 WHERE library_id = ? AND mode = 'FILL_MISSING'",
            )
            .bind(&library_id)
            .fetch_one(database.pool())
            .await
            .expect("count jobs after retry rollback"),
        1,
        "failed retry enqueue must not leave a partial job"
    );
    sqlx::query("DROP TRIGGER reject_atomic_fill_missing_retry_item")
        .execute(database.pool())
        .await
        .expect("remove retry enqueue failure trigger");
    let enabled_retry = database
        .complete_local_metadata_completeness_batch_with_policy(
            &library_id,
            &results,
            &eligible_item_ids,
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("enabled auto-match must schedule the due provider retry");
    assert_eq!(enabled_retry.updated_count, 0);
    assert_eq!(enabled_retry.scheduled_job_ids.len(), 1);
    assert_ne!(enabled_retry.scheduled_job_ids[0], *job_id);
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
                 WHERE job_id = ? AND item_id = ?",
            )
            .bind(job_id)
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("read consumed retry state"),
        1
    );
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'COMPLETED' WHERE job_id = ?",
    )
    .bind(&enabled_retry.scheduled_job_ids[0])
    .execute(database.pool())
    .await
    .expect("complete the retried fill-missing item");
    sqlx::query(
        "UPDATE metadata_reidentify_jobs
         SET status = 'COMPLETED', processed_count = total_count WHERE id = ?",
    )
    .bind(&enabled_retry.scheduled_job_ids[0])
    .execute(database.pool())
    .await
    .expect("complete the retried fill-missing job");
    let unchanged = database
        .complete_local_metadata_completeness_batch_with_policy(
            &library_id,
            &results,
            &eligible_item_ids,
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("unchanged ready completeness should be a no-op");
    assert_eq!(unchanged.updated_count, 0);
    assert!(unchanged.scheduled_job_ids.is_empty());
    independent_database.close().await;
    database.close().await;
}

#[tokio::test]
async fn progressive_scan_metadata_completeness_batches_are_atomic_and_versioned() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Batch Claims (2024)");
    tokio::fs::create_dir_all(&movie_dir)
        .await
        .expect("movie directory");
    tokio::fs::write(movie_dir.join("Batch.Claims.2024.mkv"), b"movie")
        .await
        .expect("movie file");
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Completeness claims", LibraryKind::Movie, false)
        .await
        .expect("library");
    libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("index item");
    let item_id = database
        .query_scalar::<String>(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE'",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await
        .expect("movie item");
    let fingerprint_v1 = b"batch-input-v1";
    let checks = [
        NewItemMetadataCompletenessCheck {
            item_id: &item_id,
            capability: "POSTER",
            input_fingerprint: fingerprint_v1,
        },
        NewItemMetadataCompletenessCheck {
            item_id: &item_id,
            capability: "METADATA",
            input_fingerprint: fingerprint_v1,
        },
    ];

    let (claimed_a, claimed_b) = tokio::join!(
        database.prepare_and_claim_item_metadata_completeness_checks(&checks),
        database.prepare_and_claim_item_metadata_completeness_checks(&checks),
    );
    let claimed_a = claimed_a.expect("first batch claim");
    let claimed_b = claimed_b.expect("concurrent batch claim");
    assert_eq!(claimed_a.len() + claimed_b.len(), 2);
    assert!(
        claimed_a.is_empty() || claimed_b.is_empty(),
        "concurrent workers cannot claim one input twice"
    );

    let library_id = library.id.to_string();
    let ready_results = [
        NewItemMetadataCompletenessResult {
            item_id: &item_id,
            capability: "POSTER",
            input_fingerprint: fingerprint_v1,
            is_missing: true,
            checked_at: 1_000,
        },
        NewItemMetadataCompletenessResult {
            item_id: &item_id,
            capability: "METADATA",
            input_fingerprint: fingerprint_v1,
            is_missing: false,
            checked_at: 1_000,
        },
    ];
    let completed = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &ready_results, &[])
        .await
        .expect("save local results without dispatch");
    assert_eq!(completed.updated_count, 2);
    assert!(completed.scheduled_job_ids.is_empty());
    assert!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&checks)
            .await
            .expect("same completed input stays ready")
            .is_empty()
    );

    let fingerprint_v2 = b"batch-input-v2";
    let replacement = [NewItemMetadataCompletenessCheck {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: fingerprint_v2,
    }];
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&replacement)
            .await
            .expect("replace changed input version"),
        vec![0]
    );
    assert!(
        !database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                fingerprint_v1,
                true,
                1_001,
            )
            .await
            .expect("reject stale worker result")
    );
    let replacement_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: fingerprint_v2,
        is_missing: false,
        checked_at: 1_002,
    }];
    assert_eq!(
        database
            .complete_local_metadata_and_enqueue_fill_missing(&library_id, &replacement_result, &[])
            .await
            .expect("complete replacement version")
            .updated_count,
        1
    );

    let retry_fingerprint = b"batch-retry-input";
    let retry_check = [NewItemMetadataCompletenessCheck {
        item_id: &item_id,
        capability: "BACKDROP",
        input_fingerprint: retry_fingerprint,
    }];
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&retry_check)
            .await
            .expect("claim retry check"),
        vec![0]
    );
    assert!(
        database
            .fail_item_metadata_completeness_check(
                &item_id,
                "BACKDROP",
                retry_fingerprint,
                Some(2_000),
                "temporary local error",
            )
            .await
            .expect("fail local capability")
    );
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&retry_check)
            .await
            .expect("retry failed check"),
        vec![0]
    );
    let retry_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "BACKDROP",
        input_fingerprint: retry_fingerprint,
        is_missing: false,
        checked_at: 2_001,
    }];
    assert_eq!(
        database
            .complete_local_metadata_and_enqueue_fill_missing(&library_id, &retry_result, &[])
            .await
            .expect("complete failed check retry")
            .updated_count,
        1
    );

    let duplicate_checks = [checks[0], checks[0]];
    assert!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&duplicate_checks)
            .await
            .is_err()
    );
    let empty_fingerprint = [NewItemMetadataCompletenessCheck {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: &[],
    }];
    assert!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&empty_fingerprint)
            .await
            .is_err()
    );
    let oversized_fingerprint = vec![0; 257];
    let oversized_fingerprint_check = [NewItemMetadataCompletenessCheck {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: &oversized_fingerprint,
    }];
    assert!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&oversized_fingerprint_check)
            .await
            .is_err()
    );
    let oversized_checks = vec![checks[0]; 513];
    assert!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&oversized_checks)
            .await
            .is_err()
    );
    database.close().await;
}

#[tokio::test]
async fn metadata_completeness_claims_use_bounded_sql_batches() {
    const CHECK_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Completeness batch", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    let item_ids = (0..CHECK_COUNT)
        .map(|index| format!("completeness-batch-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    let checks = item_ids
        .iter()
        .map(|item_id| NewItemMetadataCompletenessCheck {
            item_id,
            capability: "POSTER",
            input_fingerprint: b"completeness-batch-v1",
        })
        .collect::<Vec<_>>();

    database.reset_query_count();
    let claimed = database
        .prepare_and_claim_item_metadata_completeness_checks(&checks)
        .await
        .expect("claim completeness batch");
    assert_eq!(database.query_count(), 6);
    assert_eq!(claimed, (0..CHECK_COUNT).collect::<Vec<_>>());
    let running_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM item_metadata_completeness WHERE local_state = 'RUNNING'",
    )
    .fetch_one(database.pool())
    .await
    .expect("running completeness count");
    assert_eq!(running_count, CHECK_COUNT as i64);

    database.close().await;
}

#[tokio::test]
async fn metadata_completeness_results_use_bounded_update_batches() {
    const RESULT_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Completeness result batch", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    let item_ids = (0..RESULT_COUNT)
        .map(|index| format!("completeness-result-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    let checks = item_ids
        .iter()
        .map(|item_id| NewItemMetadataCompletenessCheck {
            item_id,
            capability: "POSTER",
            input_fingerprint: b"completeness-result-v1",
        })
        .collect::<Vec<_>>();
    database
        .prepare_and_claim_item_metadata_completeness_checks(&checks)
        .await
        .expect("claim completeness results");
    let results = item_ids
        .iter()
        .enumerate()
        .map(|(index, item_id)| NewItemMetadataCompletenessResult {
            item_id,
            capability: "POSTER",
            input_fingerprint: b"completeness-result-v1",
            is_missing: index % 2 == 0,
            checked_at: 10,
        })
        .collect::<Vec<_>>();

    database.reset_query_count();
    let commit = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &results,
            &[],
            MetadataAutoMatchPolicy::Disabled,
        )
        .await
        .expect("complete completeness results");
    assert_eq!(database.query_count(), 4);
    assert_eq!(commit.updated_count, RESULT_COUNT);
    let ready_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM item_metadata_completeness WHERE local_state = 'READY'",
    )
    .fetch_one(database.pool())
    .await
    .expect("ready completeness count");
    let missing_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM item_metadata_completeness
         WHERE local_state = 'READY' AND is_missing = 1",
    )
    .fetch_one(database.pool())
    .await
    .expect("missing completeness count");
    assert_eq!(ready_count, RESULT_COUNT as i64);
    assert_eq!(missing_count, RESULT_COUNT.div_ceil(2) as i64);

    database.close().await;
}

#[tokio::test]
async fn local_metadata_due_retry_candidates_use_bounded_query_batches() {
    const ITEM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Completeness retry batches", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    let item_ids = (0..ITEM_COUNT)
        .map(|index| format!("completeness-retry-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    let results = item_ids
        .iter()
        .map(|item_id| NewItemMetadataCompletenessResult {
            item_id,
            capability: "POSTER",
            input_fingerprint: b"completeness-retry-batch-v1",
            is_missing: false,
            checked_at: 10,
        })
        .collect::<Vec<_>>();

    database.reset_query_count();
    let commit = database
        .complete_local_metadata_completeness_batch_with_policy(
            &library_id,
            &results,
            &item_ids,
            MetadataAutoMatchPolicy::Disabled,
        )
        .await
        .expect("complete a candidate page larger than one SQL chunk");

    assert_eq!(commit.updated_count, ITEM_COUNT);
    assert_eq!(database.query_count(), 13);
    database.close().await;
}

#[tokio::test]
async fn user_library_order_is_replaced_in_bounded_batches() {
    const LIBRARY_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user_id = "library-order-batch-user";
    database
        .insert_user(
            user_id,
            user_id,
            "Library order user",
            "test-hash",
            false,
            true,
        )
        .await
        .expect("user");
    let library_ids = (0..LIBRARY_COUNT)
        .map(|index| format!("library-order-batch-{index:03}"))
        .collect::<Vec<_>>();
    for library_id in &library_ids {
        sqlx::query("INSERT INTO libraries (id, name, kind) VALUES (?, ?, 'MOVIE')")
            .bind(library_id)
            .bind(library_id)
            .execute(database.pool())
            .await
            .expect("library");
    }

    database.reset_query_count();
    database
        .replace_user_library_order(user_id, &library_ids)
        .await
        .expect("replace library order");
    assert_eq!(database.query_count(), 4);
    let stored = database
        .user_library_order(user_id)
        .await
        .expect("library order");
    assert_eq!(stored, library_ids);
    let positions: Vec<i64> = sqlx::query_scalar(
        "SELECT position FROM user_library_order WHERE user_id = ? ORDER BY position",
    )
    .bind(user_id)
    .fetch_all(database.pool())
    .await
    .expect("stored positions");
    assert_eq!(positions, (0..LIBRARY_COUNT as i64).collect::<Vec<_>>());

    let mut invalid_ids = library_ids.clone();
    invalid_ids[LIBRARY_COUNT - 1] = invalid_ids[0].clone();
    database.reset_query_count();
    assert!(
        database
            .replace_user_library_order(user_id, &invalid_ids)
            .await
            .is_err()
    );
    assert_eq!(database.query_count(), 4);
    assert_eq!(
        database
            .user_library_order(user_id)
            .await
            .expect("rollback order"),
        library_ids
    );

    database.reset_query_count();
    database
        .replace_user_library_order(user_id, &[])
        .await
        .expect("clear library order");
    assert_eq!(database.query_count(), 1);
    assert!(
        database
            .user_library_order(user_id)
            .await
            .expect("cleared library order")
            .is_empty()
    );
    database.close().await;
}

#[tokio::test]
async fn image_source_urls_are_checked_with_one_bounded_query() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Image source URL lookup", LibraryKind::Movie, false)
        .await
        .expect("library");
    database
        .query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('image-url-batch-item', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await
        .expect("media item");
    for (index, source_url) in [
        (0, "https://images.example/old-1"),
        (1, "https://images.example/old-2"),
    ] {
        database
            .query(
                "INSERT INTO item_images (
                    id, item_id, image_type, image_index, local_path, source, source_url
                 ) VALUES (?, 'image-url-batch-item', 'FANART', ?, ?, 'TMDB', ?)",
            )
            .bind(format!("image-{index}"))
            .bind(index)
            .bind(format!("/images/{index}.jpg"))
            .bind(source_url)
            .execute(database.pool())
            .await
            .expect("seed indexed image URL");
    }

    let urls = vec![
        "https://images.example/old-1".to_owned(),
        "https://images.example/new-1".to_owned(),
        "https://images.example/old-2".to_owned(),
        "https://images.example/new-2".to_owned(),
    ];
    database.reset_query_count();
    let existing = database
        .list_existing_item_image_source_urls("image-url-batch-item", "FANART", &urls)
        .await
        .expect("lookup indexed image URLs");
    assert_eq!(database.query_count(), 1);
    assert_eq!(
        existing,
        std::collections::HashSet::from([
            "https://images.example/old-1".to_owned(),
            "https://images.example/old-2".to_owned(),
        ])
    );

    database.reset_query_count();
    assert!(
        database
            .list_existing_item_image_source_urls("image-url-batch-item", "FANART", &[])
            .await
            .expect("empty image URL batch")
            .is_empty()
    );
    assert_eq!(database.query_count(), 0);
    database.close().await;
}

#[tokio::test]
async fn local_item_image_batch_is_bounded_idempotent_and_atomic() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    for directory in ["Batch First (2024)", "Batch Second (2024)"] {
        let movie_dir = media_root.join(directory);
        tokio::fs::create_dir_all(&movie_dir)
            .await
            .expect("movie directory");
        tokio::fs::write(
            movie_dir.join(format!("{}.mkv", directory.replace(' ', "."))),
            b"video",
        )
        .await
        .expect("movie file");
    }
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Local image batch", LibraryKind::Movie, false)
        .await
        .expect("library");
    libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("index movies");
    let item_ids: Vec<String> = database
        .query_scalar(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY title",
        )
        .bind(library.id.to_string())
        .fetch_all(database.pool())
        .await
        .expect("movie ids");
    assert_eq!(item_ids.len(), 2);

    for item_id in &item_ids {
        database
            .set_poster_fallback_required(item_id, true)
            .await
            .expect("require fallback before poster write");
    }
    let make_image = |image_type: &str, index: i64, path: &str, tag: &str| ItemImageInsert {
        image_type: image_type.to_owned(),
        image_index: index,
        local_path: path.to_owned(),
        file_size: 128,
        width: Some(64),
        height: Some(96),
        content_tag: tag.to_owned(),
        source: "LOCAL".to_owned(),
        source_url: None,
    };
    let batch = [
        ItemImageBatchInsert {
            item_id: item_ids[0].clone(),
            images: vec![
                make_image("POSTER", 0, "/media/first/poster.jpg", "first-poster-v1"),
                make_image("FANART", 0, "/media/first/fanart.jpg", "first-fanart-v1"),
                make_image("FANART", 1, "/media/first/fanart-2.jpg", "first-fanart-v2"),
            ],
            clear_poster_fallback: true,
        },
        ItemImageBatchInsert {
            item_id: item_ids[1].clone(),
            images: vec![make_image(
                "POSTER",
                0,
                "/media/second/poster.jpg",
                "second-poster-v1",
            )],
            clear_poster_fallback: true,
        },
    ];
    database.reset_query_count();
    assert_eq!(
        database
            .insert_item_images_batch_at_indices(&batch)
            .await
            .expect("insert images for both items"),
        4
    );
    assert_eq!(
        database.query_count(),
        2,
        "image rows and fallback clear share one transaction"
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM item_images WHERE item_id = ? AND image_type = 'FANART'",
            )
            .bind(&item_ids[0])
            .fetch_one(database.pool())
            .await
            .expect("count ordered fanart"),
        2
    );
    let fallback: Vec<i64> = database
        .query_scalar(
            "SELECT poster_fallback_required FROM media_items
             WHERE id IN (?, ?) ORDER BY title",
        )
        .bind(&item_ids[0])
        .bind(&item_ids[1])
        .fetch_all(database.pool())
        .await
        .expect("read fallback flags");
    assert_eq!(fallback, vec![0, 0]);
    assert_eq!(
        database
            .insert_item_images_batch_at_indices(&batch)
            .await
            .expect("repeat identical images"),
        0,
        "identical content is an idempotent upsert"
    );
    let changed_poster_batch = [ItemImageBatchInsert {
        item_id: item_ids[0].clone(),
        images: vec![make_image(
            "POSTER",
            0,
            "/media/first/poster-updated.jpg",
            "first-poster-v2",
        )],
        clear_poster_fallback: true,
    }];
    assert_eq!(
        database
            .insert_item_images_batch_at_indices(&changed_poster_batch)
            .await
            .expect("update changed poster path"),
        1
    );
    assert_eq!(
        database
            .query_scalar::<String>(
                "SELECT local_path FROM item_images
                 WHERE item_id = ? AND image_type = 'POSTER' AND image_index = 0",
            )
            .bind(&item_ids[0])
            .fetch_one(database.pool())
            .await
            .expect("read updated poster path"),
        "/media/first/poster-updated.jpg"
    );
    assert_eq!(
        database
            .insert_item_images_batch_at_indices(&[])
            .await
            .expect("empty page is a no-op"),
        0
    );
    database.reset_query_count();
    assert_eq!(
        database
            .insert_item_images_batch_at_indices(&[ItemImageBatchInsert {
                item_id: item_ids[0].clone(),
                images: Vec::new(),
                clear_poster_fallback: false,
            }])
            .await
            .expect("image-less item page is a no-op"),
        0
    );
    assert_eq!(database.query_count(), 0);
    let oversized_batch = (0..17)
        .map(|index| ItemImageBatchInsert {
            item_id: format!("item-{index}"),
            images: Vec::new(),
            clear_poster_fallback: false,
        })
        .collect::<Vec<_>>();
    assert!(
        database
            .insert_item_images_batch_at_indices(&oversized_batch)
            .await
            .is_err(),
        "a storage page cannot exceed sixteen items"
    );

    let second_item_id = item_ids[1].clone();
    let trigger_sql = format!(
        "CREATE TRIGGER reject_second_local_image
         BEFORE INSERT ON item_images
         WHEN NEW.item_id = '{second_item_id}' AND NEW.local_path LIKE '%reject%'
         BEGIN SELECT RAISE(ABORT, 'injected image batch failure'); END"
    );
    sqlx::query(sqlx::AssertSqlSafe(trigger_sql))
        .execute(database.pool())
        .await
        .expect("install failure trigger");
    for item_id in &item_ids {
        database
            .set_poster_fallback_required(item_id, true)
            .await
            .expect("restore fallback before rollback test");
    }
    let failing_batch = [
        ItemImageBatchInsert {
            item_id: item_ids[0].clone(),
            images: vec![make_image(
                "POSTER",
                0,
                "/media/first/poster-failed.jpg",
                "first-poster-v3",
            )],
            clear_poster_fallback: true,
        },
        ItemImageBatchInsert {
            item_id: item_ids[1].clone(),
            images: vec![make_image(
                "LOGO",
                0,
                "/media/second/reject-logo.jpg",
                "reject-logo",
            )],
            clear_poster_fallback: true,
        },
    ];
    assert!(
        database
            .insert_item_images_batch_at_indices(&failing_batch)
            .await
            .is_err(),
        "a single image failure rolls back the complete item page"
    );
    sqlx::query("DROP TRIGGER reject_second_local_image")
        .execute(database.pool())
        .await
        .expect("drop failure trigger");
    let first_image_path: String = database
        .query_scalar(
            "SELECT local_path FROM item_images
             WHERE item_id = ? AND image_type = 'POSTER' AND image_index = 0",
        )
        .bind(&item_ids[0])
        .fetch_one(database.pool())
        .await
        .expect("read first image after rollback");
    assert_eq!(first_image_path, "/media/first/poster-updated.jpg");
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM item_images WHERE item_id = ? AND image_type = 'LOGO'",
            )
            .bind(&item_ids[1])
            .fetch_one(database.pool())
            .await
            .expect("count rolled-back logo"),
        0
    );
    let fallback: Vec<i64> = database
        .query_scalar(
            "SELECT poster_fallback_required FROM media_items
             WHERE id IN (?, ?) ORDER BY title",
        )
        .bind(&item_ids[0])
        .bind(&item_ids[1])
        .fetch_all(database.pool())
        .await
        .expect("read rolled-back fallback flags");
    assert_eq!(fallback, vec![1, 1]);
    database.close().await;
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_local_item_image_batch_is_bounded_idempotent_and_atomic()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration = DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let connection = PostgresConnection {
        database: database_name.clone(),
        ..admin_connection
    };
    let database =
        Database::connect_with_configuration(&config, &DatabaseConfiguration::Postgres(connection))
            .await?;
    let media_root = temp_dir.path().join("Movies");
    for directory in [
        "Postgres Batch First (2024)",
        "Postgres Batch Second (2024)",
    ] {
        let movie_dir = media_root.join(directory);
        tokio::fs::create_dir_all(&movie_dir).await?;
        tokio::fs::write(
            movie_dir.join(format!("{}.mkv", directory.replace(' ', "."))),
            b"video",
        )
        .await?;
    }
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Postgres local image batch", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_ids: Vec<String> = database
        .query_scalar(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY title",
        )
        .bind(library.id.to_string())
        .fetch_all(database.pool())
        .await?;
    assert_eq!(item_ids.len(), 2);
    for item_id in &item_ids {
        database.set_poster_fallback_required(item_id, true).await?;
    }
    let make_image = |image_type: &str, index: i64, path: String, tag: String| ItemImageInsert {
        image_type: image_type.to_owned(),
        image_index: index,
        local_path: path,
        file_size: 128,
        width: Some(64),
        height: Some(96),
        content_tag: tag,
        source: "LOCAL".to_owned(),
        source_url: None,
    };
    let mut first_images = vec![make_image(
        "POSTER",
        0,
        "/media/first/poster.jpg".to_owned(),
        "poster-v1".to_owned(),
    )];
    first_images.extend((0..65).map(|index| {
        make_image(
            "FANART",
            index,
            format!("/media/first/fanart-{index}.jpg"),
            format!("fanart-v{index}"),
        )
    }));
    let batch = [
        ItemImageBatchInsert {
            item_id: item_ids[0].clone(),
            images: first_images,
            clear_poster_fallback: true,
        },
        ItemImageBatchInsert {
            item_id: item_ids[1].clone(),
            images: vec![make_image(
                "POSTER",
                0,
                "/media/second/poster.jpg".to_owned(),
                "poster-v1".to_owned(),
            )],
            clear_poster_fallback: true,
        },
    ];
    assert_eq!(
        database.insert_item_images_batch_at_indices(&batch).await?,
        67
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM item_images WHERE item_id = ? AND image_type = 'FANART'",
            )
            .bind(&item_ids[0])
            .fetch_one(database.pool())
            .await?,
        65
    );
    assert_eq!(
        database.insert_item_images_batch_at_indices(&batch).await?,
        0
    );
    let changed_poster_batch = [ItemImageBatchInsert {
        item_id: item_ids[0].clone(),
        images: vec![make_image(
            "POSTER",
            0,
            "/media/first/poster-updated.jpg".to_owned(),
            "poster-v2".to_owned(),
        )],
        clear_poster_fallback: true,
    }];
    assert_eq!(
        database
            .insert_item_images_batch_at_indices(&changed_poster_batch)
            .await?,
        1
    );
    assert_eq!(
        database
            .query_scalar::<String>(
                "SELECT local_path FROM item_images
                 WHERE item_id = ? AND image_type = 'POSTER' AND image_index = 0",
            )
            .bind(&item_ids[0])
            .fetch_one(database.pool())
            .await?,
        "/media/first/poster-updated.jpg"
    );
    let empty_batch: [ItemImageBatchInsert; 0] = [];
    assert_eq!(
        database
            .insert_item_images_batch_at_indices(&empty_batch)
            .await?,
        0
    );

    let oversized = (0..17)
        .map(|index| ItemImageBatchInsert {
            item_id: format!("postgres-item-{index}"),
            images: Vec::new(),
            clear_poster_fallback: false,
        })
        .collect::<Vec<_>>();
    assert!(
        database
            .insert_item_images_batch_at_indices(&oversized)
            .await
            .is_err()
    );

    let trigger_function = format!(
        "CREATE FUNCTION reject_postgres_local_image_batch() RETURNS trigger AS $$
         BEGIN
             IF NEW.item_id = '{second_item}' AND NEW.local_path LIKE '%reject%' THEN
                 RAISE EXCEPTION 'injected image batch failure';
             END IF;
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql",
        second_item = item_ids[1],
    );
    sqlx::query(sqlx::AssertSqlSafe(trigger_function))
        .execute(database.pool())
        .await?;
    sqlx::query(
        "CREATE TRIGGER reject_postgres_local_image_batch
         BEFORE INSERT ON item_images
         FOR EACH ROW EXECUTE FUNCTION reject_postgres_local_image_batch()",
    )
    .execute(database.pool())
    .await?;
    for item_id in &item_ids {
        database.set_poster_fallback_required(item_id, true).await?;
    }
    let failing_batch = [
        ItemImageBatchInsert {
            item_id: item_ids[0].clone(),
            images: vec![make_image(
                "POSTER",
                0,
                "/media/first/poster-failed.jpg".to_owned(),
                "poster-v3".to_owned(),
            )],
            clear_poster_fallback: true,
        },
        ItemImageBatchInsert {
            item_id: item_ids[1].clone(),
            images: vec![make_image(
                "LOGO",
                0,
                "/media/second/reject-logo.jpg".to_owned(),
                "reject-logo".to_owned(),
            )],
            clear_poster_fallback: true,
        },
    ];
    assert!(
        database
            .insert_item_images_batch_at_indices(&failing_batch)
            .await
            .is_err()
    );
    sqlx::query("DROP TRIGGER reject_postgres_local_image_batch ON item_images")
        .execute(database.pool())
        .await?;
    sqlx::query("DROP FUNCTION reject_postgres_local_image_batch()")
        .execute(database.pool())
        .await?;
    assert_eq!(
        database
            .query_scalar::<String>(
                "SELECT local_path FROM item_images
                 WHERE item_id = ? AND image_type = 'POSTER' AND image_index = 0",
            )
            .bind(&item_ids[0])
            .fetch_one(database.pool())
            .await?,
        "/media/first/poster-updated.jpg"
    );
    let fallback: Vec<i64> = database
        .query_scalar(
            "SELECT poster_fallback_required FROM media_items
             WHERE id IN (?, ?) ORDER BY title",
        )
        .bind(&item_ids[0])
        .bind(&item_ids[1])
        .fetch_all(database.pool())
        .await?;
    assert_eq!(fallback, vec![1, 1]);
    database.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    admin_pool.close().await;
    Ok(())
}

#[tokio::test]
async fn progressive_scan_metadata_dispatch_is_atomic_and_deduplicated() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    for directory in [
        "First Movie (2025)",
        "Second Movie (2025)",
        "Third Movie (2025)",
        "Replay Movie (2025)",
    ] {
        let movie_dir = media_root.join(directory);
        tokio::fs::create_dir_all(&movie_dir)
            .await
            .expect("movie directory");
        let filename = directory.replace(' ', ".");
        tokio::fs::write(movie_dir.join(format!("{filename}.mkv")), b"video")
            .await
            .expect("movie file");
    }
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Progressive dispatch", LibraryKind::Movie, false)
        .await
        .expect("library");
    libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("index movies");
    let item_ids: Vec<String> = database
        .query_scalar(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY id",
        )
        .bind(library.id.to_string())
        .fetch_all(database.pool())
        .await
        .expect("indexed movies");
    assert_eq!(item_ids.len(), 4);
    let library_id = library.id.to_string();

    sqlx::query("UPDATE libraries SET realtime_metadata_auto_match_enabled = 0 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("disable scan auto-match");
    let poster_fingerprints = [
        b"poster-v1".to_vec(),
        b"poster-v2".to_vec(),
        b"poster-v3".to_vec(),
        b"poster-v4".to_vec(),
    ];
    let mut poster_results = Vec::new();
    for (item_id, fingerprint) in item_ids.iter().zip(&poster_fingerprints) {
        assert!(
            database
                .prepare_item_metadata_completeness_check(item_id, "POSTER", fingerprint)
                .await
                .expect("prepare poster")
        );
        assert!(
            database
                .claim_item_metadata_completeness_check(item_id, "POSTER", fingerprint)
                .await
                .expect("claim poster")
        );
        poster_results.push(NewItemMetadataCompletenessResult {
            item_id,
            capability: "POSTER",
            input_fingerprint: fingerprint,
            is_missing: true,
            checked_at: 10,
        });
    }
    let disabled = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &poster_results, &item_ids)
        .await
        .expect("persist missing posters while auto-match is disabled");
    assert_eq!(disabled.updated_count, 4);
    assert!(disabled.scheduled_job_ids.is_empty());
    for item_id in &item_ids {
        let completeness = database
            .find_item_metadata_completeness(item_id, "POSTER")
            .await
            .expect("read disabled missing poster")
            .expect("poster completeness");
        assert_eq!(completeness.local_state, "READY");
        assert_eq!(completeness.is_missing, Some(true));
    }

    sqlx::query("UPDATE libraries SET realtime_metadata_auto_match_enabled = 1 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("enable scan auto-match");

    let incremental_disabled_fingerprint = b"incremental-disabled";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                &item_ids[0],
                "EXTERNAL_IDS",
                incremental_disabled_fingerprint,
            )
            .await
            .expect("prepare disabled incremental capability")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                &item_ids[0],
                "EXTERNAL_IDS",
                incremental_disabled_fingerprint,
            )
            .await
            .expect("claim disabled incremental capability")
    );
    let incremental_disabled_result = [NewItemMetadataCompletenessResult {
        item_id: &item_ids[0],
        capability: "EXTERNAL_IDS",
        input_fingerprint: incremental_disabled_fingerprint,
        is_missing: true,
        checked_at: 10,
    }];
    let incremental_disabled = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &incremental_disabled_result,
            std::slice::from_ref(&item_ids[0]),
            MetadataAutoMatchPolicy::Disabled,
        )
        .await
        .expect("incremental job policy overrides enabled full-scan policy");
    assert_eq!(incremental_disabled.updated_count, 1);
    assert!(incremental_disabled.scheduled_job_ids.is_empty());

    sqlx::query("UPDATE libraries SET realtime_metadata_auto_match_enabled = 0 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("disable full-scan policy");
    let incremental_enabled_fingerprint = b"incremental-enabled";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                &item_ids[2],
                "TRAILERS",
                incremental_enabled_fingerprint,
            )
            .await
            .expect("prepare enabled incremental capability")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                &item_ids[2],
                "TRAILERS",
                incremental_enabled_fingerprint,
            )
            .await
            .expect("claim enabled incremental capability")
    );
    let incremental_enabled_result = [NewItemMetadataCompletenessResult {
        item_id: &item_ids[2],
        capability: "TRAILERS",
        input_fingerprint: incremental_enabled_fingerprint,
        is_missing: true,
        checked_at: 10,
    }];
    database.reset_query_count();
    let incremental_enabled = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &incremental_enabled_result,
            std::slice::from_ref(&item_ids[2]),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("incremental job policy can enable auto-match for its sources");
    assert_eq!(incremental_enabled.scheduled_job_ids.len(), 1);
    assert_eq!(
        database.query_count(),
        7,
        "completeness confirmation and schedulable-item lookup should share one page query"
    );

    let replay_fingerprint = b"replay-without-new-result";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_ids[3], "POSTER", replay_fingerprint,)
            .await
            .expect("prepare replay poster")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_ids[3], "POSTER", replay_fingerprint,)
            .await
            .expect("claim replay poster")
    );
    let replay_result = [NewItemMetadataCompletenessResult {
        item_id: &item_ids[3],
        capability: "POSTER",
        input_fingerprint: replay_fingerprint,
        is_missing: true,
        checked_at: 10,
    }];
    let policy_disabled = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &replay_result,
            std::slice::from_ref(&item_ids[3]),
            MetadataAutoMatchPolicy::Disabled,
        )
        .await
        .expect("persist missing without scheduling when the call disables auto-match");
    assert_eq!(policy_disabled.updated_count, 1);
    assert!(policy_disabled.scheduled_job_ids.is_empty());
    let replayed_without_new_result = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &[],
            std::slice::from_ref(&item_ids[3]),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("schedule an already-confirmed missing poster after policy changes");
    assert_eq!(replayed_without_new_result.updated_count, 0);
    assert_eq!(replayed_without_new_result.scheduled_job_ids.len(), 1);
    let replayed_active_job = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &[],
            std::slice::from_ref(&item_ids[3]),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("deduplicate replay against the active fill-missing job");
    assert!(replayed_active_job.scheduled_job_ids.is_empty());

    sqlx::query(
        "UPDATE metadata_reidentify_jobs
         SET status = 'DEFERRED', updated_at = unixepoch()
         WHERE id IN (
             SELECT job_id FROM metadata_reidentify_job_items WHERE item_id = ?
         ) AND mode = 'FILL_MISSING'",
    )
    .bind(&item_ids[3])
    .execute(database.pool())
    .await
    .expect("defer the existing fill-missing job");
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'FAILED', error = 'SCRAPER_UNAVAILABLE'
         WHERE item_id = ? AND job_id IN (
             SELECT id FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'
         )",
    )
    .bind(&item_ids[3])
    .execute(database.pool())
    .await
    .expect("record provider-unavailable item result");
    let replayed_deferred_job = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &[],
            std::slice::from_ref(&item_ids[3]),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("deduplicate replay against the deferred fill-missing job");
    assert!(replayed_deferred_job.scheduled_job_ids.is_empty());

    sqlx::query("UPDATE libraries SET realtime_metadata_auto_match_enabled = 1 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("restore full-scan policy");
    let explicit_job_id = "manual-fill-missing";
    database
        .create_metadata_reidentify_job(explicit_job_id, &[item_ids[0].clone()], "FILL_MISSING")
        .await
        .expect("create active manual fill-missing job");

    let backdrop_fingerprints = [
        b"backdrop-v1".to_vec(),
        b"backdrop-v2".to_vec(),
        b"backdrop-v3".to_vec(),
        b"backdrop-v4".to_vec(),
    ];
    let mut backdrop_results = Vec::new();
    for (item_id, fingerprint) in item_ids.iter().zip(&backdrop_fingerprints) {
        assert!(
            database
                .prepare_item_metadata_completeness_check(item_id, "BACKDROP", fingerprint)
                .await
                .expect("prepare backdrop")
        );
        assert!(
            database
                .claim_item_metadata_completeness_check(item_id, "BACKDROP", fingerprint)
                .await
                .expect("claim backdrop")
        );
        backdrop_results.push(NewItemMetadataCompletenessResult {
            item_id,
            capability: "BACKDROP",
            input_fingerprint: fingerprint,
            is_missing: true,
            checked_at: 11,
        });
    }

    sqlx::query(
        "CREATE TRIGGER reject_fill_missing_job
         BEFORE INSERT ON metadata_reidentify_job_items
         WHEN EXISTS (
             SELECT 1 FROM metadata_reidentify_jobs
             WHERE id = NEW.job_id AND mode = 'FILL_MISSING'
         )
         BEGIN SELECT RAISE(ABORT, 'injected fill-missing job failure'); END",
    )
    .execute(database.pool())
    .await
    .expect("install rollback trigger");
    assert!(
        database
            .complete_local_metadata_and_enqueue_fill_missing(
                &library_id,
                &backdrop_results,
                &item_ids,
            )
            .await
            .is_err()
    );
    sqlx::query("DROP TRIGGER reject_fill_missing_job")
        .execute(database.pool())
        .await
        .expect("remove rollback trigger");
    for item_id in &item_ids {
        let completeness = database
            .find_item_metadata_completeness(item_id, "BACKDROP")
            .await
            .expect("read rolled-back completeness")
            .expect("backdrop completeness");
        assert_eq!(completeness.local_state, "RUNNING");
        assert_eq!(completeness.is_missing, None);
    }

    let scheduled = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &backdrop_results, &item_ids)
        .await
        .expect("atomically persist and schedule missing backdrops");
    assert_eq!(scheduled.updated_count, 4);
    assert_eq!(scheduled.scheduled_job_ids.len(), 1);
    let scheduled_job_id = &scheduled.scheduled_job_ids[0];
    let job: (String, String, i64, String) = database
        .query_as(
            "SELECT mode, status, total_count, job_scope
             FROM metadata_reidentify_jobs WHERE id = ?",
        )
        .bind(scheduled_job_id)
        .fetch_one(database.pool())
        .await
        .expect("scheduled fill-missing job");
    assert_eq!(
        job,
        (
            "FILL_MISSING".to_owned(),
            "QUEUED".to_owned(),
            3,
            "ITEMS".to_owned()
        )
    );
    let scheduled_items: Vec<String> = database
        .query_scalar(
            "SELECT item_id FROM metadata_reidentify_job_items WHERE job_id = ? ORDER BY item_id",
        )
        .bind(scheduled_job_id)
        .fetch_all(database.pool())
        .await
        .expect("scheduled item page");
    assert_eq!(scheduled_items, item_ids[1..].to_vec());

    let queued_merge_fingerprint = b"queued-merge-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                &item_ids[0],
                "STILL",
                queued_merge_fingerprint,
            )
            .await
            .expect("prepare queued merge check")
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                &item_ids[0],
                "STILL",
                queued_merge_fingerprint,
            )
            .await
            .expect("claim queued merge check")
    );
    let queued_merge_result = [NewItemMetadataCompletenessResult {
        item_id: &item_ids[0],
        capability: "STILL",
        input_fingerprint: queued_merge_fingerprint,
        is_missing: true,
        checked_at: 11,
    }];
    let explicit_item_auto_replay = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &queued_merge_result,
            &[item_ids[0].clone()],
        )
        .await
        .expect("skip an item already owned by an explicit job");
    assert!(explicit_item_auto_replay.scheduled_job_ids.is_empty());
    assert_eq!(
        database
            .query_scalar::<i64>("SELECT total_count FROM metadata_reidentify_jobs WHERE id = ?",)
            .bind(scheduled_job_id)
            .fetch_one(database.pool())
            .await
            .expect("read merged queued job count"),
        3
    );
    let explicit_job_state: (String, i64) = database
        .query_as("SELECT dispatch_source, total_count FROM metadata_reidentify_jobs WHERE id = ?")
        .bind(explicit_job_id)
        .fetch_one(database.pool())
        .await
        .expect("read explicit job state");
    assert_eq!(explicit_job_state, ("EXPLICIT".to_owned(), 1));

    let replayed = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &backdrop_results, &item_ids)
        .await
        .expect("replay the already completed local result");
    assert_eq!(replayed.updated_count, 0);
    assert!(replayed.scheduled_job_ids.is_empty());

    let still_fingerprints = [
        b"still-v1".to_vec(),
        b"still-v2".to_vec(),
        b"still-v3".to_vec(),
        b"still-v4".to_vec(),
    ];
    let duplicate_results = item_ids
        .iter()
        .zip(&still_fingerprints)
        .map(|(item_id, fingerprint)| NewItemMetadataCompletenessResult {
            item_id,
            capability: "STILL",
            input_fingerprint: fingerprint,
            is_missing: true,
            checked_at: 12,
        })
        .collect::<Vec<_>>();
    for result in &duplicate_results {
        assert!(
            database
                .prepare_item_metadata_completeness_check(
                    result.item_id,
                    result.capability,
                    result.input_fingerprint,
                )
                .await
                .expect("prepare still")
        );
        assert!(
            database
                .claim_item_metadata_completeness_check(
                    result.item_id,
                    result.capability,
                    result.input_fingerprint,
                )
                .await
                .expect("claim still")
        );
    }
    let deduplicated = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &duplicate_results,
            &item_ids,
        )
        .await
        .expect("reuse active fill-missing jobs");
    assert_eq!(deduplicated.updated_count, 4);
    assert!(deduplicated.scheduled_job_ids.is_empty());
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'",
            )
            .fetch_one(database.pool())
            .await
            .expect("count fill-missing jobs"),
        3,
        "explicit and automatic jobs keep independent queue ownership"
    );
    let source_counts: Vec<(String, i64)> = database
        .query_as(
            "SELECT dispatch_source, COUNT(*) FROM metadata_reidentify_jobs
             WHERE mode = 'FILL_MISSING' GROUP BY dispatch_source ORDER BY dispatch_source",
        )
        .fetch_all(database.pool())
        .await
        .expect("count jobs by dispatch source");
    assert_eq!(
        source_counts,
        vec![("AUTOMATIC".to_owned(), 2), ("EXPLICIT".to_owned(), 1)]
    );

    let pagination_root_path = temp_dir.path().join("Pagination Movies");
    for index in 0..103 {
        let directory = format!("Pagination Movie {index:03} (2025)");
        let movie_dir = pagination_root_path.join(&directory);
        tokio::fs::create_dir_all(&movie_dir)
            .await
            .expect("pagination movie directory");
        tokio::fs::write(
            movie_dir.join(format!("Pagination.Movie.{index:03}.2025.mkv")),
            b"video",
        )
        .await
        .expect("pagination movie file");
    }
    let pagination_library = libraries
        .create_library("Progressive dispatch pagination", LibraryKind::Movie, false)
        .await
        .expect("pagination library");
    libraries
        .add_root(
            pagination_library.id,
            pagination_root_path.to_str().expect("pagination root"),
        )
        .await
        .expect("pagination library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(pagination_library.id)
        .await
        .expect("index pagination movies");
    let pagination_library_id = pagination_library.id.to_string();
    let pagination_item_ids: Vec<String> = database
        .query_scalar(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY id",
        )
        .bind(&pagination_library_id)
        .fetch_all(database.pool())
        .await
        .expect("read pagination movies");
    assert_eq!(pagination_item_ids.len(), 103);
    let unsupported_item_id = &pagination_item_ids[0];
    let removed_item_id = &pagination_item_ids[1];
    database
        .query("UPDATE media_items SET item_type = 'FOLDER' WHERE id = ?")
        .bind(unsupported_item_id)
        .execute(database.pool())
        .await
        .expect("mark one item type as unsupported for automatic metadata jobs");
    database
        .query("UPDATE media_items SET removed_at = 1 WHERE id = ?")
        .bind(removed_item_id)
        .execute(database.pool())
        .await
        .expect("mark one item removed before metadata completion");
    let pagination_fingerprint = b"pagination-input-v1";
    let pagination_checks = pagination_item_ids
        .iter()
        .map(|item_id| NewItemMetadataCompletenessCheck {
            item_id,
            capability: "POSTER",
            input_fingerprint: pagination_fingerprint,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&pagination_checks)
            .await
            .expect("claim pagination completeness checks")
            .len(),
        103
    );
    let pagination_results = pagination_item_ids
        .iter()
        .map(|item_id| NewItemMetadataCompletenessResult {
            item_id,
            capability: "POSTER",
            input_fingerprint: pagination_fingerprint,
            is_missing: true,
            checked_at: 13,
        })
        .collect::<Vec<_>>();
    let pagination_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &pagination_library_id,
            &pagination_results,
            &pagination_item_ids,
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("enqueue pagination jobs");
    assert_eq!(pagination_dispatch.updated_count, 102);
    assert_eq!(pagination_dispatch.scheduled_job_ids.len(), 2);
    let pagination_job_counts: Vec<i64> = database
        .query_scalar(
            "SELECT total_count FROM metadata_reidentify_jobs
             WHERE library_id = ? AND mode = 'FILL_MISSING' ORDER BY total_count DESC",
        )
        .bind(&pagination_library_id)
        .fetch_all(database.pool())
        .await
        .expect("read pagination job sizes");
    assert_eq!(pagination_job_counts, vec![100, 1]);
    let pagination_job_item_ids: Vec<String> = database
        .query_scalar(
            "SELECT job_items.item_id FROM metadata_reidentify_job_items job_items
             JOIN metadata_reidentify_jobs jobs ON jobs.id = job_items.job_id
             WHERE jobs.library_id = ? AND jobs.mode = 'FILL_MISSING'",
        )
        .bind(&pagination_library_id)
        .fetch_all(database.pool())
        .await
        .expect("read paginated job items");
    assert_eq!(pagination_job_item_ids.len(), 101);
    assert!(!pagination_job_item_ids.contains(unsupported_item_id));
    assert!(!pagination_job_item_ids.contains(removed_item_id));
    let removed_completeness = database
        .find_item_metadata_completeness(removed_item_id, "POSTER")
        .await
        .expect("read removed item completeness")
        .expect("removed item completeness row");
    assert_eq!(removed_completeness.local_state, "RUNNING");

    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET error = 'METADATA_WRITE_FAILED'
         WHERE item_id = ? AND status = 'FAILED'
           AND job_id IN (
               SELECT id FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'
           )",
    )
    .bind(&item_ids[3])
    .execute(database.pool())
    .await
    .expect("set a non-provider deferred error");
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'COMPLETED'
         WHERE item_id = ? AND status = 'PENDING'
           AND job_id IN (
               SELECT id FROM metadata_reidentify_jobs
               WHERE mode = 'FILL_MISSING' AND status = 'QUEUED'
           )",
    )
    .bind(&item_ids[3])
    .execute(database.pool())
    .await
    .expect("finish the changed snapshot job before testing deferred retry");
    sqlx::query(
        "UPDATE metadata_reidentify_jobs
         SET status = 'COMPLETED', processed_count = total_count
         WHERE mode = 'FILL_MISSING' AND status = 'QUEUED' AND id IN (
             SELECT job_id FROM metadata_reidentify_job_items WHERE item_id = ?
         )",
    )
    .bind(&item_ids[3])
    .execute(database.pool())
    .await
    .expect("close the queued snapshot job before testing deferred retry");
    let retried_non_provider_failure = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &[],
            std::slice::from_ref(&item_ids[3]),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("allow retry after a non-provider failure");
    assert_eq!(retried_non_provider_failure.scheduled_job_ids.len(), 1);

    sqlx::query(
        "UPDATE metadata_reidentify_jobs
         SET status = 'DEFERRED', updated_at = unixepoch() - 3601
         WHERE id IN (
             SELECT job_id FROM metadata_reidentify_job_items WHERE item_id = ?
         ) AND mode = 'FILL_MISSING'",
    )
    .bind(&item_ids[3])
    .execute(database.pool())
    .await
    .expect("age prior deferred fill-missing jobs");
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'FAILED', error = 'SCRAPER_UNAVAILABLE',
             automatic_retry_after = unixepoch() - 1
         WHERE item_id = ? AND job_id IN (
             SELECT id FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'
         )",
    )
    .bind(&item_ids[3])
    .execute(database.pool())
    .await
    .expect("restore provider failure after deferral window");
    let retried_expired_provider_failure = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &[],
            std::slice::from_ref(&item_ids[3]),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await
        .expect("allow retry after deferred deduplication expires");
    assert_eq!(retried_expired_provider_failure.scheduled_job_ids.len(), 1);

    database.close().await;
}

#[tokio::test]
async fn changed_fill_request_is_not_deduplicated_by_recent_provider_deferral()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Deferred snapshot", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    sqlx::query("UPDATE libraries SET realtime_metadata_auto_match_enabled = 1 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    let item_id = "deferred-snapshot-item";
    database
        .query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .execute(database.pool())
        .await?;

    let first_fingerprint = b"deferred-input-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(item_id, "POSTER", first_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(item_id, "POSTER", first_fingerprint)
            .await?
    );
    let first_result = [NewItemMetadataCompletenessResult {
        item_id,
        capability: "POSTER",
        input_fingerprint: first_fingerprint,
        is_missing: true,
        checked_at: 10,
    }];
    let first = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &first_result,
            &[item_id.into()],
        )
        .await?;
    let first_job_id = &first.scheduled_job_ids[0];
    sqlx::query(
        "UPDATE metadata_reidentify_jobs SET status = 'DEFERRED', updated_at = unixepoch()
         WHERE id = ?",
    )
    .bind(first_job_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE metadata_reidentify_job_items SET status = 'FAILED', error = 'SCRAPER_UNAVAILABLE'
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(first_job_id)
    .bind(item_id)
    .execute(database.pool())
    .await?;
    let same_snapshot = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &[], &[item_id.into()])
        .await?;
    assert!(same_snapshot.scheduled_job_ids.is_empty());

    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET automatic_retry_after = unixepoch() - 1
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(first_job_id)
    .bind(item_id)
    .execute(database.pool())
    .await?;

    let changed_fingerprint = b"deferred-input-v2";
    assert!(
        database
            .prepare_item_metadata_completeness_check(item_id, "POSTER", changed_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(item_id, "POSTER", changed_fingerprint)
            .await?
    );
    let changed_result = [NewItemMetadataCompletenessResult {
        item_id,
        capability: "POSTER",
        input_fingerprint: changed_fingerprint,
        is_missing: true,
        checked_at: 11,
    }];
    let changed = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &changed_result,
            &[item_id.into()],
        )
        .await?;
    assert_eq!(changed.scheduled_job_ids.len(), 1);
    assert_ne!(changed.scheduled_job_ids[0], *first_job_id);

    let changed_job_id = &changed.scheduled_job_ids[0];
    sqlx::query(
        "UPDATE metadata_reidentify_job_items SET status = 'COMPLETED'
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(changed_job_id)
    .bind(item_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE metadata_reidentify_jobs
         SET status = 'COMPLETED', processed_count = total_count
         WHERE id = ?",
    )
    .bind(changed_job_id)
    .execute(database.pool())
    .await?;

    let old_retry_consumed: i64 = database
        .query_scalar(
            "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(first_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        old_retry_consumed, 1,
        "the newer fingerprint supersedes the old provider failure"
    );
    let (_, due_retries) = database
        .prepare_and_claim_item_metadata_completeness_checks_with_due_fill_missing_retries(
            &[NewItemMetadataCompletenessCheck {
                item_id,
                capability: "POSTER",
                input_fingerprint: changed_fingerprint,
            }],
            &library_id,
            &[item_id.to_owned()],
        )
        .await?;
    assert!(
        due_retries.is_empty(),
        "an obsolete fingerprint must not keep unlocking the completed newer request"
    );
    Ok(())
}

async fn fail_fill_missing_job_with_unavailable_provider(
    database: &Database,
    job_id: &str,
    item_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    assert!(database.claim_metadata_reidentify_job(job_id).await?);
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(job_id, 1)
            .await?,
        vec![item_id.to_owned()]
    );
    database
        .finish_metadata_reidentify_item(job_id, item_id, "FAILED", 0, Some("SCRAPER_UNAVAILABLE"))
        .await?;
    database
        .finish_metadata_reidentify_job(job_id, "DEFERRED", Some("DEFERRED_PROVIDER_UNAVAILABLE"))
        .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_changed_fill_request_consumes_obsolete_provider_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration = DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let connection = PostgresConnection {
        database: database_name.clone(),
        ..admin_connection
    };
    let database =
        Database::connect_with_configuration(&config, &DatabaseConfiguration::Postgres(connection))
            .await?;

    let assertions = async {
        let library = LibraryService::new(database.clone())
            .create_library("Postgres obsolete failure", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        database
            .query("UPDATE libraries SET realtime_metadata_auto_match_enabled = 1 WHERE id = ?")
            .bind(&library_id)
            .execute(database.pool())
            .await?;
        let item_id = "postgres-obsolete-fill-failure";
        database
            .query(
                "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(&library_id)
            .execute(database.pool())
            .await?;

        let old_fingerprint = b"postgres-obsolete-input-v1";
        assert!(
            database
                .prepare_item_metadata_completeness_check(item_id, "POSTER", old_fingerprint)
                .await?
        );
        assert!(
            database
                .claim_item_metadata_completeness_check(item_id, "POSTER", old_fingerprint)
                .await?
        );
        let old_result = [NewItemMetadataCompletenessResult {
            item_id,
            capability: "POSTER",
            input_fingerprint: old_fingerprint,
            is_missing: true,
            checked_at: 10,
        }];
        let old_dispatch = database
            .complete_local_metadata_and_enqueue_fill_missing(
                &library_id,
                &old_result,
                &[item_id.into()],
            )
            .await?;
        let old_job_id = old_dispatch
            .scheduled_job_ids
            .first()
            .ok_or("expected old fill-missing job")?
            .clone();
        fail_fill_missing_job_with_unavailable_provider(&database, &old_job_id, item_id).await?;
        database
            .query(
                "UPDATE metadata_reidentify_job_items
                 SET automatic_retry_after = unixepoch() - 1
                 WHERE job_id = ? AND item_id = ?",
            )
            .bind(&old_job_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;

        let new_fingerprint = b"postgres-obsolete-input-v2";
        assert!(
            database
                .prepare_item_metadata_completeness_check(item_id, "POSTER", new_fingerprint)
                .await?
        );
        assert!(
            database
                .claim_item_metadata_completeness_check(item_id, "POSTER", new_fingerprint)
                .await?
        );
        let new_result = [NewItemMetadataCompletenessResult {
            item_id,
            capability: "POSTER",
            input_fingerprint: new_fingerprint,
            is_missing: true,
            checked_at: 11,
        }];
        let new_dispatch = database
            .complete_local_metadata_and_enqueue_fill_missing(
                &library_id,
                &new_result,
                &[item_id.into()],
            )
            .await?;
        let new_job_id = new_dispatch
            .scheduled_job_ids
            .first()
            .ok_or("expected new fill-missing job")?
            .clone();
        assert_ne!(new_job_id, old_job_id);
        assert_eq!(
            database
                .query_scalar::<i64>(
                    "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
                     WHERE job_id = ? AND item_id = ?",
                )
                .bind(&old_job_id)
                .bind(item_id)
                .fetch_one(database.pool())
                .await?,
            1,
            "the newer fingerprint supersedes the old provider failure"
        );

        database
            .query(
                "UPDATE metadata_reidentify_job_items SET status = 'COMPLETED'
                 WHERE job_id = ? AND item_id = ?",
            )
            .bind(&new_job_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
        database
            .query(
                "UPDATE metadata_reidentify_jobs
                 SET status = 'COMPLETED', processed_count = total_count
                 WHERE id = ?",
            )
            .bind(&new_job_id)
            .execute(database.pool())
            .await?;

        let (_, due_retries) = database
            .prepare_and_claim_item_metadata_completeness_checks_with_due_fill_missing_retries(
                &[NewItemMetadataCompletenessCheck {
                    item_id,
                    capability: "POSTER",
                    input_fingerprint: new_fingerprint,
                }],
                &library_id,
                &[item_id.to_owned()],
            )
            .await?;
        assert!(
            due_retries.is_empty(),
            "an obsolete fingerprint must not unlock the completed newer request"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    assertions?;
    drop_database?;
    Ok(())
}

#[tokio::test]
async fn automatic_fill_missing_provider_retry_backoff_is_capped_and_snapshot_scoped()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Fill backoff", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    sqlx::query("UPDATE libraries SET realtime_metadata_auto_match_enabled = 1 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    let item_id = "fill-backoff-item";
    database
        .query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .execute(database.pool())
        .await?;

    let first_fingerprint = b"fill-backoff-input-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(item_id, "POSTER", first_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(item_id, "POSTER", first_fingerprint)
            .await?
    );
    let first_result = [NewItemMetadataCompletenessResult {
        item_id,
        capability: "POSTER",
        input_fingerprint: first_fingerprint,
        is_missing: true,
        checked_at: 10,
    }];
    let first_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &first_result,
            &[item_id.to_owned()],
        )
        .await?;
    let first_job_id = first_dispatch.scheduled_job_ids[0].clone();
    fail_fill_missing_job_with_unavailable_provider(&database, &first_job_id, item_id).await?;

    let first_backoff: (i64, i64) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_after - unixepoch()
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&first_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(first_backoff.0, 1);
    assert!((299..=300).contains(&first_backoff.1));

    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET automatic_retry_count = 0, automatic_retry_after = NULL
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(&first_job_id)
    .bind(item_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE server_settings SET value = CAST(unixepoch() + 300 AS TEXT)
         WHERE key = 'metadata_fill_missing_legacy_retry_after'",
    )
    .execute(database.pool())
    .await?;
    let suppressed = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &[], &[item_id.to_owned()])
        .await?;
    assert!(suppressed.scheduled_job_ids.is_empty());
    sqlx::query(
        "UPDATE server_settings SET value = CAST(unixepoch() - 1 AS TEXT)
         WHERE key = 'metadata_fill_missing_legacy_retry_after'",
    )
    .execute(database.pool())
    .await?;
    let due_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &[], &[item_id.to_owned()])
        .await?;
    assert_eq!(due_dispatch.scheduled_job_ids.len(), 1);
    let second_job_id = due_dispatch.scheduled_job_ids[0].clone();
    assert_ne!(second_job_id, first_job_id);
    let inherited_count: i64 = database
        .query_scalar(
            "SELECT automatic_retry_count FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(&second_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(inherited_count, 1);

    fail_fill_missing_job_with_unavailable_provider(&database, &second_job_id, item_id).await?;
    let second_backoff: (i64, i64) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_after - unixepoch()
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&second_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(second_backoff.0, 2);
    assert!((1_799..=1_800).contains(&second_backoff.1));

    assert!(
        database
            .retry_metadata_reidentify_job(&second_job_id)
            .await?
    );
    let manual_retry_state: (String, Option<i64>) = database
        .query_as(
            "SELECT items.status, items.automatic_retry_after
             FROM metadata_reidentify_job_items items
             WHERE items.job_id = ? AND items.item_id = ?",
        )
        .bind(&second_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(manual_retry_state, ("PENDING".to_owned(), None));
    fail_fill_missing_job_with_unavailable_provider(&database, &second_job_id, item_id).await?;
    let capped_backoff: (i64, i64) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_after - unixepoch()
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&second_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(capped_backoff.0, 3);
    assert!((21_599..=21_600).contains(&capped_backoff.1));

    let changed_fingerprint = b"fill-backoff-input-v2";
    assert!(
        database
            .prepare_item_metadata_completeness_check(item_id, "POSTER", changed_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(item_id, "POSTER", changed_fingerprint)
            .await?
    );
    let changed_result = [NewItemMetadataCompletenessResult {
        item_id,
        capability: "POSTER",
        input_fingerprint: changed_fingerprint,
        is_missing: true,
        checked_at: 11,
    }];
    let changed_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &changed_result,
            &[item_id.to_owned()],
        )
        .await?;
    assert!(changed_dispatch.scheduled_job_ids.is_empty());
    let explicit_retry_state: (i64, Option<i64>, String, i64, Option<Vec<u8>>) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_after,
                    request_capabilities_json, automatic_retry_consumed, request_fingerprint
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&second_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(explicit_retry_state.0, 3);
    assert!(explicit_retry_state.1.is_some());
    assert_eq!(explicit_retry_state.2, "[\"POSTER\"]");
    assert_eq!(explicit_retry_state.3, 0);
    assert_eq!(explicit_retry_state.4, Some(first_fingerprint.to_vec()));

    let changed_capability_request = [crate::storage::MetadataFillMissingRequest {
        item_id: item_id.to_owned(),
        input_fingerprint: Some(changed_fingerprint.to_vec()),
        capabilities_json: "[\"BACKDROP\"]".to_owned(),
        automatic_retry_count: 0,
    }];
    let mut transaction = database.begin_metadata_write_transaction().await?;
    let changed_capability_job_ids = database
        .enqueue_or_update_fill_missing_requests_in_transaction(
            &mut transaction,
            &library_id,
            &changed_capability_request,
        )
        .await?;
    transaction.commit().await?;
    assert!(changed_capability_job_ids.is_empty());
    let unchanged_retry_state: (i64, String, i64, Option<Vec<u8>>) = database
        .query_as(
            "SELECT automatic_retry_count, request_capabilities_json,
                    automatic_retry_consumed, request_fingerprint
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&second_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        unchanged_retry_state,
        (
            3,
            "[\"POSTER\"]".to_owned(),
            0,
            Some(first_fingerprint.to_vec())
        )
    );

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn automatic_fill_missing_retry_backoff_is_consumed_after_a_successful_retry()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Retry consumption", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    sqlx::query("UPDATE libraries SET realtime_metadata_auto_match_enabled = 1 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    let item_id = "retry-consumption-item";
    database
        .query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .execute(database.pool())
        .await?;

    let fingerprint = b"retry-consumption-input";
    assert!(
        database
            .prepare_item_metadata_completeness_check(item_id, "POSTER", fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(item_id, "POSTER", fingerprint)
            .await?
    );
    let result = [NewItemMetadataCompletenessResult {
        item_id,
        capability: "POSTER",
        input_fingerprint: fingerprint,
        is_missing: true,
        checked_at: 10,
    }];
    let first_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &result,
            &[item_id.to_owned()],
        )
        .await?;
    let first_job_id = first_dispatch.scheduled_job_ids[0].clone();
    fail_fill_missing_job_with_unavailable_provider(&database, &first_job_id, item_id).await?;
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET automatic_retry_after = unixepoch() - 1
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(&first_job_id)
    .bind(item_id)
    .execute(database.pool())
    .await?;

    let retry_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(&library_id, &[], &[item_id.to_owned()])
        .await?;
    assert_eq!(retry_dispatch.scheduled_job_ids.len(), 1);
    let retry_job_id = retry_dispatch.scheduled_job_ids[0].clone();
    let consumed_legacy_retry: i64 = database
        .query_scalar(
            "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(&first_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(consumed_legacy_retry, 1);
    assert!(
        database
            .claim_metadata_reidentify_job(&retry_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(&retry_job_id, 1)
            .await?,
        vec![item_id.to_owned()]
    );
    database
        .finish_metadata_reidentify_item(&retry_job_id, item_id, "COMPLETED", 1, None)
        .await?;
    database
        .finish_metadata_reidentify_job(&retry_job_id, "COMPLETED", None)
        .await?;

    let retry_check = [NewItemMetadataCompletenessCheck {
        item_id,
        capability: "POSTER",
        input_fingerprint: fingerprint,
    }];
    let (claimed_indices, due_retry_item_ids) = database
        .prepare_and_claim_item_metadata_completeness_checks_with_due_fill_missing_retries(
            &retry_check,
            &library_id,
            &[item_id.to_owned()],
        )
        .await?;
    assert!(claimed_indices.is_empty());
    assert!(
        due_retry_item_ids.is_empty(),
        "a historical expired failure must not trigger another automatic retry"
    );

    assert!(
        database
            .retry_metadata_reidentify_job(&first_job_id)
            .await?
    );
    assert!(
        database
            .claim_metadata_reidentify_job(&first_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(&first_job_id, 1)
            .await?,
        vec![item_id.to_owned()]
    );
    database
        .finish_metadata_reidentify_item(
            &first_job_id,
            item_id,
            "FAILED",
            0,
            Some("SCRAPER_UNAVAILABLE"),
        )
        .await?;
    database
        .finish_metadata_reidentify_job(
            &first_job_id,
            "DEFERRED",
            Some("DEFERRED_PROVIDER_UNAVAILABLE"),
        )
        .await?;
    let manual_retry_backoff: (i64, Option<i64>) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_consumed
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&first_job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(manual_retry_backoff, (2, Some(0)));

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn automatic_fill_missing_retry_backoff_excludes_manual_modes_and_cancelled_failures()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Fill backoff scope", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let item_ids = [
        "fill-backoff-manual",
        "fill-backoff-reidentify",
        "fill-backoff-full-refresh",
        "fill-backoff-cancelled",
    ];
    for item_id in item_ids {
        database
            .query(
                "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
    }

    let manual_job_id = database
        .create_or_merge_fill_missing_job(&library_id, &[item_ids[0].to_owned()])
        .await?;
    fail_fill_missing_job_with_unavailable_provider(&database, &manual_job_id, item_ids[0]).await?;
    for (job_id, item_id, mode) in [
        ("retry-reidentify", item_ids[1], "REIDENTIFY"),
        ("retry-full-refresh", item_ids[2], "FULL_REFRESH"),
    ] {
        database
            .create_metadata_reidentify_job(job_id, &[item_id.to_owned()], mode)
            .await?;
        fail_fill_missing_job_with_unavailable_provider(&database, job_id, item_id).await?;
    }

    let cancelled_item = item_ids[3];
    let fingerprint = b"cancelled-fill-backoff-input";
    assert!(
        database
            .prepare_item_metadata_completeness_check(cancelled_item, "POSTER", fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(cancelled_item, "POSTER", fingerprint)
            .await?
    );
    let result = [NewItemMetadataCompletenessResult {
        item_id: cancelled_item,
        capability: "POSTER",
        input_fingerprint: fingerprint,
        is_missing: true,
        checked_at: 20,
    }];
    let dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &result,
            &[cancelled_item.to_owned()],
        )
        .await?;
    let cancelled_job_id = &dispatch.scheduled_job_ids[0];
    assert!(
        database
            .claim_metadata_reidentify_job(cancelled_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(cancelled_job_id, 1)
            .await?,
        vec![cancelled_item.to_owned()]
    );
    assert!(
        database
            .request_metadata_reidentify_job_cancel(cancelled_job_id)
            .await?
    );
    database
        .finish_metadata_reidentify_item(
            cancelled_job_id,
            cancelled_item,
            "FAILED",
            0,
            Some("SCRAPER_UNAVAILABLE"),
        )
        .await?;
    database
        .finish_metadata_reidentify_job(
            cancelled_job_id,
            "DEFERRED",
            Some("DEFERRED_PROVIDER_UNAVAILABLE"),
        )
        .await?;

    let retry_states: Vec<(String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT items.item_id, items.automatic_retry_count, items.automatic_retry_after
         FROM metadata_reidentify_job_items items
         WHERE items.job_id IN (?, ?, ?, ?)
         ORDER BY items.item_id",
    )
    .bind(&manual_job_id)
    .bind("retry-reidentify")
    .bind("retry-full-refresh")
    .bind(cancelled_job_id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(retry_states.len(), 4);
    for (_, retry_count, retry_after) in retry_states {
        assert_eq!(retry_count, 0);
        assert_eq!(retry_after, None);
    }

    database.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_progressive_scan_metadata_storage_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration = DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let connection = PostgresConnection {
        database: database_name.clone(),
        ..admin_connection
    };
    let raw_database_url = DatabaseConfiguration::Postgres(connection.clone())
        .postgres_url()?
        .ok_or("missing PostgreSQL test URL")?;
    let database =
        Database::connect_with_configuration(&config, &DatabaseConfiguration::Postgres(connection))
            .await?;
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Postgres Movie (2025)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Postgres.Movie.2025.mkv"), b"video").await?;
    tokio::fs::write(media_root.join("Second.Postgres.Movie.2026.mkv"), b"video").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Postgres progressive", LibraryKind::Movie, false)
        .await?;
    let root = libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?;
    let root_id = root.root.id.to_string();
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_ids: Vec<String> = database
        .query_scalar(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY id",
        )
        .bind(library.id.to_string())
        .fetch_all(database.pool())
        .await?;
    assert_eq!(item_ids.len(), 2);
    let item_id = item_ids[0].clone();
    let replay_item_id = item_ids[1].clone();
    let scan_job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await?;
    let scan_job_id = scan_job.id;
    assert!(
        database
            .record_scan_job_file_change(
                &scan_job_id,
                &root_id,
                "Postgres Movie (2025)/Postgres.Movie.2025.srt",
                "ADD",
            )
            .await?
    );
    let file_changes = database
        .list_pending_scan_job_file_changes(&scan_job_id, None, 10)
        .await?;
    assert_eq!(file_changes.len(), 1);
    assert_eq!(file_changes[0].change_kind, "ADD");
    assert!(
        database
            .set_scan_job_file_change_state(
                &scan_job_id,
                &root_id,
                &file_changes[0].relative_path,
                file_changes[0]
                    .file_change_revision
                    .expect("file queue revision"),
                "DONE",
                None,
            )
            .await?
    );
    assert!(
        database
            .record_scan_job_file_change(
                &scan_job_id,
                &root_id,
                "Postgres Movie (2025)/Postgres.Movie.2025.srt",
                "CHANGE",
            )
            .await?
    );
    let requeued_file_changes = database
        .list_pending_scan_job_file_changes(&scan_job_id, None, 10)
        .await?;
    assert_eq!(requeued_file_changes.len(), 1);
    assert_eq!(requeued_file_changes[0].change_kind, "CHANGE");
    let manifest_id: String = sqlx::query_scalar("SELECT id FROM scan_manifests WHERE job_id = $1")
        .bind(&scan_job_id)
        .fetch_one(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_manifest_roots (
                 manifest_id, library_root_id, state, postprocessing_target_stage
             ) VALUES (?, ?, 'COMPLETE', 'NEW')
             ON CONFLICT(manifest_id, library_root_id) DO UPDATE SET
                 state = 'COMPLETE', postprocessing_target_stage = 'NEW'",
        )
        .bind(&manifest_id)
        .bind(&root_id)
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_job_targets (
                 job_id, target_type, target_id, item_id, change_kind, metadata_state
             ) VALUES (?, 'ITEM', ?, ?, 'NEW', 'PENDING')
             ON CONFLICT(job_id, target_type, target_id) DO UPDATE SET metadata_state = 'PENDING'",
        )
        .bind(&scan_job_id)
        .bind(&item_id)
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    database.reset_query_count();
    let metadata_page = database
        .load_scan_job_metadata_page(&scan_job_id, 8)
        .await?;
    assert!(metadata_page.has_pending);
    assert!(matches!(
        metadata_page.sources,
        StoredScanJobMetadataSources::Movies(ref sources)
            if sources.iter().any(|source| source.item_id == item_id)
    ));
    assert_eq!(database.query_count(), 1);
    database.reset_query_count();
    let manifest_state = database
        .get_scan_manifest_postprocessing_state_by_job(&scan_job_id)
        .await?
        .ok_or("PostgreSQL scan manifest should be present")?;
    assert!(!manifest_state.roots.is_empty());
    assert_eq!(database.query_count(), 1);

    let combined_fingerprint = b"postgres-combined-completeness-v1";
    let combined_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "COMBINED_CHECK",
        input_fingerprint: combined_fingerprint,
        is_missing: false,
        checked_at: 999,
    }];
    assert_eq!(
        database
            .complete_local_metadata_completeness_batch_with_policy(
                &library.id.to_string(),
                &combined_result,
                &[],
                MetadataAutoMatchPolicy::Disabled,
            )
            .await?
            .updated_count,
        1
    );
    assert_eq!(
        database
            .complete_local_metadata_completeness_batch_with_policy(
                &library.id.to_string(),
                &combined_result,
                &[],
                MetadataAutoMatchPolicy::Disabled,
            )
            .await?
            .updated_count,
        0,
        "the same PostgreSQL fingerprint should be complete only once"
    );
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'",
            )
            .fetch_one(database.pool())
            .await?,
        0,
        "a non-missing combined result must not create a fill-missing job"
    );

    let completeness_fingerprint_v1 = b"postgres-batch-input-v1";
    let completeness_checks = [
        NewItemMetadataCompletenessCheck {
            item_id: &item_id,
            capability: "POSTER",
            input_fingerprint: completeness_fingerprint_v1,
        },
        NewItemMetadataCompletenessCheck {
            item_id: &item_id,
            capability: "METADATA",
            input_fingerprint: completeness_fingerprint_v1,
        },
    ];
    let (check_a, check_b) = tokio::join!(
        database.prepare_and_claim_item_metadata_completeness_checks(&completeness_checks),
        database.prepare_and_claim_item_metadata_completeness_checks(&completeness_checks),
    );
    let check_a = check_a?;
    let check_b = check_b?;
    assert_eq!(check_a.len() + check_b.len(), 2);
    assert!(check_a.is_empty() || check_b.is_empty());

    const DEADLOCK_TEST_ADVISORY_KEY: i64 = 913_579_246_813_579;
    let raw_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(&raw_database_url)
        .await?;
    sqlx::query(
        r#"CREATE FUNCTION test_completeness_lock_order_trigger() RETURNS trigger
           LANGUAGE plpgsql AS $$
           BEGIN
               IF NEW.local_state = 'PENDING' AND NEW.capability = 'DEADLOCK_TEST' THEN
                   PERFORM pg_advisory_xact_lock(913579246813579);
                   PERFORM 1 FROM media_items WHERE id = NEW.item_id FOR KEY SHARE;
               END IF;
               RETURN NEW;
           END;
           $$"#,
    )
    .execute(&raw_pool)
    .await?;
    sqlx::query(
        "CREATE TRIGGER test_completeness_lock_order
         BEFORE UPDATE ON item_metadata_completeness
         FOR EACH ROW EXECUTE FUNCTION test_completeness_lock_order_trigger()",
    )
    .execute(&raw_pool)
    .await?;
    sqlx::query(
        "INSERT INTO item_metadata_completeness
             (item_id, capability, local_state, is_missing, input_fingerprint)
         VALUES ($1, 'DEADLOCK_TEST', 'RUNNING', NULL, $2)",
    )
    .bind(&replay_item_id)
    .bind(b"old".as_slice())
    .execute(&raw_pool)
    .await?;

    let mut advisory_connection = raw_pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(DEADLOCK_TEST_ADVISORY_KEY)
        .execute(&mut *advisory_connection)
        .await?;
    let mut parent_transaction = raw_pool.begin().await?;
    sqlx::query("SELECT id FROM media_items WHERE id = $1 FOR UPDATE")
        .bind(&replay_item_id)
        .fetch_one(&mut *parent_transaction)
        .await?;

    let completion_database = database.clone();
    let completion_library_id = library.id.to_string();
    let completion_item_id = replay_item_id.clone();
    let completion_task = tokio::spawn(async move {
        let results = [NewItemMetadataCompletenessResult {
            item_id: &completion_item_id,
            capability: "DEADLOCK_TEST",
            input_fingerprint: b"old",
            is_missing: false,
            checked_at: 2_000,
        }];
        completion_database
            .complete_local_metadata_and_enqueue_fill_missing_with_policy(
                &completion_library_id,
                &results,
                &[],
                MetadataAutoMatchPolicy::Disabled,
            )
            .await
    });
    let mut completion_waiting_for_parent = false;
    for _ in 0..100 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_stat_activity
             WHERE datname = current_database()
               AND wait_event_type = 'Lock'
               AND query LIKE '%SELECT id FROM media_items%'",
        )
        .fetch_one(&raw_pool)
        .await?;
        if waiting > 0 {
            completion_waiting_for_parent = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        completion_waiting_for_parent,
        "completion transaction did not reach the parent-row lock"
    );

    let claim_database = database.clone();
    let claim_item_id = replay_item_id.clone();
    let claim_task = tokio::spawn(async move {
        let checks = [NewItemMetadataCompletenessCheck {
            item_id: &claim_item_id,
            capability: "DEADLOCK_TEST",
            input_fingerprint: b"new",
        }];
        claim_database
            .prepare_and_claim_item_metadata_completeness_checks(&checks)
            .await
    });
    parent_transaction.commit().await?;

    let mut claim_waiting_for_advisory = false;
    for _ in 0..300 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_locks
             WHERE locktype = 'advisory' AND granted = false",
        )
        .fetch_one(&raw_pool)
        .await?;
        if waiting > 0 {
            claim_waiting_for_advisory = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        claim_waiting_for_advisory,
        "claim transaction did not reach the controlled parent-lock point"
    );
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(DEADLOCK_TEST_ADVISORY_KEY)
        .execute(&mut *advisory_connection)
        .await?;
    let (claim_result, completion_result) =
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(claim_task, completion_task)
        })
        .await?;
    let claim_result = claim_result.map_err(|error| std::io::Error::other(error.to_string()))??;
    let completion_result =
        completion_result.map_err(|error| std::io::Error::other(error.to_string()))??;
    assert_eq!(claim_result, vec![0]);
    assert_eq!(completion_result.updated_count, 1);
    sqlx::query(
        "DELETE FROM item_metadata_completeness
         WHERE item_id = $1 AND capability = 'DEADLOCK_TEST'",
    )
    .bind(&replay_item_id)
    .execute(&raw_pool)
    .await?;
    drop(advisory_connection);
    raw_pool.close().await;

    let completeness_results = [
        NewItemMetadataCompletenessResult {
            item_id: &item_id,
            capability: "POSTER",
            input_fingerprint: completeness_fingerprint_v1,
            is_missing: true,
            checked_at: 1_000,
        },
        NewItemMetadataCompletenessResult {
            item_id: &item_id,
            capability: "METADATA",
            input_fingerprint: completeness_fingerprint_v1,
            is_missing: false,
            checked_at: 1_000,
        },
    ];
    assert_eq!(
        database
            .complete_local_metadata_and_enqueue_fill_missing(
                &library.id.to_string(),
                &completeness_results,
                &[],
            )
            .await?
            .updated_count,
        2
    );
    assert!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&completeness_checks)
            .await?
            .is_empty()
    );

    let library_id = library.id.to_string();
    database
        .query("UPDATE libraries SET realtime_metadata_auto_match_enabled = 0 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    let replay_fingerprint = b"postgres-replay-without-new-result";
    assert!(database
        .prepare_item_metadata_completeness_check(
            &replay_item_id,
            "POSTER",
            replay_fingerprint,
        )
        .await?);
    assert!(
        database
            .claim_item_metadata_completeness_check(&replay_item_id, "POSTER", replay_fingerprint,)
            .await?
    );
    let replay_result = [NewItemMetadataCompletenessResult {
        item_id: &replay_item_id,
        capability: "POSTER",
        input_fingerprint: replay_fingerprint,
        is_missing: true,
        checked_at: 1_001,
    }];
    let policy_disabled = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &replay_result,
            std::slice::from_ref(&replay_item_id),
            MetadataAutoMatchPolicy::Disabled,
        )
        .await?;
    assert_eq!(policy_disabled.updated_count, 1);
    assert!(policy_disabled.scheduled_job_ids.is_empty());
    let default_policy_replay = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&replay_item_id),
        )
        .await?;
    assert!(default_policy_replay.scheduled_job_ids.is_empty());
    let enabled_policy_replay = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &[],
            std::slice::from_ref(&replay_item_id),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await?;
    assert_eq!(enabled_policy_replay.updated_count, 0);
    assert_eq!(enabled_policy_replay.scheduled_job_ids.len(), 1);
    let duplicate_policy_replay = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &library_id,
            &[],
            std::slice::from_ref(&replay_item_id),
            MetadataAutoMatchPolicy::Enabled,
        )
        .await?;
    assert!(duplicate_policy_replay.scheduled_job_ids.is_empty());
    let existing_fill_missing_job_id = enabled_policy_replay
        .scheduled_job_ids
        .first()
        .ok_or("enabled policy replay did not schedule a fill-missing job")?;
    assert!(
        database
            .claim_metadata_reidentify_job(existing_fill_missing_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(existing_fill_missing_job_id, 1)
            .await?,
        vec![replay_item_id.clone()]
    );
    database
        .finish_metadata_reidentify_item(
            existing_fill_missing_job_id,
            &replay_item_id,
            "COMPLETED",
            1,
            None,
        )
        .await?;
    database
        .finish_metadata_reidentify_job(existing_fill_missing_job_id, "COMPLETED", None)
        .await?;
    database
        .query("UPDATE libraries SET realtime_metadata_auto_match_enabled = 1 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;

    let failed_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&replay_item_id),
        )
        .await?;
    let failed_job_id = failed_dispatch
        .scheduled_job_ids
        .first()
        .ok_or("PostgreSQL provider-unavailable job was not scheduled")?
        .clone();
    fail_fill_missing_job_with_unavailable_provider(&database, &failed_job_id, &replay_item_id)
        .await?;
    database
        .query(
            "UPDATE metadata_reidentify_job_items
             SET automatic_retry_after = unixepoch() - 1
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(&failed_job_id)
        .bind(&replay_item_id)
        .execute(database.pool())
        .await?;
    let retry_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&replay_item_id),
        )
        .await?;
    let retry_job_id = retry_dispatch
        .scheduled_job_ids
        .first()
        .ok_or("PostgreSQL due retry was not scheduled")?
        .clone();
    let consumed_retry: i64 = database
        .query_scalar(
            "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(&failed_job_id)
        .bind(&replay_item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(consumed_retry, 1);
    assert!(
        database
            .claim_metadata_reidentify_job(&retry_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(&retry_job_id, 1)
            .await?,
        vec![replay_item_id.clone()]
    );
    database
        .finish_metadata_reidentify_item(&retry_job_id, &replay_item_id, "COMPLETED", 1, None)
        .await?;
    database
        .finish_metadata_reidentify_job(&retry_job_id, "COMPLETED", None)
        .await?;
    let retry_check = [NewItemMetadataCompletenessCheck {
        item_id: &replay_item_id,
        capability: "POSTER",
        input_fingerprint: replay_fingerprint,
    }];
    let (claimed_retry, due_retry_item_ids) = database
        .prepare_and_claim_item_metadata_completeness_checks_with_due_fill_missing_retries(
            &retry_check,
            &library_id,
            std::slice::from_ref(&replay_item_id),
        )
        .await?;
    assert!(claimed_retry.is_empty());
    assert!(due_retry_item_ids.is_empty());

    let completeness_fingerprint_v2 = b"postgres-batch-input-v2";
    let replacement_check = [NewItemMetadataCompletenessCheck {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: completeness_fingerprint_v2,
    }];
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&replacement_check)
            .await?,
        vec![0]
    );
    assert!(
        !database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                completeness_fingerprint_v1,
                true,
                1_001,
            )
            .await?
    );
    let retry_fingerprint = b"postgres-batch-retry";
    let retry_check = [NewItemMetadataCompletenessCheck {
        item_id: &item_id,
        capability: "BACKDROP",
        input_fingerprint: retry_fingerprint,
    }];
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&retry_check)
            .await?,
        vec![0]
    );
    assert!(
        database
            .fail_item_metadata_completeness_check(
                &item_id,
                "BACKDROP",
                retry_fingerprint,
                Some(2_000),
                "temporary local error",
            )
            .await?
    );
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&retry_check)
            .await?,
        vec![0]
    );
    let retry_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "BACKDROP",
        input_fingerprint: retry_fingerprint,
        is_missing: false,
        checked_at: 2_001,
    }];
    assert_eq!(
        database
            .complete_local_metadata_and_enqueue_fill_missing(
                &library.id.to_string(),
                &retry_result,
                &[],
            )
            .await?
            .updated_count,
        1
    );

    assert_eq!(
        database.ensure_scan_local_metadata_backfill_roots().await?,
        1
    );
    let (claim_a, claim_b) = tokio::join!(
        database.claim_next_scan_local_metadata_backfill_page(1),
        database.claim_next_scan_local_metadata_backfill_page(1),
    );
    let mut claimed_pages = [claim_a?, claim_b?].into_iter().flatten();
    let backfill_page = claimed_pages
        .next()
        .ok_or("PostgreSQL backfill page was not claimable")?;
    assert!(
        claimed_pages.next().is_none(),
        "concurrent PostgreSQL claimers cannot claim the same root page twice"
    );
    assert_eq!(backfill_page.library_root_id, root_id);
    assert_eq!(backfill_page.entry_ids.len(), 1);
    assert!(backfill_page.has_more);
    assert!(
        database
            .fail_scan_local_metadata_backfill_page(
                &backfill_page,
                "temporary local failure",
                Some(i64::MAX),
            )
            .await?
    );
    assert!(
        database
            .claim_next_scan_local_metadata_backfill_page(1)
            .await?
            .is_none()
    );
    database
        .query(
            "UPDATE scan_local_metadata_backfills SET next_attempt_at = 0
             WHERE library_root_id = ?",
        )
        .bind(&root_id)
        .execute(database.pool())
        .await?;
    let retry_page = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await?
        .ok_or("failed PostgreSQL page did not retry")?;
    assert_eq!(retry_page.entry_ids, backfill_page.entry_ids);
    assert_eq!(retry_page.cursor_entry_id, backfill_page.cursor_entry_id);
    assert_eq!(retry_page.attempts, 2);
    assert!(
        !database
            .complete_scan_local_metadata_backfill_page(&backfill_page)
            .await?
    );
    database
        .query(
            "UPDATE scan_local_metadata_backfills SET cursor_entry_id = 'stale-cursor'
             WHERE library_root_id = ?",
        )
        .bind(&root_id)
        .execute(database.pool())
        .await?;
    assert!(
        !database
            .complete_scan_local_metadata_backfill_page(&retry_page)
            .await?
    );
    database
        .query(
            "UPDATE scan_local_metadata_backfills SET cursor_entry_id = NULL
             WHERE library_root_id = ?",
        )
        .bind(&root_id)
        .execute(database.pool())
        .await?;
    assert!(
        database
            .complete_scan_local_metadata_backfill_page(&retry_page)
            .await?
    );
    let second_page = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await?
        .ok_or("second PostgreSQL page was not claimable")?;
    assert_eq!(
        second_page.cursor_entry_id,
        retry_page.entry_ids.first().cloned()
    );
    assert!(!second_page.has_more);
    assert_eq!(
        database
            .requeue_interrupted_scan_local_metadata_backfills()
            .await?,
        1
    );
    let recovered_page = database
        .claim_next_scan_local_metadata_backfill_page(1)
        .await?
        .ok_or("interrupted PostgreSQL page was not recovered")?;
    assert_eq!(recovered_page.entry_ids, second_page.entry_ids);
    assert_eq!(recovered_page.cursor_entry_id, second_page.cursor_entry_id);
    assert_eq!(recovered_page.attempts, 4);
    assert!(
        !database
            .complete_scan_local_metadata_backfill_page(&second_page)
            .await?
    );
    assert!(
        database
            .complete_scan_local_metadata_backfill_page(&recovered_page)
            .await?
    );
    assert!(
        !database
            .ensure_scan_local_metadata_backfill_root(&root_id)
            .await?
    );

    let source_ids = vec!["postgres-source".to_owned()];
    let batch = NewScanLocalMetadataBatch {
        id: "postgres-outbox-batch",
        job_id: "postgres-scan-job",
        library_root_id: &root_id,
        batch_sequence: 0,
        source_ids: &source_ids,
    };
    assert!(database.enqueue_scan_local_metadata_batch(batch).await?);
    assert!(!database.enqueue_scan_local_metadata_batch(batch).await?);
    let claimed = database
        .claim_next_scan_local_metadata_batch()
        .await?
        .ok_or("outbox batch was not claimable")?;
    assert_eq!(claimed.status, "RUNNING");
    assert_eq!(claimed.attempts, 1);
    assert_eq!(claimed.source_refs_json, r#"["postgres-source"]"#);
    assert_eq!(
        database
            .cancel_scan_local_metadata_batches("postgres-scan-job")
            .await?,
        1
    );
    assert!(
        !database
            .complete_scan_local_metadata_batch(&claimed.id)
            .await?
    );

    let recovery_batch = NewScanLocalMetadataBatch {
        id: "postgres-recovery-batch",
        job_id: "postgres-recovery-job",
        library_root_id: &root_id,
        batch_sequence: 0,
        source_ids: &source_ids,
    };
    assert!(
        database
            .enqueue_scan_local_metadata_batch(recovery_batch)
            .await?
    );
    assert!(
        database
            .claim_next_scan_local_metadata_batch()
            .await?
            .is_some()
    );
    assert_eq!(
        database
            .requeue_interrupted_scan_local_metadata_batches()
            .await?,
        1
    );
    let recovered = database
        .claim_next_scan_local_metadata_batch()
        .await?
        .ok_or("interrupted outbox batch was not recovered")?;
    assert_eq!(recovered.id, "postgres-recovery-batch");
    assert_eq!(recovered.attempts, 2);
    assert!(
        database
            .complete_scan_local_metadata_batch(&recovered.id)
            .await?
    );

    let old_fingerprint = b"postgres-input-v1";
    let current_fingerprint = b"postgres-input-v2";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await?
    );
    assert_eq!(
        database
            .requeue_interrupted_item_metadata_completeness_checks()
            .await?,
        1
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", old_fingerprint)
            .await?
    );
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", current_fingerprint)
            .await?
    );
    assert!(
        !database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                old_fingerprint,
                true,
                1_000,
            )
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", current_fingerprint)
            .await?
    );
    assert!(
        database
            .finish_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                current_fingerprint,
                true,
                1_001,
            )
            .await?
    );
    let missing = database
        .list_confirmed_missing_metadata("POSTER", None, 10)
        .await?;
    assert_eq!(missing.len(), 2);
    assert!(missing.iter().any(|entry| entry.item_id == item_id));

    let dispatch_fingerprint = b"postgres-dispatch-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "BACKDROP", dispatch_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "BACKDROP", dispatch_fingerprint)
            .await?
    );
    let dispatch_results = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "BACKDROP",
        input_fingerprint: dispatch_fingerprint,
        is_missing: true,
        checked_at: 1_002,
    }];
    sqlx::query(
        "CREATE FUNCTION lux_reject_fill_missing_job() RETURNS trigger AS $$
         BEGIN
             IF NEW.mode = 'FILL_MISSING' THEN
                 RAISE EXCEPTION 'injected fill-missing job failure';
             END IF;
             RETURN NEW;
         END;
         $$ LANGUAGE plpgsql",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "CREATE TRIGGER reject_fill_missing_job
         BEFORE INSERT ON metadata_reidentify_jobs
         FOR EACH ROW EXECUTE FUNCTION lux_reject_fill_missing_job()",
    )
    .execute(database.pool())
    .await?;
    // Drain the earlier queued job above so this injected failure exercises new-job insertion,
    // rather than correctly reusing that job's remaining capacity.
    assert!(
        database
            .complete_local_metadata_and_enqueue_fill_missing(
                &library_id,
                &dispatch_results,
                std::slice::from_ref(&item_id),
            )
            .await
            .is_err()
    );
    let rolled_back = database
        .find_item_metadata_completeness(&item_id, "BACKDROP")
        .await?
        .ok_or("missing rolled-back completeness row")?;
    assert_eq!(rolled_back.local_state, "RUNNING");
    assert_eq!(rolled_back.is_missing, None);
    sqlx::query("DROP TRIGGER reject_fill_missing_job ON metadata_reidentify_jobs")
        .execute(database.pool())
        .await?;
    sqlx::query("DROP FUNCTION lux_reject_fill_missing_job()")
        .execute(database.pool())
        .await?;

    let scheduled = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &dispatch_results,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert_eq!(scheduled.updated_count, 1);
    assert_eq!(scheduled.scheduled_job_ids.len(), 1);
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM metadata_reidentify_job_items
                 WHERE job_id = ? AND item_id = ? AND status = 'PENDING'",
            )
            .bind(&scheduled.scheduled_job_ids[0])
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?,
        1
    );
    let replayed = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &dispatch_results,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert_eq!(replayed.updated_count, 0);
    assert!(replayed.scheduled_job_ids.is_empty());

    fail_fill_missing_job_with_unavailable_provider(
        &database,
        &scheduled.scheduled_job_ids[0],
        &item_id,
    )
    .await?;
    let retry_pending = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(retry_pending.scheduled_job_ids.is_empty());
    let failed_retry_state: (i64, Option<i64>) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_after
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&scheduled.scheduled_job_ids[0])
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    let now: i64 = database
        .query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(failed_retry_state.0, 1);
    assert!(failed_retry_state.1.is_some_and(|retry_at| retry_at > now));

    database
        .query(
            "UPDATE metadata_reidentify_job_items
         SET automatic_retry_count = 0, automatic_retry_after = NULL
         WHERE job_id = ? AND item_id = ?",
        )
        .bind(&scheduled.scheduled_job_ids[0])
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    database
        .query(
            "UPDATE server_settings SET value = CAST(unixepoch() + 300 AS TEXT)
             WHERE key = 'metadata_fill_missing_legacy_retry_after'",
        )
        .execute(database.pool())
        .await?;
    let legacy_retry_pending = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(legacy_retry_pending.scheduled_job_ids.is_empty());
    database
        .query(
            "UPDATE server_settings SET value = CAST(unixepoch() - 1 AS TEXT)
             WHERE key = 'metadata_fill_missing_legacy_retry_after'",
        )
        .execute(database.pool())
        .await?;
    let due_retry = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert_eq!(due_retry.scheduled_job_ids.len(), 1);
    assert_ne!(
        due_retry.scheduled_job_ids[0],
        scheduled.scheduled_job_ids[0]
    );
    let inherited_retry_state: (i64, Option<i64>) = database
        .query_as(
            "SELECT automatic_retry_count, automatic_retry_after
             FROM metadata_reidentify_job_items WHERE job_id = ? AND item_id = ?",
        )
        .bind(&due_retry.scheduled_job_ids[0])
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(inherited_retry_state, (1, None));

    let queued_capacity_fingerprint = b"postgres-queued-capacity-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                &replay_item_id,
                "TRAILER",
                queued_capacity_fingerprint,
            )
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                &replay_item_id,
                "TRAILER",
                queued_capacity_fingerprint,
            )
            .await?
    );
    let queued_capacity_result = [NewItemMetadataCompletenessResult {
        item_id: &replay_item_id,
        capability: "TRAILER",
        input_fingerprint: queued_capacity_fingerprint,
        is_missing: true,
        checked_at: 1_003,
    }];
    database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &queued_capacity_result,
            std::slice::from_ref(&replay_item_id),
        )
        .await?;
    let reused_queued_job_id: String = database
        .query_scalar(
            "SELECT job_id FROM metadata_reidentify_job_items
             WHERE item_id = ? AND status = 'PENDING'
               AND job_id = ?",
        )
        .bind(&replay_item_id)
        .bind(&due_retry.scheduled_job_ids[0])
        .fetch_one(database.pool())
        .await?;
    assert_eq!(reused_queued_job_id, due_retry.scheduled_job_ids[0]);

    let still_fingerprint = b"postgres-still-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "STILL", still_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "STILL", still_fingerprint)
            .await?
    );
    let still_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "STILL",
        input_fingerprint: still_fingerprint,
        is_missing: true,
        checked_at: 1_003,
    }];
    let fill_missing_jobs_before_deduplicated: i64 = database
        .query_scalar("SELECT COUNT(*) FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'")
        .fetch_one(database.pool())
        .await?;
    let deduplicated = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &still_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert_eq!(deduplicated.updated_count, 1);
    assert!(deduplicated.scheduled_job_ids.is_empty());
    let fill_missing_jobs_after_deduplicated: i64 = database
        .query_scalar("SELECT COUNT(*) FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        fill_missing_jobs_after_deduplicated, fill_missing_jobs_before_deduplicated,
        "a deduplicated completeness result must not create another job"
    );

    let pagination_root_path = temp_dir.path().join("Pagination Movies");
    for index in 0..103 {
        let directory = format!("Pagination Movie {index:03} (2025)");
        let movie_dir = pagination_root_path.join(&directory);
        tokio::fs::create_dir_all(&movie_dir).await?;
        tokio::fs::write(
            movie_dir.join(format!("Pagination.Movie.{index:03}.2025.mkv")),
            b"video",
        )
        .await?;
    }
    let pagination_library = libraries
        .create_library("Postgres dispatch pagination", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            pagination_library.id,
            pagination_root_path
                .to_str()
                .ok_or("non-UTF8 pagination root")?,
        )
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(pagination_library.id)
        .await?;
    let pagination_library_id = pagination_library.id.to_string();
    let pagination_item_ids: Vec<String> = database
        .query_scalar(
            "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY id",
        )
        .bind(&pagination_library_id)
        .fetch_all(database.pool())
        .await?;
    assert_eq!(pagination_item_ids.len(), 103);
    let unsupported_item_id = &pagination_item_ids[0];
    let removed_item_id = &pagination_item_ids[1];
    database
        .query("UPDATE media_items SET item_type = 'FOLDER' WHERE id = ?")
        .bind(unsupported_item_id)
        .execute(database.pool())
        .await?;
    database
        .query("UPDATE media_items SET removed_at = 1 WHERE id = ?")
        .bind(removed_item_id)
        .execute(database.pool())
        .await?;
    let pagination_fingerprint = b"postgres-pagination-input-v1";
    let pagination_checks = pagination_item_ids
        .iter()
        .map(|item_id| NewItemMetadataCompletenessCheck {
            item_id,
            capability: "POSTER",
            input_fingerprint: pagination_fingerprint,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        database
            .prepare_and_claim_item_metadata_completeness_checks(&pagination_checks)
            .await?
            .len(),
        103
    );
    let pagination_results = pagination_item_ids
        .iter()
        .map(|item_id| NewItemMetadataCompletenessResult {
            item_id,
            capability: "POSTER",
            input_fingerprint: pagination_fingerprint,
            is_missing: true,
            checked_at: 1_004,
        })
        .collect::<Vec<_>>();
    let pagination_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing_with_policy(
            &pagination_library_id,
            &pagination_results,
            &pagination_item_ids,
            MetadataAutoMatchPolicy::Enabled,
        )
        .await?;
    assert_eq!(pagination_dispatch.updated_count, 102);
    assert_eq!(pagination_dispatch.scheduled_job_ids.len(), 2);
    let pagination_job_counts: Vec<i64> = database
        .query_scalar(
            "SELECT total_count FROM metadata_reidentify_jobs
             WHERE library_id = ? AND mode = 'FILL_MISSING' ORDER BY total_count DESC",
        )
        .bind(&pagination_library_id)
        .fetch_all(database.pool())
        .await?;
    assert_eq!(pagination_job_counts, vec![100, 1]);
    let pagination_job_item_ids: Vec<String> = database
        .query_scalar(
            "SELECT job_items.item_id FROM metadata_reidentify_job_items job_items
             JOIN metadata_reidentify_jobs jobs ON jobs.id = job_items.job_id
             WHERE jobs.library_id = ? AND jobs.mode = 'FILL_MISSING'",
        )
        .bind(&pagination_library_id)
        .fetch_all(database.pool())
        .await?;
    assert_eq!(pagination_job_item_ids.len(), 101);
    assert!(!pagination_job_item_ids.contains(unsupported_item_id));
    assert!(!pagination_job_item_ids.contains(removed_item_id));
    let removed_completeness = database
        .find_item_metadata_completeness(removed_item_id, "POSTER")
        .await?
        .ok_or("missing removed item completeness row")?;
    assert_eq!(removed_completeness.local_state, "RUNNING");

    let empty_root_path = temp_dir.path().join("Empty");
    tokio::fs::create_dir_all(&empty_root_path).await?;
    let empty_library = libraries
        .create_library("Postgres empty backfill", LibraryKind::Movie, false)
        .await?;
    let empty_root = libraries
        .add_root(
            empty_library.id,
            empty_root_path.to_str().ok_or("non-UTF8 empty root")?,
        )
        .await?;
    let empty_root_id = empty_root.root.id.to_string();
    assert!(
        database
            .ensure_scan_local_metadata_backfill_root(&empty_root_id)
            .await?
    );
    assert!(
        database
            .claim_next_scan_local_metadata_backfill_page(1)
            .await?
            .is_none()
    );
    database
        .query("DELETE FROM library_roots WHERE id = ?")
        .bind(&empty_root_id)
        .execute(database.pool())
        .await?;
    assert_eq!(
        database
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM scan_local_metadata_backfills WHERE library_root_id = ?",
            )
            .bind(&empty_root_id)
            .fetch_one(database.pool())
            .await?,
        0
    );

    database.close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    admin_pool.close().await;
    Ok(())
}

#[tokio::test]
async fn marking_seen_visible_media_does_not_rewrite_item_state() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root)
        .await
        .expect("media root");
    tokio::fs::write(media_root.join("Visible.Movie.2024.mkv"), b"video")
        .await
        .expect("movie file");

    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root = libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root")
        .root;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("initial index");

    sqlx::query(
        "CREATE TABLE media_item_update_audit (
             item_id TEXT NOT NULL,
             old_removed_at INTEGER,
             new_removed_at INTEGER
         )",
    )
    .execute(database.pool())
    .await
    .expect("audit table");
    sqlx::query(
        "CREATE TRIGGER audit_media_item_update
         AFTER UPDATE ON media_items
         BEGIN
             INSERT INTO media_item_update_audit (item_id, old_removed_at, new_removed_at)
             VALUES (old.id, old.removed_at, new.removed_at);
         END",
    )
    .execute(database.pool())
    .await
    .expect("audit trigger");
    let entry_id: String = sqlx::query_scalar(
        "SELECT id FROM filesystem_entries
         WHERE library_root_id = ? AND relative_path = 'Visible.Movie.2024.mkv'",
    )
    .bind(root.id.to_string())
    .fetch_one(database.pool())
    .await
    .expect("filesystem entry");
    database
        .mark_filesystem_entries_seen_batch(std::slice::from_ref(&entry_id), "next-generation")
        .await
        .expect("mark entry seen");
    let updates: Vec<(String, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT item_id, old_removed_at, new_removed_at
         FROM media_item_update_audit",
    )
    .fetch_all(database.pool())
    .await
    .expect("audit rows");
    assert!(updates.is_empty());

    sqlx::query("DELETE FROM media_item_update_audit")
        .execute(database.pool())
        .await
        .expect("clear audit rows");
    sqlx::query("UPDATE filesystem_entries SET is_missing = 1 WHERE id = ?")
        .bind(&entry_id)
        .execute(database.pool())
        .await
        .expect("mark entry missing");
    sqlx::query("DELETE FROM media_item_update_audit")
        .execute(database.pool())
        .await
        .expect("clear missing audit row");
    database
        .mark_filesystem_entries_seen_batch(std::slice::from_ref(&entry_id), "recovered-generation")
        .await
        .expect("restore entry");
    let recovery_update_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_item_update_audit")
            .fetch_one(database.pool())
            .await
            .expect("recovery audit count");
    assert_eq!(recovery_update_count, 1);
    let available: i64 = sqlx::query_scalar(
        "SELECT has_available_source FROM media_items
         WHERE id = (SELECT item_id FROM media_sources WHERE filesystem_entry_id = ?)",
    )
    .bind(&entry_id)
    .fetch_one(database.pool())
    .await
    .expect("recovered availability");
    assert_eq!(available, 1);
}

#[tokio::test]
async fn sidecar_targets_batch_multiple_directories_in_one_query() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    for (directory, file_name) in [
        ("Alpha", "Alpha.Movie.2020.mkv"),
        ("Beta", "Beta.Movie.2021.mkv"),
        ("Gamma", "Gamma.Movie.2022.mkv"),
    ] {
        let directory = media_root.join(directory);
        tokio::fs::create_dir_all(&directory)
            .await
            .expect("movie directory");
        tokio::fs::write(directory.join(file_name), b"video")
            .await
            .expect("movie file");
    }

    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root = libraries
        .add_root(library.id, media_root.to_str().expect("media root"))
        .await
        .expect("library root")
        .root;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("initial index");
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await
        .expect("scan job");

    database.reset_query_count();
    database
        .record_scan_job_sidecar_targets(
            &job.id,
            &root.id.to_string(),
            &[
                "Alpha/poster.jpg".to_owned(),
                "Beta/poster.jpg".to_owned(),
                "Gamma/poster.jpg".to_owned(),
            ],
        )
        .await
        .expect("record sidecar targets");

    assert_eq!(database.query_count(), 1);
    let target_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_job_targets
         WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await
    .expect("sidecar target count");
    assert_eq!(target_count, 3);
}

#[tokio::test]
async fn library_listing_uses_constant_number_of_child_queries() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let service = LibraryService::new(database.clone());

    for index in 0..3 {
        let library = service
            .create_library_with_scraper(
                &format!("Library {index}"),
                LibraryKind::Movie,
                false,
                Some("tmdb"),
                false,
            )
            .await
            .expect("library");
        let root = temp_dir.path().join(format!("root-{index}"));
        tokio::fs::create_dir(&root).await.expect("library root");
        service
            .add_root(library.id, root.to_str().expect("utf-8 root"))
            .await
            .expect("library root record");
    }

    database.reset_query_count();
    let views = service.list_libraries().await.expect("library views");

    assert_eq!(views.len(), 3);
    assert!(views.iter().all(|view| view.library.scrapers.len() == 1));
    assert!(views.iter().all(|view| view.roots.len() == 1));
    assert_eq!(database.query_count(), 3);
}

#[tokio::test]
async fn migration_library_identity_listing_reads_only_enabled_libraries_once() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let service = LibraryService::new(database.clone());

    let enabled = service
        .create_library("Enabled", LibraryKind::Movie, false)
        .await
        .expect("enabled library");
    let disabled = service
        .create_library("Disabled", LibraryKind::Movie, false)
        .await
        .expect("disabled library");
    let root = temp_dir.path().join("enabled-root");
    tokio::fs::create_dir(&root).await.expect("library root");
    let canonical_root = tokio::fs::canonicalize(&root)
        .await
        .expect("canonical library root");
    service
        .add_root(enabled.id, root.to_str().expect("utf-8 root"))
        .await
        .expect("enabled library root");
    sqlx::query("UPDATE libraries SET is_enabled = 0 WHERE id = ?")
        .bind(disabled.id.to_string())
        .execute(database.pool())
        .await
        .expect("disable library");

    database.reset_query_count();
    let identities = database
        .list_enabled_library_identities()
        .await
        .expect("migration identities");

    assert_eq!(database.query_count(), 1);
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0].id, enabled.id.to_string());
    assert_eq!(identities[0].name, "Enabled");
    assert_eq!(
        identities[0].root_paths,
        vec![canonical_root.to_string_lossy().into_owned()]
    );
}

#[tokio::test]
async fn system_collection_library_is_excluded_from_regular_library_workflows() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("regular library");
    let library_id = library.id.to_string();

    assert_eq!(
        database
            .list_enabled_library_ids()
            .await
            .expect("enabled media libraries"),
        vec![library_id.clone()]
    );
    let identities = database
        .list_enabled_library_identities()
        .await
        .expect("migration media-library identities");
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0].id, library_id);

    let rebuild_jobs = database
        .sync_person_index_rebuild_jobs(1)
        .await
        .expect("person rebuild jobs");
    assert_eq!(rebuild_jobs.len(), 1);
    assert_eq!(rebuild_jobs[0].library_id, library.id.to_string());

    let activity = database
        .list_library_pipeline_activity(1, 50)
        .await
        .expect("regular library activity");
    assert_eq!(activity.total_libraries, 1);
    assert_eq!(activity.libraries.len(), 1);
    assert_eq!(activity.libraries[0].id, library.id.to_string());
}

#[tokio::test]
async fn recent_catalog_rows_use_one_query_for_multiple_libraries() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let service = LibraryService::new(database.clone());
    let first = service
        .create_library("First", LibraryKind::Movie, false)
        .await
        .expect("first library");
    let second = service
        .create_library("Second", LibraryKind::Movie, false)
        .await
        .expect("second library");
    let first_id = first.id.to_string();
    let second_id = second.id.to_string();

    for (item_id, library_id, title, added_at) in [
        ("recent-first-old", &first_id, "First old movie", 10_i64),
        ("recent-first-new", &first_id, "First new movie", 20_i64),
        ("recent-second-old", &second_id, "Second old movie", 5_i64),
        ("recent-second-new", &second_id, "Second new movie", 15_i64),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, added_at, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?, 1)",
        )
        .bind(item_id)
        .bind(library_id)
        .bind(title)
        .bind(title.to_ascii_lowercase())
        .bind(added_at)
        .execute(database.pool())
        .await
        .expect("media item");
    }

    database.reset_query_count();
    let rows = database
        .list_recent_catalog_rows_by_library(&[first_id, second_id], 1)
        .await
        .expect("recent catalog rows");

    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<std::collections::HashSet<_>>(),
        std::collections::HashSet::from(["recent-first-new", "recent-second-new"])
    );
    assert_eq!(database.query_count(), 1);
}

#[tokio::test]
async fn recent_catalog_rows_include_visible_unavailable_series() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Series", LibraryKind::Series, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();

    for (item_id, title, added_at) in [
        ("recent-series-movie-old", "Old movie", 10_i64),
        ("recent-series-movie-new", "New movie", 20_i64),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, added_at, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?, 1)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(title)
        .bind(title.to_ascii_lowercase())
        .bind(added_at)
        .execute(database.pool())
        .await
        .expect("movie");
    }
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, added_at, has_available_source
             ) VALUES ('recent-visible-series', ?, 'SERIES',
                       'Visible series', 'visible series', 'LOCAL_CONFIRMED', 30, 0)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("unavailable series");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, parent_id, title, sort_title,
                identification_status, added_at, has_available_source
             ) VALUES ('recent-visible-episode', ?, 'EPISODE',
                       'recent-visible-series', 'Visible episode', 'visible episode',
                       'LOCAL_CONFIRMED', 40, 1)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("visible episode");

    let rows = database
        .list_recent_catalog_rows_by_library(&[library_id], 2)
        .await
        .expect("recent catalog rows");

    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["recent-visible-series", "recent-series-movie-new"]
    );
}

#[tokio::test]
async fn recent_catalog_rows_promote_series_when_a_new_episode_is_added() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Series", LibraryKind::Series, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();

    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, added_at, has_available_source
             ) VALUES ('recent-series-movie', ?, 'MOVIE', 'Movie', 'movie',
                       'LOCAL_CONFIRMED', 20, 1)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("movie");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, added_at, has_available_source
             ) VALUES ('recent-series', ?, 'SERIES', 'Series', 'series',
                       'LOCAL_CONFIRMED', 10, 0)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("series");
    for (item_id, title, added_at) in [
        ("recent-series-episode-old", "Episode 1", 10_i64),
        ("recent-series-episode-new", "Episode 2", 30_i64),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, parent_id, series_id,
                    title, sort_title, identification_status, added_at,
                    has_available_source
                 ) VALUES (?, ?, 'EPISODE', 'recent-series', 'recent-series',
                           ?, ?, 'LOCAL_CONFIRMED', ?, 1)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(title)
        .bind(title.to_ascii_lowercase())
        .bind(added_at)
        .execute(database.pool())
        .await
        .expect("episode");
    }

    let rows = database
        .list_recent_catalog_rows_by_library(&[library_id], 2)
        .await
        .expect("recent catalog rows");

    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["recent-series", "recent-series-movie"]
    );
}

#[tokio::test]
async fn recommended_catalog_rows_stop_awarding_freshness_after_seven_days() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await
        .expect("current timestamp");
    let library_id = library.id.to_string();
    let user_id = user.id.to_string();

    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, added_at, has_available_source
             ) VALUES
                ('item-fifteen-days-old', ?, 'MOVIE', 'Fifteen Days Old', 'fifteen days old', 'LOCAL_CONFIRMED', ?, 1),
                ('item-new', ?, 'MOVIE', 'New Movie', 'new movie', 'LOCAL_CONFIRMED', ?, 1)",
    )
    .bind(&library_id)
    .bind(now - 15 * 86_400)
    .bind(&library_id)
    .bind(now)
    .execute(database.pool())
    .await
    .expect("media items");
    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id)
         VALUES (?, 'item-new')",
    )
    .bind(&user_id)
    .execute(database.pool())
    .await
    .expect("user item state");
    refresh_recommendation_stats(&database).await;

    let rows = database
        .list_recommended_catalog_rows(&user_id, &[library_id], 0, 2)
        .await
        .expect("recommended catalog rows");

    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        ["item-fifteen-days-old", "item-new"]
    );
}

#[tokio::test]
async fn recommended_catalog_rows_limit_recent_playback_items_and_remove_old_state_bonuses() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
        .await
        .expect("library root");
    let root_id: String = sqlx::query_scalar("SELECT id FROM library_roots LIMIT 1")
        .fetch_one(database.pool())
        .await
        .expect("library root id");
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await
        .expect("current timestamp");
    let library_id = library.id.to_string();
    let user_id = user.id.to_string();
    let item_ids = [
        "active-1",
        "active-2",
        "active-3",
        "active-4",
        "active-5",
        "active-6",
        "unplayed-1",
        "unplayed-2",
        "favorite-only",
    ];

    for item_id in item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, added_at, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?, 1)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .bind(now - 15 * 86_400)
        .execute(database.pool())
        .await
        .expect("media item");
    }

    for (entry_id, source_id, relative_path) in [
        ("active-1-entry-a", "active-1-source-a", "active-1-a.mkv"),
        ("active-1-entry-b", "active-1-source-b", "active-1-b.mkv"),
    ] {
        sqlx::query(
            "INSERT INTO filesystem_entries
             (id, library_root_id, relative_path, entry_kind, size, modified_at, last_seen_generation)
             VALUES (?, ?, ?, 'FILE', 1, 1, 'generation')",
        )
        .bind(entry_id)
        .bind(&root_id)
        .bind(relative_path)
        .execute(database.pool())
        .await
        .expect("filesystem entry");
        sqlx::query(
            "INSERT INTO media_sources (id, item_id, source_kind, filesystem_entry_id)
             VALUES (?, 'active-1', 'LOCAL_FILE', ?)",
        )
        .bind(source_id)
        .bind(entry_id)
        .execute(database.pool())
        .await
        .expect("media source");
    }

    for item_id in [
        "active-1", "active-2", "active-3", "active-4", "active-5", "active-6",
    ] {
        sqlx::query(
            "INSERT INTO user_item_state (
                    user_id, item_id, position_ticks, is_favorite, last_played_at
                 ) VALUES (?, ?, 1, 1, ?)",
        )
        .bind(&user_id)
        .bind(item_id)
        .bind(now)
        .execute(database.pool())
        .await
        .expect("active user item state");
    }
    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, is_favorite)
         VALUES (?, 'favorite-only', 1)",
    )
    .bind(&user_id)
    .execute(database.pool())
    .await
    .expect("favorite user item state");
    refresh_recommendation_stats(&database).await;

    let rows = database
        .list_recommended_catalog_rows(&user_id, &[library_id], 0, 7)
        .await
        .expect("recommended catalog rows");

    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<std::collections::HashSet<_>>(),
        std::collections::HashSet::from([
            "active-1",
            "active-2",
            "active-3",
            "active-4",
            "active-5",
            "unplayed-1",
            "unplayed-2",
        ])
    );
}

#[tokio::test]
async fn recommended_catalog_rows_cap_engagement_scores_and_expire_old_playback() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await
        .expect("current timestamp");
    let playback_cutoff = now - 180 * 86_400;
    let library_id = library.id.to_string();
    let user_id = user.id.to_string();
    let item_ids = [
        "favorite-11",
        "favorite-20",
        "play-51",
        "play-60",
        "play-expired",
        "baseline",
    ];

    for item_id in item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, added_at, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?, 1)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .bind(now - 15 * 86_400)
        .execute(database.pool())
        .await
        .expect("media item");
    }

    for index in 0..60 {
        let playback_user_id = format!("recommendation-user-{index}");
        sqlx::query(
            "INSERT INTO users (
                    id, username_normalized, display_name, password_hash
                 ) VALUES (?, ?, ?, 'test')",
        )
        .bind(&playback_user_id)
        .bind(&playback_user_id)
        .bind(&playback_user_id)
        .execute(database.pool())
        .await
        .expect("playback user");

        if index < 51 {
            sqlx::query(
                "INSERT INTO user_item_state (user_id, item_id, last_played_at)
                 VALUES (?, 'play-51', ?)",
            )
            .bind(&playback_user_id)
            .bind(now)
            .execute(database.pool())
            .await
            .expect("play-51 state");
        }
        sqlx::query(
            "INSERT INTO user_item_state (user_id, item_id, last_played_at)
             VALUES (?, 'play-60', ?)",
        )
        .bind(&playback_user_id)
        .bind(now)
        .execute(database.pool())
        .await
        .expect("play-60 state");
        sqlx::query(
            "INSERT INTO user_item_state (user_id, item_id, last_played_at)
             VALUES (?, 'play-expired', ?)",
        )
        .bind(&playback_user_id)
        .bind(playback_cutoff)
        .execute(database.pool())
        .await
        .expect("expired playback state");
        if index < 11 {
            sqlx::query(
                "INSERT INTO user_item_state (user_id, item_id, is_favorite)
                 VALUES (?, 'favorite-11', 1)",
            )
            .bind(&playback_user_id)
            .execute(database.pool())
            .await
            .expect("favorite-11 state");
        }
        if index < 20 {
            sqlx::query(
                "INSERT INTO user_item_state (user_id, item_id, is_favorite)
                 VALUES (?, 'favorite-20', 1)",
            )
            .bind(&playback_user_id)
            .execute(database.pool())
            .await
            .expect("favorite-20 state");
        }
    }

    refresh_recommendation_stats(&database).await;

    let rows = database
        .list_recommended_catalog_rows(&user_id, &[library_id], 0, 5)
        .await
        .expect("recommended catalog rows");

    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        [
            "favorite-11",
            "favorite-20",
            "play-51",
            "play-60",
            "baseline",
        ]
    );
}

#[tokio::test]
async fn recommended_catalog_rows_use_rating_median_for_missing_ratings() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user = SetupService::new(database.clone())
        .expect("setup service")
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await
        .expect("current timestamp");
    let library_id = library.id.to_string();
    let user_id = user.id.to_string();

    for (item_id, rating) in [
        ("rating-low", Some(0.0_f64)),
        ("rating-top", Some(10.0_f64)),
        ("rating-unknown", None),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, added_at, has_available_source, rating
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?, 1, ?)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .bind(now - 15 * 86_400)
        .bind(rating)
        .execute(database.pool())
        .await
        .expect("rated media item");
    }
    refresh_recommendation_stats(&database).await;

    database.reset_query_count();
    let rows = database
        .list_recommended_catalog_rows(&user_id, std::slice::from_ref(&library_id), 0, 3)
        .await
        .expect("recommended catalog rows");
    let first_query_count = database.query_count();
    database.reset_query_count();
    let cached_rows = database
        .list_recommended_catalog_rows(&user_id, std::slice::from_ref(&library_id), 0, 3)
        .await
        .expect("cached recommended catalog rows");

    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        ["rating-top", "rating-unknown", "rating-low"]
    );
    assert_eq!(first_query_count, 4);
    assert_eq!(database.query_count(), 1);
    assert_eq!(
        rows.iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        cached_rows
            .iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>()
    );

    sqlx::query("UPDATE media_items SET updated_at = 0 WHERE id = 'rating-low'")
        .execute(database.pool())
        .await
        .expect("reset metadata timestamp");
    database
        .update_media_item_metadata(MediaMetadataUpdate {
            item_id: "rating-low",
            title: "rating-low",
            original_title: None,
            overview: None,
            production_year: None,
            premiere_date: None,
            rating: Some(10.0),
            rating_source: Some("TEST"),
            provider_ids_json: None,
            metadata_fingerprint: &[],
            provenance_json: "{}",
            locked_fields_json: "{}",
        })
        .await
        .expect("updated rating");
    let updated_at: i64 =
        sqlx::query_scalar("SELECT updated_at FROM media_items WHERE id = 'rating-low'")
            .fetch_one(database.pool())
            .await
            .expect("metadata timestamp");
    assert!(updated_at > 0);
    sqlx::query("UPDATE media_items SET updated_at = 123 WHERE id = 'rating-low'")
        .execute(database.pool())
        .await
        .expect("set stable metadata timestamp");
    database
        .update_media_item_metadata(MediaMetadataUpdate {
            item_id: "rating-low",
            title: "rating-low",
            original_title: None,
            overview: None,
            production_year: None,
            premiere_date: None,
            rating: Some(10.0),
            rating_source: Some("TEST"),
            provider_ids_json: None,
            metadata_fingerprint: &[],
            provenance_json: "{}",
            locked_fields_json: "{}",
        })
        .await
        .expect("no-op metadata update");
    let unchanged_at: i64 =
        sqlx::query_scalar("SELECT updated_at FROM media_items WHERE id = 'rating-low'")
            .fetch_one(database.pool())
            .await
            .expect("no-op metadata timestamp");
    assert_eq!(unchanged_at, 123);
    database.reset_query_count();
    let refreshed_rows = database
        .list_recommended_catalog_rows(&user_id, std::slice::from_ref(&library_id), 0, 3)
        .await
        .expect("refreshed recommended catalog rows");
    assert_eq!(database.query_count(), 1);
    assert_eq!(
        refreshed_rows
            .iter()
            .map(|row| row.item_id.as_str())
            .collect::<Vec<_>>(),
        ["rating-low", "rating-top", "rating-unknown"]
    );
}

#[tokio::test]
async fn selecting_metadata_candidate_keeps_recommendation_rating_median_until_ttl() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Ratings", LibraryKind::Movie, false)
        .await
        .expect("library");
    let item_id = "rating-selection-item";
    let candidate_id = "rating-selection-candidate";
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, has_available_source, rating
             ) VALUES (?, ?, 'MOVIE', 'Rating selection', 'rating selection',
                       'LOCAL_CONFIRMED', 1, 1.0)",
    )
    .bind(item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    sqlx::query(
        "INSERT INTO metadata_candidates (
                id, item_id, provider, provider_id, candidate_json, score, status
             ) VALUES (?, ?, 'TMDB', 'rating-selection', '{}', 100, 'PENDING')",
    )
    .bind(candidate_id)
    .bind(item_id)
    .execute(database.pool())
    .await
    .expect("metadata candidate");

    assert_eq!(
        database
            .recommendation_rating_median(&[library.id.to_string()])
            .await
            .expect("initial median"),
        1.0
    );
    assert!(
        database
            .select_metadata_candidate(SelectedMetadataUpdate {
                item_id,
                candidate_id,
                title: "Rating selection",
                original_title: None,
                overview: None,
                production_year: None,
                premiere_date: None,
                last_air_date: None,
                status: None,
                original_language: None,
                rating: Some(9.0),
                rating_source: Some("TMDB"),
                provider_ids_json: "{}",
                metadata_scraper_id: None,
                metadata_fingerprint: &[],
                provenance_json: "{}",
                locked_fields_json: "[]",
                poster_fallback_required: false,
                keep_pending: false,
            })
            .await
            .expect("select metadata candidate")
    );
    assert_eq!(
        database
            .recommendation_rating_median(&[library.id.to_string()])
            .await
            .expect("cached median"),
        1.0
    );
}

#[tokio::test]
async fn recommendation_rating_median_survives_restart_until_thirty_day_ttl() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Ratings", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                identification_status, has_available_source, rating
             ) VALUES ('persistent-rating-item', ?, 'MOVIE', 'Persistent rating',
                       'persistent rating', 'LOCAL_CONFIRMED', 1, 7.0)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("media item");
    assert_eq!(
        database
            .recommendation_rating_median(std::slice::from_ref(&library_id))
            .await
            .expect("initial median"),
        7.0
    );
    database.close().await;

    let restarted = Database::connect(&config)
        .await
        .expect("restarted database");
    sqlx::query("UPDATE media_items SET rating = 9.0 WHERE id = 'persistent-rating-item'")
        .execute(restarted.pool())
        .await
        .expect("updated rating");
    assert_eq!(
        restarted
            .recommendation_rating_median(std::slice::from_ref(&library_id))
            .await
            .expect("persistent median"),
        7.0
    );
    sqlx::query(
        "UPDATE recommendation_rating_cache
         SET calculated_at = unixepoch() - 30 * 86400",
    )
    .execute(restarted.pool())
    .await
    .expect("expired median cache");
    restarted.close().await;
    let expired = Database::connect(&config)
        .await
        .expect("expired cache database");
    assert_eq!(
        expired
            .recommendation_rating_median(std::slice::from_ref(&library_id))
            .await
            .expect("refreshed median"),
        9.0
    );
}

#[tokio::test]
async fn pending_metadata_candidates_load_current_items_in_one_batch() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Metadata candidates", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    for (index, title) in [(1, "First"), (2, "Second")] {
        let item_id = format!("candidate-item-{index}");
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'PENDING')",
        )
        .bind(&item_id)
        .bind(&library_id)
        .bind(title)
        .bind(title.to_ascii_lowercase())
        .execute(database.pool())
        .await
        .expect("media item");
        sqlx::query(
            "INSERT INTO metadata_candidates (
                    id, item_id, provider, provider_id, candidate_json, score, status
                 ) VALUES (?, ?, 'TMDB', ?, ?, 80, 'PENDING')",
        )
        .bind(format!("candidate-{index}"))
        .bind(&item_id)
        .bind(index.to_string())
        .bind(serde_json::json!({"title": title}).to_string())
        .execute(database.pool())
        .await
        .expect("metadata candidate");
    }

    database.reset_query_count();
    let page = MetadataCandidateService::new(database.clone())
        .list_pending(0, 50)
        .await
        .expect("pending candidates");

    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].field_diffs.len(), 0);
    assert_eq!(page.items[1].field_diffs.len(), 0);
    assert_eq!(database.query_count(), 3);
}

#[tokio::test]
async fn automatic_candidate_summary_keeps_the_best_results_from_the_existing_page_window() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Automatic candidate summary", LibraryKind::Movie, false)
        .await
        .expect("library");
    database
        .query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('automatic-summary-item', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await
        .expect("media item");

    for index in 0..60 {
        let score = match index {
            49 => 99.0,
            40 => 98.0,
            59 => 100.0,
            _ => index as f64,
        };
        database
            .query(
                "INSERT INTO metadata_candidates (
                    id, item_id, provider, provider_id, candidate_json, score, status
                 ) VALUES (?, 'automatic-summary-item', 'tmdb', ?, '{}', ?, 'PENDING')",
            )
            .bind(format!("candidate-{index:03}"))
            .bind(format!("movie-{index}"))
            .bind(score)
            .execute(database.pool())
            .await
            .expect("metadata candidate");
    }

    database.reset_query_count();
    let (candidates, total_count) = database
        .list_best_pending_metadata_candidates_for_item("automatic-summary-item", 50, 2)
        .await
        .expect("automatic candidate summary");
    assert_eq!(database.query_count(), 1);
    assert_eq!(total_count, 60);
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.id.as_str())
            .collect::<Vec<_>>(),
        ["candidate-049", "candidate-040"],
        "the summary ranks only the same first-50 page the previous loader exposed"
    );

    database.close().await;
}

#[tokio::test]
async fn collection_refresh_uses_provider_index_and_batch_insert() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Collections", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    for index in 1..=3 {
        let item_id = format!("collection-movie-{index}");
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, provider_ids_json,
                    identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, ?, 'ONLINE_CONFIRMED')",
        )
        .bind(&item_id)
        .bind(&library_id)
        .bind(format!("Movie {index}"))
        .bind(format!("movie {index}"))
        .bind(serde_json::json!({"tmdb": index.to_string()}).to_string())
        .execute(database.pool())
        .await
        .expect("media item");
    }
    let member_provider_ids = (1..=3)
        .map(|index| ("TMDB".to_owned(), index.to_string(), index))
        .collect::<Vec<_>>();

    database.reset_query_count();
    let result = database
        .upsert_collection(NewCollection {
            library_id: &library_id,
            provider: "tmdb",
            provider_id: "collection-1",
            title: "Collection",
            overview: None,
            poster_path: None,
            backdrop_path: None,
            member_provider_ids: &member_provider_ids,
        })
        .await
        .expect("collection refresh");

    assert_eq!(result.member_count, 3);
    assert_eq!(database.query_count(), 8);
    let member_ids = sqlx::query_scalar::<_, String>(
        "SELECT item_id FROM collection_items
         WHERE collection_id = (SELECT id FROM collections WHERE provider_id = 'collection-1')
         ORDER BY sort_order",
    )
    .fetch_all(database.pool())
    .await
    .expect("collection members");
    assert_eq!(
        member_ids,
        vec![
            "collection-movie-1".to_owned(),
            "collection-movie-2".to_owned(),
            "collection-movie-3".to_owned(),
        ]
    );
}

#[tokio::test]
async fn emby_collection_member_mutations_use_bounded_batches() {
    const ITEM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Manual collections", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    let other_library = LibraryService::new(database.clone())
        .create_library("Other manual collections", LibraryKind::Movie, false)
        .await
        .expect("other library");
    let item_ids = (0..ITEM_COUNT)
        .map(|index| format!("manual-collection-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    let foreign_item_id = "manual-collection-foreign";
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
    )
    .bind(foreign_item_id)
    .bind(other_library.id.to_string())
    .bind(foreign_item_id)
    .bind(foreign_item_id)
    .execute(database.pool())
    .await
    .expect("foreign media item");
    let removed_item_id = "manual-collection-removed";
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status, removed_at
         ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', unixepoch())",
    )
    .bind(removed_item_id)
    .bind(&library_id)
    .bind(removed_item_id)
    .bind(removed_item_id)
    .execute(database.pool())
    .await
    .expect("removed media item");

    let collection = database
        .create_emby_collection("Manual batch", &[item_ids[0].clone()])
        .await
        .expect("create collection")
        .expect("collection");
    let mut requested_ids = item_ids.clone();
    requested_ids.extend([foreign_item_id.to_owned(), removed_item_id.to_owned()]);

    database.reset_query_count();
    database
        .add_emby_collection_items(&collection.collection_item_id, &requested_ids)
        .await
        .expect("add collection members")
        .expect("collection exists");
    assert_eq!(database.query_count(), 5);
    let stored_ids: Vec<String> = sqlx::query_scalar(
        "SELECT mi.id
         FROM collection_items ci
         JOIN collections c ON c.id = ci.collection_id
         JOIN media_items mi ON mi.id = ci.item_id
         WHERE c.item_id = ?
         ORDER BY ci.sort_order, mi.id",
    )
    .bind(&collection.collection_item_id)
    .fetch_all(database.pool())
    .await
    .expect("stored collection members");
    assert_eq!(stored_ids, item_ids);

    database.reset_query_count();
    database
        .remove_emby_collection_items(&collection.collection_item_id, &requested_ids)
        .await
        .expect("remove collection members")
        .expect("collection exists");
    assert_eq!(database.query_count(), 2);
    let remaining: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM collection_items ci
         JOIN collections c ON c.id = ci.collection_id
         WHERE c.item_id = ?",
    )
    .bind(&collection.collection_item_id)
    .fetch_one(database.pool())
    .await
    .expect("remaining collection members");
    assert_eq!(remaining, 0);

    database.close().await;
}

#[tokio::test]
async fn favorite_catalog_filter_uses_favorite_state_index() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library_ids = vec!["library".to_owned()];
    let empty_item_types = Vec::new();
    let empty_excluded_item_types = Vec::new();
    let empty_years = Vec::new();
    let filter = CatalogFilterQuery {
        library_ids: &library_ids,
        user_id: "user",
        item_types: &empty_item_types,
        excluded_item_types: &empty_excluded_item_types,
        item_ids: None,
        person_id: None,
        media_source_ids: None,
        provider_id_equals: None,
        years: &empty_years,
        is_played: None,
        is_favorite: Some(true),
        min_date_last_saved: None,
        metadata_pending: false,
        parent_id_scope: None,
        content_restrictions: None,
        sort_by: CatalogSort::DateCreated,
        descending: true,
        offset: 0,
        limit: 24,
    };
    let (where_clause, binds) = catalog_filter_where_clause(&filter);
    let query = format!(
        "EXPLAIN QUERY PLAN
             SELECT COUNT(*) FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             {where_clause}"
    );
    let mut statement = database.query(sqlx::AssertSqlSafe(query));
    for bind in &binds {
        statement = match bind {
            CatalogBind::Text(value) => statement.bind(*value),
            CatalogBind::Integer(value) => statement.bind(*value),
            CatalogBind::Real(value) => statement.bind(*value),
        };
    }
    let plan = statement
        .fetch_all(database.pool())
        .await
        .expect("favorite query plan")
        .into_iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>();

    assert!(
        plan.iter()
            .any(|detail| detail.contains("idx_user_item_state_favorites")),
        "favorite query did not use the favorite state index: {plan:?}"
    );
}

#[tokio::test]
async fn content_restrictions_filter_catalog_rows_before_pagination()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::storage::UserContentRestrictions;

    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Content restriction filter", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();

    let items = [
        ("01-allowed", "MOVIE", None, None),
        ("02-too-high", "MOVIE", None, None),
        ("03-not-allowed", "MOVIE", None, None),
        ("04-blocked", "MOVIE", None, None),
        ("05-unrated", "MOVIE", None, None),
        ("06-unindexed", "MOVIE", None, None),
        ("07-restricted-series", "SERIES", None, None),
        (
            "08-parent-season",
            "SEASON",
            Some("07-restricted-series"),
            Some("07-restricted-series"),
        ),
        (
            "09-inherited",
            "EPISODE",
            Some("08-parent-season"),
            Some("07-restricted-series"),
        ),
        ("10-allowed", "MOVIE", None, None),
    ];
    for (item_id, item_type, parent_id, series_id) in items {
        database
            .query(
                "INSERT INTO media_items (
                     id, library_id, item_type, parent_id, series_id,
                     title, sort_title, identification_status, has_available_source
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', 1)",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_type)
            .bind(parent_id)
            .bind(series_id)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
    }

    for (item_id, certification) in [
        ("01-allowed", Some("PG-13")),
        ("02-too-high", Some("R")),
        ("03-not-allowed", Some("PG")),
        ("04-blocked", Some("PG")),
        ("05-unrated", None),
        ("07-restricted-series", Some("R")),
        ("08-parent-season", Some("G")),
        ("09-inherited", Some("G")),
        ("10-allowed", Some("G")),
    ] {
        database
            .query(
                "INSERT INTO media_content_restriction_index (item_id, certification_code)
                 VALUES (?, ?)",
            )
            .bind(item_id)
            .bind(certification)
            .execute(database.pool())
            .await?;
    }
    for item_id in [
        "01-allowed",
        "02-too-high",
        "04-blocked",
        "05-unrated",
        "09-inherited",
        "10-allowed",
    ] {
        database
            .query("INSERT INTO media_content_restriction_tags (item_id, tag) VALUES (?, 'action')")
            .bind(item_id)
            .execute(database.pool())
            .await?;
    }
    database
        .query(
            "INSERT INTO media_content_restriction_tags (item_id, tag) VALUES ('03-not-allowed', 'family')",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO media_content_restriction_tags (item_id, tag)
             VALUES ('04-blocked', 'violent')",
        )
        .execute(database.pool())
        .await?;

    let restrictions = UserContentRestrictions {
        rating_system: "MPA".to_owned(),
        maximum_rating: Some("PG-13".to_owned()),
        allow_unrated: false,
        allowed_tags: vec!["action".to_owned()],
        blocked_tags: vec!["violent".to_owned()],
        version: 4,
        updated_at: 1,
    };
    let library_ids = vec![library_id];
    let empty_item_types = Vec::new();
    let empty_excluded_item_types = Vec::new();
    let empty_years = Vec::new();
    let make_filter = |offset, limit| CatalogFilterQuery {
        library_ids: &library_ids,
        user_id: "policy-user",
        item_types: &empty_item_types,
        excluded_item_types: &empty_excluded_item_types,
        item_ids: None,
        person_id: None,
        media_source_ids: None,
        provider_id_equals: None,
        years: &empty_years,
        is_played: None,
        is_favorite: None,
        min_date_last_saved: None,
        metadata_pending: false,
        parent_id_scope: None,
        content_restrictions: Some(&restrictions),
        sort_by: CatalogSort::Name,
        descending: false,
        offset,
        limit,
    };

    let unrestricted_query = CatalogFilterQuery {
        content_restrictions: None,
        ..make_filter(0, 20)
    };
    let (_, baseline_total) = database
        .list_filtered_catalog_rows(&unrestricted_query)
        .await?;
    assert_eq!(baseline_total, 10, "fixture exposes every inserted item");

    let (first_page, total) = database
        .list_filtered_catalog_rows(&make_filter(0, 1))
        .await?;
    assert_eq!(total, 2, "only policy-eligible items contribute to total");
    assert_eq!(first_page.len(), 1);
    assert_eq!(first_page[0].item_id, "01-allowed");
    let (second_page, second_total) = database
        .list_filtered_catalog_rows(&make_filter(1, 1))
        .await?;
    assert_eq!(second_total, total);
    assert_eq!(second_page.len(), 1);
    assert_eq!(second_page[0].item_id, "10-allowed");
    let (past_end, third_total) = database
        .list_filtered_catalog_rows(&make_filter(2, 1))
        .await?;
    assert_eq!(third_total, total);
    assert!(past_end.is_empty());

    let unrestricted = UserContentRestrictions {
        rating_system: "MPA".to_owned(),
        maximum_rating: None,
        allow_unrated: true,
        allowed_tags: Vec::new(),
        blocked_tags: Vec::new(),
        version: 1,
        updated_at: 1,
    };
    let unrestricted_filter = CatalogFilterQuery {
        content_restrictions: Some(&unrestricted),
        ..make_filter(0, 20)
    };
    let (_, unrestricted_total) = database
        .list_filtered_catalog_rows(&unrestricted_filter)
        .await?;
    assert_eq!(unrestricted_total, 10, "a non-restrictive row is a no-op");

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn recommendation_query_uses_materialized_stats_and_image_indexes() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Recommendation plan", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    let query = format!(
        "EXPLAIN QUERY PLAN
         SELECT mi.id
                , COALESCE(rs.recent_playback_score, 0)
         FROM media_items mi
         JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
         LEFT JOIN recommendation_item_stats rs ON rs.item_id = mi.id
         LEFT JOIN user_item_state us
           ON us.item_id = mi.id AND us.user_id = ?
         WHERE mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
           AND mi.item_type IN ('MOVIE', 'SERIES')
           AND mi.library_id IN (?)"
    );
    let plan = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind("plan-user")
        .bind(&library_id)
        .fetch_all(database.pool())
        .await
        .expect("recommendation query plan")
        .into_iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>();
    assert!(
        plan.iter()
            .any(|detail| detail.contains("recommendation_item_stats")),
        "recommendation query did not use materialized stats: {plan:?}"
    );

    let image_plan = sqlx::query(
        "EXPLAIN QUERY PLAN
         SELECT (SELECT id FROM item_images
                 WHERE item_id = ? AND image_type = 'POSTER'
                 ORDER BY image_index LIMIT 1)",
    )
    .bind("plan-item")
    .fetch_all(database.pool())
    .await
    .expect("image query plan")
    .into_iter()
    .map(|row| row.get::<String, _>("detail"))
    .collect::<Vec<_>>();
    assert!(
        image_plan
            .iter()
            .any(|detail| detail.contains("idx_item_images_recommendation_lookup")),
        "recommendation image lookup did not use the covering index: {image_plan:?}"
    );
}

#[tokio::test]
async fn concurrent_metadata_capability_writes_are_serialized() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let item_id = "metadata-write-item";
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Metadata', 'metadata', 'LOCAL_CONFIRMED')",
    )
    .bind(item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");

    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..32 {
        let database = database.clone();
        tasks.spawn(async move {
            let results = std::iter::repeat_with(|| MetadataCapabilityResult {
                capability: "CREDITS",
                has_data: true,
            })
            .take(128)
            .collect::<Vec<_>>();
            database
                .record_metadata_capability_results(
                    item_id,
                    "tmdb",
                    &format!("{index}"),
                    &results,
                    1_000 + index,
                )
                .await
        });
    }

    while let Some(result) = tasks.join_next().await {
        result
            .expect("metadata writer task should not panic")
            .expect("metadata writes should not fail under concurrency");
    }
    database.close().await;
}

#[tokio::test]
async fn metadata_candidate_batch_uses_one_statement_and_preserves_upsert_behavior() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('candidate-batch-item', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");

    let candidates = [
        NewMetadataCandidate {
            id: "candidate-first-id",
            item_id: "candidate-batch-item",
            provider: "tmdb",
            provider_id: "movie-1",
            candidate_json: r#"{"title":"Lower score"}"#,
            score: 70.0,
            expires_at: Some(1_000),
        },
        NewMetadataCandidate {
            id: "candidate-second-id",
            item_id: "candidate-batch-item",
            provider: "tmdb",
            provider_id: "movie-1",
            candidate_json: r#"{"title":"Higher score, first result"}"#,
            score: 90.0,
            expires_at: Some(2_000),
        },
        NewMetadataCandidate {
            id: "candidate-third-id",
            item_id: "candidate-batch-item",
            provider: "tmdb",
            provider_id: "movie-1",
            candidate_json: r#"{"title":"Higher score, later tie"}"#,
            score: 90.0,
            expires_at: Some(2_500),
        },
        NewMetadataCandidate {
            id: "candidate-other-id",
            item_id: "candidate-batch-item",
            provider: "tmdb",
            provider_id: "movie-2",
            candidate_json: r#"{"title":"Other candidate"}"#,
            score: 60.0,
            expires_at: Some(3_000),
        },
    ];

    database.reset_query_count();
    database
        .insert_metadata_candidates(&candidates)
        .await
        .expect("insert candidate batch");
    assert_eq!(database.query_count(), 1);

    let rows: Vec<(String, String, String, f64, Option<i64>)> = sqlx::query_as(
        "SELECT id, provider_id, candidate_json, score, expires_at
         FROM metadata_candidates
         WHERE item_id = 'candidate-batch-item'
         ORDER BY provider_id",
    )
    .fetch_all(database.pool())
    .await
    .expect("candidate rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        (
            "candidate-first-id".to_owned(),
            "movie-1".to_owned(),
            r#"{"title":"Higher score, later tie"}"#.to_owned(),
            90.0,
            Some(2_500),
        )
    );
    assert_eq!(rows[1].0, "candidate-other-id");
    assert_eq!(rows[1].2, r#"{"title":"Other candidate"}"#);

    database.reset_query_count();
    let (page, total) = database
        .list_pending_metadata_candidates_for_item_with_count("candidate-batch-item", 0, 1)
        .await
        .expect("candidate page with total");
    assert_eq!(database.query_count(), 1);
    assert_eq!(page.len(), 1);
    assert_eq!(total, 2);

    database.close().await;
}

#[tokio::test]
async fn metadata_capability_batches_preserve_status_and_retry_backoff() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('capability-batch-item', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");

    let capabilities = [
        MetadataCapabilityResult {
            capability: "CREDITS",
            has_data: true,
        },
        MetadataCapabilityResult {
            capability: "EXTERNAL_IDS",
            has_data: false,
        },
        MetadataCapabilityResult {
            capability: "CREDITS",
            has_data: false,
        },
    ];
    database.reset_query_count();
    database
        .record_metadata_capability_results(
            "capability-batch-item",
            "tmdb",
            "movie-1",
            &capabilities,
            1_000,
        )
        .await
        .expect("record capability batch");
    assert_eq!(database.query_count(), 1);
    let capability_rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT capability, status, attempt_count
         FROM metadata_capability_attempts
         WHERE item_id = 'capability-batch-item' AND provider = 'tmdb'
         ORDER BY capability",
    )
    .fetch_all(database.pool())
    .await
    .expect("capability attempts");
    assert_eq!(
        capability_rows,
        vec![
            ("CREDITS".to_owned(), "UNAVAILABLE".to_owned(), 1),
            ("EXTERNAL_IDS".to_owned(), "UNAVAILABLE".to_owned(), 1),
        ]
    );

    database.reset_query_count();
    database
        .record_metadata_capability_failures(
            "capability-batch-item",
            "tmdb",
            "movie-1",
            &["EXTERNAL_IDS", "TRAILERS"],
            2_000,
        )
        .await
        .expect("record capability failure batch");
    assert_eq!(database.query_count(), 2);
    let failure_rows: Vec<(String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT capability, attempt_count, next_retry_at
         FROM metadata_capability_attempts
         WHERE item_id = 'capability-batch-item' AND provider = 'tmdb'
           AND status = 'FAILED'
         ORDER BY capability",
    )
    .fetch_all(database.pool())
    .await
    .expect("failed capability attempts");
    assert_eq!(
        failure_rows,
        vec![
            ("EXTERNAL_IDS".to_owned(), 2, Some(2_600)),
            ("TRAILERS".to_owned(), 1, Some(2_300)),
        ]
    );

    database.close().await;
}

#[tokio::test]
async fn unavailable_metadata_image_batch_uses_one_statement_and_preserves_attempt_count() {
    type MetadataImageAttemptRow = (String, String, i64, i64, Option<i64>, Option<String>, i64);

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('image-attempt-batch-item', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    sqlx::query(
        "INSERT INTO metadata_image_attempts (
                item_id, image_type, candidate_key, status, attempt_count,
                last_attempt_at, updated_at
             ) VALUES (
                'image-attempt-batch-item', 'POSTER', 'tmdb:1:POSTER',
                'FAILED', 5, 10, 10
             )",
    )
    .execute(database.pool())
    .await
    .expect("existing image attempt");
    let unavailable = [
        MetadataImageUnavailable {
            image_type: "POSTER",
            candidate_key: "tmdb:1:POSTER",
        },
        MetadataImageUnavailable {
            image_type: "FANART",
            candidate_key: "tmdb:1:FANART",
        },
    ];

    database.reset_query_count();
    database
        .mark_metadata_images_unavailable("image-attempt-batch-item", &unavailable, 200)
        .await
        .expect("mark missing images in one batch");
    assert_eq!(database.query_count(), 1);

    let rows: Vec<MetadataImageAttemptRow> = sqlx::query_as(
        "SELECT image_type, status, attempt_count, last_attempt_at,
                    next_retry_at, error_code, updated_at
             FROM metadata_image_attempts
             WHERE item_id = 'image-attempt-batch-item'
             ORDER BY image_type",
    )
    .fetch_all(database.pool())
    .await
    .expect("updated image attempts");
    assert_eq!(
        rows,
        vec![
            (
                "FANART".to_owned(),
                "UNAVAILABLE".to_owned(),
                1,
                200,
                None,
                Some("NO_IMAGE".to_owned()),
                200,
            ),
            (
                "POSTER".to_owned(),
                "UNAVAILABLE".to_owned(),
                5,
                200,
                None,
                Some("NO_IMAGE".to_owned()),
                200,
            ),
        ]
    );

    database.reset_query_count();
    database
        .mark_metadata_images_unavailable("image-attempt-batch-item", &[], 300)
        .await
        .expect("empty image batch is a no-op");
    assert_eq!(database.query_count(), 0);

    database.close().await;
}

#[tokio::test]
async fn metadata_job_list_counts_only_pending_items_on_the_requested_page() {
    sqlx::any::install_default_drivers();
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(
            AnyConnectOptions::from_str("sqlite://?mode=memory").expect("in-memory SQLite options"),
        )
        .await
        .expect("in-memory SQLite connection");
    sqlx::query(
        "CREATE TABLE metadata_reidentify_jobs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                processed_count INTEGER NOT NULL,
                total_count INTEGER NOT NULL,
                error TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                started_at INTEGER,
                finished_at INTEGER,
                mode TEXT NOT NULL,
                cancel_requested INTEGER NOT NULL,
                library_id TEXT,
                job_scope TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata jobs table");
    sqlx::query(
        "CREATE TABLE metadata_reidentify_job_items (
                job_id TEXT NOT NULL,
                item_id TEXT NOT NULL,
                status TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata job items table");
    sqlx::query(
        "CREATE TABLE metadata_candidates (
                item_id TEXT NOT NULL,
                status TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata candidates table");
    sqlx::query(
        "CREATE INDEX idx_metadata_reidentify_items_status
             ON metadata_reidentify_job_items(job_id, status, item_id)",
    )
    .execute(&pool)
    .await
    .expect("create metadata job item index");
    sqlx::query(
        "CREATE INDEX idx_metadata_candidates_item
             ON metadata_candidates(item_id, status)",
    )
    .execute(&pool)
    .await
    .expect("create metadata candidate index");
    for (id, created_at) in [("older", 1_i64), ("newer", 2_i64)] {
        sqlx::query(
            "INSERT INTO metadata_reidentify_jobs (
                    id, status, processed_count, total_count, error,
                    created_at, updated_at, started_at, finished_at, mode,
                    cancel_requested, library_id, job_scope
                 ) VALUES (?, 'QUEUED', 0, 2, NULL, ?, ?, NULL, NULL,
                           'REIDENTIFY', 0, NULL, 'ITEMS')",
        )
        .bind(id)
        .bind(created_at)
        .bind(created_at)
        .execute(&pool)
        .await
        .expect("insert metadata job");
    }
    for (job_id, item_id) in [("older", "old-item"), ("newer", "new-item")] {
        sqlx::query(
            "INSERT INTO metadata_reidentify_job_items (job_id, item_id, status)
                 VALUES (?, ?, 'PENDING')",
        )
        .bind(job_id)
        .bind(item_id)
        .execute(&pool)
        .await
        .expect("insert metadata job item");
    }
    sqlx::query(
        "INSERT INTO metadata_candidates (item_id, status)
             VALUES ('old-item', 'PENDING'), ('new-item', 'PENDING'),
                    ('new-item', 'PENDING')",
    )
    .execute(&pool)
    .await
    .expect("insert metadata candidates");

    let database = Database {
        pool,
        log_store: LogStore::new(Path::new("unused-metadata-summary-test")),
        pool_max_connections: 1,
        path: PathBuf::from("metadata-summary-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
        recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
            RecommendationRatingMedianCache::default(),
        )),
        filesystem_path_templates: Arc::new(SqlTemplateCache::default()),
        query_count: Arc::new(AtomicUsize::new(0)),
    };
    let jobs = database
        .list_metadata_reidentify_jobs(None, 0, 1)
        .await
        .expect("list metadata jobs");

    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, "newer");
    assert_eq!(jobs[0].pending_count, 1);

    let plan = sqlx::query(
        "EXPLAIN QUERY PLAN
             WITH selected_jobs AS (
                 SELECT id
                 FROM metadata_reidentify_jobs
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1 OFFSET 0
             ), pending_counts AS (
                 SELECT job_items.job_id, COUNT(DISTINCT candidates.item_id) AS pending_count
                 FROM metadata_reidentify_job_items job_items
                 JOIN selected_jobs ON selected_jobs.id = job_items.job_id
                 JOIN metadata_candidates candidates
                   ON candidates.item_id = job_items.item_id
                  AND candidates.status = 'PENDING'
                 GROUP BY job_items.job_id
             )
             SELECT selected_jobs.id, pending_counts.pending_count
             FROM selected_jobs
             LEFT JOIN pending_counts ON pending_counts.job_id = selected_jobs.id",
    )
    .fetch_all(database.pool())
    .await
    .expect("explain metadata summary query");
    let plan_details = plan
        .iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>();
    assert!(
        plan_details
            .iter()
            .any(|detail| detail
                .contains("USING COVERING INDEX idx_metadata_reidentify_items_status")),
        "metadata summary should seek selected job items by job_id: {plan_details:?}"
    );
    assert!(
        plan_details
            .iter()
            .any(|detail| detail.contains("USING COVERING INDEX idx_metadata_candidates_item")),
        "metadata summary should seek candidates by item_id: {plan_details:?}"
    );
    assert!(
        plan_details
            .iter()
            .all(|detail| !detail.contains("SCAN metadata_reidentify_job_items")),
        "metadata summary must not scan all job items: {plan_details:?}"
    );
    assert!(
        plan_details
            .iter()
            .all(|detail| !detail.contains("SCAN metadata_candidates")),
        "metadata summary must not scan all candidates: {plan_details:?}"
    );
    database.close().await;
}

#[tokio::test]
async fn person_credits_migration_creates_the_index_table() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'person_credits'",
    )
    .fetch_one(database.pool())
    .await
    .expect("person credits table");
    assert_eq!(table_count, 1);

    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-credits', ?, 'MOVIE', 'Credits', 'credits', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("media item");
    database
        .replace_person_credits(
            "item-credits",
            &[
                NewPersonCredit {
                    person_id: "1".to_owned(),
                    lux_person_id: Some("lux-000001".to_owned()),
                    person_type: "Actor".to_owned(),
                    person_name: "演员甲".to_owned(),
                    provider: "tmdb".to_owned(),
                    role: "角色甲".to_owned(),
                    sort_order: 0,
                    biography: None,
                    birthday: None,
                    deathday: None,
                    known_for_department: None,
                    place_of_birth: None,
                    provider_ids: BTreeMap::new(),
                    genres: Vec::new(),
                    tags: Vec::new(),
                    production_locations: Vec::new(),
                    premiere_date: None,
                    production_year: None,
                    taglines: Vec::new(),
                },
                NewPersonCredit {
                    person_id: "1".to_owned(),
                    lux_person_id: Some("lux-000001".to_owned()),
                    person_type: "Actor".to_owned(),
                    person_name: "重复演员甲".to_owned(),
                    provider: "tmdb".to_owned(),
                    role: "角色甲".to_owned(),
                    sort_order: 0,
                    biography: None,
                    birthday: None,
                    deathday: None,
                    known_for_department: None,
                    place_of_birth: None,
                    provider_ids: BTreeMap::new(),
                    genres: Vec::new(),
                    tags: Vec::new(),
                    production_locations: Vec::new(),
                    premiere_date: None,
                    production_year: None,
                    taglines: Vec::new(),
                },
                NewPersonCredit {
                    person_id: "9".to_owned(),
                    lux_person_id: Some("lux-000001".to_owned()),
                    person_type: "Actor".to_owned(),
                    person_name: "演员甲".to_owned(),
                    provider: "douban".to_owned(),
                    role: "角色甲".to_owned(),
                    sort_order: 0,
                    biography: None,
                    birthday: None,
                    deathday: None,
                    known_for_department: None,
                    place_of_birth: None,
                    provider_ids: BTreeMap::new(),
                    genres: Vec::new(),
                    tags: Vec::new(),
                    production_locations: Vec::new(),
                    premiere_date: None,
                    production_year: None,
                    taglines: Vec::new(),
                },
                NewPersonCredit {
                    person_id: "2".to_owned(),
                    lux_person_id: None,
                    person_type: "Actor".to_owned(),
                    person_name: "演员乙".to_owned(),
                    provider: "tmdb".to_owned(),
                    role: "角色乙".to_owned(),
                    sort_order: 1,
                    biography: None,
                    birthday: None,
                    deathday: None,
                    known_for_department: None,
                    place_of_birth: None,
                    provider_ids: BTreeMap::new(),
                    genres: Vec::new(),
                    tags: Vec::new(),
                    production_locations: Vec::new(),
                    premiere_date: None,
                    production_year: None,
                    taglines: Vec::new(),
                },
            ],
        )
        .await
        .expect("person credits");
    let stored_credit_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_credits WHERE item_id = 'item-credits'")
            .fetch_one(database.pool())
            .await
            .expect("stored person credit count");
    assert_eq!(stored_credit_count, 3);
    let stored_name: String = sqlx::query_scalar(
        "SELECT person_name FROM person_credits
             WHERE item_id = 'item-credits' AND person_id = '1' AND provider = 'tmdb'",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored first duplicate");
    assert_eq!(stored_name, "演员甲");
    let (credits, total) = database
        .list_person_credits_for_library(
            &library_id,
            "Actor",
            PersonListOptions {
                recursive: true,
                sort_by: PersonSort::Name,
                descending: false,
                offset: 0,
                limit: 10,
            },
        )
        .await
        .expect("list person credits");
    assert_eq!(total, 2);
    assert_eq!(credits.len(), 2);
    let names = credits
        .iter()
        .map(|credit| credit.person_name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"演员甲"));
    assert!(names.contains(&"演员乙"));
    assert_eq!(
        credits
            .iter()
            .filter(|credit| credit.lux_person_id.as_deref() == Some("lux-000001"))
            .count(),
        1
    );
}

#[tokio::test]
async fn person_credit_replacement_batches_large_credit_sets() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('item-large-credits', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    let credits = (0..41)
        .map(|index| NewPersonCredit {
            person_id: format!("person-{index}"),
            lux_person_id: None,
            person_type: "Actor".to_owned(),
            person_name: format!("演员{index}"),
            provider: "tmdb".to_owned(),
            role: format!("角色{index}"),
            sort_order: index,
            biography: None,
            birthday: None,
            deathday: None,
            known_for_department: None,
            place_of_birth: None,
            provider_ids: BTreeMap::new(),
            genres: Vec::new(),
            tags: Vec::new(),
            production_locations: Vec::new(),
            premiere_date: None,
            production_year: None,
            taglines: Vec::new(),
        })
        .collect::<Vec<_>>();
    database.reset_query_count();
    database
        .replace_person_credits("item-large-credits", &credits)
        .await
        .expect("large credit replacement");
    assert_eq!(database.query_count(), 4);
    let stored_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM person_credits WHERE item_id = 'item-large-credits'",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored credit count");
    assert_eq!(stored_count, 41);
}

#[tokio::test]
async fn person_credit_list_uses_one_consistent_representative_row() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    for (item_id, added_at) in [("item-a", 200_i64), ("item-b", 100_i64)] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title,
                    identification_status, added_at
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?)",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .bind(added_at)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    database
        .replace_person_credits(
            "item-a",
            &[NewPersonCredit {
                person_id: "1".to_owned(),
                lux_person_id: Some("lux-000001".to_owned()),
                person_type: "Actor".to_owned(),
                person_name: "同一演员".to_owned(),
                provider: "tmdb".to_owned(),
                role: "alpha-role".to_owned(),
                sort_order: 0,
                biography: Some("z-biography".to_owned()),
                birthday: None,
                deathday: None,
                known_for_department: None,
                place_of_birth: None,
                provider_ids: BTreeMap::new(),
                genres: Vec::new(),
                tags: Vec::new(),
                production_locations: Vec::new(),
                premiere_date: None,
                production_year: None,
                taglines: Vec::new(),
            }],
        )
        .await
        .expect("first person credit");
    database
        .replace_person_credits(
            "item-b",
            &[NewPersonCredit {
                person_id: "9".to_owned(),
                lux_person_id: Some("lux-000001".to_owned()),
                person_type: "Actor".to_owned(),
                person_name: "同一演员".to_owned(),
                provider: "douban".to_owned(),
                role: "zeta-role".to_owned(),
                sort_order: 0,
                biography: Some("a-biography".to_owned()),
                birthday: None,
                deathday: None,
                known_for_department: None,
                place_of_birth: None,
                provider_ids: BTreeMap::new(),
                genres: Vec::new(),
                tags: Vec::new(),
                production_locations: Vec::new(),
                premiere_date: None,
                production_year: None,
                taglines: Vec::new(),
            }],
        )
        .await
        .expect("second person credit");

    let (credits, total) = database
        .list_person_credits_for_library(
            &library_id,
            "Actor",
            PersonListOptions {
                recursive: true,
                sort_by: PersonSort::Name,
                descending: false,
                offset: 0,
                limit: 10,
            },
        )
        .await
        .expect("list person credits");

    assert_eq!(total, 1);
    let credit = credits.first().expect("representative person credit");
    assert_eq!(credit.provider, "tmdb");
    assert_eq!(credit.person_id, "1");
    assert_eq!(credit.role, "alpha-role");
    assert_eq!(credit.biography.as_deref(), Some("z-biography"));
    assert_eq!(credit.date_created, 100);
}

#[tokio::test]
async fn canonical_people_migration_creates_recoverable_identity_tables() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    for table in ["people", "person_identities", "person_id_sequence"] {
        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(table)
        .fetch_one(database.pool())
        .await
        .expect("canonical people table");
        assert_eq!(table_count, 1, "missing canonical people table {table}");
    }
}

#[tokio::test]
async fn canonical_people_reuse_one_lux_id_across_provider_identities() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let first = database
        .resolve_or_create_canonical_person(
            "华晨宇",
            "tmdb",
            "57975",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("first canonical person");
    assert_eq!(first.id, "lux-000001");

    let second = database
        .attach_canonical_person_identity(
            &first.id,
            "douban",
            "1313123",
            "MEDIA_BRIDGE",
            Some(0.98),
            r#"{"source":"same-media"}"#,
        )
        .await
        .expect("second canonical identity");
    assert_eq!(second.id, first.id);

    let repeated = database
        .resolve_or_create_canonical_person(
            "华晨宇",
            "tmdb",
            "57975",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("repeated canonical identity");
    assert_eq!(repeated.id, first.id);

    let identity_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM person_identities")
        .fetch_one(database.pool())
        .await
        .expect("identity count");
    assert_eq!(identity_count, 2);
}

#[tokio::test]
async fn canonical_people_batch_identity_lookup_returns_all_matches_in_one_query() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let first = database
        .resolve_or_create_canonical_person(
            "华晨宇",
            "tmdb",
            "57975",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("first canonical person");
    let second = database
        .resolve_or_create_canonical_person(
            "另一位演员",
            "tmdb",
            "57976",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("second canonical person");

    database.reset_query_count();
    let matches = database
        .find_canonical_people_by_identities(&[
            ("tmdb".to_owned(), "57975".to_owned()),
            ("tmdb".to_owned(), "57976".to_owned()),
            ("tmdb".to_owned(), "missing".to_owned()),
        ])
        .await
        .expect("batch identity lookup");

    assert_eq!(database.query_count(), 1);
    assert_eq!(
        matches,
        vec![
            ("tmdb".to_owned(), "57975".to_owned(), first.id),
            ("tmdb".to_owned(), "57976".to_owned(), second.id),
        ]
    );
}

#[tokio::test]
async fn canonical_people_batch_identity_attach_is_atomic() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let owner = database
        .resolve_or_create_canonical_person(
            "人物甲",
            "tmdb",
            "57975",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("owner");
    database.reset_query_count();
    database
        .attach_canonical_person_identities(
            &owner.id,
            &[
                ("douban".to_owned(), "1313123".to_owned()),
                ("imdb".to_owned(), "nm0000001".to_owned()),
            ],
            "SAME_SOURCE_ID_SET",
            Some(0.99),
            r#"{"method":"test"}"#,
        )
        .await
        .expect("batch attach");
    assert_eq!(database.query_count(), 3);
    let identity_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_identities WHERE person_id = ?")
            .bind(&owner.id)
            .fetch_one(database.pool())
            .await
            .expect("identity count");
    assert_eq!(identity_count, 3);

    let other = database
        .resolve_or_create_canonical_person(
            "人物乙",
            "tmdb",
            "57976",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("other owner");
    let result = database
        .attach_canonical_person_identities(
            &owner.id,
            &[
                ("douban".to_owned(), "new-id".to_owned()),
                ("tmdb".to_owned(), "57976".to_owned()),
            ],
            "SAME_SOURCE_ID_SET",
            Some(0.99),
            r#"{"method":"conflict"}"#,
        )
        .await;
    assert!(result.is_err());
    let new_identity_owner: Option<String> = sqlx::query_scalar(
        "SELECT person_id FROM person_identities
         WHERE provider = 'douban' AND provider_id = 'new-id'",
    )
    .fetch_optional(database.pool())
    .await
    .expect("new identity lookup");
    assert!(new_identity_owner.is_none());
    assert_eq!(other.id, "lux-000002");
}

#[tokio::test]
async fn restoring_a_manifest_rejects_a_provider_identity_owned_by_another_person() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    database
        .resolve_or_create_canonical_person(
            "华晨宇",
            "tmdb",
            "57975",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("first canonical person");

    let error = database
        .restore_canonical_person(
            "lux-000002",
            "另一位演员",
            &[("douban", "new-id"), ("tmdb", "57975")],
        )
        .await
        .expect_err("conflicting manifest must be rejected");
    assert!(matches!(error, StorageError::Conflict(_)));
    let new_identity_owner: Option<String> = sqlx::query_scalar(
        "SELECT person_id FROM person_identities
         WHERE provider = 'douban' AND provider_id = 'new-id'",
    )
    .fetch_optional(database.pool())
    .await
    .expect("new identity lookup");
    assert!(new_identity_owner.is_none());
    let restored_person: Option<String> =
        sqlx::query_scalar("SELECT id FROM people WHERE id = 'lux-000002'")
            .fetch_optional(database.pool())
            .await
            .expect("restored person lookup");
    assert!(restored_person.is_none());
    assert_eq!(
        database
            .find_canonical_person_by_identity("tmdb", "57975")
            .await
            .expect("identity lookup")
            .expect("existing identity")
            .id,
        "lux-000001"
    );
}

#[tokio::test]
async fn restoring_a_person_manifest_batches_identity_queries() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let identities = [
        ("tmdb", "57975"),
        ("douban", "1313123"),
        ("imdb", "nm0000001"),
        ("anilist", "42"),
    ];

    database.reset_query_count();
    database
        .restore_canonical_person("lux-000001", "华晨宇", &identities)
        .await
        .expect("restore person");

    assert_eq!(database.query_count(), 5);
    let identity_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_identities WHERE person_id = 'lux-000001'")
            .fetch_one(database.pool())
            .await
            .expect("identity count");
    assert_eq!(identity_count, identities.len() as i64);
}

#[tokio::test]
async fn restoring_a_person_manifest_chunks_large_identity_sets() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let owned_identities = (0..205)
        .map(|index| ("tmdb".to_owned(), format!("id-{index}")))
        .collect::<Vec<_>>();
    let identities = owned_identities
        .iter()
        .map(|(provider, provider_id)| (provider.as_str(), provider_id.as_str()))
        .collect::<Vec<_>>();

    database.reset_query_count();
    database
        .restore_canonical_person("lux-000001", "Person", &identities)
        .await
        .expect("restore person with many identities");

    assert_eq!(database.query_count(), 9);
    let identity_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_identities WHERE person_id = 'lux-000001'")
            .fetch_one(database.pool())
            .await
            .expect("identity count");
    assert_eq!(identity_count, owned_identities.len() as i64);
}

#[tokio::test]
async fn person_match_candidates_are_persistent_and_idempotent() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-1', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    let first_id = database
        .enqueue_person_match_candidate(
            "item-1",
            "douban",
            "1313123",
            r#"["lux-000001","lux-000002"]"#,
            Some(0.62),
            r#"{"method":"same-media-ambiguous"}"#,
        )
        .await
        .expect("first candidate");
    let second_id = database
        .enqueue_person_match_candidate(
            "item-1",
            "douban",
            "1313123",
            r#"["lux-000002","lux-000001"]"#,
            Some(0.65),
            r#"{"method":"same-media-ambiguous","retry":true}"#,
        )
        .await
        .expect("idempotent candidate update");
    assert_eq!(first_id, second_id);

    let (count, status, score): (i64, String, f64) = sqlx::query_as(
        "SELECT COUNT(*), MIN(status), MAX(score)
             FROM person_match_candidates
             WHERE item_id = 'item-1' AND provider = 'douban' AND provider_id = '1313123'",
    )
    .fetch_one(database.pool())
    .await
    .expect("candidate row");
    assert_eq!(count, 1);
    assert_eq!(status, "PENDING");
    assert_eq!(score, 0.65);

    sqlx::query("UPDATE person_match_candidates SET status = 'CONFIRMED' WHERE id = ?")
        .bind(&first_id)
        .execute(database.pool())
        .await
        .expect("mark candidate decided");
    database
        .enqueue_person_match_candidate(
            "item-1",
            "douban",
            "1313123",
            r#"["lux-000002"]"#,
            Some(0.9),
            r#"{"method":"retry"}"#,
        )
        .await
        .expect("retry decided candidate");
    let preserved_status: String =
        sqlx::query_scalar("SELECT status FROM person_match_candidates WHERE id = ?")
            .bind(&first_id)
            .fetch_one(database.pool())
            .await
            .expect("preserved candidate status");
    assert_eq!(preserved_status, "CONFIRMED");
}

#[tokio::test]
async fn confirming_person_match_moves_identity_and_credit_atomically() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-confirm', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("media item");
    let old = database
        .resolve_or_create_canonical_person(
            "旧人物",
            "douban",
            "1313123",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"douban"}"#,
        )
        .await
        .expect("old person");
    let target = database
        .resolve_or_create_canonical_person(
            "目标人物",
            "tmdb",
            "57975",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"tmdb"}"#,
        )
        .await
        .expect("target person");
    database
        .replace_person_credits(
            "item-confirm",
            &[NewPersonCredit {
                person_id: "1313123".to_owned(),
                lux_person_id: Some(old.id.clone()),
                person_type: "Actor".to_owned(),
                person_name: "旧人物".to_owned(),
                provider: "douban".to_owned(),
                role: "角色".to_owned(),
                sort_order: 0,
                biography: None,
                birthday: None,
                deathday: None,
                known_for_department: None,
                place_of_birth: None,
                provider_ids: BTreeMap::new(),
                genres: Vec::new(),
                tags: Vec::new(),
                production_locations: Vec::new(),
                premiere_date: None,
                production_year: None,
                taglines: Vec::new(),
            }],
        )
        .await
        .expect("credit");
    database
        .enqueue_person_match_candidate(
            "item-confirm",
            "douban",
            "1313123",
            &format!("[\"{}\"]", target.id),
            Some(0.9),
            r#"{"method":"same-media"}"#,
        )
        .await
        .expect("candidate");
    let candidate_id: String = sqlx::query_scalar(
        "SELECT id FROM person_match_candidates
             WHERE item_id = 'item-confirm'",
    )
    .fetch_one(database.pool())
    .await
    .expect("candidate id");

    let moved = database
        .confirm_person_match_candidate(&candidate_id, &target.id, r#"{"method":"manual-confirm"}"#)
        .await
        .expect("confirm candidate");
    assert_eq!(moved.previous_person_id.as_deref(), Some(old.id.as_str()));
    assert_eq!(
        database
            .find_canonical_person_by_identity("douban", "1313123")
            .await
            .expect("identity lookup")
            .expect("moved identity")
            .id,
        target.id
    );
    let lux_id: String = sqlx::query_scalar(
        "SELECT lux_person_id FROM person_credits
             WHERE item_id = 'item-confirm'",
    )
    .fetch_one(database.pool())
    .await
    .expect("credit lux id");
    assert_eq!(lux_id, target.id);
    let status: String = sqlx::query_scalar(
        "SELECT status FROM person_match_candidates
             WHERE id = ?",
    )
    .bind(&candidate_id)
    .fetch_one(database.pool())
    .await
    .expect("candidate status");
    assert_eq!(status, "CONFIRMED");

    database
        .undo_person_match_candidate(&candidate_id, r#"{"reason":"test-undo"}"#)
        .await
        .expect("undo candidate");
    assert_eq!(
        database
            .find_canonical_person_by_identity("douban", "1313123")
            .await
            .expect("identity lookup after undo")
            .expect("restored identity")
            .id,
        old.id
    );
    let restored_lux_id: String = sqlx::query_scalar(
        "SELECT lux_person_id FROM person_credits
             WHERE item_id = 'item-confirm'",
    )
    .fetch_one(database.pool())
    .await
    .expect("restored credit lux id");
    assert_eq!(restored_lux_id, old.id);
    let undone_status: String =
        sqlx::query_scalar("SELECT status FROM person_match_candidates WHERE id = ?")
            .bind(candidate_id)
            .fetch_one(database.pool())
            .await
            .expect("undone candidate status");
    assert_eq!(undone_status, "REJECTED");
}

#[tokio::test]
async fn splitting_person_identity_allocates_a_new_lux_person_and_repoints_credits() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-split', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    let old = database
        .resolve_or_create_canonical_person(
            "人物甲",
            "douban",
            "1313123",
            "PROVIDER_ID",
            Some(1.0),
            r#"{"source":"douban"}"#,
        )
        .await
        .expect("person");
    database
        .replace_person_credits(
            "item-split",
            &[NewPersonCredit {
                person_id: "1313123".to_owned(),
                lux_person_id: Some(old.id.clone()),
                person_type: "Actor".to_owned(),
                person_name: "人物甲".to_owned(),
                provider: "douban".to_owned(),
                role: "角色".to_owned(),
                sort_order: 0,
                biography: None,
                birthday: None,
                deathday: None,
                known_for_department: None,
                place_of_birth: None,
                provider_ids: BTreeMap::new(),
                genres: Vec::new(),
                tags: Vec::new(),
                production_locations: Vec::new(),
                premiere_date: None,
                production_year: None,
                taglines: Vec::new(),
            }],
        )
        .await
        .expect("credit");
    let split = database
        .split_canonical_person_identity(
            &old.id,
            "douban",
            "1313123",
            "人物乙",
            r#"{"method":"undo-merge"}"#,
        )
        .await
        .expect("split");
    assert_ne!(split.id, old.id);
    assert_eq!(split.id, "lux-000002");
    assert_eq!(
        database
            .find_canonical_person_by_identity("douban", "1313123")
            .await
            .expect("identity")
            .expect("new owner")
            .id,
        split.id
    );
    let lux_id: String =
        sqlx::query_scalar("SELECT lux_person_id FROM person_credits WHERE item_id = 'item-split'")
            .fetch_one(database.pool())
            .await
            .expect("credit owner");
    assert_eq!(lux_id, split.id);
}

#[tokio::test]
async fn catalog_tie_breakers_use_displayed_title_when_sort_key_is_stale() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, premiere_date,
                rating, identification_status, added_at, has_available_source
             ) VALUES
                ('item-alpha', ?, 'MOVIE', 'Alpha', 'zzz', '2020-01-01', 8.0, 'LOCAL_CONFIRMED', 100, 1),
                ('item-beta', ?, 'MOVIE', 'Beta', 'aaa', '2020-01-01', 8.0, 'LOCAL_CONFIRMED', 100, 1)",
        )
        .bind(&library_id)
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("media items");

    let library_ids = vec![library_id];
    let item_types = vec!["MOVIE".to_owned()];
    let empty = Vec::new();
    let empty_years = Vec::<i64>::new();
    for (sort_by, descending) in [
        (CatalogSort::DateCreated, false),
        (CatalogSort::DateCreated, true),
        (CatalogSort::PremiereDate, false),
        (CatalogSort::PremiereDate, true),
        (CatalogSort::Rating, false),
        (CatalogSort::Rating, true),
    ] {
        let filter = CatalogFilterQuery {
            library_ids: &library_ids,
            user_id: "test-user",
            item_types: &item_types,
            excluded_item_types: &empty,
            item_ids: None,
            person_id: None,
            media_source_ids: None,
            provider_id_equals: None,
            years: &empty_years,
            is_played: None,
            is_favorite: None,
            min_date_last_saved: None,
            metadata_pending: false,
            parent_id_scope: None,
            content_restrictions: None,
            sort_by,
            descending,
            offset: 0,
            limit: 10,
        };
        let (rows, total) = database
            .list_filtered_catalog_rows(&filter)
            .await
            .expect("catalog rows");
        let titles = rows.into_iter().map(|row| row.title).collect::<Vec<_>>();
        let expected = vec!["Alpha", "Beta"];
        assert_eq!(total, 2);
        assert_eq!(titles, expected, "descending={descending}");
    }
}

#[tokio::test]
async fn catalog_filter_parent_scope_applies_before_pagination() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Home videos", LibraryKind::HomeVideos, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, parent_id, title, sort_title,
            identification_status, has_available_source
         ) VALUES
            ('root-video', ?, 'VIDEO', NULL, 'Root video', 'root video', 'LOCAL_CONFIRMED', 1),
            ('root-folder', ?, 'FOLDER', ?, 'Root folder', 'root folder', 'LOCAL_CONFIRMED', 0),
            ('nested-video', ?, 'VIDEO', 'root-folder', 'Nested video', 'nested video', 'LOCAL_CONFIRMED', 1),
            ('nested-folder', ?, 'FOLDER', 'root-folder', 'Nested folder', 'nested folder', 'LOCAL_CONFIRMED', 0),
            ('deep-video', ?, 'VIDEO', 'nested-folder', 'Deep video', 'deep video', 'LOCAL_CONFIRMED', 1)",
    )
    .bind(&library_id)
    .bind(&library_id)
    .bind(&library_id)
    .bind(&library_id)
    .bind(&library_id)
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("home video entries");

    let library_ids = vec![library_id];
    let item_types = vec!["FOLDER".to_owned(), "VIDEO".to_owned()];
    let empty = Vec::new();
    let empty_years = Vec::<i64>::new();
    let mut filter = CatalogFilterQuery {
        library_ids: &library_ids,
        user_id: "test-user",
        item_types: &item_types,
        excluded_item_types: &empty,
        item_ids: None,
        person_id: None,
        media_source_ids: None,
        provider_id_equals: None,
        years: &empty_years,
        is_played: None,
        is_favorite: None,
        min_date_last_saved: None,
        metadata_pending: false,
        sort_by: CatalogSort::Name,
        descending: false,
        offset: 0,
        limit: 1,
        parent_id_scope: Some(None),
        content_restrictions: None,
    };

    let (root_page, root_total) = database
        .list_filtered_catalog_rows(&filter)
        .await
        .expect("root catalog page");
    assert_eq!(root_total, 2);
    assert_eq!(root_page.len(), 1);
    assert_eq!(root_page[0].title, "Root folder");

    filter.offset = 1;
    let (second_root_page, _) = database
        .list_filtered_catalog_rows(&filter)
        .await
        .expect("second root catalog page");
    assert_eq!(second_root_page.len(), 1);
    assert_eq!(second_root_page[0].title, "Root video");

    filter.parent_id_scope = Some(Some("root-folder"));
    filter.offset = 0;
    filter.limit = 10;
    let (nested_page, nested_total) = database
        .list_filtered_catalog_rows(&filter)
        .await
        .expect("nested catalog page");
    assert_eq!(nested_total, 2);
    assert_eq!(nested_page.len(), 2);
    assert!(
        nested_page
            .iter()
            .all(|item| item.parent_id.as_deref() == Some("root-folder"))
    );

    filter.parent_id_scope = None;
    let (_, unfiltered_total) = database
        .list_filtered_catalog_rows(&filter)
        .await
        .expect("unfiltered catalog page");
    assert_eq!(unfiltered_total, 5);
}

#[tokio::test]
async fn catalog_root_counts_are_grouped_by_library_in_one_query() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let first = libraries
        .create_library("First", LibraryKind::Mixed, false)
        .await
        .expect("first library");
    let second = libraries
        .create_library("Second", LibraryKind::Mixed, false)
        .await
        .expect("second library");
    let first_id = first.id.to_string();
    let second_id = second.id.to_string();
    sqlx::query(
        "INSERT INTO media_items
         (id, library_id, item_type, title, sort_title, identification_status, has_available_source)
         VALUES
           ('root-count-movie-1', ?, 'MOVIE', 'Movie 1', 'movie 1', 'LOCAL_CONFIRMED', 1),
           ('root-count-series-1', ?, 'SERIES', 'Series 1', 'series 1', 'LOCAL_CONFIRMED', 1),
           ('root-count-movie-2', ?, 'MOVIE', 'Movie 2', 'movie 2', 'LOCAL_CONFIRMED', 1),
           ('root-count-series-2', ?, 'SERIES', 'Series 2', 'series 2', 'LOCAL_CONFIRMED', 1),
           ('root-count-folder', ?, 'FOLDER', 'Folder', 'folder', 'LOCAL_CONFIRMED', 1),
           ('root-count-removed', ?, 'MOVIE', 'Removed', 'removed', 'LOCAL_CONFIRMED', 1)",
    )
    .bind(&first_id)
    .bind(&first_id)
    .bind(&second_id)
    .bind(&second_id)
    .bind(&first_id)
    .bind(&first_id)
    .execute(database.pool())
    .await
    .expect("media items");
    sqlx::query("UPDATE media_items SET removed_at = 1 WHERE id = 'root-count-removed'")
        .execute(database.pool())
        .await
        .expect("removed item");

    database.reset_query_count();
    let counts = database
        .count_catalog_root_items_by_library(&[first_id.clone(), second_id.clone()])
        .await
        .expect("root counts");

    assert_eq!(database.query_count(), 1);
    assert_eq!(
        counts.get(&first_id).map(|value| value.movie_count),
        Some(1)
    );
    assert_eq!(
        counts.get(&first_id).map(|value| value.series_count),
        Some(1)
    );
    assert_eq!(
        counts.get(&second_id).map(|value| value.movie_count),
        Some(1)
    );
    assert_eq!(
        counts.get(&second_id).map(|value| value.series_count),
        Some(1)
    );
}

#[tokio::test]
async fn catalog_premiere_date_sort_falls_back_to_production_year() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, production_year,
                identification_status, added_at, has_available_source
             ) VALUES
                ('item-newer', ?, 'MOVIE', 'A Newer Movie', 'a newer movie', 2025, 'LOCAL_CONFIRMED', 100, 1),
                ('item-older', ?, 'MOVIE', 'B Older Movie', 'b older movie', 2010, 'LOCAL_CONFIRMED', 100, 1)",
    )
    .bind(&library_id)
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("media items");

    let library_ids = vec![library_id];
    let item_types = vec!["MOVIE".to_owned()];
    let empty = Vec::new();
    let empty_years = Vec::<i64>::new();
    for (descending, expected) in [
        (false, vec!["B Older Movie", "A Newer Movie"]),
        (true, vec!["A Newer Movie", "B Older Movie"]),
    ] {
        let filter = CatalogFilterQuery {
            library_ids: &library_ids,
            user_id: "test-user",
            item_types: &item_types,
            excluded_item_types: &empty,
            item_ids: None,
            person_id: None,
            media_source_ids: None,
            provider_id_equals: None,
            years: &empty_years,
            is_played: None,
            is_favorite: None,
            min_date_last_saved: None,
            metadata_pending: false,
            parent_id_scope: None,
            content_restrictions: None,
            sort_by: CatalogSort::PremiereDate,
            descending,
            offset: 0,
            limit: 10,
        };
        let (rows, total) = database
            .list_filtered_catalog_rows(&filter)
            .await
            .expect("catalog rows");
        let titles = rows.into_iter().map(|row| row.title).collect::<Vec<_>>();
        assert_eq!(total, 2);
        assert_eq!(titles, expected, "descending={descending}");
    }
}

#[tokio::test]
async fn media_source_library_page_respects_limit_and_offset() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    tokio::fs::write(root_path.join("First.Movie.2024.mkv"), b"first")
        .await
        .expect("first movie");
    tokio::fs::write(root_path.join("Second.Movie.2024.mkv"), b"second")
        .await
        .expect("second movie");
    tokio::fs::write(
        root_path.join("First.Remote.2024.strm"),
        b"https://media.example.invalid/first.mkv",
    )
    .await
    .expect("first STRM");
    tokio::fs::write(
        root_path.join("Second.Remote.2024.strm"),
        b"https://media.example.invalid/second.mkv",
    )
    .await
    .expect("second STRM");
    libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
        .await
        .expect("library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("scan");

    let first_page = database
        .list_media_sources_for_library_page(&library.id.to_string(), 1, 0)
        .await
        .expect("first page");
    let second_page = database
        .list_media_sources_for_library_page(&library.id.to_string(), 1, 1)
        .await
        .expect("second page");

    assert_eq!(first_page.len(), 1);
    assert_eq!(second_page.len(), 1);
    assert_ne!(first_page[0].source_id, second_page[0].source_id);
    let existing_entries = database
        .list_filesystem_entries_for_paths(
            &database
                .list_library_roots(&library.id.to_string())
                .await
                .expect("roots")
                .into_iter()
                .next()
                .expect("root")
                .id,
            &["First.Movie.2024.mkv".to_owned()],
        )
        .await
        .expect("existing entries");
    assert_eq!(existing_entries.len(), 1);
    assert_eq!(
        database
            .list_local_thumbnail_sources_for_library_page(&library.id.to_string(), 1, 0,)
            .await
            .expect("thumbnail page")
            .len(),
        1
    );
    assert_eq!(
        database
            .list_movie_metadata_sources_page(&library.id.to_string(), 1, 1)
            .await
            .expect("metadata page")
            .len(),
        1
    );
    let first_strm_page = database
        .list_strm_media_sources_for_library_page(&library.id.to_string(), None, 1)
        .await
        .expect("first STRM page");
    let second_strm_page = database
        .list_strm_media_sources_for_library_page(
            &library.id.to_string(),
            first_strm_page
                .first()
                .map(|source| source.source_id.as_str()),
            1,
        )
        .await
        .expect("second STRM page");
    let final_strm_page = database
        .list_strm_media_sources_for_library_page(
            &library.id.to_string(),
            second_strm_page
                .first()
                .map(|source| source.source_id.as_str()),
            1,
        )
        .await
        .expect("final STRM page");
    assert_eq!(first_strm_page.len(), 1);
    assert_eq!(second_strm_page.len(), 1);
    assert_ne!(first_strm_page[0].source_id, second_strm_page[0].source_id);
    assert!(final_strm_page.is_empty());
    database.close().await;
}

#[tokio::test]
async fn filesystem_path_reads_reuse_bucketed_templates_without_extra_queries() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Filesystem path templates", LibraryKind::Movie, false)
        .await
        .expect("library");
    let first_root_path = temp_dir.path().join("first-media-root");
    let other_root_path = temp_dir.path().join("other-media-root");
    tokio::fs::create_dir_all(&first_root_path)
        .await
        .expect("first media root");
    tokio::fs::create_dir_all(&other_root_path)
        .await
        .expect("other media root");
    let library_service = LibraryService::new(database.clone());
    let first_root = library_service
        .add_root(
            library.id,
            first_root_path.to_str().expect("UTF-8 first media root"),
        )
        .await
        .expect("first library root")
        .root;
    let other_root = library_service
        .add_root(
            library.id,
            other_root_path.to_str().expect("UTF-8 other media root"),
        )
        .await
        .expect("other library root")
        .root;
    let first_root_id = first_root.id.to_string();
    let other_root_id = other_root.id.to_string();
    for (entry_id, root_id, path) in [
        (
            "filesystem-template-first",
            first_root_id.as_str(),
            "first.mkv",
        ),
        (
            "filesystem-template-last",
            first_root_id.as_str(),
            "last.mkv",
        ),
        (
            "filesystem-template-other-root",
            other_root_id.as_str(),
            "first.mkv",
        ),
    ] {
        database
            .insert_filesystem_entry(NewFilesystemEntry {
                id: entry_id,
                library_root_id: root_id,
                relative_path: path,
                entry_kind: "FILE",
                size: 1,
                modified_at: 1,
                device: None,
                inode: None,
                fingerprint: b"fingerprint",
                last_seen_generation: "filesystem-template-generation",
            })
            .await
            .expect("filesystem entry");
    }

    database.reset_query_count();
    let empty = database
        .list_filesystem_entries_for_paths(&first_root_id, &[])
        .await
        .expect("empty path lookup");
    assert!(empty.is_empty());
    assert_eq!(database.query_count(), 0);
    assert_eq!(database.filesystem_path_templates.entry_count(), 0);

    database.reset_query_count();
    let one_path = database
        .list_filesystem_entries_for_paths(&first_root_id, &["first.mkv".to_owned()])
        .await
        .expect("one path lookup");
    assert_eq!(database.query_count(), 1);
    assert_eq!(database.filesystem_path_templates.entry_count(), 1);
    assert_eq!(one_path["first.mkv"].id, "filesystem-template-first");

    database.reset_query_count();
    let three_paths = database
        .list_filesystem_entries_for_paths(
            &first_root_id,
            &[
                "first.mkv".to_owned(),
                "missing-middle.mkv".to_owned(),
                "last.mkv".to_owned(),
            ],
        )
        .await
        .expect("three path lookup");
    assert_eq!(database.query_count(), 1);
    assert_eq!(database.filesystem_path_templates.entry_count(), 2);
    assert_eq!(three_paths.len(), 2);
    assert_eq!(three_paths["first.mkv"].id, "filesystem-template-first");
    assert_eq!(three_paths["last.mkv"].id, "filesystem-template-last");

    database.reset_query_count();
    let four_paths = database
        .list_filesystem_entries_for_paths(
            &first_root_id,
            &[
                "first.mkv".to_owned(),
                "missing-one.mkv".to_owned(),
                "missing-two.mkv".to_owned(),
                "last.mkv".to_owned(),
            ],
        )
        .await
        .expect("four path lookup");
    assert_eq!(database.query_count(), 1);
    assert_eq!(database.filesystem_path_templates.entry_count(), 2);
    assert_eq!(four_paths.len(), 2);

    database.reset_query_count();
    let other_root_entry = database
        .list_filesystem_entries_for_paths(&other_root_id, &["first.mkv".to_owned()])
        .await
        .expect("same relative path in other root");
    assert_eq!(database.query_count(), 1);
    assert_eq!(
        other_root_entry["first.mkv"].id,
        "filesystem-template-other-root"
    );

    let paths_with_matches_at = |count: usize, first: usize, last: usize| {
        let mut paths = (0..count)
            .map(|index| format!("missing-{index}.mkv"))
            .collect::<Vec<_>>();
        paths[first] = "first.mkv".to_owned();
        paths[last] = "last.mkv".to_owned();
        paths
    };

    for (count, first, last, cached_shapes) in
        [(256, 0, 255, 3), (257, 0, 256, 4), (500, 0, 499, 4)]
    {
        database.reset_query_count();
        let paths = paths_with_matches_at(count, first, last);
        let entries = database
            .list_filesystem_entries_for_paths(&first_root_id, &paths)
            .await
            .expect("bucket boundary lookup");
        assert_eq!(database.query_count(), 1, "path_count={count}");
        assert_eq!(
            database.filesystem_path_templates.entry_count(),
            cached_shapes
        );
        assert_eq!(entries.len(), 2, "path_count={count}");
    }

    database.reset_query_count();
    let mut paths = (0..501)
        .map(|index| format!("missing-{index}.mkv"))
        .collect::<Vec<_>>();
    paths[499] = "first.mkv".to_owned();
    paths[500] = "last.mkv".to_owned();
    let entries = database
        .list_filesystem_entries_for_paths(&first_root_id, &paths)
        .await
        .expect("cross batch lookup");
    assert_eq!(database.query_count(), 2);
    assert_eq!(database.filesystem_path_templates.entry_count(), 4);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries["first.mkv"].id, "filesystem-template-first");
    assert_eq!(entries["last.mkv"].id, "filesystem-template-last");
    database.close().await;
}

#[tokio::test]
async fn manifest_baseline_reads_reuse_bucketed_templates_without_extra_queries() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Manifest baseline templates", LibraryKind::Movie, false)
        .await
        .expect("library");
    let first_root_path = temp_dir.path().join("first-media-root");
    let other_root_path = temp_dir.path().join("other-media-root");
    tokio::fs::create_dir_all(&first_root_path)
        .await
        .expect("first media root");
    tokio::fs::create_dir_all(&other_root_path)
        .await
        .expect("other media root");
    let library_service = LibraryService::new(database.clone());
    let first_root = library_service
        .add_root(
            library.id,
            first_root_path.to_str().expect("UTF-8 first media root"),
        )
        .await
        .expect("first library root")
        .root;
    let other_root = library_service
        .add_root(
            library.id,
            other_root_path.to_str().expect("UTF-8 other media root"),
        )
        .await
        .expect("other library root")
        .root;
    let first_root_id = first_root.id.to_string();
    let other_root_id = other_root.id.to_string();
    for (entry_id, root_id, path, size, fingerprint) in [
        (
            "manifest-baseline-first-root",
            first_root_id.as_str(),
            "same.mkv",
            17,
            b"first-fingerprint".as_slice(),
        ),
        (
            "manifest-baseline-last-root",
            first_root_id.as_str(),
            "last.mkv",
            23,
            b"last-fingerprint".as_slice(),
        ),
        (
            "manifest-baseline-other-root",
            other_root_id.as_str(),
            "same.mkv",
            99,
            b"other-fingerprint".as_slice(),
        ),
    ] {
        database
            .insert_filesystem_entry(NewFilesystemEntry {
                id: entry_id,
                library_root_id: root_id,
                relative_path: path,
                entry_kind: "FILE",
                size,
                modified_at: size + 100,
                device: None,
                inode: None,
                fingerprint,
                last_seen_generation: "manifest-baseline-template-generation",
            })
            .await
            .expect("filesystem entry");
    }

    database.reset_query_count();
    let empty = database
        .list_scan_manifest_filesystem_baselines(&first_root_id, &[])
        .await
        .expect("empty baseline lookup");
    assert!(empty.is_empty());
    assert_eq!(database.query_count(), 0);
    assert_eq!(database.filesystem_path_templates.entry_count(), 0);

    database.reset_query_count();
    let one_path = database
        .list_scan_manifest_filesystem_baselines(&first_root_id, &["same.mkv".to_owned()])
        .await
        .expect("one path baseline lookup");
    assert_eq!(database.query_count(), 1);
    assert_eq!(database.filesystem_path_templates.entry_count(), 1);
    assert_eq!(one_path.len(), 1);
    assert_eq!(one_path["same.mkv"].id, "manifest-baseline-first-root");
    assert_eq!(one_path["same.mkv"].size, 17);
    assert_eq!(one_path["same.mkv"].modified_at, 117);
    assert_eq!(
        one_path["same.mkv"].fingerprint.as_deref(),
        Some(b"first-fingerprint".as_slice())
    );

    database.reset_query_count();
    let three_paths = database
        .list_scan_manifest_filesystem_baselines(
            &first_root_id,
            &[
                "same.mkv".to_owned(),
                "same.mkv".to_owned(),
                "missing.mkv".to_owned(),
            ],
        )
        .await
        .expect("duplicate and missing path lookup");
    assert_eq!(database.query_count(), 1);
    assert_eq!(database.filesystem_path_templates.entry_count(), 2);
    assert_eq!(three_paths.len(), 1);
    assert_eq!(three_paths["same.mkv"].id, "manifest-baseline-first-root");

    database.reset_query_count();
    let four_paths = database
        .list_scan_manifest_filesystem_baselines(
            &first_root_id,
            &[
                "same.mkv".to_owned(),
                "last.mkv".to_owned(),
                "missing-one.mkv".to_owned(),
                "missing-two.mkv".to_owned(),
            ],
        )
        .await
        .expect("full four-path bucket lookup");
    assert_eq!(database.query_count(), 1);
    assert_eq!(database.filesystem_path_templates.entry_count(), 2);
    assert_eq!(four_paths.len(), 2);

    database.reset_query_count();
    let other_root_entry = database
        .list_scan_manifest_filesystem_baselines(&other_root_id, &["same.mkv".to_owned()])
        .await
        .expect("same relative path in another root");
    assert_eq!(database.query_count(), 1);
    assert_eq!(
        other_root_entry["same.mkv"].id,
        "manifest-baseline-other-root"
    );

    let paths_with_matches_at = |count: usize, first: usize, last: usize| {
        let mut paths = (0..count)
            .map(|index| format!("missing-{index}.mkv"))
            .collect::<Vec<_>>();
        paths[first] = "same.mkv".to_owned();
        paths[last] = "last.mkv".to_owned();
        paths
    };

    for (count, first, last, expected_shapes) in
        [(256, 0, 255, 3), (257, 0, 256, 4), (2_900, 0, 2_899, 5)]
    {
        database.reset_query_count();
        let paths = paths_with_matches_at(count, first, last);
        let baselines = database
            .list_scan_manifest_filesystem_baselines(&first_root_id, &paths)
            .await
            .expect("manifest baseline bucket boundary lookup");
        assert_eq!(database.query_count(), 1, "path_count={count}");
        assert_eq!(
            database.filesystem_path_templates.entry_count(),
            expected_shapes,
            "path_count={count}"
        );
        assert_eq!(baselines.len(), 2, "path_count={count}");
    }

    for (count, expected_queries) in [(2_901, 2), (5_801, 3)] {
        database.reset_query_count();
        let paths = paths_with_matches_at(count, 0, count - 1);
        let baselines = database
            .list_scan_manifest_filesystem_baselines(&first_root_id, &paths)
            .await
            .expect("manifest baseline cross-chunk lookup");
        assert_eq!(
            database.query_count(),
            expected_queries,
            "path_count={count}"
        );
        assert_eq!(baselines.len(), 2, "path_count={count}");
        assert_eq!(
            baselines["same.mkv"].id, "manifest-baseline-first-root",
            "path_count={count}"
        );
        assert_eq!(
            baselines["last.mkv"].id, "manifest-baseline-last-root",
            "path_count={count}"
        );
    }
    database.close().await;
}

#[tokio::test]
async fn subtitle_stream_query_is_source_scoped_and_paginated() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    tokio::fs::write(root_path.join("Subtitle.Movie.2024.mkv"), b"fixture")
        .await
        .expect("movie");
    libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
        .await
        .expect("library root");
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await
        .expect("scan");

    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await
            .expect("item");
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await
        .expect("source");
    for (stream_index, codec, title) in [(2_i64, "srt", "English"), (3, "ass", "中文")] {
        sqlx::query(
            "INSERT INTO media_streams
             (id, media_source_id, stream_index, stream_type, codec, language, title,
              details_json, is_external, is_default, is_forced)
             VALUES (?, ?, ?, 'SUBTITLE', ?, ?, ?, ?, 0, ?, 0)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&source_id)
        .bind(stream_index)
        .bind(codec)
        .bind(if stream_index == 2 { "eng" } else { "zho" })
        .bind(title)
        .bind(r#"{"disposition":{"default":true}}"#)
        .bind(if stream_index == 2 { 1_i64 } else { 0 })
        .execute(database.pool())
        .await
        .expect("subtitle stream");
    }

    let page = database
        .list_subtitle_streams(&item_id, Some(&source_id), 1, 1)
        .await
        .expect("source-scoped page");
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].media_source_id, source_id);
    assert_eq!(page[0].item_id, item_id);
    assert_eq!(page[0].stream_index, 3);
    assert_eq!(page[0].codec.as_deref(), Some("ass"));
    assert_eq!(page[0].source_kind, "LOCAL_FILE");
    assert_eq!(page[0].relative_path, "Subtitle.Movie.2024.mkv");
    assert!(page[0].external_path.is_none());

    let default_page = database
        .list_subtitle_streams(&page[0].item_id, None, 0, 10)
        .await
        .expect("default source page");
    assert_eq!(default_page.len(), 2);
    assert!(
        database
            .list_subtitle_streams(&item_id, Some("not-this-source"), 0, 10)
            .await
            .expect("other source page")
            .is_empty()
    );
    database.close().await;
}

#[tokio::test]
async fn library_roots_can_be_loaded_by_id_in_one_query() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let first_path = temp_dir.path().join("first");
    let second_path = temp_dir.path().join("second");
    tokio::fs::create_dir_all(&first_path)
        .await
        .expect("first root");
    tokio::fs::create_dir_all(&second_path)
        .await
        .expect("second root");
    let first = libraries
        .add_root(library.id, first_path.to_str().expect("first path"))
        .await
        .expect("first library root")
        .root;
    let second = libraries
        .add_root(library.id, second_path.to_str().expect("second path"))
        .await
        .expect("second library root")
        .root;

    database.reset_query_count();
    let roots = database
        .list_library_roots_by_ids(&[first.id.to_string(), second.id.to_string()])
        .await
        .expect("roots");

    assert_eq!(roots.len(), 2);
    assert_eq!(database.query_count(), 1);
}

#[tokio::test]
async fn movie_batch_insert_uses_one_item_for_multiple_sources() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
        .await
        .expect("library root");
    let root = database
        .list_library_roots(&library.id.to_string())
        .await
        .expect("roots")
        .into_iter()
        .next()
        .expect("root");
    let files = vec![
        NewMovieFile {
            filesystem_entry_id: "entry-1".to_owned(),
            source_id: "source-1".to_owned(),
            relative_path: "Movie/Movie.2024.mkv".to_owned(),
            size: 1,
            modified_at: 1,
            device: None,
            inode: None,
            fingerprint: vec![1],
            title: "Movie".to_owned(),
            sort_title: "movie".to_owned(),
            original_title: "Movie".to_owned(),
            production_year: Some(2024),
            provider_ids_json: None,
            source_kind: "LOCAL_FILE".to_owned(),
            strm_target_kind: None,
            edition_name: None,
            quality_label: None,
            container: "mkv".to_owned(),
            external_url: None,
        },
        NewMovieFile {
            filesystem_entry_id: "entry-2".to_owned(),
            source_id: "source-2".to_owned(),
            relative_path: "Movie/Movie.2024.Directors.Cut.mkv".to_owned(),
            size: 2,
            modified_at: 2,
            device: None,
            inode: None,
            fingerprint: vec![2],
            title: "Movie".to_owned(),
            sort_title: "movie".to_owned(),
            original_title: "Movie".to_owned(),
            production_year: Some(2024),
            provider_ids_json: Some(r#"{"tmdb":"1"}"#.to_owned()),
            source_kind: "LOCAL_FILE".to_owned(),
            strm_target_kind: None,
            edition_name: Some("Director's Cut".to_owned()),
            quality_label: None,
            container: "mkv".to_owned(),
            external_url: None,
        },
    ];
    database.reset_query_count();
    let created_items = database
        .insert_movie_files_batch(&library.id.to_string(), &root.id, "generation", &files)
        .await
        .expect("batch insert");

    assert_eq!(created_items, 1);
    assert_eq!(database.query_count(), 7);
    let item_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_items WHERE item_type <> 'FOLDER'")
            .fetch_one(database.pool())
            .await
            .expect("item count");
    let source_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_sources")
        .fetch_one(database.pool())
        .await
        .expect("source count");
    assert_eq!(item_count, 1);
    assert_eq!(source_count, 2);
    let default_count: i64 = sqlx::query_scalar("SELECT SUM(is_default) FROM media_sources")
        .fetch_one(database.pool())
        .await
        .expect("default source count");
    assert_eq!(default_count, 1, "an item has exactly one default source");
    let folder_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_items WHERE item_type = 'FOLDER'")
            .fetch_one(database.pool())
            .await
            .expect("folder count");
    assert_eq!(folder_count, 1);

    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type <> 'FOLDER'")
            .fetch_one(database.pool())
            .await
            .expect("item id");
    let rows = database
        .list_catalog_rows_by_ids(std::slice::from_ref(&item_id))
        .await
        .expect("catalog rows");
    assert_eq!(rows.iter().filter(|row| row.item_id == item_id).count(), 2);
    let details = database
        .list_catalog_details_by_ids(std::slice::from_ref(&item_id))
        .await
        .expect("catalog details");
    assert!(details.contains_key(&item_id));
    let provider_ids: Option<String> =
        sqlx::query_scalar("SELECT provider_ids_json FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("provider ids");
    assert_eq!(provider_ids.as_deref(), Some(r#"{"tmdb":"1"}"#));

    let follow_up_file = NewMovieFile {
        filesystem_entry_id: "entry-3".to_owned(),
        source_id: "source-3".to_owned(),
        relative_path: "Another.Movie.2025.mkv".to_owned(),
        size: 3,
        modified_at: 3,
        device: None,
        inode: None,
        fingerprint: vec![3],
        title: "Another Movie".to_owned(),
        sort_title: "another movie".to_owned(),
        original_title: "Another Movie".to_owned(),
        production_year: Some(2025),
        provider_ids_json: Some(r#"{"tmdb":"2"}"#.to_owned()),
        source_kind: "LOCAL_FILE".to_owned(),
        strm_target_kind: None,
        edition_name: None,
        quality_label: None,
        container: "mkv".to_owned(),
        external_url: None,
    };
    database.reset_query_count();
    assert_eq!(
        database
            .insert_movie_files_batch(
                &library.id.to_string(),
                &root.id,
                "generation-2",
                &[follow_up_file],
            )
            .await
            .expect("follow-up batch insert"),
        1
    );
    assert_eq!(database.query_count(), 4);
}

#[tokio::test]
async fn manifest_movie_materialization_accepts_borrowed_records_and_reuses_item() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
        .await
        .expect("library root")
        .root;
    let root_id = root.id.to_string();
    let files = [
        NewMovieFile {
            filesystem_entry_id: "borrowed-entry-1".to_owned(),
            source_id: "borrowed-source-1".to_owned(),
            relative_path: "Movie/Movie.2024.mkv".to_owned(),
            size: 1,
            modified_at: 1,
            device: None,
            inode: None,
            fingerprint: vec![1],
            title: "Movie".to_owned(),
            sort_title: "movie".to_owned(),
            original_title: "Movie".to_owned(),
            production_year: Some(2024),
            provider_ids_json: None,
            source_kind: "LOCAL_FILE".to_owned(),
            strm_target_kind: None,
            edition_name: None,
            quality_label: None,
            container: "mkv".to_owned(),
            external_url: None,
        },
        NewMovieFile {
            filesystem_entry_id: "borrowed-entry-2".to_owned(),
            source_id: "borrowed-source-2".to_owned(),
            relative_path: "Movie/Movie.2024.Directors.Cut.mkv".to_owned(),
            size: 2,
            modified_at: 2,
            device: None,
            inode: None,
            fingerprint: vec![2],
            title: "Movie".to_owned(),
            sort_title: "movie".to_owned(),
            original_title: "Movie".to_owned(),
            production_year: Some(2024),
            provider_ids_json: Some(r#"{"tmdb":"1"}"#.to_owned()),
            source_kind: "LOCAL_FILE".to_owned(),
            strm_target_kind: None,
            edition_name: Some("Director's Cut".to_owned()),
            quality_label: None,
            container: "mkv".to_owned(),
            external_url: None,
        },
    ];
    for file in &files {
        database
            .query(
                "INSERT INTO filesystem_entries (
                     id, library_root_id, relative_path, entry_kind, size, modified_at,
                     fingerprint, last_seen_generation, last_seen_change_kind, is_missing
                 ) VALUES (?, ?, ?, 'FILE', ?, ?, ?, 'generation', 'NEW', 0)",
            )
            .bind(&file.filesystem_entry_id)
            .bind(&root_id)
            .bind(&file.relative_path)
            .bind(file.size)
            .bind(file.modified_at)
            .bind(&file.fingerprint)
            .execute(database.pool())
            .await
            .expect("claimed filesystem entry");
    }
    let borrowed_files = files.iter().collect::<Vec<_>>();
    let mut transaction = database
        .begin_scan_write_transaction()
        .await
        .expect("transaction");
    let created_items = database
        .insert_movie_files_without_filesystem_entries_refs_in_transaction(
            &mut transaction,
            &library.id.to_string(),
            &root_id,
            "generation",
            &borrowed_files,
        )
        .await
        .expect("borrowed movie materialization");
    transaction.commit().await.expect("commit");

    assert_eq!(created_items, 1);
    let counts: (i64, i64, i64, Option<String>) = sqlx::query_as(
        "SELECT COUNT(DISTINCT item.id), COUNT(source.id), SUM(source.is_default),
                MAX(item.provider_ids_json)
         FROM media_items item
         JOIN media_sources source ON source.item_id = item.id
         WHERE item.item_type = 'MOVIE' AND item.library_id = ?",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await
    .expect("materialized movie and sources");
    assert_eq!(counts, (1, 2, 1, Some(r#"{"tmdb":"1"}"#.to_owned())));
}

#[tokio::test]
async fn manifest_episode_materialization_accepts_borrowed_records_and_builds_hierarchy() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("shows");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("series root");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 series root"))
        .await
        .expect("library root")
        .root;
    let root_id = root.id.to_string();
    let episode = NewEpisodeFile {
        filesystem_entry_id: "borrowed-episode-entry".to_owned(),
        source_id: "borrowed-episode-source".to_owned(),
        relative_path: "Example Show/Season 01/Example.Show.S01E01.mkv".to_owned(),
        size: 10,
        modified_at: 1,
        device: None,
        inode: None,
        fingerprint: vec![1, 2, 3],
        series_identity: format!("series:{root_id}:Example Show"),
        series_title: "Example Show".to_owned(),
        series_sort_title: "example show".to_owned(),
        series_production_year: Some(2024),
        series_provider_ids_json: Some(r#"{"tvdb":"123"}"#.to_owned()),
        season_identity: format!("series:{root_id}:Example Show:season:1"),
        season_number: 1,
        episode_identity: format!(
            "episode:{root_id}:Example Show/Season 01/Example.Show.S01E01.mkv"
        ),
        episode_title: "Pilot".to_owned(),
        episode_sort_title: "pilot".to_owned(),
        episode_number: 1,
        episode_absolute_number: None,
        source_kind: "LOCAL_FILE".to_owned(),
        strm_target_kind: None,
        edition_name: None,
        quality_label: Some("1080p".to_owned()),
        container: "mkv".to_owned(),
        external_url: None,
    };
    database
        .query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 fingerprint, last_seen_generation, last_seen_change_kind, is_missing
             ) VALUES (?, ?, ?, 'FILE', ?, ?, ?, 'generation', 'NEW', 0)",
        )
        .bind(&episode.filesystem_entry_id)
        .bind(&root_id)
        .bind(&episode.relative_path)
        .bind(episode.size)
        .bind(episode.modified_at)
        .bind(&episode.fingerprint)
        .execute(database.pool())
        .await
        .expect("claimed filesystem entry");

    let borrowed_episodes = [&episode];
    let mut transaction = database
        .begin_scan_write_transaction()
        .await
        .expect("transaction");
    let created_items = database
        .insert_episode_files_without_filesystem_entries_refs_in_transaction(
            &mut transaction,
            &library.id.to_string(),
            &root_id,
            "generation",
            &borrowed_episodes,
        )
        .await
        .expect("borrowed episode materialization");
    transaction.commit().await.expect("commit");

    assert_eq!(created_items, 3);
    let hierarchy_counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT SUM(CASE WHEN item_type = 'SERIES' THEN 1 ELSE 0 END),
                SUM(CASE WHEN item_type = 'SEASON' THEN 1 ELSE 0 END),
                SUM(CASE WHEN item_type = 'EPISODE' THEN 1 ELSE 0 END)
         FROM media_items WHERE library_id = ?",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await
    .expect("series hierarchy");
    assert_eq!(hierarchy_counts, (1, 1, 1));
    let default_sources: i64 = sqlx::query_scalar(
        "SELECT SUM(source.is_default) FROM media_sources source
         JOIN media_items episode ON episode.id = source.item_id
         WHERE episode.library_id = ? AND episode.item_type = 'EPISODE'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await
    .expect("default episode source");
    assert_eq!(default_sources, 1);
}

#[tokio::test]
async fn movie_batch_insert_updates_strm_poster_fallbacks_as_a_set() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
        .await
        .expect("library root")
        .root;
    let files = [
        NewMovieFile {
            filesystem_entry_id: "strm-entry-1".to_owned(),
            source_id: "strm-source-1".to_owned(),
            relative_path: "First.Remote.2024.strm".to_owned(),
            size: 1,
            modified_at: 1,
            device: None,
            inode: None,
            fingerprint: vec![1],
            title: "First Remote".to_owned(),
            sort_title: "first remote".to_owned(),
            original_title: "First Remote".to_owned(),
            production_year: Some(2024),
            provider_ids_json: None,
            source_kind: "STRM_URL".to_owned(),
            strm_target_kind: Some("URL".to_owned()),
            edition_name: None,
            quality_label: None,
            container: "strm".to_owned(),
            external_url: Some("https://example.invalid/first".to_owned()),
        },
        NewMovieFile {
            filesystem_entry_id: "strm-entry-2".to_owned(),
            source_id: "strm-source-2".to_owned(),
            relative_path: "Second.Remote.2025.strm".to_owned(),
            size: 1,
            modified_at: 2,
            device: None,
            inode: None,
            fingerprint: vec![2],
            title: "Second Remote".to_owned(),
            sort_title: "second remote".to_owned(),
            original_title: "Second Remote".to_owned(),
            production_year: Some(2025),
            provider_ids_json: None,
            source_kind: "STRM_URL".to_owned(),
            strm_target_kind: Some("URL".to_owned()),
            edition_name: None,
            quality_label: None,
            container: "strm".to_owned(),
            external_url: Some("https://example.invalid/second".to_owned()),
        },
    ];

    database.reset_query_count();
    database
        .insert_movie_files_batch(
            &library.id.to_string(),
            &root.id.to_string(),
            "generation",
            &files,
        )
        .await
        .expect("batch insert");

    assert_eq!(
        database.query_count(),
        5,
        "STRM poster fallback promotion should use one set-based update"
    );
    let fallback_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items WHERE item_type = 'MOVIE' AND poster_fallback_required = 1",
    )
    .fetch_one(database.pool())
    .await
    .expect("poster fallback count");
    assert_eq!(fallback_count, 2);
}

#[tokio::test]
async fn movie_batch_insert_refreshes_existing_parent_folders_as_a_set() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
        .await
        .expect("library root")
        .root;

    let make_file = |index: usize, folder: &str, name: &str| NewMovieFile {
        filesystem_entry_id: format!("folder-entry-{index}"),
        source_id: format!("folder-source-{index}"),
        relative_path: format!("{folder}/{name}.202{index}.mkv"),
        size: 1,
        modified_at: index as i64,
        device: None,
        inode: None,
        fingerprint: vec![index as u8],
        title: format!("{name} {index}"),
        sort_title: format!("{name} {index}"),
        original_title: format!("{name} {index}"),
        production_year: Some(2020 + index as i64),
        provider_ids_json: None,
        source_kind: "LOCAL_FILE".to_owned(),
        strm_target_kind: None,
        edition_name: None,
        quality_label: None,
        container: "mkv".to_owned(),
        external_url: None,
    };

    let first_files = vec![
        make_file(1, "Alpha", "First"),
        make_file(2, "Beta", "First"),
        make_file(3, "Gamma", "First"),
    ];
    database
        .insert_movie_files_batch(
            &library.id.to_string(),
            &root.id.to_string(),
            "generation-1",
            &first_files,
        )
        .await
        .expect("first batch");

    sqlx::query(
        "UPDATE media_items
         SET title = 'stale', sort_title = 'stale', removed_at = unixepoch()
         WHERE identity_key = ?",
    )
    .bind(format!("folder:{}:Alpha", root.id))
    .execute(database.pool())
    .await
    .expect("stale folder");

    let second_files = vec![
        make_file(4, "Alpha", "Second"),
        make_file(5, "Beta", "Second"),
        make_file(6, "Gamma", "Second"),
    ];
    database.reset_query_count();
    database
        .insert_movie_files_batch(
            &library.id.to_string(),
            &root.id.to_string(),
            "generation-2",
            &second_files,
        )
        .await
        .expect("second batch");

    assert_eq!(
        database.query_count(),
        6,
        "existing parent folders should refresh in one set-based statement"
    );
    let (title, removed_at): (String, Option<i64>) =
        sqlx::query_as("SELECT title, removed_at FROM media_items WHERE identity_key = ?")
            .bind(format!("folder:{}:Alpha", root.id))
            .fetch_one(database.pool())
            .await
            .expect("refreshed folder");
    assert_eq!(title, "Alpha");
    assert!(removed_at.is_none());
}

#[tokio::test]
async fn write_probe_reports_a_query_only_sqlite_connection() {
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(
            AnyConnectOptions::from_str("sqlite://?mode=memory").expect("in-memory SQLite options"),
        )
        .await
        .expect("in-memory SQLite connection");
    sqlx::query("CREATE TABLE lux_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("create probe table");
    sqlx::query("PRAGMA query_only = ON")
        .execute(&pool)
        .await
        .expect("enable query-only mode");

    let database = Database {
        pool,
        log_store: LogStore::new(Path::new("unused-query-only-test")),
        pool_max_connections: 1,
        path: PathBuf::from("query-only-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
        recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
            RecommendationRatingMedianCache::default(),
        )),
        filesystem_path_templates: Arc::new(SqlTemplateCache::default()),
        query_count: Arc::new(AtomicUsize::new(0)),
    };
    assert!(database.probe_write().await.is_err());
    database.close().await;
}

#[tokio::test]
async fn metadata_jobs_process_series_before_seasons_and_episodes() {
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(
            AnyConnectOptions::from_str("sqlite://?mode=memory").expect("in-memory SQLite options"),
        )
        .await
        .expect("in-memory SQLite connection");
    sqlx::query("CREATE TABLE media_items (id TEXT PRIMARY KEY, item_type TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("create media items table");
    sqlx::query(
        "CREATE TABLE metadata_reidentify_job_items (
                job_id TEXT NOT NULL,
                item_id TEXT NOT NULL,
                status TEXT NOT NULL,
                priority INTEGER NOT NULL,
                PRIMARY KEY (job_id, item_id)
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata job items table");
    for (item_id, item_type, priority) in [
        ("episode", "EPISODE", 2_i64),
        ("season", "SEASON", 1_i64),
        ("series", "SERIES", 0_i64),
    ] {
        sqlx::query("INSERT INTO media_items (id, item_type) VALUES (?, ?)")
            .bind(item_id)
            .bind(item_type)
            .execute(&pool)
            .await
            .expect("insert media item");
        sqlx::query(
            "INSERT INTO metadata_reidentify_job_items
                 (job_id, item_id, status, priority)
                 VALUES ('job', ?, 'PENDING', ?)",
        )
        .bind(item_id)
        .bind(priority)
        .execute(&pool)
        .await
        .expect("insert metadata job item");
    }
    let database = Database {
        pool,
        log_store: LogStore::new(Path::new("unused-metadata-order-test")),
        pool_max_connections: 1,
        path: PathBuf::from("metadata-order-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
        recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
            RecommendationRatingMedianCache::default(),
        )),
        filesystem_path_templates: Arc::new(SqlTemplateCache::default()),
        query_count: Arc::new(AtomicUsize::new(0)),
    };

    assert_eq!(
        database.next_metadata_reidentify_item("job").await.unwrap(),
        Some("series".to_owned())
    );
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
             SET status = 'COMPLETED'
             WHERE job_id = 'job' AND item_id = 'series'",
    )
    .execute(&database.pool)
    .await
    .expect("complete series item");
    assert_eq!(
        database.next_metadata_reidentify_item("job").await.unwrap(),
        Some("season".to_owned())
    );
    database.close().await;
}

#[tokio::test]
async fn metadata_jobs_claim_items_in_priority_order_as_a_batch() {
    sqlx::any::install_default_drivers();
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(
            AnyConnectOptions::from_str("sqlite://?mode=memory").expect("in-memory SQLite options"),
        )
        .await
        .expect("in-memory SQLite connection");
    sqlx::query("CREATE TABLE media_items (id TEXT PRIMARY KEY, item_type TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("create media items table");
    sqlx::query(
        "CREATE TABLE metadata_reidentify_jobs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                cancel_requested INTEGER NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata jobs table");
    sqlx::query(
        "CREATE TABLE metadata_reidentify_job_items (
                job_id TEXT NOT NULL,
                item_id TEXT NOT NULL,
                status TEXT NOT NULL,
                priority INTEGER NOT NULL,
                updated_at INTEGER NOT NULL DEFAULT 0,
                request_fingerprint BLOB,
                request_capabilities_json TEXT NOT NULL DEFAULT '[]',
                claimed_request_fingerprint BLOB,
                claimed_request_capabilities_json TEXT NOT NULL DEFAULT '[]',
                PRIMARY KEY (job_id, item_id)
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata job items table");
    sqlx::query(
        "CREATE INDEX idx_metadata_reidentify_items_claim_priority
             ON metadata_reidentify_job_items(job_id, status, priority, item_id)",
    )
    .execute(&pool)
    .await
    .expect("create metadata job claim index");
    for (item_id, item_type, priority) in [
        ("episode", "EPISODE", 2_i64),
        ("season", "SEASON", 1_i64),
        ("series", "SERIES", 0_i64),
        ("movie", "MOVIE", 0_i64),
    ] {
        sqlx::query("INSERT INTO media_items (id, item_type) VALUES (?, ?)")
            .bind(item_id)
            .bind(item_type)
            .execute(&pool)
            .await
            .expect("insert media item");
        sqlx::query(
            "INSERT INTO metadata_reidentify_job_items
                 (job_id, item_id, status, priority)
                 VALUES ('job', ?, 'PENDING', ?)",
        )
        .bind(item_id)
        .bind(priority)
        .execute(&pool)
        .await
        .expect("insert metadata job item");
    }
    sqlx::query("INSERT INTO metadata_reidentify_jobs (id, status, cancel_requested) VALUES ('job', 'RUNNING', 0)")
        .execute(&pool)
        .await
        .expect("insert metadata job");
    let plan = sqlx::query(
        "EXPLAIN QUERY PLAN
         SELECT MIN(priority) FROM metadata_reidentify_job_items
         WHERE job_id = 'job' AND status = 'PENDING'",
    )
    .fetch_all(&pool)
    .await
    .expect("explain priority lookup");
    let plan_details = plan
        .iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>();
    assert!(
        plan_details.iter().any(|detail| {
            detail.contains("USING COVERING INDEX idx_metadata_reidentify_items_claim_priority")
        }),
        "priority lookup should use the claim index: {plan_details:?}"
    );
    let database = Database {
        pool,
        log_store: LogStore::new(Path::new("unused-metadata-batch-claim-test")),
        pool_max_connections: 1,
        path: PathBuf::from("metadata-batch-claim-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
        recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
            RecommendationRatingMedianCache::default(),
        )),
        filesystem_path_templates: Arc::new(SqlTemplateCache::default()),
        query_count: Arc::new(AtomicUsize::new(0)),
    };

    let claimed = database
        .claim_next_metadata_reidentify_items("job", 2)
        .await
        .expect("claim metadata items");
    assert_eq!(claimed, vec!["movie", "series"]);
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'COMPLETED'
         WHERE job_id = 'job' AND item_id IN ('movie', 'series')",
    )
    .execute(&database.pool)
    .await
    .expect("complete series item");
    let claimed = database
        .claim_next_metadata_reidentify_items("job", 2)
        .await
        .expect("claim remaining metadata items");
    assert_eq!(claimed, vec!["season"]);
    let statuses = sqlx::query_as::<_, (String, String)>(
        "SELECT item_id, status FROM metadata_reidentify_job_items
         WHERE job_id = 'job' ORDER BY item_id",
    )
    .fetch_all(&database.pool)
    .await
    .expect("read claimed statuses");
    assert_eq!(
        statuses,
        vec![
            ("episode".to_owned(), "PENDING".to_owned()),
            ("movie".to_owned(), "COMPLETED".to_owned()),
            ("season".to_owned(), "RUNNING".to_owned()),
            ("series".to_owned(), "COMPLETED".to_owned()),
        ]
    );
    database.close().await;
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_metadata_claim_uses_materialized_priorities_and_preserves_group_order()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration = DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    admin_pool.close().await;

    let connection = PostgresConnection {
        database: database_name.clone(),
        ..admin_connection.clone()
    };
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database =
        Database::connect_with_configuration(&config, &DatabaseConfiguration::Postgres(connection))
            .await?;
    let result = async {
        let library = LibraryService::new(database.clone())
            .create_library("PostgreSQL metadata claim", LibraryKind::Movie, false)
            .await?;
        let item_ids = [
            "metadata-claim-movie",
            "metadata-claim-series",
            "metadata-claim-season",
            "metadata-claim-episode",
        ];
        for (item_id, item_type) in [
            (item_ids[0], "MOVIE"),
            (item_ids[1], "SERIES"),
            (item_ids[2], "SEASON"),
            (item_ids[3], "EPISODE"),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES ($1, $2, $3, $1, $1, 'PENDING')",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .bind(item_type)
            .execute(database.pool())
            .await?;
        }
        database
            .create_metadata_reidentify_job(
                "metadata-claim-job",
                &item_ids.map(str::to_owned),
                "REIDENTIFY",
            )
            .await?;
        let priorities: Vec<(String, i32)> = sqlx::query_as(
            "SELECT item_id, priority FROM metadata_reidentify_job_items
             WHERE job_id = $1 ORDER BY priority, item_id",
        )
        .bind("metadata-claim-job")
        .fetch_all(database.pool())
        .await?;
        let claim_index_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_indexes
             WHERE schemaname = current_schema()
               AND indexname = 'idx_metadata_reidentify_items_claim_priority')",
        )
        .fetch_one(database.pool())
        .await?;

        let first_claim = database
            .claim_next_metadata_reidentify_items("metadata-claim-job", 2)
            .await?;
        let blocked_claim = database
            .claim_next_metadata_reidentify_items("metadata-claim-job", 2)
            .await?;
        sqlx::query(
            "UPDATE metadata_reidentify_job_items SET status = 'COMPLETED'
             WHERE job_id = $1 AND priority = 0",
        )
        .bind("metadata-claim-job")
        .execute(database.pool())
        .await?;
        let next_claim = database
            .claim_next_metadata_reidentify_items("metadata-claim-job", 2)
            .await?;

        Ok::<_, Box<dyn std::error::Error>>((
            priorities,
            claim_index_exists,
            first_claim,
            blocked_claim,
            next_claim,
        ))
    }
    .await;
    database.close().await;

    let cleanup_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&cleanup_pool)
    .await?;
    cleanup_pool.close().await;

    let (priorities, claim_index_exists, first_claim, blocked_claim, next_claim) = result?;
    assert!(claim_index_exists);
    assert_eq!(
        priorities,
        [
            ("metadata-claim-movie".to_owned(), 0),
            ("metadata-claim-series".to_owned(), 0),
            ("metadata-claim-season".to_owned(), 1),
            ("metadata-claim-episode".to_owned(), 2),
        ]
    );
    assert_eq!(
        first_claim,
        ["metadata-claim-movie", "metadata-claim-series"]
    );
    assert!(blocked_claim.is_empty());
    assert_eq!(next_claim, ["metadata-claim-season"]);
    Ok(())
}

#[tokio::test]
async fn server_settings_are_written_with_one_upsert() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    database.reset_query_count();
    database
        .set_server_settings(
            95,
            300_000_000,
            r#"{"thumbnailScrapingMode":"SCRAPER_FIRST"}"#,
            true,
            "PLUGIN",
        )
        .await?;

    assert_eq!(database.query_count(), 1);
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, value FROM server_settings
         WHERE key IN (
             'resume_played_percent', 'resume_min_ticks', 'media_strategy',
             'force_admin_library_order', 'login_background_source'
         )
         ORDER BY key",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        rows,
        vec![
            ("force_admin_library_order".to_owned(), "1".to_owned()),
            ("login_background_source".to_owned(), "PLUGIN".to_owned()),
            (
                "media_strategy".to_owned(),
                r#"{"thumbnailScrapingMode":"SCRAPER_FIRST"}"#.to_owned()
            ),
            ("resume_min_ticks".to_owned(), "300000000".to_owned()),
            ("resume_played_percent".to_owned(), "95".to_owned()),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn configured_user_deletion_grace_period_sets_purge_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    for user_id in [
        "default-retention-user",
        "custom-retention-user",
        "invalid-retention-user",
    ] {
        sqlx::query(
            "INSERT INTO users (
                id, username_normalized, display_name, password_hash
             ) VALUES (?, ?, ?, 'test')",
        )
        .bind(user_id)
        .bind(user_id)
        .bind(user_id)
        .execute(database.pool())
        .await?;
    }

    database
        .soft_delete_user("default-retention-user")
        .await?
        .expect("default retention user is deleted");
    let default_deadline: (i64, i64) =
        sqlx::query_as("SELECT deleted_at, deletion_purge_at FROM users WHERE id = ?")
            .bind("default-retention-user")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(default_deadline.1 - default_deadline.0, 7 * 86_400);

    sqlx::query(
        "INSERT INTO server_settings (key, value)
         VALUES ('user_deletion_grace_days', '30')",
    )
    .execute(database.pool())
    .await?;
    database
        .soft_delete_user("custom-retention-user")
        .await?
        .expect("custom retention user is deleted");
    let custom_deadline: (i64, i64) =
        sqlx::query_as("SELECT deleted_at, deletion_purge_at FROM users WHERE id = ?")
            .bind("custom-retention-user")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(custom_deadline.1 - custom_deadline.0, 30 * 86_400);

    sqlx::query(
        "UPDATE server_settings SET value = '0'
         WHERE key = 'user_deletion_grace_days'",
    )
    .execute(database.pool())
    .await?;
    database
        .soft_delete_user("invalid-retention-user")
        .await?
        .expect("invalid retention user is deleted");
    let invalid_deadline: (i64, i64) =
        sqlx::query_as("SELECT deleted_at, deletion_purge_at FROM users WHERE id = ?")
            .bind("invalid-retention-user")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(invalid_deadline.1 - invalid_deadline.0, 7 * 86_400);

    Ok(())
}

#[tokio::test]
async fn disable_and_soft_delete_user_revoke_credentials_and_stop_playback_sessions()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library = LibraryService::new(database.clone())
        .create_library("User deletion", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title,
            identification_status, has_available_source
         ) VALUES ('user-deletion-item', ?, 'MOVIE', 'Deletion', 'deletion',
                   'LOCAL_CONFIRMED', 1)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    for (user_id, is_disabled) in [("deletion-target", 0), ("deletion-other", 0)] {
        sqlx::query(
            "INSERT INTO users (
                id, username_normalized, display_name, password_hash,
                can_download, is_disabled
             ) VALUES (?, ?, ?, 'test', 1, ?)",
        )
        .bind(user_id)
        .bind(user_id)
        .bind(user_id)
        .bind(is_disabled)
        .execute(database.pool())
        .await?;
    }
    for (session_id, user_id, hash) in [
        ("delete-web-session", "deletion-target", vec![1_u8; 32]),
        ("other-web-session", "deletion-other", vec![2_u8; 32]),
    ] {
        sqlx::query(
            "INSERT INTO web_sessions (
                id, user_id, session_token_hash, csrf_token_hash, expires_at
             ) VALUES (?, ?, ?, ?, unixepoch() + 3600)",
        )
        .bind(session_id)
        .bind(user_id)
        .bind(&hash)
        .bind(vec![3_u8; 32])
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO access_tokens (
            id, token_hash, user_id, device_id, client_name, device_name, client_version
         ) VALUES ('delete-access-token', ?, 'deletion-target', 'device', 'client', 'device', '1')",
    )
    .bind(vec![4_u8; 32])
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO access_tokens (
            id, token_hash, user_id, device_id, client_name, device_name, client_version
         ) VALUES ('other-access-token', ?, 'deletion-other', 'device', 'client', 'device', '1')",
    )
    .bind(vec![5_u8; 32])
    .execute(database.pool())
    .await?;
    for (session_id, user_id, play_session_id, state) in [
        ("delete-playing", "deletion-target", "playing", "PLAYING"),
        ("delete-paused", "deletion-target", "paused", "PAUSED"),
        ("other-playing", "deletion-other", "other", "PLAYING"),
    ] {
        sqlx::query(
            "INSERT INTO playback_sessions (
                id, user_id, item_id, play_session_id, device_id, state
             ) VALUES (?, ?, 'user-deletion-item', ?, 'device', ?)",
        )
        .bind(session_id)
        .bind(user_id)
        .bind(play_session_id)
        .bind(state)
        .execute(database.pool())
        .await?;
    }
    for (session_id, plan) in [
        ("delete-web-direct", "DIRECT"),
        ("delete-web-hls", "SERVER_HLS"),
    ] {
        sqlx::query(
            "INSERT INTO web_playback_sessions (
                id, user_id, item_id, play_session_id, tier, plan, state,
                expires_at, last_heartbeat_at
             ) VALUES (?, 'deletion-target', 'user-deletion-item', ?, 0, ?, 'ACTIVE',
                       unixepoch() + 3600, unixepoch())",
        )
        .bind(session_id)
        .bind(session_id)
        .bind(plan)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO web_playback_sessions (
            id, user_id, item_id, play_session_id, tier, plan, state,
            expires_at, last_heartbeat_at
         ) VALUES ('other-web-active', 'deletion-other', 'user-deletion-item',
                   'other-web-active', 0, 'DIRECT', 'ACTIVE', unixepoch() + 3600, unixepoch())",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO web_playback_sessions (
            id, user_id, item_id, play_session_id, tier, plan, state,
            expires_at, last_heartbeat_at
         ) VALUES ('other-web-hls', 'deletion-other', 'user-deletion-item',
                   'other-web-hls', 0, 'SERVER_HLS', 'ACTIVE', unixepoch() + 3600, unixepoch())",
    )
    .execute(database.pool())
    .await?;

    assert_eq!(
        database.soft_delete_user("deletion-target").await?,
        Some(vec!["delete-web-hls".to_owned()])
    );
    let deleted: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT is_disabled, is_disabled_before_delete, can_download,
                deleted_at, deletion_purge_at
         FROM users WHERE id = 'deletion-target'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(deleted.0, 1);
    assert_eq!(deleted.1, 0);
    assert_eq!(deleted.2, 1, "permissions remain available for recovery");
    assert_eq!(deleted.4 - deleted.3, 7 * 86_400);
    let web_session_revoked: Option<i64> =
        sqlx::query_scalar("SELECT revoked_at FROM web_sessions WHERE id = 'delete-web-session'")
            .fetch_one(database.pool())
            .await?;
    assert!(web_session_revoked.is_some());
    let access_token_revoked: Option<i64> =
        sqlx::query_scalar("SELECT revoked_at FROM access_tokens WHERE id = 'delete-access-token'")
            .fetch_one(database.pool())
            .await?;
    assert!(access_token_revoked.is_some());
    let playback_states: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT id, state, is_paused FROM playback_sessions
         WHERE user_id = 'deletion-target' ORDER BY id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        playback_states,
        vec![
            ("delete-paused".to_owned(), "STOPPED".to_owned(), 0),
            ("delete-playing".to_owned(), "STOPPED".to_owned(), 0),
        ]
    );
    let web_playback_states: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, state FROM web_playback_sessions
         WHERE user_id = 'deletion-target' ORDER BY id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        web_playback_states,
        vec![
            ("delete-web-direct".to_owned(), "STOPPED".to_owned()),
            ("delete-web-hls".to_owned(), "STOPPED".to_owned()),
        ]
    );
    let other_session_revoked: Option<i64> =
        sqlx::query_scalar("SELECT revoked_at FROM web_sessions WHERE id = 'other-web-session'")
            .fetch_one(database.pool())
            .await?;
    assert!(other_session_revoked.is_none());
    let other_playback_state: String =
        sqlx::query_scalar("SELECT state FROM playback_sessions WHERE id = 'other-playing'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(other_playback_state, "PLAYING");
    let other_web_playback_state: String =
        sqlx::query_scalar("SELECT state FROM web_playback_sessions WHERE id = 'other-web-active'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(other_web_playback_state, "ACTIVE");

    let (_, disabled_hls_ids) = database
        .update_user_with_hls_session_ids(
            "deletion-other",
            UpdateUser {
                display_name: None,
                password_hash: None,
                has_password: None,
                is_disabled: Some(true),
                is_admin: None,
                can_manage_server: None,
                can_remote_access: None,
                can_download: None,
            },
        )
        .await?
        .ok_or("other user should be disabled")?;
    assert_eq!(disabled_hls_ids, ["other-web-hls"]);
    let other_web_session_revoked: Option<i64> =
        sqlx::query_scalar("SELECT revoked_at FROM web_sessions WHERE id = 'other-web-session'")
            .fetch_one(database.pool())
            .await?;
    assert!(other_web_session_revoked.is_some());
    let other_access_token_revoked: Option<i64> =
        sqlx::query_scalar("SELECT revoked_at FROM access_tokens WHERE id = 'other-access-token'")
            .fetch_one(database.pool())
            .await?;
    assert!(other_access_token_revoked.is_some());
    let disabled_playback_state: String =
        sqlx::query_scalar("SELECT state FROM playback_sessions WHERE id = 'other-playing'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(disabled_playback_state, "STOPPED");
    let disabled_web_playback_states: Vec<String> = sqlx::query_scalar(
        "SELECT state FROM web_playback_sessions
         WHERE user_id = 'deletion-other' ORDER BY id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(disabled_web_playback_states, ["STOPPED", "STOPPED"]);

    assert!(
        database
            .soft_delete_user("deletion-target")
            .await?
            .is_none()
    );
    let after_repeat: (i64, i64) = sqlx::query_as(
        "SELECT deleted_at, deletion_purge_at FROM users WHERE id = 'deletion-target'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!((after_repeat.0, after_repeat.1), (deleted.3, deleted.4));
    Ok(())
}

#[tokio::test]
async fn restore_deleted_user_restores_disabled_state_without_credentials()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    for (user_id, is_disabled) in [("restore-enabled", 0_i64), ("restore-disabled", 1_i64)] {
        sqlx::query(
            "INSERT INTO users (
                id, username_normalized, display_name, password_hash, is_disabled, can_download
             ) VALUES (?, ?, ?, 'test', ?, 1)",
        )
        .bind(user_id)
        .bind(user_id)
        .bind(user_id)
        .bind(is_disabled)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO web_sessions (
                id, user_id, session_token_hash, csrf_token_hash, expires_at
             ) VALUES (?, ?, ?, ?, unixepoch() + 3600)",
        )
        .bind(format!("{user_id}-session"))
        .bind(user_id)
        .bind(format!("{user_id}-session-token").into_bytes())
        .bind(format!("{user_id}-csrf-token").into_bytes())
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO access_tokens (
                id, token_hash, user_id, device_id, client_name, device_name, client_version
             ) VALUES (?, ?, ?, 'device', 'client', 'device', '1')",
        )
        .bind(format!("{user_id}-access-token"))
        .bind(format!("{user_id}-access-token-hash").into_bytes())
        .bind(user_id)
        .execute(database.pool())
        .await?;
        assert!(database.soft_delete_user(user_id).await?.is_some());
    }

    for (user_id, expected_disabled) in [("restore-enabled", 0_i64), ("restore-disabled", 1_i64)] {
        assert!(database.restore_deleted_user(user_id).await?);
        let restored: (i64, i64, i64, Option<i64>, Option<i64>) = sqlx::query_as(
            "SELECT is_disabled, is_disabled_before_delete, can_download,
                    deleted_at, deletion_purge_at
             FROM users WHERE id = ?",
        )
        .bind(user_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(restored.0, expected_disabled);
        assert_eq!(restored.1, 0);
        assert_eq!(restored.2, 1);
        assert!(restored.3.is_none());
        assert!(restored.4.is_none());
        let revoked_at: Option<i64> =
            sqlx::query_scalar("SELECT revoked_at FROM web_sessions WHERE id = ?")
                .bind(format!("{user_id}-session"))
                .fetch_one(database.pool())
                .await?;
        assert!(revoked_at.is_some());
        let access_token_revoked_at: Option<i64> =
            sqlx::query_scalar("SELECT revoked_at FROM access_tokens WHERE id = ?")
                .bind(format!("{user_id}-access-token"))
                .fetch_one(database.pool())
                .await?;
        assert!(access_token_revoked_at.is_some());
        assert!(
            !database.restore_deleted_user(user_id).await?,
            "a second restore must not alter the restored account"
        );
    }
    Ok(())
}

#[tokio::test]
async fn user_deletion_protects_last_manager_and_only_purges_deleted_users()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    for user_id in ["purge-manager-one", "purge-manager-two"] {
        sqlx::query(
            "INSERT INTO users (
                id, username_normalized, display_name, password_hash,
                is_admin, can_manage_server
             ) VALUES (?, ?, ?, 'test', 1, 1)",
        )
        .bind(user_id)
        .bind(user_id)
        .bind(user_id)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('purge-active-user', 'purge-active-user', 'Active', 'test')",
    )
    .execute(database.pool())
    .await?;

    assert!(!database.purge_deleted_user("purge-active-user").await?);
    assert!(
        database
            .soft_delete_user("purge-manager-one")
            .await?
            .is_some()
    );
    assert!(matches!(
        database.soft_delete_user("purge-manager-two").await,
        Err(StorageError::LastManager)
    ));
    let last_manager_deleted_at: Option<i64> =
        sqlx::query_scalar("SELECT deleted_at FROM users WHERE id = 'purge-manager-two'")
            .fetch_one(database.pool())
            .await?;
    assert!(last_manager_deleted_at.is_none());
    assert!(database.purge_deleted_user("purge-manager-one").await?);
    assert!(!database.purge_deleted_user("purge-manager-one").await?);
    assert!(!database.purge_deleted_user("missing-purge-user").await?);

    assert!(
        database
            .soft_delete_user("purge-active-user")
            .await?
            .is_some()
    );
    let purge_at: i64 =
        sqlx::query_scalar("SELECT deletion_purge_at FROM users WHERE id = 'purge-active-user'")
            .fetch_one(database.pool())
            .await?;
    assert!(
        database
            .purge_expired_deleted_users(purge_at - 1, 50)
            .await?
            .is_empty()
    );
    assert_eq!(
        database.purge_expired_deleted_users(purge_at, 50).await?,
        ["purge-active-user"]
    );
    assert!(
        database
            .purge_expired_deleted_users(purge_at, 50)
            .await?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn user_playback_policy_migration_defaults_keep_existing_playback_compatible()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('playback-policy-user', 'playback-policy-user', 'Playback Policy', 'test')",
    )
    .execute(database.pool())
    .await?;
    let policy: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT can_play, max_simultaneous_playbacks, allow_remux,
                allow_audio_transcode, allow_video_transcode
         FROM users WHERE id = 'playback-policy-user'",
    )
    .fetch_one(database.pool())
    .await?;

    assert_eq!(policy, (1, 0, 1, 1, 1));

    let legacy_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    sqlx::raw_sql(
        "CREATE TABLE users (id TEXT PRIMARY KEY);
         INSERT INTO users (id) VALUES ('legacy-playback-policy-user');",
    )
    .execute(&legacy_pool)
    .await?;
    sqlx::raw_sql(include_str!(
        "../../migrations/0185_user_playback_policy.sql"
    ))
    .execute(&legacy_pool)
    .await?;
    let migrated_policy: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT can_play, max_simultaneous_playbacks, allow_remux,
                allow_audio_transcode, allow_video_transcode
         FROM users WHERE id = 'legacy-playback-policy-user'",
    )
    .fetch_one(&legacy_pool)
    .await?;
    assert_eq!(migrated_policy, (1, 0, 1, 1, 1));
    assert!(
        sqlx::query(
            "UPDATE users SET max_simultaneous_playbacks = -1
         WHERE id = 'legacy-playback-policy-user'",
        )
        .execute(&legacy_pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query("UPDATE users SET can_play = 2 WHERE id = 'legacy-playback-policy-user'",)
            .execute(&legacy_pool)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn user_playback_admission_migration_creates_constrained_ledger()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let table_exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'playback_admissions'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        table_exists, 1,
        "fresh SQLite databases need the admission ledger"
    );

    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('admission-ledger-user', 'admission-ledger-user', 'Admission Ledger', 'test')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO playback_admissions (
             user_id, play_session_id, state, admitted_at, last_seen_at
         ) VALUES ('admission-ledger-user', 'play-session-1', 'ACTIVE', 100, 100)",
    )
    .execute(database.pool())
    .await?;
    assert!(
        sqlx::query(
            "INSERT INTO playback_admissions (
                 user_id, play_session_id, state, admitted_at, last_seen_at
             ) VALUES ('admission-ledger-user', 'play-session-1', 'ACTIVE', 101, 101)",
        )
        .execute(database.pool())
        .await
        .is_err(),
        "a user cannot have duplicate rows for one playback session"
    );
    assert!(
        sqlx::query(
            "INSERT INTO playback_admissions (
                 user_id, play_session_id, state, admitted_at, last_seen_at
             ) VALUES ('missing-admission-user', 'play-session-2', 'ACTIVE', 100, 100)",
        )
        .execute(database.pool())
        .await
        .is_err(),
        "admissions must reference an existing user"
    );
    assert!(
        sqlx::query(
            "INSERT INTO playback_admissions (
                 user_id, play_session_id, state, admitted_at, last_seen_at
             ) VALUES ('admission-ledger-user', 'play-session-3', 'UNKNOWN', 100, 100)",
        )
        .execute(database.pool())
        .await
        .is_err(),
        "admission state must be constrained"
    );
    assert!(
        sqlx::query(
            "INSERT INTO playback_admissions (
                 user_id, play_session_id, state, admitted_at, last_seen_at
             ) VALUES ('admission-ledger-user', 'play-session-4', 'ACTIVE', 100, 99)",
        )
        .execute(database.pool())
        .await
        .is_err(),
        "last-seen time cannot precede admission time"
    );

    let legacy_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    sqlx::raw_sql(
        "CREATE TABLE users (id TEXT PRIMARY KEY);
         INSERT INTO users (id) VALUES ('legacy-admission-user');
         CREATE TABLE playback_sessions (
             id TEXT PRIMARY KEY,
             user_id TEXT NOT NULL,
             play_session_id TEXT NOT NULL
         );
         INSERT INTO playback_sessions (id, user_id, play_session_id)
         VALUES ('legacy-playback', 'legacy-admission-user', 'legacy-session');",
    )
    .execute(&legacy_pool)
    .await?;
    sqlx::raw_sql(include_str!(
        "../../migrations/0186_user_playback_admissions.sql"
    ))
    .execute(&legacy_pool)
    .await?;
    let legacy_table_exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'playback_admissions'
         )",
    )
    .fetch_one(&legacy_pool)
    .await?;
    assert_eq!(
        legacy_table_exists, 1,
        "existing SQLite databases need the ledger migration"
    );
    let legacy_playback_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM playback_sessions WHERE id = 'legacy-playback'")
            .fetch_one(&legacy_pool)
            .await?;
    assert_eq!(
        legacy_playback_count, 1,
        "migration must preserve old playback records"
    );
    Ok(())
}

#[tokio::test]
async fn user_library_access_mode_migration_preserves_legacy_semantics()
-> Result<(), Box<dyn std::error::Error>> {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    sqlx::raw_sql(
        "CREATE TABLE users (id TEXT PRIMARY KEY);
         CREATE TABLE user_library_access (
             user_id TEXT NOT NULL,
             library_id TEXT NOT NULL,
             can_view INTEGER NOT NULL
         );
         INSERT INTO users (id) VALUES ('legacy-all'), ('legacy-denials-only'), ('legacy-selected');
         INSERT INTO user_library_access (user_id, library_id, can_view)
             VALUES ('legacy-denials-only', 'library-a', 0),
                    ('legacy-selected', 'library-a', 1);",
    )
    .execute(&pool)
    .await?;
    sqlx::raw_sql(include_str!(
        "../../migrations/0187_user_library_access_mode.sql"
    ))
    .execute(&pool)
    .await?;

    let modes: Vec<(String, String)> =
        sqlx::query_as("SELECT id, library_access_mode FROM users ORDER BY id")
            .fetch_all(&pool)
            .await?;
    assert_eq!(
        modes,
        [
            ("legacy-all".to_owned(), "ALL".to_owned()),
            ("legacy-denials-only".to_owned(), "ALL".to_owned()),
            ("legacy-selected".to_owned(), "SELECTED".to_owned()),
        ]
    );
    let legacy_access_rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT user_id, library_id, can_view FROM user_library_access
         ORDER BY user_id, library_id",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        legacy_access_rows,
        [
            ("legacy-denials-only".to_owned(), "library-a".to_owned(), 0,),
            ("legacy-selected".to_owned(), "library-a".to_owned(), 1),
        ]
    );
    sqlx::query("UPDATE users SET library_access_mode = 'NONE' WHERE id = 'legacy-all'")
        .execute(&pool)
        .await?;
    assert!(
        sqlx::query("UPDATE users SET library_access_mode = 'INVALID' WHERE id = 'legacy-all'")
            .execute(&pool)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn account_preferences_use_compare_and_swap_versions()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .insert_user(
            "preferences-cas-user",
            "preferences-cas-user",
            "Preferences CAS",
            "test",
            false,
            true,
        )
        .await?;
    let libraries = crate::application::libraries::LibraryService::new(database.clone());
    let library_one = libraries
        .create_library(
            "Preference order one",
            crate::library::LibraryKind::Movie,
            false,
        )
        .await?;
    let library_two = libraries
        .create_library(
            "Preference order two",
            crate::library::LibraryKind::Movie,
            false,
        )
        .await?;
    let initial_library_order = vec![library_two.id.to_string(), library_one.id.to_string()];
    let stale_library_order = vec![library_one.id.to_string()];
    let invalid_library_order = vec!["missing-library".to_owned()];
    assert!(
        database
            .user_account_preferences("preferences-cas-user")
            .await?
            .is_none()
    );

    let created = database
        .compare_and_swap_user_account_preferences(
            "preferences-cas-user",
            0,
            NewUserAccountPreferences {
                theme: "light",
                accent_color: "berry",
                show_media_libraries: true,
                show_continue_watching: false,
                audio_language: "日本語",
                subtitle_language: "关闭",
                auto_play_next_episode: false,
                library_order: Some(&initial_library_order),
                played_percent: Some(73),
            },
        )
        .await?;
    let UserAccountPreferencesCasResult::Updated(created) = created else {
        return Err("first account preference write should create version 1".into());
    };
    assert_eq!(created.version, 1);
    assert_eq!(created.theme, "light");
    assert_eq!(created.accent_color, "berry");
    assert!(created.show_media_libraries);
    assert!(!created.show_continue_watching);
    assert_eq!(created.audio_language, "日本語");
    assert_eq!(created.subtitle_language, "关闭");
    assert!(!created.auto_play_next_episode);
    assert_eq!(
        created.library_order,
        [library_two.id.to_string(), library_one.id.to_string()]
    );
    assert_eq!(created.played_percent, 73);
    assert_eq!(
        database.user_played_percent("preferences-cas-user").await?,
        73
    );

    let updated = database
        .compare_and_swap_user_account_preferences(
            "preferences-cas-user",
            1,
            NewUserAccountPreferences {
                theme: "dark",
                accent_color: "ocean",
                show_media_libraries: false,
                show_continue_watching: true,
                audio_language: "原始音轨",
                subtitle_language: "简体中文",
                auto_play_next_episode: true,
                library_order: None,
                played_percent: None,
            },
        )
        .await?;
    let UserAccountPreferencesCasResult::Updated(updated) = updated else {
        return Err("matching account preference version should update".into());
    };
    assert_eq!(updated.version, 2);
    assert_eq!(updated.theme, "dark");
    assert_eq!(updated.accent_color, "ocean");
    assert_eq!(updated.library_order, created.library_order);
    assert_eq!(updated.played_percent, 73);

    let stale = database
        .compare_and_swap_user_account_preferences(
            "preferences-cas-user",
            1,
            NewUserAccountPreferences {
                theme: "light",
                accent_color: "mint",
                show_media_libraries: true,
                show_continue_watching: true,
                audio_language: "原始音轨",
                subtitle_language: "简体中文",
                auto_play_next_episode: true,
                library_order: Some(&stale_library_order),
                played_percent: Some(49),
            },
        )
        .await?;
    assert_eq!(stale, UserAccountPreferencesCasResult::Conflict);
    assert_eq!(
        database.user_played_percent("preferences-cas-user").await?,
        73
    );
    assert_eq!(
        database
            .user_account_preferences("preferences-cas-user")
            .await?
            .map(|preferences| (preferences.version, preferences.accent_color)),
        Some((2, "ocean".to_owned()))
    );

    assert!(
        database
            .compare_and_swap_user_account_preferences(
                "preferences-cas-user",
                2,
                NewUserAccountPreferences {
                    theme: "sepia",
                    accent_color: "silver",
                    show_media_libraries: true,
                    show_continue_watching: true,
                    audio_language: "原始音轨",
                    subtitle_language: "简体中文",
                    auto_play_next_episode: true,
                    library_order: Some(&invalid_library_order),
                    played_percent: Some(41),
                },
            )
            .await
            .is_err()
    );
    assert_eq!(
        database
            .user_account_preferences("preferences-cas-user")
            .await?
            .map(|preferences| preferences.version),
        Some(2),
        "failed mirror writes must roll back the preference version"
    );
    assert_eq!(
        database.user_played_percent("preferences-cas-user").await?,
        73
    );
    assert_eq!(
        database.user_library_order("preferences-cas-user").await?,
        created.library_order
    );
    assert!(
        database
            .soft_delete_user("preferences-cas-user")
            .await?
            .is_some()
    );
    assert_eq!(
        database
            .compare_and_swap_user_account_preferences(
                "preferences-cas-user",
                2,
                NewUserAccountPreferences {
                    theme: "light",
                    accent_color: "silver",
                    show_media_libraries: true,
                    show_continue_watching: true,
                    audio_language: "原始音轨",
                    subtitle_language: "简体中文",
                    auto_play_next_episode: true,
                    library_order: None,
                    played_percent: None,
                },
            )
            .await?,
        UserAccountPreferencesCasResult::UserUnavailable
    );
    assert_eq!(
        database
            .compare_and_swap_user_account_preferences(
                "missing-preferences-user",
                0,
                NewUserAccountPreferences {
                    theme: "dark",
                    accent_color: "silver",
                    show_media_libraries: true,
                    show_continue_watching: true,
                    audio_language: "原始音轨",
                    subtitle_language: "简体中文",
                    auto_play_next_episode: true,
                    library_order: None,
                    played_percent: None,
                },
            )
            .await?,
        UserAccountPreferencesCasResult::UserUnavailable
    );
    Ok(())
}

#[tokio::test]
async fn user_content_restrictions_storage_uses_atomic_versioned_tag_replacement()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .insert_user(
            "content-policy-cas-user",
            "content-policy-cas-user",
            "Content Policy",
            "test",
            false,
            true,
        )
        .await?;
    assert!(
        database
            .user_content_restrictions("content-policy-cas-user")
            .await?
            .is_none()
    );

    let allowed = vec!["Family".to_owned(), "Animation".to_owned()];
    let blocked = vec!["Horror".to_owned()];
    let created = database
        .compare_and_swap_user_content_restrictions(
            "content-policy-cas-user",
            0,
            NewUserContentRestrictions {
                rating_system: "MPAA",
                maximum_rating: Some("PG-13"),
                allow_unrated: false,
                allowed_tags: &allowed,
                blocked_tags: &blocked,
            },
        )
        .await?;
    let UserContentRestrictionsCasResult::Updated(created) = created else {
        return Err("first content restriction write should create version 1".into());
    };
    assert_eq!(created.version, 1);
    assert_eq!(created.rating_system, "MPAA");
    assert_eq!(created.maximum_rating.as_deref(), Some("PG-13"));
    assert!(!created.allow_unrated);
    assert_eq!(created.allowed_tags, ["animation", "family"]);
    assert_eq!(created.blocked_tags, ["horror"]);

    let next_allowed = vec!["Documentary".to_owned()];
    let next_blocked = vec!["Violence".to_owned()];
    let updated = database
        .compare_and_swap_user_content_restrictions(
            "content-policy-cas-user",
            1,
            NewUserContentRestrictions {
                rating_system: "MPAA",
                maximum_rating: None,
                allow_unrated: true,
                allowed_tags: &next_allowed,
                blocked_tags: &next_blocked,
            },
        )
        .await?;
    let UserContentRestrictionsCasResult::Updated(updated) = updated else {
        return Err("matching content restriction version should update".into());
    };
    assert_eq!(updated.version, 2);
    assert_eq!(updated.maximum_rating, None);
    assert!(updated.allow_unrated);
    assert_eq!(updated.allowed_tags, ["documentary"]);
    assert_eq!(updated.blocked_tags, ["violence"]);

    let stale = database
        .compare_and_swap_user_content_restrictions(
            "content-policy-cas-user",
            1,
            NewUserContentRestrictions {
                rating_system: "MPAA",
                maximum_rating: Some("G"),
                allow_unrated: false,
                allowed_tags: &allowed,
                blocked_tags: &blocked,
            },
        )
        .await?;
    assert_eq!(stale, UserContentRestrictionsCasResult::Conflict);
    let unchanged = database
        .user_content_restrictions("content-policy-cas-user")
        .await?
        .ok_or("updated content restriction should be readable")?;
    assert_eq!(unchanged, updated);

    let duplicate_tags = vec!["Family".to_owned(), " family ".to_owned()];
    assert!(
        database
            .compare_and_swap_user_content_restrictions(
                "content-policy-cas-user",
                2,
                NewUserContentRestrictions {
                    rating_system: "MPAA",
                    maximum_rating: None,
                    allow_unrated: true,
                    allowed_tags: &duplicate_tags,
                    blocked_tags: &[],
                },
            )
            .await
            .is_err()
    );
    let too_many_tags = vec!["family".to_owned(); 101];
    assert!(
        database
            .compare_and_swap_user_content_restrictions(
                "content-policy-cas-user",
                2,
                NewUserContentRestrictions {
                    rating_system: "MPAA",
                    maximum_rating: None,
                    allow_unrated: true,
                    allowed_tags: &too_many_tags,
                    blocked_tags: &[],
                },
            )
            .await
            .is_err()
    );
    for (rating_system, maximum_rating) in [("  ", None), ("MPAA", Some(" \t "))] {
        assert!(
            database
                .compare_and_swap_user_content_restrictions(
                    "content-policy-cas-user",
                    2,
                    NewUserContentRestrictions {
                        rating_system,
                        maximum_rating,
                        allow_unrated: true,
                        allowed_tags: &[],
                        blocked_tags: &[],
                    },
                )
                .await
                .is_err()
        );
    }
    let too_long_tag = vec!["x".repeat(129)];
    assert!(
        database
            .compare_and_swap_user_content_restrictions(
                "content-policy-cas-user",
                2,
                NewUserContentRestrictions {
                    rating_system: "MPAA",
                    maximum_rating: None,
                    allow_unrated: true,
                    allowed_tags: &too_long_tag,
                    blocked_tags: &[],
                },
            )
            .await
            .is_err()
    );
    assert_eq!(
        database
            .compare_and_swap_user_content_restrictions(
                "content-policy-cas-user",
                -1,
                NewUserContentRestrictions {
                    rating_system: "MPAA",
                    maximum_rating: None,
                    allow_unrated: true,
                    allowed_tags: &[],
                    blocked_tags: &[],
                },
            )
            .await?,
        UserContentRestrictionsCasResult::Conflict
    );

    sqlx::query(
        "CREATE TRIGGER reject_content_restriction_tag_write
         BEFORE INSERT ON user_content_restriction_tags
         BEGIN SELECT RAISE(ABORT, 'injected content restriction tag failure'); END",
    )
    .execute(database.pool())
    .await?;
    assert!(
        database
            .compare_and_swap_user_content_restrictions(
                "content-policy-cas-user",
                2,
                NewUserContentRestrictions {
                    rating_system: "MPAA",
                    maximum_rating: Some("PG"),
                    allow_unrated: false,
                    allowed_tags: &allowed,
                    blocked_tags: &blocked,
                },
            )
            .await
            .is_err()
    );
    assert_eq!(
        database
            .user_content_restrictions("content-policy-cas-user")
            .await?
            .ok_or("content restriction should survive a failed tag write")?,
        updated,
        "policy version and tags must roll back together"
    );
    database.soft_delete_user("content-policy-cas-user").await?;
    assert_eq!(
        database
            .compare_and_swap_user_content_restrictions(
                "content-policy-cas-user",
                2,
                NewUserContentRestrictions {
                    rating_system: "MPAA",
                    maximum_rating: None,
                    allow_unrated: true,
                    allowed_tags: &[],
                    blocked_tags: &[],
                },
            )
            .await?,
        UserContentRestrictionsCasResult::UserUnavailable
    );
    assert_eq!(
        database
            .compare_and_swap_user_content_restrictions(
                "missing-content-policy-user",
                0,
                NewUserContentRestrictions {
                    rating_system: "MPAA",
                    maximum_rating: None,
                    allow_unrated: true,
                    allowed_tags: &[],
                    blocked_tags: &[],
                },
            )
            .await?,
        UserContentRestrictionsCasResult::UserUnavailable
    );
    Ok(())
}

#[tokio::test]
async fn user_permission_copy_snapshots_read_bounded_complete_snapshots()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    for (id, username) in [
        ("permission-copy-source", "permission-copy-source"),
        ("permission-copy-target", "permission-copy-target"),
        ("permission-copy-empty", "permission-copy-empty"),
        ("permission-copy-deleted", "permission-copy-deleted"),
    ] {
        database
            .insert_user(id, username, username, "test", false, true)
            .await?;
    }
    let library = LibraryService::new(database.clone())
        .create_library("Permission Copy", LibraryKind::Movie, false)
        .await?;
    let second_library = LibraryService::new(database.clone())
        .create_library("Permission Copy Two", LibraryKind::Movie, false)
        .await?;
    let mut library_ids = vec![library.id.to_string(), second_library.id.to_string()];
    library_ids.sort_unstable();
    database
        .replace_user_library_access_mode("permission-copy-source", "SELECTED", &library_ids)
        .await?;
    database
        .query(
            "UPDATE users SET can_download = 1, can_play = 1,
                max_simultaneous_playbacks = 3, allow_remux = 0,
                allow_audio_transcode = 1, allow_video_transcode = 0
             WHERE id = 'permission-copy-source'",
        )
        .execute(database.pool())
        .await?;
    let allowed_tags = vec!["Family".to_owned(), "Animation".to_owned()];
    let blocked_tags = vec!["Horror".to_owned(), "Action".to_owned()];
    let policy = database
        .compare_and_swap_user_content_restrictions(
            "permission-copy-source",
            0,
            NewUserContentRestrictions {
                rating_system: "MPA",
                maximum_rating: Some("PG-13"),
                allow_unrated: false,
                allowed_tags: &allowed_tags,
                blocked_tags: &blocked_tags,
            },
        )
        .await?;
    assert!(matches!(
        policy,
        UserContentRestrictionsCasResult::Updated(_)
    ));
    database
        .query("UPDATE users SET deleted_at = unixepoch() WHERE id = 'permission-copy-deleted'")
        .execute(database.pool())
        .await?;

    database.reset_query_count();
    let snapshots = database
        .user_permission_copy_snapshots(&[
            "permission-copy-source".to_owned(),
            "permission-copy-target".to_owned(),
            "permission-copy-empty".to_owned(),
            "permission-copy-deleted".to_owned(),
            "permission-copy-missing".to_owned(),
        ])
        .await?;

    assert_eq!(
        database.query_count(),
        3,
        "snapshot reads should stay batched"
    );
    assert_eq!(
        snapshots.len(),
        3,
        "missing and deleted users are unavailable"
    );
    let source = &snapshots["permission-copy-source"];
    assert_eq!(source.library_access_mode, "SELECTED");
    assert_eq!(source.library_ids, library_ids);
    assert!(source.can_download);
    assert!(source.playback_policy.can_play);
    assert_eq!(source.playback_policy.max_simultaneous_playbacks, 3);
    assert!(!source.playback_policy.allow_remux);
    assert!(source.playback_policy.allow_audio_transcode);
    assert!(!source.playback_policy.allow_video_transcode);
    let restrictions = source
        .content_restrictions
        .as_ref()
        .ok_or("source content restriction snapshot missing")?;
    assert_eq!(restrictions.version, 1);
    assert_eq!(restrictions.rating_system, "MPA");
    assert_eq!(restrictions.maximum_rating.as_deref(), Some("PG-13"));
    assert!(!restrictions.allow_unrated);
    assert_eq!(restrictions.allowed_tags, ["animation", "family"]);
    assert_eq!(restrictions.blocked_tags, ["action", "horror"]);

    let empty_policy_user = &snapshots["permission-copy-empty"];
    assert_eq!(empty_policy_user.library_access_mode, "ALL");
    assert!(empty_policy_user.content_restrictions.is_none());
    assert_eq!(
        empty_policy_user.playback_policy.max_simultaneous_playbacks,
        2
    );

    let over_limit = (0..102)
        .map(|index| format!("permission-copy-target-{index}"))
        .collect::<Vec<_>>();
    database.reset_query_count();
    assert!(
        database
            .user_permission_copy_snapshots(&over_limit)
            .await
            .is_err()
    );
    assert_eq!(
        database.query_count(),
        0,
        "over-limit reads should not query"
    );

    database.reset_query_count();
    assert!(
        database
            .user_permission_copy_snapshots(&[])
            .await?
            .is_empty()
    );
    assert_eq!(database.query_count(), 0, "empty reads should not query");
    Ok(())
}

#[tokio::test]
async fn user_permission_copy_applies_selected_groups_with_snapshot_checks()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    for id in [
        "permission-write-source",
        "permission-write-target",
        "permission-write-empty-source",
    ] {
        database
            .insert_user(id, id, id, "test", false, true)
            .await?;
    }
    let library = LibraryService::new(database.clone())
        .create_library("Permission Write", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    database
        .replace_user_library_access_mode(
            "permission-write-source",
            "SELECTED",
            std::slice::from_ref(&library_id),
        )
        .await?;
    database
        .replace_user_library_access_mode("permission-write-target", "NONE", &[])
        .await?;
    database
        .query(
            "UPDATE users SET can_download = 1, is_admin = 1, can_manage_server = 1
             WHERE id = 'permission-write-source'",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "UPDATE users SET can_download = 0, can_play = 0,
                max_simultaneous_playbacks = 1, allow_remux = 0,
                allow_audio_transcode = 0, allow_video_transcode = 0,
                is_admin = 0, can_manage_server = 0
             WHERE id = 'permission-write-target'",
        )
        .execute(database.pool())
        .await?;
    database
        .update_user_playback_policy(
            "permission-write-source",
            UserPlaybackPolicy {
                can_play: true,
                max_simultaneous_playbacks: 4,
                allow_remux: true,
                allow_audio_transcode: true,
                allow_video_transcode: false,
            },
        )
        .await?;
    let allowed_tags = vec!["Family".to_owned()];
    let blocked_tags = vec!["Horror".to_owned()];
    let source_policy = database
        .compare_and_swap_user_content_restrictions(
            "permission-write-source",
            0,
            NewUserContentRestrictions {
                rating_system: "MPA",
                maximum_rating: Some("PG-13"),
                allow_unrated: false,
                allowed_tags: &allowed_tags,
                blocked_tags: &blocked_tags,
            },
        )
        .await?;
    assert!(matches!(
        source_policy,
        UserContentRestrictionsCasResult::Updated(_)
    ));
    let target_allowed_tags = vec!["Documentary".to_owned()];
    let target_blocked_tags = vec!["Violence".to_owned()];
    let target_policy = database
        .compare_and_swap_user_content_restrictions(
            "permission-write-target",
            0,
            NewUserContentRestrictions {
                rating_system: "BBFC",
                maximum_rating: Some("12"),
                allow_unrated: true,
                allowed_tags: &target_allowed_tags,
                blocked_tags: &target_blocked_tags,
            },
        )
        .await?;
    assert!(matches!(
        target_policy,
        UserContentRestrictionsCasResult::Updated(_)
    ));

    let snapshots = database
        .user_permission_copy_snapshots(&[
            "permission-write-source".to_owned(),
            "permission-write-target".to_owned(),
            "permission-write-empty-source".to_owned(),
        ])
        .await?;
    assert_eq!(
        database
            .copy_user_permission_groups(
                "permission-write-source",
                &snapshots["permission-write-source"],
                "permission-write-target",
                &snapshots["permission-write-target"],
                UserPermissionCopyGroups {
                    library: false,
                    content: false,
                    playback: true,
                },
            )
            .await?,
        UserPermissionCopyResult::Updated
    );
    let playback_only = database
        .user_permission_copy_snapshots(&[
            "permission-write-source".to_owned(),
            "permission-write-target".to_owned(),
        ])
        .await?;
    let target = &playback_only["permission-write-target"];
    assert_eq!(target.library_access_mode, "NONE");
    assert!(target.library_ids.is_empty());
    assert!(target.can_download);
    assert!(target.playback_policy.can_play);
    assert_eq!(target.playback_policy.max_simultaneous_playbacks, 4);
    assert_eq!(
        target
            .content_restrictions
            .as_ref()
            .map(|policy| policy.rating_system.as_str()),
        Some("BBFC")
    );

    assert_eq!(
        database
            .copy_user_permission_groups(
                "permission-write-source",
                &playback_only["permission-write-source"],
                "permission-write-target",
                &playback_only["permission-write-target"],
                UserPermissionCopyGroups {
                    library: true,
                    content: true,
                    playback: false,
                },
            )
            .await?,
        UserPermissionCopyResult::Updated
    );
    let copied = database
        .user_permission_copy_snapshots(&[
            "permission-write-source".to_owned(),
            "permission-write-target".to_owned(),
        ])
        .await?;
    let source = &copied["permission-write-source"];
    let target = &copied["permission-write-target"];
    assert_eq!(target.library_access_mode, "SELECTED");
    assert_eq!(target.library_ids, [library_id.clone()]);
    assert!(target.can_download);
    let copied_content = target
        .content_restrictions
        .as_ref()
        .ok_or("target content restriction snapshot missing")?;
    let source_content = source
        .content_restrictions
        .as_ref()
        .ok_or("source content restriction snapshot missing")?;
    assert_eq!(copied_content.version, 2);
    assert_eq!(copied_content.rating_system, source_content.rating_system);
    assert_eq!(copied_content.maximum_rating, source_content.maximum_rating);
    assert_eq!(copied_content.allow_unrated, source_content.allow_unrated);
    assert_eq!(copied_content.allowed_tags, source_content.allowed_tags);
    assert_eq!(copied_content.blocked_tags, source_content.blocked_tags);
    let target_admin: (i64, i64) = sqlx::query_as(
        "SELECT is_admin, can_manage_server FROM users WHERE id = 'permission-write-target'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        target_admin,
        (0, 0),
        "copy must not grant management rights"
    );

    assert_eq!(
        database
            .copy_user_permission_groups(
                "permission-write-empty-source",
                &snapshots["permission-write-empty-source"],
                "permission-write-target",
                target,
                UserPermissionCopyGroups {
                    library: false,
                    content: true,
                    playback: false,
                },
            )
            .await?,
        UserPermissionCopyResult::Updated
    );
    let cleared = database
        .user_permission_copy_snapshots(&["permission-write-target".to_owned()])
        .await?;
    assert!(
        cleared["permission-write-target"]
            .content_restrictions
            .is_none()
    );

    let stale_target = copied["permission-write-target"].clone();
    database
        .update_user_playback_policy(
            "permission-write-target",
            UserPlaybackPolicy {
                can_play: false,
                max_simultaneous_playbacks: 2,
                allow_remux: false,
                allow_audio_transcode: false,
                allow_video_transcode: false,
            },
        )
        .await?;
    assert_eq!(
        database
            .copy_user_permission_groups(
                "permission-write-source",
                source,
                "permission-write-target",
                &stale_target,
                UserPermissionCopyGroups {
                    library: true,
                    content: false,
                    playback: true,
                },
            )
            .await?,
        UserPermissionCopyResult::Conflict
    );
    let after_conflict = database
        .user_permission_copy_snapshots(&["permission-write-target".to_owned()])
        .await?;
    assert_eq!(
        after_conflict["permission-write-target"].library_access_mode, "SELECTED",
        "stale playback snapshot must not partially copy the library group"
    );
    database
        .update_user_playback_policy(
            "permission-write-source",
            UserPlaybackPolicy {
                can_play: true,
                max_simultaneous_playbacks: 6,
                allow_remux: true,
                allow_audio_transcode: true,
                allow_video_transcode: false,
            },
        )
        .await?;
    assert_eq!(
        database
            .copy_user_permission_groups(
                "permission-write-source",
                source,
                "permission-write-target",
                &after_conflict["permission-write-target"],
                UserPermissionCopyGroups {
                    library: false,
                    content: false,
                    playback: true,
                },
            )
            .await?,
        UserPermissionCopyResult::Conflict,
        "stale source snapshot must not overwrite the target"
    );
    database
        .query("UPDATE libraries SET is_enabled = 0 WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    assert_eq!(
        database
            .copy_user_permission_groups(
                "permission-write-source",
                source,
                "permission-write-target",
                &after_conflict["permission-write-target"],
                UserPermissionCopyGroups {
                    library: true,
                    content: false,
                    playback: false,
                },
            )
            .await?,
        UserPermissionCopyResult::InvalidSnapshot
    );
    let after_invalid_library = database
        .user_permission_copy_snapshots(&["permission-write-target".to_owned()])
        .await?;
    assert_eq!(
        after_invalid_library["permission-write-target"].library_access_mode, "SELECTED",
        "unavailable source library must not alter target permissions"
    );
    Ok(())
}

#[tokio::test]
async fn user_permission_copy_rolls_back_all_selected_groups_on_content_write_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    for id in ["permission-rollback-source", "permission-rollback-target"] {
        database
            .insert_user(id, id, id, "test", false, true)
            .await?;
    }
    let library = LibraryService::new(database.clone())
        .create_library("Permission Rollback", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    database
        .replace_user_library_access_mode(
            "permission-rollback-source",
            "SELECTED",
            std::slice::from_ref(&library_id),
        )
        .await?;
    database
        .replace_user_library_access_mode("permission-rollback-target", "NONE", &[])
        .await?;
    database
        .update_user_playback_policy(
            "permission-rollback-source",
            UserPlaybackPolicy {
                can_play: true,
                max_simultaneous_playbacks: 5,
                allow_remux: true,
                allow_audio_transcode: true,
                allow_video_transcode: true,
            },
        )
        .await?;
    database
        .update_user_playback_policy(
            "permission-rollback-target",
            UserPlaybackPolicy {
                can_play: false,
                max_simultaneous_playbacks: 2,
                allow_remux: false,
                allow_audio_transcode: false,
                allow_video_transcode: false,
            },
        )
        .await?;
    let source_tags = vec!["Family".to_owned()];
    let no_tags: Vec<String> = Vec::new();
    for (user_id, expected_version, rating_system) in [
        ("permission-rollback-source", 0, "MPA"),
        ("permission-rollback-target", 0, "BBFC"),
    ] {
        let policy = database
            .compare_and_swap_user_content_restrictions(
                user_id,
                expected_version,
                NewUserContentRestrictions {
                    rating_system,
                    maximum_rating: None,
                    allow_unrated: true,
                    allowed_tags: if user_id == "permission-rollback-source" {
                        &source_tags
                    } else {
                        &no_tags
                    },
                    blocked_tags: &no_tags,
                },
            )
            .await?;
        assert!(matches!(
            policy,
            UserContentRestrictionsCasResult::Updated(_)
        ));
    }
    database
        .query(
            "CREATE TRIGGER fail_permission_copy_content_tag
             BEFORE INSERT ON user_content_restriction_tags
             WHEN NEW.user_id = 'permission-rollback-target'
             BEGIN SELECT RAISE(ABORT, 'forced permission copy failure'); END",
        )
        .execute(database.pool())
        .await?;
    let snapshots = database
        .user_permission_copy_snapshots(&[
            "permission-rollback-source".to_owned(),
            "permission-rollback-target".to_owned(),
        ])
        .await?;

    assert!(
        database
            .copy_user_permission_groups(
                "permission-rollback-source",
                &snapshots["permission-rollback-source"],
                "permission-rollback-target",
                &snapshots["permission-rollback-target"],
                UserPermissionCopyGroups {
                    library: true,
                    content: true,
                    playback: true,
                },
            )
            .await
            .is_err()
    );

    let after_failure = database
        .user_permission_copy_snapshots(&["permission-rollback-target".to_owned()])
        .await?;
    let target = &after_failure["permission-rollback-target"];
    assert_eq!(target.library_access_mode, "NONE");
    assert!(target.library_ids.is_empty());
    assert!(!target.playback_policy.can_play);
    assert_eq!(target.playback_policy.max_simultaneous_playbacks, 2);
    let retained_content = target
        .content_restrictions
        .as_ref()
        .ok_or("failed copy removed target content policy")?;
    assert_eq!(retained_content.version, 1);
    assert_eq!(retained_content.rating_system, "BBFC");
    assert!(retained_content.allowed_tags.is_empty());
    Ok(())
}

async fn assert_media_content_restriction_index_storage(
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let library = LibraryService::new(database.clone())
        .create_library("Restriction Index", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let original_nfo = r#"{"certification":"PG-13"}"#;
    let changed_nfo = r#"{"certification":"R"}"#;
    for (item_id, nfo_json, removed_at) in [
        ("restriction-index-a", Some(original_nfo), None),
        ("restriction-index-b", None, None),
        (
            "restriction-index-c",
            Some(r#"{"certification":"G"}"#),
            None,
        ),
        (
            "restriction-index-d",
            Some(r#"{"certification":"NC-17"}"#),
            Some(1_i64),
        ),
    ] {
        database
            .query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, identification_status,
                     nfo_metadata_json, removed_at
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?, ?)",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_id)
            .bind(item_id)
            .bind(nfo_json)
            .bind(removed_at)
            .execute(database.pool())
            .await?;
    }
    database
        .query(
            "INSERT INTO media_content_restriction_index (item_id, certification_code)
             VALUES ('restriction-index-c', 'G')",
        )
        .execute(database.pool())
        .await?;

    assert_eq!(
        database.media_content_restriction_index_status().await?,
        (3, 1)
    );

    let candidates = database
        .media_content_restriction_index_candidates(None, 1)
        .await?;
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].item_id, "restriction-index-a");
    assert_eq!(
        candidates[0].nfo_metadata_json.as_deref(),
        Some(original_nfo)
    );
    let after_first = database
        .media_content_restriction_index_candidates(Some("restriction-index-a"), 1)
        .await?;
    assert_eq!(after_first.len(), 1);
    assert_eq!(after_first[0].item_id, "restriction-index-b");
    assert!(
        database
            .media_content_restriction_index_candidates(Some("restriction-index-b"), 10)
            .await?
            .is_empty()
    );
    assert!(
        database
            .media_content_restriction_index_candidates(None, 0)
            .await?
            .is_empty()
    );
    assert!(
        !database
            .replace_media_content_restriction_index_if_nfo_json_matches(
                "restriction-index-d",
                Some(r#"{"certification":"NC-17"}"#),
                &ContentRestrictionIndexProjection {
                    certification_code: Some("NC-17".to_owned()),
                    tags: Vec::new(),
                },
            )
            .await?
    );

    let projection = ContentRestrictionIndexProjection {
        certification_code: Some("PG-13".to_owned()),
        tags: vec!["family".to_owned()],
    };
    assert!(
        database
            .replace_media_content_restriction_index_if_nfo_json_matches(
                "restriction-index-a",
                Some(original_nfo),
                &projection,
            )
            .await?
    );
    assert_eq!(
        database.media_content_restriction_index_status().await?,
        (3, 2)
    );
    let indexed: (Option<String>,) = database
        .query_as(
            "SELECT certification_code FROM media_content_restriction_index
             WHERE item_id = 'restriction-index-a'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(indexed.0.as_deref(), Some("PG-13"));
    let indexed_tags: Vec<String> = database
        .query_scalar(
            "SELECT tag FROM media_content_restriction_tags
             WHERE item_id = 'restriction-index-a' ORDER BY tag",
        )
        .fetch_all(database.pool())
        .await?;
    assert_eq!(indexed_tags, ["family"]);

    database
        .query("UPDATE media_items SET nfo_metadata_json = ? WHERE id = ?")
        .bind(changed_nfo)
        .bind("restriction-index-a")
        .execute(database.pool())
        .await?;
    assert!(
        !database
            .replace_media_content_restriction_index_if_nfo_json_matches(
                "restriction-index-a",
                Some(original_nfo),
                &projection,
            )
            .await?
    );
    let pending = database
        .media_content_restriction_index_candidates(None, 10)
        .await?;
    assert_eq!(
        pending,
        [
            MediaContentRestrictionIndexCandidate {
                item_id: "restriction-index-a".to_owned(),
                nfo_metadata_json: Some(changed_nfo.to_owned()),
            },
            MediaContentRestrictionIndexCandidate {
                item_id: "restriction-index-b".to_owned(),
                nfo_metadata_json: None,
            },
        ]
    );

    assert!(
        database
            .replace_media_content_restriction_index_if_nfo_json_matches(
                "restriction-index-b",
                None,
                &ContentRestrictionIndexProjection {
                    certification_code: None,
                    tags: vec!["x".repeat(129)],
                },
            )
            .await
            .is_err()
    );
    let failed_write_rows: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM media_content_restriction_index
             WHERE item_id = 'restriction-index-b'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        failed_write_rows, 0,
        "index and tags must roll back together"
    );
    assert!(
        database
            .replace_media_content_restriction_index_if_nfo_json_matches(
                "restriction-index-b",
                None,
                &ContentRestrictionIndexProjection {
                    certification_code: None,
                    tags: Vec::new(),
                },
            )
            .await?
    );
    assert!(
        database
            .replace_media_content_restriction_index_if_nfo_json_matches(
                "restriction-index-a",
                Some(changed_nfo),
                &ContentRestrictionIndexProjection {
                    certification_code: None,
                    tags: vec!["horror".to_owned()],
                },
            )
            .await?
    );
    let ready_unrated: (Option<String>,) = database
        .query_as(
            "SELECT certification_code FROM media_content_restriction_index
             WHERE item_id = 'restriction-index-b'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(ready_unrated.0, None);
    let ready_unrated_tags: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM media_content_restriction_tags
             WHERE item_id = 'restriction-index-b'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(ready_unrated_tags, 0);
    assert!(
        database
            .media_content_restriction_index_candidates(None, 10)
            .await?
            .is_empty()
    );

    for index in 0..105 {
        let item_id = format!("restriction-index-z-batch-{index:03}");
        database
            .query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
            )
            .bind(&item_id)
            .bind(&library_id)
            .bind(&item_id)
            .bind(&item_id)
            .execute(database.pool())
            .await?;
    }
    let bounded = database
        .media_content_restriction_index_candidates(None, usize::MAX)
        .await?;
    assert_eq!(bounded.len(), CONTENT_RESTRICTION_INDEX_BATCH_LIMIT);
    assert_eq!(
        bounded.first().map(|row| row.item_id.as_str()),
        Some("restriction-index-z-batch-000")
    );
    assert_eq!(
        bounded.last().map(|row| row.item_id.as_str()),
        Some("restriction-index-z-batch-099")
    );
    Ok(())
}

#[tokio::test]
async fn media_content_restriction_index_storage_uses_bounded_snapshot_writes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    assert_media_content_restriction_index_storage(&database).await
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_media_content_restriction_index_storage_uses_bounded_snapshot_writes()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_url = crate::config::DatabaseConfiguration::Postgres(admin_connection.clone())
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect_with_configuration(
        &Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        },
        &crate::config::DatabaseConfiguration::Postgres(PostgresConnection {
            database: database_name.clone(),
            ..admin_connection
        }),
    )
    .await?;
    let assertions = assert_media_content_restriction_index_storage(&database).await;
    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    assertions?;
    drop_database?;
    Ok(())
}

#[tokio::test]
async fn user_library_access_modes_are_enforced_by_storage_queries()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let libraries = LibraryService::new(database.clone());
    let first_library = libraries
        .create_library("Access Mode One", LibraryKind::Movie, false)
        .await?;
    let second_library = libraries
        .create_library("Access Mode Two", LibraryKind::Movie, false)
        .await?;
    for user_id in [
        "access-mode-all",
        "access-mode-selected",
        "access-mode-none",
    ] {
        database
            .insert_user(user_id, user_id, user_id, "test-hash", false, true)
            .await?;
    }
    let later_library = libraries
        .create_library("Access Mode Three", LibraryKind::Movie, false)
        .await?;

    database
        .set_user_library_access("access-mode-selected", &first_library.id.to_string(), true)
        .await?;
    database
        .set_user_library_access("access-mode-none", &first_library.id.to_string(), true)
        .await?;
    let selected_mode: String = sqlx::query_scalar(
        "SELECT library_access_mode FROM users WHERE id = 'access-mode-selected'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(selected_mode, "SELECTED");
    sqlx::query("UPDATE users SET library_access_mode = 'NONE' WHERE id = 'access-mode-none'")
        .execute(database.pool())
        .await?;

    assert!(
        database
            .has_user_library_access("access-mode-all", &first_library.id.to_string())
            .await?
    );
    assert!(
        database
            .has_user_library_access("access-mode-all", &second_library.id.to_string())
            .await?
    );
    assert!(
        database
            .has_user_library_access("access-mode-selected", &first_library.id.to_string())
            .await?
    );
    assert!(
        !database
            .has_user_library_access("access-mode-selected", &second_library.id.to_string())
            .await?
    );
    assert!(
        !database
            .has_user_library_access("access-mode-none", &first_library.id.to_string())
            .await?
    );
    assert!(
        !database
            .has_user_library_access("missing-access-mode-user", &first_library.id.to_string())
            .await?
    );

    assert_eq!(
        database
            .list_accessible_library_ids("access-mode-all")
            .await?,
        vec![
            first_library.id.to_string(),
            later_library.id.to_string(),
            second_library.id.to_string(),
        ]
    );
    assert_eq!(
        database
            .list_accessible_library_ids("access-mode-selected")
            .await?,
        vec![first_library.id.to_string()]
    );
    database
        .set_user_library_access("access-mode-selected", &first_library.id.to_string(), false)
        .await?;
    let mode_after_clearing_selection: String = sqlx::query_scalar(
        "SELECT library_access_mode FROM users WHERE id = 'access-mode-selected'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(mode_after_clearing_selection, "ALL");
    assert!(
        database
            .has_user_library_access("access-mode-selected", &later_library.id.to_string())
            .await?
    );
    assert!(
        database
            .list_accessible_library_ids("access-mode-none")
            .await?
            .is_empty()
    );
    assert!(
        database
            .list_accessible_library_ids("missing-access-mode-user")
            .await?
            .is_empty()
    );

    Ok(())
}

#[tokio::test]
async fn new_user_permission_snapshot_is_inserted_with_selected_library_access()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Default snapshot", LibraryKind::Movie, false)
        .await?;
    let user = NewStoredUser {
        id: "new-user-snapshot",
        username_normalized: "snapshot-user",
        display_name: "Snapshot User",
        password_hash: "test-hash",
        is_admin: false,
        has_password: true,
        email: None,
    };
    let library_ids = [library.id.to_string()];
    let permissions = NewStoredUserPermissions {
        library_access_mode: "SELECTED",
        library_ids: &library_ids,
        can_download: true,
        can_play: false,
        max_simultaneous_playbacks: 4,
        allow_remux: false,
        allow_audio_transcode: true,
        allow_video_transcode: false,
    };

    database
        .insert_user_with_email_and_permissions(user, permissions)
        .await?;

    let stored: (i64, i64, i64, i64, i64, i64, String) = sqlx::query_as(
        "SELECT can_download, can_play, max_simultaneous_playbacks,
                allow_remux, allow_audio_transcode, allow_video_transcode,
                library_access_mode
         FROM users WHERE id = 'new-user-snapshot'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored, (1, 0, 4, 0, 1, 0, "SELECTED".to_owned()));
    assert_eq!(
        database
            .list_accessible_library_ids("new-user-snapshot")
            .await?,
        library_ids
    );
    Ok(())
}

#[tokio::test]
async fn new_user_permission_snapshot_rolls_back_user_when_library_grant_fails()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Failing default snapshot", LibraryKind::Movie, false)
        .await?;
    sqlx::query(
        "CREATE TRIGGER fail_new_user_permission_grant
         BEFORE INSERT ON user_library_access
         BEGIN SELECT RAISE(ABORT, 'grant rejected'); END",
    )
    .execute(database.pool())
    .await?;
    let library_ids = [library.id.to_string()];
    let result = database
        .insert_user_with_email_and_permissions(
            NewStoredUser {
                id: "new-user-snapshot-rollback",
                username_normalized: "snapshot-rollback",
                display_name: "Snapshot Rollback",
                password_hash: "test-hash",
                is_admin: false,
                has_password: true,
                email: None,
            },
            NewStoredUserPermissions {
                library_access_mode: "SELECTED",
                library_ids: &library_ids,
                can_download: false,
                can_play: true,
                max_simultaneous_playbacks: 2,
                allow_remux: true,
                allow_audio_transcode: true,
                allow_video_transcode: true,
            },
        )
        .await;
    assert!(result.is_err());
    let user_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = 'new-user-snapshot-rollback'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(user_count, 0);
    Ok(())
}

#[tokio::test]
async fn apply_user_permission_preset_replaces_snapshot_and_source()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let libraries = LibraryService::new(database.clone());
    let first_library = libraries
        .create_library("Preset source first", LibraryKind::Movie, false)
        .await?;
    let second_library = libraries
        .create_library("Preset source second", LibraryKind::Movie, false)
        .await?;
    let first_library_ids = [first_library.id.to_string()];
    database
        .insert_user_with_email_and_permissions(
            NewStoredUser {
                id: "preset-apply-user",
                username_normalized: "preset-apply-user",
                display_name: "Preset Apply User",
                password_hash: "test-hash",
                is_admin: false,
                has_password: true,
                email: None,
            },
            NewStoredUserPermissions {
                library_access_mode: "SELECTED",
                library_ids: &first_library_ids,
                can_download: false,
                can_play: true,
                max_simultaneous_playbacks: 2,
                allow_remux: true,
                allow_audio_transcode: true,
                allow_video_transcode: true,
            },
        )
        .await?;
    database
        .insert_user(
            "preset-apply-secondary-manager",
            "preset-apply-secondary-manager",
            "Preset Apply Secondary Manager",
            "test-hash",
            true,
            true,
        )
        .await?;

    let second_library_ids = [second_library.id.to_string()];
    assert!(
        database
            .apply_user_permission_preset(
                "preset-apply-user",
                ApplyUserPermissionPreset {
                    preset_id: "preset-family",
                    preset_name: "Family",
                    preset_version: 2,
                    is_admin: false,
                    can_manage_server: false,
                    permissions: NewStoredUserPermissions {
                        library_access_mode: "SELECTED",
                        library_ids: &second_library_ids,
                        can_download: true,
                        can_play: false,
                        max_simultaneous_playbacks: 4,
                        allow_remux: false,
                        allow_audio_transcode: true,
                        allow_video_transcode: false,
                    },
                },
            )
            .await?
    );

    let stored: (i64, i64, i64, i64, i64, i64, i64, i64, String) = sqlx::query_as(
        "SELECT is_admin, can_manage_server, can_download, can_play,
                max_simultaneous_playbacks, allow_remux, allow_audio_transcode,
                allow_video_transcode,
                library_access_mode
         FROM users WHERE id = 'preset-apply-user'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored, (0, 0, 1, 0, 4, 0, 1, 0, "SELECTED".to_owned()));
    assert_eq!(
        database
            .list_accessible_library_ids("preset-apply-user")
            .await?,
        second_library_ids
    );
    let source: (String, String, i64, i64) = sqlx::query_as(
        "SELECT preset_id, preset_name, preset_version, applied_at
         FROM user_permission_preset_sources WHERE user_id = 'preset-apply-user'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(source.0, "preset-family");
    assert_eq!(source.1, "Family");
    assert_eq!(source.2, 2);
    assert!(source.3 > 0);

    assert!(
        database
            .apply_user_permission_preset(
                "preset-apply-user",
                ApplyUserPermissionPreset {
                    preset_id: "builtin-admin",
                    preset_name: "管理员",
                    preset_version: 1,
                    is_admin: true,
                    can_manage_server: true,
                    permissions: NewStoredUserPermissions {
                        library_access_mode: "ALL",
                        library_ids: &[],
                        can_download: true,
                        can_play: true,
                        max_simultaneous_playbacks: 0,
                        allow_remux: true,
                        allow_audio_transcode: true,
                        allow_video_transcode: true,
                    },
                },
            )
            .await?
    );
    let remaining_access: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_library_access WHERE user_id = 'preset-apply-user'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(remaining_access, 0);
    assert!(
        database
            .has_user_library_access("preset-apply-user", &first_library.id.to_string())
            .await?
    );
    assert!(
        database
            .has_user_library_access("preset-apply-user", &second_library.id.to_string())
            .await?
    );

    assert!(
        database
            .apply_user_permission_preset(
                "preset-apply-user",
                ApplyUserPermissionPreset {
                    preset_id: "builtin-user",
                    preset_name: "普通用户",
                    preset_version: 1,
                    is_admin: false,
                    can_manage_server: false,
                    permissions: NewStoredUserPermissions {
                        library_access_mode: "NONE",
                        library_ids: &[],
                        can_download: false,
                        can_play: true,
                        max_simultaneous_playbacks: 2,
                        allow_remux: true,
                        allow_audio_transcode: true,
                        allow_video_transcode: true,
                    },
                },
            )
            .await?
    );
    let remaining_access: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_library_access WHERE user_id = 'preset-apply-user'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(remaining_access, 0);
    assert!(
        !database
            .has_user_library_access("preset-apply-user", &first_library.id.to_string())
            .await?
    );
    let updated_source: (String, String, i64) = sqlx::query_as(
        "SELECT preset_id, preset_name, preset_version
         FROM user_permission_preset_sources WHERE user_id = 'preset-apply-user'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        updated_source,
        ("builtin-user".to_owned(), "普通用户".to_owned(), 1)
    );
    Ok(())
}

#[tokio::test]
async fn user_permission_preset_source_reads_support_single_and_batch_queries()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    for user_id in [
        "preset-source-first",
        "preset-source-second",
        "preset-source-empty",
    ] {
        database
            .insert_user(user_id, user_id, user_id, "test-hash", false, true)
            .await?;
    }
    for (user_id, preset_id, preset_name, preset_version, applied_at) in [
        ("preset-source-first", "family", "Family", 3_i64, 101_i64),
        (
            "preset-source-second",
            "builtin-user",
            "普通用户",
            1_i64,
            202_i64,
        ),
    ] {
        sqlx::query(
            "INSERT INTO user_permission_preset_sources
             (user_id, preset_id, preset_name, preset_version, applied_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(user_id)
        .bind(preset_id)
        .bind(preset_name)
        .bind(preset_version)
        .bind(applied_at)
        .execute(database.pool())
        .await?;
    }

    database.reset_query_count();
    assert_eq!(
        database
            .user_permission_preset_source("preset-source-first")
            .await?,
        Some(crate::storage::StoredUserPermissionPresetSource {
            preset_id: "family".to_owned(),
            preset_name: "Family".to_owned(),
            preset_version: 3,
            applied_at: 101,
        })
    );
    assert_eq!(database.query_count(), 1);
    database.reset_query_count();
    assert_eq!(
        database
            .user_permission_preset_source("preset-source-empty")
            .await?,
        None
    );
    assert_eq!(database.query_count(), 1);

    database.reset_query_count();
    let empty = database.user_permission_preset_sources(&[]).await?;
    assert!(empty.is_empty());
    assert_eq!(database.query_count(), 0);
    let user_ids = [
        "preset-source-first".to_owned(),
        "preset-source-second".to_owned(),
        "preset-source-empty".to_owned(),
    ];
    database.reset_query_count();
    let sources = database.user_permission_preset_sources(&user_ids).await?;
    assert_eq!(database.query_count(), 1);
    assert_eq!(sources.len(), 2);
    assert_eq!(
        sources["preset-source-second"],
        crate::storage::StoredUserPermissionPresetSource {
            preset_id: "builtin-user".to_owned(),
            preset_name: "普通用户".to_owned(),
            preset_version: 1,
            applied_at: 202,
        }
    );
    assert!(!sources.contains_key("preset-source-empty"));
    Ok(())
}

#[tokio::test]
async fn apply_user_permission_preset_protects_last_manager_and_ignores_deleted_users()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .insert_user(
            "only-manager",
            "only-manager",
            "Only Manager",
            "test-hash",
            true,
            true,
        )
        .await?;
    let regular_preset = ApplyUserPermissionPreset {
        preset_id: "builtin-user",
        preset_name: "普通用户",
        preset_version: 1,
        is_admin: false,
        can_manage_server: false,
        permissions: NewStoredUserPermissions {
            library_access_mode: "NONE",
            library_ids: &[],
            can_download: false,
            can_play: true,
            max_simultaneous_playbacks: 2,
            allow_remux: true,
            allow_audio_transcode: true,
            allow_video_transcode: true,
        },
    };
    assert!(matches!(
        database
            .apply_user_permission_preset("only-manager", regular_preset)
            .await,
        Err(StorageError::LastManager)
    ));
    assert!(
        !database
            .apply_user_permission_preset("missing-preset-user", regular_preset)
            .await?
    );

    database
        .insert_user(
            "second-preset-manager",
            "second-preset-manager",
            "Second Preset Manager",
            "test-hash",
            true,
            true,
        )
        .await?;
    let (first_demotion, second_demotion) = tokio::join!(
        database.apply_user_permission_preset("only-manager", regular_preset),
        database.apply_user_permission_preset("second-preset-manager", regular_preset),
    );
    assert_eq!(
        (matches!(&first_demotion, Ok(true)) as usize)
            + (matches!(&second_demotion, Ok(true)) as usize),
        1
    );
    assert!(
        matches!(&first_demotion, Err(StorageError::LastManager))
            || matches!(&second_demotion, Err(StorageError::LastManager))
    );
    let active_manager_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users
         WHERE can_manage_server = 1 AND is_disabled = 0 AND deleted_at IS NULL",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(active_manager_count, 1);

    database
        .insert_user(
            "deleted-preset-user",
            "deleted-preset-user",
            "Deleted Preset User",
            "test-hash",
            false,
            true,
        )
        .await?;
    sqlx::query("UPDATE users SET deleted_at = unixepoch() WHERE id = 'deleted-preset-user'")
        .execute(database.pool())
        .await?;
    assert!(
        !database
            .apply_user_permission_preset("deleted-preset-user", regular_preset)
            .await?
    );
    Ok(())
}

#[tokio::test]
async fn apply_user_permission_preset_rolls_back_when_library_write_fails()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Preset rollback", LibraryKind::Movie, false)
        .await?;
    database
        .insert_user(
            "preset-rollback-user",
            "preset-rollback-user",
            "Preset Rollback User",
            "test-hash",
            false,
            true,
        )
        .await?;
    sqlx::query(
        "CREATE TRIGGER fail_user_permission_preset_grant
         BEFORE INSERT ON user_library_access
         BEGIN SELECT RAISE(ABORT, 'grant rejected'); END",
    )
    .execute(database.pool())
    .await?;
    let library_ids = [library.id.to_string()];
    let result = database
        .apply_user_permission_preset(
            "preset-rollback-user",
            ApplyUserPermissionPreset {
                preset_id: "preset-family",
                preset_name: "Family",
                preset_version: 1,
                is_admin: true,
                can_manage_server: true,
                permissions: NewStoredUserPermissions {
                    library_access_mode: "SELECTED",
                    library_ids: &library_ids,
                    can_download: true,
                    can_play: false,
                    max_simultaneous_playbacks: 5,
                    allow_remux: false,
                    allow_audio_transcode: false,
                    allow_video_transcode: false,
                },
            },
        )
        .await;
    assert!(result.is_err());
    let stored: (i64, i64, i64, i64, String) = sqlx::query_as(
        "SELECT is_admin, can_manage_server, can_download, max_simultaneous_playbacks,
                library_access_mode
         FROM users WHERE id = 'preset-rollback-user'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored, (0, 0, 0, 2, "ALL".to_owned()));
    let source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_permission_preset_sources
         WHERE user_id = 'preset-rollback-user'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(source_count, 0);
    Ok(())
}

#[tokio::test]
async fn playback_admission_serializes_quota_and_reuses_releases_sessions()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let user_id = "playback-admission-user";
    database
        .insert_user(user_id, user_id, "Playback Admission", "test", false, true)
        .await?;
    database
        .update_user_playback_policy(
            user_id,
            UserPlaybackPolicy {
                can_play: true,
                max_simultaneous_playbacks: 1,
                allow_remux: true,
                allow_audio_transcode: true,
                allow_video_transcode: true,
            },
        )
        .await?;

    let now = 1_000_i64;
    let (first, second) = tokio::join!(
        database.reserve_playback_admission(user_id, "admission-session-a", now),
        database.reserve_playback_admission(user_id, "admission-session-b", now),
    );
    let first = first?.expect("user exists");
    let second = second?.expect("user exists");
    assert_ne!(first.decision, second.decision);
    let (accepted_session, denied) = if first.decision == PlaybackAdmissionDecision::Admitted {
        ("admission-session-a", second)
    } else {
        ("admission-session-b", first)
    };
    assert_eq!(
        denied.decision,
        PlaybackAdmissionDecision::Denied(PlaybackAdmissionDenialReason::ConcurrentLimit)
    );
    assert_eq!(denied.active_playback_count, 1);

    let renewal = database
        .reserve_playback_admission(user_id, accepted_session, now + 1)
        .await?
        .expect("user exists");
    assert_eq!(renewal.decision, PlaybackAdmissionDecision::Admitted);
    assert_eq!(renewal.active_playback_count, 1);
    assert!(
        database
            .release_playback_admission(user_id, accepted_session, now + 2)
            .await?
    );
    assert!(
        !database
            .release_playback_admission(user_id, accepted_session, now + 2)
            .await?
    );

    let retry = database
        .reserve_playback_admission(user_id, "admission-session-c", now + 3)
        .await?
        .expect("user exists");
    assert_eq!(retry.decision, PlaybackAdmissionDecision::Admitted);
    assert_eq!(retry.active_playback_count, 1);

    database
        .update_user_playback_policy(
            user_id,
            UserPlaybackPolicy {
                can_play: true,
                max_simultaneous_playbacks: 0,
                allow_remux: true,
                allow_audio_transcode: true,
                allow_video_transcode: true,
            },
        )
        .await?;
    for session_id in ["admission-session-d", "admission-session-e"] {
        let unlimited = database
            .reserve_playback_admission(user_id, session_id, now + 4)
            .await?
            .expect("user exists");
        assert_eq!(unlimited.decision, PlaybackAdmissionDecision::Admitted);
    }
    database
        .update_user_playback_policy(
            user_id,
            UserPlaybackPolicy {
                can_play: true,
                max_simultaneous_playbacks: 1,
                allow_remux: true,
                allow_audio_transcode: true,
                allow_video_transcode: true,
            },
        )
        .await?;
    for session_id in [
        "admission-session-c",
        "admission-session-d",
        "admission-session-e",
    ] {
        let existing = database
            .reserve_playback_admission(user_id, session_id, now + 5)
            .await?
            .expect("user exists");
        assert_eq!(existing.decision, PlaybackAdmissionDecision::Admitted);
        assert_eq!(existing.active_playback_count, 3);
    }
    let new_session = database
        .reserve_playback_admission(user_id, "admission-session-f", now + 6)
        .await?
        .expect("user exists");
    assert_eq!(
        new_session.decision,
        PlaybackAdmissionDecision::Denied(PlaybackAdmissionDenialReason::ConcurrentLimit)
    );
    assert_eq!(new_session.active_playback_count, 3);

    database
        .update_user_playback_policy(
            user_id,
            UserPlaybackPolicy {
                can_play: false,
                max_simultaneous_playbacks: 1,
                allow_remux: true,
                allow_audio_transcode: true,
                allow_video_transcode: true,
            },
        )
        .await?;
    let disabled = database
        .reserve_playback_admission(user_id, "admission-session-g", now + 7)
        .await?
        .expect("user exists");
    assert_eq!(
        disabled.decision,
        PlaybackAdmissionDecision::Denied(PlaybackAdmissionDenialReason::PlaybackDisabled)
    );
    assert_eq!(disabled.active_playback_count, 3);
    Ok(())
}

#[tokio::test]
async fn playback_admission_counts_legacy_sessions_and_reclaims_stale_reservations()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let user_id = "legacy-playback-admission-user";
    database
        .insert_user(
            user_id,
            user_id,
            "Legacy Playback Admission",
            "test",
            false,
            true,
        )
        .await?;
    database
        .update_user_playback_policy(
            user_id,
            UserPlaybackPolicy {
                can_play: true,
                max_simultaneous_playbacks: 1,
                allow_remux: true,
                allow_audio_transcode: true,
                allow_video_transcode: true,
            },
        )
        .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Playback Admission", LibraryKind::Movie, false)
        .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('playback-admission-item', ?, 'MOVIE', 'Admission', 'Admission', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    database
        .record_playback_event(NewPlaybackEvent {
            user_id,
            item_id: "playback-admission-item",
            media_source_id: None,
            play_session_id: "legacy-play-session",
            device_id: "test-device",
            client: None,
            device_name: None,
            client_version: None,
            device_type: None,
            remote_ip: None,
            state: "PAUSED",
            position_ticks: 10,
            duration_ticks: Some(100),
            played_percent: 95,
            is_paused: true,
        })
        .await?;
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await?;
    let existing = database
        .reserve_playback_admission(user_id, "legacy-play-session", now)
        .await?
        .expect("user exists");
    assert_eq!(existing.decision, PlaybackAdmissionDecision::Admitted);
    assert_eq!(existing.active_playback_count, 1);
    let denied = database
        .reserve_playback_admission(user_id, "new-play-session", now)
        .await?
        .expect("user exists");
    assert_eq!(
        denied.decision,
        PlaybackAdmissionDecision::Denied(PlaybackAdmissionDenialReason::ConcurrentLimit)
    );

    database
        .record_playback_event(NewPlaybackEvent {
            user_id,
            item_id: "playback-admission-item",
            media_source_id: None,
            play_session_id: "legacy-play-session",
            device_id: "test-device",
            client: None,
            device_name: None,
            client_version: None,
            device_type: None,
            remote_ip: None,
            state: "STOPPED",
            position_ticks: 10,
            duration_ticks: Some(100),
            played_percent: 95,
            is_paused: false,
        })
        .await?;
    assert!(
        database
            .release_playback_admission(user_id, "legacy-play-session", now + 1)
            .await?
    );
    let stale_admission = database
        .reserve_playback_admission(user_id, "stale-play-session", now + 2)
        .await?
        .expect("user exists");
    assert_eq!(stale_admission.active_playback_count, 1);
    let after_stale = database
        .reserve_playback_admission(user_id, "after-stale-play-session", now + 92)
        .await?
        .expect("user exists");
    assert_eq!(after_stale.decision, PlaybackAdmissionDecision::Admitted);
    assert_eq!(after_stale.active_playback_count, 1);
    let stale_state: String = sqlx::query_scalar(
        "SELECT state FROM playback_admissions
         WHERE user_id = ? AND play_session_id = 'stale-play-session'",
    )
    .bind(user_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stale_state, "RELEASED");
    Ok(())
}

#[tokio::test]
async fn new_user_playback_policy_defaults_to_two_and_updates()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    assert!(
        database
            .insert_initial_user(
                "initial-playback-policy-user",
                "initial-playback-policy-user",
                "Initial Playback Policy",
                "test",
            )
            .await?
    );
    database
        .insert_user(
            "new-playback-policy-user",
            "new-playback-policy-user",
            "New Playback Policy",
            "test",
            false,
            true,
        )
        .await?;

    let default_policy = UserPlaybackPolicy {
        can_play: true,
        max_simultaneous_playbacks: 2,
        allow_remux: true,
        allow_audio_transcode: true,
        allow_video_transcode: true,
    };
    for user_id in ["initial-playback-policy-user", "new-playback-policy-user"] {
        assert_eq!(
            database.get_user_playback_policy(user_id).await?,
            Some(default_policy)
        );
    }

    let updated_policy = UserPlaybackPolicy {
        can_play: false,
        max_simultaneous_playbacks: 1,
        allow_remux: false,
        allow_audio_transcode: false,
        allow_video_transcode: true,
    };
    assert!(
        database
            .update_user_playback_policy("new-playback-policy-user", updated_policy)
            .await?
    );
    assert_eq!(
        database
            .get_user_playback_policy("new-playback-policy-user")
            .await?,
        Some(updated_policy)
    );

    let invalid_policy = UserPlaybackPolicy {
        max_simultaneous_playbacks: -1,
        ..updated_policy
    };
    assert!(
        database
            .update_user_playback_policy("new-playback-policy-user", invalid_policy)
            .await
            .is_err()
    );
    assert!(
        !database
            .update_user_playback_policy("missing-playback-policy-user", updated_policy)
            .await?
    );
    database
        .soft_delete_user("new-playback-policy-user")
        .await?
        .ok_or("user soft delete should succeed")?;
    assert!(
        !database
            .update_user_playback_policy("new-playback-policy-user", updated_policy)
            .await?
    );
    Ok(())
}

#[tokio::test]
async fn uninstalling_a_plugin_batches_library_scraper_rewrites()
-> Result<(), Box<dyn std::error::Error>> {
    const REMOVED_PLUGIN: &str = "org.lux.removed-scraper";
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let first = libraries
        .create_library("First", LibraryKind::Movie, false)
        .await?;
    let second = libraries
        .create_library("Second", LibraryKind::Movie, false)
        .await?;
    let first_id = first.id.to_string();
    let second_id = second.id.to_string();

    sqlx::query(
        "INSERT INTO installed_plugins (plugin_id, is_enabled)
         VALUES (?, 1)",
    )
    .bind(REMOVED_PLUGIN)
    .execute(database.pool())
    .await?;
    for (library_id, scrapers) in [
        (
            first_id.as_str(),
            [
                (REMOVED_PLUGIN, 0_i64, "PRIMARY"),
                ("backup-one", 1, "BACKUP"),
            ]
            .as_slice(),
        ),
        (
            second_id.as_str(),
            [
                (REMOVED_PLUGIN, 0_i64, "PRIMARY"),
                ("backup-two", 1, "BACKUP"),
                ("supplement-two", 2, "SUPPLEMENT"),
            ]
            .as_slice(),
        ),
    ] {
        for (scraper_id, position, role) in scrapers {
            sqlx::query(
                "INSERT INTO library_scrapers (library_id, scraper_id, position, role)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(library_id)
            .bind(scraper_id)
            .bind(position)
            .bind(role)
            .execute(database.pool())
            .await?;
        }
    }
    sqlx::query(
        "UPDATE libraries
         SET scraper_id = ?, chapter_source_id = ?
         WHERE id = ?",
    )
    .bind(REMOVED_PLUGIN)
    .bind(REMOVED_PLUGIN)
    .bind(&first_id)
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE libraries SET scraper_id = ? WHERE id = ?")
        .bind(REMOVED_PLUGIN)
        .bind(&second_id)
        .execute(database.pool())
        .await?;

    database.reset_query_count();
    database.uninstall_plugin(REMOVED_PLUGIN).await?;
    assert_eq!(database.query_count(), 6);

    let first_scrapers: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT scraper_id, position, role FROM library_scrapers
         WHERE library_id = ? ORDER BY position",
    )
    .bind(&first_id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        first_scrapers,
        vec![("backup-one".to_owned(), 0, "PRIMARY".to_owned())]
    );
    let second_scrapers: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT scraper_id, position, role FROM library_scrapers
         WHERE library_id = ? ORDER BY position",
    )
    .bind(&second_id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        second_scrapers,
        vec![
            ("backup-two".to_owned(), 0, "PRIMARY".to_owned()),
            ("supplement-two".to_owned(), 1, "SUPPLEMENT".to_owned()),
        ]
    );
    let library_values: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, scraper_id, chapter_source_id FROM libraries
         WHERE id IN (?, ?) ORDER BY id",
    )
    .bind(&first_id)
    .bind(&second_id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        library_values,
        vec![
            (first_id.clone(), Some("backup-one".to_owned()), None),
            (second_id.clone(), Some("backup-two".to_owned()), None),
        ]
    );
    let installed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM installed_plugins WHERE plugin_id = ?")
            .bind(REMOVED_PLUGIN)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(installed, 0);
    Ok(())
}

#[tokio::test]
async fn metadata_attempt_state_loads_both_attempt_tables_with_one_query() {
    sqlx::any::install_default_drivers();
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(
            AnyConnectOptions::from_str("sqlite://?mode=memory").expect("in-memory SQLite options"),
        )
        .await
        .expect("in-memory SQLite connection");
    sqlx::query(
        "CREATE TABLE metadata_capability_attempts (
                item_id TEXT NOT NULL,
                provider TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                capability TEXT NOT NULL,
                status TEXT NOT NULL,
                next_retry_at INTEGER
            )",
    )
    .execute(&pool)
    .await
    .expect("create capability attempts");
    sqlx::query(
        "CREATE TABLE metadata_image_attempts (
                item_id TEXT NOT NULL,
                image_type TEXT NOT NULL,
                candidate_key TEXT NOT NULL,
                status TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .expect("create image attempts");
    sqlx::query(
        "INSERT INTO metadata_capability_attempts
             (item_id, provider, provider_id, capability, status, next_retry_at)
         VALUES ('item', 'tmdb', '42', 'CREDITS', 'UNAVAILABLE', 123)",
    )
    .execute(&pool)
    .await
    .expect("insert capability attempt");
    sqlx::query(
        "INSERT INTO metadata_image_attempts
             (item_id, image_type, candidate_key, status)
         VALUES ('item', 'POSTER', 'tmdb:42', 'FAILED')",
    )
    .execute(&pool)
    .await
    .expect("insert image attempt");
    let database = Database {
        pool,
        log_store: LogStore::new(Path::new("unused-metadata-attempt-test")),
        pool_max_connections: 1,
        path: PathBuf::from("metadata-attempt-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
        recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
            RecommendationRatingMedianCache::default(),
        )),
        filesystem_path_templates: Arc::new(SqlTemplateCache::default()),
        query_count: Arc::new(AtomicUsize::new(0)),
    };
    database.reset_query_count();

    let (capability_attempts, image_attempts) = database
        .list_metadata_attempts("item")
        .await
        .expect("load metadata attempts");

    assert_eq!(capability_attempts.len(), 1);
    assert_eq!(capability_attempts[0].capability, "CREDITS");
    assert_eq!(capability_attempts[0].next_retry_at, Some(123));
    assert_eq!(image_attempts.len(), 1);
    assert_eq!(image_attempts[0].image_type, "POSTER");
    assert_eq!(database.query_count(), 1);
    database.close().await;
}

#[tokio::test]
async fn item_media_strategy_settings_use_one_query_and_require_an_active_item() {
    sqlx::any::install_default_drivers();
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(
            AnyConnectOptions::from_str("sqlite://?mode=memory").expect("in-memory SQLite options"),
        )
        .await
        .expect("in-memory SQLite connection");
    sqlx::query(
        "CREATE TABLE libraries (
                id TEXT PRIMARY KEY,
                is_enabled INTEGER NOT NULL,
                media_strategy_json TEXT
            )",
    )
    .execute(&pool)
    .await
    .expect("create libraries");
    sqlx::query(
        "CREATE TABLE media_items (
                id TEXT PRIMARY KEY,
                library_id TEXT NOT NULL,
                removed_at INTEGER
            )",
    )
    .execute(&pool)
    .await
    .expect("create media items");
    sqlx::query("CREATE TABLE server_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(&pool)
        .await
        .expect("create server settings");
    sqlx::query(
        "INSERT INTO libraries (id, is_enabled, media_strategy_json)
         VALUES ('enabled', 1, '{\"thumbnailScrapingMode\":\"SCREENSHOT_FIRST\"}'),
                ('disabled', 0, '{\"thumbnailScrapingMode\":\"NONE\"}')",
    )
    .execute(&pool)
    .await
    .expect("insert libraries");
    sqlx::query(
        "INSERT INTO media_items (id, library_id, removed_at)
         VALUES ('active-item', 'enabled', NULL), ('disabled-item', 'disabled', NULL),
                ('removed-item', 'enabled', 123)",
    )
    .execute(&pool)
    .await
    .expect("insert media items");
    sqlx::query(
        "INSERT INTO server_settings (key, value)
         VALUES ('media_strategy', '{\"thumbnailScrapingMode\":\"SCRAPER_FIRST\"}')",
    )
    .execute(&pool)
    .await
    .expect("insert global strategy");
    let database = Database {
        pool,
        log_store: LogStore::new(Path::new("unused-media-strategy-test")),
        pool_max_connections: 1,
        path: PathBuf::from("media-strategy-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
        recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
            RecommendationRatingMedianCache::default(),
        )),
        filesystem_path_templates: Arc::new(SqlTemplateCache::default()),
        query_count: Arc::new(AtomicUsize::new(0)),
    };
    database.reset_query_count();

    let strategy = database
        .find_item_media_strategy_settings("active-item")
        .await
        .expect("load active item strategy");

    assert_eq!(
        strategy,
        Some((
            Some("{\"thumbnailScrapingMode\":\"SCREENSHOT_FIRST\"}".to_owned()),
            Some("{\"thumbnailScrapingMode\":\"SCRAPER_FIRST\"}".to_owned()),
        ))
    );
    assert_eq!(database.query_count(), 1);
    assert_eq!(
        database
            .find_item_media_strategy_settings("disabled-item")
            .await
            .expect("ignore disabled library"),
        None
    );
    assert_eq!(
        database
            .find_item_media_strategy_settings("removed-item")
            .await
            .expect("ignore removed item"),
        None
    );
    database.close().await;
}

#[tokio::test]
async fn local_metadata_completeness_dependencies_are_read_in_bounded_batches()
-> Result<(), Box<dyn std::error::Error>> {
    const ITEM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Completeness dependencies", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let item_ids = (0..ITEM_COUNT)
        .map(|index| format!("completeness-dependency-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO metadata_capability_attempts
             (item_id, provider, provider_id, capability, status, next_retry_at)
         VALUES (?, 'tmdb', '42', 'CREDITS', 'UNAVAILABLE', 123)",
    )
    .bind(&item_ids[0])
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO metadata_image_attempts
             (item_id, image_type, candidate_key, status)
         VALUES (?, 'POSTER', 'tmdb:42:POSTER', 'FAILED')",
    )
    .bind(&item_ids[0])
    .execute(database.pool())
    .await?;

    database.reset_query_count();
    for item_id in &item_ids {
        database.find_item_media_strategy_settings(item_id).await?;
        database.list_item_images(item_id).await?;
        database.list_metadata_attempts(item_id).await?;
    }
    assert_eq!(database.query_count(), ITEM_COUNT * 3);

    database.reset_query_count();
    let strategies = database
        .list_item_media_strategy_settings_by_ids(&item_ids)
        .await?;
    let images = database.list_item_images_by_ids(&item_ids).await?;
    let attempts = database
        .list_metadata_attempts_by_item_ids(&item_ids)
        .await?;

    assert_eq!(strategies.len(), ITEM_COUNT);
    assert_eq!(images.len(), 0);
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[&item_ids[0]].0[0].capability, "CREDITS");
    assert_eq!(attempts[&item_ids[0]].1[0].image_type, "POSTER");
    assert_eq!(database.query_count(), 3);
    Ok(())
}

#[tokio::test]
async fn item_scraper_configurations_are_read_in_one_bounded_batch()
-> Result<(), Box<dyn std::error::Error>> {
    const ITEM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library_with_scraper(
            "Scraper configuration",
            LibraryKind::Movie,
            false,
            Some("legacy-tmdb"),
            true,
        )
        .await?;
    let library_id = library.id.to_string();
    let item_ids = (0..ITEM_COUNT)
        .map(|index| format!("scraper-config-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await?;
    }

    database.reset_query_count();
    for item_id in &item_ids {
        database
            .query(
                "SELECT ls.scraper_id, ls.position, ls.role
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 JOIN library_scrapers ls ON ls.library_id = l.id
                 WHERE mi.id = ? AND mi.removed_at IS NULL
                 ORDER BY ls.position",
            )
            .bind(item_id)
            .fetch_all(database.pool())
            .await?;
    }
    assert_eq!(database.query_count(), ITEM_COUNT);

    database.reset_query_count();
    let configurations = database
        .list_item_scraper_configurations_by_ids(&item_ids)
        .await?;
    assert_eq!(configurations.len(), ITEM_COUNT);
    let (configured, legacy) = &configurations[&item_ids[0]];
    assert_eq!(configured.len(), 1);
    assert_eq!(configured[0].scraper_id, "legacy-tmdb");
    assert_eq!(legacy.as_deref(), Some("legacy-tmdb"));
    assert_eq!(database.query_count(), 1);
    Ok(())
}

#[tokio::test]
async fn plugin_installation_statuses_are_read_in_one_bounded_batch()
-> Result<(), Box<dyn std::error::Error>> {
    const PLUGIN_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let plugin_ids = (0..PLUGIN_COUNT)
        .map(|index| format!("org.lux.batch-plugin-{index:03}"))
        .collect::<Vec<_>>();
    sqlx::query(
        "INSERT INTO installed_plugins (plugin_id, is_enabled)
         VALUES (?, 1), (?, 0)",
    )
    .bind(&plugin_ids[0])
    .bind(&plugin_ids[1])
    .execute(database.pool())
    .await?;

    database.reset_query_count();
    for plugin_id in &plugin_ids {
        database.plugin_installation_status(plugin_id).await?;
    }
    assert_eq!(database.query_count(), PLUGIN_COUNT);

    database.reset_query_count();
    let statuses = database
        .list_plugin_installation_statuses_by_ids(&plugin_ids)
        .await?;
    assert_eq!(statuses.len(), 2);
    assert!(statuses[&plugin_ids[0]]);
    assert!(!statuses[&plugin_ids[1]]);
    assert_eq!(database.query_count(), 1);
    Ok(())
}

#[tokio::test]
async fn active_media_metadata_with_libraries_uses_one_bounded_query()
-> Result<(), Box<dyn std::error::Error>> {
    const ITEM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Metadata preflight", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let item_ids = (0..ITEM_COUNT)
        .map(|index| format!("metadata-preflight-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await?;
    }

    database.reset_query_count();
    for item_id in &item_ids {
        assert!(database.find_media_item_metadata(item_id).await?.is_some());
        assert_eq!(
            database.find_item_library_id(item_id).await?.as_deref(),
            Some(library_id.as_str())
        );
    }
    assert_eq!(database.query_count(), ITEM_COUNT * 2);

    database.reset_query_count();
    let metadata = database
        .list_active_media_item_metadata_with_libraries(&item_ids)
        .await?;
    assert_eq!(metadata.len(), ITEM_COUNT);
    assert!(
        metadata
            .values()
            .all(|(id, item)| { id == &library_id && item.item_type == "MOVIE" })
    );
    assert_eq!(database.query_count(), 1);
    Ok(())
}

#[tokio::test]
async fn media_writeback_contexts_are_read_in_one_bounded_batch()
-> Result<(), Box<dyn std::error::Error>> {
    const ITEM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Writeback contexts", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let item_ids = (0..ITEM_COUNT)
        .map(|index| format!("writeback-context-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await?;
    }

    database.reset_query_count();
    for item_id in &item_ids {
        assert_eq!(
            database
                .find_media_item_kind(item_id)
                .await?
                .as_ref()
                .map(|kind| kind.item_type.as_str()),
            Some("MOVIE")
        );
        assert!(
            database
                .find_metadata_writeback_source_path(item_id)
                .await?
                .is_none()
        );
    }
    assert_eq!(database.query_count(), ITEM_COUNT * 2);

    database.reset_query_count();
    let contexts = database
        .list_media_item_writeback_contexts_by_ids(&item_ids)
        .await?;
    assert_eq!(contexts.len(), ITEM_COUNT);
    assert_eq!(contexts[&item_ids[0]].item_type, "MOVIE");
    assert!(contexts[&item_ids[0]].source.is_none());
    assert_eq!(database.query_count(), 1);
    Ok(())
}

#[tokio::test]
async fn media_writeback_contexts_preserve_direct_and_episode_source_selection()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Writeback source selection", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES
            ('writeback-movie', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED'),
            ('writeback-series', ?, 'SERIES', 'Series', 'series', 'LOCAL_CONFIRMED'),
            ('writeback-episode', ?, 'EPISODE', 'Episode', 'episode', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .bind(&library_id)
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE media_items
         SET series_id = 'writeback-series'
         WHERE id = 'writeback-episode'",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('writeback-root', ?, ?, ?, 1, 1)",
    )
    .bind(&library_id)
    .bind(temp_dir.path().to_string_lossy().as_ref())
    .bind(temp_dir.path().to_string_lossy().as_ref())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO filesystem_entries (
             id, library_root_id, relative_path, entry_kind, size, modified_at,
             last_seen_generation
         ) VALUES
            ('writeback-movie-file', 'writeback-root', 'movie.mkv', 'FILE', 10, 1, 'generation'),
            ('writeback-episode-file', 'writeback-root', 'show/episode.mkv', 'FILE', 10, 1, 'generation')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_sources (
             id, item_id, source_kind, filesystem_entry_id, is_default, probe_status
         ) VALUES
            ('writeback-movie-source', 'writeback-movie', 'LOCAL_FILE',
             'writeback-movie-file', 1, 'READY'),
            ('writeback-episode-source', 'writeback-episode', 'LOCAL_FILE',
             'writeback-episode-file', 1, 'READY')",
    )
    .execute(database.pool())
    .await?;

    database.reset_query_count();
    let contexts = database
        .list_media_item_writeback_contexts_by_ids(&[
            "writeback-movie".to_owned(),
            "writeback-series".to_owned(),
        ])
        .await?;
    assert_eq!(database.query_count(), 1);
    assert_eq!(contexts["writeback-movie"].item_type, "MOVIE");
    assert_eq!(
        contexts["writeback-movie"]
            .source
            .as_ref()
            .map(|source| source.item_id.as_str()),
        Some("writeback-movie")
    );
    assert_eq!(contexts["writeback-series"].item_type, "SERIES");
    assert_eq!(
        contexts["writeback-series"]
            .source
            .as_ref()
            .map(|source| source.item_id.as_str()),
        Some("writeback-episode")
    );
    Ok(())
}

#[tokio::test]
async fn metadata_jobs_reconcile_items_left_running_by_workers() {
    sqlx::any::install_default_drivers();
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .connect_with(
            AnyConnectOptions::from_str("sqlite://?mode=memory").expect("in-memory SQLite options"),
        )
        .await
        .expect("in-memory SQLite connection");
    sqlx::query(
        "CREATE TABLE metadata_reidentify_jobs (
                id TEXT PRIMARY KEY,
                processed_count INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata jobs table");
    sqlx::query(
        "CREATE TABLE metadata_reidentify_job_items (
                job_id TEXT NOT NULL,
                item_id TEXT NOT NULL,
                status TEXT NOT NULL,
                candidate_count INTEGER NOT NULL,
                error TEXT,
                updated_at INTEGER NOT NULL,
                request_fingerprint BLOB,
                request_capabilities_json TEXT NOT NULL DEFAULT '[]',
                claimed_request_fingerprint BLOB,
                claimed_request_capabilities_json TEXT NOT NULL DEFAULT '[]',
                PRIMARY KEY (job_id, item_id)
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata job items table");
    sqlx::query(
        "INSERT INTO metadata_reidentify_jobs (id, processed_count, updated_at)
             VALUES ('job', 0, unixepoch())",
    )
    .execute(&pool)
    .await
    .expect("insert metadata job");
    for (item_id, status) in [
        ("running-1", "RUNNING"),
        ("running-2", "RUNNING"),
        ("done", "COMPLETED"),
    ] {
        sqlx::query(
            "INSERT INTO metadata_reidentify_job_items (
                    job_id, item_id, status, candidate_count, error, updated_at
                 ) VALUES ('job', ?, ?, 0, NULL, unixepoch())",
        )
        .bind(item_id)
        .bind(status)
        .execute(&pool)
        .await
        .expect("insert metadata job item");
    }
    let database = Database {
        pool,
        log_store: LogStore::new(Path::new("unused-metadata-reconcile-test")),
        pool_max_connections: 1,
        path: PathBuf::from("metadata-reconcile-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
        recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
            RecommendationRatingMedianCache::default(),
        )),
        filesystem_path_templates: Arc::new(SqlTemplateCache::default()),
        query_count: Arc::new(AtomicUsize::new(0)),
    };

    let reconciled = database
        .fail_running_metadata_reidentify_items("job", "WORKER_FAILED")
        .await
        .expect("reconcile running items");

    assert_eq!(reconciled, 2);
    let failed_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM metadata_reidentify_job_items
             WHERE job_id = 'job' AND status = 'FAILED' AND error = 'WORKER_FAILED'",
    )
    .fetch_one(database.pool())
    .await
    .expect("failed item count");
    assert_eq!(failed_count, 2);
    let processed_count: i64 =
        sqlx::query_scalar("SELECT processed_count FROM metadata_reidentify_jobs WHERE id = 'job'")
            .fetch_one(database.pool())
            .await
            .expect("processed count");
    assert_eq!(processed_count, 2);
    database.close().await;
}

#[test]
fn provider_identity_uses_the_selected_scraper_without_falling_back_to_another_id() {
    let providers = Some(
        serde_json::json!({
            "Imdb": "tt123",
            "Tvdb": "456"
        })
        .to_string(),
    );

    assert_eq!(
        first_provider_id(providers.clone(), None, Some("org.example.tvdb")),
        Some(("Tvdb".to_owned(), "456".to_owned()))
    );
    assert_eq!(first_provider_id(providers, None, Some("tmdb")), None);
}

#[test]
fn postgres_placeholder_adapter_preserves_quoted_question_marks() {
    let sql = "SELECT ?, '?' AS literal, \"?\" AS identifier, ?";
    assert_eq!(
        adapt_sql_for_backend(DatabaseBackend::Postgres, sql).as_str(),
        "SELECT $1, '?' AS literal, \"?\" AS identifier, $2"
    );
    assert_eq!(
        adapt_sql_for_backend(DatabaseBackend::Sqlite, sql).as_str(),
        sql
    );
}

#[test]
fn sqlite_sql_adapter_preserves_static_owned_and_arc_storage() {
    let static_sql = "SELECT 1";
    let adapted = adapt_sql_for_backend(DatabaseBackend::Sqlite, static_sql);
    assert!(std::ptr::eq(static_sql.as_ptr(), adapted.as_str().as_ptr()));

    let owned_sql = String::from("SELECT 2");
    let owned_pointer = owned_sql.as_ptr();
    let adapted = adapt_sql_for_backend(DatabaseBackend::Sqlite, sqlx::AssertSqlSafe(owned_sql));
    assert!(std::ptr::eq(owned_pointer, adapted.as_str().as_ptr()));

    let shared_sql: std::sync::Arc<str> = std::sync::Arc::from("SELECT 3");
    let shared_pointer = shared_sql.as_ptr();
    let adapted = adapt_sql_for_backend(DatabaseBackend::Sqlite, sqlx::AssertSqlSafe(shared_sql));
    assert!(std::ptr::eq(shared_pointer, adapted.as_str().as_ptr()));
}

#[test]
fn postgres_sql_adapter_preserves_unmodified_text_and_numbers_placeholders() {
    let no_parameters = "SELECT 'no parameters'";
    let adapted = adapt_sql_for_backend(DatabaseBackend::Postgres, no_parameters);
    assert!(std::ptr::eq(
        no_parameters.as_ptr(),
        adapted.as_str().as_ptr()
    ));

    let parameters = std::iter::repeat_n("?", 12).collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT ?, '?' AS literal, \"?\" AS identifier, 'it''s ?' AS escaped_literal, {parameters}"
    );
    assert_eq!(
        adapt_sql_for_backend(DatabaseBackend::Postgres, sqlx::AssertSqlSafe(sql)).as_str(),
        "SELECT $1, '?' AS literal, \"?\" AS identifier, 'it''s ?' AS escaped_literal, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13"
    );
}

#[tokio::test]
async fn chapter_detection_job_creation_is_atomic_per_library() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Shows", LibraryKind::Series, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    fn new_job<'a>(id: &'a str, library_id: &'a str) -> NewChapterDetectionJob<'a> {
        NewChapterDetectionJob {
            id,
            library_id,
            plugin_id: "org.lux.intro-outro-detector",
            concurrency: 1,
            intro_window_seconds: 180,
            credits_window_seconds: 180,
            match_threshold: 0.8,
            total_count: 0,
        }
    }

    assert!(
        database
            .create_chapter_detection_job(new_job("chapter-job-1", &library_id))
            .await
            .expect("first job should be created")
    );
    assert!(
        !database
            .create_chapter_detection_job(new_job("chapter-job-2", &library_id))
            .await
            .expect("active duplicate should be rejected")
    );
}

#[tokio::test]
async fn chapter_detection_job_items_are_inserted_in_bounded_batches() {
    const ITEM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Chapter batch", LibraryKind::Series, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('chapter-season-batch', ?, 'SEASON', 'Season 1', 'season-1', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("season item");
    sqlx::query(
        "WITH RECURSIVE sequence(value) AS (
             SELECT 1 UNION ALL SELECT value + 1 FROM sequence WHERE value < ?
         )
         INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         )
         SELECT 'chapter-item-' || value, ?, 'EPISODE', 'Episode ' || value,
                'episode-' || value, 'LOCAL_CONFIRMED'
         FROM sequence",
    )
    .bind(ITEM_COUNT as i64)
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("episode items");
    sqlx::query(
        "WITH RECURSIVE sequence(value) AS (
             SELECT 1 UNION ALL SELECT value + 1 FROM sequence WHERE value < ?
         )
         INSERT INTO media_sources (id, item_id, source_kind, probe_status)
         SELECT 'chapter-source-' || value, 'chapter-item-' || value, 'LOCAL_FILE', 'READY'
         FROM sequence",
    )
    .bind(ITEM_COUNT as i64)
    .execute(database.pool())
    .await
    .expect("media sources");

    let source_ids = (1..=ITEM_COUNT)
        .map(|index| format!("chapter-source-{index}"))
        .collect::<Vec<_>>();
    let item_ids = (1..=ITEM_COUNT)
        .map(|index| format!("chapter-item-{index}"))
        .collect::<Vec<_>>();
    let job_id = "chapter-job-batch-insert";
    let new_job = |id| NewChapterDetectionJob {
        id,
        library_id: &library_id,
        plugin_id: "org.lux.intro-outro-detector",
        concurrency: 1,
        intro_window_seconds: 180,
        credits_window_seconds: 180,
        match_threshold: 0.8,
        total_count: ITEM_COUNT as i64,
    };
    assert!(
        database
            .create_chapter_detection_job(new_job(job_id))
            .await
            .expect("create chapter job")
    );

    database.reset_query_count();
    database
        .insert_chapter_detection_job_items(&[])
        .await
        .expect("empty input should be a no-op");
    assert_eq!(database.query_count(), 0);

    let input_fingerprint = [0x24; 32];
    let source_fingerprint = b"source-fingerprint";
    let items = source_ids
        .iter()
        .zip(&item_ids)
        .enumerate()
        .map(|(index, (source_id, item_id))| NewChapterDetectionJobItem {
            job_id,
            source_id,
            item_id,
            season_id: "chapter-season-batch",
            source_fingerprint,
            input_fingerprint: &input_fingerprint,
            is_context: index % 2 == 0,
        })
        .collect::<Vec<_>>();
    database.reset_query_count();
    database
        .insert_chapter_detection_job_items(&items)
        .await
        .expect("insert chapter job items");
    assert_eq!(database.query_count(), 3);
    let persisted_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM chapter_detection_job_items WHERE job_id = ?")
            .bind(job_id)
            .fetch_one(database.pool())
            .await
            .expect("persisted item count");
    assert_eq!(persisted_count, ITEM_COUNT as i64);
    let context_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM chapter_detection_job_items
         WHERE job_id = ? AND is_context = 1",
    )
    .bind(job_id)
    .fetch_one(database.pool())
    .await
    .expect("context item count");
    assert_eq!(context_count, ITEM_COUNT.div_ceil(2) as i64);
    let stored_item: (String, Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT status, source_fingerprint, input_fingerprint
         FROM chapter_detection_job_items WHERE job_id = ? AND source_id = ?",
    )
    .bind(job_id)
    .bind(&source_ids[0])
    .fetch_one(database.pool())
    .await
    .expect("stored item fields");
    assert_eq!(stored_item.0, "PENDING");
    assert_eq!(stored_item.1, source_fingerprint);
    assert_eq!(stored_item.2, input_fingerprint);

    sqlx::query("UPDATE chapter_detection_jobs SET status = 'COMPLETED' WHERE id = ?")
        .bind(job_id)
        .execute(database.pool())
        .await
        .expect("complete first job");
    let rollback_job_id = "chapter-job-batch-rollback";
    assert!(
        database
            .create_chapter_detection_job(new_job(rollback_job_id))
            .await
            .expect("create rollback job")
    );
    let mut invalid_items = source_ids
        .iter()
        .zip(&item_ids)
        .map(|(source_id, item_id)| NewChapterDetectionJobItem {
            job_id: rollback_job_id,
            source_id,
            item_id,
            season_id: "chapter-season-batch",
            source_fingerprint,
            input_fingerprint: &input_fingerprint,
            is_context: false,
        })
        .collect::<Vec<_>>();
    invalid_items[ITEM_COUNT - 1].source_id = &source_ids[0];
    assert!(
        database
            .insert_chapter_detection_job_items(&invalid_items)
            .await
            .is_err()
    );
    let rolled_back_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM chapter_detection_job_items WHERE job_id = ?")
            .bind(rollback_job_id)
            .fetch_one(database.pool())
            .await
            .expect("rolled back item count");
    assert_eq!(rolled_back_count, 0);
    database.close().await;
}

#[tokio::test]
async fn media_probe_streams_are_replaced_in_bounded_batches() {
    const STREAM_COUNT: usize = 205;

    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Probe batch", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('probe-batch-item', ?, 'MOVIE', 'Probe', 'probe', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    sqlx::query(
        "INSERT INTO media_sources (id, item_id, source_kind, container, probe_status)
         VALUES ('probe-batch-source', 'probe-batch-item', 'LOCAL_FILE', 'mp4', 'PENDING')",
    )
    .execute(database.pool())
    .await
    .expect("media source");
    sqlx::query(
        "INSERT INTO media_streams (id, media_source_id, stream_index, stream_type)
         VALUES ('probe-stale-stream', 'probe-batch-source', 999, 'AUDIO')",
    )
    .execute(database.pool())
    .await
    .expect("stale stream");

    let new_stream = |index: usize, stream_index: i64| {
        let stream_type = match index % 3 {
            0 => "VIDEO",
            1 => "AUDIO",
            _ => "SUBTITLE",
        };
        let is_external = stream_type == "SUBTITLE";
        MediaStreamUpdate {
            stream_index,
            stream_type,
            codec: Some("test-codec"),
            language: Some("eng"),
            title: Some("Test stream"),
            details_json: Some(r#"{"generated":true}"#),
            external_path: is_external.then_some("subs/eng.srt"),
            is_external,
            is_default: index == 0,
            is_forced: index == 5,
        }
    };
    let streams = (0..STREAM_COUNT)
        .map(|index| new_stream(index, index as i64))
        .collect::<Vec<_>>();
    database.reset_query_count();
    database
        .save_media_probe(MediaProbeUpdate {
            source_id: "probe-batch-source",
            container: Some("mkv"),
            source_size: Some(42),
            duration_ticks: Some(456),
            bitrate: Some(789),
            streams: &streams,
            chapters: &[],
        })
        .await
        .expect("save probe result");
    assert_eq!(database.query_count(), 6);

    let stored_indices: Vec<i64> = sqlx::query_scalar(
        "SELECT stream_index FROM media_streams
         WHERE media_source_id = 'probe-batch-source' ORDER BY stream_index",
    )
    .fetch_all(database.pool())
    .await
    .expect("stored stream indexes");
    assert_eq!(stored_indices, (0..STREAM_COUNT as i64).collect::<Vec<_>>());
    #[derive(sqlx::FromRow)]
    struct StoredMediaStream {
        stream_type: String,
        codec: Option<String>,
        language: Option<String>,
        title: Option<String>,
        details_json: Option<String>,
        is_external: i64,
        is_default: i64,
        is_forced: i64,
    }
    let stored_subtitle: StoredMediaStream = sqlx::query_as(
        "SELECT stream_type, codec, language, title, details_json, is_external, is_default, is_forced
         FROM media_streams
         WHERE media_source_id = 'probe-batch-source' AND stream_index = 2",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored subtitle stream");
    assert_eq!(stored_subtitle.stream_type, "SUBTITLE");
    assert_eq!(stored_subtitle.codec.as_deref(), Some("test-codec"));
    assert_eq!(stored_subtitle.language.as_deref(), Some("eng"));
    assert_eq!(stored_subtitle.title.as_deref(), Some("Test stream"));
    assert_eq!(
        stored_subtitle.details_json.as_deref(),
        Some(r#"{"generated":true}"#)
    );
    assert_eq!(stored_subtitle.is_external, 1);
    assert_eq!(stored_subtitle.is_default, 0);
    assert_eq!(stored_subtitle.is_forced, 0);
    let stored_external_path: Option<String> = sqlx::query_scalar(
        "SELECT external_path FROM media_streams
         WHERE media_source_id = 'probe-batch-source' AND stream_index = 2",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored external subtitle path");
    assert_eq!(stored_external_path.as_deref(), Some("subs/eng.srt"));

    let mut invalid_streams = (0..STREAM_COUNT)
        .map(|index| new_stream(index, index as i64))
        .collect::<Vec<_>>();
    invalid_streams[STREAM_COUNT - 1].stream_index = 0;
    database.reset_query_count();
    assert!(
        database
            .save_media_probe(MediaProbeUpdate {
                source_id: "probe-batch-source",
                container: Some("invalid"),
                source_size: Some(100),
                duration_ticks: Some(200),
                bitrate: Some(300),
                streams: &invalid_streams,
                chapters: &[],
            })
            .await
            .is_err()
    );
    assert_eq!(database.query_count(), 5);
    let retained_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_streams WHERE media_source_id = 'probe-batch-source'",
    )
    .fetch_one(database.pool())
    .await
    .expect("retained stream count after rollback");
    assert_eq!(retained_count, STREAM_COUNT as i64);
    let retained_source: (
        Option<String>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        String,
    ) = sqlx::query_as(
        "SELECT container, size, duration_ticks, bitrate, probe_status
             FROM media_sources WHERE id = 'probe-batch-source'",
    )
    .fetch_one(database.pool())
    .await
    .expect("retained media source after rollback");
    assert_eq!(
        retained_source,
        (
            Some("mp4".to_owned()),
            Some(42),
            Some(456),
            Some(789),
            "READY".to_owned()
        )
    );

    database.reset_query_count();
    database
        .save_media_probe(MediaProbeUpdate {
            source_id: "probe-batch-source",
            container: None,
            source_size: None,
            duration_ticks: None,
            bitrate: None,
            streams: &[],
            chapters: &[],
        })
        .await
        .expect("save probe result with no streams");
    assert_eq!(database.query_count(), 3);
    let empty_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_streams WHERE media_source_id = 'probe-batch-source'",
    )
    .fetch_one(database.pool())
    .await
    .expect("empty stream count");
    assert_eq!(empty_count, 0);
    database.close().await;
}

#[tokio::test]
async fn scan_job_status_counts_are_aggregated_in_storage() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Scan jobs", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    for (id, status, job_type) in [
        ("scan-count-pending", "PENDING", "INCREMENTAL_SCAN"),
        ("scan-count-running", "RUNNING", "RECONCILE_LIBRARY"),
        ("scan-count-failed", "FAILED", "INCREMENTAL_SCAN"),
        ("scan-count-completed", "COMPLETED", "INCREMENTAL_SCAN"),
    ] {
        sqlx::query(
            "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
             VALUES (?, ?, ?, ?, 'generation')",
        )
        .bind(id)
        .bind(&library_id)
        .bind(job_type)
        .bind(status)
        .execute(database.pool())
        .await
        .expect("scan job");
    }

    assert_eq!(
        database
            .count_scan_jobs_by_status()
            .await
            .expect("status counts"),
        StoredScanJobCounts {
            running: 2,
            failed: 1,
        }
    );
}

#[tokio::test]
async fn scan_job_media_progress_is_loaded_from_job_queries() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Scan media progress", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO scan_jobs (
             id, library_id, job_type, status, generation, processed_media_count
         ) VALUES
             ('scan-media-progress-running', ?, 'RECONCILE_LIBRARY', 'RUNNING', 'g1', 7),
             ('scan-media-progress-legacy', ?, 'RECONCILE_LIBRARY', 'COMPLETED', 'g2', NULL)",
    )
    .bind(&library_id)
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("scan jobs");

    let found = database
        .find_scan_job("scan-media-progress-running")
        .await
        .expect("find scan job")
        .expect("running scan job");
    assert_eq!(found.processed_media_count, Some(7));
    let legacy = database
        .find_scan_job("scan-media-progress-legacy")
        .await
        .expect("find legacy scan job")
        .expect("legacy scan job");
    assert_eq!(legacy.processed_media_count, None);

    let listed = database
        .list_scan_jobs(None, 0, 10)
        .await
        .expect("list scan jobs");
    assert_eq!(
        listed
            .iter()
            .find(|job| job.id == "scan-media-progress-running")
            .and_then(|job| job.processed_media_count),
        Some(7)
    );
    let active = database
        .list_scan_jobs_for_activity(10)
        .await
        .expect("list active scan jobs");
    assert_eq!(
        active
            .iter()
            .find(|job| job.id == "scan-media-progress-running")
            .and_then(|job| job.processed_media_count),
        Some(7)
    );
}

#[tokio::test]
async fn filesystem_entry_scan_updates_include_inode_in_the_state_write() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Filesystem entry updates", LibraryKind::Movie, false)
        .await
        .expect("library");
    let media_root = temp_dir.path().join("filesystem-entry-updates");
    tokio::fs::create_dir_all(&media_root)
        .await
        .expect("media root");
    let root = LibraryService::new(database.clone())
        .add_root(library.id, media_root.to_str().expect("UTF-8 media root"))
        .await
        .expect("library root")
        .root;
    let root_id = root.id.to_string();
    database
        .insert_filesystem_entry(NewFilesystemEntry {
            id: "filesystem-entry-update",
            library_root_id: &root_id,
            relative_path: "movie.mkv",
            entry_kind: "FILE",
            size: 10,
            modified_at: 20,
            device: None,
            inode: Some(30),
            fingerprint: b"old-fingerprint",
            last_seen_generation: "old-generation",
        })
        .await
        .expect("filesystem entry");
    sqlx::query(
        "UPDATE filesystem_entries SET is_missing = 1 WHERE id = 'filesystem-entry-update'",
    )
    .execute(database.pool())
    .await
    .expect("mark filesystem entry missing");

    database.reset_query_count();
    database
        .update_filesystem_entry(FilesystemEntryUpdate {
            id: "filesystem-entry-update",
            size: 40,
            modified_at: 50,
            fingerprint: b"new-fingerprint",
            device: None,
            inode: Some(60),
            last_seen_generation: "new-generation",
        })
        .await
        .expect("filesystem entry update");
    assert_eq!(database.query_count(), 2);

    let updated: (i64, i64, Option<i64>, Vec<u8>, String, i64) = sqlx::query_as(
        "SELECT size, modified_at, inode, fingerprint, last_seen_generation, is_missing
         FROM filesystem_entries WHERE id = 'filesystem-entry-update'",
    )
    .fetch_one(database.pool())
    .await
    .expect("updated filesystem entry");
    assert_eq!(
        updated,
        (
            40,
            50,
            Some(60),
            b"new-fingerprint".to_vec(),
            "new-generation".to_owned(),
            0,
        )
    );

    sqlx::query(
        "UPDATE filesystem_entries SET is_missing = 1 WHERE id = 'filesystem-entry-update'",
    )
    .execute(database.pool())
    .await
    .expect("mark filesystem entry missing again");
    database.reset_query_count();
    database
        .mark_filesystem_entry_seen("filesystem-entry-update", "seen-generation", None, Some(70))
        .await
        .expect("mark filesystem entry seen");
    assert_eq!(database.query_count(), 2);

    let seen: (Option<i64>, String, i64) = sqlx::query_as(
        "SELECT inode, last_seen_generation, is_missing
         FROM filesystem_entries WHERE id = 'filesystem-entry-update'",
    )
    .fetch_one(database.pool())
    .await
    .expect("seen filesystem entry");
    assert_eq!(seen, (Some(70), "seen-generation".to_owned(), 0));
}

#[cfg(unix)]
#[tokio::test]
async fn filesystem_entry_inode_match_requires_the_same_device()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Device scoped inode lookup", LibraryKind::Movie, false)
        .await?;
    let first_root_path = temp_dir.path().join("first-root");
    let second_root_path = temp_dir.path().join("second-root");
    tokio::fs::create_dir_all(&first_root_path).await?;
    tokio::fs::create_dir_all(&second_root_path).await?;
    let first_root = libraries
        .add_root(library.id, first_root_path.to_str().ok_or("non-UTF8 root")?)
        .await?
        .root;
    let first_root_id = first_root.id.to_string();
    let second_root = libraries
        .add_root(
            library.id,
            second_root_path.to_str().ok_or("non-UTF8 root")?,
        )
        .await?
        .root;
    let second_root_id = second_root.id.to_string();
    database
        .insert_filesystem_entry(NewFilesystemEntry {
            id: "same-inode-on-another-device",
            library_root_id: &first_root_id,
            relative_path: "old-name.mkv",
            entry_kind: "FILE",
            size: 10,
            modified_at: 20,
            device: Some(1),
            inode: Some(42),
            fingerprint: b"fingerprint",
            last_seen_generation: "generation",
        })
        .await?;

    let entry = database
        .find_filesystem_entry_by_inode(
            &library.id.to_string(),
            &second_root_id,
            2,
            42,
            "new-name.mkv",
        )
        .await?;

    assert!(entry.is_none());
    database.close().await;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn scan_fingerprint_migration_replaces_legacy_token_and_persists_cursor()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::MetadataExt;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Fingerprint migration", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&media_root).await?;
    tokio::fs::write(
        media_root.join("movie.mkv"),
        b"metadata-only fingerprint fixture",
    )
    .await?;
    let root = libraries
        .add_root(library.id, media_root.to_str().ok_or("non-UTF8 root")?)
        .await?
        .root;
    let root_id = root.id.to_string();
    let metadata = std::fs::metadata(media_root.join("movie.mkv"))?;
    let size = i64::try_from(metadata.len())?;
    let modified_at = i64::try_from(
        metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
    )?;
    let device = i64::try_from(metadata.dev())?;
    let inode = i64::try_from(metadata.ino())?;
    let legacy = crate::application::scanner::compute_file_fingerprint(
        "movie.mkv",
        size,
        modified_at,
        Some(device as u64),
        Some(inode as u64),
    );
    database
        .insert_filesystem_entry(NewFilesystemEntry {
            id: "migration-entry",
            library_root_id: &root_id,
            relative_path: "movie.mkv",
            entry_kind: "FILE",
            size,
            modified_at,
            device: Some(device),
            inode: Some(inode),
            fingerprint: &legacy,
            last_seen_generation: "migration-generation",
        })
        .await?;

    let library_id = library.id.to_string();
    let cursor = database
        .ensure_scan_fingerprint_migration_state(&library_id)
        .await?;
    let candidate = database
        .find_scan_fingerprint_migration_candidate(&library_id, &cursor)
        .await?
        .ok_or("legacy candidate not found")?;
    let lease_token = "migration-lease";
    assert!(
        database
            .claim_scan_fingerprint_migration_lease(&library_id, lease_token)
            .await?
    );
    let fingerprint_v2 = crate::application::scanner::compute_scan_fingerprint_v2(
        "movie.mkv",
        size,
        modified_at,
        Some(device as u64),
        Some(inode as u64),
    );
    assert!(
        database
            .migrate_scan_fingerprint_candidate(ScanFingerprintMigrationCommit {
                candidate: &candidate,
                fingerprint_v2: &fingerprint_v2,
                size,
                modified_at,
                device,
                inode,
                lease_token,
            },)
            .await?
    );

    let stored: (Vec<u8>, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT fingerprint, device, inode FROM filesystem_entries WHERE id = 'migration-entry'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored.0, fingerprint_v2);
    assert_ne!(stored.0, legacy);
    assert_eq!((stored.1, stored.2), (Some(device), Some(inode)));
    let persisted_cursor = database
        .ensure_scan_fingerprint_migration_state(&library_id)
        .await?;
    assert_eq!(persisted_cursor.root_id.as_deref(), Some(root_id.as_str()));
    assert_eq!(persisted_cursor.relative_path.as_deref(), Some("movie.mkv"));
    assert!(
        database
            .find_scan_fingerprint_migration_candidate(&library_id, &persisted_cursor)
            .await?
            .is_none()
    );

    database
        .insert_filesystem_entry(NewFilesystemEntry {
            id: "migration-missing-entry",
            library_root_id: &root_id,
            relative_path: "gone.mkv",
            entry_kind: "FILE",
            size,
            modified_at,
            device: Some(device),
            inode: Some(inode),
            fingerprint: &legacy,
            last_seen_generation: "migration-generation",
        })
        .await?;
    sqlx::query("DROP TRIGGER trg_filesystem_entries_clear_missing_fingerprint_update")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "UPDATE filesystem_entries SET is_missing = 1
         WHERE id = 'migration-missing-entry'",
    )
    .execute(database.pool())
    .await?;
    assert!(
        database
            .scan_fingerprint_migration_has_pending_entries(&library_id)
            .await?
    );
    assert!(
        database
            .clear_one_missing_scan_fingerprint(&library_id)
            .await?
    );
    let missing_fingerprint: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT fingerprint FROM filesystem_entries
         WHERE id = 'migration-missing-entry'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(missing_fingerprint, None);
    assert!(
        !database
            .scan_fingerprint_migration_has_pending_entries(&library_id)
            .await?
    );
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn incremental_scan_paths_are_enqueued_with_bounded_sql_and_last_change_wins() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Incremental queue", LibraryKind::Movie, false)
        .await
        .expect("library");
    let media_root = temp_dir.path().join("incremental-queue");
    tokio::fs::create_dir_all(&media_root)
        .await
        .expect("media root");
    let root = LibraryService::new(database.clone())
        .add_root(library.id, media_root.to_str().expect("UTF-8 media root"))
        .await
        .expect("library root")
        .root;
    let library_id = library.id.to_string();
    let root_id = root.id.to_string();
    database
        .create_scan_job(
            "incremental-queue-job",
            &library_id,
            "INCREMENTAL_SCAN",
            "generation",
            0,
            false,
        )
        .await
        .expect("scan job");

    database.reset_query_count();
    database
        .enqueue_incremental_scan_path(
            "incremental-queue-job",
            &root_id,
            "folder/file-000.mkv",
            "CREATE",
        )
        .await
        .expect("initial incremental path");
    assert_eq!(database.query_count(), 2);
    sqlx::query(
        "UPDATE scan_job_paths SET processed_at = 1
         WHERE job_id = 'incremental-queue-job' AND relative_path = 'folder/file-000.mkv'",
    )
    .execute(database.pool())
    .await
    .expect("mark initial path processed");

    let mut paths = (0..205)
        .map(|index| {
            (
                root_id.clone(),
                format!("folder/file-{index:03}.mkv"),
                "CREATE".to_owned(),
            )
        })
        .collect::<Vec<_>>();
    paths.push((
        root_id.clone(),
        "folder/file-000.mkv".to_owned(),
        "MODIFY".to_owned(),
    ));
    let paths = paths
        .iter()
        .map(|(root_id, relative_path, change_kind)| {
            (
                root_id.as_str(),
                relative_path.as_str(),
                change_kind.as_str(),
            )
        })
        .collect::<Vec<_>>();

    database.reset_query_count();
    database
        .enqueue_incremental_scan_paths("incremental-queue-job", &paths)
        .await
        .expect("incremental paths");

    assert_eq!(database.query_count(), 6);
    let stored: (i64, i64, String, Option<i64>) = sqlx::query_as(
        "SELECT job.total_count,
                (SELECT COUNT(*) FROM scan_job_paths WHERE job_id = job.id),
                path.change_kind, path.processed_at
         FROM scan_jobs job
         JOIN scan_job_paths path ON path.job_id = job.id
         WHERE job.id = 'incremental-queue-job'
           AND path.relative_path = 'folder/file-000.mkv'",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored incremental paths");
    assert_eq!(stored, (205, 205, "MODIFY".to_owned(), None));
}

#[tokio::test]
async fn reconciliation_entries_use_scan_safe_batches() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Scan batches", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("media root path"))
        .await
        .expect("library root")
        .root;
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await
        .expect("scan job");
    sqlx::query("UPDATE scan_jobs SET status = 'RUNNING' WHERE id = ?")
        .bind(&job.id)
        .execute(database.pool())
        .await
        .expect("start scan job");
    database
        .clear_reconciliation_scan_entries(&job.id)
        .await
        .expect("clear root entry");

    let paths = (0..1_025)
        .map(|index| format!("Movie {index:04}.mkv"))
        .collect::<Vec<_>>();
    database.reset_query_count();
    database
        .commit_reconciliation_discovery_chunk(&job.id, &root.id.to_string(), &[], &paths, None)
        .await
        .expect("commit reconciliation entries");

    database.reset_query_count();
    database
        .commit_reconciliation_discovery_chunk(&job.id, &root.id.to_string(), &[], &paths, None)
        .await
        .expect("re-commit reconciliation entries");

    assert_eq!(database.query_count(), 6);
    let stored: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = ? AND entry_type = 'FILE'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await
    .expect("stored reconciliation entries");
    assert_eq!(stored, 1_025);
    let total_count: i64 = sqlx::query_scalar("SELECT total_count FROM scan_jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await
        .expect("scan total count");
    assert_eq!(total_count, 1_025);
}

#[tokio::test]
async fn reconciliation_discovery_chunk_commits_entries_progress_and_completion() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Discovery commit", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("media root path"))
        .await
        .expect("library root")
        .root;
    let root_id = root.id.to_string();
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await
        .expect("scan job");
    sqlx::query("UPDATE scan_jobs SET status = 'RUNNING' WHERE id = ?")
        .bind(&job.id)
        .execute(database.pool())
        .await
        .expect("start scan job");
    database
        .clear_reconciliation_scan_entries(&job.id)
        .await
        .expect("clear initial directory");
    sqlx::query(
        "INSERT INTO reconciliation_scan_entries (
             job_id, library_root_id, relative_path, entry_type, status
         ) VALUES (?, ?, '', 'DIRECTORY', 'PENDING')",
    )
    .bind(&job.id)
    .bind(&root_id)
    .execute(database.pool())
    .await
    .expect("root directory entry");

    database
        .commit_reconciliation_discovery_chunk(
            &job.id,
            &root_id,
            &["Movie".to_owned()],
            &[
                "Movie/First.Movie.2024.mkv".to_owned(),
                "Movie/Second.Movie.2024.mkv".to_owned(),
                "Movie/First.Movie.2024.mkv".to_owned(),
            ],
            Some(""),
        )
        .await
        .expect("discovery chunk");

    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM reconciliation_scan_entries
              WHERE job_id = ? AND entry_type = 'DIRECTORY'),
             (SELECT COUNT(*) FROM reconciliation_scan_entries
              WHERE job_id = ? AND entry_type = 'FILE'),
             (SELECT total_count FROM scan_jobs WHERE id = ?)",
    )
    .bind(&job.id)
    .bind(&job.id)
    .bind(&job.id)
    .fetch_one(database.pool())
    .await
    .expect("discovery state");
    assert_eq!(counts, (1, 2, 2));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM reconciliation_scan_entries
             WHERE job_id = ? AND relative_path = '' AND entry_type = 'DIRECTORY'",
        )
        .bind(&job.id)
        .fetch_one(database.pool())
        .await
        .expect("completed directory count"),
        0
    );
}

#[tokio::test]
async fn marking_an_unchanged_scan_stage_does_not_touch_the_row() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("Scan stage", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('unchanged-stage-job', ?, 'INCREMENTAL_SCAN', 'COMPLETED', 'generation')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("scan job");
    sqlx::query(
        "INSERT INTO scan_job_targets (
             job_id, target_type, target_id, item_id, change_kind,
             metadata_state, updated_at
         ) VALUES ('unchanged-stage-job', 'ITEM', 'unchanged-target', 'item', 'NEW',
                   'DONE', 1)",
    )
    .execute(database.pool())
    .await
    .expect("scan target");

    database
        .mark_scan_job_target_stage(
            "unchanged-stage-job",
            "ITEM",
            &["unchanged-target".to_owned()],
            "METADATA",
            "DONE",
        )
        .await
        .expect("mark unchanged stage");
    let updated_at: i64 = sqlx::query_scalar(
        "SELECT updated_at FROM scan_job_targets WHERE target_id = 'unchanged-target'",
    )
    .fetch_one(database.pool())
    .await
    .expect("unchanged target timestamp");
    assert_eq!(updated_at, 1);
}

#[tokio::test]
async fn chapter_detection_outcomes_commit_status_state_and_progress_as_one_batch() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Chapter jobs", LibraryKind::Series, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 root"))
        .await
        .expect("library root");
    let root_id: String = sqlx::query_scalar("SELECT id FROM library_roots LIMIT 1")
        .fetch_one(database.pool())
        .await
        .expect("root");
    let item_id = "chapter-item";
    let source_id = "chapter-source";
    let entry_id = "chapter-entry";
    sqlx::query(
        "INSERT INTO media_items (id, library_id, item_type, title, sort_title, identification_status)
         VALUES (?, ?, 'EPISODE', 'Episode', 'episode', 'LOCAL_CONFIRMED')",
    )
    .bind(item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    sqlx::query(
        "INSERT INTO filesystem_entries
         (id, library_root_id, relative_path, entry_kind, size, modified_at, last_seen_generation)
         VALUES (?, ?, 'episode.mkv', 'FILE', 1, 1, 'generation')",
    )
    .bind(entry_id)
    .bind(&root_id)
    .execute(database.pool())
    .await
    .expect("filesystem entry");
    sqlx::query(
        "INSERT INTO media_sources (id, item_id, source_kind, filesystem_entry_id, duration_ticks)
         VALUES (?, ?, 'LOCAL_FILE', ?, 10000000)",
    )
    .bind(source_id)
    .bind(item_id)
    .bind(entry_id)
    .execute(database.pool())
    .await
    .expect("media source");
    database
        .create_chapter_detection_job(NewChapterDetectionJob {
            id: "chapter-job-batch",
            library_id: &library.id.to_string(),
            plugin_id: "builtin",
            concurrency: 1,
            intro_window_seconds: 15,
            credits_window_seconds: 15,
            match_threshold: 0.8,
            total_count: 1,
        })
        .await
        .expect("chapter job");
    database
        .claim_chapter_detection_job("chapter-job-batch")
        .await
        .expect("claim chapter job");
    sqlx::query(
        "INSERT INTO chapter_detection_job_items
         (job_id, source_id, item_id, season_id, source_fingerprint, input_fingerprint, is_context, status)
         VALUES (?, ?, ?, ?, ?, ?, 0, 'PENDING')",
    )
    .bind("chapter-job-batch")
    .bind(source_id)
    .bind(item_id)
    .bind(item_id)
    .bind(vec![1_u8])
    .bind(vec![2_u8])
    .execute(database.pool())
    .await
    .expect("chapter item");

    database.reset_query_count();
    database
        .apply_chapter_detection_outcomes(
            "chapter-job-batch",
            "builtin",
            &[ChapterDetectionOutcomeUpdate {
                source_id: source_id.to_owned(),
                status: "COMPLETED".to_owned(),
                error: None,
                source_state: Some(ChapterDetectionSourceStateUpdate {
                    input_fingerprint: vec![2],
                    status: "NOT_FOUND".to_owned(),
                    last_checked_at: 10,
                    last_success_at: None,
                    next_retry_at: Some(20),
                    error: None,
                    intro_fingerprint: None,
                    credits_fingerprint: None,
                }),
            }],
            Some(source_id),
            1,
        )
        .await
        .expect("apply chapter batch");
    assert_eq!(database.query_count(), 3);
    let item_status: String = sqlx::query_scalar(
        "SELECT status FROM chapter_detection_job_items WHERE job_id = ? AND source_id = ?",
    )
    .bind("chapter-job-batch")
    .bind(source_id)
    .fetch_one(database.pool())
    .await
    .expect("item status");
    let source_status: String = sqlx::query_scalar(
        "SELECT status FROM chapter_detection_source_states WHERE source_id = ? AND plugin_id = ?",
    )
    .bind(source_id)
    .bind("builtin")
    .fetch_one(database.pool())
    .await
    .expect("source state");
    let progress: (i64, String) =
        sqlx::query_as("SELECT processed_count, cursor FROM chapter_detection_jobs WHERE id = ?")
            .bind("chapter-job-batch")
            .fetch_one(database.pool())
            .await
            .expect("job progress");
    assert_eq!(item_status, "COMPLETED");
    assert_eq!(source_status, "NOT_FOUND");
    assert_eq!(progress, (1, source_id.to_owned()));
}

#[tokio::test]
async fn detected_media_chapters_are_inserted_in_one_bounded_batch()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Chapter markers", LibraryKind::Series, false)
        .await?;
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path).await?;
    libraries
        .add_root(library.id, root_path.to_str().ok_or("non-utf8 root")?)
        .await?;
    let root_id: String = sqlx::query_scalar("SELECT id FROM library_roots LIMIT 1")
        .fetch_one(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO media_items (id, library_id, item_type, title, sort_title, identification_status)
         VALUES ('marker-item', ?, 'EPISODE', 'Episode', 'episode', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO filesystem_entries
         (id, library_root_id, relative_path, entry_kind, size, modified_at, fingerprint, last_seen_generation)
         VALUES ('marker-entry', ?, 'episode.mkv', 'FILE', 1, 1, ?, 'generation')",
    )
    .bind(&root_id)
    .bind(vec![1_u8])
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_sources (id, item_id, source_kind, filesystem_entry_id)
         VALUES ('marker-source', 'marker-item', 'LOCAL_FILE', 'marker-entry')",
    )
    .execute(database.pool())
    .await?;

    let markers = (0_usize..3)
        .map(|index| NewMediaChapterMarker {
            start_position_ticks: ((index + 1) * 10_000_000) as i64,
            name: None,
            marker_type: ["INTRO_START", "INTRO_END", "CREDITS_START"][index].to_owned(),
            chapter_index: index as i64,
            confidence: 0.9,
        })
        .collect::<Vec<_>>();
    database.reset_query_count();
    assert!(
        database
            .replace_detected_media_chapters("marker-source", "detector", &[1], &markers)
            .await?
    );
    assert_eq!(database.query_count(), 3);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM media_chapters
             WHERE media_source_id = 'marker-source' AND provider_id = 'detector'",
        )
        .fetch_one(database.pool())
        .await?,
        3
    );
    Ok(())
}

#[tokio::test]
async fn person_index_rebuild_tasks_are_token_guarded_and_requeueable() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("People", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();

    let jobs = database
        .sync_person_index_rebuild_jobs(1)
        .await
        .expect("sync jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, "QUEUED");
    assert!(
        database
            .claim_person_index_rebuild_job(&library_id, "run-a")
            .await
            .expect("claim first run")
    );
    assert!(
        database
            .update_person_index_rebuild_progress(&library_id, "run-a", "item-a", 1, 2)
            .await
            .expect("record first run progress")
            .is_some()
    );
    assert!(
        database
            .request_person_index_rebuild_job_cancel(&library_id)
            .await
            .expect("request first run cancellation")
    );
    let active_jobs = database
        .sync_person_index_rebuild_jobs(1)
        .await
        .expect("keep a current run active");
    assert_eq!(active_jobs[0].status, "RUNNING");
    let active_job: (Option<String>, i64, i64, i64, Option<String>) = sqlx::query_as(
        "SELECT cursor_id, processed_count, total_count, cancel_requested, run_token
         FROM person_index_rebuild_jobs WHERE library_id = ?",
    )
    .bind(&library_id)
    .fetch_one(database.pool())
    .await
    .expect("current run fields");
    assert_eq!(
        active_job,
        (Some("item-a".to_owned()), 1, 2, 1, Some("run-a".to_owned()))
    );
    sqlx::query(
        "UPDATE person_index_rebuild_jobs
             SET updated_at = unixepoch() - 61
             WHERE library_id = ?",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("mark interrupted run stale");
    let recovered_jobs = database
        .sync_person_index_rebuild_jobs(1)
        .await
        .expect("recover stale run");
    assert_eq!(recovered_jobs[0].status, "QUEUED");
    let recovered_token: Option<String> =
        sqlx::query_scalar("SELECT run_token FROM person_index_rebuild_jobs WHERE library_id = ?")
            .bind(&library_id)
            .fetch_one(database.pool())
            .await
            .expect("read recovered token");
    assert_eq!(recovered_token, None);
    assert!(
        database
            .claim_person_index_rebuild_job(&library_id, "run-b")
            .await
            .expect("claim recovered run")
    );
    assert!(
        database
            .request_person_index_rebuild_job_cancel(&library_id)
            .await
            .expect("request cancellation")
    );
    assert!(
        database
            .request_person_index_rebuild_job(&library_id, 1)
            .await
            .expect("requeue job")
    );
    assert!(
        !database
            .finish_person_index_rebuild_job(&library_id, "run-a", "COMPLETED", None)
            .await
            .expect("ignore stale completion")
    );
    assert!(
        !database
            .finish_person_index_rebuild_job(&library_id, "run-b", "COMPLETED", None)
            .await
            .expect("ignore cancelled run completion")
    );
    assert!(
        database
            .claim_person_index_rebuild_job(&library_id, "run-c")
            .await
            .expect("claim requeued run")
    );
    assert!(
        database
            .update_person_index_rebuild_progress(&library_id, "run-a", "item-a", 1, 2)
            .await
            .expect("ignore stale progress")
            .is_none()
    );
    assert!(
        database
            .update_person_index_rebuild_progress(&library_id, "run-c", "item-c", 2, 2)
            .await
            .expect("update progress")
            .is_some()
    );
    assert!(
        database
            .finish_person_index_rebuild_job(&library_id, "run-c", "COMPLETED", None)
            .await
            .expect("finish current run")
    );
    let jobs = database
        .list_person_index_rebuild_jobs(0, 20)
        .await
        .expect("list jobs");
    assert_eq!(jobs[0].status, "COMPLETED");
    assert_eq!(jobs[0].processed_count, 2);
}

#[tokio::test]
async fn syncing_person_rebuild_jobs_batches_libraries_and_skips_noop_updates() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let mut enabled_library_ids = Vec::new();
    for index in 0..4 {
        let library = libraries
            .create_library(&format!("People {index}"), LibraryKind::Movie, false)
            .await
            .expect("enabled library");
        enabled_library_ids.push(library.id.to_string());
    }
    let disabled_library = libraries
        .create_library("Disabled People", LibraryKind::Movie, false)
        .await
        .expect("disabled library");
    let disabled_library_id = disabled_library.id.to_string();
    sqlx::query("UPDATE libraries SET is_enabled = 0 WHERE id = ?")
        .bind(&disabled_library_id)
        .execute(database.pool())
        .await
        .expect("disable library");

    database.reset_query_count();
    let jobs = database
        .sync_person_index_rebuild_jobs(1)
        .await
        .expect("create jobs for enabled libraries");
    assert_eq!(database.query_count(), 2);
    assert_eq!(jobs.len(), enabled_library_ids.len());
    let disabled_job_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM person_index_rebuild_jobs WHERE library_id = ?")
            .bind(&disabled_library_id)
            .fetch_one(database.pool())
            .await
            .expect("disabled library job count");
    assert_eq!(disabled_job_count, 0);

    sqlx::query("CREATE TABLE person_index_rebuild_update_probe (count INTEGER NOT NULL)")
        .execute(database.pool())
        .await
        .expect("create update probe");
    sqlx::query("INSERT INTO person_index_rebuild_update_probe (count) VALUES (0)")
        .execute(database.pool())
        .await
        .expect("initialize update probe");
    sqlx::query(
        "CREATE TRIGGER person_index_rebuild_update_probe_trigger
         AFTER UPDATE ON person_index_rebuild_jobs
         BEGIN
             UPDATE person_index_rebuild_update_probe SET count = count + 1;
         END",
    )
    .execute(database.pool())
    .await
    .expect("create update trigger");

    database.reset_query_count();
    database
        .sync_person_index_rebuild_jobs(1)
        .await
        .expect("sync unchanged jobs");
    assert_eq!(database.query_count(), 2);
    let unchanged_update_count: i64 =
        sqlx::query_scalar("SELECT count FROM person_index_rebuild_update_probe")
            .fetch_one(database.pool())
            .await
            .expect("unchanged update count");
    assert_eq!(unchanged_update_count, 0);

    sqlx::query(
        "UPDATE person_index_rebuild_jobs
         SET status = 'RUNNING', cursor_id = 'checkpoint', processed_count = 2,
             total_count = 4, cancel_requested = 1, run_token = 'run-token', error = 'old'
         WHERE library_id = ?",
    )
    .bind(&enabled_library_ids[0])
    .execute(database.pool())
    .await
    .expect("prepare old-schema job");
    sqlx::query("UPDATE person_index_rebuild_update_probe SET count = 0")
        .execute(database.pool())
        .await
        .expect("reset update probe");

    database.reset_query_count();
    database
        .sync_person_index_rebuild_jobs(2)
        .await
        .expect("reset jobs for new schema");
    assert_eq!(database.query_count(), 2);
    type PersonIndexRebuildSchemaResetRow = (
        i64,
        String,
        Option<String>,
        i64,
        i64,
        i64,
        Option<String>,
        Option<String>,
    );
    let schema_changed: PersonIndexRebuildSchemaResetRow = sqlx::query_as(
        "SELECT schema_version, status, cursor_id, processed_count, total_count,
                    cancel_requested, run_token, error
             FROM person_index_rebuild_jobs WHERE library_id = ?",
    )
    .bind(&enabled_library_ids[0])
    .fetch_one(database.pool())
    .await
    .expect("schema-changed job");
    assert_eq!(
        schema_changed,
        (2, "QUEUED".to_owned(), None, 0, 0, 0, None, None)
    );
    let schema_change_update_count: i64 =
        sqlx::query_scalar("SELECT count FROM person_index_rebuild_update_probe")
            .fetch_one(database.pool())
            .await
            .expect("schema change update count");
    assert_eq!(schema_change_update_count, enabled_library_ids.len() as i64);
}

#[tokio::test]
async fn person_index_keyset_pages_and_fingerprints_are_conservative() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("People", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    for item_id in ["item-a", "item-b", "item-c"] {
        sqlx::query(
            "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }
    let first_page = database
        .list_person_index_item_ids(&library_id, None, 2)
        .await
        .expect("first keyset page");
    assert_eq!(first_page, ["item-a", "item-b"]);
    let second_page = database
        .list_person_index_item_ids(&library_id, first_page.last().map(String::as_str), 2)
        .await
        .expect("second keyset page");
    assert_eq!(second_page, ["item-c"]);
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-ab', ?, 'MOVIE', 'item-ab', 'item-ab', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("insert item before the cursor");
    let second_page_after_insert = database
        .list_person_index_item_ids(&library_id, first_page.last().map(String::as_str), 2)
        .await
        .expect("second keyset page after insert");
    assert_eq!(second_page_after_insert, ["item-c"]);
    sqlx::query("DELETE FROM media_items WHERE id = 'item-c'")
        .execute(database.pool())
        .await
        .expect("delete item after the cursor");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-z', ?, 'MOVIE', 'item-z', 'item-z', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("insert item after the cursor");
    let second_page_after_delete = database
        .list_person_index_item_ids(&library_id, first_page.last().map(String::as_str), 2)
        .await
        .expect("second keyset page after delete");
    assert_eq!(second_page_after_delete, ["item-z"]);

    database
        .replace_person_credits_with_relation_checksum(
            "item-a",
            &[],
            Some("fingerprint-a"),
            Some("relation-a"),
        )
        .await
        .expect("store fingerprint");
    assert!(
        database
            .person_index_item_state_matches_snapshot(
                "item-a",
                Some("fingerprint-a"),
                Some("relation-a"),
            )
            .await
            .expect("matching fingerprint and relation checksum")
    );
    assert!(
        !database
            .person_index_item_state_matches_snapshot("item-a", None, Some("relation-a"))
            .await
            .expect("missing fingerprint must not be current")
    );
    assert!(
        !database
            .person_index_item_state_matches_snapshot(
                "item-a",
                Some("fingerprint-b"),
                Some("relation-a"),
            )
            .await
            .expect("changed fingerprint")
    );
    assert!(
        !database
            .person_index_item_state_matches_snapshot(
                "item-a",
                Some("fingerprint-a"),
                Some("relation-b"),
            )
            .await
            .expect("changed relation checksum")
    );
    sqlx::query(
        "UPDATE person_index_item_state
             SET relation_schema_version = 3
             WHERE item_id = 'item-a'",
    )
    .execute(database.pool())
    .await
    .expect("change relation schema version");
    assert!(
        !database
            .person_index_item_state_matches_snapshot(
                "item-a",
                Some("fingerprint-a"),
                Some("relation-a"),
            )
            .await
            .expect("changed relation schema version")
    );
    database
        .clear_person_credits("item-a")
        .await
        .expect("clear person credits");
    assert!(
        !database
            .person_index_item_state_matches_snapshot(
                "item-a",
                Some("fingerprint-a"),
                Some("relation-a"),
            )
            .await
            .expect("cleared relation must be rebuilt")
    );
}

#[tokio::test]
async fn person_credit_writes_persist_relation_checksums_and_legacy_writes_clear_them() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("People checksum", LibraryKind::Movie, false)
        .await
        .expect("library");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-checksum', ?, 'MOVIE', 'Checksum', 'checksum', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-checksum-batch', ?, 'MOVIE', 'Checksum batch', 'checksum batch', 'LOCAL_CONFIRMED')",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("batch media item");

    database
        .replace_person_credits_with_relation_checksum(
            "item-checksum",
            &[],
            Some("source-v1"),
            Some("relation-v1"),
        )
        .await
        .expect("store relation snapshot");
    let stored_single_checksum: Option<String> = sqlx::query_scalar(
        "SELECT relation_checksum FROM person_index_item_state WHERE item_id = 'item-checksum'",
    )
    .fetch_one(database.pool())
    .await
    .expect("read single-item relation checksum");
    assert_eq!(stored_single_checksum.as_deref(), Some("relation-v1"));

    let no_credits = [];
    database
        .replace_person_credits_batch_with_relation_checksum(&[
            (
                "item-checksum",
                &no_credits,
                Some("source-v1"),
                Some("relation-v2"),
            ),
            (
                "item-checksum-batch",
                &no_credits,
                Some("source-batch"),
                Some("relation-batch"),
            ),
        ])
        .await
        .expect("store relation checksums in a batch");
    let stored_batch_checksum: Option<String> = sqlx::query_scalar(
        "SELECT relation_checksum FROM person_index_item_state
         WHERE item_id = 'item-checksum-batch'",
    )
    .fetch_one(database.pool())
    .await
    .expect("read batched relation checksum");
    assert_eq!(stored_batch_checksum.as_deref(), Some("relation-batch"));

    assert!(
        database
            .person_index_item_state_matches_snapshot(
                "item-checksum",
                Some("source-v1"),
                Some("relation-v2")
            )
            .await
            .expect("matching source and relation checksums")
    );

    database
        .replace_person_credits_with_fingerprint("item-checksum", &[], Some("source-v1"))
        .await
        .expect("legacy writer stores no relation checksum");
    assert!(
        !database
            .person_index_item_state_matches_snapshot("item-checksum", Some("source-v1"), None)
            .await
            .expect("legacy checksum-free state must be stale")
    );
    let stored_checksum_after_legacy_write: Option<String> = sqlx::query_scalar(
        "SELECT relation_checksum FROM person_index_item_state WHERE item_id = 'item-checksum'",
    )
    .fetch_one(database.pool())
    .await
    .expect("read legacy checksum");
    assert_eq!(stored_checksum_after_legacy_write, None);
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_metadata_candidate_selection_accepts_integer_boolean_flags() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let connection = DatabaseConfiguration::Postgres(PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .unwrap_or_else(|_| "55432".to_owned())
            .parse()
            .expect("test port"),
        database: std::env::var("POSTGRES_TEST_DATABASE").unwrap_or_else(|_| "lux".to_owned()),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    });
    let database = Database::connect_with_configuration(&config, &connection)
        .await
        .expect("PostgreSQL database");
    let library = LibraryService::new(database.clone())
        .create_library("Metadata selection", LibraryKind::Movie, false)
        .await
        .expect("library");
    let item_id = Uuid::now_v7().to_string();
    let candidate_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status,
                has_available_source
             ) VALUES (?, ?, 'MOVIE', 'Metadata selection', 'metadata selection',
                       'LOCAL_CONFIRMED', 1)",
    )
    .bind(&item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    sqlx::query(
        "INSERT INTO metadata_candidates (
                id, item_id, provider, provider_id, candidate_json, score, status
             ) VALUES (?, ?, 'TMDB', '603', '{}', 100, 'PENDING')",
    )
    .bind(&candidate_id)
    .bind(&item_id)
    .execute(database.pool())
    .await
    .expect("metadata candidate");

    let update = |keep_pending| SelectedMetadataUpdate {
        item_id: &item_id,
        candidate_id: &candidate_id,
        title: "Metadata selection",
        original_title: None,
        overview: None,
        production_year: None,
        premiere_date: None,
        last_air_date: None,
        status: None,
        original_language: None,
        rating: None,
        rating_source: None,
        provider_ids_json: "{}",
        metadata_scraper_id: None,
        metadata_fingerprint: &[],
        provenance_json: "{}",
        locked_fields_json: "[]",
        poster_fallback_required: false,
        keep_pending,
    };

    assert!(
        database
            .select_metadata_candidate(update(true))
            .await
            .expect("keep-pending selection")
    );
    let identification_status: String =
        sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("identification status");
    assert_eq!(identification_status, "PENDING");
    let candidate_status: String =
        sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
            .bind(&candidate_id)
            .fetch_one(database.pool())
            .await
            .expect("candidate status");
    assert_eq!(candidate_status, "PENDING");

    assert!(
        database
            .select_metadata_candidate(update(false))
            .await
            .expect("confirmed selection")
    );
    let identification_status: String =
        sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await
            .expect("confirmed identification status");
    assert_eq!(identification_status, "ONLINE_CONFIRMED");
    let candidate_status: String =
        sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
            .bind(&candidate_id)
            .fetch_one(database.pool())
            .await
            .expect("confirmed candidate status");
    assert_eq!(candidate_status, "SELECTED");
}

fn invitation_storage_bundle<'a>(
    id: &'a str,
    recipient_email: &'a str,
    challenge_id: &'a str,
    idempotency_key: &'a str,
    expires_at: i64,
    available_at: i64,
    token_hash: &'a [u8],
) -> (
    NewUserInvitation<'a>,
    NewMailChallenge<'a>,
    NewMailOutboxMessage<'a>,
) {
    (
        NewUserInvitation {
            id,
            recipient_email,
            created_by_user_id: None,
            preset_id: "viewer",
            preset_name: "Viewer",
            preset_version: 1,
            permission_snapshot_json: r#"{"isAdmin":false,"canManageServer":false,"permissions":{"libraryAccessMode":"NONE","libraryIds":[],"canDownload":false,"canPlay":true,"maxSimultaneousPlaybacks":2,"allowRemux":true,"allowAudioTranscode":true,"allowVideoTranscode":true}}"#,
            expires_at,
        },
        NewMailChallenge {
            id: challenge_id,
            kind: MailChallengeKind::Invitation,
            recipient_email,
            reference_id: id,
            token_hash,
            expires_at,
        },
        NewMailOutboxMessage {
            idempotency_key,
            recipient_email,
            template_kind: MailOutboxTemplateKind::Invitation,
            reference_id: challenge_id,
            available_at,
        },
    )
}

#[tokio::test]
async fn user_invitation_storage_creates_mail_atomically_and_rejects_duplicate_email()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let now = 1_000;
    let first_hash = [1_u8; 32];
    let (invitation, challenge, outbox) = invitation_storage_bundle(
        "invitation-first",
        "viewer@example.test",
        "challenge-first",
        "invite-mail-first",
        10_000,
        now,
        &first_hash,
    );
    assert_eq!(
        database
            .create_user_invitation_with_mail(invitation, challenge, outbox, now)
            .await?,
        UserInvitationCreateResult::Created
    );
    let created = database
        .get_user_invitation("invitation-first", now)
        .await?
        .ok_or("created invitation was not found")?;
    assert_eq!(created.status, UserInvitationStatus::Pending);
    assert_eq!(created.recipient_email, "viewer@example.test");
    let stored_challenge = database
        .get_mail_challenge("challenge-first")
        .await?
        .ok_or("invitation challenge was not stored")?;
    assert_eq!(stored_challenge.reference_id, created.id);
    let queued = database.list_mail_outbox_page(None, 100, 0).await?;
    assert_eq!(queued.messages.len(), 1);
    assert_eq!(queued.messages[0].reference_id, "challenge-first");

    let second_hash = [2_u8; 32];
    let (invitation, challenge, outbox) = invitation_storage_bundle(
        "invitation-duplicate",
        "viewer@example.test",
        "challenge-duplicate",
        "invite-mail-duplicate",
        10_000,
        now,
        &second_hash,
    );
    assert_eq!(
        database
            .create_user_invitation_with_mail(invitation, challenge, outbox, now)
            .await?,
        UserInvitationCreateResult::AlreadyPending
    );
    assert!(
        database
            .get_mail_challenge("challenge-duplicate")
            .await?
            .is_none()
    );
    assert_eq!(
        database
            .list_mail_outbox_page(None, 100, 0)
            .await?
            .messages
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn user_invitation_storage_requeues_valid_challenge_idempotently()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let token_hash = [16_u8; 32];
    let (invitation, challenge, outbox) = invitation_storage_bundle(
        "resend-active-invitation",
        "resend-active@example.test",
        "resend-active-challenge",
        "resend-active-initial-mail",
        10_000,
        1_000,
        &token_hash,
    );
    database
        .create_user_invitation_with_mail(invitation, challenge, outbox, 1_000)
        .await?;

    assert!(
        database
            .queue_user_invitation_resend("resend-active-invitation", "resend-mail-1", 1_001)
            .await?
    );
    assert!(
        database
            .queue_user_invitation_resend("resend-active-invitation", "resend-mail-1", 1_002)
            .await?
    );
    assert!(
        database
            .queue_user_invitation_resend("resend-active-invitation", "resend-mail-2", 1_003)
            .await?
    );
    assert!(
        database
            .queue_user_invitation_resend("resend-active-invitation", "", 1_004)
            .await
            .is_err()
    );

    let resend_rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT idempotency_key, recipient_email, reference_id
         FROM mail_outbox WHERE idempotency_key LIKE 'resend-mail-%'
         ORDER BY idempotency_key",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(resend_rows.len(), 2);
    assert_eq!(resend_rows[0].0, "resend-mail-1");
    assert_eq!(resend_rows[1].0, "resend-mail-2");
    assert!(resend_rows.iter().all(|row| {
        row.1 == "resend-active@example.test" && row.2 == "resend-active-challenge"
    }));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mail_challenges")
            .fetch_one(database.pool())
            .await?,
        1
    );
    assert_eq!(
        database
            .get_user_invitation("resend-active-invitation", 1_003)
            .await?
            .ok_or("resend invitation disappeared")?
            .expires_at,
        10_000
    );
    Ok(())
}

#[tokio::test]
async fn user_invitation_storage_does_not_resend_inactive_challenges()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    for (id, email, challenge_id, mail_key, expires_at, byte) in [
        (
            "resend-expired-invitation",
            "resend-expired@example.test",
            "resend-expired-challenge",
            "resend-expired-mail",
            1_100,
            17_u8,
        ),
        (
            "resend-revoked-invitation",
            "resend-revoked@example.test",
            "resend-revoked-challenge",
            "resend-revoked-mail",
            10_000,
            18_u8,
        ),
        (
            "resend-accepted-invitation",
            "resend-accepted@example.test",
            "resend-accepted-challenge",
            "resend-accepted-mail",
            10_000,
            19_u8,
        ),
        (
            "resend-consumed-invitation",
            "resend-consumed@example.test",
            "resend-consumed-challenge",
            "resend-consumed-mail",
            10_000,
            20_u8,
        ),
        (
            "resend-challenge-revoked-invitation",
            "resend-challenge-revoked@example.test",
            "resend-challenge-revoked",
            "resend-challenge-revoked-mail",
            10_000,
            21_u8,
        ),
        (
            "resend-challenge-expired-invitation",
            "resend-challenge-expired@example.test",
            "resend-challenge-expired",
            "resend-challenge-expired-mail",
            10_000,
            22_u8,
        ),
        (
            "resend-challenge-missing-invitation",
            "resend-challenge-missing@example.test",
            "resend-challenge-missing",
            "resend-challenge-missing-mail",
            10_000,
            23_u8,
        ),
    ] {
        let token_hash = [byte; 32];
        let (invitation, challenge, outbox) = invitation_storage_bundle(
            id,
            email,
            challenge_id,
            mail_key,
            expires_at,
            1_000,
            &token_hash,
        );
        database
            .create_user_invitation_with_mail(invitation, challenge, outbox, 1_000)
            .await?;
    }

    assert!(
        !database
            .queue_user_invitation_resend("resend-expired-invitation", "resend-x1", 1_100)
            .await?
    );
    assert!(
        database
            .revoke_user_invitation("resend-revoked-invitation", 1_200)
            .await?
    );
    sqlx::query(
        "UPDATE user_invitations SET status = 'ACCEPTED', accepted_at = 1200
         WHERE id = 'resend-accepted-invitation'",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE mail_challenges SET consumed_at = 1200 WHERE id = 'resend-consumed-challenge'",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE mail_challenges SET revoked_at = 1200
         WHERE id = 'resend-challenge-revoked'",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE mail_challenges SET expires_at = 1100
         WHERE id = 'resend-challenge-expired'",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("DELETE FROM mail_challenges WHERE id = 'resend-challenge-missing'")
        .execute(database.pool())
        .await?;

    for (invitation_id, idempotency_key) in [
        ("resend-revoked-invitation", "resend-x2"),
        ("resend-accepted-invitation", "resend-x3"),
        ("resend-consumed-invitation", "resend-x4"),
        ("resend-challenge-revoked-invitation", "resend-x5"),
        ("resend-challenge-expired-invitation", "resend-x6"),
        ("resend-challenge-missing-invitation", "resend-x7"),
    ] {
        assert!(
            !database
                .queue_user_invitation_resend(invitation_id, idempotency_key, 1_201)
                .await?
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mail_outbox")
            .fetch_one(database.pool())
            .await?,
        7
    );
    Ok(())
}

#[tokio::test]
async fn user_invitation_storage_expires_replaced_rows_and_paginates_revokeable_rows()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let first_hash = [3_u8; 32];
    let (invitation, challenge, outbox) = invitation_storage_bundle(
        "invitation-expired",
        "viewer@example.test",
        "challenge-expired",
        "invite-mail-expired",
        1_100,
        1_000,
        &first_hash,
    );
    database
        .create_user_invitation_with_mail(invitation, challenge, outbox, 1_000)
        .await?;

    let second_hash = [4_u8; 32];
    let (invitation, challenge, outbox) = invitation_storage_bundle(
        "invitation-current",
        "viewer@example.test",
        "challenge-current",
        "invite-mail-current",
        10_000,
        1_100,
        &second_hash,
    );
    database
        .create_user_invitation_with_mail(invitation, challenge, outbox, 1_100)
        .await?;
    let expired = database
        .get_user_invitation("invitation-expired", 1_100)
        .await?
        .ok_or("expired invitation was not found")?;
    assert_eq!(expired.status, UserInvitationStatus::Expired);

    let all = database
        .list_user_invitations_page(None, 1_000, 0, 1_100)
        .await?;
    assert_eq!(all.total_count, 2);
    assert_eq!(all.page_size, 100);
    assert_eq!(all.entries.len(), 2);
    assert_eq!(all.entries[0].id, "invitation-current");
    let expired_page = database
        .list_user_invitations_page(Some(UserInvitationStatus::Expired), 20, 0, 1_100)
        .await?;
    assert_eq!(expired_page.total_count, 1);
    assert_eq!(expired_page.entries[0].id, "invitation-expired");

    assert!(
        database
            .revoke_user_invitation("invitation-current", 1_200)
            .await?
    );
    assert!(
        !database
            .revoke_user_invitation("invitation-current", 1_201)
            .await?
    );
    let revoked = database
        .get_user_invitation("invitation-current", 1_201)
        .await?
        .ok_or("revoked invitation was not found")?;
    assert_eq!(revoked.status, UserInvitationStatus::Revoked);
    assert!(
        !database
            .revoke_user_invitation("missing-invitation", 1_201)
            .await?
    );
    Ok(())
}

#[tokio::test]
async fn user_invitation_storage_rolls_back_when_mail_enqueue_fails()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let token_hash = [5_u8; 32];
    let (first_invitation, first_challenge, first_outbox) = invitation_storage_bundle(
        "invitation-existing",
        "existing@example.test",
        "challenge-existing",
        "shared-mail-idempotency",
        10_000,
        1_000,
        &token_hash,
    );
    database
        .create_user_invitation_with_mail(first_invitation, first_challenge, first_outbox, 1_000)
        .await?;

    let second_hash = [6_u8; 32];
    let (second_invitation, second_challenge, second_outbox) = invitation_storage_bundle(
        "invitation-rolled-back",
        "rollback@example.test",
        "challenge-rolled-back",
        "shared-mail-idempotency",
        10_000,
        1_001,
        &second_hash,
    );
    assert!(
        database
            .create_user_invitation_with_mail(
                second_invitation,
                second_challenge,
                second_outbox,
                1_001,
            )
            .await
            .is_err()
    );
    assert!(
        database
            .get_user_invitation("invitation-rolled-back", 1_001)
            .await?
            .is_none()
    );
    assert!(
        database
            .get_mail_challenge("challenge-rolled-back")
            .await?
            .is_none()
    );
    assert_eq!(
        database
            .list_mail_outbox_page(None, 100, 0)
            .await?
            .messages
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_user_invitation_storage_covers_lifecycle_and_atomicity()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration = DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    admin_pool.close().await;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let result = async {
        let database = Database::connect_with_configuration(
            &config,
            &DatabaseConfiguration::Postgres(PostgresConnection {
                database: database_name.clone(),
                ..admin_connection.clone()
            }),
        )
        .await?;
        let token_hash = [7_u8; 32];
        let (invitation, challenge, outbox) = invitation_storage_bundle(
            "postgres-expiring-invitation",
            "postgres@example.test",
            "postgres-expiring-challenge",
            "postgres-mail-key",
            1_100,
            1_000,
            &token_hash,
        );
        assert_eq!(
            database
                .create_user_invitation_with_mail(invitation, challenge, outbox, 1_000)
                .await?,
            UserInvitationCreateResult::Created
        );

        let duplicate_hash = [8_u8; 32];
        let (invitation, challenge, outbox) = invitation_storage_bundle(
            "postgres-duplicate-invitation",
            "postgres@example.test",
            "postgres-duplicate-challenge",
            "postgres-duplicate-mail-key",
            10_000,
            1_050,
            &duplicate_hash,
        );
        assert_eq!(
            database
                .create_user_invitation_with_mail(invitation, challenge, outbox, 1_050)
                .await?,
            UserInvitationCreateResult::AlreadyPending
        );
        assert!(
            database
                .get_mail_challenge("postgres-duplicate-challenge")
                .await?
                .is_none()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mail_outbox")
                .fetch_one(database.pool())
                .await?,
            1
        );

        let replacement_hash = [9_u8; 32];
        let (invitation, challenge, outbox) = invitation_storage_bundle(
            "postgres-replacement-invitation",
            "postgres@example.test",
            "postgres-replacement-challenge",
            "postgres-replacement-mail-key",
            10_000,
            1_100,
            &replacement_hash,
        );
        assert_eq!(
            database
                .create_user_invitation_with_mail(invitation, challenge, outbox, 1_100)
                .await?,
            UserInvitationCreateResult::Created
        );
        assert_eq!(
            database
                .get_user_invitation("postgres-expiring-invitation", 1_100)
                .await?
                .ok_or("expired PostgreSQL invitation was not found")?
                .status,
            UserInvitationStatus::Expired
        );
        assert_eq!(
            database
                .list_user_invitations_page(Some(UserInvitationStatus::Expired), 10, 0, 1_100)
                .await?
                .total_count,
            1
        );

        let revocable_hash = [10_u8; 32];
        let (invitation, challenge, outbox) = invitation_storage_bundle(
            "postgres-revocable-invitation",
            "revoke@example.test",
            "postgres-revocable-challenge",
            "postgres-revocable-mail-key",
            10_000,
            1_200,
            &revocable_hash,
        );
        database
            .create_user_invitation_with_mail(invitation, challenge, outbox, 1_200)
            .await?;

        let accepted_hash = [11_u8; 32];
        let (invitation, challenge, outbox) = invitation_storage_bundle(
            "postgres-accepted-invitation",
            "accepted@example.test",
            "postgres-accepted-challenge",
            "postgres-accepted-mail-key",
            10_000,
            1_200,
            &accepted_hash,
        );
        database
            .create_user_invitation_with_mail(invitation, challenge, outbox, 1_200)
            .await?;
        sqlx::query(
            "UPDATE user_invitations
             SET status = 'ACCEPTED', accepted_at = 1_200, updated_at = 1_200
             WHERE id = 'postgres-accepted-invitation'",
        )
        .execute(database.pool())
        .await?;

        for (id, email, challenge_id, key, byte) in [
            (
                "postgres-stable-a",
                "stable-a@example.test",
                "postgres-stable-a-challenge",
                "postgres-stable-a-mail-key",
                12_u8,
            ),
            (
                "postgres-stable-b",
                "stable-b@example.test",
                "postgres-stable-b-challenge",
                "postgres-stable-b-mail-key",
                13_u8,
            ),
        ] {
            let token_hash = [byte; 32];
            let (invitation, challenge, outbox) =
                invitation_storage_bundle(id, email, challenge_id, key, 10_000, 1_300, &token_hash);
            database
                .create_user_invitation_with_mail(invitation, challenge, outbox, 1_300)
                .await?;
        }

        let page = database
            .list_user_invitations_page(None, 1_000, 0, 1_300)
            .await?;
        assert_eq!(page.total_count, 6);
        assert_eq!(page.page_size, 100);
        assert_eq!(page.entries.len(), 6);
        assert_eq!(page.entries[0].id, "postgres-stable-b");
        assert_eq!(page.entries[1].id, "postgres-stable-a");

        let rollback_hash = [14_u8; 32];
        let (invitation, challenge, outbox) = invitation_storage_bundle(
            "postgres-rolled-back-invitation",
            "rollback@example.test",
            "postgres-rolled-back-challenge",
            "postgres-mail-key",
            10_000,
            1_400,
            &rollback_hash,
        );
        assert!(
            database
                .create_user_invitation_with_mail(invitation, challenge, outbox, 1_400)
                .await
                .is_err()
        );
        assert!(
            database
                .get_user_invitation("postgres-rolled-back-invitation", 1_400)
                .await?
                .is_none()
        );
        assert!(
            database
                .get_mail_challenge("postgres-rolled-back-challenge")
                .await?
                .is_none()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mail_outbox")
                .fetch_one(database.pool())
                .await?,
            6
        );

        let mismatch_hash = [15_u8; 32];
        let (invitation, mut challenge, outbox) = invitation_storage_bundle(
            "postgres-mismatched-invitation",
            "mismatch@example.test",
            "postgres-mismatched-challenge",
            "postgres-mismatched-mail-key",
            10_000,
            1_500,
            &mismatch_hash,
        );
        challenge.recipient_email = "different@example.test";
        assert!(
            database
                .create_user_invitation_with_mail(invitation, challenge, outbox, 1_500)
                .await
                .is_err()
        );
        assert!(
            database
                .get_user_invitation("postgres-mismatched-invitation", 1_500)
                .await?
                .is_none()
        );

        assert!(
            database
                .revoke_user_invitation("postgres-revocable-invitation", 1_501)
                .await?
        );
        assert_eq!(
            database
                .get_user_invitation("postgres-revocable-invitation", 1_501)
                .await?
                .ok_or("PostgreSQL invitation was not found")?
                .status,
            UserInvitationStatus::Revoked
        );
        assert!(
            !database
                .revoke_user_invitation("postgres-revocable-invitation", 1_502)
                .await?
        );
        assert!(
            !database
                .revoke_user_invitation("postgres-expiring-invitation", 1_501)
                .await?
        );
        assert!(
            !database
                .revoke_user_invitation("postgres-accepted-invitation", 1_501)
                .await?
        );
        assert!(
            !database
                .revoke_user_invitation("postgres-missing-invitation", 1_501)
                .await?
        );
        let challenge_revoked_at: Option<i64> =
            sqlx::query_scalar("SELECT revoked_at FROM mail_challenges WHERE id = $1")
                .bind("postgres-revocable-challenge")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(challenge_revoked_at, Some(1_501));
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM mail_outbox WHERE idempotency_key = $1",
            )
            .bind("postgres-revocable-mail-key")
            .fetch_one(database.pool())
            .await?,
            "FAILED"
        );
        assert!(
            !database
                .queue_user_invitation_resend(
                    "postgres-revocable-invitation",
                    "postgres-resend-revoked",
                    1_502
                )
                .await?
        );
        assert!(
            !database
                .queue_user_invitation_resend(
                    "postgres-expiring-invitation",
                    "postgres-resend-expired",
                    1_501
                )
                .await?
        );
        assert!(
            !database
                .queue_user_invitation_resend(
                    "postgres-accepted-invitation",
                    "postgres-resend-accepted",
                    1_501
                )
                .await?
        );

        let resend_hash = [24_u8; 32];
        let (invitation, challenge, outbox) = invitation_storage_bundle(
            "postgres-resend-invitation",
            "postgres-resend@example.test",
            "postgres-resend-challenge",
            "postgres-resend-initial-mail",
            20_000,
            1_600,
            &resend_hash,
        );
        database
            .create_user_invitation_with_mail(invitation, challenge, outbox, 1_600)
            .await?;
        assert!(
            database
                .queue_user_invitation_resend(
                    "postgres-resend-invitation",
                    "postgres-resend-mail-1",
                    1_601
                )
                .await?
        );
        assert!(
            database
                .queue_user_invitation_resend(
                    "postgres-resend-invitation",
                    "postgres-resend-mail-1",
                    1_602
                )
                .await?
        );
        assert!(
            database
                .queue_user_invitation_resend(
                    "postgres-resend-invitation",
                    "postgres-resend-mail-2",
                    1_603
                )
                .await?
        );
        let resend_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM mail_outbox
             WHERE reference_id = 'postgres-resend-challenge'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(resend_count, 3);
        database.close().await;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let admin_url = DatabaseConfiguration::Postgres(PostgresConnection {
        database: "postgres".to_owned(),
        ..admin_connection
    })
    .postgres_url()?
    .ok_or("missing PostgreSQL admin URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    admin_pool.close().await;
    result?;
    Ok(())
}

#[tokio::test]
async fn user_current_playback_sessions_are_filtered_and_paginated()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let admin_id: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(database.pool())
        .await?;
    let user_id = "playback-page-user";
    database
        .insert_user(
            user_id,
            "playback-page-user",
            "Playback User",
            "hash",
            false,
            true,
        )
        .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Playback page", LibraryKind::Movie, false)
        .await?;
    let item_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Playback page', 'playback page', 'LOCAL_CONFIRMED')",
    )
    .bind(&item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await?;
    for (id, session_user_id, state, last_event_at) in [
        ("target-newest", user_id, "PLAYING", now - 1),
        ("target-older", user_id, "PAUSED", now - 2),
        ("target-stale", user_id, "PLAYING", now - 91),
        ("target-stopped", user_id, "STOPPED", now - 1),
        ("other-current", admin_id.as_str(), "PLAYING", now - 1),
    ] {
        sqlx::query(
            "INSERT INTO playback_sessions (
                    id, user_id, item_id, play_session_id, device_id, state, last_event_at
                 ) VALUES (?, ?, ?, ?, 'test-device', ?, ?)",
        )
        .bind(id)
        .bind(session_user_id)
        .bind(&item_id)
        .bind(format!("play-{id}"))
        .bind(state)
        .bind(last_event_at)
        .execute(database.pool())
        .await?;
    }

    let (first_page, first_total) = database
        .list_current_user_playback_sessions(user_id, 1, 0)
        .await?;
    assert_eq!(first_total, 2);
    assert_eq!(first_page.len(), 1);
    assert_eq!(first_page[0].id, "target-newest");

    let (second_page, second_total) = database
        .list_current_user_playback_sessions(user_id, 1, 1)
        .await?;
    assert_eq!(second_total, 2);
    assert_eq!(second_page.len(), 1);
    assert_eq!(second_page[0].id, "target-older");

    for index in 0..101 {
        let id = format!("target-page-{index:03}");
        sqlx::query(
            "INSERT INTO playback_sessions (
                    id, user_id, item_id, play_session_id, device_id, state, last_event_at
                 ) VALUES (?, ?, ?, ?, 'test-device', 'PLAYING', ?)",
        )
        .bind(&id)
        .bind(user_id)
        .bind(&item_id)
        .bind(format!("play-{id}"))
        .bind(now - 3)
        .execute(database.pool())
        .await?;
    }
    let (capped_page, capped_total) = database
        .list_current_user_playback_sessions(user_id, 1_000, -1)
        .await?;
    assert_eq!(capped_total, 103);
    assert_eq!(capped_page.len(), 100);
    assert_eq!(capped_page[0].id, "target-newest");

    Ok(())
}

#[tokio::test]
async fn expired_web_playback_sessions_are_stopped_in_a_bounded_batch() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let setup = SetupService::new(database.clone()).expect("setup service");
    setup
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let user_id: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(database.pool())
        .await
        .expect("user");
    let library = LibraryService::new(database.clone())
        .create_library("Playback cleanup", LibraryKind::Movie, false)
        .await
        .expect("library");
    let item_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Playback cleanup', 'playback cleanup', 'LOCAL_CONFIRMED')",
    )
    .bind(&item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    database
        .insert_web_playback_session(NewWebPlaybackSession {
            id: "expired-session",
            user_id: &user_id,
            item_id: &item_id,
            media_source_id: None,
            play_session_id: "lux-web:expired-session",
            tier: 1,
            plan: "SERVER_HLS",
            temp_dir: Some("/config/web-playback/expired-session"),
            is_admin: true,
            expires_at: 99,
            now: 1,
        })
        .await
        .expect("web playback session");

    for index in 0..129 {
        let id = format!("expired-session-{index:03}");
        database
            .insert_web_playback_session(NewWebPlaybackSession {
                id: &id,
                user_id: &user_id,
                item_id: &item_id,
                media_source_id: None,
                play_session_id: &format!("lux-web:{id}"),
                tier: 1,
                plan: "SERVER_HLS",
                temp_dir: None,
                is_admin: true,
                expires_at: 99,
                now: 1,
            })
            .await
            .expect("additional web playback session");
    }

    database.reset_query_count();
    let expired = database
        .take_expired_web_playback_sessions(100)
        .await
        .expect("expired sessions");

    assert_eq!(expired.len(), 128);
    assert_eq!(database.query_count(), 2);
    assert_eq!(expired[0].id, "expired-session");
    let state: String =
        sqlx::query_scalar("SELECT state FROM web_playback_sessions WHERE id = 'expired-session'")
            .fetch_one(database.pool())
            .await
            .expect("session state");
    assert_eq!(state, "STOPPED");
    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM web_playback_sessions
         WHERE state = 'ACTIVE' AND expires_at < 100",
    )
    .fetch_one(database.pool())
    .await
    .expect("remaining expired sessions");
    assert_eq!(active_count, 2);
}

#[tokio::test]
async fn web_playback_access_context_is_scoped_to_active_session()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        application::{libraries::LibraryService, setup::SetupService},
        config::Config,
        library::LibraryKind,
    };

    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let user = SetupService::new(database.clone())?
        .complete("Playback Context", "Playback Context", "context password")
        .await?;
    let user_id = user.id.to_string();
    let library = LibraryService::new(database.clone())
        .create_library("Playback Context", LibraryKind::Movie, false)
        .await?;
    let item_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES (?, ?, 'MOVIE', 'Playback Context', 'playback context', 'LOCAL_CONFIRMED')",
    )
    .bind(&item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    database
        .insert_web_playback_session(NewWebPlaybackSession {
            id: "playback-context-session",
            user_id: &user_id,
            item_id: &item_id,
            media_source_id: None,
            play_session_id: "lux-web:playback-context-session",
            tier: 1,
            plan: "DIRECT",
            temp_dir: None,
            is_admin: true,
            expires_at: 10_000,
            now: 1,
        })
        .await?;

    let empty_context = database
        .find_web_playback_access_context("playback-context-session")
        .await?
        .ok_or("active playback session has no access context")?;
    assert_eq!(empty_context.user_id, user_id);
    assert_eq!(empty_context.device_id, None);
    assert_eq!(empty_context.client_name, None);

    assert!(
        database
            .set_web_playback_access_context(
                "playback-context-session",
                &user_id,
                Some("browser-device-id"),
                Some("Lux Web"),
                2,
            )
            .await?
    );
    let stored_context = database
        .find_web_playback_access_context("playback-context-session")
        .await?
        .ok_or("updated playback context was not found")?;
    assert_eq!(
        stored_context.device_id.as_deref(),
        Some("browser-device-id")
    );
    assert_eq!(stored_context.client_name.as_deref(), Some("Lux Web"));

    assert!(
        !database
            .set_web_playback_access_context(
                "playback-context-session",
                "another-user",
                Some("other-device"),
                Some("Other Client"),
                3,
            )
            .await?
    );
    sqlx::query(
        "UPDATE web_playback_sessions SET state = 'STOPPED' WHERE id = 'playback-context-session'",
    )
    .execute(database.pool())
    .await?;
    assert!(
        !database
            .set_web_playback_access_context(
                "playback-context-session",
                &user_id,
                Some("late-device"),
                Some("Late Client"),
                4,
            )
            .await?
    );
    assert!(
        database
            .find_web_playback_access_context("playback-context-session")
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn emby_direct_playback_ticket_migration_creates_revocable_context_table()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{application::setup::SetupService, config::Config};

    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'emby_direct_playback_tickets'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(table_count, 1);

    let columns = sqlx::query("PRAGMA table_info(emby_direct_playback_tickets)")
        .fetch_all(database.pool())
        .await?
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect::<Vec<_>>();
    for expected in [
        "user_id",
        "play_session_id",
        "authorization_device_id",
        "authorization_client_name",
        "expires_at",
        "revoked_at",
        "created_at",
    ] {
        assert!(columns.iter().any(|column| column == expected));
    }

    let user = SetupService::new(database.clone())?
        .complete("Direct Ticket", "Direct Ticket", "ticket password")
        .await?;
    let user_id = user.id.to_string();
    sqlx::query(
        "INSERT INTO emby_direct_playback_tickets (
             user_id, play_session_id, authorization_device_id,
             authorization_client_name, expires_at, created_at
         ) VALUES (?, 'ticket-session', 'device-1', 'client-1', 500, 100)",
    )
    .bind(&user_id)
    .execute(database.pool())
    .await?;
    assert!(
        sqlx::query(
            "INSERT INTO emby_direct_playback_tickets (
                 user_id, play_session_id, expires_at, created_at
             ) VALUES (?, '', 500, 100)",
        )
        .bind(&user_id)
        .execute(database.pool())
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO emby_direct_playback_tickets (
                 user_id, play_session_id, expires_at, created_at
             ) VALUES (?, 'negative-time', -1, 100)",
        )
        .bind(&user_id)
        .execute(database.pool())
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO emby_direct_playback_tickets (
                 user_id, play_session_id, expires_at, created_at
             ) VALUES (?, 'reversed-time', 99, 100)",
        )
        .bind(&user_id)
        .execute(database.pool())
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO emby_direct_playback_tickets (
                 user_id, play_session_id, expires_at, created_at
             ) VALUES (?, 'ticket-session', 500, 100)",
        )
        .bind(&user_id)
        .execute(database.pool())
        .await
        .is_err()
    );
    let expiry_index_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'idx_emby_direct_playback_tickets_expiry'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(expiry_index_count, 1);

    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(&user_id)
        .execute(database.pool())
        .await?;
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM emby_direct_playback_tickets WHERE user_id = ?")
            .bind(&user_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining, 0);
    Ok(())
}

#[tokio::test]
async fn emby_direct_playback_ticket_storage_scopes_and_revokes_context()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::{application::setup::SetupService, config::Config};

    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let user = SetupService::new(database.clone())?
        .complete(
            "Direct Ticket Storage",
            "Direct Ticket Storage",
            "ticket password",
        )
        .await?;
    let user_id = user.id.to_string();
    database
        .insert_emby_direct_playback_ticket(NewEmbyDirectPlaybackTicket {
            user_id: &user_id,
            play_session_id: "stored-ticket-session",
            authorization_device_id: Some("device-1"),
            authorization_client_name: Some("Client 1"),
            expires_at: 500,
            created_at: 100,
        })
        .await?;

    let stored = database
        .find_emby_direct_playback_ticket(&user_id, "stored-ticket-session")
        .await?
        .ok_or("stored Emby direct playback ticket was not found")?;
    assert_eq!(stored.authorization_device_id.as_deref(), Some("device-1"));
    assert_eq!(
        stored.authorization_client_name.as_deref(),
        Some("Client 1")
    );
    assert_eq!(stored.revoked_at, None);
    assert!(
        database
            .find_emby_direct_playback_ticket("another-user", "stored-ticket-session")
            .await?
            .is_none()
    );

    assert!(
        !database
            .ensure_legacy_emby_direct_playback_ticket(&user_id, "stored-ticket-session", 500, 100,)
            .await?
    );
    let unchanged = database
        .find_emby_direct_playback_ticket(&user_id, "stored-ticket-session")
        .await?
        .ok_or("ticket disappeared after legacy ensure")?;
    assert_eq!(
        unchanged.authorization_device_id.as_deref(),
        Some("device-1")
    );

    assert!(
        database
            .ensure_legacy_emby_direct_playback_ticket(&user_id, "legacy-ticket-session", 600, 100)
            .await?
    );
    let legacy = database
        .find_emby_direct_playback_ticket(&user_id, "legacy-ticket-session")
        .await?
        .ok_or("legacy ticket context was not created")?;
    assert_eq!(legacy.authorization_device_id, None);
    assert_eq!(legacy.authorization_client_name, None);

    assert!(
        database
            .revoke_emby_direct_playback_ticket(&user_id, "stored-ticket-session", 200)
            .await?
    );
    assert!(
        !database
            .revoke_emby_direct_playback_ticket(&user_id, "stored-ticket-session", 300)
            .await?
    );
    assert_eq!(
        database
            .find_emby_direct_playback_ticket(&user_id, "stored-ticket-session")
            .await?
            .and_then(|ticket| ticket.revoked_at),
        Some(200)
    );

    database
        .insert_emby_direct_playback_ticket(NewEmbyDirectPlaybackTicket {
            user_id: &user_id,
            play_session_id: "expired-ticket-session",
            authorization_device_id: None,
            authorization_client_name: None,
            expires_at: 150,
            created_at: 100,
        })
        .await?;
    assert_eq!(
        database
            .delete_expired_emby_direct_playback_tickets(151, 1)
            .await?,
        1
    );
    assert!(
        database
            .find_emby_direct_playback_ticket(&user_id, "expired-ticket-session")
            .await?
            .is_none()
    );
    sqlx::query(
        "WITH RECURSIVE numbers(value) AS (
             SELECT 1
             UNION ALL
             SELECT value + 1 FROM numbers WHERE value < 1001
         )
         INSERT INTO emby_direct_playback_tickets (
             user_id, play_session_id, expires_at, created_at
         )
         SELECT ?, 'bounded-expired-' || value, 150, 100 FROM numbers",
    )
    .bind(&user_id)
    .execute(database.pool())
    .await?;
    assert_eq!(
        database
            .delete_expired_emby_direct_playback_tickets(151, 50_000)
            .await?,
        1_000
    );
    let remaining_expired: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM emby_direct_playback_tickets
         WHERE user_id = ? AND expires_at < 151",
    )
    .bind(&user_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(remaining_expired, 1);
    assert_eq!(
        database
            .delete_expired_emby_direct_playback_tickets(151, 0)
            .await?,
        1
    );
    Ok(())
}

#[tokio::test]
async fn inactive_server_hls_sessions_are_stopped_in_a_bounded_batch() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let setup = SetupService::new(database.clone()).expect("setup service");
    setup
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let user_id: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(database.pool())
        .await
        .expect("user");
    let library = LibraryService::new(database.clone())
        .create_library("Playback stale cleanup", LibraryKind::Movie, false)
        .await
        .expect("library");
    let item_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Playback stale cleanup', 'playback stale cleanup', 'LOCAL_CONFIRMED')",
    )
    .bind(&item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    database
        .insert_web_playback_session(NewWebPlaybackSession {
            id: "inactive-session",
            user_id: &user_id,
            item_id: &item_id,
            media_source_id: None,
            play_session_id: "lux-web:inactive-session",
            tier: 4,
            plan: "SERVER_HLS",
            temp_dir: Some("/config/web-playback/inactive-session"),
            is_admin: true,
            expires_at: 10_000,
            now: 1_000,
        })
        .await
        .expect("web playback session");

    for index in 0..129 {
        let id = format!("inactive-session-{index:03}");
        database
            .insert_web_playback_session(NewWebPlaybackSession {
                id: &id,
                user_id: &user_id,
                item_id: &item_id,
                media_source_id: None,
                play_session_id: &format!("lux-web:{id}"),
                tier: 4,
                plan: "SERVER_HLS",
                temp_dir: None,
                is_admin: true,
                expires_at: 10_000,
                now: 1_000,
            })
            .await
            .expect("additional inactive web playback session");
        sqlx::query(
            "UPDATE web_playback_sessions
             SET last_heartbeat_at = 900
             WHERE id = ?",
        )
        .bind(&id)
        .execute(database.pool())
        .await
        .expect("stale additional heartbeat");
    }
    assert_eq!(
        database
            .accept_web_playback_event(NewWebPlaybackEvent {
                session_id: "inactive-session",
                user_id: &user_id,
                event_id: "playing-1",
                sequence: 0,
                state: "PLAYING",
                position_ticks: 0,
                duration_ticks: Some(10_000),
                now: 1_000,
            })
            .await
            .expect("playing event"),
        WebPlaybackEventClaim::Accepted
    );
    let last_heartbeat_at: i64 = sqlx::query_scalar(
        "SELECT last_heartbeat_at FROM web_playback_sessions WHERE id = 'inactive-session'",
    )
    .fetch_one(database.pool())
    .await
    .expect("heartbeat timestamp");
    assert_eq!(last_heartbeat_at, 1_000);
    sqlx::query(
        "UPDATE web_playback_sessions
         SET last_heartbeat_at = 900
         WHERE id = 'inactive-session'",
    )
    .execute(database.pool())
    .await
    .expect("stale heartbeat");

    database.reset_query_count();
    let inactive = database
        .take_inactive_web_playback_sessions(1_000, 90)
        .await
        .expect("inactive sessions");

    assert_eq!(inactive.len(), 128);
    assert_eq!(database.query_count(), 2);
    assert_eq!(inactive[0].id, "inactive-session");
    let state: String =
        sqlx::query_scalar("SELECT state FROM web_playback_sessions WHERE id = 'inactive-session'")
            .fetch_one(database.pool())
            .await
            .expect("session state");
    assert_eq!(state, "STOPPED");
    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM web_playback_sessions
         WHERE state = 'ACTIVE' AND plan = 'SERVER_HLS' AND last_heartbeat_at < 910",
    )
    .fetch_one(database.pool())
    .await
    .expect("remaining inactive sessions");
    assert_eq!(active_count, 2);
}

#[tokio::test]
async fn web_playback_cleanup_updates_each_kind_in_one_statement() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let setup = SetupService::new(database.clone()).expect("setup service");
    setup
        .complete("Admin", "Admin", "correct password")
        .await
        .expect("setup");
    let user_id: String = sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(database.pool())
        .await
        .expect("user");
    let library = LibraryService::new(database.clone())
        .create_library("Playback cleanup batch", LibraryKind::Movie, false)
        .await
        .expect("library");
    let item_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Playback cleanup batch',
                       'playback cleanup batch', 'LOCAL_CONFIRMED')",
    )
    .bind(&item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await
    .expect("media item");
    for prefix in ["expired", "inactive"] {
        for index in 0..3 {
            let id = format!("{prefix}-session-{index}");
            database
                .insert_web_playback_session(NewWebPlaybackSession {
                    id: &id,
                    user_id: &user_id,
                    item_id: &item_id,
                    media_source_id: None,
                    play_session_id: &format!("lux-web:{id}"),
                    tier: 1,
                    plan: "SERVER_HLS",
                    temp_dir: None,
                    is_admin: true,
                    expires_at: if prefix == "expired" { 99 } else { 10_000 },
                    now: 1_000,
                })
                .await
                .expect("web playback session");
        }
    }
    sqlx::query(
        "UPDATE web_playback_sessions
         SET last_heartbeat_at = 900
         WHERE id LIKE 'inactive-session-%'",
    )
    .execute(database.pool())
    .await
    .expect("inactive heartbeat");

    database.reset_query_count();
    let expired = database
        .take_expired_web_playback_sessions(1_000)
        .await
        .expect("expired sessions");
    assert_eq!(expired.len(), 3);
    assert_eq!(database.query_count(), 2);

    database.reset_query_count();
    let inactive = database
        .take_inactive_web_playback_sessions(1_000, 90)
        .await
        .expect("inactive sessions");
    assert_eq!(inactive.len(), 3);
    assert_eq!(database.query_count(), 2);
}

#[tokio::test]
async fn user_updates_wait_for_a_concurrent_sqlite_writer() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let user_id = Uuid::now_v7().to_string();
    database
        .insert_initial_user(&user_id, "admin", "Admin", "hash")
        .await
        .expect("user");

    let mut blocker = database.pool().acquire().await.expect("blocker connection");
    sqlx::query("BEGIN EXCLUSIVE")
        .execute(&mut *blocker)
        .await
        .expect("begin exclusive transaction");

    let update_database = database.clone();
    let update = tokio::spawn(async move {
        update_database
            .update_user(
                &user_id,
                UpdateUser {
                    display_name: Some("Updated"),
                    password_hash: None,
                    has_password: None,
                    is_disabled: None,
                    is_admin: None,
                    can_manage_server: None,
                    can_remote_access: None,
                    can_download: None,
                },
            )
            .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    sqlx::query("COMMIT")
        .execute(&mut *blocker)
        .await
        .expect("release exclusive transaction");
    let updated = update
        .await
        .expect("user update task")
        .expect("user update")
        .expect("updated user");

    assert_eq!(updated.display_name, "Updated");
}

#[tokio::test]
async fn database_lifecycle_cleanup_is_one_time_and_preserves_retry_state() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Cleanup", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("root directory");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("root path"))
        .await
        .expect("root")
        .root;
    let library_id = library.id.to_string();
    let root_id = root.id.to_string();
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(database.pool())
        .await
        .expect("current timestamp");

    for (job_id, job_type, status, cursor, current_item, cancel_requested) in [
        (
            "cleanup-completed",
            "RECONCILE_LIBRARY",
            "COMPLETED",
            Some("completed-cursor"),
            Some("completed-item"),
            1_i64,
        ),
        (
            "cleanup-failed",
            "INCREMENTAL_SCAN",
            "FAILED",
            Some("failed-cursor"),
            Some("failed-item"),
            0_i64,
        ),
        (
            "cleanup-active",
            "INCREMENTAL_SCAN",
            "RUNNING",
            Some("active-cursor"),
            Some("active-item"),
            0_i64,
        ),
    ] {
        sqlx::query(
            "INSERT INTO scan_jobs (
                id, library_id, job_type, status, generation, cursor,
                current_item, cancel_requested, scan_phase, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'IDLE', ?, ?)",
        )
        .bind(job_id)
        .bind(&library_id)
        .bind(job_type)
        .bind(status)
        .bind(format!("generation-{job_id}"))
        .bind(cursor)
        .bind(current_item)
        .bind(cancel_requested)
        .bind(now - 10 * 86_400)
        .bind(now - 10 * 86_400)
        .execute(database.pool())
        .await
        .expect("scan job");
    }
    sqlx::query(
        "INSERT INTO scan_jobs (
            id, library_id, job_type, status, generation, cursor,
            current_item, cancel_requested, scan_phase, created_at, updated_at
         ) VALUES ('cleanup-postprocessing', ?, 'RECONCILE_LIBRARY', 'COMPLETED', ?,
                   'postprocessing-cursor', 'postprocessing-item', 0, 'POSTPROCESSING', ?, ?)",
    )
    .bind(&library_id)
    .bind("generation-cleanup-postprocessing")
    .bind(now - 10 * 86_400)
    .bind(now - 10 * 86_400)
    .execute(database.pool())
    .await
    .expect("postprocessing scan job");

    sqlx::query(
        "INSERT INTO scan_job_paths (job_id, library_root_id, relative_path, change_kind)
         VALUES ('cleanup-completed', ?, 'completed.mkv', 'MODIFY'),
                ('cleanup-failed', ?, 'failed.mkv', 'MODIFY'),
                ('cleanup-active', ?, 'active.mkv', 'MODIFY')",
    )
    .bind(&root_id)
    .bind(&root_id)
    .bind(&root_id)
    .execute(database.pool())
    .await
    .expect("scan job paths");
    sqlx::query(
        "INSERT INTO reconciliation_scan_entries (
            job_id, library_root_id, relative_path, entry_type
         ) VALUES ('cleanup-completed', ?, 'completed', 'FILE'),
                  ('cleanup-failed', ?, 'failed', 'FILE'),
                  ('cleanup-active', ?, 'active', 'FILE')",
    )
    .bind(&root_id)
    .bind(&root_id)
    .bind(&root_id)
    .execute(database.pool())
    .await
    .expect("reconciliation entries");

    for (job_id, target_id, metadata_state) in [
        ("cleanup-completed", "completed-target", "DONE"),
        ("cleanup-failed", "failed-target-done", "DONE"),
        ("cleanup-failed", "failed-target-retry", "FAILED"),
        ("cleanup-active", "active-target", "PENDING"),
        ("cleanup-postprocessing", "postprocessing-target", "PENDING"),
    ] {
        sqlx::query(
            "INSERT INTO scan_job_targets (
                job_id, target_type, target_id, item_id, change_kind,
                probe_state, metadata_state, thumbnail_state
             ) VALUES (?, 'ITEM', ?, ?, 'CHANGED', 'SKIPPED', ?, 'SKIPPED')",
        )
        .bind(job_id)
        .bind(target_id)
        .bind(target_id)
        .bind(metadata_state)
        .execute(database.pool())
        .await
        .expect("scan target");
    }

    sqlx::query("DROP TABLE scan_job_events")
        .execute(database.pool())
        .await
        .expect("drop legacy log table to verify cleanup does not query it");

    let report = database
        .run_database_lifecycle_cleanup()
        .await
        .expect("cleanup")
        .expect("cleanup should be claimed");
    assert_eq!(report.scan_job_paths_deleted, 1);
    assert_eq!(report.reconciliation_entries_deleted, 1);
    assert_eq!(report.scan_job_targets_deleted, 2);
    assert_eq!(report.scan_jobs_summarized, 2);

    let remaining_paths: Vec<String> =
        sqlx::query_scalar("SELECT job_id FROM scan_job_paths ORDER BY job_id")
            .fetch_all(database.pool())
            .await
            .expect("remaining paths");
    assert_eq!(remaining_paths, ["cleanup-active", "cleanup-failed"]);
    let remaining_entries: Vec<String> =
        sqlx::query_scalar("SELECT job_id FROM reconciliation_scan_entries ORDER BY job_id")
            .fetch_all(database.pool())
            .await
            .expect("remaining entries");
    assert_eq!(remaining_entries, ["cleanup-active", "cleanup-failed"]);
    let remaining_targets: Vec<(String, String)> =
        sqlx::query_as("SELECT job_id, target_id FROM scan_job_targets ORDER BY job_id, target_id")
            .fetch_all(database.pool())
            .await
            .expect("remaining targets");
    assert_eq!(
        remaining_targets,
        [
            ("cleanup-active".to_owned(), "active-target".to_owned()),
            (
                "cleanup-failed".to_owned(),
                "failed-target-retry".to_owned()
            ),
            (
                "cleanup-postprocessing".to_owned(),
                "postprocessing-target".to_owned()
            ),
        ]
    );
    let summary: (Option<String>, Option<String>, i64) = sqlx::query_as(
        "SELECT cursor, current_item, cancel_requested
         FROM scan_jobs WHERE id = 'cleanup-completed'",
    )
    .fetch_one(database.pool())
    .await
    .expect("completed summary");
    assert_eq!(summary, (None, None, 0));
    let active_cursor: Option<String> =
        sqlx::query_scalar("SELECT cursor FROM scan_jobs WHERE id = 'cleanup-active'")
            .fetch_one(database.pool())
            .await
            .expect("active job");
    assert_eq!(active_cursor.as_deref(), Some("active-cursor"));
    let postprocessing_summary: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT cursor, current_item FROM scan_jobs
         WHERE id = 'cleanup-postprocessing'",
    )
    .fetch_one(database.pool())
    .await
    .expect("postprocessing job");
    assert_eq!(
        postprocessing_summary,
        (
            Some("postprocessing-cursor".to_owned()),
            Some("postprocessing-item".to_owned())
        )
    );

    assert!(
        database
            .run_database_lifecycle_cleanup()
            .await
            .expect("second cleanup")
            .is_none()
    );
    let marker: String = sqlx::query_scalar(
        "SELECT value FROM lux_meta WHERE key = 'database_lifecycle_cleanup_v1'",
    )
    .fetch_one(database.pool())
    .await
    .expect("cleanup marker");
    assert_eq!(marker, "COMPLETED");

    assert!(
        database
            .run_database_lifecycle_cleanup()
            .await
            .expect("recurring cleanup")
            .is_none()
    );
}

#[tokio::test]
async fn reconciliation_batch_commit_is_atomic_and_counts_confirmed_and_missing_entries() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Atomic scan", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("root directory");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 root"))
        .await
        .expect("library root")
        .root;
    let library_id = library.id.to_string();
    let root_id = root.id.to_string();

    let job_id = "reconciliation-atomic-test";
    let generation = "generation-1";
    sqlx::query(
        "INSERT INTO scan_jobs (
             id, library_id, job_type, status, generation, total_count,
             discovery_completed, processed_count
         ) VALUES (?, ?, 'RECONCILE_LIBRARY', 'RUNNING', ?, 0, 1, 0)",
    )
    .bind(job_id)
    .bind(&library_id)
    .bind(generation)
    .execute(database.pool())
    .await
    .expect("scan job");

    let first_path = "Atomic.Movie.2024.mkv";
    let second_path = "Already.Done.Movie.2023.mkv";
    let attachment_path = "Atomic.Movie.2024.en.srt";
    for path in [first_path, second_path, attachment_path] {
        sqlx::query(
            "INSERT INTO reconciliation_scan_entries (
                 job_id, library_root_id, relative_path, entry_type, status
             ) VALUES (?, ?, ?, 'FILE', 'PENDING')",
        )
        .bind(job_id)
        .bind(&root_id)
        .bind(path)
        .execute(database.pool())
        .await
        .expect("pending work item");
    }
    sqlx::query(
        "INSERT INTO reconciliation_scan_entries (
             job_id, library_root_id, relative_path, entry_type, status
         ) VALUES (?, ?, '', 'DIRECTORY', 'PENDING')",
    )
    .bind(job_id)
    .bind(&root_id)
    .execute(database.pool())
    .await
    .expect("pending directory work item");

    let movie_file = NewMovieFile {
        filesystem_entry_id: "atomic-filesystem-entry".to_owned(),
        source_id: "atomic-source".to_owned(),
        relative_path: first_path.to_owned(),
        size: 7,
        modified_at: 1,
        device: None,
        inode: None,
        fingerprint: vec![1, 2, 3],
        title: "Atomic Movie".to_owned(),
        sort_title: "atomic movie".to_owned(),
        original_title: "Atomic Movie".to_owned(),
        production_year: Some(2024),
        provider_ids_json: None,
        source_kind: "LOCAL_FILE".to_owned(),
        strm_target_kind: None,
        edition_name: None,
        quality_label: None,
        container: "mkv".to_owned(),
        external_url: None,
    };
    let entries = vec![
        StoredReconciliationScanEntry {
            library_root_id: root_id.clone(),
            relative_path: attachment_path.to_owned(),
        },
        StoredReconciliationScanEntry {
            library_root_id: root_id.clone(),
            relative_path: first_path.to_owned(),
        },
        StoredReconciliationScanEntry {
            library_root_id: root_id.clone(),
            relative_path: second_path.to_owned(),
        },
    ];
    let new_paths = vec![first_path.to_owned()];
    let missing_paths = vec![second_path.to_owned()];
    let file_change_events = vec![(attachment_path.to_owned(), "ADD".to_owned())];

    sqlx::query(
        "CREATE TRIGGER fail_reconciliation_targets
         BEFORE INSERT ON scan_job_targets
         BEGIN SELECT RAISE(ABORT, 'injected target failure'); END",
    )
    .execute(database.pool())
    .await
    .expect("failure trigger");

    let batch = ReconciliationBatchCommit {
        job_id,
        library_id: &library_id,
        library_root_id: &root_id,
        generation,
        entries: &entries,
        movie_files: std::slice::from_ref(&movie_file),
        episode_files: &[],
        seen_entry_ids: &[],
        missing_paths: &missing_paths,
        new_paths: &new_paths,
        changed_paths: &[],
        sidecar_paths: &[],
        file_change_events: &file_change_events,
    };
    assert!(database.commit_reconciliation_batch(&batch).await.is_err());

    let rollback_state: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM filesystem_entries),
             (SELECT COUNT(*) FROM media_sources),
             (SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?),
             (SELECT COUNT(*) FROM scan_job_file_changes WHERE job_id = ?),
             (SELECT processed_count FROM scan_jobs WHERE id = ?)",
    )
    .bind(job_id)
    .bind(job_id)
    .bind(job_id)
    .fetch_one(database.pool())
    .await
    .expect("rollback state");
    assert_eq!(rollback_state, (0, 0, 0, 0, 0));
    let pending_after_failure: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = ? AND entry_type = 'FILE' AND status = 'PENDING'",
    )
    .bind(job_id)
    .fetch_one(database.pool())
    .await
    .expect("pending after failure");
    assert_eq!(pending_after_failure, 3);

    sqlx::query("DROP TRIGGER fail_reconciliation_targets")
        .execute(database.pool())
        .await
        .expect("drop failure trigger");
    database.reset_query_count();
    let committed = database
        .commit_reconciliation_batch(&batch)
        .await
        .expect("retry batch");
    assert_eq!(committed.confirmed_entries, 3);
    assert!(committed.metadata_targets_changed);
    assert_eq!(database.query_count(), 12);

    let persisted_file_change: (String, String) = sqlx::query_as(
        "SELECT relative_path, change_kind FROM scan_job_file_changes
         WHERE job_id = ? AND library_root_id = ?",
    )
    .bind(job_id)
    .bind(&root_id)
    .fetch_one(database.pool())
    .await
    .expect("committed file change");
    assert_eq!(
        persisted_file_change,
        (attachment_path.to_owned(), "ADD".to_owned())
    );

    let second_commit = database
        .commit_reconciliation_batch(&batch)
        .await
        .expect("idempotent confirmation");
    assert_eq!(second_commit.confirmed_entries, 0);
    assert!(!second_commit.metadata_targets_changed);

    let entry_states: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, COUNT(*)
         FROM reconciliation_scan_entries
         WHERE job_id = ?
         GROUP BY status
         ORDER BY status",
    )
    .bind(job_id)
    .fetch_all(database.pool())
    .await
    .expect("entry states");
    assert_eq!(
        entry_states,
        vec![("DONE".to_owned(), 2), ("PENDING".to_owned(), 1)]
    );

    let final_state: (i64, i64, i64, i64, i64, Option<String>) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM filesystem_entries),
             (SELECT COUNT(*) FROM media_sources),
             (SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?),
             (SELECT processed_count FROM scan_jobs WHERE id = ?),
             (SELECT total_count FROM scan_jobs WHERE id = ?),
             (SELECT cursor FROM scan_jobs WHERE id = ?)",
    )
    .bind(job_id)
    .bind(job_id)
    .bind(job_id)
    .bind(job_id)
    .fetch_one(database.pool())
    .await
    .expect("final state");
    assert_eq!(final_state, (1, 1, 2, 3, 3, Some(second_path.to_owned())));
    assert!(final_state.4 >= final_state.3);
}

#[tokio::test]
async fn empty_reconciliation_batch_is_a_noop() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let batch = ReconciliationBatchCommit {
        job_id: "missing-job",
        library_id: "missing-library",
        library_root_id: "missing-root",
        generation: "missing-generation",
        entries: &[],
        movie_files: &[],
        episode_files: &[],
        seen_entry_ids: &[],
        missing_paths: &[],
        new_paths: &[],
        changed_paths: &[],
        sidecar_paths: &[],
        file_change_events: &[],
    };

    database.reset_query_count();
    let result = database
        .commit_reconciliation_batch(&batch)
        .await
        .expect("empty batch should be ignored");

    assert_eq!(result.confirmed_entries, 0);
    assert_eq!(result.created_items, 0);
    assert!(!result.metadata_targets_changed);
    assert_eq!(database.query_count(), 0);
}

#[tokio::test]
async fn fill_missing_job_creation_coalesces_active_items() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let media_root = temp_dir.path().join("Movies");
    for title in ["First Movie (2025)", "Second Movie (2025)"] {
        let directory = media_root.join(title);
        tokio::fs::create_dir_all(&directory).await?;
        tokio::fs::write(
            directory.join(format!("{}.mkv", title.replace(' ', "."))),
            b"video",
        )
        .await?;
    }
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Fill missing coalesce", LibraryKind::Movie, false)
        .await?;
    LibraryService::new(database.clone())
        .add_root(library.id, media_root.to_str().ok_or("media root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_ids: Vec<String> = database
        .query_scalar(
            "SELECT id FROM media_items
             WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY id",
        )
        .bind(library.id.to_string())
        .fetch_all(database.pool())
        .await?;
    assert_eq!(item_ids.len(), 2);
    let library_id = library.id.to_string();
    let first_job = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[..1])
        .await?;
    let merged_job = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids)
        .await?;
    assert_eq!(merged_job, first_job);
    let repeated_job = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids)
        .await?;
    assert_eq!(repeated_job, first_job);
    let job_count: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM metadata_reidentify_jobs
             WHERE library_id = ? AND mode = 'FILL_MISSING'",
        )
        .bind(&library_id)
        .fetch_one(database.pool())
        .await?;
    let item_count: i64 = database
        .query_scalar("SELECT COUNT(*) FROM metadata_reidentify_job_items WHERE job_id = ?")
        .bind(&first_job)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(job_count, 1);
    assert_eq!(item_count, 2);
    assert!(
        database
            .request_metadata_reidentify_job_cancel(&first_job)
            .await?
    );
    database
        .finish_metadata_reidentify_job(&first_job, "COMPLETED", None)
        .await?;
    let cancelled_items: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM metadata_reidentify_job_items
             WHERE job_id = ? AND status = 'FAILED'",
        )
        .bind(&first_job)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(cancelled_items, 2);
    assert!(database.retry_metadata_reidentify_job(&first_job).await?);
    let pending_items: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM metadata_reidentify_job_items
             WHERE job_id = ? AND status = 'PENDING'",
        )
        .bind(&first_job)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(pending_items, 2);

    sqlx::query(
        "UPDATE metadata_reidentify_jobs SET status = 'DEFERRED', updated_at = unixepoch()
         WHERE id = ?",
    )
    .bind(&first_job)
    .execute(database.pool())
    .await
    .expect("defer fill-missing job after provider failure");
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'FAILED', error = 'SCRAPER_UNAVAILABLE'
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(&first_job)
    .bind(&item_ids[0])
    .execute(database.pool())
    .await
    .expect("record deferred provider failure");
    let repeated_deferred_job = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[..1])
        .await?;
    assert_eq!(repeated_deferred_job, first_job);

    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET error = 'METADATA_WRITE_FAILED'
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(&first_job)
    .bind(&item_ids[0])
    .execute(database.pool())
    .await
    .expect("change deferred error to a non-provider failure");
    let retried_non_provider_failure = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[..1])
        .await?;
    assert_ne!(retried_non_provider_failure, first_job);

    sqlx::query(
        "UPDATE metadata_reidentify_jobs SET status = 'DEFERRED', updated_at = unixepoch() - 3601
         WHERE id = ?",
    )
    .bind(&retried_non_provider_failure)
    .execute(database.pool())
    .await
    .expect("age deferred fill-missing job");
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'FAILED', error = 'SCRAPER_UNAVAILABLE'
         WHERE job_id = ? AND item_id = ?",
    )
    .bind(&retried_non_provider_failure)
    .bind(&item_ids[0])
    .execute(database.pool())
    .await
    .expect("record old provider failure");
    let retried_expired_deferred_job = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[..1])
        .await?;
    assert_ne!(retried_expired_deferred_job, retried_non_provider_failure);
    Ok(())
}

#[tokio::test]
async fn automatic_and_explicit_fill_missing_jobs_keep_dispatch_sources_separate()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Dispatch source separation", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    for item_id in [
        "dispatch-explicit-item",
        "dispatch-automatic-item",
        "dispatch-automatic-item-2",
        "dispatch-explicit-after-auto-item",
    ] {
        database
            .query(
                "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
    }

    let explicit_job_id = database
        .create_or_merge_fill_missing_job(&library_id, &["dispatch-explicit-item".to_owned()])
        .await?;
    let fingerprint = b"automatic-dispatch-source";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                "dispatch-automatic-item",
                "POSTER",
                fingerprint,
            )
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                "dispatch-automatic-item",
                "POSTER",
                fingerprint,
            )
            .await?
    );
    let results = [NewItemMetadataCompletenessResult {
        item_id: "dispatch-automatic-item",
        capability: "POSTER",
        input_fingerprint: fingerprint,
        is_missing: true,
        checked_at: 42,
    }];
    let automatic_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &results,
            &["dispatch-automatic-item".to_owned()],
        )
        .await?;
    assert_eq!(automatic_dispatch.scheduled_job_ids.len(), 1);
    let automatic_job_id = automatic_dispatch.scheduled_job_ids[0].clone();
    assert_ne!(automatic_job_id, explicit_job_id);

    assert_eq!(
        database
            .metadata_reidentify_job_dispatch_source(&automatic_job_id)
            .await?,
        Some("AUTOMATIC".to_owned())
    );
    let automatic_created_at: i64 = database
        .query_scalar("SELECT created_at FROM metadata_reidentify_jobs WHERE id = ?")
        .bind(&automatic_job_id)
        .fetch_one(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO metadata_reidentify_jobs (
             id, status, total_count, mode, library_id, job_scope, dispatch_source, created_at
         ) VALUES ('automatic-dispatch-page-2', 'QUEUED', 0, 'FILL_MISSING', ?, 'LIBRARY', 'AUTOMATIC', ?)",
    )
    .bind(&library_id)
    .bind(automatic_created_at + 1)
    .execute(database.pool())
    .await?;
    let first_automatic_page = database
        .list_queued_automatic_fill_missing_job_ids(&library_id, None, None, 1)
        .await?;
    assert_eq!(first_automatic_page.len(), 1);
    assert_eq!(first_automatic_page[0].0, automatic_job_id);
    let second_automatic_page = database
        .list_queued_automatic_fill_missing_job_ids(
            &library_id,
            Some(first_automatic_page[0].1),
            Some(&first_automatic_page[0].0),
            1,
        )
        .await?;
    assert_eq!(
        second_automatic_page,
        vec![(
            "automatic-dispatch-page-2".to_owned(),
            automatic_created_at + 1
        )]
    );
    assert_eq!(
        database
            .list_queued_automatic_fill_missing_library_ids(None, 1)
            .await?,
        vec![library_id.clone()]
    );
    assert!(
        database
            .list_queued_automatic_fill_missing_library_ids(Some(&library_id), 1)
            .await?
            .is_empty()
    );
    sqlx::query("DELETE FROM metadata_reidentify_jobs WHERE id = 'automatic-dispatch-page-2'")
        .execute(database.pool())
        .await?;

    let second_fingerprint = b"automatic-dispatch-source-second";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                "dispatch-automatic-item-2",
                "POSTER",
                second_fingerprint,
            )
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                "dispatch-automatic-item-2",
                "POSTER",
                second_fingerprint,
            )
            .await?
    );
    let second_results = [NewItemMetadataCompletenessResult {
        item_id: "dispatch-automatic-item-2",
        capability: "POSTER",
        input_fingerprint: second_fingerprint,
        is_missing: true,
        checked_at: 43,
    }];
    let second_automatic_dispatch = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &second_results,
            &["dispatch-automatic-item-2".to_owned()],
        )
        .await?;
    assert_eq!(second_automatic_dispatch.scheduled_job_ids.len(), 1);
    assert_eq!(
        second_automatic_dispatch.scheduled_job_ids[0],
        automatic_job_id
    );

    fail_fill_missing_job_with_unavailable_provider(
        &database,
        &explicit_job_id,
        "dispatch-explicit-item",
    )
    .await?;

    let explicit_fingerprint = b"must-not-rewrite-explicit-request";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                "dispatch-explicit-item",
                "POSTER",
                explicit_fingerprint,
            )
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                "dispatch-explicit-item",
                "POSTER",
                explicit_fingerprint,
            )
            .await?
    );
    let explicit_results = [NewItemMetadataCompletenessResult {
        item_id: "dispatch-explicit-item",
        capability: "POSTER",
        input_fingerprint: explicit_fingerprint,
        is_missing: true,
        checked_at: 44,
    }];
    let explicit_item_auto_replay = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &explicit_results,
            &["dispatch-explicit-item".to_owned()],
        )
        .await?;
    assert!(explicit_item_auto_replay.scheduled_job_ids.is_empty());
    let explicit_item_fingerprint: Option<Vec<u8>> = database
        .query_scalar(
            "SELECT request_fingerprint FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = 'dispatch-explicit-item'",
        )
        .bind(&explicit_job_id)
        .fetch_one(database.pool())
        .await?;
    assert!(explicit_item_fingerprint.is_none());
    let explicit_item_retry_consumed: i64 = database
        .query_scalar(
            "SELECT automatic_retry_consumed FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = 'dispatch-explicit-item'",
        )
        .bind(&explicit_job_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(explicit_item_retry_consumed, 0);

    let explicit_after_automatic_job_id = database
        .create_or_merge_fill_missing_job(
            &library_id,
            &["dispatch-explicit-after-auto-item".to_owned()],
        )
        .await?;
    assert_ne!(explicit_after_automatic_job_id, automatic_job_id);
    assert_ne!(explicit_after_automatic_job_id, explicit_job_id);

    let dispatch_rows: Vec<(String, String, i64)> = database
        .query_as(
            "SELECT id, dispatch_source, total_count
             FROM metadata_reidentify_jobs
             WHERE library_id = ? AND mode = 'FILL_MISSING'
             ORDER BY id",
        )
        .bind(&library_id)
        .fetch_all(database.pool())
        .await?;
    let mut expected_dispatch_rows = vec![
        (explicit_job_id.clone(), "EXPLICIT".to_owned(), 1),
        (automatic_job_id.clone(), "AUTOMATIC".to_owned(), 2),
        (explicit_after_automatic_job_id, "EXPLICIT".to_owned(), 1),
    ];
    expected_dispatch_rows.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(dispatch_rows, expected_dispatch_rows);

    database
        .finish_metadata_reidentify_job(&automatic_job_id, "FAILED", Some("TEST_FAILURE"))
        .await?;
    assert!(
        database
            .retry_metadata_reidentify_job(&automatic_job_id)
            .await?
    );
    let retried_source: String = database
        .query_scalar("SELECT dispatch_source FROM metadata_reidentify_jobs WHERE id = ?")
        .bind(&automatic_job_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(retried_source, "EXPLICIT");

    database.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_automatic_and_explicit_fill_missing_jobs_keep_dispatch_sources_separate()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_url = DatabaseConfiguration::Postgres(admin_connection.clone())
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(
        &config,
        &DatabaseConfiguration::Postgres(PostgresConnection {
            database: database_name.clone(),
            ..admin_connection
        }),
    )
    .await?;

    let assertions = async {
        let library = LibraryService::new(database.clone())
            .create_library("Postgres dispatch sources", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        for item_id in ["postgres-dispatch-explicit", "postgres-dispatch-automatic"] {
            database
                .query(
                    "INSERT INTO media_items (
                        id, library_id, item_type, title, sort_title, identification_status
                     ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
                )
                .bind(item_id)
                .bind(&library_id)
                .bind(item_id)
                .bind(item_id)
                .execute(database.pool())
                .await?;
        }

        let explicit_job_id = database
            .create_or_merge_fill_missing_job(
                &library_id,
                &["postgres-dispatch-explicit".to_owned()],
            )
            .await?;
        let fingerprint = b"postgres-automatic-dispatch";
        assert!(
            database
                .prepare_item_metadata_completeness_check(
                    "postgres-dispatch-automatic",
                    "POSTER",
                    fingerprint,
                )
                .await?
        );
        assert!(
            database
                .claim_item_metadata_completeness_check(
                    "postgres-dispatch-automatic",
                    "POSTER",
                    fingerprint,
                )
                .await?
        );
        let results = [NewItemMetadataCompletenessResult {
            item_id: "postgres-dispatch-automatic",
            capability: "POSTER",
            input_fingerprint: fingerprint,
            is_missing: true,
            checked_at: 42,
        }];
        let automatic_dispatch = database
            .complete_local_metadata_and_enqueue_fill_missing(
                &library_id,
                &results,
                &["postgres-dispatch-automatic".to_owned()],
            )
            .await?;
        let automatic_job_id = automatic_dispatch
            .scheduled_job_ids
            .first()
            .ok_or("expected automatic fill-missing job")?;
        assert_ne!(automatic_job_id, &explicit_job_id);

        let dispatch_rows: Vec<(String, String)> = database
            .query_as(
                "SELECT id, dispatch_source FROM metadata_reidentify_jobs
                 WHERE library_id = ? AND mode = 'FILL_MISSING' ORDER BY id",
            )
            .bind(&library_id)
            .fetch_all(database.pool())
            .await?;
        assert_eq!(dispatch_rows.len(), 2);
        assert!(dispatch_rows.contains(&(explicit_job_id, "EXPLICIT".to_owned())));
        assert!(dispatch_rows.contains(&(automatic_job_id.clone(), "AUTOMATIC".to_owned())));
        assert_eq!(
            database
                .list_queued_automatic_fill_missing_job_ids(&library_id, None, None, 10)
                .await?,
            vec![(
                automatic_job_id.clone(),
                database
                    .query_scalar("SELECT created_at FROM metadata_reidentify_jobs WHERE id = ?")
                    .bind(automatic_job_id)
                    .fetch_one(database.pool())
                    .await?
            )]
        );
        assert_eq!(
            database
                .list_queued_automatic_fill_missing_library_ids(None, 10)
                .await?,
            vec![library_id.clone()]
        );

        database
            .finish_metadata_reidentify_job(automatic_job_id, "FAILED", Some("TEST_FAILURE"))
            .await?;
        assert!(
            database
                .retry_metadata_reidentify_job(automatic_job_id)
                .await?
        );
        let retried_source: String = database
            .query_scalar("SELECT dispatch_source FROM metadata_reidentify_jobs WHERE id = ?")
            .bind(automatic_job_id)
            .fetch_one(database.pool())
            .await?;
        assert_eq!(retried_source, "EXPLICIT");
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    assertions?;
    drop_database?;
    Ok(())
}

#[tokio::test]
async fn fill_missing_job_creation_reuses_later_queued_capacity()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Fill missing queued capacity", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let item_ids = (0..331)
        .map(|index| format!("queue-capacity-item-{index:03}"))
        .collect::<Vec<_>>();
    for item_id in &item_ids {
        database
            .query(
                "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
    }

    database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[..100])
        .await?;
    database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[100..200])
        .await?;
    database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[200..250])
        .await?;
    let initial_jobs: Vec<(String, i64)> = database
        .query_as(
            "SELECT id, total_count FROM metadata_reidentify_jobs
             WHERE library_id = ? AND mode = 'FILL_MISSING'
             ORDER BY created_at, id",
        )
        .bind(&library_id)
        .fetch_all(database.pool())
        .await?;
    assert_eq!(
        initial_jobs
            .iter()
            .map(|(_, count)| *count)
            .collect::<Vec<_>>(),
        vec![100, 100, 50]
    );

    let reused_job_id = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[250..251])
        .await?;
    assert_eq!(reused_job_id, initial_jobs[2].0);

    let jobs: Vec<(String, i64)> = database
        .query_as(
            "SELECT id, total_count FROM metadata_reidentify_jobs
             WHERE library_id = ? AND mode = 'FILL_MISSING'
             ORDER BY created_at, id",
        )
        .bind(&library_id)
        .fetch_all(database.pool())
        .await?;
    assert_eq!(
        jobs.len(),
        3,
        "an available queued slot must prevent a new job"
    );
    assert_eq!(
        jobs.iter().map(|(_, count)| *count).collect::<Vec<_>>(),
        vec![100, 100, 51]
    );

    let first_reused_job_id = database
        .create_or_merge_fill_missing_job(&library_id, &item_ids[251..])
        .await?;
    assert_eq!(first_reused_job_id, initial_jobs[2].0);
    let jobs: Vec<(String, i64)> = database
        .query_as(
            "SELECT id, total_count FROM metadata_reidentify_jobs
             WHERE library_id = ? AND mode = 'FILL_MISSING'
             ORDER BY created_at, id",
        )
        .bind(&library_id)
        .fetch_all(database.pool())
        .await?;
    assert_eq!(jobs.len(), 4);
    assert_eq!(
        jobs.iter().map(|(_, count)| *count).collect::<Vec<_>>(),
        vec![100, 100, 100, 31]
    );
    let queued_items: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM metadata_reidentify_job_items
             WHERE job_id IN (
                 SELECT id FROM metadata_reidentify_jobs
                 WHERE library_id = ? AND mode = 'FILL_MISSING'
             )",
        )
        .bind(&library_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(queued_items, 331);
    Ok(())
}

#[tokio::test]
async fn legacy_fill_missing_job_keeps_empty_request_snapshot_defaults()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Snapshot migration", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let item_id = "legacy-fill-item";
    database
        .query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    let job_id = database
        .create_or_merge_fill_missing_job(&library_id, &[item_id.to_owned()])
        .await?;
    let snapshot: (Option<Vec<u8>>, String, Option<Vec<u8>>, String) = database
        .query_as(
            "SELECT request_fingerprint, request_capabilities_json,
                    claimed_request_fingerprint, claimed_request_capabilities_json
             FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(job_id)
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(snapshot, (None, "[]".to_owned(), None, "[]".to_owned()));
    Ok(())
}

#[tokio::test]
async fn changed_local_fill_request_during_running_item_is_not_lost()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Running Request Movie (2025)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Running.Request.Movie.2025.mkv"), b"video").await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Running fill request", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("media root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = database
        .query_scalar(
            "SELECT id FROM media_items
             WHERE library_id = ? AND item_type = 'MOVIE' ORDER BY id LIMIT 1",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?;
    let library_id = library.id.to_string();

    let first_fingerprint = b"fill-request-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", first_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", first_fingerprint)
            .await?
    );
    let first_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: first_fingerprint,
        is_missing: true,
        checked_at: 10,
    }];
    let first_commit = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &first_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert_eq!(first_commit.scheduled_job_ids.len(), 1);
    let job_id = &first_commit.scheduled_job_ids[0];
    sqlx::query(
        "CREATE TABLE fill_request_write_probe (
             item_inserts INTEGER NOT NULL DEFAULT 0,
             item_updates INTEGER NOT NULL DEFAULT 0,
             job_inserts INTEGER NOT NULL DEFAULT 0
         );
         INSERT INTO fill_request_write_probe DEFAULT VALUES;
         CREATE TRIGGER fill_request_item_insert_probe AFTER INSERT
         ON metadata_reidentify_job_items
         BEGIN
             UPDATE fill_request_write_probe SET item_inserts = item_inserts + 1;
         END;
         CREATE TRIGGER fill_request_item_update_probe AFTER UPDATE
         ON metadata_reidentify_job_items
         BEGIN
             UPDATE fill_request_write_probe SET item_updates = item_updates + 1;
         END;
         CREATE TRIGGER fill_request_job_insert_probe AFTER INSERT
         ON metadata_reidentify_jobs
         BEGIN
             UPDATE fill_request_write_probe SET job_inserts = job_inserts + 1;
         END;",
    )
    .execute(database.pool())
    .await?;
    let repeated_request = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(repeated_request.scheduled_job_ids.is_empty());
    let repeated_writes: (i64, i64, i64) = database
        .query_as("SELECT item_inserts, item_updates, job_inserts FROM fill_request_write_probe")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(repeated_writes, (0, 0, 0), "same input performs no job DML");

    let queued_fingerprint = b"fill-request-v2";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", queued_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", queued_fingerprint)
            .await?
    );
    let queued_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: queued_fingerprint,
        is_missing: true,
        checked_at: 11,
    }];
    let queued_commit = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &queued_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(queued_commit.scheduled_job_ids.is_empty());
    let changed_queued_writes: (i64, i64, i64) = database
        .query_as("SELECT item_inserts, item_updates, job_inserts FROM fill_request_write_probe")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        changed_queued_writes,
        (0, 1, 0),
        "queued input changes update the existing job item once"
    );
    let queued_snapshot: Vec<u8> = database
        .query_scalar(
            "SELECT request_fingerprint FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(job_id)
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(queued_snapshot, queued_fingerprint);

    assert!(database.claim_metadata_reidentify_job(job_id).await?);
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(job_id, 1)
            .await?,
        vec![item_id.clone()]
    );
    let same_running_request = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &[],
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(same_running_request.scheduled_job_ids.is_empty());

    let second_fingerprint = b"fill-request-v3";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", second_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", second_fingerprint)
            .await?
    );
    let second_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: second_fingerprint,
        is_missing: true,
        checked_at: 12,
    }];
    let second_commit = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &second_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(second_commit.scheduled_job_ids.is_empty());

    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "CREDITS", second_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "CREDITS", second_fingerprint)
            .await?
    );
    let added_capability_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "CREDITS",
        input_fingerprint: second_fingerprint,
        is_missing: true,
        checked_at: 13,
    }];
    let changed_capability_commit = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &added_capability_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(changed_capability_commit.scheduled_job_ids.is_empty());
    let latest_capabilities: String = database
        .query_scalar(
            "SELECT request_capabilities_json FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(job_id)
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(latest_capabilities, "[\"CREDITS\",\"POSTER\"]");

    database
        .finish_metadata_reidentify_item(job_id, &item_id, "COMPLETED", 1, None)
        .await?;
    assert_eq!(
        database.next_metadata_reidentify_item(job_id).await?,
        Some(item_id.clone()),
        "the changed request must be claimable after the active item finishes"
    );
    let processed_count: i64 = database
        .query_scalar("SELECT processed_count FROM metadata_reidentify_jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(processed_count, 0, "a requeued item is not yet processed");

    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(job_id, 1)
            .await?,
        vec![item_id.clone()]
    );
    assert_eq!(
        database
            .fail_running_metadata_reidentify_items(job_id, "WORKER_FAILED")
            .await?,
        1
    );
    database
        .finish_metadata_reidentify_job(job_id, "FAILED", Some("ITEM_FAILED"))
        .await?;
    assert!(database.retry_metadata_reidentify_job(job_id).await?);
    assert!(database.claim_metadata_reidentify_job(job_id).await?);
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(job_id, 1)
            .await?,
        vec![item_id.clone()]
    );
    let claimed_after_worker_retry: Vec<u8> = database
        .query_scalar(
            "SELECT claimed_request_fingerprint FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(job_id)
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(claimed_after_worker_retry, second_fingerprint);
    database
        .finish_metadata_reidentify_item(job_id, &item_id, "COMPLETED", 1, None)
        .await?;
    assert_eq!(database.next_metadata_reidentify_item(job_id).await?, None);
    let completed_count: i64 = database
        .query_scalar("SELECT processed_count FROM metadata_reidentify_jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(completed_count, 1, "only the final pass is counted");

    let completed_item_new_fingerprint = b"fill-request-completed-new-input";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                completed_item_new_fingerprint,
            )
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                completed_item_new_fingerprint,
            )
            .await?
    );
    let completed_item_new_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: completed_item_new_fingerprint,
        is_missing: true,
        checked_at: 12,
    }];
    let completed_item_new_commit = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &completed_item_new_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert_eq!(completed_item_new_commit.scheduled_job_ids.len(), 1);
    let completed_item_new_job_id = &completed_item_new_commit.scheduled_job_ids[0];
    assert_ne!(completed_item_new_job_id, job_id);
    let original_job_snapshot: Vec<u8> = database
        .query_scalar(
            "SELECT request_fingerprint FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(job_id)
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(original_job_snapshot, second_fingerprint);
    assert_eq!(
        database.next_metadata_reidentify_item(job_id).await?,
        None,
        "automatic completeness does not reopen a completed explicit job"
    );
    let completed_processed_count: i64 = database
        .query_scalar("SELECT processed_count FROM metadata_reidentify_jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(completed_processed_count, 1);
    assert!(
        database
            .claim_metadata_reidentify_job(completed_item_new_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(completed_item_new_job_id, 1)
            .await?,
        vec![item_id.clone()]
    );

    let cancelled_fingerprint = b"fill-request-v4";
    assert!(
        database
            .prepare_item_metadata_completeness_check(&item_id, "POSTER", cancelled_fingerprint)
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(&item_id, "POSTER", cancelled_fingerprint)
            .await?
    );
    let cancelled_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: cancelled_fingerprint,
        is_missing: true,
        checked_at: 13,
    }];
    let cancelled_commit = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &cancelled_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(cancelled_commit.scheduled_job_ids.is_empty());
    let cancelled_job_id = completed_item_new_job_id;
    let newer_cancelled_fingerprint = b"fill-request-v5";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                &item_id,
                "POSTER",
                newer_cancelled_fingerprint,
            )
            .await?
    );
    assert!(database
        .claim_item_metadata_completeness_check(
            &item_id,
            "POSTER",
            newer_cancelled_fingerprint,
        )
        .await?);
    let newer_cancelled_result = [NewItemMetadataCompletenessResult {
        item_id: &item_id,
        capability: "POSTER",
        input_fingerprint: newer_cancelled_fingerprint,
        is_missing: true,
        checked_at: 14,
    }];
    database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &newer_cancelled_result,
            std::slice::from_ref(&item_id),
        )
        .await?;
    assert!(
        database
            .request_metadata_reidentify_job_cancel(cancelled_job_id)
            .await?
    );
    database
        .finish_metadata_reidentify_item(cancelled_job_id, &item_id, "COMPLETED", 1, None)
        .await?;
    assert_eq!(
        database
            .next_metadata_reidentify_item(cancelled_job_id)
            .await?,
        None,
        "a cancellation request must prevent a follow-up pass"
    );

    let worker_error_item_id = "fill-request-worker-error-item";
    database
        .query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Worker Error', 'worker error', 'LOCAL_CONFIRMED')",
        )
        .bind(worker_error_item_id)
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    let worker_first_fingerprint = b"worker-input-v1";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                worker_error_item_id,
                "POSTER",
                worker_first_fingerprint,
            )
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                worker_error_item_id,
                "POSTER",
                worker_first_fingerprint,
            )
            .await?
    );
    let worker_first_result = [NewItemMetadataCompletenessResult {
        item_id: worker_error_item_id,
        capability: "POSTER",
        input_fingerprint: worker_first_fingerprint,
        is_missing: true,
        checked_at: 15,
    }];
    let worker_first_commit = database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &worker_first_result,
            &[worker_error_item_id.into()],
        )
        .await?;
    let worker_job_id = &worker_first_commit.scheduled_job_ids[0];
    assert!(
        database
            .claim_metadata_reidentify_job(worker_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(worker_job_id, 1)
            .await?,
        vec![worker_error_item_id]
    );
    let worker_latest_fingerprint = b"worker-input-v2";
    assert!(
        database
            .prepare_item_metadata_completeness_check(
                worker_error_item_id,
                "POSTER",
                worker_latest_fingerprint,
            )
            .await?
    );
    assert!(
        database
            .claim_item_metadata_completeness_check(
                worker_error_item_id,
                "POSTER",
                worker_latest_fingerprint,
            )
            .await?
    );
    let worker_latest_result = [NewItemMetadataCompletenessResult {
        item_id: worker_error_item_id,
        capability: "POSTER",
        input_fingerprint: worker_latest_fingerprint,
        is_missing: true,
        checked_at: 16,
    }];
    database
        .complete_local_metadata_and_enqueue_fill_missing(
            &library_id,
            &worker_latest_result,
            &[worker_error_item_id.into()],
        )
        .await?;
    assert_eq!(
        database
            .fail_running_metadata_reidentify_items(worker_job_id, "WORKER_FAILED")
            .await?,
        1
    );
    let latest_after_failure: Vec<u8> = database
        .query_scalar(
            "SELECT request_fingerprint FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(worker_job_id)
        .bind(worker_error_item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(latest_after_failure, worker_latest_fingerprint);
    database
        .finish_metadata_reidentify_job(worker_job_id, "FAILED", Some("ITEM_FAILED"))
        .await?;
    assert!(
        database
            .retry_metadata_reidentify_job(worker_job_id)
            .await?
    );
    assert!(
        database
            .claim_metadata_reidentify_job(worker_job_id)
            .await?
    );
    assert_eq!(
        database
            .claim_next_metadata_reidentify_items(worker_job_id, 1)
            .await?,
        vec![worker_error_item_id]
    );
    let claimed_after_retry: Vec<u8> = database
        .query_scalar(
            "SELECT claimed_request_fingerprint FROM metadata_reidentify_job_items
             WHERE job_id = ? AND item_id = ?",
        )
        .bind(worker_job_id)
        .bind(worker_error_item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(claimed_after_retry, worker_latest_fingerprint);
    Ok(())
}

#[tokio::test]
async fn person_credit_page_replacement_commits_all_items_atomically() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("People", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    for item_id in ["credit-page-a", "credit-page-b"] {
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(item_id)
        .bind(item_id)
        .execute(database.pool())
        .await
        .expect("media item");
    }

    let credit = |person_id: &str| NewPersonCredit {
        person_id: person_id.to_owned(),
        lux_person_id: None,
        person_type: "Actor".to_owned(),
        person_name: person_id.to_owned(),
        provider: "tmdb".to_owned(),
        role: "Actor".to_owned(),
        sort_order: 0,
        biography: None,
        birthday: None,
        deathday: None,
        known_for_department: None,
        place_of_birth: None,
        provider_ids: std::collections::BTreeMap::new(),
        genres: Vec::new(),
        tags: Vec::new(),
        production_locations: Vec::new(),
        premiere_date: None,
        production_year: None,
        taglines: Vec::new(),
    };
    let credits_a = [credit("actor-a")];
    let credits_b = [credit("actor-b")];
    let replacements = [
        (
            "credit-page-a",
            credits_a.as_slice(),
            Some("fingerprint-a"),
            Some("relation-a"),
        ),
        (
            "credit-page-b",
            credits_b.as_slice(),
            Some("fingerprint-b"),
            Some("relation-b"),
        ),
    ];

    sqlx::query(
        "CREATE TRIGGER fail_second_person_credit_page_state
         BEFORE INSERT ON person_index_item_state
         WHEN NEW.item_id = 'credit-page-b'
         BEGIN SELECT RAISE(ABORT, 'forced second item failure'); END",
    )
    .execute(database.pool())
    .await
    .expect("failure trigger");
    assert!(
        database
            .replace_person_credits_batch_with_relation_checksum(&replacements)
            .await
            .is_err()
    );
    let row_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM person_credits
         WHERE item_id IN ('credit-page-a', 'credit-page-b')",
    )
    .fetch_one(database.pool())
    .await
    .expect("credits after rollback");
    assert_eq!(row_count, 0, "the failed page must roll back both items");
    let state_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM person_index_item_state
         WHERE item_id IN ('credit-page-a', 'credit-page-b')",
    )
    .fetch_one(database.pool())
    .await
    .expect("states after rollback");
    assert_eq!(state_count, 0, "the failed page must roll back both states");

    sqlx::query("DROP TRIGGER fail_second_person_credit_page_state")
        .execute(database.pool())
        .await
        .expect("drop failure trigger");
    database
        .replace_person_credits_batch_with_relation_checksum(&replacements)
        .await
        .expect("replace whole page");
    let persisted_checksums: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT item_id, relation_checksum FROM person_index_item_state
         WHERE item_id IN ('credit-page-a', 'credit-page-b') ORDER BY item_id",
    )
    .fetch_all(database.pool())
    .await
    .expect("persisted page checksums");
    assert_eq!(
        persisted_checksums,
        vec![
            ("credit-page-a".to_owned(), Some("relation-a".to_owned())),
            ("credit-page-b".to_owned(), Some("relation-b".to_owned())),
        ]
    );
    let stored_credit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM person_credits
         WHERE item_id IN ('credit-page-a', 'credit-page-b')",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored page credits");
    assert_eq!(stored_credit_count, 2);
}

#[tokio::test]
async fn person_manifest_restore_pending_skips_unchanged_state_updates() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    database
        .mark_person_manifest_restore_pending(3)
        .await
        .expect("mark restore pending");
    sqlx::query(
        "CREATE TABLE person_manifest_restore_update_probe (count INTEGER NOT NULL);
         INSERT INTO person_manifest_restore_update_probe (count) VALUES (0);
         CREATE TRIGGER person_manifest_restore_update_probe_trigger
         AFTER UPDATE ON person_manifest_restore_state
         BEGIN
             UPDATE person_manifest_restore_update_probe SET count = count + 1;
         END",
    )
    .execute(database.pool())
    .await
    .expect("update probe");

    database
        .mark_person_manifest_restore_pending(3)
        .await
        .expect("repeat current pending state");
    let update_count: i64 =
        sqlx::query_scalar("SELECT count FROM person_manifest_restore_update_probe")
            .fetch_one(database.pool())
            .await
            .expect("probe after duplicate pending state");
    assert_eq!(update_count, 0);

    database
        .mark_person_manifest_restore_completed(3)
        .await
        .expect("complete restore");
    database
        .mark_person_manifest_restore_pending(3)
        .await
        .expect("requeue after completion");
    database
        .mark_person_manifest_restore_pending(4)
        .await
        .expect("requeue for a new schema");
    let update_count: i64 =
        sqlx::query_scalar("SELECT count FROM person_manifest_restore_update_probe")
            .fetch_one(database.pool())
            .await
            .expect("probe after meaningful changes");
    assert_eq!(update_count, 3);
    let (status, schema_version): (String, i64) = sqlx::query_as(
        "SELECT status, schema_version FROM person_manifest_restore_state WHERE id = 1",
    )
    .fetch_one(database.pool())
    .await
    .expect("restore state");
    assert_eq!((status.as_str(), schema_version), ("PENDING", 4));
}

#[tokio::test]
async fn person_credit_refresh_preserves_unchanged_rows() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    let library = LibraryService::new(database.clone())
        .create_library("People", LibraryKind::Movie, false)
        .await
        .expect("library");
    let library_id = library.id.to_string();
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('credit-refresh-item', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await
    .expect("media item");

    let credit = |person_id: &str, name: &str, role: &str| NewPersonCredit {
        person_id: person_id.to_owned(),
        lux_person_id: None,
        person_type: "Actor".to_owned(),
        person_name: name.to_owned(),
        provider: "tmdb".to_owned(),
        role: role.to_owned(),
        sort_order: 0,
        biography: None,
        birthday: None,
        deathday: None,
        known_for_department: None,
        place_of_birth: None,
        provider_ids: BTreeMap::new(),
        genres: Vec::new(),
        tags: Vec::new(),
        production_locations: Vec::new(),
        premiere_date: None,
        production_year: None,
        taglines: Vec::new(),
    };
    let initial = vec![
        credit("person-1", "Actor One", "Lead"),
        credit("person-2", "Actor Two", "Friend"),
    ];
    database
        .replace_person_credits("credit-refresh-item", &initial)
        .await
        .expect("initial credits");
    let unchanged_row_id: i64 = sqlx::query_scalar(
        "SELECT rowid FROM person_credits
         WHERE item_id = 'credit-refresh-item' AND person_id = 'person-1'",
    )
    .fetch_one(database.pool())
    .await
    .expect("initial row");
    sqlx::query(
        "CREATE TABLE person_credit_update_probe (count INTEGER NOT NULL);
         INSERT INTO person_credit_update_probe (count) VALUES (0);
         CREATE TRIGGER person_credit_update_probe_trigger
         AFTER UPDATE ON person_credits
         BEGIN
             UPDATE person_credit_update_probe SET count = count + 1;
         END;",
    )
    .execute(database.pool())
    .await
    .expect("update probe");

    database
        .replace_person_credits("credit-refresh-item", &initial)
        .await
        .expect("unchanged credits");
    let same_row_id: i64 = sqlx::query_scalar(
        "SELECT rowid FROM person_credits
         WHERE item_id = 'credit-refresh-item' AND person_id = 'person-1'",
    )
    .fetch_one(database.pool())
    .await
    .expect("unchanged row");
    assert_eq!(same_row_id, unchanged_row_id);
    let unchanged_updates: i64 = sqlx::query_scalar("SELECT count FROM person_credit_update_probe")
        .fetch_one(database.pool())
        .await
        .expect("unchanged update count");
    assert_eq!(unchanged_updates, 0);

    let mut refreshed_credit = credit("person-1", "Actor One Updated", "Lead");
    refreshed_credit.lux_person_id = Some("lux-person-1".to_owned());
    let refreshed = vec![refreshed_credit, credit("person-3", "Actor Three", "New")];
    database
        .replace_person_credits("credit-refresh-item", &refreshed)
        .await
        .expect("changed credits");
    let changed_credit: (i64, String, Option<String>) = sqlx::query_as(
        "SELECT rowid, person_name, lux_person_id FROM person_credits
         WHERE item_id = 'credit-refresh-item' AND person_id = 'person-1'",
    )
    .fetch_one(database.pool())
    .await
    .expect("changed row");
    assert_eq!(changed_credit.0, unchanged_row_id);
    assert_eq!(changed_credit.1, "Actor One Updated");
    assert_eq!(changed_credit.2.as_deref(), Some("lux-person-1"));
    let changed_updates: i64 = sqlx::query_scalar("SELECT count FROM person_credit_update_probe")
        .fetch_one(database.pool())
        .await
        .expect("changed update count");
    assert_eq!(changed_updates, 1);
    let remaining_people: Vec<String> = sqlx::query_scalar(
        "SELECT person_id FROM person_credits
         WHERE item_id = 'credit-refresh-item' ORDER BY person_id",
    )
    .fetch_all(database.pool())
    .await
    .expect("remaining credits");
    assert_eq!(remaining_people, ["person-1", "person-3"]);
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_merges_movie_items_and_moves_every_source_to_the_primary()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration =
        crate::config::DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(
        &config,
        &crate::config::DatabaseConfiguration::Postgres(PostgresConnection {
            database: database_name.clone(),
            ..admin_connection
        }),
    )
    .await?;

    let assertions = async {
        let library = LibraryService::new(database.clone())
            .create_library("Merge", LibraryKind::Movie, false)
            .await?;
        for (item_id, title, source_id, is_default) in [
            ("merge-primary", "Movie", "merge-source-1", 1),
            ("merge-secondary", "Movie Part 2", "merge-source-2", 1),
        ] {
            database
                .query(
                    "INSERT INTO media_items (
                        id, library_id, item_type, title, sort_title, identification_status,
                        has_available_source
                     ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1)",
                )
                .bind(item_id)
                .bind(library.id.to_string())
                .bind(title)
                .bind(title.to_lowercase())
                .execute(database.pool())
                .await?;
            database
                .query(
                    "INSERT INTO media_sources (id, item_id, source_kind, is_default, probe_status)
                     VALUES (?, ?, 'LOCAL_FILE', ?, 'PENDING')",
                )
                .bind(source_id)
                .bind(item_id)
                .bind(is_default)
                .execute(database.pool())
                .await?;
        }
        let item_ids = vec!["merge-primary".to_owned(), "merge-secondary".to_owned()];
        let merged = database
            .merge_media_items("merge-primary", &item_ids)
            .await?;
        assert_eq!(merged.merged_item_ids, vec!["merge-secondary".to_owned()]);
        let sources: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM media_sources WHERE item_id = $1")
                .bind("merge-primary")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(sources, 2);
        let defaults: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM media_sources WHERE item_id = $1 AND is_default = 1",
        )
        .bind("merge-primary")
        .fetch_one(database.pool())
        .await?;
        assert_eq!(defaults, 1, "exactly one default source after the merge");
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    assertions?;
    drop_database?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_metadata_update_persists_and_skips_unchanged_ratings()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration =
        crate::config::DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(
        &config,
        &crate::config::DatabaseConfiguration::Postgres(PostgresConnection {
            database: database_name.clone(),
            ..admin_connection
        }),
    )
    .await?;

    let assertions = async {
        let library = LibraryService::new(database.clone())
            .create_library("Rating", LibraryKind::Movie, false)
            .await?;
        database
            .query(
                "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status,
                    has_available_source
                 ) VALUES ('rating-item', ?, 'MOVIE', 'Old', 'old', 'LOCAL_CONFIRMED', 1)",
            )
            .bind(library.id.to_string())
            .execute(database.pool())
            .await?;
        let update = |title: &'static str, rating: Option<f64>| MediaMetadataUpdate {
            item_id: "rating-item",
            title,
            original_title: None,
            overview: None,
            production_year: None,
            premiere_date: None,
            rating,
            rating_source: Some("nfo"),
            provider_ids_json: None,
            metadata_fingerprint: b"fingerprint",
            provenance_json: "{}",
            locked_fields_json: "[]",
        };
        let rating = || async {
            sqlx::query_scalar::<_, Option<f64>>("SELECT rating FROM media_items WHERE id = $1")
                .bind("rating-item")
                .fetch_one(database.pool())
                .await
        };

        // Only the rating differs from the stored row: the unchanged-write guard must still see it.
        database
            .update_media_item_metadata(update("Old", None))
            .await?;
        assert_eq!(rating().await?, None);
        database
            .update_media_item_metadata(update("Old", Some(8.2)))
            .await?;
        assert_eq!(rating().await?, Some(8.2));
        // Same values again, then a missing rating must not erase the stored one.
        database
            .update_media_item_metadata(update("Old", Some(8.2)))
            .await?;
        database
            .update_media_item_metadata(update("Old", None))
            .await?;
        assert_eq!(rating().await?, Some(8.2));
        database
            .update_media_item_metadata(update("Old", Some(9.1)))
            .await?;
        assert_eq!(rating().await?, Some(9.1));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    assertions?;
    drop_database?;
    Ok(())
}

fn strm_file(index: i64, relative_path: &str) -> NewMovieFile {
    NewMovieFile {
        filesystem_entry_id: format!("scope-entry-{index}"),
        source_id: format!("scope-source-{index}"),
        relative_path: relative_path.to_owned(),
        size: 1,
        modified_at: index,
        device: None,
        inode: None,
        fingerprint: vec![index as u8],
        title: format!("Scope {index}"),
        sort_title: format!("scope {index}"),
        original_title: format!("Scope {index}"),
        production_year: Some(2024),
        provider_ids_json: None,
        source_kind: "STRM_URL".to_owned(),
        strm_target_kind: Some("PATH".to_owned()),
        edition_name: None,
        quality_label: None,
        container: "strm".to_owned(),
        external_url: Some(format!("/cloud/scope-{index}.mp4")),
    }
}

/// STRM sources selected for an incremental scan job must be exactly those under the scanned
/// paths: a directory (not a sibling that merely shares its prefix), an exact file, or the root.
async fn assert_incremental_scan_scope(database: &Database, temp_dir: &std::path::Path) {
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Scope", LibraryKind::Movie, false)
        .await
        .expect("library");
    let root_path = temp_dir.join("media");
    tokio::fs::create_dir_all(&root_path)
        .await
        .expect("media root");
    let root = libraries
        .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
        .await
        .expect("library root")
        .root;
    database
        .insert_movie_files_batch(
            &library.id.to_string(),
            &root.id.to_string(),
            "generation",
            &[
                strm_file(1, "A/Movie.One/Movie.One.strm"),
                strm_file(2, "A/Movie.One/extras/Movie.One.Extra.strm"),
                strm_file(3, "A/Movie.One.2/Movie.One.2.strm"),
                strm_file(4, "B/Movie.Two/Movie.Two.strm"),
                strm_file(5, "Movie.Root.strm"),
            ],
        )
        .await
        .expect("insert");
    // Real scans also record the scanned directories; the count query uses them to tell a
    // directory scope from a plain file scope.
    for (index, directory) in ["A/Movie.One", "B/Movie.Two"].into_iter().enumerate() {
        database
            .query(
                "INSERT INTO filesystem_entries (
                    id, library_root_id, relative_path, entry_kind, size, modified_at,
                    fingerprint, last_seen_generation, is_missing
                 ) VALUES (?, ?, ?, 'DIRECTORY', 0, 0, ?, 'generation', 0)",
            )
            .bind(format!("scope-directory-{index}"))
            .bind(root.id.to_string())
            .bind(directory)
            .bind(vec![0_u8])
            .execute(database.pool())
            .await
            .expect("directory entry");
    }
    let now = 1_700_000_000_i64;
    let jobs = [
        ("scope-dir", vec!["A/Movie.One"]),
        ("scope-file", vec!["B/Movie.Two/Movie.Two.strm"]),
        ("scope-root", vec!["."]),
        ("scope-two", vec!["A/Movie.One", "B/Movie.Two"]),
        ("scope-none", vec!["C/Missing"]),
    ];
    for (job_id, paths) in &jobs {
        database
            .query(
                "INSERT INTO scan_jobs (
                id, library_id, job_type, status, generation, scan_phase, created_at, updated_at
             ) VALUES (?, ?, 'INCREMENTAL_SCAN', 'COMPLETED', ?, 'IDLE', ?, ?)",
            )
            .bind(*job_id)
            .bind(library.id.to_string())
            .bind(format!("generation-{job_id}"))
            .bind(now)
            .bind(now)
            .execute(database.pool())
            .await
            .expect("scan job");
        for path in paths {
            database
                .query(
                "INSERT INTO scan_job_paths (job_id, library_root_id, relative_path, change_kind, processed_at)
                 VALUES (?, ?, ?, 'MODIFY', ?)",
            )
            .bind(*job_id)
            .bind(root.id.to_string())
            .bind(*path)
            .bind(now)
            .execute(database.pool())
            .await
            .expect("scan path");
        }
    }
    for (job_id, expected) in [
        ("scope-dir", vec!["scope-source-1", "scope-source-2"]),
        ("scope-file", vec!["scope-source-4"]),
        (
            "scope-root",
            vec![
                "scope-source-1",
                "scope-source-2",
                "scope-source-3",
                "scope-source-4",
                "scope-source-5",
            ],
        ),
        (
            "scope-two",
            vec!["scope-source-1", "scope-source-2", "scope-source-4"],
        ),
        ("scope-none", vec![]),
    ] {
        let listed = database
            .list_strm_media_sources_for_incremental_scan_page(job_id, None, 100)
            .await
            .expect("list");
        let mut listed = listed
            .into_iter()
            .map(|source| source.source_id)
            .collect::<Vec<_>>();
        listed.sort();
        assert_eq!(listed, expected, "listed sources for {job_id}");
        let counted = database
            .count_strm_media_sources_for_incremental_scan(job_id)
            .await
            .expect("count");
        assert_eq!(
            counted,
            i64::try_from(expected.len()).expect("small"),
            "counted sources for {job_id}"
        );
    }
}

#[tokio::test]
async fn incremental_scan_scope_selects_only_sources_under_the_scanned_paths() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    assert_incremental_scan_scope(&database, temp_dir.path()).await;
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_incremental_scan_scope_selects_only_sources_under_the_scanned_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration =
        crate::config::DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(
        &config,
        &crate::config::DatabaseConfiguration::Postgres(PostgresConnection {
            database: database_name.clone(),
            ..admin_connection
        }),
    )
    .await?;
    let outcome = tokio::spawn({
        let database = database.clone();
        let path = temp_dir.path().to_path_buf();
        async move { assert_incremental_scan_scope(&database, &path).await }
    })
    .await;
    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    outcome?;
    drop_database?;
    Ok(())
}

async fn assert_terminal_local_metadata_batches_are_cleaned(
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Batches", LibraryKind::Movie, false)
        .await?;
    let root_path = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&root_path).await?;
    let root = libraries
        .add_root(library.id, root_path.to_str().ok_or("root path")?)
        .await?
        .root;
    let library_id = library.id.to_string();
    let root_id = root.id.to_string();
    for (job_id, status, phase) in [
        ("done-job", "COMPLETED", "IDLE"),
        ("cancelled-job", "CANCELLED", "IDLE"),
        ("running-job", "RUNNING", "FINALIZING"),
    ] {
        database
            .query(
                "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, scan_phase)
                 VALUES (?, ?, 'RECONCILE_LIBRARY', ?, 'generation', ?)",
            )
            .bind(job_id)
            .bind(&library_id)
            .bind(status)
            .bind(phase)
            .execute(database.pool())
            .await?;
    }
    // (id, job, status)
    let batches = [
        ("done-completed", "done-job", "COMPLETED"),
        ("done-failed", "done-job", "FAILED"),
        ("done-pending", "done-job", "PENDING"),
        ("cancelled-cancelled", "cancelled-job", "CANCELLED"),
        ("orphan-completed", "missing-job", "COMPLETED"),
        ("running-completed", "running-job", "COMPLETED"),
        ("running-pending", "running-job", "PENDING"),
    ];
    for (sequence, (id, job_id, status)) in batches.into_iter().enumerate() {
        database
            .query(
                "INSERT INTO scan_local_metadata_batches
                 (id, job_id, library_root_id, batch_sequence, source_refs_json, source_count,
                  status)
                 VALUES (?, ?, ?, ?, '[\"source\"]', 1, ?)",
            )
            .bind(id)
            .bind(job_id)
            .bind(&root_id)
            .bind(i64::try_from(sequence)?)
            .bind(status)
            .execute(database.pool())
            .await?;
    }

    // A batch that just finished is kept for a grace period; backdate all but one.
    database
        .query(
            "UPDATE scan_local_metadata_batches SET updated_at = unixepoch() - 7200
             WHERE id <> 'cancelled-cancelled'",
        )
        .execute(database.pool())
        .await?;
    let report = database.cleanup_completed_scan_manifest_payloads().await?;
    assert_eq!(report.scan_local_metadata_batches_deleted, 2);
    let remaining: Vec<String> = database
        .query_scalar("SELECT id FROM scan_local_metadata_batches ORDER BY id")
        .fetch_all(database.pool())
        .await?;
    assert_eq!(
        remaining,
        [
            "cancelled-cancelled",
            "done-failed",
            "done-pending",
            "running-completed",
            "running-pending"
        ]
    );
    database
        .query("UPDATE scan_local_metadata_batches SET updated_at = unixepoch() - 7200")
        .execute(database.pool())
        .await?;
    let report = database.cleanup_completed_scan_manifest_payloads().await?;
    assert_eq!(report.scan_local_metadata_batches_deleted, 1);
    Ok(())
}

#[tokio::test]
async fn terminal_local_metadata_batches_are_cleaned_after_the_scan_finishes() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    assert_terminal_local_metadata_batches_are_cleaned(&database)
        .await
        .expect("batch cleanup");
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_terminal_local_metadata_batches_are_cleaned_after_the_scan_finishes()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration =
        crate::config::DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(
        &config,
        &crate::config::DatabaseConfiguration::Postgres(PostgresConnection {
            database: database_name.clone(),
            ..admin_connection
        }),
    )
    .await?;
    let outcome = tokio::spawn({
        let database = database.clone();
        async move {
            assert_terminal_local_metadata_batches_are_cleaned(&database)
                .await
                .map_err(|error| error.to_string())
        }
    })
    .await;
    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    outcome??;
    drop_database?;
    Ok(())
}

#[tokio::test]
async fn user_activity_index_insert_redacts_metadata_and_keeps_independent_subjects()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    let first_id = database
        .insert_user_activity_event(NewUserActivityEvent {
            user_id: "activity-index-subject",
            actor_user_id: Some("activity-index-actor"),
            event_type: "AUTH_LOGIN",
            target_type: Some("media_item"),
            target_id: Some("activity-index-item"),
            metadata_json: r#"{
                "remoteIp":"203.0.113.8",
                "nested":{"accessToken":"do-not-store-this"},
                "requestUrl":"https://private.example/path"
            }"#,
        })
        .await?;
    let second_id = database
        .insert_user_activity_event(NewUserActivityEvent {
            user_id: "activity-index-subject",
            actor_user_id: None,
            event_type: "AUTH_LOGIN",
            target_type: None,
            target_id: None,
            metadata_json: "not json",
        })
        .await?;

    assert_ne!(first_id, second_id);
    let first_row: (
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        String,
        i64,
    ) = sqlx::query_as(
        "SELECT id, user_id, actor_user_id, event_type, target_type, target_id,
                metadata_json, occurred_at
         FROM user_activity_events WHERE id = ?",
    )
    .bind(&first_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(first_row.0, first_id);
    assert_eq!(first_row.1, "activity-index-subject");
    assert_eq!(first_row.2.as_deref(), Some("activity-index-actor"));
    assert_eq!(first_row.3, "AUTH_LOGIN");
    assert_eq!(first_row.4.as_deref(), Some("media_item"));
    assert_eq!(first_row.5.as_deref(), Some("activity-index-item"));
    assert!(first_row.7 > 0);
    assert!(!first_row.6.contains("do-not-store-this"));
    assert!(!first_row.6.contains("private.example"));
    let first_metadata: serde_json::Value = serde_json::from_str(&first_row.6)?;
    assert_eq!(first_metadata["nested"]["accessToken"], "[REDACTED]");
    assert_eq!(first_metadata["requestUrl"], "[REDACTED]");
    assert_eq!(first_metadata["remoteIp"], "203.0.113.8");

    let invalid_metadata: String =
        sqlx::query_scalar("SELECT metadata_json FROM user_activity_events WHERE id = ?")
            .bind(second_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(invalid_metadata, r#"{"invalid":true}"#);
    Ok(())
}

#[tokio::test]
async fn user_device_observations_merge_monotonically_and_are_user_scoped()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp.path().join("config"),
    })
    .await?;
    for (user_id, normalized) in [
        ("device-observation-owner", "device-observation-owner"),
        ("device-observation-other", "device-observation-other"),
    ] {
        database
            .query(
                "INSERT INTO users (id, username_normalized, display_name, password_hash)
                 VALUES (?, ?, ?, 'test-hash')",
            )
            .bind(user_id)
            .bind(normalized)
            .bind(user_id)
            .execute(database.pool())
            .await?;
    }

    database
        .upsert_user_device(NewUserDeviceObservation {
            user_id: "device-observation-owner",
            device_id: "shared-device",
            device_name: Some("Phone"),
            device_type: Some("Mobile"),
            client_name: "Lux Web",
            client_version: Some("1.0"),
            last_remote_ip: Some("198.51.100.1"),
            seen_at: 100,
        })
        .await?;
    database
        .upsert_user_device(NewUserDeviceObservation {
            user_id: "device-observation-owner",
            device_id: "shared-device",
            device_name: Some("Phone Updated"),
            device_type: Some("Mobile"),
            client_name: "Lux Prism",
            client_version: Some("2.0"),
            last_remote_ip: Some("198.51.100.2"),
            seen_at: 200,
        })
        .await?;
    database
        .upsert_user_device(NewUserDeviceObservation {
            user_id: "device-observation-owner",
            device_id: "shared-device",
            device_name: Some("Outdated Name"),
            device_type: Some("Unknown"),
            client_name: "Outdated Client",
            client_version: Some("0.9"),
            last_remote_ip: Some("198.51.100.0"),
            seen_at: 50,
        })
        .await?;
    database
        .upsert_user_device(NewUserDeviceObservation {
            user_id: "device-observation-owner",
            device_id: "shared-device",
            device_name: None,
            device_type: None,
            client_name: "Lux Prism",
            client_version: None,
            last_remote_ip: None,
            seen_at: 250,
        })
        .await?;
    database
        .upsert_user_device(NewUserDeviceObservation {
            user_id: "device-observation-other",
            device_id: "shared-device",
            device_name: Some("Other User Device"),
            device_type: Some("TV"),
            client_name: "Other Client",
            client_version: Some("3.0"),
            last_remote_ip: Some("203.0.113.3"),
            seen_at: 300,
        })
        .await?;

    let owner_record: (
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
         FROM user_devices
         WHERE user_id = 'device-observation-owner' AND device_id = 'shared-device'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(owner_record.0.as_deref(), Some("Phone Updated"));
    assert_eq!(owner_record.1.as_deref(), Some("Mobile"));
    assert_eq!(owner_record.2, "Lux Prism");
    assert_eq!(owner_record.3.as_deref(), Some("2.0"));
    assert_eq!(owner_record.4, 50);
    assert_eq!(owner_record.5, 250);
    assert_eq!(owner_record.6.as_deref(), Some("198.51.100.2"));

    let user_scoped_devices: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_devices WHERE device_id = 'shared-device'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(user_scoped_devices, 2);
    Ok(())
}

#[tokio::test]
async fn user_device_listing_is_scoped_stable_and_bounded() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp.path().join("config"),
    })
    .await?;
    for (user_id, normalized) in [
        ("device-list-owner", "device-list-owner"),
        ("device-list-other", "device-list-other"),
    ] {
        database
            .query(
                "INSERT INTO users (id, username_normalized, display_name, password_hash)
                 VALUES (?, ?, ?, 'test-hash')",
            )
            .bind(user_id)
            .bind(normalized)
            .bind(user_id)
            .execute(database.pool())
            .await?;
    }
    for (user_id, device_id, device_name, client_name, seen_at) in [
        (
            "device-list-owner",
            "device-b",
            Some("Phone B"),
            "Infuse",
            300_i64,
        ),
        (
            "device-list-owner",
            "device-a",
            Some("Phone A"),
            "Lux Web",
            300_i64,
        ),
        ("device-list-owner", "device-c", None, "Kodi", 200_i64),
        (
            "device-list-other",
            "other-device",
            Some("Other User Device"),
            "Other Client",
            999_i64,
        ),
    ] {
        database
            .upsert_user_device(NewUserDeviceObservation {
                user_id,
                device_id,
                device_name,
                device_type: Some("TEST"),
                client_name,
                client_version: Some("1.2.3"),
                last_remote_ip: Some("203.0.113.10"),
                seen_at,
            })
            .await?;
    }
    sqlx::query(
        "INSERT INTO web_sessions (
             id, user_id, session_token_hash, csrf_token_hash, expires_at, device_id
         ) VALUES ('device-a-web-session', 'device-list-owner', X'01', X'11', unixepoch() + 3600, 'device-a'),
                  ('device-c-expired-web-session', 'device-list-owner', X'02', X'12', unixepoch() - 1, 'device-c')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO access_tokens (
             id, token_hash, user_id, device_id, client_name, device_name,
             client_version, created_at, updated_at, revoked_at
         ) VALUES ('device-b-emby-token', X'21', 'device-list-owner', 'device-b', 'Emby', 'Phone B', '1.0', unixepoch(), unixepoch(), NULL),
                  ('device-c-revoked-emby-token', X'22', 'device-list-owner', 'device-c', 'Emby', 'Tablet C', '1.0', unixepoch(), unixepoch(), unixepoch())",
    )
    .execute(database.pool())
    .await?;

    let (first_page, total_count) = database
        .list_user_devices_page(UserDevicePageQuery {
            user_id: "device-list-owner",
            offset: 0,
            limit: 2,
        })
        .await?;
    assert_eq!(total_count, 3);
    assert_eq!(
        first_page
            .iter()
            .map(|device| device.device_id.as_str())
            .collect::<Vec<_>>(),
        vec!["device-a", "device-b"]
    );
    assert_eq!(first_page[0].device_name.as_deref(), Some("Phone A"));
    assert_eq!(first_page[0].device_type.as_deref(), Some("TEST"));
    assert_eq!(first_page[0].client_name, "Lux Web");
    assert_eq!(first_page[0].client_version.as_deref(), Some("1.2.3"));
    assert_eq!(first_page[0].first_seen_at, 300);
    assert_eq!(first_page[0].last_seen_at, 300);
    assert!(first_page[0].has_active_login);
    assert!(first_page[1].has_active_login);
    assert_eq!(
        first_page[0].last_remote_ip.as_deref(),
        Some("203.0.113.10")
    );

    let (second_page, second_total) = database
        .list_user_devices_page(UserDevicePageQuery {
            user_id: "device-list-owner",
            offset: 2,
            limit: 2,
        })
        .await?;
    assert_eq!(second_total, 3);
    assert_eq!(second_page.len(), 1);
    assert_eq!(second_page[0].device_id, "device-c");
    assert!(!second_page[0].has_active_login);

    let (empty_page, empty_total) = database
        .list_user_devices_page(UserDevicePageQuery {
            user_id: "device-list-owner",
            offset: 100,
            limit: 2,
        })
        .await?;
    assert_eq!(empty_total, 3);
    assert!(empty_page.is_empty());

    let (negative_offset_page, _) = database
        .list_user_devices_page(UserDevicePageQuery {
            user_id: "device-list-owner",
            offset: -1,
            limit: 0,
        })
        .await?;
    assert_eq!(negative_offset_page.len(), 1);
    assert_eq!(negative_offset_page[0].device_id, "device-a");

    for index in 0..101_i64 {
        let device_id = format!("overflow-{index:03}");
        database
            .upsert_user_device(NewUserDeviceObservation {
                user_id: "device-list-owner",
                device_id: &device_id,
                device_name: None,
                device_type: Some("TEST"),
                client_name: "Client",
                client_version: None,
                last_remote_ip: None,
                seen_at: 1000 + index,
            })
            .await?;
    }
    let (bounded_page, bounded_total) = database
        .list_user_devices_page(UserDevicePageQuery {
            user_id: "device-list-owner",
            offset: 0,
            limit: 101,
        })
        .await?;
    assert_eq!(bounded_total, 104);
    assert_eq!(bounded_page.len(), 100);
    Ok(())
}

#[tokio::test]
async fn user_activity_index_query_filters_and_pages_in_stable_order()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    for (id, user_id, event_type, occurred_at) in [
        ("login-10", "timeline-user", "AUTH_LOGIN", 10_i64),
        ("pause-20", "timeline-user", "PLAYBACK_PAUSED", 20),
        ("started-20", "timeline-user", "PLAYBACK_STARTED", 20),
        ("before-9", "timeline-user", "AUTH_LOGIN", 9),
        ("other-30", "other-timeline-user", "AUTH_LOGIN", 30),
    ] {
        sqlx::query(
            "INSERT INTO user_activity_events (id, user_id, event_type, occurred_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(user_id)
        .bind(event_type)
        .bind(occurred_at)
        .execute(database.pool())
        .await?;
    }

    let (first_page, total_count) = database
        .list_user_activity_events(UserActivityEventQuery {
            user_id: "timeline-user",
            event_type: None,
            from_at: Some(10),
            to_at: Some(20),
            offset: 0,
            limit: 2,
        })
        .await?;
    assert_eq!(total_count, 3);
    assert_eq!(
        first_page
            .iter()
            .map(|event: &StoredUserActivityEvent| event.id.as_str())
            .collect::<Vec<_>>(),
        vec!["started-20", "pause-20"]
    );
    assert_eq!(first_page[0].user_id, "timeline-user");
    assert_eq!(first_page[0].event_type, "PLAYBACK_STARTED");
    assert_eq!(first_page[0].occurred_at, 20);

    let (second_page, second_total) = database
        .list_user_activity_events(UserActivityEventQuery {
            user_id: "timeline-user",
            event_type: None,
            from_at: Some(10),
            to_at: Some(20),
            offset: 2,
            limit: 2,
        })
        .await?;
    assert_eq!(second_total, 3);
    assert_eq!(second_page.len(), 1);
    assert_eq!(second_page[0].id, "login-10");

    let (playback_events, playback_total) = database
        .list_user_activity_events(UserActivityEventQuery {
            user_id: "timeline-user",
            event_type: Some("PLAYBACK_PAUSED"),
            from_at: None,
            to_at: None,
            offset: 0,
            limit: 100,
        })
        .await?;
    assert_eq!(playback_total, 1);
    assert_eq!(playback_events[0].id, "pause-20");

    for index in 0..101_i64 {
        sqlx::query(
            "INSERT INTO user_activity_events (id, user_id, event_type, occurred_at)
             VALUES (?, 'timeline-user', 'FILLER', ?)",
        )
        .bind(format!("filler-{index:03}"))
        .bind(100 + index)
        .execute(database.pool())
        .await?;
    }
    let (bounded_page, bounded_total) = database
        .list_user_activity_events(UserActivityEventQuery {
            user_id: "timeline-user",
            event_type: None,
            from_at: None,
            to_at: None,
            offset: -1,
            limit: 1_000,
        })
        .await?;
    assert_eq!(bounded_total, 105);
    assert_eq!(bounded_page.len(), 100);
    assert_eq!(bounded_page[0].id, "filler-100");

    let (past_end, past_end_total) = database
        .list_user_activity_events(UserActivityEventQuery {
            user_id: "timeline-user",
            event_type: None,
            from_at: None,
            to_at: None,
            offset: 10_000,
            limit: 20,
        })
        .await?;
    assert!(past_end.is_empty());
    assert_eq!(past_end_total, 105);
    Ok(())
}

#[tokio::test]
async fn user_activity_index_retention_deletes_old_events_in_bounded_batches()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    for (id, occurred_at) in [
        ("retention-old-a", 10_i64),
        ("retention-old-b", 10),
        ("retention-old-c", 99),
        ("retention-boundary", 100),
        ("retention-new", 101),
    ] {
        sqlx::query(
            "INSERT INTO user_activity_events (id, user_id, event_type, occurred_at)
             VALUES (?, 'retention-user', 'AUTH_LOGIN', ?)",
        )
        .bind(id)
        .bind(occurred_at)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO audit_events (id, event_type, metadata_json, created_at)
         VALUES ('retention-audit', 'AUTH_LOGIN', '{}', 1)",
    )
    .execute(database.pool())
    .await?;

    assert_eq!(
        database.delete_user_activity_events_before(100, 2).await?,
        2
    );
    let old_after_first_batch: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_activity_events WHERE occurred_at < 100")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(old_after_first_batch, 1);
    assert_eq!(
        database.delete_user_activity_events_before(100, 2).await?,
        1
    );
    assert_eq!(
        database.delete_user_activity_events_before(100, 2).await?,
        0
    );
    let retained_ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM user_activity_events ORDER BY occurred_at, id")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(retained_ids, vec!["retention-boundary", "retention-new"]);
    let audit_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE id = 'retention-audit'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(audit_count, 1);
    assert_eq!(database.delete_user_activity_events_before(0, 0).await?, 0);
    assert_eq!(database.delete_user_activity_events_before(0, -1).await?, 0);
    Ok(())
}

#[tokio::test]
async fn user_activity_retention_setting_defaults_validates_and_recovers_invalid_values()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    assert_eq!(database.user_activity_retention_days().await?, 90);
    database.set_user_activity_retention_days(180).await?;
    assert_eq!(database.user_activity_retention_days().await?, 180);
    assert!(database.set_user_activity_retention_days(0).await.is_err());
    assert!(database.set_user_activity_retention_days(-1).await.is_err());
    assert!(
        database
            .set_user_activity_retention_days(3_651)
            .await
            .is_err()
    );
    assert_eq!(database.user_activity_retention_days().await?, 180);

    sqlx::query(
        "UPDATE server_settings SET value = 'not-a-number'
         WHERE key = 'user_activity_retention_days'",
    )
    .execute(database.pool())
    .await?;
    assert_eq!(database.user_activity_retention_days().await?, 90);
    sqlx::query(
        "UPDATE server_settings SET value = '0'
         WHERE key = 'user_activity_retention_days'",
    )
    .execute(database.pool())
    .await?;
    assert_eq!(database.user_activity_retention_days().await?, 90);
    Ok(())
}

#[tokio::test]
async fn trusted_proxy_cidrs_setting_reads_and_replaces_raw_json()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    assert_eq!(database.trusted_proxy_cidrs_json().await?, None);
    database
        .set_trusted_proxy_cidrs_json("[\"10.0.0.0/8\"]")
        .await?;
    assert_eq!(
        database.trusted_proxy_cidrs_json().await?.as_deref(),
        Some("[\"10.0.0.0/8\"]")
    );
    database
        .set_trusted_proxy_cidrs_json("[\"192.0.2.0/24\",\"2001:db8::/32\"]")
        .await?;
    assert_eq!(
        database.trusted_proxy_cidrs_json().await?.as_deref(),
        Some("[\"192.0.2.0/24\",\"2001:db8::/32\"]")
    );
    Ok(())
}

#[tokio::test]
async fn admin_emby_login_session_page_is_bounded_and_excludes_revoked_tokens()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    for user_id in ["emby-page-user", "emby-page-other"] {
        sqlx::query(
            "INSERT INTO users (id, username_normalized, display_name, password_hash)
             VALUES (?, ?, ?, 'test')",
        )
        .bind(user_id)
        .bind(user_id)
        .bind(user_id)
        .execute(database.pool())
        .await?;
    }
    for (id, user_id, updated_at, last_seen_at, revoked_at) in [
        ("older", "emby-page-user", 120_i64, Some(130_i64), None),
        ("newer", "emby-page-user", 140, Some(150), None),
        ("revoked", "emby-page-user", 1_000, Some(1_000), Some(1_100)),
        ("other-user", "emby-page-other", 2_000, Some(2_000), None),
    ] {
        let mut token_hash = id.as_bytes().to_vec();
        token_hash.resize(32, 0);
        sqlx::query(
            "INSERT INTO access_tokens (
                id, token_hash, user_id, device_id, client_name, device_name,
                client_version, device_type, created_at, updated_at, last_seen_at, revoked_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 100, ?, ?, ?)",
        )
        .bind(id)
        .bind(token_hash)
        .bind(user_id)
        .bind(format!("device-{id}"))
        .bind("Emby")
        .bind(format!("Device {id}"))
        .bind("1.2.3")
        .bind("TV")
        .bind(updated_at)
        .bind(last_seen_at)
        .bind(revoked_at)
        .execute(database.pool())
        .await?;
    }
    for index in 0..101_i64 {
        let id = format!("filler-{index:03}");
        let last_seen_at = (index != 100).then_some(110_i64);
        sqlx::query(
            "INSERT INTO access_tokens (
                id, token_hash, user_id, device_id, client_name, device_name,
                client_version, device_type, created_at, updated_at, last_seen_at
             ) VALUES (?, ?, 'emby-page-user', ?, 'Emby', ?, '1.2.3', 'TV', 100, 110, ?)",
        )
        .bind(&id)
        .bind(vec![index as u8; 32])
        .bind(format!("device-{id}"))
        .bind(format!("Device {id}"))
        .bind(last_seen_at)
        .execute(database.pool())
        .await?;
    }

    let (first_page, total_count) = database
        .list_user_emby_login_sessions_for_admin("emby-page-user", 1, 0)
        .await?;
    assert_eq!(total_count, 103);
    assert_eq!(first_page.len(), 1);
    assert_eq!(first_page[0].id, "newer");
    assert_eq!(first_page[0].device_id, "device-newer");
    assert_eq!(first_page[0].device_name, "Device newer");
    assert_eq!(first_page[0].device_type.as_deref(), Some("TV"));
    assert_eq!(first_page[0].client_name, "Emby");
    assert_eq!(first_page[0].client_version, "1.2.3");
    assert_eq!(first_page[0].created_at, 100);
    assert_eq!(first_page[0].updated_at, 140);
    assert_eq!(first_page[0].last_seen_at, Some(150));
    assert!(!format!("{:?}", first_page[0]).contains("token_hash"));

    let (second_page, second_count) = database
        .list_user_emby_login_sessions_for_admin("emby-page-user", 1, 1)
        .await?;
    assert_eq!(second_count, 103);
    assert_eq!(second_page[0].id, "older");
    let (tie_page, tie_count) = database
        .list_user_emby_login_sessions_for_admin("emby-page-user", 2, 2)
        .await?;
    assert_eq!(tie_count, 103);
    assert_eq!(tie_page[0].id, "filler-100");
    assert_eq!(tie_page[0].last_seen_at, None);
    assert_eq!(tie_page[1].id, "filler-099");
    let (bounded_page, bounded_count) = database
        .list_user_emby_login_sessions_for_admin("emby-page-user", 1_000, -4)
        .await?;
    assert_eq!(bounded_count, 103);
    assert_eq!(bounded_page.len(), 100);
    assert_eq!(bounded_page[0].id, "newer");
    assert!(bounded_page.iter().all(|session| session.id != "revoked"));
    assert!(
        bounded_page
            .iter()
            .all(|session| session.id != "other-user")
    );
    let (empty_page, empty_count) = database
        .list_user_emby_login_sessions_for_admin("missing-user", 50, 0)
        .await?;
    assert!(empty_page.is_empty());
    assert_eq!(empty_count, 0);
    Ok(())
}

#[tokio::test]
async fn admin_emby_login_session_revocation_is_scoped_and_idempotent()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    for user_id in ["emby-revoke-user", "emby-revoke-other"] {
        sqlx::query(
            "INSERT INTO users (id, username_normalized, display_name, password_hash)
             VALUES (?, ?, ?, 'test')",
        )
        .bind(user_id)
        .bind(user_id)
        .bind(user_id)
        .execute(database.pool())
        .await?;
    }
    for (id, user_id, revoked_at) in [
        ("target-one", "emby-revoke-user", None),
        ("target-two", "emby-revoke-user", None),
        ("target-revoked", "emby-revoke-user", Some(777_i64)),
        ("other-active", "emby-revoke-other", None),
    ] {
        let mut token_hash = id.as_bytes().to_vec();
        token_hash.resize(32, 0);
        sqlx::query(
            "INSERT INTO access_tokens (
                id, token_hash, user_id, device_id, client_name, device_name,
                client_version, created_at, updated_at, revoked_at
             ) VALUES (?, ?, ?, ?, 'Emby', ?, '1.2.3', 100, 100, ?)",
        )
        .bind(id)
        .bind(token_hash)
        .bind(user_id)
        .bind(format!("device-{id}"))
        .bind(format!("Device {id}"))
        .bind(revoked_at)
        .execute(database.pool())
        .await?;
    }

    let emby_auth = crate::auth::emby::EmbyAuthService::new(database.clone())?;
    let (initial_sessions, initial_count) = emby_auth
        .list_admin_login_sessions("emby-revoke-user", 100, 0)
        .await?;
    assert_eq!(initial_count, 2);
    assert_eq!(initial_sessions.len(), 2);
    assert!(
        initial_sessions
            .iter()
            .all(|session| session.id != "target-revoked")
    );

    assert!(
        emby_auth
            .revoke_admin_login_session("emby-revoke-user", "target-one")
            .await?
    );
    assert!(
        !emby_auth
            .revoke_admin_login_session("emby-revoke-user", "target-one")
            .await?
    );
    assert!(
        !emby_auth
            .revoke_admin_login_session("emby-revoke-user", "other-active")
            .await?
    );
    assert!(
        !emby_auth
            .revoke_admin_login_session("emby-revoke-user", "missing-session")
            .await?
    );

    let all_revoked = emby_auth
        .revoke_all_admin_login_sessions("emby-revoke-user")
        .await?;
    assert_eq!(all_revoked, 1);
    assert_eq!(
        emby_auth
            .revoke_all_admin_login_sessions("emby-revoke-user")
            .await?,
        0
    );
    let target_revocations: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT id, revoked_at FROM access_tokens WHERE user_id = 'emby-revoke-user' ORDER BY id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(target_revocations.len(), 3);
    assert!(
        target_revocations
            .iter()
            .all(|(_, revoked_at)| revoked_at.is_some())
    );
    assert_eq!(
        target_revocations
            .iter()
            .find(|(id, _)| id == "target-revoked")
            .map(|(_, revoked_at)| *revoked_at),
        Some(Some(777))
    );
    let other_revocation: Option<i64> =
        sqlx::query_scalar("SELECT revoked_at FROM access_tokens WHERE id = 'other-active'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(other_revocation, None);
    Ok(())
}

#[tokio::test]
async fn library_pipeline_activity_page_is_bounded_and_aggregates_scan_status()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::storage::StoredPipelineStageKind;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let alpha = libraries
        .create_library("Alpha", LibraryKind::Movie, false)
        .await?;
    let beta = libraries
        .create_library("Beta", LibraryKind::Movie, false)
        .await?;
    let disabled = libraries
        .create_library("Disabled", LibraryKind::Movie, false)
        .await?;
    let removed = libraries
        .create_library("Removed", LibraryKind::Movie, false)
        .await?;
    sqlx::query("UPDATE libraries SET is_enabled = 0 WHERE id = ?")
        .bind(disabled.id.to_string())
        .execute(database.pool())
        .await?;
    database
        .create_scan_job(
            "pipeline-removed-library",
            &removed.id.to_string(),
            "RECONCILE_LIBRARY",
            "g0",
            1,
            false,
        )
        .await?;
    libraries.delete_library(removed.id).await?;

    let alpha_id = alpha.id.to_string();
    database
        .create_scan_job(
            "pipeline-alpha-running",
            &alpha_id,
            "RECONCILE_LIBRARY",
            "g1",
            4,
            false,
        )
        .await?;
    database
        .create_scan_job(
            "pipeline-alpha-completed",
            &alpha_id,
            "INCREMENTAL_SCAN",
            "g2",
            7,
            false,
        )
        .await?;
    sqlx::query(
        "UPDATE scan_jobs
         SET status = 'RUNNING', scan_phase = 'INDEXING', processed_count = 2,
             updated_at = 100
         WHERE id = 'pipeline-alpha-running'",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE scan_jobs
         SET status = 'COMPLETED', scan_phase = 'IDLE', processed_count = 7,
             updated_at = 200
         WHERE id = 'pipeline-alpha-completed'",
    )
    .execute(database.pool())
    .await?;

    database.reset_query_count();
    let first_page = database.list_library_pipeline_activity(1, 1).await?;
    assert_eq!(first_page.total_libraries, 2);
    assert_eq!(first_page.libraries.len(), 1);
    assert_eq!(first_page.libraries[0].name, "Alpha");
    let index = first_page.libraries[0]
        .stages
        .iter()
        .find(|stage| stage.kind == StoredPipelineStageKind::Index)
        .ok_or("missing index stage")?;
    assert!(index.planned);
    assert_eq!(index.counts.running, 1);
    assert_eq!(index.counts.completed, 1);
    assert_eq!(index.recent_activity_at, Some(200));
    assert!(database.query_count() <= 2, "page query must stay bounded");

    let second_page = database.list_library_pipeline_activity(2, 1).await?;
    assert_eq!(second_page.total_libraries, 2);
    assert_eq!(second_page.libraries[0].name, "Beta");
    assert_eq!(second_page.libraries[0].id, beta.id.to_string());
    assert!(
        second_page.libraries[0]
            .stages
            .iter()
            .all(|stage| !stage.planned)
    );
    assert!(
        database
            .list_library_pipeline_activity(3, 1)
            .await?
            .libraries
            .is_empty()
    );
    assert!(
        database
            .list_library_pipeline_activity(1, 51)
            .await
            .is_err()
    );

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn library_pipeline_activity_counts_persisted_background_stages()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::storage::StoredPipelineStageKind;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Pipeline", LibraryKind::Movie, false)
        .await?;
    let root_path = temp_dir.path().join("media-root");
    std::fs::create_dir(&root_path)?;
    let root = libraries
        .add_root(
            library.id,
            root_path.to_str().ok_or("media root is not UTF-8")?,
        )
        .await?
        .root;
    let library_id = library.id.to_string();
    let root_id = root.id.to_string();

    for (job_id, job_type, status, phase, current_item, updated_at) in [
        (
            "pipeline-postprocessing",
            "RECONCILE_LIBRARY",
            "COMPLETED",
            "POSTPROCESSING",
            Some("视频缩略图"),
            900_i64,
        ),
        (
            "pipeline-cancelled",
            "INCREMENTAL_SCAN",
            "CANCELLED",
            "IDLE",
            None,
            800_i64,
        ),
        (
            "pipeline-probing",
            "INCREMENTAL_SCAN",
            "COMPLETED",
            "POSTPROCESSING",
            Some("媒体探测"),
            850_i64,
        ),
    ] {
        database
            .create_scan_job(job_id, &library_id, job_type, job_id, 1, false)
            .await?;
        sqlx::query(
            "UPDATE scan_jobs SET status = ?, scan_phase = ?, current_item = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(status)
        .bind(phase)
        .bind(current_item)
        .bind(updated_at)
        .bind(job_id)
        .execute(database.pool())
        .await?;
    }

    for (sequence, status, source_count) in [
        (0_i64, "PENDING", 3_i64),
        (1, "RUNNING", 2),
        (2, "COMPLETED", 4),
        (3, "FAILED", 1),
        (4, "CANCELLED", 2),
    ] {
        sqlx::query(
            "INSERT INTO scan_local_metadata_batches (
                 id, job_id, library_root_id, batch_sequence, source_refs_json,
                 source_count, status, updated_at
             ) VALUES (?, 'pipeline-postprocessing', ?, ?, '[\"source\"]', ?, ?, ?)",
        )
        .bind(format!("pipeline-batch-{sequence}"))
        .bind(&root_id)
        .bind(sequence)
        .bind(source_count)
        .bind(status)
        .bind(100 + sequence)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO scan_local_metadata_backfills (library_root_id, status, updated_at)
         VALUES (?, 'FAILED', 300)",
    )
    .bind(&root_id)
    .execute(database.pool())
    .await?;

    for (target_id, metadata_state, thumbnail_state) in [
        ("legacy-pending", "PENDING", "PENDING"),
        ("legacy-done", "DONE", "DONE"),
        ("legacy-failed", "FAILED", "FAILED"),
    ] {
        sqlx::query(
            "INSERT INTO scan_job_targets (
                 job_id, target_type, target_id, item_id, change_kind,
                 metadata_state, thumbnail_state, updated_at
             ) VALUES ('pipeline-cancelled', 'ITEM', ?, ?, 'NEW', ?, ?, 400)",
        )
        .bind(target_id)
        .bind(target_id)
        .bind(metadata_state)
        .bind(thumbnail_state)
        .execute(database.pool())
        .await?;
    }
    for (job_id, target_id, probe_state) in [
        ("pipeline-probing", "probe-pending", "PENDING"),
        ("pipeline-probing", "probe-done", "DONE"),
        ("pipeline-probing", "probe-failed", "FAILED"),
        ("pipeline-cancelled", "probe-cancelled", "PENDING"),
    ] {
        sqlx::query(
            "INSERT INTO scan_job_targets (
                 job_id, target_type, target_id, item_id, source_id, change_kind,
                 probe_state, updated_at
             ) VALUES (?, 'SOURCE', ?, 'pipeline-secret-title', ?, 'NEW', ?, 450)",
        )
        .bind(job_id)
        .bind(target_id)
        .bind(target_id)
        .bind(probe_state)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO scan_job_targets (
             job_id, target_type, target_id, item_id, change_kind,
             thumbnail_state, updated_at
         ) VALUES ('pipeline-postprocessing', 'ITEM', 'thumb-pending', 'thumb-pending', 'NEW', 'PENDING', 500),
                  ('pipeline-postprocessing', 'ITEM', 'thumb-done', 'thumb-done', 'NEW', 'DONE', 500),
                  ('pipeline-postprocessing', 'ITEM', 'thumb-failed', 'thumb-failed', 'NEW', 'FAILED', 500)",
    )
    .execute(database.pool())
    .await?;

    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('pipeline-secret-title', ?, 'MOVIE', 'DO_NOT_LEAK_MEDIA_TITLE', 'pipeline-title', 'LOCAL_CONFIRMED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    for (job_id, status, updated_at) in [
        ("probe-queued", "PENDING", 506_i64),
        ("probe-failed-job", "FAILED", 507),
        ("probe-cancelled-job", "CANCELLED", 508),
    ] {
        sqlx::query(
            "INSERT INTO strm_probe_jobs (id, operation_id, library_id, status, concurrency, updated_at)
             VALUES (?, ?, ?, ?, 1, ?)",
        )
        .bind(job_id)
        .bind("pipeline-operation")
        .bind(&library_id)
        .bind(status)
        .bind(updated_at)
        .execute(database.pool())
        .await?;
    }

    for (job_id, status, item_status) in [
        ("auto-queued", "QUEUED", "PENDING"),
        ("auto-running", "RUNNING", "RUNNING"),
        ("auto-completed", "COMPLETED", "COMPLETED"),
        ("auto-failed", "FAILED", "FAILED"),
        ("auto-cancelled", "CANCELLED", "PENDING"),
    ] {
        sqlx::query(
            "INSERT INTO metadata_reidentify_jobs (
                 id, status, total_count, mode, library_id, dispatch_source
             ) VALUES (?, ?, 1, 'FILL_MISSING', ?, 'AUTOMATIC')",
        )
        .bind(job_id)
        .bind(status)
        .bind(&library_id)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO metadata_reidentify_job_items (job_id, item_id, status)
             VALUES (?, 'pipeline-secret-title', ?)",
        )
        .bind(job_id)
        .bind(item_status)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO metadata_reidentify_jobs (
             id, status, total_count, mode, library_id, dispatch_source
         ) VALUES ('explicit-fill', 'COMPLETED', 1, 'FILL_MISSING', ?, 'EXPLICIT')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO metadata_reidentify_job_items (job_id, item_id, status)
         VALUES ('explicit-fill', 'pipeline-secret-title', 'COMPLETED')",
    )
    .execute(database.pool())
    .await?;

    let page = database.list_library_pipeline_activity(1, 50).await?;
    let stages = &page.libraries[0].stages;
    assert_eq!(
        stages.iter().map(|stage| stage.kind).collect::<Vec<_>>(),
        vec![
            StoredPipelineStageKind::Index,
            StoredPipelineStageKind::LocalMetadata,
            StoredPipelineStageKind::Probe,
            StoredPipelineStageKind::Thumbnail,
            StoredPipelineStageKind::AutomaticMetadata,
        ]
    );
    let local_metadata = stages
        .iter()
        .find(|stage| stage.kind == StoredPipelineStageKind::LocalMetadata)
        .ok_or("missing local metadata stage")?;
    assert_eq!(local_metadata.counts.queued, 3);
    assert_eq!(local_metadata.counts.running, 2);
    assert_eq!(local_metadata.counts.completed, 5);
    assert_eq!(local_metadata.counts.failed, 3);
    assert_eq!(local_metadata.counts.cancelled, 3);

    let probe = stages
        .iter()
        .find(|stage| stage.kind == StoredPipelineStageKind::Probe)
        .ok_or("missing probe stage")?;
    assert_eq!(probe.counts.queued, 2);
    assert_eq!(probe.counts.running, 1);
    assert_eq!(probe.counts.completed, 1);
    assert_eq!(probe.counts.failed, 2);
    assert_eq!(probe.counts.cancelled, 2);

    let thumbnail = stages
        .iter()
        .find(|stage| stage.kind == StoredPipelineStageKind::Thumbnail)
        .ok_or("missing thumbnail stage")?;
    assert_eq!(thumbnail.counts.queued, 1);
    assert_eq!(thumbnail.counts.running, 1);
    assert_eq!(thumbnail.counts.completed, 2);
    assert_eq!(thumbnail.counts.failed, 2);
    assert_eq!(thumbnail.counts.cancelled, 1);

    let automatic_metadata = stages
        .iter()
        .find(|stage| stage.kind == StoredPipelineStageKind::AutomaticMetadata)
        .ok_or("missing automatic metadata stage")?;
    assert_eq!(automatic_metadata.counts.queued, 1);
    assert_eq!(automatic_metadata.counts.running, 1);
    assert_eq!(automatic_metadata.counts.completed, 1);
    assert_eq!(automatic_metadata.counts.failed, 1);
    assert_eq!(automatic_metadata.counts.cancelled, 1);
    assert!(!format!("{page:?}").contains("DO_NOT_LEAK_MEDIA_TITLE"));

    database.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_library_pipeline_activity_uses_the_shared_aggregate_query()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD").unwrap_or_default(),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration = DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(
        &config,
        &DatabaseConfiguration::Postgres(PostgresConnection {
            database: database_name.clone(),
            ..admin_connection
        }),
    )
    .await?;
    let check_result = async {
        let library = LibraryService::new(database.clone())
            .create_library("PostgreSQL Pipeline", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        let root_path = temp_dir.path().join("postgres-media-root");
        std::fs::create_dir(&root_path)?;
        let root = LibraryService::new(database.clone())
            .add_root(
                library.id,
                root_path.to_str().ok_or("media root is not UTF-8")?,
            )
            .await?
            .root;
        let root_id = root.id.to_string();
        database
            .create_scan_job(
                "pg-pipeline-scan",
                &library_id,
                "RECONCILE_LIBRARY",
                "pg-pipeline-generation",
                1,
                false,
            )
            .await?;
        database
            .query(
                "UPDATE scan_jobs
                 SET status = 'COMPLETED', scan_phase = 'POSTPROCESSING',
                     current_item = '媒体探测', updated_at = 900
                 WHERE id = 'pg-pipeline-scan'",
            )
            .execute(database.pool())
            .await?;
        database
            .query(
                "INSERT INTO scan_local_metadata_batches (
                     id, job_id, library_root_id, batch_sequence, source_refs_json,
                     source_count, status, updated_at
                 ) VALUES ('pg-pipeline-batch', 'pg-pipeline-scan', ?, 0, '[\"source\"]', 3, 'COMPLETED', 800)",
            )
            .bind(&root_id)
            .execute(database.pool())
            .await?;
        database
            .query(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, item_id, source_id, change_kind,
                     probe_state, updated_at
                 ) VALUES ('pg-pipeline-scan', 'SOURCE', 'pg-probe-failed', 'pg-item',
                          'pg-probe-failed', 'NEW', 'FAILED', 850)",
            )
            .execute(database.pool())
            .await?;
        database
            .query(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, item_id, change_kind,
                     thumbnail_state, updated_at
                 ) VALUES ('pg-pipeline-scan', 'ITEM', 'pg-thumbnail-done', 'pg-item',
                          'NEW', 'DONE', 810)",
            )
            .execute(database.pool())
            .await?;
        database
            .query(
                "INSERT INTO metadata_reidentify_jobs (
                     id, status, total_count, mode, library_id, dispatch_source
                 ) VALUES ('pg-auto-fill-missing', 'QUEUED', 1, 'FILL_MISSING', ?, 'AUTOMATIC')",
            )
            .bind(&library_id)
            .execute(database.pool())
            .await?;
        let page = database.list_library_pipeline_activity(1, 50).await?;
        assert_eq!(page.total_libraries, 1);
        assert_eq!(page.libraries.len(), 1);
        assert_eq!(page.libraries[0].stages.len(), 5);
        let stages = &page.libraries[0].stages;
        let index = stages
            .iter()
            .find(|stage| stage.kind == StoredPipelineStageKind::Index)
            .ok_or("missing index stage")?;
        assert_eq!(index.counts.completed, 1);
        let local_metadata = stages
            .iter()
            .find(|stage| stage.kind == StoredPipelineStageKind::LocalMetadata)
            .ok_or("missing local metadata stage")?;
        assert_eq!(local_metadata.counts.completed, 3);
        let probe = stages
            .iter()
            .find(|stage| stage.kind == StoredPipelineStageKind::Probe)
            .ok_or("missing probe stage")?;
        assert_eq!(probe.counts.running, 1);
        assert_eq!(probe.counts.failed, 1);
        assert_eq!(probe.recent_activity_at, Some(900));
        let thumbnail = stages
            .iter()
            .find(|stage| stage.kind == StoredPipelineStageKind::Thumbnail)
            .ok_or("missing thumbnail stage")?;
        assert_eq!(thumbnail.counts.completed, 1);
        let automatic_metadata = stages
            .iter()
            .find(|stage| stage.kind == StoredPipelineStageKind::AutomaticMetadata)
            .ok_or("missing automatic metadata stage")?;
        assert_eq!(automatic_metadata.counts.queued, 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    database.close().await;
    let drop_result = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    check_result?;
    drop_result?;
    Ok(())
}

async fn assert_removed_media_item_purge(
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let library = LibraryService::new(database.clone())
        .create_library("Purge", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    database
        .query(
            "INSERT INTO users (id, username_normalized, display_name, password_hash)
             VALUES ('purge-user', 'purge-user', 'Purge User', 'test')",
        )
        .execute(database.pool())
        .await?;
    let day = 86_400_i64;
    // (id, item_type, removed_days_ago)
    let items = [
        ("expired", "MOVIE", Some(60_i64)),
        ("expired-two", "EPISODE", Some(45)),
        ("expired-three", "VIDEO", Some(40)),
        ("recent", "MOVIE", Some(2)),
        ("active", "MOVIE", None),
        ("watched", "MOVIE", Some(90)),
        ("merge-target", "MOVIE", Some(90)),
        ("merged-away", "MOVIE", None),
        ("expired-series", "SERIES", Some(90)),
    ];
    for (id, item_type, removed_days_ago) in items {
        database
            .query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, identification_status,
                     has_available_source, removed_at
                 ) VALUES (?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', 0,
                           CASE WHEN ? IS NULL THEN NULL ELSE unixepoch() - ? END)",
            )
            .bind(id)
            .bind(&library_id)
            .bind(item_type)
            .bind(id)
            .bind(id)
            .bind(removed_days_ago.unwrap_or_default())
            .bind(removed_days_ago.unwrap_or_default() * day)
            .execute(database.pool())
            .await?;
        if removed_days_ago.is_none() {
            database
                .query("UPDATE media_items SET removed_at = NULL WHERE id = ?")
                .bind(id)
                .execute(database.pool())
                .await?;
        }
        database
            .query(
                "INSERT INTO item_images (id, item_id, image_type, image_index, local_path)
                 VALUES (?, ?, 'PRIMARY', 0, ?)",
            )
            .bind(format!("image-{id}"))
            .bind(id)
            .bind(format!("/images/{id}.jpg"))
            .execute(database.pool())
            .await?;
    }
    database
        .query(
            "INSERT INTO user_item_state (user_id, item_id, is_played)
             VALUES ('purge-user', 'watched', 1)",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "UPDATE media_items SET merged_into_item_id = 'merge-target' WHERE id = 'merged-away'",
        )
        .execute(database.pool())
        .await?;

    let retention = 30 * day;
    // Batch of one with several passes proves the purge pages through the backlog.
    let purged = database
        .purge_expired_removed_media_items(retention, 1, 2, std::time::Duration::ZERO)
        .await?;
    assert_eq!(purged, 2);
    let purged = database
        .purge_expired_removed_media_items(retention, 1, 10, std::time::Duration::ZERO)
        .await?;
    assert_eq!(purged, 1);
    assert_eq!(
        database
            .purge_expired_removed_media_items(retention, 10, 10, std::time::Duration::ZERO)
            .await?,
        0
    );

    let remaining: Vec<String> = database
        .query_scalar("SELECT id FROM media_items ORDER BY id")
        .fetch_all(database.pool())
        .await?;
    assert_eq!(
        remaining,
        [
            "active",
            "expired-series",
            "merge-target",
            "merged-away",
            "recent",
            "watched"
        ]
    );
    let orphan_images: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM item_images
             WHERE item_id IN ('expired', 'expired-two', 'expired-three')",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(orphan_images, 0);
    let remaining_images: i64 = database
        .query_scalar("SELECT COUNT(*) FROM item_images")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(remaining_images, 6);
    let orphan_search_rows: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM media_search
             WHERE item_id IN ('expired', 'expired-two', 'expired-three')",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(orphan_search_rows, 0);
    // Retention is measured from the removal time: a zero-day window also takes the
    // recently removed item, but still never the guarded ones.
    assert_eq!(
        database
            .purge_expired_removed_media_items(0, 10, 10, std::time::Duration::ZERO)
            .await?,
        1
    );
    assert_missing_filesystem_entry_purge(database).await?;
    Ok(())
}

async fn assert_missing_filesystem_entry_purge(
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let library_service = LibraryService::new(database.clone());
    let library = library_service
        .create_library("Missing entry purge", LibraryKind::Movie, false)
        .await?;
    let active_library = library_service
        .create_library("Active scan purge", LibraryKind::Movie, false)
        .await?;
    let root_dir = tempfile::tempdir()?;
    let active_root_dir = tempfile::tempdir()?;
    let root_id = uuid::Uuid::now_v7().to_string();
    let active_root_id = uuid::Uuid::now_v7().to_string();
    for (root_id, library_id, path) in [
        (root_id.as_str(), library.id.to_string(), root_dir.path()),
        (
            active_root_id.as_str(),
            active_library.id.to_string(),
            active_root_dir.path(),
        ),
    ] {
        let path = path.to_str().ok_or("non-UTF8 test root")?;
        database
            .query(
                "INSERT INTO library_roots (
                     id, library_id, canonical_path, display_path, is_available, is_writable
                 ) VALUES (?, ?, ?, ?, 1, 0)",
            )
            .bind(root_id)
            .bind(library_id)
            .bind(path)
            .bind(path)
            .execute(database.pool())
            .await?;
    }
    ScanJobService::new(database.clone())
        .create_movie_scan_job(active_library.id)
        .await?;

    let mut expected_retained_entries = Vec::new();
    for (relative_path, missing, age_days, linked) in [
        ("expired-oldest.mkv", 1_i64, 90_i64, false),
        ("expired-middle.mkv", 1, 60, false),
        ("expired-newest.mkv", 1, 45, false),
        ("recent.mkv", 1, 2, false),
        ("live.mkv", 0, 90, false),
        ("source-linked.mkv", 1, 90, true),
    ] {
        let entry_id = uuid::Uuid::now_v7().to_string();
        database
            .query(
                "INSERT INTO filesystem_entries (
                     id, library_root_id, relative_path, entry_kind, size, modified_at,
                     last_seen_generation, is_missing, updated_at
                 ) VALUES (?, ?, ?, 'FILE', 1, 1, 'purge-test', ?,
                           unixepoch() - ? * 86400)",
            )
            .bind(&entry_id)
            .bind(&root_id)
            .bind(relative_path)
            .bind(missing)
            .bind(age_days)
            .execute(database.pool())
            .await?;
        if missing == 0 || age_days < 30 || linked {
            expected_retained_entries.push(entry_id.clone());
        }
        if linked {
            let item_id = uuid::Uuid::now_v7().to_string();
            database
                .query(
                    "INSERT INTO media_items (
                         id, library_id, item_type, title, sort_title, identification_status
                     ) VALUES (?, ?, 'MOVIE', 'Retained source', 'retained source',
                               'LOCAL_CONFIRMED')",
                )
                .bind(&item_id)
                .bind(library.id.to_string())
                .execute(database.pool())
                .await?;
            database
                .query(
                    "INSERT INTO media_sources (id, item_id, source_kind, filesystem_entry_id)
                     VALUES (?, ?, 'LOCAL_FILE', ?)",
                )
                .bind(uuid::Uuid::now_v7().to_string())
                .bind(item_id)
                .bind(&entry_id)
                .execute(database.pool())
                .await?;
        }
    }

    let active_entry_id = uuid::Uuid::now_v7().to_string();
    database
        .query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 last_seen_generation, is_missing, updated_at
             ) VALUES (?, ?, 'active-scan.mkv', 'FILE', 1, 1, 'purge-test', 1,
                       unixepoch() - 90 * 86400)",
        )
        .bind(&active_entry_id)
        .bind(&active_root_id)
        .execute(database.pool())
        .await?;
    expected_retained_entries.push(active_entry_id.clone());

    let retention = 30 * 86_400;
    assert_eq!(
        database
            .purge_expired_missing_filesystem_entries(retention, 1, 2, std::time::Duration::ZERO,)
            .await?,
        2
    );
    assert_eq!(
        database
            .purge_expired_missing_filesystem_entries(retention, 1, 2, std::time::Duration::ZERO,)
            .await?,
        1
    );
    assert_eq!(
        database
            .purge_expired_missing_filesystem_entries(retention, 1, 2, std::time::Duration::ZERO,)
            .await?,
        0
    );

    expected_retained_entries.sort();
    let remaining: Vec<String> = database
        .query_scalar("SELECT id FROM filesystem_entries ORDER BY id")
        .fetch_all(database.pool())
        .await?;
    assert_eq!(remaining, expected_retained_entries);
    Ok(())
}

#[tokio::test]
async fn expired_soft_deleted_media_items_are_purged_in_batches() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    assert_removed_media_item_purge(&database)
        .await
        .expect("removed item purge");
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_expired_soft_deleted_media_items_are_purged_in_batches()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration =
        crate::config::DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(
        &config,
        &crate::config::DatabaseConfiguration::Postgres(PostgresConnection {
            database: database_name.clone(),
            ..admin_connection
        }),
    )
    .await?;
    let outcome = tokio::spawn({
        let database = database.clone();
        async move {
            assert_removed_media_item_purge(&database)
                .await
                .map_err(|error| error.to_string())
        }
    })
    .await;
    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    outcome??;
    drop_database?;
    Ok(())
}

async fn assert_media_source_defaults_are_normalized(
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let library = LibraryService::new(database.clone())
        .create_library("Defaults", LibraryKind::Movie, false)
        .await?;
    // (item, [(source, is_default)])
    let items: [(&str, [(&str, i64); 3]); 4] = [
        ("two-defaults", [("a1", 1), ("a2", 1), ("a3", 0)]),
        ("no-default", [("b1", 0), ("b2", 0), ("b3", 0)]),
        ("three-defaults", [("c1", 1), ("c2", 1), ("c3", 1)]),
        ("healthy", [("d1", 0), ("d2", 1), ("d3", 0)]),
    ];
    for (item_id, sources) in items {
        database
            .query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
        for (source_id, is_default) in sources {
            database
                .query(
                    "INSERT INTO media_sources (id, item_id, source_kind, is_default, probe_status)
                     VALUES (?, ?, 'LOCAL_FILE', ?, 'PENDING')",
                )
                .bind(source_id)
                .bind(item_id)
                .bind(is_default)
                .execute(database.pool())
                .await?;
        }
    }

    assert_eq!(database.normalize_media_source_defaults().await?, 3);
    let defaults: Vec<String> = database
        .query_scalar("SELECT id FROM media_sources WHERE is_default = 1 ORDER BY id")
        .fetch_all(database.pool())
        .await?;
    assert_eq!(defaults, ["a1", "b1", "c1", "d2"]);
    assert_eq!(database.normalize_media_source_defaults().await?, 0);
    Ok(())
}

#[tokio::test]
async fn media_items_are_normalized_to_one_default_source() {
    let temp_dir = tempfile::tempdir().expect("temporary directory");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await.expect("database");
    assert_media_source_defaults_are_normalized(&database)
        .await
        .expect("default normalization");
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_media_items_are_normalized_to_one_default_source()
-> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration =
        crate::config::DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(
        &config,
        &crate::config::DatabaseConfiguration::Postgres(PostgresConnection {
            database: database_name.clone(),
            ..admin_connection
        }),
    )
    .await?;
    let outcome = tokio::spawn({
        let database = database.clone();
        async move {
            assert_media_source_defaults_are_normalized(&database)
                .await
                .map_err(|error| error.to_string())
        }
    })
    .await;
    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    outcome??;
    drop_database?;
    Ok(())
}

#[tokio::test]
async fn manifest_discovery_presence_templates_preserve_path_and_generation_results()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    database
        .query(
            "INSERT INTO libraries (id, name, kind) VALUES ('presence-lib', 'Presence', 'MOVIE')",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('presence-root', 'presence-lib', '/presence', '/presence', 1, 0)",
        )
        .execute(database.pool())
        .await?;
    let roots = [NewScanManifestRoot {
        library_root_id: "presence-root",
    }];
    let manifest = NewScanManifest {
        id: "presence-manifest",
        job_id: "presence-job",
        library_id: "presence-lib",
        roots: &roots,
    };
    database
        .create_full_scan_manifest_job(
            "presence-job",
            "presence-generation",
            false,
            &manifest,
            None,
        )
        .await?;
    assert!(database.claim_scan_job("presence-job").await?);

    for (id, path, fingerprint, generation) in [
        (
            "presence-current",
            "already-seen.mkv",
            b"current".as_slice(),
            "presence-generation",
        ),
        (
            "presence-fast",
            "fast-update.mkv",
            b"fast".as_slice(),
            "old-generation",
        ),
        (
            "presence-partial",
            "partial-update.mkv",
            b"partial".as_slice(),
            "old-generation",
        ),
    ] {
        database
            .query(
                "INSERT INTO filesystem_entries (
                     id, library_root_id, relative_path, entry_kind, size, modified_at,
                     fingerprint, last_seen_generation, is_missing
                 ) VALUES (?, 'presence-root', ?, 'FILE', 10, 20, ?, ?, 0)",
            )
            .bind(id)
            .bind(path)
            .bind(fingerprint)
            .bind(generation)
            .execute(database.pool())
            .await?;
    }

    let entry = |relative_path: &str, fingerprint: &[u8]| NewScanManifestEntry {
        relative_path: relative_path.to_owned(),
        entry_kind: "FILE".to_owned(),
        size: 11,
        modified_at: 21,
        device: None,
        inode: None,
        fingerprint: fingerprint.to_vec(),
        fingerprint_pending: false,
    };
    let seen_entry =
        |id: &str, relative_path: &str, fingerprint: &[u8]| NewScanManifestSeenFilesystemEntry {
            filesystem_entry_id: id.to_owned(),
            relative_path: relative_path.to_owned(),
            size: 11,
            modified_at: 21,
            device: None,
            inode: None,
            fingerprint: fingerprint.to_vec(),
        };

    let current_entries = [entry("already-seen.mkv", b"current")];
    let current_chunk = NewScanManifestDiscoveryChunk {
        manifest_id: "presence-manifest",
        job_id: "presence-job",
        library_root_id: "presence-root",
        child_directories: &[],
        entries: &current_entries,
        positive_indexes: &[],
        unchanged_paths: &[],
        seen_filesystem_entries: &[],
        completed_directory: None,
    };
    assert_eq!(
        database
            .commit_scan_manifest_discovery_chunk(&current_chunk)
            .await?,
        0,
        "format 3 treats a path already seen in this generation as known"
    );

    let fast_entries = [entry("fast-update.mkv", b"fast")];
    let fast_seen = [seen_entry("presence-fast", "fast-update.mkv", b"fast")];
    let fast_chunk = NewScanManifestDiscoveryChunk {
        manifest_id: "presence-manifest",
        job_id: "presence-job",
        library_root_id: "presence-root",
        child_directories: &[],
        entries: &fast_entries,
        positive_indexes: &[],
        unchanged_paths: &[],
        seen_filesystem_entries: &fast_seen,
        completed_directory: None,
    };
    assert_eq!(
        database
            .commit_scan_manifest_discovery_chunk(&fast_chunk)
            .await?,
        1,
        "a full successful presence update counts the file once"
    );
    let fast_generation: String = database
        .query_scalar(
            "SELECT last_seen_generation FROM filesystem_entries WHERE id = 'presence-fast'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(fast_generation, "presence-generation");
    let fast_ledger_count: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM scan_manifest_seen_paths
             WHERE manifest_id = 'presence-manifest' AND relative_path = 'fast-update.mkv'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        fast_ledger_count, 0,
        "successful updates do not duplicate the ledger"
    );

    let partial_entries = [
        entry("partial-update.mkv", b"partial"),
        entry("missing-entry.mkv", b"missing"),
    ];
    let partial_seen = [
        seen_entry("presence-partial", "partial-update.mkv", b"partial"),
        seen_entry("presence-missing", "missing-entry.mkv", b"missing"),
    ];
    let partial_chunk = NewScanManifestDiscoveryChunk {
        manifest_id: "presence-manifest",
        job_id: "presence-job",
        library_root_id: "presence-root",
        child_directories: &[],
        entries: &partial_entries,
        positive_indexes: &[],
        unchanged_paths: &[],
        seen_filesystem_entries: &partial_seen,
        completed_directory: None,
    };
    assert_eq!(
        database
            .commit_scan_manifest_discovery_chunk(&partial_chunk)
            .await?,
        2,
        "partial updates return the updated path and ledger fallback path once each"
    );
    let partial_generation: String = database
        .query_scalar(
            "SELECT last_seen_generation FROM filesystem_entries WHERE id = 'presence-partial'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(partial_generation, "presence-generation");
    let ledger_paths: Vec<String> = database
        .query_scalar(
            "SELECT relative_path FROM scan_manifest_seen_paths
             WHERE manifest_id = 'presence-manifest' ORDER BY relative_path",
        )
        .fetch_all(database.pool())
        .await?;
    assert_eq!(ledger_paths, ["already-seen.mkv", "missing-entry.mkv"]);

    database
        .query(
            "UPDATE scan_manifests SET discovery_format_version = 2 WHERE id = 'presence-manifest'",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_manifest_entries (
                 manifest_id, library_root_id, relative_path, observation_sequence,
                 entry_kind, size, modified_at
             ) VALUES ('presence-manifest', 'presence-root', 'legacy-known.mkv', 1, 'FILE', 1, 1)",
        )
        .execute(database.pool())
        .await?;
    database
        .query("UPDATE scan_manifest_roots SET next_observation_sequence = 1 WHERE manifest_id = 'presence-manifest'")
        .execute(database.pool())
        .await?;
    let legacy_entries = [entry("legacy-known.mkv", b"legacy")];
    let legacy_chunk = NewScanManifestDiscoveryChunk {
        manifest_id: "presence-manifest",
        job_id: "presence-job",
        library_root_id: "presence-root",
        child_directories: &[],
        entries: &legacy_entries,
        positive_indexes: &[],
        unchanged_paths: &[],
        seen_filesystem_entries: &[],
        completed_directory: None,
    };
    assert_eq!(
        database
            .commit_scan_manifest_discovery_chunk(&legacy_chunk)
            .await?,
        0,
        "legacy format queries known paths from prior manifest observations"
    );
    database.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_scan_job_retry_diagnostics() -> Result<(), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", uuid::Uuid::now_v7().simple());
    let admin_connection = PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(55432),
        database: "postgres".to_owned(),
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    };
    let admin_configuration =
        crate::config::DatabaseConfiguration::Postgres(admin_connection.clone());
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let admin_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&admin_pool)
    .await?;
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(
        &config,
        &crate::config::DatabaseConfiguration::Postgres(PostgresConnection {
            database: database_name.clone(),
            ..admin_connection
        }),
    )
    .await?;
    let outcome = tokio::spawn({
        let database = database.clone();
        async move {
            assert_scan_job_retry_diagnostics(&database, temp_dir.path())
                .await
                .map_err(|error| error.to_string())
        }
    })
    .await;
    database.close().await;
    let drop_database = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&admin_pool)
    .await;
    admin_pool.close().await;
    outcome??;
    drop_database?;
    Ok(())
}
