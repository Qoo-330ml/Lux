use super::*;

pub(super) fn api_routes() -> Router<AppState> {
    Router::new().route("/api/v1/admin/health", get(admin_health))
}

pub(crate) async fn admin_health(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    match admin_health_payload(&state).await {
        Ok(payload) => Json(payload).into_response(),
        Err(status) => status.into_response(),
    }
}

pub(crate) async fn admin_health_payload(state: &AppState) -> Result<Value, StatusCode> {
    let Some(database) = state.database.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let schema_version = match database.schema_version().await {
        Ok(version) => version,
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let resources = state.resources.snapshot().await;
    let database_pool = database.pool_snapshot();
    let probes = state
        .admin_health_probe_cache
        .get_or_probe(|| async {
            let database_writable = database.probe_write().await.is_ok();
            let config_available = match state.config_dir.as_deref() {
                Some(path) => fs::metadata(path)
                    .await
                    .map(|metadata| metadata.is_dir())
                    .unwrap_or(false),
                None => false,
            };
            let config_writable = match state.config_dir.as_deref() {
                Some(path) if config_available => probe_directory_writable(path).await,
                _ => false,
            };
            let ffprobe_available = Command::new("ffprobe")
                .arg("-version")
                .output()
                .await
                .is_ok_and(|output| output.status.success());
            AdminHealthProbeSnapshot {
                database_writable,
                config_available,
                config_writable,
                ffprobe_available,
            }
        })
        .await;
    let AdminHealthProbeSnapshot {
        database_writable,
        config_available,
        config_writable,
        ffprobe_available,
    } = probes;
    let libraries = match state.libraries.as_ref() {
        Some(libraries) => match libraries.list_libraries().await {
            Ok(views) => views
                .iter()
                .map(|view| {
                    json!({
                        "id": view.library.id.to_string(),
                        "name": view.library.name,
                        "isEnabled": view.library.is_enabled,
                        "rootCount": view.roots.len(),
                        "availableRootCount": view.roots.iter().filter(|root| root.is_available).count(),
                        "writableRootCount": view.roots.iter().filter(|root| root.is_writable).count(),
                    })
                })
                .collect::<Vec<_>>(),
            Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
        },
        None => Vec::new(),
    };
    let jobs = match database.count_scan_jobs_by_status().await {
        Ok(counts) => json!({
            "scanRunning": counts.running,
            "scanFailed": counts.failed,
        }),
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let metadata_reidentify_running = match database.list_active_metadata_reidentify_job_ids().await
    {
        Ok(ids) => ids.len(),
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let status = if database_writable && config_available && config_writable && ffprobe_available {
        "ok"
    } else {
        "degraded"
    };
    let (database_backend, journal_mode) = match database.backend() {
        DatabaseBackend::Sqlite => ("SQLITE", "wal"),
        DatabaseBackend::Postgres => ("POSTGRESQL", ""),
    };
    Ok(json!({
        "status": status,
        "schemaVersion": schema_version,
        "runtime": { "seconds": resources.runtime_seconds },
        "resources": resources,
        "database": {
            "status": if database_writable { "ok" } else { "degraded" },
            "backend": database_backend,
            "journalMode": journal_mode,
            "writable": database_writable,
            "pool": {
                "maxConnections": database_pool.max_connections,
                "size": database_pool.size,
                "idle": database_pool.idle,
                "inUse": database_pool.in_use,
                "saturated": database_pool.saturated,
            },
        },
        "config": { "available": config_available, "writable": config_writable },
        "ffprobe": { "available": ffprobe_available },
        "jobs": {
            "scanRunning": jobs["scanRunning"],
            "scanFailed": jobs["scanFailed"],
            "metadataReidentifyRunning": metadata_reidentify_running,
        },
        "libraries": libraries,
    }))
}
