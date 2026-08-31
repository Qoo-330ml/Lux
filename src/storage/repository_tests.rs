use super::*;
use crate::{
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    config::{Config, DatabaseBackend, PostgresConnection},
    library::LibraryKind,
};

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
        years: &empty_years,
        is_played: None,
        is_favorite: Some(true),
        metadata_pending: false,
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
        pool_max_connections: 1,
        path: PathBuf::from("metadata-summary-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
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
        .restore_canonical_person("lux-000002", "另一位演员", &[("tmdb", "57975")])
        .await
        .expect_err("conflicting manifest must be rejected");
    assert!(matches!(error, StorageError::Conflict(_)));
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
            years: &empty_years,
            is_played: None,
            is_favorite: None,
            metadata_pending: false,
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
            years: &empty_years,
            is_played: None,
            is_favorite: None,
            metadata_pending: false,
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
        pool_max_connections: 1,
        path: PathBuf::from("query-only-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
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
                PRIMARY KEY (job_id, item_id)
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata job items table");
    for (item_id, item_type) in [
        ("episode", "EPISODE"),
        ("season", "SEASON"),
        ("series", "SERIES"),
    ] {
        sqlx::query("INSERT INTO media_items (id, item_type) VALUES (?, ?)")
            .bind(item_id)
            .bind(item_type)
            .execute(&pool)
            .await
            .expect("insert media item");
        sqlx::query(
            "INSERT INTO metadata_reidentify_job_items (job_id, item_id, status)
                 VALUES ('job', ?, 'PENDING')",
        )
        .bind(item_id)
        .execute(&pool)
        .await
        .expect("insert metadata job item");
    }
    let database = Database {
        pool,
        pool_max_connections: 1,
        path: PathBuf::from("metadata-order-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
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
                updated_at INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (job_id, item_id)
            )",
    )
    .execute(&pool)
    .await
    .expect("create metadata job items table");
    for (item_id, item_type) in [
        ("episode", "EPISODE"),
        ("season", "SEASON"),
        ("series", "SERIES"),
    ] {
        sqlx::query("INSERT INTO media_items (id, item_type) VALUES (?, ?)")
            .bind(item_id)
            .bind(item_type)
            .execute(&pool)
            .await
            .expect("insert media item");
        sqlx::query(
            "INSERT INTO metadata_reidentify_job_items (job_id, item_id, status)
                 VALUES ('job', ?, 'PENDING')",
        )
        .bind(item_id)
        .execute(&pool)
        .await
        .expect("insert metadata job item");
    }
    sqlx::query("INSERT INTO metadata_reidentify_jobs (id, status, cancel_requested) VALUES ('job', 'RUNNING', 0)")
        .execute(&pool)
        .await
        .expect("insert metadata job");
    let database = Database {
        pool,
        pool_max_connections: 1,
        path: PathBuf::from("metadata-batch-claim-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
        query_count: Arc::new(AtomicUsize::new(0)),
    };

    let claimed = database
        .claim_next_metadata_reidentify_items("job", 2)
        .await
        .expect("claim metadata items");
    assert_eq!(claimed, vec!["series"]);
    sqlx::query(
        "UPDATE metadata_reidentify_job_items
         SET status = 'COMPLETED' WHERE job_id = 'job' AND item_id = 'series'",
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
            ("season".to_owned(), "RUNNING".to_owned()),
            ("series".to_owned(), "COMPLETED".to_owned()),
        ]
    );
    database.close().await;
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
        pool_max_connections: 1,
        path: PathBuf::from("metadata-reconcile-test.db"),
        server_id: "test".to_owned(),
        backend: DatabaseBackend::Sqlite,
        person_credits_write_lock: Arc::new(AsyncMutex::new(())),
        metadata_write_lock: Arc::new(AsyncMutex::new(())),
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
        adapt_sql_for_backend(DatabaseBackend::Postgres, sql),
        "SELECT $1, '?' AS literal, \"?\" AS identifier, $2"
    );
    assert_eq!(adapt_sql_for_backend(DatabaseBackend::Sqlite, sql), sql);
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
        .replace_person_credits_with_fingerprint("item-a", &[], Some("fingerprint-a"))
        .await
        .expect("store fingerprint");
    assert!(
        database
            .person_index_item_state_is_current("item-a", Some("fingerprint-a"))
            .await
            .expect("same fingerprint")
    );
    assert!(
        !database
            .person_index_item_state_is_current("item-a", None)
            .await
            .expect("missing fingerprint must not be current")
    );
    assert!(
        !database
            .person_index_item_state_is_current("item-a", Some("fingerprint-b"))
            .await
            .expect("changed fingerprint")
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
            .person_index_item_state_is_current("item-a", Some("fingerprint-a"))
            .await
            .expect("changed relation schema version")
    );
    database
        .clear_person_credits("item-a")
        .await
        .expect("clear person credits");
    assert!(
        !database
            .person_index_item_state_is_current("item-a", Some("fingerprint-a"))
            .await
            .expect("cleared relation must be rebuilt")
    );
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

    let expired = database
        .take_expired_web_playback_sessions(100)
        .await
        .expect("expired sessions");

    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].id, "expired-session");
    let state: String =
        sqlx::query_scalar("SELECT state FROM web_playback_sessions WHERE id = 'expired-session'")
            .fetch_one(database.pool())
            .await
            .expect("session state");
    assert_eq!(state, "STOPPED");
}
