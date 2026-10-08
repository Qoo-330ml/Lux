use super::*;
use serde_json::{Value, json};
use std::time::Duration;

const MAX_DIAGNOSTIC_RELATIONS: i64 = 10_000;
const MAX_POSTGRES_STATEMENTS: i64 = 20;
const POSTGRES_STATEMENTS_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct DatabaseDiagnosticsSnapshot {
    pub collected_at: i64,
    pub details: Value,
}

impl Database {
    pub async fn collect_database_diagnostics(
        &self,
    ) -> Result<DatabaseDiagnosticsSnapshot, StorageError> {
        let details = match self.backend {
            DatabaseBackend::Sqlite => self.collect_sqlite_diagnostics().await?,
            DatabaseBackend::Postgres => self.collect_postgres_diagnostics().await?,
        };
        Ok(DatabaseDiagnosticsSnapshot {
            collected_at: current_unix_timestamp(),
            details,
        })
    }

    async fn collect_sqlite_diagnostics(&self) -> Result<Value, StorageError> {
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
        let tables = self.query(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?",
        ).bind(MAX_DIAGNOSTIC_RELATIONS).fetch_all(&self.pool).await
            .map_err(|source| self.diagnostics_error(source))?;
        let indexes = self
            .query(
                "SELECT name, tbl_name FROM sqlite_master
             WHERE type = 'index' AND tbl_name NOT LIKE 'sqlite_%'
             ORDER BY name LIMIT ?",
            )
            .bind(MAX_DIAGNOSTIC_RELATIONS)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| self.diagnostics_error(source))?;

        let dbstat_rows = tokio::time::timeout(
            Duration::from_secs(90),
            self.query(
                "SELECT name, SUM(pgsize) AS bytes, SUM(payload) AS payload_bytes,
                    SUM(unused) AS unused_bytes, COUNT(*) AS pages
             FROM dbstat GROUP BY name ORDER BY SUM(pgsize) DESC LIMIT ?",
            )
            .bind(MAX_DIAGNOSTIC_RELATIONS)
            .fetch_all(&self.pool),
        )
        .await;
        let (dbstat_available, mut object_sizes) = match dbstat_rows {
            Ok(Ok(rows)) => {
                let mut sizes = HashMap::new();
                for row in rows {
                    let name: String = row.try_get("name").unwrap_or_default();
                    sizes.insert(name, json!({
                        "bytes": row.try_get::<i64, _>("bytes").unwrap_or_default(),
                        "payloadBytes": row.try_get::<i64, _>("payload_bytes").unwrap_or_default(),
                        "unusedBytes": row.try_get::<i64, _>("unused_bytes").unwrap_or_default(),
                        "pages": row.try_get::<i64, _>("pages").unwrap_or_default(),
                    }));
                }
                (true, sizes)
            }
            _ => (false, HashMap::new()),
        };

        let mut index_details = Vec::new();
        let mut index_bytes_by_table = HashMap::<String, i64>::new();
        for row in indexes {
            let name: String = row.try_get("name").unwrap_or_default();
            let table_name: String = row.try_get("tbl_name").unwrap_or_default();
            let size = object_sizes.remove(&name).unwrap_or(Value::Null);
            let index_bytes = size["bytes"].as_i64().unwrap_or_default();
            *index_bytes_by_table.entry(table_name.clone()).or_default() += index_bytes;
            index_details.push(json!({ "name": name, "table": table_name, "size": size }));
        }
        for row in tables {
            let name: String = row.try_get("name").unwrap_or_default();
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
            "relationSizesNote": if dbstat_available { "SQLite dbstat" } else { "SQLite dbstat unavailable" },
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
            .query(
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
            let live_tuples = row.try_get::<Option<i64>, _>("live_tuples").ok().flatten();
            let dead_tuples = row.try_get::<Option<i64>, _>("dead_tuples").ok().flatten();
            relations.push(json!({
                "schema": row.try_get::<String, _>("schema_name").unwrap_or_default(),
                "name": row.try_get::<String, _>("relation_name").unwrap_or_default(),
                "kind": "TABLE",
                "size": {
                    "heapBytes": row.try_get::<i64, _>("heap_bytes").unwrap_or_default(),
                    "toastBytes": row.try_get::<i64, _>("toast_bytes").unwrap_or_default(),
                    "indexBytes": row.try_get::<i64, _>("index_bytes").unwrap_or_default(),
                    "totalBytes": row.try_get::<i64, _>("total_bytes").unwrap_or_default(),
                },
                "rowCountEstimate": live_tuples,
                "rowCountKind": "PG_STAT_ESTIMATE",
                "liveTupleEstimate": live_tuples,
                "deadTupleEstimate": dead_tuples,
                "lastAnalyze": row.try_get::<Option<String>, _>("last_analyze").ok().flatten(),
                "lastAutoAnalyze": row.try_get::<Option<String>, _>("last_autoanalyze").ok().flatten(),
                "lastVacuum": row.try_get::<Option<String>, _>("last_vacuum").ok().flatten(),
                "lastAutoVacuum": row.try_get::<Option<String>, _>("last_autovacuum").ok().flatten(),
            }));
        }
        let index_rows = self
            .query(
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
                "schema": row.try_get::<String, _>("schema_name").unwrap_or_default(),
                "table": row.try_get::<String, _>("table_name").unwrap_or_default(),
                "name": row.try_get::<String, _>("index_name").unwrap_or_default(),
                "bytes": row.try_get::<i64, _>("index_bytes").unwrap_or_default(),
                "scansEstimate": row.try_get::<i64, _>("index_scans").unwrap_or_default(),
                "isValid": row.try_get::<bool, _>("is_valid").unwrap_or(false),
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

    async fn collect_postgres_query_statistics(&self) -> Value {
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
            self.query(
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
            .iter()
            .map(|row| {
                json!({
                    "queryId": row.try_get::<String, _>("query_id").unwrap_or_default(),
                    "calls": row.try_get::<i64, _>("calls").unwrap_or_default(),
                    "totalExecTimeMs": row.try_get::<f64, _>("total_exec_time_ms").unwrap_or_default(),
                    "meanExecTimeMs": row.try_get::<f64, _>("mean_exec_time_ms").unwrap_or_default(),
                    "maxExecTimeMs": row.try_get::<f64, _>("max_exec_time_ms").unwrap_or_default(),
                    "rows": row.try_get::<i64, _>("rows").unwrap_or_default(),
                    "sharedBlocksHit": row.try_get::<i64, _>("shared_blocks_hit").unwrap_or_default(),
                    "sharedBlocksRead": row.try_get::<i64, _>("shared_blocks_read").unwrap_or_default(),
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

fn shared_preload_contains_pg_stat_statements(preload_libraries: &str) -> bool {
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
    async fn sqlite_database_diagnostics_report_file_and_relation_sizes_without_row_data() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let report = database
            .collect_database_diagnostics()
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
            .collect_database_diagnostics()
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
}
