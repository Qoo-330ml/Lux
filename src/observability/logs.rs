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
        self.initialize().await?;
        let manager = Arc::clone(&self.manager);
        tokio::task::spawn_blocking(move || {
            let mut line = serde_json::to_vec(&record)
                .map_err(|error| io::Error::other(format!("日志记录编码失败: {error}")))?;
            line.push(b'\n');
            let mut manager = manager
                .lock()
                .map_err(|_| io::Error::other("日志 writer 状态不可用"))?;
            let manager = manager
                .as_mut()
                .ok_or_else(|| io::Error::other("日志 writer 尚未初始化"))?;
            manager.write_line(&line)
        })
        .await
        .map_err(|error| io::Error::other(format!("日志写入任务失败: {error}")))?
    }

    pub(crate) async fn append_json_batch_and_sync(&self, records: Vec<Value>) -> io::Result<()> {
        self.suspend_archive_pruning().await?;
        if records.is_empty() {
            return Ok(());
        }
        let manager = Arc::clone(&self.manager);
        tokio::task::spawn_blocking(move || {
            let mut manager = manager
                .lock()
                .map_err(|_| io::Error::other("日志 writer 状态不可用"))?;
            let manager = manager
                .as_mut()
                .ok_or_else(|| io::Error::other("日志 writer 尚未初始化"))?;
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

fn read_scan_job_events(
    log_dir: &Path,
    archive_dir: &Path,
    job_id: &str,
    level: Option<&str>,
    event_code: Option<&str>,
    offset: i64,
    limit: i64,
) -> io::Result<(i64, Vec<ScanJobLogEvent>)> {
    let mut events = Vec::new();
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
                let file = fs::File::open(path)?;
                read_scan_job_event_lines(
                    io::BufReader::new(file),
                    job_id,
                    level,
                    event_code,
                    &mut events,
                )?;
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
            let file = fs::File::open(path)?;
            let mut archive = ZipArchive::new(file)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            for index in 0..archive.len() {
                let member = archive
                    .by_index(index)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                if !member.name().ends_with(".log") {
                    continue;
                }
                read_scan_job_event_lines(
                    io::BufReader::new(member),
                    job_id,
                    level,
                    event_code,
                    &mut events,
                )?;
            }
        }
    }
    events.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    let mut seen_ids = std::collections::HashSet::with_capacity(events.len());
    events.retain(|event| seen_ids.insert(event.id.clone()));
    let total = i64::try_from(events.len()).unwrap_or(i64::MAX);
    let start = usize::try_from(offset.max(0)).unwrap_or(usize::MAX);
    let count = usize::try_from(limit.max(0)).unwrap_or(usize::MAX);
    let page = events.into_iter().skip(start).take(count).collect();
    Ok((total, page))
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

fn read_scan_job_event_lines<R: io::BufRead>(
    mut reader: R,
    job_id: &str,
    level: Option<&str>,
    event_code: Option<&str>,
    events: &mut Vec<ScanJobLogEvent>,
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
        if record["recordType"] != "scan_job_event" || record["jobId"] != job_id {
            continue;
        }
        let Some(id) = record["id"].as_str() else {
            continue;
        };
        let Some(event_level) = record["level"].as_str() else {
            continue;
        };
        let Some(code) = record["eventCode"].as_str() else {
            continue;
        };
        if level.is_some_and(|value| !event_level.eq_ignore_ascii_case(value))
            || event_code.is_some_and(|value| !code.eq_ignore_ascii_case(value))
        {
            continue;
        }
        let created_at = record["createdAt"].as_i64().unwrap_or_default();
        let message = record["message"].as_str().unwrap_or_default().to_owned();
        let details_json =
            serde_json::to_string(&record["details"]).unwrap_or_else(|_| "{}".to_owned());
        events.push(ScanJobLogEvent {
            id: id.to_owned(),
            job_id: job_id.to_owned(),
            level: event_level.to_owned(),
            event_code: code.to_owned(),
            message,
            details_json,
            created_at,
        });
    }
}

fn read_audit_events(
    log_dir: &Path,
    archive_dir: &Path,
    offset: i64,
    limit: i64,
    event_types: Option<&[&str]>,
) -> io::Result<(i64, Vec<AuditLogEvent>)> {
    let mut events = Vec::new();
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
                read_audit_event_lines(
                    io::BufReader::new(fs::File::open(path)?),
                    event_types,
                    &mut events,
                )?;
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
                    read_audit_event_lines(io::BufReader::new(member), event_types, &mut events)?;
                }
            }
        }
    }
    events.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    let mut seen_ids = HashSet::with_capacity(events.len());
    events.retain(|event| seen_ids.insert(event.id.clone()));
    let total = i64::try_from(events.len()).unwrap_or(i64::MAX);
    let start = usize::try_from(offset.max(0)).unwrap_or(usize::MAX);
    let count = usize::try_from(limit.max(0)).unwrap_or(usize::MAX);
    Ok((total, events.into_iter().skip(start).take(count).collect()))
}

