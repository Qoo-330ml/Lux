//! Wall-clock helpers.
//!
//! Timestamps are stored as Unix seconds. A clock that reads before the epoch or overflows must
//! not silently become 1970: that would sort new rows as ancient and make progress look stale.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Current Unix time in seconds, saturating at the representable bounds.
pub(crate) fn unix_timestamp() -> i64 {
    seconds_from_epoch_offset(SystemTime::now().duration_since(UNIX_EPOCH))
}

fn seconds_from_epoch_offset(offset: Result<Duration, std::time::SystemTimeError>) -> i64 {
    match offset {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        // Before 1970 the system clock is still a real instant; represent it as negative time
        // rather than pretending the event happened at the epoch.
        Err(error) => -i64::try_from(error.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::seconds_from_epoch_offset;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn epoch_offsets_map_to_signed_seconds() {
        assert_eq!(seconds_from_epoch_offset(Ok(Duration::from_secs(0))), 0);
        assert_eq!(
            seconds_from_epoch_offset(Ok(Duration::from_secs(1_700_000_000))),
            1_700_000_000
        );
        let before_epoch = UNIX_EPOCH
            .checked_sub(Duration::from_secs(60))
            .expect("epoch supports subtraction")
            .duration_since(UNIX_EPOCH)
            .expect_err("this instant is before the epoch");
        assert_eq!(seconds_from_epoch_offset(Err(before_epoch)), -60);
        assert_eq!(
            seconds_from_epoch_offset(Ok(Duration::from_secs(u64::MAX))),
            i64::MAX
        );
    }
}
