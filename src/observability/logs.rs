use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt, fs,
    io::{self, Cursor, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex, OnceLock, Weak},
};

use serde_json::Value;
use time::{Date, Month, OffsetDateTime, format_description::well_known::Rfc3339};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

pub const LOG_DIRECTORY: &str = "logs";
pub const LOG_ARCHIVE_DIRECTORY: &str = "archive";
pub const MAX_EXPORT_DAYS: i64 = 31;
pub const LOG_SEGMENT_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_LOG_ARCHIVES: usize = 20;
const DEFAULT_EXPORT_DAYS: i64 = 7;
const MAX_DAILY_LOG_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EXPORT_BYTES: u64 = 128 * 1024 * 1024;
/// Hard ceiling on the decompressed bytes one activity/audit page request may scan.
///
/// The admin dashboard polls the activity list every 15 seconds, so the read must stay cheap even
/// when the retained log volume is large. Both daily files and archive members are read from their
/// newest end first (see `read_daily_log_file` and `read_archive_member`), so the ceiling keeps the
/// most recent records and drops older ones.
const MAX_EVENT_SCAN_BYTES: u64 = 16 * 1024 * 1024;
/// Bytes one job-event page may scan.
///
/// The response carries an exact `total`, so this path has to cover the whole retained volume
/// instead of stopping early; the ceiling is derived from the retention constants so a large
/// installation never loses older events from the admin job page. Only an administrator opening one
/// job's event list pays for it (it is not on the dashboard polling path), and the read is still
/// streamed with the byte prefilter and a page-sized buffer, so memory stays bounded even though the
/// disk read is proportional to the retained volume.
const MAX_JOB_EVENT_SCAN_BYTES: u64 = LOG_SEGMENT_BYTES.saturating_mul(MAX_LOG_ARCHIVES as u64 + 2);
/// Hard ceiling on the number of log files one page request may open: the retention keeps at most
/// `MAX_LOG_ARCHIVES` archived segments plus the daily files still being written (migration batches
/// can leave one daily file per migrated day).
const MAX_EVENT_SCAN_FILES: usize = MAX_LOG_ARCHIVES + 8;
/// Deep pages are refused instead of materialised.
///
/// The admin console only requests the first page with at most 100 rows per page, so this still
/// admits roughly the first hundred pages of the audit log while keeping one request from buffering
/// the whole log (and holding the log mutex) for an arbitrarily large `offset`.
const MAX_LOG_PAGE_WINDOW: usize = 10_000;
/// A single JSONL record larger than this is skipped instead of being buffered in memory.
const MAX_EVENT_LINE_BYTES: usize = 1024 * 1024;

const _: () = assert!(
    MAX_JOB_EVENT_SCAN_BYTES >= LOG_SEGMENT_BYTES.saturating_mul(MAX_LOG_ARCHIVES as u64 + 1)
);
const _: () = assert!(MAX_EVENT_SCAN_FILES > MAX_LOG_ARCHIVES);

const ACTIVITY_EVENT_TYPES: [&str; 4] = [
    "AUTH_LOGIN",
    "PLAYBACK_STARTED",
    "PLAYBACK_PAUSED",
    "PLAYBACK_STOPPED",
];

type SharedLogManager = Arc<StdMutex<Option<LogManager>>>;
type LogManagerRegistry = StdMutex<HashMap<PathBuf, Weak<StdMutex<Option<LogManager>>>>>;
static LOG_MANAGER_REGISTRY: OnceLock<LogManagerRegistry> = OnceLock::new();

/// Cloneable handle for structured JSONL logging and application event records.
#[derive(Clone)]
pub struct LogStore {
    config_dir: PathBuf,
    manager: SharedLogManager,
}

#[derive(Debug)]
pub struct ScanJobLogEvent {
    pub id: String,
    pub job_id: String,
    pub level: String,
    pub event_code: String,
    pub message: String,
    pub details_json: String,
    pub created_at: i64,
}

#[derive(Debug)]
pub struct AuditLogEvent {
    pub id: String,
    pub actor_user_id: Option<String>,
    pub actor_username: Option<String>,
    pub event_type: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub metadata_json: String,
    pub created_at: i64,
}

pub(crate) struct NewAuditLogEvent<'a> {
    pub(crate) id: &'a str,
    pub(crate) actor_user_id: Option<&'a str>,
    pub(crate) actor_username: Option<&'a str>,
    pub(crate) event_type: &'a str,
    pub(crate) target_type: Option<&'a str>,
    pub(crate) target_id: Option<&'a str>,
    pub(crate) metadata_json: &'a str,
}

/// Opens the manager on first use inside an already-blocking context.
fn open_manager<'a>(
    slot: &'a mut Option<LogManager>,
    config_dir: &Path,
) -> io::Result<&'a mut LogManager> {
    if slot.is_none() {
        *slot = Some(LogManager::open(config_dir)?);
    }
    slot.as_mut()
        .ok_or_else(|| io::Error::other("日志 writer 尚未初始化"))
}

impl LogStore {
    pub fn new(config_dir: &Path) -> Self {
        let manager_key = config_dir.to_path_buf();
        let registry = LOG_MANAGER_REGISTRY.get_or_init(Default::default);
        let mut registry = match registry.lock() {
            Ok(registry) => registry,
            Err(poisoned) => poisoned.into_inner(),
        };
        registry.retain(|_, manager| manager.strong_count() > 0);
        let manager = registry
            .get(&manager_key)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let manager = Arc::new(StdMutex::new(None));
                registry.insert(manager_key.clone(), Arc::downgrade(&manager));
                manager
            });
        Self {
            config_dir: manager_key,
            manager,
        }
    }

    #[cfg(test)]
    fn new_with_limits_for_test(
        config_dir: &Path,
        active_date: Date,
        segment_limit: u64,
        archive_limit: usize,
    ) -> io::Result<Self> {
        let store = Self::new(config_dir);
        let manager =
            LogManager::open_with_limits(config_dir, active_date, segment_limit, archive_limit)?;
        let mut slot = store
            .manager
            .lock()
            .map_err(|_| io::Error::other("日志 writer 状态不可用"))?;
        *slot = Some(manager);
        drop(slot);
        Ok(store)
    }

    pub async fn open(config_dir: &Path) -> io::Result<Self> {
        let store = Self::new(config_dir);
        store.initialize().await?;
        Ok(store)
    }

    async fn initialize(&self) -> io::Result<()> {
        let manager = Arc::clone(&self.manager);
        let config_dir = self.config_dir.clone();
        tokio::task::spawn_blocking(move || {
            let mut manager = manager
                .lock()
                .map_err(|_| io::Error::other("日志 writer 状态不可用"))?;
            if manager.is_none() {
                *manager = Some(LogManager::open(&config_dir)?);
            }
            Ok(())
        })
        .await
        .map_err(|error| io::Error::other(format!("日志 writer 初始化任务失败: {error}")))?
    }

    pub async fn append_json(&self, record: Value) -> io::Result<()> {
        // Redact here, not only at the call sites: any caller that writes a record must not be
        // able to persist a token or cookie by forgetting the helper.
        let record = redact_sensitive_log_values(record);
        let manager = Arc::clone(&self.manager);
        let config_dir = self.config_dir.clone();
        tokio::task::spawn_blocking(move || {
            let mut line = serde_json::to_vec(&record)
                .map_err(|error| io::Error::other(format!("日志记录编码失败: {error}")))?;
            line.push(b'\n');
            let mut slot = manager
                .lock()
                .map_err(|_| io::Error::other("日志 writer 状态不可用"))?;
            // Open on first use inside this blocking task: a separate initialize() would cost a
            // second thread hop per record.
            open_manager(&mut slot, &config_dir)?.write_line(&line)
        })
        .await
        .map_err(|error| io::Error::other(format!("日志写入任务失败: {error}")))?
    }

    pub(crate) async fn append_json_batch_and_sync(&self, records: Vec<Value>) -> io::Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let records = records
            .into_iter()
            .map(redact_sensitive_log_values)
            .collect::<Vec<_>>();
        let manager = Arc::clone(&self.manager);
        let config_dir = self.config_dir.clone();
        tokio::task::spawn_blocking(move || {
            let mut slot = manager
                .lock()
                .map_err(|_| io::Error::other("日志 writer 状态不可用"))?;
            let manager = open_manager(&mut slot, &config_dir)?;
            manager.defer_archive_pruning = true;
            for record in records {
                let mut line = serde_json::to_vec(&record)
                    .map_err(|error| io::Error::other(format!("日志记录编码失败: {error}")))?;
                line.push(b'\n');
                let record_date = record["timestamp"]
                    .as_str()
                    .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
                    .or_else(|| {
                        record["createdAt"]
                            .as_i64()
                            .and_then(|value| OffsetDateTime::from_unix_timestamp(value).ok())
                    })
                    .map(|value| value.date())
                    .unwrap_or_else(|| OffsetDateTime::now_utc().date());
                manager.write_line_for_date(record_date, &line)?;
            }
            if let Some(file) = manager.active_file.as_mut() {
                file.flush()?;
                file.sync_all()?;
            }
            Ok(())
        })
        .await
        .map_err(|error| io::Error::other(format!("日志批量写入任务失败: {error}")))?
    }

    pub(crate) async fn suspend_archive_pruning(&self) -> io::Result<()> {
        self.initialize().await?;
        let manager = Arc::clone(&self.manager);
        tokio::task::spawn_blocking(move || {
            let mut manager = manager
                .lock()
                .map_err(|_| io::Error::other("日志 writer 状态不可用"))?;
            let manager = manager
                .as_mut()
                .ok_or_else(|| io::Error::other("日志 writer 尚未初始化"))?;
            manager.defer_archive_pruning = true;
            Ok(())
        })
        .await
        .map_err(|error| io::Error::other(format!("日志归档延期清理任务失败: {error}")))?
    }

    pub(crate) async fn prune_archives(&self) -> io::Result<()> {
        self.initialize().await?;
        let manager = Arc::clone(&self.manager);
        tokio::task::spawn_blocking(move || {
            let mut manager = manager
                .lock()
                .map_err(|_| io::Error::other("日志 writer 状态不可用"))?;
            let manager = manager
                .as_mut()
                .ok_or_else(|| io::Error::other("日志 writer 尚未初始化"))?;
            let previous_pruning_state = manager.defer_archive_pruning;
            manager.defer_archive_pruning = false;
            let result = manager.prune_archives();
            if result.is_err() {
                manager.defer_archive_pruning = previous_pruning_state;
            }
            result
        })
        .await
        .map_err(|error| io::Error::other(format!("日志归档清理任务失败: {error}")))?
    }

    pub(crate) async fn existing_scan_job_event_ids(
        &self,
        expected_ids: &[String],
    ) -> io::Result<HashSet<String>> {
        self.existing_record_ids("scan_job_event", expected_ids)
            .await
    }

    pub(crate) async fn existing_audit_event_ids(
        &self,
        expected_ids: &[String],
    ) -> io::Result<HashSet<String>> {
        self.existing_record_ids("admin_audit_event", expected_ids)
            .await
    }

    async fn existing_record_ids(
        &self,
        record_type: &'static str,
        expected_ids: &[String],
    ) -> io::Result<HashSet<String>> {
        if expected_ids.is_empty() {
            return Ok(HashSet::new());
        }
        self.initialize().await?;
        let log_dir = log_dir(&self.config_dir);
        let archive_dir = archive_dir(&self.config_dir);
        let expected_ids = expected_ids.iter().cloned().collect::<HashSet<_>>();
        let manager = Arc::clone(&self.manager);
        tokio::task::spawn_blocking(move || {
            let _manager = manager
                .lock()
                .map_err(|_| io::Error::other("日志 writer 状态不可用"))?;
            read_record_ids(&log_dir, &archive_dir, record_type, &expected_ids)
        })
        .await
        .map_err(|error| io::Error::other(format!("任务日志索引读取失败: {error}")))?
    }

    pub(crate) async fn verify_scan_job_event_ids(
        &self,
        expected_ids: &[String],
    ) -> io::Result<()> {
        let persisted_ids = self.existing_scan_job_event_ids(expected_ids).await?;
        verify_expected_log_ids(expected_ids, &persisted_ids, "任务日志")
    }

    pub(crate) async fn verify_audit_event_ids(&self, expected_ids: &[String]) -> io::Result<()> {
        let persisted_ids = self.existing_audit_event_ids(expected_ids).await?;
        verify_expected_log_ids(expected_ids, &persisted_ids, "审计日志")
    }

    pub async fn append_scan_job_event(
        &self,
        id: &str,
        job_id: &str,
        level: &str,
        event_code: &str,
        message: &str,
        details_json: &str,
    ) -> io::Result<()> {
        if !matches!(level, "INFO" | "WARN" | "ERROR") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "任务事件级别无效",
            ));
        }
        let details = serde_json::from_str::<Value>(details_json)
            .map(redact_sensitive_log_values)
            .unwrap_or_else(|_| serde_json::json!({ "invalid": true }));
        let message = redact_sensitive_log_values(Value::String(message.to_owned()))
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let created_at = OffsetDateTime::now_utc();
        self.append_json(serde_json::json!({
            "recordType": "scan_job_event",
            "id": id,
            "jobId": job_id,
            "level": level,
            "eventCode": event_code,
            "message": message,
            "details": details,
            "timestamp": created_at
                .format(&Rfc3339)
                .unwrap_or_else(|_| created_at.unix_timestamp().to_string()),
            "createdAt": created_at.unix_timestamp(),
        }))
        .await
    }

    pub async fn list_scan_job_events(
        &self,
        job_id: &str,
        level: Option<&str>,
        event_code: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> io::Result<(i64, Vec<ScanJobLogEvent>)> {
        self.initialize().await?;
        let log_dir = log_dir(&self.config_dir);
        let archive_dir = archive_dir(&self.config_dir);
        let job_id = job_id.to_owned();
        let level = level.map(str::to_owned);
        let event_code = event_code.map(str::to_owned);
        let manager = Arc::clone(&self.manager);
        tokio::task::spawn_blocking(move || {
            let _manager = manager
                .lock()
                .map_err(|_| io::Error::other("日志 writer 状态不可用"))?;
            read_scan_job_events(
                &log_dir,
                &archive_dir,
                &job_id,
                level.as_deref(),
                event_code.as_deref(),
                offset,
                limit,
            )
        })
        .await
        .map_err(|error| io::Error::other(format!("任务日志读取任务失败: {error}")))?
    }

    pub(crate) async fn append_audit_event(&self, event: NewAuditLogEvent<'_>) -> io::Result<()> {
        let metadata = serde_json::from_str::<Value>(event.metadata_json)
            .map(redact_sensitive_log_values)
            .unwrap_or_else(|_| serde_json::json!({ "invalid": true }));
        let created_at = OffsetDateTime::now_utc();
        self.append_json(serde_json::json!({
            "recordType": "admin_audit_event",
            "id": event.id,
            "actorUserId": event.actor_user_id,
            "actorUsername": event.actor_username,
            "eventType": event.event_type,
            "targetType": event.target_type,
            "targetId": event.target_id,
            "metadata": metadata,
            "timestamp": created_at
                .format(&Rfc3339)
                .unwrap_or_else(|_| created_at.unix_timestamp().to_string()),
            "createdAt": created_at.unix_timestamp(),
        }))
        .await
    }

    pub async fn list_audit_events(
        &self,
        offset: i64,
        limit: i64,
    ) -> io::Result<(i64, Vec<AuditLogEvent>)> {
        self.initialize().await?;
        let log_dir = log_dir(&self.config_dir);
        let archive_dir = archive_dir(&self.config_dir);
        let manager = Arc::clone(&self.manager);
        tokio::task::spawn_blocking(move || {
            let _manager = manager
                .lock()
                .map_err(|_| io::Error::other("日志 writer 状态不可用"))?;
            read_audit_events(&log_dir, &archive_dir, offset, limit, None)
        })
        .await
        .map_err(|error| io::Error::other(format!("审计日志读取任务失败: {error}")))?
    }

    pub async fn list_activity_events(&self, limit: i64) -> io::Result<Vec<AuditLogEvent>> {
        self.initialize().await?;
        let log_dir = log_dir(&self.config_dir);
        let archive_dir = archive_dir(&self.config_dir);
        let manager = Arc::clone(&self.manager);
        tokio::task::spawn_blocking(move || {
            let _manager = manager
                .lock()
                .map_err(|_| io::Error::other("日志 writer 状态不可用"))?;
            read_activity_events(&log_dir, &archive_dir, limit)
        })
        .await
        .map_err(|error| io::Error::other(format!("近期活动日志读取任务失败: {error}")))?
    }

    pub(crate) fn writer(&self) -> LogWriter {
        LogWriter {
            manager: Arc::clone(&self.manager),
        }
    }
}

