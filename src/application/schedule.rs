use std::time::Duration;

pub const DEFAULT_STRM_MEDIA_INFO_INTERVAL: &str = "24h";
pub const STRM_MEDIA_INFO_TASK_TYPE: &str = "STRM_MEDIA_INFO";
const MIN_INTERVAL_SECONDS: u64 = 60;
const MAX_INTERVAL_SECONDS: u64 = 365 * 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntervalParseError {
    Empty,
    Invalid,
    OutOfRange,
}

pub fn parse_interval(value: &str) -> Result<Duration, IntervalParseError> {
    let value = value.trim();
    let Some(unit) = value.chars().last() else {
        return Err(IntervalParseError::Empty);
    };
    let number = &value[..value.len() - unit.len_utf8()];
    let amount = number
        .parse::<u64>()
        .map_err(|_| IntervalParseError::Invalid)?;
    let multiplier = match unit {
        's' => 1,
        'm' => 60,
        'h' => 60 * 60,
        'd' => 24 * 60 * 60,
        _ => return Err(IntervalParseError::Invalid),
    };
    let seconds = amount
        .checked_mul(multiplier)
        .ok_or(IntervalParseError::OutOfRange)?;
    if !(MIN_INTERVAL_SECONDS..=MAX_INTERVAL_SECONDS).contains(&seconds) {
        return Err(IntervalParseError::OutOfRange);
    }
    Ok(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::parse_interval;
    use std::time::Duration;

    #[test]
    fn parses_supported_interval_units() {
        assert_eq!(parse_interval("1m").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_interval("6h").unwrap(), Duration::from_secs(21_600));
        assert_eq!(
            parse_interval("365d").unwrap(),
            Duration::from_secs(31_536_000)
        );
    }

    #[test]
    fn rejects_invalid_or_out_of_range_intervals() {
        for value in ["", "0m", "59s", "366d", "1w", "1.5h", "cron"] {
            assert!(parse_interval(value).is_err(), "{value}");
        }
    }
}
