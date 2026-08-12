//! Offline SQLite to PostgreSQL migration support.

use std::{fmt, path::PathBuf, time::Instant};

use serde::Serialize;
use sqlx::{
    AssertSqlSafe, Executor, PgPool, Postgres, QueryBuilder, Row, Sqlite, SqlitePool, Transaction,
    ValueRef,
    postgres::PgPoolOptions,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};

use crate::config::{DatabaseConfiguration, PostgresConnection};

use super::{POSTGRES_MIGRATOR, SQLITE_MIGRATOR};

const MAX_BATCH_SIZE: usize = 10_000;

/// A persistent Lux business table copied during an offline migration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigrationTable {
    pub name: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationOptions {
    pub source: PathBuf,
    pub batch_size: usize,
}

impl MigrationOptions {
    pub fn new(source: PathBuf, batch_size: usize) -> Result<Self, MigrationError> {
        Ok(Self {
            source,
            batch_size: validate_batch_size(batch_size)?,
        })
    }
}

#[derive(Debug)]
pub enum MigrationError {
    InvalidBatchSize,
    InvalidSource(String),
    TargetNotEmpty(String),
    SchemaMismatch {
        backend: &'static str,
        actual: i64,
        expected: i64,
    },
    Configuration(String),
    Database(String),
    Verification {
        table: String,
        source: i64,
        target: i64,
    },
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBatchSize => formatter.write_str("migration batch size must be 1..=10000"),
            Self::InvalidSource(message) => write!(formatter, "invalid SQLite source: {message}"),
            Self::TargetNotEmpty(table) => {
                write!(formatter, "PostgreSQL target table is not empty: {table}")
            }
            Self::SchemaMismatch {
                backend,
                actual,
                expected,
            } => write!(
                formatter,
                "{backend} schema version mismatch: found {actual}, expected {expected}"
            ),
            Self::Configuration(message) => {
                write!(formatter, "invalid PostgreSQL configuration: {message}")
            }
            Self::Database(message) => write!(formatter, "database migration failed: {message}"),
            Self::Verification {
                table,
                source,
                target,
            } => write!(
                formatter,
                "row count mismatch for {table}: SQLite {source}, PostgreSQL {target}"
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableMigrationReport {
    pub table: String,
    pub rows: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationReport {
    pub schema_version: i64,
    pub tables: Vec<TableMigrationReport>,
    pub elapsed_ms: u128,
}

#[derive(Clone, Debug)]
enum SqliteValue {
    Null(ColumnKind),
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ColumnKind {
    Integer,
    Real,
    Text,
    Blob,
}

#[derive(Clone, Debug)]
struct SourceColumn {
    name: String,
    kind: ColumnKind,
    primary_key_position: i64,
}

pub async fn migrate_sqlite_to_postgres(
    options: &MigrationOptions,
    connection: &PostgresConnection,
) -> Result<MigrationReport, MigrationError> {
    let started = Instant::now();
    let source = connect_source(options).await?;
    let target = connect_target(connection).await?;
    if let Err(error) = POSTGRES_MIGRATOR.run(&target).await {
        source.close().await;
        target.close().await;
        return Err(MigrationError::Database(error.to_string()));
    }

    let result = migrate_pools(&source, &target, options.batch_size, started).await;
    source.close().await;
    target.close().await;
    result
}

async fn connect_source(options: &MigrationOptions) -> Result<SqlitePool, MigrationError> {
    let metadata = std::fs::metadata(&options.source)
        .map_err(|error| MigrationError::InvalidSource(error.to_string()))?;
    if !metadata.is_file() {
        return Err(MigrationError::InvalidSource(
            "source is not a regular file".to_owned(),
        ));
    }
    let connect = SqliteConnectOptions::new()
        .filename(&options.source)
        .read_only(true)
        .create_if_missing(false);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect)
        .await
        .map_err(database_error)
}

async fn connect_target(connection: &PostgresConnection) -> Result<PgPool, MigrationError> {
    let configuration = DatabaseConfiguration::Postgres(connection.clone());
    let url = configuration
        .postgres_url()
        .map_err(|error| MigrationError::Configuration(error.to_string()))?
        .ok_or_else(|| MigrationError::Configuration("connection is missing".to_owned()))?;
    PgPoolOptions::new()
        .max_connections(2)
        .after_connect(|connection, _| {
            Box::pin(async move {
                connection.execute("SET TIME ZONE 'UTC'").await?;
                connection
                    .execute("SET application_name = 'lux-db-migrate'")
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .map_err(database_error)
}

async fn migrate_pools(
    source_pool: &SqlitePool,
    target: &PgPool,
    batch_size: usize,
    started: Instant,
) -> Result<MigrationReport, MigrationError> {
    validate_target_empty(target).await?;
    let mut source = source_pool.begin().await.map_err(database_error)?;
    let source_version = schema_version_sqlite(&mut source).await?;
    let target_version = schema_version_postgres(target).await?;
    let expected_source = latest_version(&SQLITE_MIGRATOR, "SQLite")?;
    let expected_target = latest_version(&POSTGRES_MIGRATOR, "PostgreSQL")?;
    if source_version != expected_source {
        return Err(MigrationError::SchemaMismatch {
            backend: "SQLite",
            actual: source_version,
            expected: expected_source,
        });
    }
    if target_version != expected_target {
        return Err(MigrationError::SchemaMismatch {
            backend: "PostgreSQL",
            actual: target_version,
            expected: expected_target,
        });
    }

    let mut transaction = target.begin().await.map_err(database_error)?;
    sqlx::query("DELETE FROM lux_meta")
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    let mut reports = Vec::with_capacity(MIGRATION_TABLES.len());
    for table in MIGRATION_TABLES {
        let rows = copy_table(&mut source, &mut transaction, *table, batch_size).await?;
        reports.push(TableMigrationReport {
            table: table.name.to_owned(),
            rows,
        });
    }
    normalize_active_jobs(&mut transaction).await?;
    rebuild_postgres_search(&mut transaction).await?;
    verify_counts(&mut source, &mut transaction, &reports).await?;
    source.rollback().await.map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(MigrationReport {
        schema_version: source_version,
        tables: reports,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

async fn validate_target_empty(target: &PgPool) -> Result<(), MigrationError> {
    for table in MIGRATION_TABLES
        .iter()
        .filter(|table| table.name != "lux_meta")
    {
        let query = format!(
            "SELECT EXISTS(SELECT 1 FROM {} LIMIT 1)",
            quote_identifier(table.name)?
        );
        // SAFETY: the table comes from MIGRATION_TABLES and quote_identifier
        // accepts only ASCII alphanumeric characters and underscores.
        let has_rows: bool = sqlx::query_scalar(AssertSqlSafe(query))
            .fetch_one(target)
            .await
            .map_err(database_error)?;
        if has_rows {
            return Err(MigrationError::TargetNotEmpty(table.name.to_owned()));
        }
    }
    Ok(())
}

async fn schema_version_sqlite(
    source: &mut Transaction<'_, Sqlite>,
) -> Result<i64, MigrationError> {
    sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations")
        .fetch_one(&mut **source)
        .await
        .map_err(database_error)
}

async fn schema_version_postgres(target: &PgPool) -> Result<i64, MigrationError> {
    sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations")
        .fetch_one(target)
        .await
        .map_err(database_error)
}

async fn copy_table(
    source: &mut Transaction<'_, Sqlite>,
    target: &mut Transaction<'_, Postgres>,
    table: MigrationTable,
    requested_batch_size: usize,
) -> Result<i64, MigrationError> {
    let columns = source_columns(source, table.name).await?;
    if columns.is_empty() {
        return Err(MigrationError::Database(format!(
            "source table has no columns: {}",
            table.name
        )));
    }
    validate_target_columns(target, table.name, &columns).await?;
    let quoted_table = quote_identifier(table.name)?;
    let quoted_columns = columns
        .iter()
        .map(|column| quote_identifier(&column.name))
        .collect::<Result<Vec<_>, _>>()?;
    let mut primary_key = columns
        .iter()
        .filter(|column| column.primary_key_position > 0)
        .collect::<Vec<_>>();
    primary_key.sort_by_key(|column| column.primary_key_position);
    if primary_key.is_empty() {
        return Err(MigrationError::Database(format!(
            "source table has no primary key: {}",
            table.name
        )));
    }
    let order_by = primary_key
        .iter()
        .map(|column| quote_identifier(&column.name))
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    let select = format!(
        "SELECT {} FROM {quoted_table} ORDER BY {order_by} LIMIT ? OFFSET ?",
        quoted_columns.join(", "),
    );
    let batch_size = requested_batch_size.min((60_000 / columns.len()).max(1));
    let mut offset = 0_i64;
    loop {
        // SAFETY: table and column identifiers are allowlisted/validated; row
        // limits and offsets remain bind parameters.
        let rows = sqlx::query(AssertSqlSafe(select.clone()))
            .bind(i64::try_from(batch_size).unwrap_or(i64::MAX))
            .bind(offset)
            .fetch_all(&mut **source)
            .await
            .map_err(database_error)?;
        if rows.is_empty() {
            break;
        }
        let values = rows
            .iter()
            .map(|row| read_source_row(row, &columns))
            .collect::<Result<Vec<_>, _>>()?;
        let mut builder = QueryBuilder::<Postgres>::new(format!(
            "INSERT INTO {quoted_table} ({}) ",
            quoted_columns.join(", ")
        ));
        builder.push_values(values.iter(), |mut separated, row| {
            for value in row {
                match value {
                    SqliteValue::Null(ColumnKind::Integer) => {
                        separated.push_bind(Option::<i64>::None);
                    }
                    SqliteValue::Null(ColumnKind::Real) => {
                        separated.push_bind(Option::<f64>::None);
                    }
                    SqliteValue::Null(ColumnKind::Text) => {
                        separated.push_bind(Option::<String>::None);
                    }
                    SqliteValue::Null(ColumnKind::Blob) => {
                        separated.push_bind(Option::<Vec<u8>>::None);
                    }
                    SqliteValue::Integer(value) => {
                        separated.push_bind(*value);
                    }
                    SqliteValue::Real(value) => {
                        separated.push_bind(*value);
                    }
                    SqliteValue::Text(value) => {
                        separated.push_bind(value);
                    }
                    SqliteValue::Blob(value) => {
                        separated.push_bind(value);
                    }
                }
            }
        });
        builder
            .build()
            .execute(&mut **target)
            .await
            .map_err(database_error)?;
        let count = i64::try_from(rows.len()).unwrap_or(i64::MAX);
        offset = offset.saturating_add(count);
        if rows.len() < batch_size {
            break;
        }
    }
    Ok(offset)
}

async fn validate_target_columns(
    target: &mut Transaction<'_, Postgres>,
    table: &str,
    source_columns: &[SourceColumn],
) -> Result<(), MigrationError> {
    let target_columns: Vec<(String, String)> = sqlx::query_as(
        "SELECT column_name, data_type
         FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = $1
         ORDER BY ordinal_position",
    )
    .bind(table)
    .fetch_all(&mut **target)
    .await
    .map_err(database_error)?;
    if target_columns.len() != source_columns.len() {
        return Err(MigrationError::Database(format!(
            "schema column mismatch for {table}: SQLite {}, PostgreSQL {}",
            source_columns.len(),
            target_columns.len()
        )));
    }
    for (source, (target_name, target_type)) in source_columns.iter().zip(&target_columns) {
        if source.name != *target_name || !postgres_type_matches(source.kind, target_type) {
            return Err(MigrationError::Database(format!(
                "schema column mismatch for {table}.{}",
                source.name
            )));
        }
    }
    Ok(())
}

fn postgres_type_matches(source: ColumnKind, target: &str) -> bool {
    matches!(
        (source, target),
        (ColumnKind::Integer, "bigint")
            | (ColumnKind::Real, "double precision")
            | (ColumnKind::Text, "text")
            | (ColumnKind::Blob, "bytea")
    )
}

async fn source_columns(
    source: &mut Transaction<'_, Sqlite>,
    table: &str,
) -> Result<Vec<SourceColumn>, MigrationError> {
    let query = format!("PRAGMA table_info({})", quote_identifier(table)?);
    // SAFETY: the table is allowlisted and validated by quote_identifier.
    let rows = sqlx::query(AssertSqlSafe(query))
        .fetch_all(&mut **source)
        .await
        .map_err(database_error)?;
    rows.into_iter()
        .map(|row| {
            let name: String = row.try_get("name").map_err(database_error)?;
            let declared: String = row.try_get("type").map_err(database_error)?;
            let primary_key_position: i64 = row.try_get("pk").map_err(database_error)?;
            Ok(SourceColumn {
                name,
                kind: column_kind(&declared)?,
                primary_key_position,
            })
        })
        .collect()
}

fn column_kind(declared: &str) -> Result<ColumnKind, MigrationError> {
    let declared = declared.to_ascii_uppercase();
    if declared.contains("INT") {
        Ok(ColumnKind::Integer)
    } else if declared.contains("REAL") || declared.contains("FLOA") || declared.contains("DOUB") {
        Ok(ColumnKind::Real)
    } else if declared.contains("BLOB") {
        Ok(ColumnKind::Blob)
    } else if declared.contains("CHAR") || declared.contains("CLOB") || declared.contains("TEXT") {
        Ok(ColumnKind::Text)
    } else {
        Err(MigrationError::Database(format!(
            "unsupported SQLite column type: {declared}"
        )))
    }
}

fn read_source_row(
    row: &sqlx::sqlite::SqliteRow,
    columns: &[SourceColumn],
) -> Result<Vec<SqliteValue>, MigrationError> {
    columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let raw = row.try_get_raw(index).map_err(database_error)?;
            if raw.is_null() {
                return Ok(SqliteValue::Null(column.kind));
            }
            match column.kind {
                ColumnKind::Integer => row
                    .try_get(index)
                    .map(SqliteValue::Integer)
                    .map_err(database_error),
                ColumnKind::Real => row
                    .try_get(index)
                    .map(SqliteValue::Real)
                    .map_err(database_error),
                ColumnKind::Text => row
                    .try_get(index)
                    .map(SqliteValue::Text)
                    .map_err(database_error),
                ColumnKind::Blob => row
                    .try_get(index)
                    .map(SqliteValue::Blob)
                    .map_err(database_error),
            }
        })
        .collect()
}

async fn normalize_active_jobs(
    target: &mut Transaction<'_, Postgres>,
) -> Result<(), MigrationError> {
    for (table, queued) in [
        ("scan_jobs", "PENDING"),
        ("metadata_reidentify_jobs", "QUEUED"),
        ("strm_probe_jobs", "PENDING"),
        ("danmaku_match_jobs", "PENDING"),
    ] {
        let query = format!(
            "UPDATE {} SET status = $1 WHERE status = 'RUNNING'",
            quote_identifier(table)?
        );
        // SAFETY: the table is selected from the static job table list.
        sqlx::query(AssertSqlSafe(query))
            .bind(queued)
            .execute(&mut **target)
            .await
            .map_err(database_error)?;
    }
    for table in ["metadata_reidentify_job_items", "danmaku_match_job_items"] {
        let query = format!(
            "UPDATE {} SET status = 'PENDING' WHERE status = 'RUNNING'",
            quote_identifier(table)?
        );
        // SAFETY: the table is selected from the static job item table list.
        sqlx::query(AssertSqlSafe(query))
            .execute(&mut **target)
            .await
            .map_err(database_error)?;
    }
    Ok(())
}

async fn rebuild_postgres_search(
    target: &mut Transaction<'_, Postgres>,
) -> Result<(), MigrationError> {
    sqlx::query("DELETE FROM media_search")
        .execute(&mut **target)
        .await
        .map_err(database_error)?;
    sqlx::query(
        "INSERT INTO media_search (item_id, title, sort_title, original_title, aliases)
         SELECT mi.id, mi.title, mi.sort_title, COALESCE(mi.original_title, ''),
                COALESCE(string_agg(ia.alias, ' ' ORDER BY ia.id), '')
         FROM media_items mi LEFT JOIN item_aliases ia ON ia.item_id = mi.id
         GROUP BY mi.id, mi.title, mi.sort_title, mi.original_title",
    )
    .execute(&mut **target)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn verify_counts(
    source: &mut Transaction<'_, Sqlite>,
    target: &mut Transaction<'_, Postgres>,
    reports: &[TableMigrationReport],
) -> Result<(), MigrationError> {
    for report in reports {
        let table = quote_identifier(&report.table)?;
        // SAFETY: the table comes from the static migration report and passed
        // quote_identifier before interpolation.
        let source_count: i64 =
            sqlx::query_scalar(AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(&mut **source)
                .await
                .map_err(database_error)?;
        let target_count: i64 =
            sqlx::query_scalar(AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(&mut **target)
                .await
                .map_err(database_error)?;
        if source_count != target_count || source_count != report.rows {
            return Err(MigrationError::Verification {
                table: report.table.clone(),
                source: source_count,
                target: target_count,
            });
        }
    }
    Ok(())
}

fn quote_identifier(identifier: &str) -> Result<String, MigrationError> {
    if identifier.is_empty()
        || !identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(MigrationError::Database(
            "unsafe schema identifier".to_owned(),
        ));
    }
    Ok(format!("\"{identifier}\""))
}

fn database_error(error: impl fmt::Display) -> MigrationError {
    MigrationError::Database(error.to_string())
}

fn latest_version(
    migrator: &'static sqlx::migrate::Migrator,
    backend: &'static str,
) -> Result<i64, MigrationError> {
    migrator
        .iter()
        .map(|migration| migration.version)
        .max()
        .ok_or_else(|| MigrationError::Database(format!("{backend} migrations are empty")))
}

impl std::error::Error for MigrationError {}

pub fn validate_batch_size(batch_size: usize) -> Result<usize, MigrationError> {
    if (1..=MAX_BATCH_SIZE).contains(&batch_size) {
        Ok(batch_size)
    } else {
        Err(MigrationError::InvalidBatchSize)
    }
}

pub fn connection_from_environment(
    read: impl Fn(&str) -> Option<String>,
) -> Result<PostgresConnection, MigrationError> {
    fn required(
        read: &impl Fn(&str) -> Option<String>,
        name: &str,
    ) -> Result<String, MigrationError> {
        read(name).filter(|value| !value.is_empty()).ok_or_else(|| {
            MigrationError::Configuration(format!(
                "required environment variable is missing: {name}"
            ))
        })
    }

    let port_name = "LUX_MIGRATE_POSTGRES_PORT";
    let port = required(&read, port_name)?.parse::<u16>().map_err(|_| {
        MigrationError::Configuration(format!("invalid environment variable: {port_name}"))
    })?;
    let connection = PostgresConnection {
        host: required(&read, "LUX_MIGRATE_POSTGRES_HOST")?,
        port,
        database: required(&read, "LUX_MIGRATE_POSTGRES_DATABASE")?,
        username: required(&read, "LUX_MIGRATE_POSTGRES_USER")?,
        password: required(&read, "LUX_MIGRATE_POSTGRES_PASSWORD")?,
        ssl_mode: required(&read, "LUX_MIGRATE_POSTGRES_SSL_MODE")?,
    };
    DatabaseConfiguration::Postgres(connection.clone())
        .validate()
        .map_err(|error| MigrationError::Configuration(error.to_string()))?;
    Ok(connection)
}

/// Parent-before-child order for every persistent business table shared by the
/// SQLite and PostgreSQL schemas. Derived search data is rebuilt separately.
pub const MIGRATION_TABLES: &[MigrationTable] = &[
    table("lux_meta"),
    table("users"),
    table("libraries"),
    table("library_roots"),
    table("filesystem_entries"),
    table("media_items"),
    table("media_sources"),
    table("web_sessions"),
    table("access_tokens"),
    table("item_images"),
    table("media_streams"),
    table("user_library_access"),
    table("scan_jobs"),
    table("scheduled_task_configs"),
    table("metadata_candidates"),
    table("user_item_state"),
    table("playback_sessions"),
    table("server_settings"),
    table("item_aliases"),
    table("collections"),
    table("collection_items"),
    table("audit_events"),
    table("scan_job_events"),
    table("metadata_reidentify_jobs"),
    table("metadata_reidentify_job_items"),
    table("installed_plugins"),
    table("strm_probe_jobs"),
    table("danmaku_tracks"),
    table("danmaku_match_jobs"),
    table("danmaku_match_job_items"),
    table("scan_job_paths"),
    table("reconciliation_scan_entries"),
];

const fn table(name: &'static str) -> MigrationTable {
    MigrationTable { name }
}

pub fn is_excluded_sqlite_table(name: &str) -> bool {
    name == "_sqlx_migrations"
        || name == "sqlite_sequence"
        || name == "media_search"
        || name.starts_with("media_search_")
}

pub fn normalized_job_tables() -> &'static [&'static str] {
    &[
        "scan_jobs",
        "metadata_reidentify_jobs",
        "strm_probe_jobs",
        "danmaku_match_jobs",
    ]
}