pub(crate) fn redact_sensitive_log_values(mut value: Value) -> Value {
    match &mut value {
        Value::Object(object) => {
            for (key, value) in object.iter_mut() {
                let normalized_key = key
                    .chars()
                    .filter(|character| character.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect::<String>();
                if [
                    "password",
                    "token",
                    "secret",
                    "cookie",
                    "authorization",
                    "apikey",
                    "credential",
                ]
                .iter()
                .any(|needle| normalized_key.contains(needle))
                    || normalized_key.ends_with("url")
                    || normalized_key.ends_with("uri")
                {
                    *value = Value::String("[REDACTED]".to_owned());
                } else {
                    *value = redact_sensitive_log_values(value.clone());
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                *value = redact_sensitive_log_values(value.clone());
            }
        }
        Value::String(value) => {
            let normalized = value.to_ascii_lowercase();
            if value.contains("http://") || value.contains("https://") {
                *value = "[REDACTED_URL]".to_owned();
            } else if [
                "token=",
                "api_key=",
                "apikey=",
                "secret=",
                "password=",
                "cookie=",
                "authorization:",
                "bearer ",
            ]
            .iter()
            .any(|needle| normalized.contains(needle))
            {
                *value = "[REDACTED]".to_owned();
            }
        }
        _ => {}
    }
    value
}

// ---------------------------------------------------------------------------------------------
// Bounded event reads
//
// The page readers below must do work proportional to the requested page instead of the retained
// log volume. Three properties make that possible:
//
// * a record is stored in the file named after the record's own UTC day:
//   `LogManager::write_line_for_date` derives the file date from `timestamp`/`createdAt`, and the
//   migration batches in `storage::database_cleanup` keep both fields identical, so a newer day
//   file only holds newer `createdAt` values;
// * inside one day the file name says nothing about ordering, so a whole day is always read;
// * inside one segment (daily file or archive member) the JSONL is append-only, so the newest
//   records sit at the end; a segment larger than the remaining budget is therefore read from its
//   tail (`read_daily_log_file`, `read_archive_member`).
//
// Sources are therefore visited newest day first and a read stops once nothing left in an older day
// can outrank the events already collected for the page. A stop is also refused when the weakest
// retained record is older than the next unread day: that is what a migrated record with an old
// timestamp inside a newer file looks like, and reading further is the only safe answer.
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum LogSourceKind {
    /// Daily file the writer still appends to.
    Daily,
    /// Archived segment, `lux.YYYY-MM-DD.part-NNNNNN.log.zip`.
    Archive,
}

#[derive(Debug)]
struct LogSource {
    /// UTC day encoded in the file name; `None` when the name carries no parseable day.
    date: Option<Date>,
    /// Segment number inside one day; a higher number was written later.
    sequence: u64,
    kind: LogSourceKind,
    path: PathBuf,
}

/// One item handed to a bounded reader.
enum ScanItem<'a> {
    /// A JSONL record without its trailing newline.
    Line(&'a [u8]),
    /// Every source for `completed` has been read. `next` is `None` when nothing is left, and
    /// `Some(None)` when the next older source carries no day we could order.
    GroupCompleted {
        completed: Option<Date>,
        next: Option<Option<Date>>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScanControl {
    Continue,
    Stop,
}

/// Sort key shared by the file-backed event types: newest `created_at` first, then highest id.
trait RankedLogEvent {
    fn rank_id(&self) -> &str;
    fn rank_created_at(&self) -> i64;
}

impl RankedLogEvent for AuditLogEvent {
    fn rank_id(&self) -> &str {
        &self.id
    }

    fn rank_created_at(&self) -> i64 {
        self.created_at
    }
}

impl RankedLogEvent for ScanJobLogEvent {
    fn rank_id(&self) -> &str {
        &self.id
    }

    fn rank_created_at(&self) -> i64 {
        self.created_at
    }
}

/// Sorts events by the shared ordering contract: newest `created_at` first, then highest id.
fn sort_by_rank<T: RankedLogEvent>(events: &mut [T]) {
    events.sort_by(|left, right| {
        right
            .rank_created_at()
            .cmp(&left.rank_created_at())
            .then_with(|| right.rank_id().cmp(left.rank_id()))
    });
}

/// Keeps only the events that can still reach the requested page, so memory and sorting cost track
/// the page size instead of the log volume.
///
/// `push` appends to a buffer and only folds the buffer into the sorted window once it is a window
/// size long, so one event costs O(log window) amortised instead of moving the whole window.
struct PageCollector<T> {
    offset: usize,
    limit: usize,
    /// Distinct events already known to rank before the requested page.
    settled: usize,
    /// Best distinct events of the page window, highest rank first; only valid after `flush`.
    retained: Vec<T>,
    /// Events seen since the last `flush`, not sorted yet.
    pending: Vec<T>,
    seen_ids: HashSet<String>,
}

impl<T: RankedLogEvent> PageCollector<T> {
    fn new(offset: usize, limit: usize) -> Self {
        Self {
            offset,
            limit,
            settled: 0,
            retained: Vec::new(),
            pending: Vec::new(),
            seen_ids: HashSet::new(),
        }
    }

    fn push(&mut self, event: T) {
        if !self.seen_ids.insert(event.rank_id().to_owned()) {
            return;
        }
        self.pending.push(event);
        if self.pending.len() >= self.window().max(1) {
            self.flush();
        }
    }

    /// Folds the pending events into the sorted window and drops everything that cannot reach it.
    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        self.retained.append(&mut self.pending);
        sort_by_rank(&mut self.retained);
        let window = self.window();
        self.retained.truncate(window);
    }

    fn window(&self) -> usize {
        self.offset
            .saturating_sub(self.settled)
            .saturating_add(self.limit)
    }

    /// `true` once enough distinct events are buffered to fill the page. Requires a `flush` first.
    fn is_page_complete(&self) -> bool {
        self.settled.saturating_add(self.retained.len()) >= self.offset.saturating_add(self.limit)
    }

    /// Lowest `created_at` still inside the page window; `None` when the window is empty. Requires a
    /// `flush` first.
    fn floor_created_at(&self) -> Option<i64> {
        self.retained.last().map(RankedLogEvent::rank_created_at)
    }

    /// Drops the events that certainly rank before the requested page.
    ///
    /// Only valid once a whole day has been read *and* the caller proved that the remaining days
    /// cannot hold anything that outranks this window (`floor_dominates_older_days`): a migrated
    /// record with an old `createdAt` can sit in a newer file, which reorders the window later.
    fn settle(&mut self) {
        let dropped = self
            .offset
            .saturating_sub(self.settled)
            .min(self.retained.len());
        if dropped > 0 {
            self.retained.drain(..dropped);
            self.settled = self.settled.saturating_add(dropped);
        }
    }

    fn distinct_total(&self) -> usize {
        self.seen_ids.len()
    }

    /// Applies a final `flush` and returns the requested page.
    fn into_page(mut self) -> Vec<T> {
        self.flush();
        self.retained
            .into_iter()
            .skip(self.offset.saturating_sub(self.settled))
            .take(self.limit)
            .collect()
    }
}

fn page_offset(offset: i64) -> usize {
    usize::try_from(offset.max(0)).unwrap_or(usize::MAX)
}

fn page_limit(limit: i64) -> usize {
    usize::try_from(limit.max(0)).unwrap_or(usize::MAX)
}

/// Resolves `offset`/`limit` into a page window, or `None` for pages deeper than
/// `MAX_LOG_PAGE_WINDOW`, which are answered without touching the log files at all.
fn bounded_page_window(offset: i64, limit: i64) -> Option<(usize, usize)> {
    let offset = page_offset(offset);
    let limit = page_limit(limit);
    (offset.saturating_add(limit) <= MAX_LOG_PAGE_WINDOW).then_some((offset, limit))
}

fn day_start_timestamp(date: Date) -> i64 {
    date.midnight().assume_utc().unix_timestamp()
}

/// True when every record still unread lives in an older day than the collected page floor.
fn floor_dominates_older_days(floor: Option<i64>, next: Option<Option<Date>>) -> bool {
    match next {
        // Nothing left to read.
        None => true,
        // A source without a day is still unread: its records could rank anywhere, so keep going.
        Some(None) => false,
        Some(Some(next)) => {
            floor.is_some_and(|floor| floor >= day_start_timestamp(add_days(next, 1)))
        }
    }
}

fn read_directory(directory: &Path) -> io::Result<Option<fs::ReadDir>> {
    match fs::read_dir(directory) {
        Ok(entries) => Ok(Some(entries)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn parse_archive_log_name(name: &str) -> Option<(Date, u64)> {
    let date_and_part = name.strip_prefix("lux.")?.strip_suffix(".log.zip")?;
    let (date, part) = date_and_part.split_once(".part-")?;
    let date = parse_date(date).ok()?;
    let sequence = part.parse::<u64>().ok()?;
    Some((date, sequence))
}

fn compare_log_sources(left: &LogSource, right: &LogSource) -> std::cmp::Ordering {
    let by_date = match (left.date, right.date) {
        // Names without a day cannot be ordered: read them first instead of guessing.
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(left_date), Some(right_date)) => right_date.cmp(&left_date),
    };
    by_date
        .then_with(|| left.kind.cmp(&right.kind))
        .then_with(|| right.sequence.cmp(&left.sequence))
        .then_with(|| left.path.cmp(&right.path))
}

fn collect_log_sources(log_dir: &Path, archive_dir: &Path) -> io::Result<Vec<LogSource>> {
    let mut sources = Vec::new();
    if let Some(entries) = read_directory(log_dir)? {
        for entry in entries {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !(name.starts_with("lux.") && name.ends_with(".log")) {
                continue;
            }
            sources.push(LogSource {
                date: parse_daily_log_name(name),
                sequence: 0,
                kind: LogSourceKind::Daily,
                path,
            });
        }
    }
    if let Some(entries) = read_directory(archive_dir)? {
        for entry in entries {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.ends_with(".log.zip") {
                continue;
            }
            let parsed = parse_archive_log_name(name);
            sources.push(LogSource {
                date: parsed.map(|(date, _)| date),
                sequence: parsed.map_or(0, |(_, sequence)| sequence),
                kind: LogSourceKind::Archive,
                path,
            });
        }
    }
    sources.sort_by(compare_log_sources);
    Ok(sources)
}

/// Streams every candidate log source newest day first while the byte and file ceilings allow it.
///
/// Returns `true` when the scan stopped before visiting every source because a ceiling was reached.
fn scan_log_records<F>(
    log_dir: &Path,
    archive_dir: &Path,
    max_bytes: u64,
    mut on_item: F,
) -> io::Result<bool>
where
    F: FnMut(ScanItem<'_>) -> io::Result<ScanControl>,
{
    let sources = collect_log_sources(log_dir, archive_dir)?;
    let mut remaining_bytes = max_bytes;
    let mut opened_files = 0_usize;
    let mut index = 0_usize;
    while index < sources.len() {
        let completed = sources[index].date;
        let mut group_end = index + 1;
        while group_end < sources.len() && sources[group_end].date == completed {
            group_end += 1;
        }
        let next = sources.get(group_end).map(|source| source.date);
        for source in &sources[index..group_end] {
            if opened_files >= MAX_EVENT_SCAN_FILES || remaining_bytes == 0 {
                return Ok(true);
            }
            opened_files = opened_files.saturating_add(1);
            if read_log_source(source, &mut remaining_bytes, &mut on_item)? == ScanControl::Stop {
                return Ok(true);
            }
        }
        index = group_end;
        if on_item(ScanItem::GroupCompleted { completed, next })? == ScanControl::Stop {
            return Ok(false);
        }
    }
    Ok(false)
}

fn read_log_source<F>(
    source: &LogSource,
    remaining_bytes: &mut u64,
    on_item: &mut F,
) -> io::Result<ScanControl>
where
    F: FnMut(ScanItem<'_>) -> io::Result<ScanControl>,
{
    let file = fs::File::open(&source.path)?;
    match source.kind {
        LogSourceKind::Daily => read_daily_log_file(file, remaining_bytes, on_item),
        LogSourceKind::Archive => {
            let mut archive = ZipArchive::new(file).map_err(zip_io_error)?;
            for index in 0..archive.len() {
                if *remaining_bytes == 0 {
                    return Ok(ScanControl::Stop);
                }
                let member = archive.by_index(index).map_err(zip_io_error)?;
                if !member.name().ends_with(".log") {
                    continue;
                }
                let length = member.size();
                if read_archive_member(member, length, remaining_bytes, on_item)?
                    == ScanControl::Stop
                {
                    return Ok(ScanControl::Stop);
                }
            }
            Ok(ScanControl::Continue)
        }
    }
}

/// Reads one daily file. When the remaining budget cannot cover the whole file only its tail is
/// read, because append-only JSONL keeps the newest records there.
fn read_daily_log_file<F>(
    mut file: fs::File,
    remaining_bytes: &mut u64,
    on_item: &mut F,
) -> io::Result<ScanControl>
where
    F: FnMut(ScanItem<'_>) -> io::Result<ScanControl>,
{
    let length = file.metadata()?.len();
    if length <= *remaining_bytes {
        let mut reader = io::BufReader::new(file);
        return scan_jsonl(&mut reader, remaining_bytes, on_item);
    }
    file.seek(SeekFrom::Start(length.saturating_sub(*remaining_bytes)))?;
    scan_segment_tail(io::BufReader::new(file), remaining_bytes, on_item)
}

/// Reads one archive member, which is a segment of the same append-only JSONL as a daily file.
///
/// When the remaining budget cannot cover the member's uncompressed size, the head is decompressed
/// and dropped in a fixed-size buffer instead of being parsed, so the request still returns the
/// member's newest records. The decode cost stays bounded by one segment (`LOG_SEGMENT_BYTES`), and
/// nothing beyond the buffer reaches memory or the parser. A member this large is the newest source
/// left, so the caller must not continue with older sources afterwards: the result is `Stop`.
fn read_archive_member<R, F>(
    member: R,
    length: u64,
    remaining_bytes: &mut u64,
    on_item: &mut F,
) -> io::Result<ScanControl>
where
    R: io::Read,
    F: FnMut(ScanItem<'_>) -> io::Result<ScanControl>,
{
    let mut reader = io::BufReader::new(member);
    if length <= *remaining_bytes {
        return scan_jsonl(&mut reader, remaining_bytes, on_item);
    }
    discard_bytes(&mut reader, length.saturating_sub(*remaining_bytes))?;
    scan_segment_tail(reader, remaining_bytes, on_item)
}

/// Parses a segment tail after its head was skipped: the skip can land inside a record, so the
/// fragment is dropped instead of being parsed. Always reports `Stop` because the head was skipped.
fn scan_segment_tail<R, F>(
    mut reader: R,
    remaining_bytes: &mut u64,
    on_item: &mut F,
) -> io::Result<ScanControl>
where
    R: io::BufRead,
    F: FnMut(ScanItem<'_>) -> io::Result<ScanControl>,
{
    if discard_partial_line(&mut reader)? {
        scan_jsonl(&mut reader, remaining_bytes, on_item)?;
    }
    Ok(ScanControl::Stop)
}

/// Advances a reader by `bytes` through a fixed-size buffer, decompressing but never buffering the
/// skipped data.
fn discard_bytes<R: io::Read>(reader: &mut R, mut bytes: u64) -> io::Result<()> {
    let mut buffer = [0_u8; 8 * 1024];
    while bytes > 0 {
        let limit = usize::try_from(bytes.min(u64::try_from(buffer.len()).unwrap_or(u64::MAX)))
            .unwrap_or(buffer.len());
        if limit == 0 {
            return Ok(());
        }
        let read = reader.read(&mut buffer[..limit])?;
        if read == 0 {
            return Ok(());
        }
        bytes = bytes.saturating_sub(u64::try_from(read).unwrap_or(0));
    }
    Ok(())
}

fn discard_partial_line<R: io::BufRead>(reader: &mut R) -> io::Result<bool> {
    loop {
        let (found, consumed) = {
            let buffer = reader.fill_buf()?;
            if buffer.is_empty() {
                return Ok(false);
            }
            match buffer.iter().position(|byte| *byte == b'\n') {
                Some(index) => (true, index.saturating_add(1)),
                None => (false, buffer.len()),
            }
        };
        reader.consume(consumed);
        if found {
            return Ok(true);
        }
    }
}

/// Streams newline-delimited records while enforcing the byte ceiling and the record size ceiling.
fn scan_jsonl<R, F>(
    reader: &mut R,
    remaining_bytes: &mut u64,
    on_item: &mut F,
) -> io::Result<ScanControl>
where
    R: io::BufRead,
    F: FnMut(ScanItem<'_>) -> io::Result<ScanControl>,
{
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let mut control = ScanControl::Continue;
        let consumed = {
            let chunk = reader.fill_buf()?;
            if chunk.is_empty() {
                if !line.is_empty() && !oversized {
                    control = on_item(ScanItem::Line(&line))?;
                }
                return Ok(control);
            }
            let available = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
            let allowed = usize::try_from((*remaining_bytes).min(available)).unwrap_or(0);
            if allowed == 0 {
                return Ok(ScanControl::Stop);
            }
            let bytes = &chunk[..allowed];
            let mut start = 0_usize;
            for (index, byte) in bytes.iter().enumerate() {
                if *byte != b'\n' {
                    continue;
                }
                if oversized {
                    oversized = false;
                } else if line.is_empty() {
                    control = on_item(ScanItem::Line(&bytes[start..index]))?;
                } else {
                    line.extend_from_slice(&bytes[start..index]);
                    control = on_item(ScanItem::Line(&line))?;
                    line.clear();
                }
                start = index.saturating_add(1);
                if control == ScanControl::Stop {
                    break;
                }
            }
            if control == ScanControl::Continue && start < bytes.len() && !oversized {
                let tail = &bytes[start..];
                if line.len().saturating_add(tail.len()) > MAX_EVENT_LINE_BYTES {
                    oversized = true;
                    line.clear();
                } else {
                    line.extend_from_slice(tail);
                }
            }
            allowed
        };
        reader.consume(consumed);
        *remaining_bytes = (*remaining_bytes).saturating_sub(u64::try_from(consumed).unwrap_or(0));
        if control == ScanControl::Stop {
            return Ok(ScanControl::Stop);
        }
    }
}

/// Cheap prefilter so most non-matching JSONL records never reach the JSON parser.
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    let last_start = haystack.len().saturating_sub(needle.len());
    let first = needle[0];
    let mut index = 0_usize;
    while index <= last_start {
        match haystack[index..=last_start]
            .iter()
            .position(|byte| *byte == first)
        {
            Some(offset) => {
                let start = index.saturating_add(offset);
                let end = start.saturating_add(needle.len());
                if &haystack[start..end] == needle {
                    return true;
                }
                index = start.saturating_add(1);
            }
            None => return false,
        }
    }
    false
}

fn parse_scan_job_event(
    line: &[u8],
    job_id: &str,
    level: Option<&str>,
    event_code: Option<&str>,
) -> Option<ScanJobLogEvent> {
    let record = serde_json::from_slice::<Value>(line).ok()?;
    if record["recordType"] != "scan_job_event" || record["jobId"] != job_id {
        return None;
    }
    let id = record["id"].as_str()?;
    let event_level = record["level"].as_str()?;
    let code = record["eventCode"].as_str()?;
    if level.is_some_and(|value| !event_level.eq_ignore_ascii_case(value))
        || event_code.is_some_and(|value| !code.eq_ignore_ascii_case(value))
    {
        return None;
    }
    let details_json =
        serde_json::to_string(&record["details"]).unwrap_or_else(|_| "{}".to_owned());
    Some(ScanJobLogEvent {
        id: id.to_owned(),
        job_id: job_id.to_owned(),
        level: event_level.to_owned(),
        event_code: code.to_owned(),
        message: record["message"].as_str().unwrap_or_default().to_owned(),
        details_json,
        created_at: record["createdAt"].as_i64().unwrap_or_default(),
    })
}

fn read_scan_job_events(
    log_dir: &Path,
    archive_dir: &Path,
    job_id: &str,
    level: Option<&str>,
    event_code: Option<&str>,
    offset: i64,
    limit: i64,
) -> io::Result<(i64, Vec<ScanJobLogEvent>)> {
    let Some((offset, limit)) = bounded_page_window(offset, limit) else {
        // A page deeper than `MAX_LOG_PAGE_WINDOW` is refused instead of buffering the whole log;
        // `total` reports the lower bound zero because counting it would need the declined scan.
        return Ok((0, Vec::new()));
    };
    let mut collector = PageCollector::new(offset, limit);
    let job_id_bytes = job_id.as_bytes();
    scan_log_records(log_dir, archive_dir, MAX_JOB_EVENT_SCAN_BYTES, |item| {
        if let ScanItem::Line(line) = item
            && contains_bytes(line, b"scan_job_event")
            && contains_bytes(line, job_id_bytes)
            && let Some(event) = parse_scan_job_event(line, job_id, level, event_code)
        {
            collector.push(event);
        }
        // The API response carries an exact `total`, so every file still has to be visited.
        Ok(ScanControl::Continue)
    })?;
    // Exact while the whole retained log fit into the scan ceiling, a lower bound once it was cut.
    let total = i64::try_from(collector.distinct_total()).unwrap_or(i64::MAX);
    Ok((total, collector.into_page()))
}

fn verify_expected_log_ids(
    expected_ids: &[String],
    persisted_ids: &HashSet<String>,
    description: &str,
) -> io::Result<()> {
    if expected_ids
        .iter()
        .any(|expected_id| !persisted_ids.contains(expected_id))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{description}文件缺少本批次事件记录"),
        ));
    }
    Ok(())
}

fn read_record_ids(
    log_dir: &Path,
    archive_dir: &Path,
    record_type: &str,
    expected_ids: &HashSet<String>,
) -> io::Result<HashSet<String>> {
    let mut found_ids = HashSet::with_capacity(expected_ids.len());
    let active_entries = match fs::read_dir(log_dir) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if let Some(entries) = active_entries {
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if name.starts_with("lux.") && name.ends_with(".log") {
                read_record_ids_from_lines(
                    io::BufReader::new(fs::File::open(path)?),
                    record_type,
                    expected_ids,
                    &mut found_ids,
                )?;
                if found_ids.len() == expected_ids.len() {
                    return Ok(found_ids);
                }
            }
        }
    }
    let archive_entries = match fs::read_dir(archive_dir) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if let Some(entries) = archive_entries {
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".log.zip"))
            {
                continue;
            }
            let mut archive = ZipArchive::new(fs::File::open(path)?)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            for index in 0..archive.len() {
                let member = archive
                    .by_index(index)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                if member.name().ends_with(".log") {
                    read_record_ids_from_lines(
                        io::BufReader::new(member),
                        record_type,
                        expected_ids,
                        &mut found_ids,
                    )?;
                    if found_ids.len() == expected_ids.len() {
                        return Ok(found_ids);
                    }
                }
            }
        }
    }
    Ok(found_ids)
}

fn read_record_ids_from_lines<R: io::BufRead>(
    mut reader: R,
    record_type: &str,
    expected_ids: &HashSet<String>,
    found_ids: &mut HashSet<String>,
) -> io::Result<()> {
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        let Ok(record) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if record["recordType"] == record_type
            && let Some(id) = record["id"].as_str()
            && expected_ids.contains(id)
        {
            found_ids.insert(id.to_owned());
            if found_ids.len() == expected_ids.len() {
                return Ok(());
            }
        }
    }
}

fn parse_audit_event(line: &[u8], event_types: Option<&[&str]>) -> Option<AuditLogEvent> {
    let record = serde_json::from_slice::<Value>(line).ok()?;
    if record["recordType"] != "admin_audit_event" {
        return None;
    }
    let id = record["id"].as_str()?;
    let event_type = record["eventType"].as_str()?;
    if event_types.is_some_and(|allowed| !allowed.contains(&event_type)) {
        return None;
    }
    let metadata_json =
        serde_json::to_string(&record["metadata"]).unwrap_or_else(|_| "{}".to_owned());
    Some(AuditLogEvent {
        id: id.to_owned(),
        actor_user_id: record["actorUserId"].as_str().map(str::to_owned),
        actor_username: record["actorUsername"].as_str().map(str::to_owned),
        event_type: event_type.to_owned(),
        target_type: record["targetType"].as_str().map(str::to_owned),
        target_id: record["targetId"].as_str().map(str::to_owned),
        metadata_json,
        created_at: record["createdAt"].as_i64().unwrap_or_default(),
    })
}

fn read_audit_events(
    log_dir: &Path,
    archive_dir: &Path,
    offset: i64,
    limit: i64,
    event_types: Option<&[&str]>,
) -> io::Result<(i64, Vec<AuditLogEvent>)> {
    let Some((offset, limit)) = bounded_page_window(offset, limit) else {
        // A page deeper than `MAX_LOG_PAGE_WINDOW` is refused instead of buffering the whole log.
        // `total` reports the lower bound zero because counting it would need the scan we declined;
        // the only caller of this path (`admin_list_audit`) discards the total.
        return Ok((0, Vec::new()));
    };
    let mut collector = PageCollector::new(offset, limit);
    scan_log_records(
        log_dir,
        archive_dir,
        MAX_EVENT_SCAN_BYTES,
        |item| match item {
            ScanItem::Line(line) => {
                if contains_bytes(line, b"admin_audit_event")
                    && let Some(event) = parse_audit_event(line, event_types)
                {
                    collector.push(event);
                }
                Ok(ScanControl::Continue)
            }
            ScanItem::GroupCompleted { completed, next } => {
                if completed.is_none() {
                    // A name without a day cannot be ordered, so keep scanning instead of stopping.
                    return Ok(ScanControl::Continue);
                }
                collector.flush();
                // Only settle once the remaining days provably cannot outrank this window; a
                // migrated record with an old `createdAt` in a newer file would reorder it.
                let page_is_ordered =
                    floor_dominates_older_days(collector.floor_created_at(), next);
                if page_is_ordered {
                    collector.settle();
                }
                let page_is_final = page_is_ordered && collector.is_page_complete();
                Ok(if page_is_final {
                    ScanControl::Stop
                } else {
                    ScanControl::Continue
                })
            }
        },
    )?;
    // Exact while the whole log fit into the scan ceiling, a lower bound once it was truncated.
    // Every current caller of this path discards the total (see `admin_list_audit`).
    let total = i64::try_from(collector.distinct_total()).unwrap_or(i64::MAX);
    Ok((total, collector.into_page()))
}

fn read_activity_events(
    log_dir: &Path,
    archive_dir: &Path,
    limit: i64,
) -> io::Result<Vec<AuditLogEvent>> {
    if bounded_page_window(0, limit).is_none() {
        // Same refusal as the audit page: never buffer more than `MAX_LOG_PAGE_WINDOW` events.
        return Ok(Vec::new());
    }
    let category_limit = usize::try_from((limit / 2).max(1)).unwrap_or(usize::MAX);
    let mut login_events = PageCollector::new(0, category_limit);
    let mut playback_events = PageCollector::new(0, category_limit);
    scan_log_records(
        log_dir,
        archive_dir,
        MAX_EVENT_SCAN_BYTES,
        |item| match item {
            ScanItem::Line(line) => {
                if contains_bytes(line, b"admin_audit_event")
                    && let Some(event) = parse_audit_event(line, Some(&ACTIVITY_EVENT_TYPES))
                {
                    if event.event_type == "AUTH_LOGIN" {
                        login_events.push(event);
                    } else {
                        playback_events.push(event);
                    }
                }
                Ok(ScanControl::Continue)
            }
            ScanItem::GroupCompleted { completed, next } => {
                if completed.is_none() {
                    return Ok(ScanControl::Continue);
                }
                login_events.flush();
                playback_events.flush();
                let category_is_final = |collector: &PageCollector<AuditLogEvent>| {
                    collector.is_page_complete()
                        && floor_dominates_older_days(collector.floor_created_at(), next)
                };
                let page_is_final =
                    category_is_final(&login_events) && category_is_final(&playback_events);
                Ok(if page_is_final {
                    ScanControl::Stop
                } else {
                    ScanControl::Continue
                })
            }
        },
    )?;
    let mut events = login_events.into_page();
    events.extend(playback_events.into_page());
    sort_audit_events(&mut events);
    let mut seen_ids = HashSet::with_capacity(events.len());
    events.retain(|event| seen_ids.insert(event.id.clone()));
    events.truncate(page_limit(limit));
    Ok(events)
}

fn sort_audit_events(events: &mut [AuditLogEvent]) {
    sort_by_rank(events);
}

pub(crate) struct LogWriter {
    manager: SharedLogManager,
}

impl Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut manager = self
            .manager
            .lock()
            .map_err(|_| io::Error::other("日志 writer 状态不可用"))?;
        let manager = manager
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "日志 writer 尚未初始化"))?;
        manager.write_bytes(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut manager = self
            .manager
            .lock()
            .map_err(|_| io::Error::other("日志 writer 状态不可用"))?;
        if let Some(manager) = manager.as_mut() {
            manager.flush_pending()?;
        }
        Ok(())
    }
}

