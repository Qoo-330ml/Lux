use super::*;
use serde_json::{Value, json};
use std::time::Duration;

const MAX_DIAGNOSTIC_RELATIONS: i64 = 10_000;
const MAX_POSTGRES_STATEMENTS: i64 = 20;
const POSTGRES_STATEMENTS_TIMEOUT: Duration = Duration::from_secs(5);

/// Row of the SQLite `dbstat` size aggregation.
///
/// `dbstat.name` is NULL for pages that belong to no table or index (freelist and overflow
/// pages), which the previous lenient read collapsed into the empty object name; it stays
/// optional here so the report is unchanged.
#[derive(Debug, sqlx::FromRow)]
struct SqliteDbstatObjectRow {
    name: Option<String>,
    bytes: i64,
    payload_bytes: i64,
    unused_bytes: i64,
    pages: i64,
}

/// Row of the SQLite `sqlite_master` index listing.
#[derive(Debug, sqlx::FromRow)]
struct SqliteSchemaIndexRow {
    name: String,
    tbl_name: String,
}

/// Row of the PostgreSQL relation/table size listing.
///
/// The statistics columns come from a `LEFT JOIN pg_stat_user_tables`, so they are all nullable
/// even though the relation itself always has a schema and a name.
#[derive(Debug, sqlx::FromRow)]
struct PostgresRelationSizeRow {
    schema_name: String,
    relation_name: String,
    heap_bytes: i64,
    toast_bytes: i64,
    index_bytes: i64,
    total_bytes: i64,
    live_tuples: Option<i64>,
    dead_tuples: Option<i64>,
    last_analyze: Option<String>,
    last_autoanalyze: Option<String>,
    last_vacuum: Option<String>,
    last_autovacuum: Option<String>,
}

/// Row of the PostgreSQL index size/usage listing.
///
/// `idx_scan` comes from a `LEFT JOIN pg_stat_user_indexes`, so a never-analyzed index reports
/// NULL; the previous decoder defaulted it to 0 and this struct keeps that.
#[derive(Debug, sqlx::FromRow)]
struct PostgresIndexSizeRow {
    schema_name: String,
    table_name: String,
    index_name: String,
    index_bytes: i64,
    index_scans: Option<i64>,
    is_valid: bool,
}

/// Row of the PostgreSQL `pg_stat_statements` top-statement listing.
#[derive(Debug, sqlx::FromRow)]
struct PostgresStatementStatRow {
    query_id: String,
    calls: i64,
    total_exec_time_ms: f64,
    mean_exec_time_ms: f64,
    max_exec_time_ms: f64,
    rows: i64,
    shared_blocks_hit: i64,
    shared_blocks_read: i64,
}

#[derive(Clone, Debug)]
pub struct DatabaseDiagnosticsSnapshot {
    pub collected_at: i64,
    pub details: Value,
}

impl Database {
    pub async fn collect_database_diagnostics(
        &self,
        include_relation_sizes: bool,
    ) -> Result<DatabaseDiagnosticsSnapshot, StorageError> {
        let details = match self.backend {
            DatabaseBackend::Sqlite => {
                self.collect_sqlite_diagnostics(include_relation_sizes)
                    .await?
            }
            DatabaseBackend::Postgres => self.collect_postgres_diagnostics().await?,
        };
        Ok(DatabaseDiagnosticsSnapshot {
            collected_at: current_unix_timestamp(),
            details,
        })
    }

