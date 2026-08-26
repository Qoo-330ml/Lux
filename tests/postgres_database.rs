use std::env;

use luxd::{
    application::{
        access::{AccessPrincipal, MediaAccessService},
        candidates::{MetadataSelectionMode, MetadataSelectionService},
        catalog::CatalogService,
        images::ImageWriteService,
        libraries::{LibraryService, LibrarySettingsPatch},
        metadata::MetadataEnricher,
        people::PeopleService,
        plugins::PluginService,
        scanner::LibraryScanner,
        setup::SetupService,
        strm_probe::{StrmProbeOptions, StrmProbeService},
    },
    auth::sessions::WebAuthService,
    config::{Config, DatabaseConfiguration, PostgresConnection},
    domain::ids::{LibraryId, UserId},
    storage::Database,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

fn postgres_connection(database: String) -> PostgresConnection {
    PostgresConnection {
        host: env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: env::var("POSTGRES_TEST_PORT")
            .unwrap_or_else(|_| "55432".to_owned())
            .parse()
            .unwrap_or(55432),
        database,
        username: env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    }
}

async fn create_postgres_test_database()
-> Result<(DatabaseConfiguration, String), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", Uuid::now_v7().simple());
    let admin = postgres_connection("postgres".to_owned());
    let admin_configuration = DatabaseConfiguration::Postgres(admin);
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&pool)
    .await?;
    pool.close().await;
    Ok((
        DatabaseConfiguration::Postgres(postgres_connection(database_name.clone())),
        database_name,
    ))
}

