use std::{
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const TICKS_PER_SECOND: u64 = 10_000_000;
const NANOS_PER_TICK: u64 = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UtcTime(SystemTime);

impl UtcTime {
    pub fn now() -> Self {
        Self(SystemTime::now())
    }

    pub const fn from_system_time(time: SystemTime) -> Self {
        Self(time)
    }

    pub const fn system_time(self) -> SystemTime {
        self.0
    }

    pub fn duration_since_epoch(self) -> Result<Duration, TimeError> {
        self.0
            .duration_since(UNIX_EPOCH)
            .map_err(|_| TimeError::BeforeUnixEpoch)
    }
}

pub fn duration_to_ticks(duration: Duration) -> Result<i64, TimeError> {
    let second_ticks = duration
        .as_secs()
        .checked_mul(TICKS_PER_SECOND)
        .ok_or(TimeError::Overflow)?;
    let subsecond_ticks = u64::from(duration.subsec_nanos()) / NANOS_PER_TICK;
    let ticks = second_ticks
        .checked_add(subsecond_ticks)
        .ok_or(TimeError::Overflow)?;
    i64::try_from(ticks).map_err(|_| TimeError::Overflow)
}

pub fn ticks_to_duration(ticks: i64) -> Result<Duration, TimeError> {
    if ticks < 0 {
        return Err(TimeError::NegativeTicks);
    }

    let ticks = ticks as u64;
    let seconds = ticks / TICKS_PER_SECOND;
    let nanos = (ticks % TICKS_PER_SECOND) * NANOS_PER_TICK;
    Ok(Duration::new(seconds, nanos as u32))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeError {
    BeforeUnixEpoch,
    NegativeTicks,
    Overflow,
}

impl fmt::Display for TimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::BeforeUnixEpoch => "UTC time is before the Unix epoch",
            Self::NegativeTicks => "Emby ticks cannot be negative",
            Self::Overflow => "time value exceeds the supported tick range",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for TimeError {}