    async fn collect_sqlite_diagnostics(
        &self,
        include_relation_sizes: bool,
    ) -> Result<Value, StorageError> {
        let page_size: i64 = self
            .query_scalar("PRAGMA page_size")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| self.diagnostics_error(source))?;
        let page_count: i64 = self
            .query_scalar("PRAGMA page_count")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| self.diagnostics_error(source))?;
        let freelist_count: i64 = self
            .query_scalar("PRAGMA freelist_count")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| self.diagnostics_error(source))?;
        let engine_version: String = self
            .query_scalar("SELECT sqlite_version()")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| self.diagnostics_error(source))?;
        let database_bytes = fs::metadata(&self.path)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let wal_path = PathBuf::from(format!("{}-wal", self.path.display()));
        let wal_bytes = fs::metadata(wal_path)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let mut relations = Vec::new();
        let tables: Vec<String> = self
            .query_scalar(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?",
            )
            .bind(MAX_DIAGNOSTIC_RELATIONS)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| self.diagnostics_error(source))?;
        let indexes = self
            .query_as::<SqliteSchemaIndexRow>(
                "SELECT name, tbl_name FROM sqlite_master
             WHERE type = 'index' AND tbl_name NOT LIKE 'sqlite_%'
             ORDER BY name LIMIT ?",
            )
            .bind(MAX_DIAGNOSTIC_RELATIONS)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| self.diagnostics_error(source))?;

        let (dbstat_available, mut object_sizes) = if include_relation_sizes {
            let dbstat_rows = tokio::time::timeout(
                Duration::from_secs(90),
                self.query_as::<SqliteDbstatObjectRow>(
                    "SELECT name, SUM(pgsize) AS bytes, SUM(payload) AS payload_bytes,
                        SUM(unused) AS unused_bytes, COUNT(*) AS pages
                 FROM dbstat GROUP BY name ORDER BY SUM(pgsize) DESC LIMIT ?",
                )
                .bind(MAX_DIAGNOSTIC_RELATIONS)
                .fetch_all(&self.pool),
            )
            .await;
            match dbstat_rows {
                Ok(Ok(rows)) => {
                    let mut sizes = HashMap::new();
                    for row in rows {
                        sizes.insert(
                            row.name.unwrap_or_default(),
                            json!({
                                "bytes": row.bytes,
                                "payloadBytes": row.payload_bytes,
                                "unusedBytes": row.unused_bytes,
                                "pages": row.pages,
                            }),
                        );
                    }
                    (true, sizes)
                }
                _ => (false, HashMap::new()),
            }
        } else {
            (false, HashMap::new())
        };

        let mut index_details = Vec::new();
        let mut index_bytes_by_table = HashMap::<String, i64>::new();
        for row in indexes {
            let size = object_sizes.remove(&row.name).unwrap_or(Value::Null);
            let index_bytes = size["bytes"].as_i64().unwrap_or_default();
            *index_bytes_by_table
                .entry(row.tbl_name.clone())
                .or_default() += index_bytes;
            index_details.push(json!({ "name": row.name, "table": row.tbl_name, "size": size }));
        }
        for name in tables {
            let table_size = object_sizes.remove(&name).unwrap_or(Value::Null);
            let table_bytes = table_size["bytes"].as_i64();
            let index_bytes = index_bytes_by_table.get(&name).copied().unwrap_or_default();
            let total_bytes = table_bytes.map(|bytes| bytes.saturating_add(index_bytes));
            relations.push(json!({
                "schema": "main", "name": name, "kind": "TABLE",
                "size": {
                    "tableBytes": table_bytes,
                    "indexBytes": if dbstat_available { Some(index_bytes) } else { None },
                    "totalBytes": total_bytes,
                    "payloadBytes": table_size["payloadBytes"],
                    "unusedBytes": table_size["unusedBytes"],
                    "pages": table_size["pages"],
                },
                "rowCountEstimate": null, "rowCountKind": "UNAVAILABLE",
                "liveTupleEstimate": null, "deadTupleEstimate": null,
            }));
        }
        relations.sort_by(|left, right| {
            right["size"]["totalBytes"]
                .as_i64()
                .unwrap_or_default()
                .cmp(&left["size"]["totalBytes"].as_i64().unwrap_or_default())
        });
        index_details.sort_by(|left, right| {
            right["size"]["bytes"]
                .as_i64()
                .unwrap_or_default()
                .cmp(&left["size"]["bytes"].as_i64().unwrap_or_default())
        });
        let relations_truncated = relations.len() as i64 >= MAX_DIAGNOSTIC_RELATIONS;
        let indexes_truncated = index_details.len() as i64 >= MAX_DIAGNOSTIC_RELATIONS;
        let schema_sizes = json!([{ "schema": "main", "totalBytes": relations.iter()
            .filter_map(|relation| relation["size"]["totalBytes"].as_i64())
            .fold(0_i64, i64::saturating_add) }]);

        Ok(json!({
            "backend": "SQLITE", "engineVersion": engine_version,
            "databaseName": "embedded", "databaseBytes": database_bytes,
            "databaseBytesKind": "EXACT_FILE_SIZE", "walBytes": wal_bytes,
            "pageSizeBytes": page_size, "pageCount": page_count,
            "freelistPages": freelist_count, "relations": relations,
            "indexes": index_details, "schemaSizes": schema_sizes,
            "queryStatistics": { "status": "NOT_APPLICABLE", "topStatements": [] },
            "relationSizesAvailable": dbstat_available,
            "relationSizesNote": if dbstat_available {
                "SQLite dbstat"
            } else if include_relation_sizes {
                "SQLite dbstat unavailable"
            } else {
                "SQLite dbstat skipped for automatic collection"
            },
            "relationsTruncated": relations_truncated,
            "indexesTruncated": indexes_truncated,
        }))
    }

    async fn collect_postgres_diagnostics(&self) -> Result<Value, StorageError> {
        let engine_version: String = self
            .query_scalar("SELECT current_setting('server_version')")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| self.diagnostics_error(source))?;
        let database_name: String = self
            .query_scalar("SELECT current_database()::TEXT")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| self.diagnostics_error(source))?;
        let database_bytes: i64 = self
            .query_scalar("SELECT pg_database_size(current_database())::BIGINT")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| self.diagnostics_error(source))?;
        let relation_rows = self
            .query_as::<PostgresRelationSizeRow>(
                "SELECT ns.nspname::TEXT AS schema_name, cls.relname::TEXT AS relation_name,
                    pg_relation_size(cls.oid)::BIGINT AS heap_bytes,
                    CASE WHEN cls.reltoastrelid = 0 THEN 0
                         ELSE pg_total_relation_size(cls.reltoastrelid)::BIGINT END AS toast_bytes,
                    pg_indexes_size(cls.oid)::BIGINT AS index_bytes,
                    pg_total_relation_size(cls.oid)::BIGINT AS total_bytes,
                    stats.n_live_tup::BIGINT AS live_tuples,
                    stats.n_dead_tup::BIGINT AS dead_tuples,
                    stats.last_analyze::TEXT AS last_analyze,
                    stats.last_autoanalyze::TEXT AS last_autoanalyze,
                    stats.last_vacuum::TEXT AS last_vacuum,
                    stats.last_autovacuum::TEXT AS last_autovacuum
             FROM pg_class cls
             JOIN pg_namespace ns ON ns.oid = cls.relnamespace
             LEFT JOIN pg_stat_user_tables stats ON stats.relid = cls.oid
             WHERE cls.relkind IN ('r', 'm', 'p')
               AND ns.nspname NOT IN ('pg_catalog', 'information_schema')
             ORDER BY pg_total_relation_size(cls.oid) DESC LIMIT ?",
            )
            .bind(MAX_DIAGNOSTIC_RELATIONS)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| self.diagnostics_error(source))?;
        let mut relations = Vec::new();
        for row in relation_rows {
            relations.push(json!({
                "schema": row.schema_name,
                "name": row.relation_name,
                "kind": "TABLE",
                "size": {
                    "heapBytes": row.heap_bytes,
                    "toastBytes": row.toast_bytes,
                    "indexBytes": row.index_bytes,
                    "totalBytes": row.total_bytes,
                },
                "rowCountEstimate": row.live_tuples,
                "rowCountKind": "PG_STAT_ESTIMATE",
                "liveTupleEstimate": row.live_tuples,
                "deadTupleEstimate": row.dead_tuples,
                "lastAnalyze": row.last_analyze,
                "lastAutoAnalyze": row.last_autoanalyze,
                "lastVacuum": row.last_vacuum,
                "lastAutoVacuum": row.last_autovacuum,
            }));
        }
        let index_rows = self
            .query_as::<PostgresIndexSizeRow>(
                "SELECT table_ns.nspname::TEXT AS schema_name,
                    table_cls.relname::TEXT AS table_name,
                    index_cls.relname::TEXT AS index_name,
                    pg_relation_size(index_cls.oid)::BIGINT AS index_bytes,
                    stats.idx_scan::BIGINT AS index_scans,
                    index_info.indisvalid AS is_valid
             FROM pg_index index_info
             JOIN pg_class table_cls ON table_cls.oid = index_info.indrelid
             JOIN pg_class index_cls ON index_cls.oid = index_info.indexrelid
             JOIN pg_namespace table_ns ON table_ns.oid = table_cls.relnamespace
             LEFT JOIN pg_stat_user_indexes stats ON stats.indexrelid = index_cls.oid
             WHERE table_ns.nspname NOT IN ('pg_catalog', 'information_schema')
             ORDER BY pg_relation_size(index_cls.oid) DESC LIMIT ?",
            )
            .bind(MAX_DIAGNOSTIC_RELATIONS)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| self.diagnostics_error(source))?;
        let mut indexes = Vec::new();
        for row in index_rows {
            indexes.push(json!({
                "schema": row.schema_name,
                "table": row.table_name,
                "name": row.index_name,
                "bytes": row.index_bytes,
                "scansEstimate": row.index_scans.unwrap_or_default(),
                "isValid": row.is_valid,
            }));
        }
        let relations_truncated = relations.len() as i64 >= MAX_DIAGNOSTIC_RELATIONS;
        let indexes_truncated = indexes.len() as i64 >= MAX_DIAGNOSTIC_RELATIONS;
        let mut schema_bytes = BTreeMap::<String, i64>::new();
        for relation in &relations {
            if let (Some(schema), Some(bytes)) = (
                relation["schema"].as_str(),
                relation["size"]["totalBytes"].as_i64(),
            ) {
                *schema_bytes.entry(schema.to_owned()).or_default() += bytes;
            }
        }
        let mut schema_sizes = schema_bytes
            .into_iter()
            .map(|(schema, total_bytes)| json!({ "schema": schema, "totalBytes": total_bytes }))
            .collect::<Vec<_>>();
        schema_sizes.sort_by(|left, right| {
            right["totalBytes"]
                .as_i64()
                .unwrap_or_default()
                .cmp(&left["totalBytes"].as_i64().unwrap_or_default())
        });
        let query_statistics = self.collect_postgres_query_statistics().await;
        Ok(json!({
            "backend": "POSTGRESQL", "engineVersion": engine_version,
            "databaseName": database_name, "databaseBytes": database_bytes,
            "databaseBytesKind": "EXACT", "relations": relations,
            "schemaSizes": schema_sizes,
            "indexes": indexes, "rowCountsAreEstimates": true,
            "queryStatistics": query_statistics,
            "relationsTruncated": relations_truncated,
            "indexesTruncated": indexes_truncated,
        }))
    }

    pub(super) async fn collect_postgres_query_statistics(&self) -> Value {
        let configuration = self
            .query_as::<(String, bool)>(
                "SELECT current_setting('shared_preload_libraries')::TEXT AS preload_libraries,
                        EXISTS (
                            SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements'
                        ) AS extension_installed",
            )
            .fetch_one(&self.pool)
            .await;
        let (preload_libraries, extension_installed) = match configuration {
            Ok(configuration) => configuration,
            Err(_) => return unavailable_query_statistics("UNAVAILABLE"),
        };
        if !shared_preload_contains_pg_stat_statements(&preload_libraries) {
            return unavailable_query_statistics("NOT_PRELOADED");
        }
        if !extension_installed {
            return unavailable_query_statistics("EXTENSION_MISSING");
        }

        let statements = tokio::time::timeout(
            POSTGRES_STATEMENTS_TIMEOUT,
            self.query_as::<PostgresStatementStatRow>(
                "SELECT queryid::TEXT AS query_id, calls::BIGINT AS calls,
                        total_exec_time::DOUBLE PRECISION AS total_exec_time_ms,
                        mean_exec_time::DOUBLE PRECISION AS mean_exec_time_ms,
                        max_exec_time::DOUBLE PRECISION AS max_exec_time_ms,
                        rows::BIGINT AS rows,
                        shared_blks_hit::BIGINT AS shared_blocks_hit,
                        shared_blks_read::BIGINT AS shared_blocks_read
                 FROM pg_stat_statements
                 WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database())
                   AND queryid IS NOT NULL
                 ORDER BY total_exec_time DESC
                 LIMIT ?",
            )
            .bind(MAX_POSTGRES_STATEMENTS)
            .fetch_all(&self.pool),
        )
        .await;
        let rows = match statements {
            Err(_) => return unavailable_query_statistics("QUERY_TIMEOUT"),
            Ok(Err(error)) => {
                let status = if error
                    .as_database_error()
                    .and_then(|database_error| database_error.code())
                    .as_deref()
                    == Some("42501")
                {
                    "INSUFFICIENT_PRIVILEGE"
                } else {
                    "UNAVAILABLE"
                };
                return unavailable_query_statistics(status);
            }
            Ok(Ok(rows)) => rows,
        };
        let top_statements = rows
            .into_iter()
            .map(|row| {
                json!({
                    "queryId": row.query_id,
                    "calls": row.calls,
                    "totalExecTimeMs": row.total_exec_time_ms,
                    "meanExecTimeMs": row.mean_exec_time_ms,
                    "maxExecTimeMs": row.max_exec_time_ms,
                    "rows": row.rows,
                    "sharedBlocksHit": row.shared_blocks_hit,
                    "sharedBlocksRead": row.shared_blocks_read,
                })
            })
            .collect::<Vec<_>>();
        json!({ "status": "AVAILABLE", "topStatements": top_statements })
    }

    fn diagnostics_error(&self, source: sqlx::Error) -> StorageError {
        StorageError::Sqlx {
            path: self.path.clone(),
            source,
        }
    }
}

