//! Index audit for `media_items` (second-round W4, `docs/INDEX-AUDIT.md`).
//!
//! These tests pin the two things the audit changes: the `media_items` index inventory a database
//! ends up with after `Database::connect` runs the migrations *and* the legacy catalog repair, and
//! the query plans of the lookups that used the two indexes dropped by migration 0242.

use luxd::{config::Config, storage::Database};
use sqlx::Row;

static INDEX_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn test_database(
    temp_dir: &tempfile::TempDir,
) -> Result<Database, Box<dyn std::error::Error>> {
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    Ok(Database::connect(&config).await?)
}

async fn media_item_indexes(
    database: &Database,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'index' AND tbl_name = 'media_items' AND sql IS NOT NULL
         ORDER BY name",
    )
    .fetch_all(database.pool())
    .await?)
}

/// Runs `EXPLAIN QUERY PLAN` and returns the detail column.
///
/// The statements are literals from this file and never carry user input, so asserting the SQL is
/// safe is local to the test.
async fn explain(
    database: &Database,
    sql: impl Into<String>,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(sql.into()))
        .fetch_all(database.pool())
        .await?
        .into_iter()
        .filter_map(|row| row.try_get::<String, _>(3).ok())
        .collect())
}

/// The complete `media_items` index inventory after a fresh `Database::connect`.
///
/// This is deliberately explicit: the audit found that the legacy catalog repair recreates a
/// hardcoded index list, so the migrated schema and the repaired schema used to differ. The list
/// below is the repaired schema, which is what a new install keeps. `docs/INDEX-AUDIT.md` records
/// the three indexes that only survive because of the repair and the owner decision they need.
const EXPECTED_MEDIA_ITEM_INDEXES: [&str; 20] = [
    "idx_media_items_home_unavailable_series",
    "idx_media_items_identity_key",
    "idx_media_items_library_added_sort",
    "idx_media_items_library_added_visible",
    "idx_media_items_library_name_sort",
    "idx_media_items_library_premiere_sort",
    "idx_media_items_library_rating_sort",
    "idx_media_items_library_sort",
    "idx_media_items_library_sortname_sort",
    "idx_media_items_library_type_visible",
    "idx_media_items_merged_into",
    "idx_media_items_migration_title",
    "idx_media_items_nfo_collection_reference_pending",
    "idx_media_items_parent_available",
    "idx_media_items_parent_episode_latest",
    "idx_media_items_people_visible",
    "idx_media_items_removed_at",
    "idx_media_items_series_available",
    "idx_media_items_series_episode_latest",
    "idx_media_items_updated_at",
];

#[tokio::test]
async fn migrations_run_from_an_empty_database_and_settle_the_audited_indexes()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = INDEX_SERIAL.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;

    assert!(
        database.schema_version().await? >= 242,
        "the index audit migration must be applied"
    );

    let indexes = media_item_indexes(&database).await?;
    let expected: Vec<String> = EXPECTED_MEDIA_ITEM_INDEXES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(
        indexes, expected,
        "media_items index inventory drifted from docs/INDEX-AUDIT.md"
    );

    for dropped in [
        "idx_media_items_parent_removed",
        "idx_media_items_series_removed",
    ] {
        assert!(
            !indexes.iter().any(|name| name == dropped),
            "{dropped} is redundant with the wider availability index and must stay dropped"
        );
    }
    Ok(())
}