async fn drop_postgres_test_database(
    database_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let admin = postgres_connection("postgres".to_owned());
    let admin_configuration = DatabaseConfiguration::Postgres(admin);
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&pool)
    .await?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_bootstrap_runs_migrations_and_persists_core_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;

    let database_url = connection.postgres_url()?.ok_or("missing PostgreSQL URL")?;
    let probe_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await?;
    sqlx::query("CREATE TABLE non_lux_application_table (id BIGINT PRIMARY KEY)")
        .execute(&probe_pool)
        .await?;
    let non_lux_result = Database::test_configuration(&connection).await;
    sqlx::query("DROP TABLE non_lux_application_table")
        .execute(&probe_pool)
        .await?;
    probe_pool.close().await;
    assert!(non_lux_result.is_err());

    let database = Database::connect_with_configuration(&config, &connection).await?;
    assert_eq!(database.backend(), luxd::config::DatabaseBackend::Postgres);
    assert!(database.schema_version().await? > 0);
    let chapter_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.tables
         WHERE table_schema = current_schema() AND table_name = 'media_chapters'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(chapter_table_count, 1);
    let chapter_job_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.tables
         WHERE table_schema = current_schema()
           AND table_name IN ('chapter_detection_jobs', 'chapter_detection_job_items')",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(chapter_job_table_count, 2);

    let setup = SetupService::new(database.clone())?;
    setup
        .complete("postgres-admin", "PostgreSQL Admin", "test-password")
        .await?;
    let auth = WebAuthService::new(database.clone())?;
    let login = auth
        .login("postgres-admin", "test-password")
        .await?
        .ok_or("PostgreSQL admin login failed")?;
    assert_eq!(login.user.username_normalized, "postgres-admin");
    assert!(auth.resolve(&login.session_token).await?.is_some());

    let library_id = uuid::Uuid::now_v7().to_string();
    let inserted = sqlx::query(
        "INSERT INTO libraries (
            id, name, kind, is_enabled, realtime_watch_enabled,
            scan_concurrency, probe_concurrency
        ) VALUES ($1, $2, $3, 1, 1, 2, 1)",
    )
    .bind(&library_id)
    .bind("PostgreSQL Test Library")
    .bind("MOVIE")
    .execute(database.pool())
    .await?;
    assert_eq!(inserted.rows_affected(), 1);

    let stored_name: String = sqlx::query_scalar("SELECT name FROM libraries WHERE id = $1")
        .bind(&library_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(stored_name, "PostgreSQL Test Library");

    let library_service = LibraryService::new(database.clone());
    let library_id = library_id.parse::<LibraryId>()?;
    let library = library_service
        .update_settings(
            library_id,
            LibrarySettingsPatch {
                is_enabled: Some(false),
                realtime_watch_enabled: Some(true),
                ..LibrarySettingsPatch::default()
            },
        )
        .await?;
    assert!(!library.library.is_enabled);
    assert!(library.library.realtime_watch_enabled);
    let library = library_service
        .update_settings(
            library_id,
            LibrarySettingsPatch {
                is_enabled: Some(true),
                ..LibrarySettingsPatch::default()
            },
        )
        .await?;
    assert!(library.library.is_enabled);

    let item_id = uuid::Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title, identification_status, has_available_source
        ) VALUES ($1, $2, 'MOVIE', 'Postgres Search Movie', 'postgres search movie', 'LOCAL_CONFIRMED', 1)",
    )
    .bind(&item_id)
    .bind(library_id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO item_aliases (id, item_id, alias, language, alias_normalized)
         VALUES ($1, $2, '银河搜索电影', 'zh-CN', '银河搜索电影')",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(&item_id)
    .execute(database.pool())
    .await?;

    let catalog = CatalogService::new(database.clone(), MediaAccessService::new(database.clone()));
    let page = catalog
        .search_items(
            AccessPrincipal::new(UserId::new(), true),
            "银河",
            "%银河%",
            0,
            10,
        )
        .await?;
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, item_id);
    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_rescan_of_existing_movie_uses_integer_boolean_projection()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;
    let libraries = LibraryService::new(database.clone());
    let library_name = format!("PostgreSQL rescan {}", uuid::Uuid::now_v7());
    let library = libraries
        .create_library(&library_name, luxd::library::LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Existing.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_movie_library(library.id).await?;
    scanner.scan_movie_library(library.id).await?;
    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_strm_probe_job_accepts_boolean_options() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;
    let libraries = LibraryService::new(database.clone());
    let library_name = format!("PostgreSQL STRM {}", uuid::Uuid::now_v7());
    let library = libraries
        .create_library(&library_name, luxd::library::LibraryKind::Movie, false)
        .await?;
    let plugins = PluginService::new(database.clone(), config.config_dir.clone());
    let service = StrmProbeService::new(database.clone(), plugins);
    let jobs = service
        .create_jobs(
            &[library.id],
            StrmProbeOptions {
                concurrency: 1,
                include_ready: false,
                write_sidecars: false,
                media_info_enabled: true,
                thumbnail_enabled: false,
                thumbnail_position_percent: 30,
            },
        )
        .await?;
    assert_eq!(jobs.len(), 1);
    assert!(!jobs[0].include_ready);
    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_metadata_priority_locks_images_and_people_regression()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library(
            &format!("PostgreSQL metadata {}", Uuid::now_v7()),
            luxd::library::LibraryKind::Movie,
            false,
        )
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Local Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Local.Movie.2024.mkv"), b"fixture").await?;
    tokio::fs::write(
        movie_dir.join("movie.nfo"),
        r#"<movie><title>本地标题</title><actor><name>本地演员</name><role>本地角色</role><order>0</order></actor></movie>"#,
    )
    .await?;
    tokio::fs::write(movie_dir.join("poster.jpg"), b"local-poster").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;

    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    MetadataEnricher::new(database.clone())
        .enrich_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items
         WHERE library_id = $1 AND item_type = 'MOVIE' AND removed_at IS NULL
         LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;

    sqlx::query("UPDATE media_items SET locked_fields_json = $1 WHERE id = $2")
        .bind(json!(["title"]).to_string())
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    let candidate_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO metadata_candidates (
            id, item_id, provider, provider_id, candidate_json, score, status
         ) VALUES ($1, $2, 'TMDB', '123', $3, 100, 'PENDING')",
    )
    .bind(&candidate_id)
    .bind(&item_id)
    .bind(
        json!({
            "title": "Online Title",
            "overview": "Online Overview",
            "posterUrl": "https://example.invalid/poster.jpg",
            "providerIds": {"Tmdb": "123"}
        })
        .to_string(),
    )
    .execute(database.pool())
    .await?;

    let image_writer =
        ImageWriteService::new_with_config_dir(database.clone(), config.config_dir.clone())?;
    let selection = MetadataSelectionService::with_config_dir(
        database.clone(),
        image_writer,
        config.config_dir.clone(),
    );
    selection
        .select(&item_id, &candidate_id, MetadataSelectionMode::FillMissing)
        .await?;

    let metadata: (String, String, String, String) = sqlx::query_as(
        "SELECT title, overview, locked_fields_json, metadata_provenance_json
         FROM media_items WHERE id = $1",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(metadata.0, "本地标题");
    assert_eq!(metadata.1, "Online Overview");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&metadata.2)?,
        json!(["title"])
    );
    let provenance: serde_json::Value = serde_json::from_str(&metadata.3)?;
    assert_eq!(provenance["title"], "LOCKED_LOCAL");
    assert_eq!(provenance["overview"], "SCRAPER_LOCALIZED");

    let image: (String, String, i64) = sqlx::query_as(
        "SELECT image_type, local_path, COUNT(*) OVER ()
         FROM item_images WHERE item_id = $1 AND image_type = 'POSTER'
         ORDER BY image_index LIMIT 1",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(image.0, "POSTER");
    let expected_image_path = tokio::fs::canonicalize(movie_dir.join("poster.jpg")).await?;
    assert_eq!(image.1, expected_image_path.to_string_lossy());
    assert_eq!(image.2, 1);
    assert_eq!(tokio::fs::read(&image.1).await?, b"local-poster");
    let attempted_images: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM metadata_image_attempts WHERE item_id = $1")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(attempted_images, 0);

    let people = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    let actors = people.list_item_actors(&item_id).await?;
    assert_eq!(actors.len(), 1);
    assert_eq!(actors[0].name, "本地演员");
    assert_eq!(actors[0].character.as_deref(), Some("本地角色"));
    let nfo = tokio::fs::read_to_string(movie_dir.join("movie.nfo")).await?;
    assert!(nfo.contains("<title>本地标题</title>"));
    assert!(nfo.contains("<plot>Online Overview</plot>"));

    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}
