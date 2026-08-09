use std::{
    fmt,
    io::{self, Cursor, Write},
    path::{Path, PathBuf},
};

use time::{Date, Month, OffsetDateTime};
use tokio::fs;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

pub const LOG_DIRECTORY: &str = "logs";
pub const MAX_EXPORT_DAYS: i64 = 31;
const DEFAULT_EXPORT_DAYS: i64 = 7;
const MAX_DAILY_LOG_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EXPORT_BYTES: u64 = 128 * 1024 * 1024;

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
pub struct LogExport {
    pub archive: Vec<u8>,
    pub filename: String,
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
    let directory = log_dir(config_dir);
    let mut files = Vec::new();
    let mut total_bytes = 0_u64;
    for date in range.dates() {
        let name = log_file_name(date);
        let path = directory.join(&name);
        let contents = match fs::read(&path).await {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(LogExportError::Io(error)),
        };
        let size = u64::try_from(contents.len()).unwrap_or(u64::MAX);
        if size > MAX_DAILY_LOG_BYTES || total_bytes.saturating_add(size) > MAX_EXPORT_BYTES {
            return Err(LogExportError::ExportTooLarge);
        }
        total_bytes = total_bytes.saturating_add(size);
        files.push((name, contents));
    }
    if files.is_empty() {
        return Err(LogExportError::NoLogs);
    }

    let filename = format!(
        "lux-logs-{}-{}.zip",
        compact_date(range.from),
        compact_date(range.to)
    );
    let archive = tokio::task::spawn_blocking(move || create_archive(files))
        .await
        .map_err(|error| LogExportError::Worker(error.to_string()))??;
    Ok(LogExport { archive, filename })
}

fn create_archive(files: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>, LogExportError> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, contents) in files {
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
    use super::{LogDateRange, LogExportError, log_file_name, parse_date};
    use time::{Date, Month};

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
}
