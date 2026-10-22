use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::{Value, json};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::storage::Database;

const START_DELAY: Duration = Duration::from_secs(5 * 60);
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatabaseDiagnosticsStartError {
    AlreadyRunning,
}

#[derive(Clone)]
pub struct DatabaseDiagnosticsService {
    database: Database,
    state: Arc<RwLock<Value>>,
    started: Arc<AtomicBool>,
}

impl DatabaseDiagnosticsService {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            state: Arc::new(RwLock::new(json!({
                "status": "WAITING",
                "scheduledAt": null,
                "startedAt": null,
                "completedAt": null,
                "report": null,
                "hasReport": false,
                "reportGeneratedAt": null,
                "errorCode": null,
            }))),
            started: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn start(&self) {
        self.start_after(START_DELAY).await;
    }

    async fn start_after(&self, delay: Duration) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let scheduled_at = unix_timestamp().saturating_add(delay.as_secs() as i64);
        self.state.write().await["scheduledAt"] = json!(scheduled_at);
        let service = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            // The automatic report must not scan every SQLite page at startup;
            // relation sizes are collected when an administrator asks for them.
            let _ = service.begin_collection(true, false).await;
        });
    }

    pub async fn request_collection(&self) -> Result<Value, DatabaseDiagnosticsStartError> {
        self.begin_collection(false, true).await?;
        Ok(self.status().await)
    }

    async fn begin_collection(
        &self,
        only_if_waiting: bool,
        include_relation_sizes: bool,
    ) -> Result<(), DatabaseDiagnosticsStartError> {
        let started_at = unix_timestamp();
        let mut state = self.state.write().await;
        let current_status = state["status"].as_str().unwrap_or_default();
        if current_status == "RUNNING" {
            return Err(DatabaseDiagnosticsStartError::AlreadyRunning);
        }
        if only_if_waiting && current_status != "WAITING" {
            return Ok(());
        }

        let scheduled_at = if only_if_waiting {
            state["scheduledAt"].as_i64().unwrap_or(started_at)
        } else {
            started_at
        };
        let previous_report = state["report"].clone();
        let report_generated_at = state["reportGeneratedAt"].clone();
        let has_report = !previous_report.is_null();
        *state = json!({
            "status": "RUNNING",
            "scheduledAt": scheduled_at,
            "startedAt": started_at,
            "completedAt": null,
            "report": previous_report,
            "hasReport": has_report,
            "reportGeneratedAt": report_generated_at,
            "errorCode": null,
        });
        drop(state);

        let service = self.clone();
        tokio::spawn(async move {
            service.collect(include_relation_sizes).await;
        });
        Ok(())
    }

    pub async fn status(&self) -> Value {
        let mut status = self.state.read().await.clone();
        if let Some(object) = status.as_object_mut() {
            object.remove("report");
        }
        status
    }

    pub async fn report(&self) -> Option<Value> {
        let state = self.state.read().await;
        let report = state["report"].clone();
        (!report.is_null()).then_some(report)
    }

    async fn collect(&self, include_relation_sizes: bool) {
        let result = tokio::time::timeout(COLLECTION_TIMEOUT, async {
            let mut report = self
                .database
                .collect_database_diagnostics(include_relation_sizes)
                .await
                .map_err(|_| ())?;
            let schema_version = self.database.schema_version().await.map_err(|_| ())?;
            report.details["reportVersion"] = json!(1);
            report.details["schemaVersion"] = json!(schema_version);
            report.details["generatedAt"] = json!(report.collected_at);
            Ok::<Value, ()>(report.details)
        })
        .await;
        let completed_at = unix_timestamp();
        match result {
            Ok(Ok(report)) => {
                let report_generated_at = report["generatedAt"].clone();
                let mut state = self.state.write().await;
                state["status"] = json!("READY");
                state["completedAt"] = json!(completed_at);
                state["report"] = report;
                state["hasReport"] = json!(true);
                state["reportGeneratedAt"] = report_generated_at;
                state["errorCode"] = Value::Null;
                info!("read-only database diagnostics report is ready");
            }
            Ok(Err(())) => self.mark_failed(completed_at, "DIAGNOSTICS_FAILED").await,
            Err(_) => self.mark_failed(completed_at, "DIAGNOSTICS_TIMEOUT").await,
        }
    }

    async fn mark_failed(&self, completed_at: i64, error_code: &str) {
        let mut state = self.state.write().await;
        state["status"] = json!("FAILED");
        state["completedAt"] = json!(completed_at);
        state["errorCode"] = json!(error_code);
        warn!(error_code, "read-only database diagnostics report failed");
    }

    #[cfg(test)]
    pub(crate) async fn start_immediately_for_test(&self) {
        self.start_after(Duration::ZERO).await;
    }
}

fn unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[tokio::test]
    async fn diagnostics_state_starts_waiting_and_report_is_absent() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let service = DatabaseDiagnosticsService::new(database.clone());
        let status = service.status().await;

        assert_eq!(status["status"], "WAITING");
        assert!(status["scheduledAt"].is_null());
        assert!(service.report().await.is_none());

        service.start_immediately_for_test().await;
        let status = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let status = service.status().await;
                if status["status"] == "READY" {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("report becomes ready");
        let report = service.report().await.expect("generated report");
        assert_eq!(report["backend"], "SQLITE");
        assert_eq!(status["status"], "READY");
        assert!(status["scheduledAt"].as_i64().is_some());
        assert!(status["completedAt"].as_i64().is_some());
        assert_eq!(status["hasReport"], true);

        let recollection_status = service
            .request_collection()
            .await
            .expect("request recollection");
        assert_eq!(recollection_status["hasReport"], true);
        assert!(matches!(
            service.request_collection().await,
            Err(DatabaseDiagnosticsStartError::AlreadyRunning)
        ));
        assert!(service.report().await.is_some());
        let recollected_status = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let status = service.status().await;
                if status["status"] == "READY" {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("recollection becomes ready");
        assert_eq!(recollected_status["hasReport"], true);
        database.close().await;
    }
}