struct LogManager {
    log_dir: PathBuf,
    archive_dir: PathBuf,
    active_date: Date,
    active_path: PathBuf,
    active_file: Option<fs::File>,
    active_bytes: u64,
    pending_bytes: Vec<u8>,
    segment_limit: u64,
    archive_limit: usize,
    defer_archive_pruning: bool,
}

/// Creates a directory only its owner can traverse; also tightens legacy 0755 directories.
fn create_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        if permissions.mode() & 0o077 != 0 {
            permissions.set_mode(0o700);
            fs::set_permissions(path, permissions)?;
        }
    }
    Ok(())
}

/// Ensures a log file is owner-only, including files written by older versions as 0644.
fn restrict_log_file_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        if permissions.mode() & 0o077 != 0 {
            permissions.set_mode(0o600);
            fs::set_permissions(path, permissions)?;
        }
    }
    Ok(())
}

impl LogManager {
    fn open(config_dir: &Path) -> io::Result<Self> {
        Self::open_with_limits(
            config_dir,
            OffsetDateTime::now_utc().date(),
            LOG_SEGMENT_BYTES,
            MAX_LOG_ARCHIVES,
        )
    }

    fn open_with_limits(
        config_dir: &Path,
        active_date: Date,
        segment_limit: u64,
        archive_limit: usize,
    ) -> io::Result<Self> {
        let log_dir = log_dir(config_dir);
        let archive_dir = archive_dir(config_dir);
        create_private_directory(&log_dir)?;
        create_private_directory(&archive_dir)?;
        let active_path = log_dir.join(log_file_name(active_date));
        let mut manager = Self {
            log_dir,
            archive_dir,
            active_date,
            active_path,
            active_file: None,
            active_bytes: 0,
            pending_bytes: Vec::new(),
            segment_limit: segment_limit.max(1),
            archive_limit,
            defer_archive_pruning: false,
        };
        manager.archive_previous_days(active_date)?;
        manager.open_active_file()?;
        if manager.active_bytes >= manager.segment_limit {
            manager.archive_active()?;
            manager.open_active_file()?;
        }
        manager.prune_archives()?;
        Ok(manager)
    }

