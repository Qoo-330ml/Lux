use std::fmt;

use time::OffsetDateTime;

pub const DEFAULT_STRM_MEDIA_INFO_SCHEDULE: &str = "0 3 * * *";
pub const STRM_MEDIA_INFO_TASK_TYPE: &str = "STRM_MEDIA_INFO";
const MAX_SCHEDULE_LENGTH: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CronSchedule {
    minute: CronField,
    hour: CronField,
    day_of_month: CronField,
    month: CronField,
    day_of_week: CronField,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CronField {
    values: Vec<bool>,
    any: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CronParseError {
    Empty,
    TooLong,
    FieldCount,
    InvalidField,
    OutOfRange,
}

impl fmt::Display for CronParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Empty => "cron expression is empty",
            Self::TooLong => "cron expression is too long",
            Self::FieldCount => "cron expression must contain five fields",
            Self::InvalidField => "cron expression contains an invalid field",
            Self::OutOfRange => "cron expression contains a value outside its field range",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CronParseError {}

pub fn parse_cron(value: &str) -> Result<CronSchedule, CronParseError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(CronParseError::Empty);
    }
    if value.chars().count() > MAX_SCHEDULE_LENGTH {
        return Err(CronParseError::TooLong);
    }
    let fields = value.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 {
        return Err(CronParseError::FieldCount);
    }
    Ok(CronSchedule {
        minute: parse_field(fields[0], 0, 59, false)?,
        hour: parse_field(fields[1], 0, 23, false)?,
        day_of_month: parse_field(fields[2], 1, 31, false)?,
        month: parse_field(fields[3], 1, 12, false)?,
        day_of_week: parse_field(fields[4], 0, 7, true)?,
    })
}

pub fn validate_cron(value: &str) -> Result<(), CronParseError> {
    parse_cron(value).map(|_| ())
}

impl CronSchedule {
    pub fn matches(&self, now: OffsetDateTime) -> bool {
        if !self.minute.matches(now.minute())
            || !self.hour.matches(now.hour())
            || !self.month.matches(now.month() as u8)
        {
            return false;
        }
        let day_of_month_matches = self.day_of_month.matches(now.day());
        let day_of_week_matches = self
            .day_of_week
            .matches(now.weekday().number_days_from_sunday());
        match (self.day_of_month.any, self.day_of_week.any) {
            (true, true) => true,
            (true, false) => day_of_week_matches,
            (false, true) => day_of_month_matches,
            (false, false) => day_of_month_matches || day_of_week_matches,
        }
    }
}

impl CronField {
    fn matches(&self, value: u8) -> bool {
        self.values.get(value as usize).copied().unwrap_or(false)
    }
}

fn parse_field(
    value: &str,
    minimum: u8,
    maximum: u8,
    day_of_week: bool,
) -> Result<CronField, CronParseError> {
    if value.is_empty() {
        return Err(CronParseError::InvalidField);
    }
    let mut values = vec![false; usize::from(maximum) + 1];
    let mut any = false;
    for item in value.split(',') {
        if item.is_empty() {
            return Err(CronParseError::InvalidField);
        }
        let (range, step, has_step) = match item.split_once('/') {
            Some((range, step)) => {
                let step = step
                    .parse::<u8>()
                    .map_err(|_| CronParseError::InvalidField)?;
                if step == 0 {
                    return Err(CronParseError::InvalidField);
                }
                (range, step, true)
            }
            None => (item, 1, false),
        };
        if range == "*" {
            any = true;
        }
        let (start, end) = if range == "*" {
            (minimum, maximum)
        } else if let Some((start, end)) = range.split_once('-') {
            let start = parse_value(start, minimum, maximum)?;
            let end = parse_value(end, minimum, maximum)?;
            if start > end {
                return Err(CronParseError::InvalidField);
            }
            (start, end)
        } else {
            let value = parse_value(range, minimum, maximum)?;
            if has_step {
                (value, maximum)
            } else {
                (value, value)
            }
        };
        for value in (start..=end).step_by(usize::from(step)) {
            let value = if day_of_week && value == 7 { 0 } else { value };
            if let Some(entry) = values.get_mut(usize::from(value)) {
                *entry = true;
            }
        }
    }
    Ok(CronField { values, any })
}

fn parse_value(value: &str, minimum: u8, maximum: u8) -> Result<u8, CronParseError> {
    let value = value
        .parse::<u8>()
        .map_err(|_| CronParseError::InvalidField)?;
    if value < minimum || value > maximum {
        return Err(CronParseError::OutOfRange);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{CronParseError, parse_cron, validate_cron};
    use time::{Date, Month, OffsetDateTime, Time};

    fn utc_datetime(hour: u8, day: u8) -> OffsetDateTime {
        Date::from_calendar_date(2026, Month::August, day)
            .expect("valid test date")
            .with_time(Time::from_hms(hour, 0, 0).expect("valid test time"))
            .assume_utc()
    }

    #[test]
    fn parses_five_field_cron_and_matches_utc_time() {
        let schedule = parse_cron("0 3 * * *").expect("valid cron");
        assert!(schedule.matches(utc_datetime(3, 10)));
        assert!(!schedule.matches(utc_datetime(4, 10)));
    }

    #[test]
    fn supports_lists_ranges_steps_and_sunday_seven() {
        let schedule = parse_cron("*/15 2,4 1-10 1,8 0,7").expect("valid cron");
        assert!(schedule.matches(utc_datetime(2, 2)));
        assert!(schedule.matches(utc_datetime(4, 9)));
        assert!(!schedule.matches(utc_datetime(3, 2)));
    }

    #[test]
    fn restricted_day_of_month_and_week_use_cron_or_semantics() {
        let schedule = parse_cron("0 3 10 * 1").expect("valid cron");
        assert!(schedule.matches(utc_datetime(3, 10)));
        assert!(schedule.matches(utc_datetime(3, 17)));
    }

    #[test]
    fn rejects_non_cron_values() {
        for value in ["", "0 3 * *", "60 3 * * *", "0 3 * * 8", "*/0 * * * *"] {
            assert!(validate_cron(value).is_err(), "{value}");
        }
        assert_eq!(
            validate_cron("interval:5m"),
            Err(CronParseError::FieldCount)
        );
    }

    #[test]
    fn accepts_seven_in_non_weekday_fields() {
        assert!(validate_cron("0 7 7 7 *").is_ok());
    }
}