fn read_audit_event_lines<R: io::BufRead>(
    mut reader: R,
    event_types: Option<&[&str]>,
    events: &mut Vec<AuditLogEvent>,
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
        if record["recordType"] != "admin_audit_event" {
            continue;
        }
        let Some(id) = record["id"].as_str() else {
            continue;
        };
        let Some(event_type) = record["eventType"].as_str() else {
            continue;
        };
        if event_types.is_some_and(|allowed| !allowed.contains(&event_type)) {
            continue;
        }
        let metadata_json =
            serde_json::to_string(&record["metadata"]).unwrap_or_else(|_| "{}".to_owned());
        events.push(AuditLogEvent {
            id: id.to_owned(),
            actor_user_id: record["actorUserId"].as_str().map(str::to_owned),
            actor_username: record["actorUsername"].as_str().map(str::to_owned),
            event_type: event_type.to_owned(),
            target_type: record["targetType"].as_str().map(str::to_owned),
            target_id: record["targetId"].as_str().map(str::to_owned),
            metadata_json,
            created_at: record["createdAt"].as_i64().unwrap_or_default(),
        });
    }
}

fn read_activity_events(
    log_dir: &Path,
    archive_dir: &Path,
    limit: i64,
) -> io::Result<Vec<AuditLogEvent>> {
    let category_limit = usize::try_from((limit / 2).max(1)).unwrap_or(usize::MAX);
    let mut login_events = Vec::with_capacity(category_limit.min(12));
    let mut playback_events = Vec::with_capacity(category_limit.min(12));
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
                read_activity_event_lines(
                    io::BufReader::new(fs::File::open(path)?),
                    category_limit,
                    &mut login_events,
                    &mut playback_events,
                )?;
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
                    read_activity_event_lines(
                        io::BufReader::new(member),
                        category_limit,
                        &mut login_events,
                        &mut playback_events,
                    )?;
                }
            }
        }
    }
    login_events.extend(playback_events);
    sort_audit_events(&mut login_events);
    let mut seen_ids = HashSet::with_capacity(login_events.len());
    login_events.retain(|event| seen_ids.insert(event.id.clone()));
    login_events.truncate(usize::try_from(limit.max(0)).unwrap_or(usize::MAX));
    Ok(login_events)
}

fn read_activity_event_lines<R: io::BufRead>(
    mut reader: R,
    category_limit: usize,
    login_events: &mut Vec<AuditLogEvent>,
    playback_events: &mut Vec<AuditLogEvent>,
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
        if record["recordType"] != "admin_audit_event" {
            continue;
        }
        let Some(event_type) = record["eventType"].as_str() else {
            continue;
        };
        let category = match event_type {
            "AUTH_LOGIN" => &mut *login_events,
            "PLAYBACK_STARTED" | "PLAYBACK_PAUSED" | "PLAYBACK_STOPPED" => &mut *playback_events,
            _ => continue,
        };
        let Some(id) = record["id"].as_str() else {
            continue;
        };
        let metadata_json =
            serde_json::to_string(&record["metadata"]).unwrap_or_else(|_| "{}".to_owned());
        category.push(AuditLogEvent {
            id: id.to_owned(),
            actor_user_id: record["actorUserId"].as_str().map(str::to_owned),
            actor_username: record["actorUsername"].as_str().map(str::to_owned),
            event_type: event_type.to_owned(),
            target_type: record["targetType"].as_str().map(str::to_owned),
            target_id: record["targetId"].as_str().map(str::to_owned),
            metadata_json,
            created_at: record["createdAt"].as_i64().unwrap_or_default(),
        });
        sort_audit_events(category);
        category.truncate(category_limit);
    }
}

fn sort_audit_events(events: &mut [AuditLogEvent]) {
    events.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.id.cmp(&left.id))
    });
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
        if let Some(file) = self.active_file.as_ref() {
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
    Ok(files)
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
        fs,
        io::{self, Write},
        sync::Arc,
    };

    use super::{
        LogDateRange, LogExport, LogExportError, LogManager, LogStore, NewAuditLogEvent,
        archive_dir, export_logs, log_dir, log_file_name, parse_date, read_audit_events,
        read_scan_job_events, verify_archive,
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