    fn archive_previous_days(&mut self, active_date: Date) -> io::Result<()> {
        let entries = fs::read_dir(&self.log_dir)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_str()?;
                let date = parse_daily_log_name(name)?;
                (date < active_date).then_some((path, date))
            })
            .collect::<Vec<_>>();
        for (path, date) in entries {
            self.archive_path(&path, date)?;
        }
        Ok(())
    }

    fn open_active_file(&mut self) -> io::Result<()> {
        create_private_directory(&self.log_dir)?;
        let mut options = fs::OpenOptions::new();
        options.create(true).append(true).read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&self.active_path)?;
        restrict_log_file_permissions(&self.active_path)?;
        self.active_bytes = repair_incomplete_jsonl_tail(&file)?;
        self.active_file = Some(file);
        Ok(())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.pending_bytes.extend_from_slice(bytes);
        while let Some(newline) = self.pending_bytes.iter().position(|byte| *byte == b'\n') {
            let line = self.pending_bytes.drain(..=newline).collect::<Vec<_>>();
            self.write_line(&line)?;
        }
        Ok(())
    }

    fn flush_pending(&mut self) -> io::Result<()> {
        if !self.pending_bytes.is_empty() {
            let mut line = std::mem::take(&mut self.pending_bytes);
            if !line.ends_with(b"\n") {
                line.push(b'\n');
            }
            self.write_line(&line)?;
        }
        if let Some(file) = self.active_file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }

    fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        self.write_line_for_date(OffsetDateTime::now_utc().date(), line)
    }

    fn write_line_for_date(&mut self, date: Date, line: &[u8]) -> io::Result<()> {
        if date != self.active_date {
            self.archive_active()?;
            self.active_date = date;
            self.active_path = self.log_dir.join(log_file_name(date));
            self.open_active_file()?;
        }
        // `active_bytes` tracks every write, so a plain size comparison detects a torn tail left
        // by another writer (or a previous crash) without re-reading the file per line.
        if let Some(file) = self.active_file.as_ref()
            && file.metadata()?.len() != self.active_bytes
        {
            self.active_bytes = repair_incomplete_jsonl_tail(file)?;
        }
        let line_len = u64::try_from(line.len()).unwrap_or(u64::MAX);
        if self.active_bytes > 0 && self.active_bytes.saturating_add(line_len) > self.segment_limit
        {
            self.archive_active()?;
            self.open_active_file()?;
        }
        let file = self
            .active_file
            .as_mut()
            .ok_or_else(|| io::Error::other("日志活动文件未打开"))?;
        file.write_all(line)?;
        self.active_bytes = self.active_bytes.saturating_add(line_len);
        if self.active_bytes >= self.segment_limit {
            self.archive_active()?;
            self.open_active_file()?;
        }
        Ok(())
    }

    fn archive_active(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.active_file.take() {
            file.flush()?;
            file.sync_all()?;
        }
        if self.active_bytes > 0 {
            self.archive_path(&self.active_path.clone(), self.active_date)?;
        } else if self.active_bytes == 0 {
            self.active_file = None;
        }
        self.active_bytes = 0;
        Ok(())
    }

    fn archive_path(&mut self, source_path: &Path, date: Date) -> io::Result<()> {
        if !source_path.exists() {
            return Ok(());
        }
        let source_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(source_path)?;
        let source_len = repair_incomplete_jsonl_tail(&source_file)?;
        drop(source_file);
        if source_len == 0 {
            fs::remove_file(source_path)?;
            return Ok(());
        }
        let sequence = self.next_sequence(date)?;
        let member_name = log_segment_file_name(date, sequence);
        let archive_name = format!("{member_name}.zip");
        let archive_path = self.archive_dir.join(&archive_name);
        let temp_path = self
            .archive_dir
            .join(format!(".{archive_name}.{}.tmp", uuid::Uuid::now_v7()));
        let temp_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        let result = (|| {
            let mut writer = ZipWriter::new(temp_file);
            writer
                .start_file(
                    &member_name,
                    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
                )
                .map_err(zip_io_error)?;
            let mut source = fs::File::open(source_path)?;
            io::copy(&mut source, &mut writer)?;
            let mut archive_file = writer.finish().map_err(zip_io_error)?;
            archive_file.flush()?;
            archive_file.sync_all()?;
            verify_archive(&temp_path, &member_name)?;
            if archive_path.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "日志归档目标已存在",
                ));
            }
            fs::rename(&temp_path, &archive_path)?;
            fs::remove_file(source_path)?;
            if !self.defer_archive_pruning {
                self.prune_archives()?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }

    fn next_sequence(&self, date: Date) -> io::Result<u64> {
        let prefix = format!("lux.{}.part-", format_log_date(date));
        let mut next = 1_u64;
        for entry in fs::read_dir(&self.archive_dir)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if let Some(sequence) = name
                .strip_prefix(&prefix)
                .and_then(|value| value.strip_suffix(".log.zip"))
                .and_then(|value| value.parse::<u64>().ok())
            {
                next = next.max(sequence.saturating_add(1));
            }
        }
        Ok(next)
    }

    fn prune_archives(&self) -> io::Result<()> {
        let mut archives = fs::read_dir(&self.archive_dir)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".log.zip"))
                    .then_some(path)
            })
            .collect::<Vec<_>>();
        archives.sort();
        let remove_count = archives.len().saturating_sub(self.archive_limit);
        for path in archives.into_iter().take(remove_count) {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

fn repair_incomplete_jsonl_tail(file: &fs::File) -> io::Result<u64> {
    const CHUNK_BYTES: u64 = 8 * 1024;
    let original_len = file.metadata()?.len();
    let mut remaining = original_len;
    let mut reader = file;
    let mut buffer = [0_u8; CHUNK_BYTES as usize];
    while remaining > 0 {
        let chunk_start = remaining.saturating_sub(CHUNK_BYTES);
        let chunk_len = usize::try_from(remaining - chunk_start).map_err(io::Error::other)?;
        reader.seek(SeekFrom::Start(chunk_start))?;
        reader.read_exact(&mut buffer[..chunk_len])?;
        if let Some(newline_index) = buffer[..chunk_len].iter().rposition(|byte| *byte == b'\n') {
            let valid_len = chunk_start
                .saturating_add(u64::try_from(newline_index).map_err(io::Error::other)?)
                .saturating_add(1);
            if valid_len < original_len {
                file.set_len(valid_len)?;
                file.sync_all()?;
            }
            return Ok(valid_len);
        }
        remaining = chunk_start;
    }
    file.set_len(0)?;
    file.sync_all()?;
    Ok(0)
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LogDateRange {
    pub from: Date,
    pub to: Date,
}

impl LogDateRange {
    pub fn from_query(from: Option<&str>, to: Option<&str>) -> Result<Self, LogExportError> {
        let today = OffsetDateTime::now_utc().date();
        let to = to.map(parse_date).transpose()?.unwrap_or(today);
        let from = from
            .map(parse_date)
            .transpose()?
            .unwrap_or_else(|| subtract_days(to, DEFAULT_EXPORT_DAYS - 1));
        Self::new(from, to)
    }

    pub fn new(from: Date, to: Date) -> Result<Self, LogExportError> {
        if from > to {
            return Err(LogExportError::DateRangeReversed);
        }
        let days = (to - from).whole_days().saturating_add(1);
        if days > MAX_EXPORT_DAYS {
            return Err(LogExportError::DateRangeTooLarge);
        }
        Ok(Self { from, to })
    }

    fn dates(self) -> impl Iterator<Item = Date> {
        let days = (self.to - self.from).whole_days();
        (0..=days).map(move |offset| add_days(self.from, offset))
    }
}

#[derive(Debug)]
pub enum LogExport {
    Daily { contents: Vec<u8>, filename: String },
    Archive { contents: Vec<u8>, filename: String },
}

#[derive(Debug)]
pub enum LogExportError {
    InvalidDate,
    DateRangeReversed,
    DateRangeTooLarge,
    ExportTooLarge,
    NoLogs,
    Io(io::Error),
    Archive(String),
    Worker(String),
}

impl fmt::Display for LogExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDate => formatter.write_str("日志日期必须使用 YYYY-MM-DD 格式"),
            Self::DateRangeReversed => formatter.write_str("日志起止日期无效"),
            Self::DateRangeTooLarge => formatter.write_str("日志导出范围最多为 31 天"),
            Self::ExportTooLarge => formatter.write_str("日志导出文件过大，请缩小日期范围"),
            Self::NoLogs => formatter.write_str("所选日期没有可导出的日志"),
            Self::Io(_) | Self::Archive(_) | Self::Worker(_) => {
                formatter.write_str("日志文件暂时无法导出")
            }
        }
    }
}