#[tokio::test]
async fn parent_and_series_child_lookups_use_the_wider_indexes()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = INDEX_SERIAL.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    let mut transaction = database.pool().begin().await?;
    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('audit-lib', 'Audit', 'SERIES')")
        .execute(&mut *transaction)
        .await?;
    // Enough rows for the planner to prefer the indexes over a table scan. The mix of item types
    // matters: with episodes as half the table the planner prefers the title index for
    // `item_type = 'EPISODE'` instead of the parent/series index the audit is about.
    for movie in 0..400 {
        let movie_id = format!("audit-movie-{movie:03}");
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title,
                 identification_status, added_at, has_available_source
             ) VALUES (?, 'audit-lib', 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1_700_000_000, 1)",
        )
        .bind(&movie_id)
        .bind(&movie_id)
        .bind(&movie_id)
        .execute(&mut *transaction)
        .await?;
    }
    for series in 0..4 {
        let series_id = format!("audit-series-{series}");
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title,
                 identification_status, added_at, has_available_source
             ) VALUES (?, 'audit-lib', 'SERIES', ?, ?, 'LOCAL_CONFIRMED', 1_700_000_000, 1)",
        )
        .bind(&series_id)
        .bind(&series_id)
        .bind(&series_id)
        .execute(&mut *transaction)
        .await?;
        for season in 0..3 {
            let season_id = format!("audit-season-{series}-{season}");
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, parent_id, series_id,
                     identification_status, added_at, has_available_source
                 ) VALUES (?, 'audit-lib', 'SEASON', ?, ?, ?, ?, 'LOCAL_CONFIRMED', 1_700_000_000, 1)",
            )
            .bind(&season_id)
            .bind(&season_id)
            .bind(&season_id)
            .bind(&series_id)
            .bind(&series_id)
            .execute(&mut *transaction)
            .await?;
            for episode in 0..20 {
                let episode_id = format!("audit-episode-{series}-{season}-{episode}");
                sqlx::query(
                    "INSERT INTO media_items (
                         id, library_id, item_type, title, sort_title, parent_id, series_id,
                         identification_status, added_at, has_available_source
                     ) VALUES (?, 'audit-lib', 'EPISODE', ?, ?, ?, ?, 'LOCAL_CONFIRMED', ?, 1)",
                )
                .bind(&episode_id)
                .bind(&episode_id)
                .bind(&episode_id)
                .bind(&season_id)
                .bind(&series_id)
                .bind(1_700_000_000_i64 + i64::from(episode))
                .execute(&mut *transaction)
                .await?;
            }
        }
    }
    transaction.commit().await?;
    sqlx::query("ANALYZE").execute(database.pool()).await?;

    // `count_catalog_children`: the shape that used idx_media_items_parent_removed. SQLite picks
    // between idx_media_items_parent_available and idx_media_items_migration_title here depending
    // on the version and the ANALYZE sample, so the assertion is the property that matters: an
    // index on media_items answers the lookup and it never degrades to a table scan.
    let plan = explain(
        &database,
        "EXPLAIN QUERY PLAN
         SELECT COUNT(*) FROM media_items mi
         JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
         WHERE mi.parent_id = 'audit-season-0-0' AND mi.item_type IN ('EPISODE')
           AND mi.removed_at IS NULL",
    )
    .await?;
    assert!(
        plan.iter()
            .any(|detail| detail.contains("USING INDEX idx_media_items_")),
        "parent lookups must stay on a media_items index: {plan:?}"
    );
    assert!(
        !plan.iter().any(|detail| detail.contains("SCAN mi")),
        "parent lookups must not fall back to a table scan: {plan:?}"
    );

    // `list_series_episode_ids`: the shape that used idx_media_items_series_removed.
    let plan = explain(
        &database,
        "EXPLAIN QUERY PLAN
         SELECT mi.id FROM media_items mi
         JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
         WHERE mi.series_id = 'audit-series-0' AND mi.item_type = 'EPISODE'
           AND mi.removed_at IS NULL
         ORDER BY mi.season_number, mi.episode_number, mi.sort_title, mi.id
         LIMIT 24 OFFSET 0",
    )
    .await?;
    assert!(
        plan.iter()
            .any(|detail| detail.contains("idx_media_items_series_available")),
        "series lookups must use idx_media_items_series_available: {plan:?}"
    );

    // `sync_played_container_states_for_scope`: both equality predicates stay on the index.
    let plan = explain(
        &database,
        "EXPLAIN QUERY PLAN
         SELECT episode.id FROM media_items episode
         WHERE episode.item_type = 'EPISODE'
           AND episode.removed_at IS NULL
           AND episode.has_available_source = 1
           AND episode.parent_id = 'audit-season-0-0'",
    )
    .await?;
    assert!(
        plan.iter().any(|detail| {
            detail.contains("idx_media_items_parent_available")
                && detail.contains("has_available_source")
        }),
        "the played-container aggregate must keep the availability column on the index: {plan:?}"
    );
    Ok(())
}

/// The redundancy argument itself: the surviving `*_available` indexes answer the narrow
/// `(parent_id, removed_at)` / `(series_id, removed_at)` predicates without a sort or a table scan.
/// `INDEXED BY` forces the plan, so this assertion does not depend on planner heuristics.
#[tokio::test]
async fn the_surviving_availability_indexes_answer_the_dropped_indexes_predicates()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = INDEX_SERIAL.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;

    for (index, column, value) in [
        ("idx_media_items_parent_available", "parent_id", "season-a"),
        ("idx_media_items_series_available", "series_id", "series-a"),
    ] {
        let plan = explain(
            &database,
            format!(
                "EXPLAIN QUERY PLAN
                 SELECT COUNT(*) FROM media_items INDEXED BY {index}
                 WHERE {column} = '{value}' AND removed_at IS NULL"
            ),
        )
        .await?;
        assert!(
            plan.iter()
                .any(|detail| detail.contains(index) && detail.contains("removed_at")),
            "{index} must answer the dropped index predicate: {plan:?}"
        );
        assert!(
            !plan
                .iter()
                .any(|detail| detail.contains("USING TEMP B-TREE")),
            "{index} must not need a temporary sort: {plan:?}"
        );
    }
    Ok(())
}