pub(super) fn shared_preload_contains_pg_stat_statements(preload_libraries: &str) -> bool {
    preload_libraries
        .split(',')
        .map(str::trim)
        .any(|library| library.trim_matches('"') == "pg_stat_statements")
}

fn unavailable_query_statistics(status: &str) -> Value {
    json!({ "status": status, "topStatements": [] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[tokio::test]
    async fn sqlite_diagnostics_can_skip_dbstat_for_automatic_collection() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let report = database
            .collect_database_diagnostics(false)
            .await
            .expect("diagnostics");
        assert_eq!(report.details["relationSizesAvailable"], false);
        assert_eq!(
            report.details["relationSizesNote"],
            "SQLite dbstat skipped for automatic collection"
        );
    }

    #[tokio::test]
    async fn sqlite_database_diagnostics_report_file_and_relation_sizes_without_row_data() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let report = database
            .collect_database_diagnostics(true)
            .await
            .expect("diagnostics");

        assert_eq!(report.details["backend"], "SQLITE");
        assert_eq!(
            report.details["queryStatistics"]["status"],
            "NOT_APPLICABLE"
        );
        assert!(report.details["databaseBytes"].as_u64().unwrap_or_default() > 0);
        assert!(report.details["pageCount"].as_i64().unwrap_or_default() > 0);
        assert!(
            report.details["relations"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
        assert!(
            report.details["relations"]
                .to_string()
                .contains("media_items")
        );
        assert!(
            report.details["relations"]
                .to_string()
                .contains("rowCountKind")
        );
        if report.details["relationSizesAvailable"] == true {
            assert!(
                report.details["relations"]
                    .to_string()
                    .contains("totalBytes")
            );
            assert!(report.details["indexes"].as_array().is_some());
        } else {
            assert_eq!(
                report.details["relationSizesNote"],
                "SQLite dbstat unavailable"
            );
        }
        assert!(
            !report
                .details
                .to_string()
                .contains(config.config_dir.to_string_lossy().as_ref())
        );
        database.close().await;
    }

    #[test]
    fn pg_stat_statements_preload_detection_matches_a_complete_library_name() {
        assert!(shared_preload_contains_pg_stat_statements(
            "pg_stat_statements,auto_explain"
        ));
        assert!(shared_preload_contains_pg_stat_statements(
            "auto_explain, pg_stat_statements"
        ));
        assert!(!shared_preload_contains_pg_stat_statements(
            "pg_stat_statements_test"
        ));
        assert!(!shared_preload_contains_pg_stat_statements(""));
    }

    #[test]
    fn unavailable_query_statistics_returns_a_bounded_empty_result() {
        let report = unavailable_query_statistics("NOT_PRELOADED");
        assert_eq!(report["status"], "NOT_PRELOADED");
        assert_eq!(report["topStatements"], json!([]));
        assert!(report.get("query").is_none());
    }

    #[tokio::test]
    #[ignore = "requires a local PostgreSQL instance"]
    async fn postgres_database_diagnostics_report_relation_and_index_statistics() {
        let database_name = format!("lux_diag_{}", Uuid::now_v7().simple());
        let admin_connection = crate::config::PostgresConnection {
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
            .postgres_url()
            .expect("valid PostgreSQL configuration")
            .expect("PostgreSQL URL");
        let admin_pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .expect("PostgreSQL admin connection");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE DATABASE {database_name}"
        )))
        .execute(&admin_pool)
        .await
        .expect("create test database");

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect_with_configuration(
            &config,
            &crate::config::DatabaseConfiguration::Postgres(crate::config::PostgresConnection {
                database: database_name.clone(),
                ..admin_connection
            }),
        )
        .await
        .expect("test database");
        let preload_libraries: String =
            sqlx::query_scalar("SELECT current_setting('shared_preload_libraries')::TEXT")
                .fetch_one(&admin_pool)
                .await
                .expect("read PostgreSQL preload configuration");
        let extension_ready = if shared_preload_contains_pg_stat_statements(&preload_libraries) {
            sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
                .execute(database.pool())
                .await
                .is_ok()
        } else {
            false
        };
        if extension_ready {
            let _: i64 = database
                .query_scalar("SELECT 424242::BIGINT")
                .fetch_one(database.pool())
                .await
                .expect("record a statement in pg_stat_statements");
        }
        let report = database
            .collect_database_diagnostics(true)
            .await
            .expect("diagnostics");

        assert_eq!(report.details["backend"], "POSTGRESQL");
        let query_statistics = &report.details["queryStatistics"];
        assert!(matches!(
            query_statistics["status"].as_str(),
            Some(
                "AVAILABLE"
                    | "NOT_PRELOADED"
                    | "EXTENSION_MISSING"
                    | "INSUFFICIENT_PRIVILEGE"
                    | "QUERY_TIMEOUT"
                    | "UNAVAILABLE"
            )
        ));
        if query_statistics["status"] == "AVAILABLE" {
            let statements = query_statistics["topStatements"]
                .as_array()
                .expect("available query statistics include statements");
            assert!(statements.len() <= 20);
            for statement in statements {
                assert!(statement["queryId"].as_str().is_some());
                assert!(statement["calls"].as_u64().is_some());
                assert!(statement["totalExecTimeMs"].as_f64().is_some());
                assert!(statement.get("query").is_none());
                assert!(statement.get("statement").is_none());
            }
        } else {
            assert_eq!(query_statistics["topStatements"], json!([]));
        }
        if extension_ready {
            assert_eq!(query_statistics["status"], "AVAILABLE");
        }
        assert!(report.details["databaseBytes"].as_i64().unwrap_or_default() > 0);
        assert!(
            report.details["relations"]
                .to_string()
                .contains("media_items")
        );
        assert!(
            report.details["indexes"]
                .to_string()
                .contains("media_items_pkey")
        );
        assert!(report.details["schemaSizes"].as_array().is_some());
        assert_eq!(report.details["rowCountsAreEstimates"], true);

        database.close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS {database_name}"
        )))
        .execute(&admin_pool)
        .await
        .expect("drop test database");
        admin_pool.close().await;
    }

    /// The diagnostic projections are decoded through named row structs, so a projection that
    /// drifts from its struct is a `ColumnNotFound` instead of a silently defaulted field.
    #[tokio::test]
    async fn diagnostic_row_structs_decode_their_columns_and_reject_a_missing_one() {
        use sqlx::FromRow;

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");

        let dbstat = sqlx::query(
            "SELECT NULL AS name, 4096 AS bytes, 1024 AS payload_bytes,
                    2048 AS unused_bytes, 3 AS pages",
        )
        .fetch_one(database.pool())
        .await
        .expect("dbstat projection");
        let dbstat = SqliteDbstatObjectRow::from_row(&dbstat).expect("dbstat row");
        assert_eq!(dbstat.name, None);
        assert_eq!(dbstat.bytes, 4096);
        assert_eq!(dbstat.payload_bytes, 1024);
        assert_eq!(dbstat.unused_bytes, 2048);
        assert_eq!(dbstat.pages, 3);

        let index = sqlx::query("SELECT 'idx_a' AS name, 'table_a' AS tbl_name")
            .fetch_one(database.pool())
            .await
            .expect("index projection");
        let index = SqliteSchemaIndexRow::from_row(&index).expect("index row");
        assert_eq!(index.name, "idx_a");
        assert_eq!(index.tbl_name, "table_a");

        let relation = sqlx::query(
            "SELECT 'public' AS schema_name, 'media_items' AS relation_name,
                    4096 AS heap_bytes, 0 AS toast_bytes, 2048 AS index_bytes,
                    6144 AS total_bytes, 12 AS live_tuples, 3 AS dead_tuples,
                    '2026-01-01 00:00:00+00' AS last_analyze,
                    NULL AS last_autoanalyze,
                    '2026-01-02 00:00:00+00' AS last_vacuum,
                    NULL AS last_autovacuum",
        )
        .fetch_one(database.pool())
        .await
        .expect("relation projection");
        let relation = PostgresRelationSizeRow::from_row(&relation).expect("relation row");
        assert_eq!(relation.schema_name, "public");
        assert_eq!(relation.relation_name, "media_items");
        assert_eq!(relation.heap_bytes, 4096);
        assert_eq!(relation.toast_bytes, 0);
        assert_eq!(relation.index_bytes, 2048);
        assert_eq!(relation.total_bytes, 6144);
        assert_eq!(relation.live_tuples, Some(12));
        assert_eq!(relation.dead_tuples, Some(3));
        assert_eq!(
            relation.last_analyze.as_deref(),
            Some("2026-01-01 00:00:00+00")
        );
        assert_eq!(relation.last_autoanalyze, None);
        assert_eq!(
            relation.last_vacuum.as_deref(),
            Some("2026-01-02 00:00:00+00")
        );
        assert_eq!(relation.last_autovacuum, None);

        let statement = sqlx::query(
            "SELECT '42' AS query_id, 7 AS calls,
                    10.5 AS total_exec_time_ms, 1.5 AS mean_exec_time_ms,
                    3.5 AS max_exec_time_ms, 90 AS rows,
                    100 AS shared_blocks_hit, 20 AS shared_blocks_read",
        )
        .fetch_one(database.pool())
        .await
        .expect("statement projection");
        let statement = PostgresStatementStatRow::from_row(&statement).expect("statement row");
        assert_eq!(statement.query_id, "42");
        assert_eq!(statement.calls, 7);
        assert_eq!(statement.total_exec_time_ms, 10.5);
        assert_eq!(statement.mean_exec_time_ms, 1.5);
        assert_eq!(statement.max_exec_time_ms, 3.5);
        assert_eq!(statement.rows, 90);
        assert_eq!(statement.shared_blocks_hit, 100);
        assert_eq!(statement.shared_blocks_read, 20);

        let missing = sqlx::query("SELECT 1 AS other")
            .fetch_one(database.pool())
            .await
            .expect("placeholder row");
        let errors = [
            SqliteDbstatObjectRow::from_row(&missing).expect_err("dbstat columns must not decode"),
            SqliteSchemaIndexRow::from_row(&missing)
                .expect_err("schema index columns must not decode"),
            PostgresRelationSizeRow::from_row(&missing)
                .expect_err("relation columns must not decode"),
            PostgresIndexSizeRow::from_row(&missing).expect_err("index columns must not decode"),
            PostgresStatementStatRow::from_row(&missing)
                .expect_err("statement columns must not decode"),
        ];
        for error in errors {
            assert!(
                matches!(error, sqlx::Error::ColumnNotFound(_)),
                "unexpected decode error: {error}"
            );
        }
        database.close().await;
    }
}