impl std::error::Error for LogExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidDate
            | Self::DateRangeReversed
            | Self::DateRangeTooLarge
            | Self::ExportTooLarge
            | Self::NoLogs
            | Self::Archive(_)
            | Self::Worker(_) => None,
        }
    }
}

pub fn log_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(LOG_DIRECTORY)
}

pub fn archive_dir(config_dir: &Path) -> PathBuf {
    log_dir(config_dir).join(LOG_ARCHIVE_DIRECTORY)
}

pub fn log_file_name(date: Date) -> String {
    format!(
        "lux.{:04}-{:02}-{:02}.log",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

pub async fn export_logs(
    config_dir: &Path,
    range: LogDateRange,
) -> Result<LogExport, LogExportError> {
    let store = LogStore::new(config_dir);
    store.initialize().await.map_err(LogExportError::Io)?;
    let config_dir = config_dir.to_path_buf();
    let manager = Arc::clone(&store.manager);
    let files = tokio::task::spawn_blocking(move || {
        let _manager = manager
            .lock()
            .map_err(|_| LogExportError::Worker("日志 writer 状态不可用".to_owned()))?;
        read_log_files(&config_dir, range)
    })
    .await
    .map_err(|error| LogExportError::Worker(error.to_string()))??;
    if files.is_empty() {
        return Err(LogExportError::NoLogs);
    }

    if range.from == range.to {
        let filename = log_file_name(range.from);
        let mut contents = Vec::new();
        for (_, segment) in files {
            contents.extend_from_slice(&segment);
        }
        return Ok(LogExport::Daily { contents, filename });
    }

    let filename = format!(
        "lux-logs-{}-{}.zip",
        compact_date(range.from),
        compact_date(range.to)
    );
    let archive = tokio::task::spawn_blocking(move || create_archive(files))
        .await
        .map_err(|error| LogExportError::Worker(error.to_string()))??;
    Ok(LogExport::Archive {
        contents: archive,
        filename,
    })
}

fn read_log_files(
    config_dir: &Path,
    range: LogDateRange,
) -> Result<Vec<(String, Vec<u8>)>, LogExportError> {
    let directory = log_dir(config_dir);
    let archive_directory = archive_dir(config_dir);
    let mut files = Vec::new();
    let mut daily_bytes = 0_u64;
    let mut total_bytes = 0_u64;
    for date in range.dates() {
        let date_prefix = format!("lux.{}", format_log_date(date));
        let mut archived = Vec::new();
        match fs::read_dir(&archive_directory) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(LogExportError::Io)?;
                    let path = entry.path();
                    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                        continue;
                    };
                    if !name.starts_with(&format!("{date_prefix}.part-"))
                        || !name.ends_with(".log.zip")
                    {
                        continue;
                    }
                    let archive_file = fs::File::open(&path).map_err(LogExportError::Io)?;
                    let mut archive = ZipArchive::new(archive_file)
                        .map_err(|error| LogExportError::Archive(error.to_string()))?;
                    for index in 0..archive.len() {
                        let mut member = archive
                            .by_index(index)
                            .map_err(|error| LogExportError::Archive(error.to_string()))?;
                        let member_name = member.name().to_owned();
                        if !member_name.starts_with(&format!("{date_prefix}.part-"))
                            || !member_name.ends_with(".log")
                        {
                            continue;
                        }
                        let mut contents = Vec::new();
                        member
                            .read_to_end(&mut contents)
                            .map_err(LogExportError::Io)?;
                        archived.push((member_name, contents));
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(LogExportError::Io(error)),
        }
        archived.sort_by(|left, right| left.0.cmp(&right.0));
        for (name, contents) in archived {
            add_export_file(
                &mut files,
                &mut daily_bytes,
                &mut total_bytes,
                name,
                contents,
            )?;
        }

        let name = log_file_name(date);
        match fs::read(directory.join(&name)) {
            Ok(contents) => add_export_file(
                &mut files,
                &mut daily_bytes,
                &mut total_bytes,
                name,
                contents,
            )?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(LogExportError::Io(error)),
        }
    }
    // Export bundles leave the server, so redact every JSON line once here instead of trusting
    // each producer (tracing lines included) to have done it.
    Ok(files
        .into_iter()
        .map(|(name, bytes)| (name, redact_jsonl_bytes(&bytes)))
        .collect())
}

fn redact_jsonl_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    for chunk in bytes.split_inclusive(|byte| *byte == b'\n') {
        let line = chunk.strip_suffix(b"\n").unwrap_or(chunk);
        match serde_json::from_slice::<Value>(line) {
            Ok(value) => match serde_json::to_vec(&redact_sensitive_log_values(value)) {
                Ok(encoded) => {
                    output.extend_from_slice(&encoded);
                    output.push(b'\n');
                }
                Err(_) => output.extend_from_slice(chunk),
            },
            Err(_) => output.extend_from_slice(chunk),
        }
    }
    output
}

fn add_export_file(
    files: &mut Vec<(String, Vec<u8>)>,
    daily_bytes: &mut u64,
    total_bytes: &mut u64,
    name: String,
    contents: Vec<u8>,
) -> Result<(), LogExportError> {
    let size = u64::try_from(contents.len()).unwrap_or(u64::MAX);
    *daily_bytes = daily_bytes.saturating_add(size);
    *total_bytes = total_bytes.saturating_add(size);
    if *daily_bytes > MAX_DAILY_LOG_BYTES || *total_bytes > MAX_EXPORT_BYTES {
        return Err(LogExportError::ExportTooLarge);
    }
    files.push((name, contents));
    Ok(())
}

fn log_segment_file_name(date: Date, sequence: u64) -> String {
    format!("lux.{}.part-{sequence:06}.log", format_log_date(date))
}

fn format_log_date(date: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

fn parse_daily_log_name(name: &str) -> Option<Date> {
    name.strip_prefix("lux.")?
        .strip_suffix(".log")
        .and_then(|date| parse_date(date).ok())
}

fn verify_archive(path: &Path, member_name: &str) -> io::Result<()> {
    let file = fs::File::open(path)?;
    let mut archive = ZipArchive::new(file).map_err(zip_io_error)?;
    let mut member = archive.by_name(member_name).map_err(zip_io_error)?;
    io::copy(&mut member, &mut io::sink())?;
    Ok(())
}

fn zip_io_error(error: zip::result::ZipError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn create_archive(files: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>, LogExportError> {
    let mut daily_files = BTreeMap::<String, Vec<u8>>::new();
    for (name, contents) in files {
        let date = export_file_date(&name)
            .ok_or_else(|| LogExportError::Archive("日志分段文件名无效".to_owned()))?;
        daily_files
            .entry(log_file_name(date))
            .or_default()
            .extend_from_slice(&contents);
    }
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, contents) in daily_files {
        writer
            .start_file(name, options)
            .map_err(|error| LogExportError::Archive(error.to_string()))?;
        writer
            .write_all(&contents)
            .map_err(|error| LogExportError::Archive(error.to_string()))?;
    }
    writer
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|error| LogExportError::Archive(error.to_string()))
}

fn export_file_date(name: &str) -> Option<Date> {
    let date_and_part = name.strip_prefix("lux.")?.strip_suffix(".log")?;
    let date = date_and_part.split(".part-").next()?;
    parse_date(date).ok()
}

fn parse_date(value: &str) -> Result<Date, LogExportError> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(LogExportError::InvalidDate);
    }
    let year = parse_number(&bytes[0..4])
        .and_then(|value| i32::try_from(value).ok())
        .ok_or(LogExportError::InvalidDate)?;
    let month = parse_number(&bytes[5..7])
        .and_then(|value| u8::try_from(value).ok())
        .and_then(|value| Month::try_from(value).ok())
        .ok_or(LogExportError::InvalidDate)?;
    let day = parse_number(&bytes[8..10])
        .and_then(|value| u8::try_from(value).ok())
        .ok_or(LogExportError::InvalidDate)?;
    Date::from_calendar_date(year, month, day).map_err(|_| LogExportError::InvalidDate)
}

fn parse_number(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0_u32, |value, byte| {
        byte.is_ascii_digit().then_some(
            value
                .saturating_mul(10)
                .saturating_add(u32::from(byte - b'0')),
        )
    })
}

fn add_days(date: Date, days: i64) -> Date {
    let mut current = date;
    for _ in 0..days {
        if let Some(next) = current.next_day() {
            current = next;
        }
    }
    current
}

fn subtract_days(date: Date, days: i64) -> Date {
    let mut current = date;
    for _ in 0..days {
        if let Some(previous) = current.previous_day() {
            current = previous;
        }
    }
    current
}

