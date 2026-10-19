use std::{
    path::{Component, Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use tokio::{sync::watch, task::spawn_blocking, time::sleep};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::{
    application::{restart::ShutdownReason, scanner},
    storage::{Database, ScanFingerprintMigrationCommit, StoredScanFingerprintMigrationCandidate},
};

const START_DELAY: Duration = Duration::from_secs(60);
const FILE_INTERVAL: Duration = Duration::from_millis(50);
const IDLE_INTERVAL: Duration = Duration::from_secs(10);
const LIBRARY_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileStatSnapshot {
    size: i64,
    modified_at: i64,
    device: i64,
    inode: i64,
}

enum CandidateResult {
    Migrated,
    Deferred(&'static str),
    NoLongerLegacy,
}

pub fn start(database: Database, shutdown: watch::Receiver<ShutdownReason>) {
    tokio::spawn(run(database, shutdown));
}

async fn run(database: Database, mut shutdown: watch::Receiver<ShutdownReason>) {
    if wait_or_shutdown(START_DELAY, &mut shutdown).await {
        return;
    }

    let mut next_library_index = 0_usize;
    let mut library_ids = Vec::new();
    let mut library_list_refreshed_at = None;
    loop {
        if *shutdown.borrow() != ShutdownReason::Running {
            return;
        }

        if library_ids.is_empty()
            || library_list_refreshed_at.is_none_or(|refreshed_at: Instant| {
                refreshed_at.elapsed() >= LIBRARY_REFRESH_INTERVAL
            })
        {
            match database.list_scan_fingerprint_migration_library_ids().await {
                Ok(ids) => {
                    library_ids = ids;
                    library_list_refreshed_at = Some(Instant::now());
                }
                Err(error) => {
                    warn!(%error, "scan fingerprint migration could not list libraries");
                    if wait_or_shutdown(IDLE_INTERVAL, &mut shutdown).await {
                        return;
                    }
                    continue;
                }
            }
        }
        if library_ids.is_empty() {
            if wait_or_shutdown(IDLE_INTERVAL, &mut shutdown).await {
                return;
            }
            continue;
        }

        let start_index = next_library_index % library_ids.len();
        let mut did_work = false;
        for offset in 0..library_ids.len() {
            if *shutdown.borrow() != ShutdownReason::Running {
                return;
            }
            let library_id = library_ids[(start_index + offset) % library_ids.len()].as_str();
            let cursor = match database
                .ensure_scan_fingerprint_migration_state(library_id)
                .await
            {
                Ok(cursor) => cursor,
                Err(error) => {
                    warn!(%error, library_id, "scan fingerprint migration state unavailable");
                    continue;
                }
            };
            if cursor.completed_at.is_some() {
                continue;
            }

            match database.find_active_scan_job_for_library(library_id).await {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(error) => {
                    warn!(%error, library_id, "scan fingerprint migration could not check scan activity");
                    continue;
                }
            }
            match database
                .has_active_chapter_detection_job_for_library(library_id)
                .await
            {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    warn!(%error, library_id, "scan fingerprint migration could not check chapter activity");
                    continue;
                }
            }

            let candidate = match database
                .find_scan_fingerprint_migration_candidate(library_id, &cursor)
                .await
            {
                Ok(candidate) => candidate,
                Err(error) => {
                    warn!(%error, library_id, "scan fingerprint migration candidate query failed");
                    continue;
                }
            };
            let Some(candidate) = candidate else {
                if cursor.root_id.is_some() {
                    match database
                        .reset_scan_fingerprint_migration_cursor(library_id)
                        .await
                    {
                        Ok(()) => {
                            next_library_index = (start_index + offset + 1) % library_ids.len();
                            did_work = true;
                            break;
                        }
                        Err(error) => {
                            warn!(%error, library_id, "scan fingerprint migration cursor reset failed");
                            continue;
                        }
                    }
                }
                match database
                    .clear_one_missing_scan_fingerprint(library_id)
                    .await
                {
                    Ok(true) => {
                        debug!(library_id, "cleared a missing file scan fingerprint");
                        next_library_index = (start_index + offset + 1) % library_ids.len();
                        did_work = true;
                        break;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        warn!(%error, library_id, "scan fingerprint migration could not clear a missing fingerprint");
                        continue;
                    }
                }
                match database
                    .scan_fingerprint_migration_has_pending_entries(library_id)
                    .await
                {
                    Ok(false) => {
                        if let Err(error) = database
                            .complete_scan_fingerprint_migration_library(library_id)
                            .await
                        {
                            warn!(%error, library_id, "scan fingerprint migration completion update failed");
                        }
                    }
                    Ok(true) => {}
                    Err(error) => {
                        warn!(%error, library_id, "scan fingerprint migration completion check failed");
                    }
                }
                continue;
            };

            let result = process_candidate(&database, &candidate).await;
            match result {
                Ok(CandidateResult::Migrated) => {
                    debug!(
                        library_id,
                        relative_path = candidate.relative_path,
                        "scan fingerprint migrated to v2"
                    );
                }
                Ok(CandidateResult::Deferred(reason))
                    if matches!(reason, "root_stat_failed" | "root_identity_changed") =>
                {
                    let delay = retry_delay(candidate.attempts, reason);
                    let retry_at = unix_timestamp()
                        .saturating_add(i64::try_from(delay.as_secs()).unwrap_or(i64::MAX));
                    if let Err(error) = database
                        .defer_scan_fingerprint_migration_root(
                            &candidate.library_root_id,
                            retry_at,
                            reason,
                        )
                        .await
                    {
                        warn!(%error, library_id, root_id = candidate.library_root_id, "scan fingerprint root deferral could not be persisted");
                    }
                }
                Ok(CandidateResult::Deferred(reason)) => {
                    let delay = retry_delay(candidate.attempts, reason);
                    let retry_at = unix_timestamp()
                        .saturating_add(i64::try_from(delay.as_secs()).unwrap_or(i64::MAX));
                    if let Err(error) = database
                        .defer_scan_fingerprint_migration_candidate(&candidate, retry_at, reason)
                        .await
                    {
                        warn!(%error, library_id, entry_id = candidate.entry_id, "scan fingerprint deferral could not be persisted");
                    }
                }
                Ok(CandidateResult::NoLongerLegacy) => {
                    if let Err(error) = database
                        .advance_scan_fingerprint_migration_cursor(
                            library_id,
                            &candidate.library_root_id,
                            &candidate.relative_path,
                        )
                        .await
                    {
                        warn!(%error, library_id, entry_id = candidate.entry_id, "scan fingerprint cursor update failed");
                    }
                }
                Err(error) => {
                    warn!(%error, library_id, entry_id = candidate.entry_id, "scan fingerprint migration step failed");
                    let retry_at = unix_timestamp().saturating_add(60);
                    if let Err(persist_error) = database
                        .defer_scan_fingerprint_migration_candidate(
                            &candidate,
                            retry_at,
                            "storage_error",
                        )
                        .await
                    {
                        warn!(%persist_error, library_id, entry_id = candidate.entry_id, "scan fingerprint retry could not be persisted");
                    }
                }
            }
            next_library_index = (start_index + offset + 1) % library_ids.len();
            did_work = true;
            break;
        }

        if wait_or_shutdown(
            if did_work {
                FILE_INTERVAL
            } else {
                IDLE_INTERVAL
            },
            &mut shutdown,
        )
        .await
        {
            return;
        }
    }
}

async fn process_candidate(
    database: &Database,
    candidate: &StoredScanFingerprintMigrationCandidate,
) -> Result<CandidateResult, crate::storage::StorageError> {
    if !candidate.root_is_available {
        return Ok(CandidateResult::Deferred("root_unavailable"));
    }
    let candidate_for_stat = candidate.clone();
    let first = match spawn_blocking(move || observe_candidate(&candidate_for_stat)).await {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(reason)) => return Ok(CandidateResult::Deferred(reason)),
        Err(_) => return Ok(CandidateResult::Deferred("stat_worker_failed")),
    };
    if scanner::compute_file_fingerprint(
        &candidate.relative_path,
        first.size,
        first.modified_at,
        Some(first.device as u64),
        Some(first.inode as u64),
    ) != candidate.fingerprint
    {
        return Ok(CandidateResult::Deferred("legacy_fingerprint_mismatch"));
    }

    let second_candidate = candidate.clone();
    let second = match spawn_blocking(move || observe_candidate(&second_candidate)).await {
        Ok(Ok(snapshot)) => snapshot,
        Ok(Err(reason)) => return Ok(CandidateResult::Deferred(reason)),
        Err(_) => return Ok(CandidateResult::Deferred("second_stat_worker_failed")),
    };
    if first != second {
        return Ok(CandidateResult::Deferred("stat_changed_during_migration"));
    }
    let lease_token = Uuid::now_v7().to_string();
    if !database
        .claim_scan_fingerprint_migration_lease(&candidate.library_id, &lease_token)
        .await?
    {
        return Ok(CandidateResult::Deferred("library_became_busy"));
    }

    let fingerprint_v2 = scanner::compute_scan_fingerprint_v2(
        &candidate.relative_path,
        second.size,
        second.modified_at,
        Some(second.device as u64),
        Some(second.inode as u64),
    );
    match database
        .migrate_scan_fingerprint_candidate(ScanFingerprintMigrationCommit {
            candidate,
            fingerprint_v2: &fingerprint_v2,
            size: second.size,
            modified_at: second.modified_at,
            device: second.device,
            inode: second.inode,
            lease_token: &lease_token,
        })
        .await
    {
        Ok(true) => Ok(CandidateResult::Migrated),
        Ok(false) => Ok(CandidateResult::NoLongerLegacy),
        Err(error) => {
            let _ = database
                .release_scan_fingerprint_migration_lease(&candidate.library_id, &lease_token)
                .await;
            Err(error)
        }
    }
}

fn observe_candidate(
    candidate: &StoredScanFingerprintMigrationCandidate,
) -> Result<FileStatSnapshot, &'static str> {
    let root = PathBuf::from(&candidate.root_path);
    let canonical_root = std::fs::canonicalize(&root).map_err(|_| "root_stat_failed")?;
    if canonical_root != root || !canonical_root.is_absolute() {
        return Err("root_identity_changed");
    }
    let relative = Path::new(&candidate.relative_path);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("invalid_relative_path");
    }
    let expected = canonical_root.join(relative);
    let canonical_file = std::fs::canonicalize(&expected).map_err(|_| "file_stat_failed")?;
    if canonical_file != expected || !canonical_file.starts_with(&canonical_root) {
        return Err("path_identity_changed");
    }
    let metadata = std::fs::metadata(&canonical_file).map_err(|_| "file_stat_failed")?;
    if !metadata.is_file() {
        return Err("not_regular_file");
    }
    let size = i64::try_from(metadata.len()).map_err(|_| "file_size_overflow")?;
    let modified_at = metadata
        .modified()
        .map_err(|_| "modified_time_unavailable")?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "modified_time_before_epoch")?
        .as_nanos();
    let modified_at = i64::try_from(modified_at).map_err(|_| "modified_time_overflow")?;
    if modified_at <= 0 {
        return Err("modified_time_unavailable");
    }
    #[cfg(unix)]
    let (device, inode) = (
        i64::try_from(metadata.dev()).map_err(|_| "device_overflow")?,
        i64::try_from(metadata.ino()).map_err(|_| "inode_overflow")?,
    );
    #[cfg(not(unix))]
    let (device, inode) = return Err("stable_file_identity_unavailable");

    Ok(FileStatSnapshot {
        size,
        modified_at,
        device,
        inode,
    })
}

fn retry_delay(attempts: i64, reason: &str) -> Duration {
    let base_seconds: i64 = if matches!(reason, "root_stat_failed" | "root_identity_changed") {
        15 * 60
    } else {
        30
    };
    let shift = u32::try_from(attempts.clamp(0, 16)).unwrap_or(16);
    Duration::from_secs(
        base_seconds
            .saturating_mul(1_i64.checked_shl(shift).unwrap_or(i64::MAX))
            .min(i64::try_from(MAX_RETRY_DELAY.as_secs()).unwrap_or(i64::MAX)) as u64,
    )
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(i64::MAX)
}

async fn wait_or_shutdown(
    duration: Duration,
    shutdown: &mut watch::Receiver<ShutdownReason>,
) -> bool {
    tokio::select! {
        _ = sleep(duration) => false,
        changed = shutdown.changed() => changed.is_err() || *shutdown.borrow() != ShutdownReason::Running,
    }
}