fn compact_date(date: Date) -> String {
    format!(
        "{:04}{:02}{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        fs,
        io::{self, Read, Write},
        path::Path,
        sync::Arc,
    };

    use super::{
        AuditLogEvent, LOG_SEGMENT_BYTES, LogDateRange, LogExport, LogExportError, LogManager,
        LogStore, MAX_EVENT_SCAN_BYTES, MAX_EVENT_SCAN_FILES, MAX_JOB_EVENT_SCAN_BYTES,
        MAX_LOG_ARCHIVES, MAX_LOG_PAGE_WINDOW, NewAuditLogEvent, ScanJobLogEvent, archive_dir,
        export_logs, log_dir, log_file_name, log_segment_file_name, parse_date,
        read_activity_events, read_audit_events, read_scan_job_events, sort_audit_events,
        verify_archive,
    };
    use serde_json::Value;
    use time::{Date, Month, OffsetDateTime};

    #[test]
    fn daily_file_name_is_utc_date_based() {
        let date = Date::from_calendar_date(2026, Month::August, 9).unwrap();
        assert_eq!(log_file_name(date), "lux.2026-08-09.log");
    }

    #[test]
    fn export_range_rejects_more_than_31_days() {
        let from = Date::from_calendar_date(2026, Month::January, 1).unwrap();
        let to = Date::from_calendar_date(2026, Month::February, 1).unwrap();
        assert!(matches!(
            LogDateRange::new(from, to),
            Err(LogExportError::DateRangeTooLarge)
        ));
    }

    #[test]
    fn dates_require_exact_calendar_format() {
        assert!(parse_date("2026-8-09").is_err());
        assert!(parse_date("2026-02-30").is_err());
        assert!(parse_date("2026-02-09").is_ok());
    }

    #[test]
    fn log_stores_for_the_same_config_directory_share_a_writer() {
        let temp_dir = tempfile::tempdir().unwrap();
        let first = LogStore::new(temp_dir.path());
        let second = LogStore::new(temp_dir.path());
        assert!(Arc::ptr_eq(&first.manager, &second.manager));
    }

    #[test]
    fn size_rotation_keeps_only_the_newest_archives() -> io::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let date = Date::from_calendar_date(2026, Month::August, 9).unwrap();
        let mut manager = LogManager::open_with_limits(temp_dir.path(), date, 3, 2)?;
        for _ in 0..3 {
            manager.write_line_for_date(date, b"x\n\n")?;
        }

        let archive_dir = archive_dir(temp_dir.path());
        let mut archives = fs::read_dir(archive_dir)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        archives.sort();
        assert_eq!(archives.len(), 2);
        assert_eq!(
            archives[0].to_string_lossy(),
            "lux.2026-08-09.part-000002.log.zip"
        );
        assert_eq!(
            archives[1].to_string_lossy(),
            "lux.2026-08-09.part-000003.log.zip"
        );
        assert!(temp_dir.path().join("logs/lux.2026-08-09.log").exists());
        Ok(())
    }

    #[test]
    fn date_rotation_archives_the_previous_day() -> io::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let yesterday = Date::from_calendar_date(2026, Month::August, 8).unwrap();
        let today = Date::from_calendar_date(2026, Month::August, 9).unwrap();
        let mut manager = LogManager::open_with_limits(temp_dir.path(), yesterday, 100, 20)?;
        manager.write_line_for_date(yesterday, b"previous day\n")?;
        manager.write_line_for_date(today, b"current day\n")?;

        let archive_path = archive_dir(temp_dir.path()).join("lux.2026-08-08.part-000001.log.zip");
        verify_archive(&archive_path, "lux.2026-08-08.part-000001.log")?;
        assert_eq!(
            fs::read(temp_dir.path().join("logs/lux.2026-08-09.log"))?,
            b"current day\n"
        );
        Ok(())
    }

    #[test]
    fn failed_archive_keeps_the_original_log_segment() -> io::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let date = Date::from_calendar_date(2026, Month::August, 9).unwrap();
        let mut manager = LogManager::open_with_limits(temp_dir.path(), date, 3, 20)?;
        let archive_directory = archive_dir(temp_dir.path());
        fs::remove_dir(&archive_directory)?;
        fs::write(&archive_directory, b"block archive writes")?;

        assert!(manager.write_line_for_date(date, b"x\n\n").is_err());
        assert_eq!(
            fs::read(temp_dir.path().join("logs/lux.2026-08-09.log"))?,
            b"x\n\n"
        );
        Ok(())
    }

    #[test]
    fn scan_job_event_query_reads_archived_jsonl_segments() -> io::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let date = Date::from_calendar_date(2026, Month::August, 9).unwrap();
        let mut manager = LogManager::open_with_limits(temp_dir.path(), date, 1, 20)?;
        for (id, level, code) in [
            ("event-1", "INFO", "JOB_STARTED"),
            ("event-2", "ERROR", "SCAN_IO"),
        ] {
            let record = serde_json::json!({
                "recordType": "scan_job_event",
                "id": id,
                "jobId": "job-1",
                "level": level,
                "eventCode": code,
                "message": "event",
                "details": {"attempt": id},
                "createdAt": if id == "event-1" { 1 } else { 2 },
            });
            let mut line = serde_json::to_vec(&record).map_err(io::Error::other)?;
            line.push(b'\n');
            manager.write_line_for_date(date, &line)?;
        }

        let (total, events) = read_scan_job_events(
            &log_dir(temp_dir.path()),
            &archive_dir(temp_dir.path()),
            "job-1",
            Some("ERROR"),
            Some("SCAN_IO"),
            0,
            10,
        )?;
        assert_eq!(total, 1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "event-2");
        assert_eq!(events[0].created_at, 2);
        Ok(())
    }

    #[tokio::test]
    async fn audit_events_are_file_backed_and_redacted() -> io::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let store = LogStore::open(temp_dir.path()).await?;
        store
            .append_audit_event(NewAuditLogEvent {
                id: "audit-1",
                actor_user_id: Some("user-1"),
                actor_username: Some("admin"),
                event_type: "SETTINGS_UPDATED",
                target_type: Some("settings"),
                target_id: None,
                metadata_json: r#"{"apiKey":"must-not-be-saved","remoteIp":"192.0.2.4"}"#,
            })
            .await?;

        let (total, events) = store.list_audit_events(0, 10).await?;
        assert_eq!(total, 1);
        assert_eq!(events[0].actor_username.as_deref(), Some("admin"));
        assert_eq!(events[0].event_type, "SETTINGS_UPDATED");
        assert_eq!(
            serde_json::from_str::<Value>(&events[0].metadata_json).map_err(io::Error::other)?["apiKey"],
            "[REDACTED]"
        );
        let log_file =
            log_dir(temp_dir.path()).join(log_file_name(OffsetDateTime::now_utc().date()));
        assert!(!fs::read_to_string(log_file)?.contains("must-not-be-saved"));
        Ok(())
    }

    #[tokio::test]
    async fn scan_job_event_persistence_verification_rejects_missing_ids()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let store = LogStore::open(temp_dir.path()).await?;
        store
            .append_scan_job_event(
                "persisted-event",
                "job-1",
                "INFO",
                "JOB_CREATED",
                "created",
                "{}",
            )
            .await?;

        assert!(
            store
                .verify_scan_job_event_ids(&[
                    "persisted-event".to_owned(),
                    "missing-event".to_owned(),
                ])
                .await
                .is_err()
        );
        store
            .verify_scan_job_event_ids(&["persisted-event".to_owned()])
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn migration_batch_can_verify_a_segment_before_fifo_pruning()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let future_date = Date::from_calendar_date(2030, Month::January, 1)?;
        let old_date = Date::from_calendar_date(2020, Month::January, 1)?;
        let archive_directory = archive_dir(temp_dir.path());
        fs::create_dir_all(&archive_directory)?;
        let future_member = format!("lux.{}.part-000001.log", "2030-01-01");
        let future_archive = archive_directory.join(format!("{future_member}.zip"));
        let archive_file = fs::File::create(future_archive)?;
        let mut archive = zip::ZipWriter::new(archive_file);
        archive.start_file(
            &future_member,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )?;
        archive.write_all(b"{\"recordType\":\"program_log\"}\n")?;
        archive.finish()?;

        let store = LogStore::new_with_limits_for_test(temp_dir.path(), future_date, 1, 1)?;
        let expected_scan_ids = ["oldest-scan-event".to_owned()];
        let expected_audit_ids = ["oldest-audit-event".to_owned()];
        store
            .append_json_batch_and_sync(vec![serde_json::json!({
                "recordType": "scan_job_event",
                "id": "oldest-scan-event",
                "jobId": "job-1",
                "level": "WARN",
                "eventCode": "SCAN_IO",
                "message": "legacy event",
                "details": {},
                "timestamp": "2020-01-01T00:00:00Z",
                "createdAt": old_date.midnight().assume_utc().unix_timestamp(),
            })])
            .await?;
        store.verify_scan_job_event_ids(&expected_scan_ids).await?;
        store
            .append_json_batch_and_sync(vec![serde_json::json!({
                "recordType": "admin_audit_event",
                "id": "oldest-audit-event",
                "eventType": "USER_CREATED",
                "metadata": {},
                "timestamp": "2020-01-01T00:00:00Z",
                "createdAt": old_date.midnight().assume_utc().unix_timestamp(),
            })])
            .await?;
        store.verify_audit_event_ids(&expected_audit_ids).await?;
        let archives_before_prune = fs::read_dir(archive_directory.clone())?.count();
        assert_eq!(archives_before_prune, 3);

        store.prune_archives().await?;
        let remaining_scan_ids = store
            .existing_scan_job_event_ids(&expected_scan_ids)
            .await?;
        let remaining_audit_ids = store.existing_audit_event_ids(&expected_audit_ids).await?;
        assert!(remaining_scan_ids.is_empty());
        assert!(remaining_audit_ids.is_empty());
        assert_eq!(fs::read_dir(archive_directory)?.count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn reopen_discards_an_incomplete_jsonl_tail_before_appending()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let current_log =
            log_dir(temp_dir.path()).join(log_file_name(OffsetDateTime::now_utc().date()));
        fs::create_dir_all(log_dir(temp_dir.path()))?;
        fs::write(
            &current_log,
            b"{\"recordType\":\"scan_job_event\",\"id\":\"complete\",\"jobId\":\"job-1\",\"level\":\"WARN\",\"eventCode\":\"SCAN_IO\",\"message\":\"complete\",\"details\":{},\"createdAt\":1}\n{\"recordType\":\"scan_job_event\",\"id\":\"torn",
        )?;
        let store = LogStore::open(temp_dir.path()).await?;
        store
            .append_scan_job_event("next", "job-1", "INFO", "JOB_STARTED", "started", "{}")
            .await?;
        fs::OpenOptions::new()
            .append(true)
            .open(&current_log)?
            .write_all(b"{\"torn-in-process\"")?;
        store
            .append_scan_job_event(
                "after-torn-tail",
                "job-1",
                "INFO",
                "JOB_STARTED",
                "recovered",
                "{}",
            )
            .await?;

        let (total, events) = store
            .list_scan_job_events("job-1", None, None, 0, 10)
            .await?;
        assert_eq!(total, 3);
        let mut ids = events
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, ["after-torn-tail", "complete", "next"]);
        for line in fs::read(current_log)?
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            serde_json::from_slice::<Value>(line)?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn jsonl_batch_writer_is_durable_and_retries_after_a_write_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path().join("config");
        fs::create_dir_all(&config_dir)?;
        let blocked_log_dir = log_dir(&config_dir);
        fs::write(&blocked_log_dir, b"block directory creation")?;
        let store = LogStore::new(&config_dir);
        let now = OffsetDateTime::now_utc();
        let records = vec![
            serde_json::json!({"recordType":"migration-test","id":"one","createdAt":now.unix_timestamp()}),
            serde_json::json!({"recordType":"migration-test","id":"two","createdAt":now.unix_timestamp()}),
        ];
        assert!(
            store
                .append_json_batch_and_sync(records.clone())
                .await
                .is_err()
        );

        fs::remove_file(blocked_log_dir)?;
        store.append_json_batch_and_sync(records).await?;
        let date = now.date();
        let LogExport::Daily { contents, .. } =
            export_logs(&config_dir, LogDateRange::new(date, date)?).await?
        else {
            return Err("single-day export should be raw JSONL".into());
        };
        let ids = contents
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice::<Value>)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|record| record["id"].as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["one", "two"]);
        Ok(())
    }

    #[test]
    fn audit_event_query_reads_archived_jsonl_segments() -> io::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let date = Date::from_calendar_date(2026, Month::August, 9).unwrap();
        let mut manager = LogManager::open_with_limits(temp_dir.path(), date, 1, 20)?;
        let record = serde_json::json!({
            "recordType": "admin_audit_event",
            "id": "audit-archived",
            "actorUserId": "user-1",
            "actorUsername": "admin",
            "eventType": "SETTINGS_UPDATED",
            "targetType": "settings",
            "targetId": null,
            "metadata": {"auth": "admin_api_key"},
            "createdAt": 2,
        });
        let mut line = serde_json::to_vec(&record).map_err(io::Error::other)?;
        line.push(b'\n');
        manager.write_line_for_date(date, &line)?;

        let (total, events) = read_audit_events(
            &log_dir(temp_dir.path()),
            &archive_dir(temp_dir.path()),
            0,
            10,
            None,
        )?;
        assert_eq!(total, 1);
        assert_eq!(events[0].id, "audit-archived");
        assert_eq!(events[0].actor_username.as_deref(), Some("admin"));
        assert_eq!(events[0].event_type, "SETTINGS_UPDATED");
        Ok(())
    }

    // ---------------------------------------------------------------------------------------
    // Bounded event reads: the page readers must stay proportional to the requested page, so the
    // fixtures below compare them against a naive "read every file, sort, then paginate" reference.
    // ---------------------------------------------------------------------------------------

    fn fixture_date(value: &str) -> Date {
        parse_date(value).expect("fixture date")
    }

    fn day_start(date: Date) -> i64 {
        date.midnight().assume_utc().unix_timestamp()
    }

    fn audit_fixture_line(id: &str, event_type: &str, created_at: i64) -> String {
        serde_json::json!({
            "recordType": "admin_audit_event",
            "id": id,
            "actorUserId": "user-1",
            "actorUsername": "admin",
            "eventType": event_type,
            "targetType": "settings",
            "targetId": null,
            "metadata": {"event": id},
            "createdAt": created_at,
        })
        .to_string()
    }

    fn scan_job_fixture_line(id: &str, created_at: i64) -> String {
        serde_json::json!({
            "recordType": "scan_job_event",
            "id": id,
            "jobId": "job-1",
            "level": "WARN",
            "eventCode": "SCAN_IO",
            "message": format!("event {id}"),
            "details": {"event": id},
            "createdAt": created_at,
        })
        .to_string()
    }

    fn write_daily_segment(config_dir: &Path, date: Date, lines: &[String]) -> io::Result<()> {
        let directory = log_dir(config_dir);
        fs::create_dir_all(&directory)?;
        let mut payload = String::new();
        for line in lines {
            payload.push_str(line);
            payload.push('\n');
        }
        fs::write(directory.join(log_file_name(date)), payload)
    }

    fn write_log_archive(
        config_dir: &Path,
        date: Date,
        sequence: u64,
        lines: &[String],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = archive_dir(config_dir);
        fs::create_dir_all(&directory)?;
        let member = log_segment_file_name(date, sequence);
        let file = fs::File::create(directory.join(format!("{member}.zip")))?;
        let mut writer = zip::ZipWriter::new(file);
        writer.start_file(
            &member,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )?;
        let mut payload = String::new();
        for line in lines {
            payload.push_str(line);
            payload.push('\n');
        }
        writer.write_all(payload.as_bytes())?;
        writer.finish()?;
        Ok(())
    }

    fn collect_reference_json_lines(payload: &[u8], lines: &mut Vec<Vec<u8>>) {
        for line in payload.split(|byte| *byte == b'\n') {
            if !line.is_empty() {
                lines.push(line.to_vec());
            }
        }
    }

    /// Naive reference reader: every daily file and every archive member, fully loaded.
    fn reference_json_lines(config_dir: &Path) -> io::Result<Vec<Vec<u8>>> {
        let mut lines = Vec::new();
        let mut daily = Vec::new();
        if let Ok(entries) = fs::read_dir(log_dir(config_dir)) {
            for entry in entries {
                let path = entry?.path();
                let is_daily = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("lux.") && name.ends_with(".log"));
                if is_daily {
                    daily.push(path);
                }
            }
        }
        for path in daily {
            collect_reference_json_lines(&fs::read(path)?, &mut lines);
        }
        if let Ok(entries) = fs::read_dir(archive_dir(config_dir)) {
            for entry in entries {
                let path = entry?.path();
                let is_archive = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".log.zip"));
                if !is_archive {
                    continue;
                }
                let mut archive =
                    zip::ZipArchive::new(fs::File::open(&path)?).map_err(io::Error::other)?;
                for index in 0..archive.len() {
                    let mut member = archive.by_index(index).map_err(io::Error::other)?;
                    if !member.name().ends_with(".log") {
                        continue;
                    }
                    let mut payload = Vec::new();
                    member.read_to_end(&mut payload)?;
                    collect_reference_json_lines(&payload, &mut lines);
                }
            }
        }
        Ok(lines)
    }

    fn reference_audit_events(config_dir: &Path) -> io::Result<Vec<AuditLogEvent>> {
        let mut events = Vec::new();
        for line in reference_json_lines(config_dir)? {
            let Ok(record) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            if record["recordType"] != "admin_audit_event" {
                continue;
            }
            let (Some(id), Some(event_type), Some(created_at)) = (
                record["id"].as_str(),
                record["eventType"].as_str(),
                record["createdAt"].as_i64(),
            ) else {
                continue;
            };
            events.push(AuditLogEvent {
                id: id.to_owned(),
                actor_user_id: record["actorUserId"].as_str().map(str::to_owned),
                actor_username: record["actorUsername"].as_str().map(str::to_owned),
                event_type: event_type.to_owned(),
                target_type: record["targetType"].as_str().map(str::to_owned),
                target_id: record["targetId"].as_str().map(str::to_owned),
                metadata_json: String::new(),
                created_at,
            });
        }
        sort_audit_events(&mut events);
        let mut seen_ids = HashSet::new();
        events.retain(|event| seen_ids.insert(event.id.clone()));
        Ok(events)
    }

    fn reference_audit_ids(
        config_dir: &Path,
        offset: usize,
        limit: usize,
    ) -> io::Result<Vec<String>> {
        Ok(reference_audit_events(config_dir)?
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|event| event.id)
            .collect())
    }

    fn reference_scan_job_events(config_dir: &Path) -> io::Result<Vec<ScanJobLogEvent>> {
        let mut events = Vec::new();
        for line in reference_json_lines(config_dir)? {
            let Ok(record) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            if record["recordType"] != "scan_job_event" || record["jobId"] != "job-1" {
                continue;
            }
            let (Some(id), Some(created_at)) =
                (record["id"].as_str(), record["createdAt"].as_i64())
            else {
                continue;
            };
            events.push(ScanJobLogEvent {
                id: id.to_owned(),
                job_id: "job-1".to_owned(),
                level: "WARN".to_owned(),
                event_code: "SCAN_IO".to_owned(),
                message: String::new(),
                details_json: String::new(),
                created_at,
            });
        }
        events.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        let mut seen_ids = HashSet::new();
        events.retain(|event| seen_ids.insert(event.id.clone()));
        Ok(events)
    }

    fn reference_activity_ids(config_dir: &Path, limit: i64) -> io::Result<Vec<String>> {
        let category_limit = usize::try_from((limit / 2).max(1)).unwrap_or(usize::MAX);
        let mut login_events = Vec::new();
        let mut playback_events = Vec::new();
        for event in reference_audit_events(config_dir)? {
            match event.event_type.as_str() {
                "AUTH_LOGIN" => login_events.push(event),
                "PLAYBACK_STARTED" | "PLAYBACK_PAUSED" | "PLAYBACK_STOPPED" => {
                    playback_events.push(event);
                }
                _ => {}
            }
        }
        login_events.truncate(category_limit);
        playback_events.truncate(category_limit);
        login_events.extend(playback_events);
        sort_audit_events(&mut login_events);
        let mut seen_ids = HashSet::new();
        login_events.retain(|event| seen_ids.insert(event.id.clone()));
        login_events.truncate(usize::try_from(limit.max(0)).unwrap_or(usize::MAX));
        Ok(login_events.into_iter().map(|event| event.id).collect())
    }

    fn event_ids<T>(events: &[T], id: impl Fn(&T) -> &str) -> Vec<String> {
        events.iter().map(|event| id(event).to_owned()).collect()
    }

    #[test]
    fn audit_pages_match_a_full_scan_across_days_and_archives()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path();
        let first = fixture_date("2026-08-06");
        let second = fixture_date("2026-08-07");
        let third = fixture_date("2026-08-08");
        let fourth = fixture_date("2026-08-09");
        write_log_archive(
            config_dir,
            first,
            1,
            &[
                audit_fixture_line("a1-0", "SETTINGS_UPDATED", day_start(first) + 60),
                audit_fixture_line("a1-1", "USER_CREATED", day_start(first) + 120),
                audit_fixture_line("a1-2", "SETTINGS_UPDATED", day_start(first) + 180),
            ],
        )?;
        write_log_archive(
            config_dir,
            first,
            2,
            &[
                audit_fixture_line("a2-0", "SETTINGS_UPDATED", day_start(first) + 240),
                audit_fixture_line("a2-1", "SETTINGS_UPDATED", day_start(first) + 300),
            ],
        )?;
        write_daily_segment(
            config_dir,
            second,
            &[
                audit_fixture_line("b-0", "SETTINGS_UPDATED", day_start(second) + 60),
                audit_fixture_line("b-1", "SETTINGS_UPDATED", day_start(second) + 120),
                audit_fixture_line("b-2", "USER_CREATED", day_start(second) + 180),
            ],
        )?;
        write_log_archive(
            config_dir,
            third,
            2,
            &[
                audit_fixture_line("c2-0", "SETTINGS_UPDATED", day_start(third) + 60),
                audit_fixture_line("c-shared", "SETTINGS_UPDATED", day_start(third) + 120),
            ],
        )?;
        // The same record written into the daily file and its archived segment: id dedupe must
        // collapse it exactly once.
        write_daily_segment(
            config_dir,
            third,
            &[
                audit_fixture_line("c-shared", "SETTINGS_UPDATED", day_start(third) + 120),
                audit_fixture_line("c-1", "SETTINGS_UPDATED", day_start(third) + 180),
            ],
        )?;
        write_daily_segment(
            config_dir,
            fourth,
            &[
                audit_fixture_line("d-0", "SETTINGS_UPDATED", day_start(fourth) + 60),
                audit_fixture_line("d-1", "SETTINGS_UPDATED", day_start(fourth) + 120),
            ],
        )?;

        for (offset, limit) in [(0_usize, 1_i64), (0, 30), (1, 2), (4, 3), (12, 5), (40, 5)] {
            let (_, page) = read_audit_events(
                &log_dir(config_dir),
                &archive_dir(config_dir),
                i64::try_from(offset).unwrap_or(i64::MAX),
                limit,
                None,
            )?;
            assert_eq!(
                event_ids(&page, |event| event.id.as_str()),
                reference_audit_ids(config_dir, offset, usize::try_from(limit).unwrap_or(0))?,
                "offset={offset} limit={limit}"
            );
        }
        Ok(())
    }

    #[test]
    fn scan_job_pages_match_a_full_scan_across_days_and_archives()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path();
        let first = fixture_date("2026-08-07");
        let second = fixture_date("2026-08-09");
        write_log_archive(
            config_dir,
            first,
            1,
            &[
                scan_job_fixture_line("job-a", day_start(first) + 60),
                scan_job_fixture_line("job-b", day_start(first) + 120),
            ],
        )?;
        write_daily_segment(
            config_dir,
            second,
            &[
                scan_job_fixture_line("job-c", day_start(second) + 60),
                scan_job_fixture_line("job-d", day_start(second) + 120),
                scan_job_fixture_line("job-e", day_start(second) + 180),
            ],
        )?;

        let reference = reference_scan_job_events(config_dir)?;
        for (offset, limit) in [(0_usize, 1_i64), (0, 10), (1, 2), (3, 5)] {
            let (total, page) = read_scan_job_events(
                &log_dir(config_dir),
                &archive_dir(config_dir),
                "job-1",
                None,
                None,
                i64::try_from(offset).unwrap_or(i64::MAX),
                limit,
            )?;
            assert_eq!(
                usize::try_from(total).unwrap_or(usize::MAX),
                reference.len(),
                "the job-event total must stay exact"
            );
            let expected = reference
                .iter()
                .skip(offset)
                .take(usize::try_from(limit).unwrap_or(0))
                .map(|event| event.id.clone())
                .collect::<Vec<_>>();
            assert_eq!(
                event_ids(&page, |event| event.id.as_str()),
                expected,
                "offset={offset} limit={limit}"
            );
        }
        Ok(())
    }

    #[test]
    fn first_page_skips_older_archives_without_opening_them()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path();
        let newest = fixture_date("2026-08-09");
        write_daily_segment(
            config_dir,
            newest,
            &[
                audit_fixture_line("newest-audit", "SETTINGS_UPDATED", day_start(newest) + 10),
                audit_fixture_line("login-1", "AUTH_LOGIN", day_start(newest) + 1),
                audit_fixture_line("login-2", "AUTH_LOGIN", day_start(newest) + 2),
                audit_fixture_line("playback-1", "PLAYBACK_STARTED", day_start(newest) + 3),
                audit_fixture_line("playback-2", "PLAYBACK_STOPPED", day_start(newest) + 4),
            ],
        )?;
        // A first page that only needs the newest day must never open this broken archive.
        fs::create_dir_all(archive_dir(config_dir))?;
        fs::write(
            archive_dir(config_dir).join("lux.2026-08-08.part-000001.log.zip"),
            b"not a zip archive",
        )?;

        let (_, page) =
            read_audit_events(&log_dir(config_dir), &archive_dir(config_dir), 0, 1, None)?;
        assert_eq!(
            event_ids(&page, |event| event.id.as_str()),
            ["newest-audit".to_owned()]
        );

        let activity = read_activity_events(&log_dir(config_dir), &archive_dir(config_dir), 4)?;
        assert_eq!(activity.len(), 4);

        // The job-event page keeps an exact `total`, so it still has to open every file and the
        // broken archive is reported instead of silently skipped.
        assert!(
            read_scan_job_events(
                &log_dir(config_dir),
                &archive_dir(config_dir),
                "job-1",
                None,
                None,
                0,
                10,
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn legacy_timestamps_in_a_newer_file_keep_the_global_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path();
        let newest = fixture_date("2026-08-09");
        let previous = fixture_date("2026-08-08");
        // Migrated/legacy records can be appended to a newer file while keeping their original
        // `createdAt`; the file name must not be trusted over the record timestamp.
        write_daily_segment(
            config_dir,
            newest,
            &[audit_fixture_line(
                "legacy",
                "SETTINGS_UPDATED",
                day_start(fixture_date("2026-08-01")),
            )],
        )?;
        write_daily_segment(
            config_dir,
            previous,
            &[audit_fixture_line(
                "recent",
                "SETTINGS_UPDATED",
                day_start(newest) - 60,
            )],
        )?;

        let (_, first_page) =
            read_audit_events(&log_dir(config_dir), &archive_dir(config_dir), 0, 1, None)?;
        assert_eq!(
            event_ids(&first_page, |event| event.id.as_str()),
            ["recent".to_owned()],
            "an older file may still hold the newest record"
        );
        let (_, both) =
            read_audit_events(&log_dir(config_dir), &archive_dir(config_dir), 0, 5, None)?;
        assert_eq!(
            event_ids(&both, |event| event.id.as_str()),
            ["recent".to_owned(), "legacy".to_owned()]
        );
        assert_eq!(
            event_ids(&both, |event| event.id.as_str()),
            reference_audit_ids(config_dir, 0, 5)?
        );
        Ok(())
    }

    #[test]
    fn activity_quota_keeps_logins_and_playback_balanced() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path();
        let newest = fixture_date("2026-08-09");
        let previous = fixture_date("2026-08-08");
        write_daily_segment(
            config_dir,
            newest,
            &[
                audit_fixture_line("login-a", "AUTH_LOGIN", day_start(newest) + 10),
                audit_fixture_line("login-b", "AUTH_LOGIN", day_start(newest) + 20),
                audit_fixture_line("login-c", "AUTH_LOGIN", day_start(newest) + 30),
                audit_fixture_line("login-d", "AUTH_LOGIN", day_start(newest) + 40),
                audit_fixture_line("login-e", "AUTH_LOGIN", day_start(newest) + 50),
                audit_fixture_line("playback-a", "PLAYBACK_STARTED", day_start(newest) + 60),
            ],
        )?;
        write_log_archive(
            config_dir,
            previous,
            1,
            &[
                audit_fixture_line("playback-b", "PLAYBACK_STARTED", day_start(previous) + 10),
                audit_fixture_line("playback-c", "PLAYBACK_PAUSED", day_start(previous) + 20),
                audit_fixture_line("playback-d", "PLAYBACK_STOPPED", day_start(previous) + 30),
                audit_fixture_line("playback-e", "PLAYBACK_STARTED", day_start(previous) + 40),
                audit_fixture_line("playback-f", "PLAYBACK_STOPPED", day_start(previous) + 50),
                audit_fixture_line("login-f", "AUTH_LOGIN", day_start(previous) + 60),
            ],
        )?;

        for limit in [2_i64, 4, 5, 10] {
            let events =
                read_activity_events(&log_dir(config_dir), &archive_dir(config_dir), limit)?;
            assert_eq!(
                event_ids(&events, |event| event.id.as_str()),
                reference_activity_ids(config_dir, limit)?,
                "limit={limit}"
            );
            let logins = events
                .iter()
                .filter(|event| event.event_type == "AUTH_LOGIN")
                .count();
            let category_limit = usize::try_from((limit / 2).max(1)).unwrap_or(usize::MAX);
            assert!(logins <= category_limit, "limit={limit} logins={logins}");
            assert!(events.len() <= usize::try_from(limit.max(0)).unwrap_or(usize::MAX));
        }
        Ok(())
    }

    #[test]
    fn oversized_newest_segment_stops_at_the_byte_ceiling() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path();
        let newest = fixture_date("2026-08-09");
        let previous = fixture_date("2026-08-08");
        let padding = "p".repeat(48 * 1024);
        let noise_lines = usize::try_from(MAX_EVENT_SCAN_BYTES / (48 * 1024)).unwrap_or(0) + 32;
        let mut payload = Vec::with_capacity(noise_lines);
        for index in 0..noise_lines {
            payload.push(
                serde_json::json!({"recordType": "program_log", "message": format!("{index}-{padding}")})
                    .to_string(),
            );
        }
        // The newest records sit at the end of the oversized segment: the tail must be read first.
        payload.push(audit_fixture_line(
            "tail-1",
            "SETTINGS_UPDATED",
            day_start(newest) + 60,
        ));
        payload.push(audit_fixture_line(
            "tail-2",
            "SETTINGS_UPDATED",
            day_start(newest) + 120,
        ));
        write_daily_segment(config_dir, newest, &payload)?;
        write_daily_segment(
            config_dir,
            previous,
            &[audit_fixture_line(
                "unreachable",
                "SETTINGS_UPDATED",
                day_start(previous),
            )],
        )?;

        let (_, page) =
            read_audit_events(&log_dir(config_dir), &archive_dir(config_dir), 0, 10, None)?;
        assert_eq!(
            event_ids(&page, |event| event.id.as_str()),
            ["tail-2".to_owned(), "tail-1".to_owned()],
            "the byte ceiling must return the newest records and stop before older segments"
        );
        Ok(())
    }

    #[test]
    fn oversized_archive_member_returns_its_newest_records()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path();
        let newest = fixture_date("2026-08-09");
        let previous = fixture_date("2026-08-08");
        // An archived segment is a rotated daily file: its oldest records come first.
        let padding = "p".repeat(48 * 1024);
        let noise_lines = usize::try_from(MAX_EVENT_SCAN_BYTES / (48 * 1024)).unwrap_or(0) + 32;
        let mut payload = Vec::with_capacity(noise_lines + 3);
        payload.push(audit_fixture_line(
            "head-oldest",
            "SETTINGS_UPDATED",
            day_start(newest),
        ));
        for index in 0..noise_lines {
            payload.push(
                serde_json::json!({"recordType": "program_log", "message": format!("{index}-{padding}")})
                    .to_string(),
            );
        }
        payload.push(audit_fixture_line(
            "tail-1",
            "SETTINGS_UPDATED",
            day_start(newest) + 60,
        ));
        payload.push(audit_fixture_line(
            "tail-2",
            "SETTINGS_UPDATED",
            day_start(newest) + 120,
        ));
        write_log_archive(config_dir, newest, 1, &payload)?;
        // The oversized member is the newest source: older archives must stay unopened.
        fs::create_dir_all(archive_dir(config_dir))?;
        fs::write(
            archive_dir(config_dir).join(format!("{}.zip", log_segment_file_name(previous, 1))),
            b"not a zip archive",
        )?;

        let (_, page) =
            read_audit_events(&log_dir(config_dir), &archive_dir(config_dir), 0, 10, None)?;
        assert_eq!(
            event_ids(&page, |event| event.id.as_str()),
            ["tail-2".to_owned(), "tail-1".to_owned()],
            "an over-budget archive member must be read from its newest end"
        );
        assert_eq!(page.len(), 2, "the skipped head must not leak its records");
        Ok(())
    }

    #[test]
    fn legacy_timestamp_with_offset_returns_the_globally_ordered_row()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path();
        let newest = fixture_date("2026-08-09");
        let previous = fixture_date("2026-08-08");
        // `legacy` keeps an old `createdAt` inside the newer file (a migrated record), so the day
        // order cannot be trusted before both files are read.
        write_daily_segment(
            config_dir,
            newest,
            &[audit_fixture_line(
                "legacy",
                "SETTINGS_UPDATED",
                day_start(fixture_date("2026-08-01")),
            )],
        )?;
        write_daily_segment(
            config_dir,
            previous,
            &[audit_fixture_line(
                "recent",
                "SETTINGS_UPDATED",
                day_start(newest) - 60,
            )],
        )?;

        let (_, page) =
            read_audit_events(&log_dir(config_dir), &archive_dir(config_dir), 1, 1, None)?;
        assert_eq!(
            event_ids(&page, |event| event.id.as_str()),
            ["legacy".to_owned()],
            "the second row of the globally ordered log is the migrated record"
        );
        Ok(())
    }

    #[test]
    fn legacy_timestamp_pages_match_a_full_scan_for_every_offset()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path();
        let newest = fixture_date("2026-08-09");
        let middle = fixture_date("2026-08-08");
        let oldest = fixture_date("2026-08-07");
        write_daily_segment(
            config_dir,
            newest,
            &[
                audit_fixture_line("newest-a", "SETTINGS_UPDATED", day_start(newest) + 60),
                audit_fixture_line("legacy-a", "SETTINGS_UPDATED", day_start(oldest) + 90),
                audit_fixture_line("legacy-b", "SETTINGS_UPDATED", day_start(middle) + 30),
            ],
        )?;
        write_log_archive(
            config_dir,
            middle,
            1,
            &[
                audit_fixture_line("middle-a", "SETTINGS_UPDATED", day_start(middle) + 60),
                audit_fixture_line("legacy-c", "SETTINGS_UPDATED", day_start(oldest) + 30),
            ],
        )?;
        write_daily_segment(
            config_dir,
            oldest,
            &[
                audit_fixture_line("oldest-a", "SETTINGS_UPDATED", day_start(oldest) + 60),
                audit_fixture_line("oldest-b", "SETTINGS_UPDATED", day_start(oldest) + 120),
            ],
        )?;

        for offset in [0_usize, 1, 2, 3] {
            for limit in [1_i64, 2, 5] {
                let (_, page) = read_audit_events(
                    &log_dir(config_dir),
                    &archive_dir(config_dir),
                    i64::try_from(offset).unwrap_or(i64::MAX),
                    limit,
                    None,
                )?;
                assert_eq!(
                    event_ids(&page, |event| event.id.as_str()),
                    reference_audit_ids(config_dir, offset, usize::try_from(limit).unwrap_or(0))?,
                    "offset={offset} limit={limit}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn retention_ceilings_cover_the_full_retained_log_volume() {
        // Every retained archive slot plus the daily files still being written must fit, otherwise
        // older job events would silently disappear from the admin page.
        const {
            assert!(MAX_JOB_EVENT_SCAN_BYTES >= LOG_SEGMENT_BYTES * (MAX_LOG_ARCHIVES as u64 + 1));
            assert!(MAX_EVENT_SCAN_FILES > MAX_LOG_ARCHIVES);
            assert!(MAX_EVENT_SCAN_BYTES < MAX_JOB_EVENT_SCAN_BYTES);
            // The window must at least cover a full console page, and stay small enough to stay
            // bounded.
            assert!(MAX_LOG_PAGE_WINDOW >= 100);
            assert!(MAX_LOG_PAGE_WINDOW <= 10_000);
        }
    }

    #[test]
    fn deep_pages_are_refused_without_opening_log_files() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path();
        fs::create_dir_all(archive_dir(config_dir))?;
        fs::write(
            archive_dir(config_dir).join("lux.2026-08-08.part-000001.log.zip"),
            b"not a zip archive",
        )?;
        let deep_offset = i64::try_from(MAX_LOG_PAGE_WINDOW).unwrap_or(i64::MAX);

        let (total, page) = read_audit_events(
            &log_dir(config_dir),
            &archive_dir(config_dir),
            deep_offset,
            1,
            None,
        )?;
        assert_eq!(total, 0);
        assert!(page.is_empty());
        assert!(
            read_activity_events(
                &log_dir(config_dir),
                &archive_dir(config_dir),
                deep_offset.saturating_add(1),
            )?
            .is_empty()
        );
        let (total, events) = read_scan_job_events(
            &log_dir(config_dir),
            &archive_dir(config_dir),
            "job-1",
            None,
            None,
            deep_offset,
            1,
        )?;
        assert_eq!(total, 0);
        assert!(events.is_empty());
        Ok(())
    }

    #[test]
    fn wide_page_windows_match_a_full_scan() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path();
        let first = fixture_date("2026-08-06");
        let second = fixture_date("2026-08-07");
        let third = fixture_date("2026-08-08");
        let mut first_day = Vec::with_capacity(2_000);
        let mut second_day = Vec::with_capacity(2_000);
        let mut third_day = Vec::with_capacity(2_000);
        for index in 0..2_000 {
            first_day.push(audit_fixture_line(
                &format!("first-{index:05}"),
                "SETTINGS_UPDATED",
                day_start(first) + index,
            ));
            second_day.push(audit_fixture_line(
                &format!("second-{index:05}"),
                "SETTINGS_UPDATED",
                day_start(second) + index,
            ));
            third_day.push(audit_fixture_line(
                &format!("third-{index:05}"),
                "SETTINGS_UPDATED",
                day_start(third) + index,
            ));
        }
        write_log_archive(config_dir, first, 1, &first_day)?;
        write_daily_segment(config_dir, second, &second_day)?;
        write_daily_segment(config_dir, third, &third_day)?;

        for (offset, limit) in [(5_000_usize, 100_i64), (0, 100), (1_500, 100), (5_950, 100)] {
            let (_, page) = read_audit_events(
                &log_dir(config_dir),
                &archive_dir(config_dir),
                i64::try_from(offset).unwrap_or(i64::MAX),
                limit,
                None,
            )?;
            assert_eq!(
                event_ids(&page, |event| event.id.as_str()),
                reference_audit_ids(config_dir, offset, usize::try_from(limit).unwrap_or(0))?,
                "offset={offset} limit={limit}"
            );
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod permission_tests {
    use super::LogManager;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn log_files_and_directories_are_owner_only() {
        let temp = tempfile::tempdir().expect("temp dir");
        let manager = LogManager::open(temp.path()).expect("open log manager");
        let log_dir = temp.path().join("logs");
        let mode = std::fs::metadata(&log_dir)
            .expect("log dir")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "log directory must not be world readable");
        let file_mode = std::fs::metadata(&manager.active_path)
            .expect("active log file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600, "log files must be owner only");
    }

    #[test]
    fn legacy_world_readable_logs_are_tightened_on_open() {
        let temp = tempfile::tempdir().expect("temp dir");
        let log_dir = temp.path().join("logs");
        std::fs::create_dir_all(&log_dir).expect("log dir");
        std::fs::set_permissions(&log_dir, std::fs::Permissions::from_mode(0o755))
            .expect("chmod dir");
        let first = LogManager::open(temp.path()).expect("open log manager");
        let path = first.active_path.clone();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        drop(first);
        let second = LogManager::open(temp.path()).expect("reopen log manager");
        assert_eq!(second.active_path, path);
        let mode = std::fs::metadata(&path)
            .expect("log file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = std::fs::metadata(&log_dir)
            .expect("log dir")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
    }
}

#[cfg(test)]
mod export_redaction_tests {
    use super::redact_jsonl_bytes;

    #[test]
    fn export_lines_are_redacted_and_non_json_lines_survive() {
        let input = concat!(
            "{\"message\":\"login\",\"password\":\"hunter2\",\"url\":\"https://example.com/a\"}\n",
            "plain text line\n",
            "{\"message\":\"ok\",\"requestId\":\"abc\"}\n",
        );
        let output = String::from_utf8(redact_jsonl_bytes(input.as_bytes())).expect("utf8");
        assert!(!output.contains("hunter2"));
        assert!(!output.contains("example.com"));
        assert!(output.contains("plain text line"));
        assert!(output.contains("\"requestId\":\"abc\""));
        assert_eq!(output.lines().count(), 3);
    }
}
