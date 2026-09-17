use std::{env, net::SocketAddr, path::PathBuf};

use crate::network::proxy_url_from_env;

pub mod database;

pub use database::{
    DatabaseBackend, DatabaseConfiguration, DatabaseConfigurationError, PostgresConnection,
};

const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:8097";
const DEFAULT_CONFIG_DIR: &str = "./config";
pub const DEFAULT_SCAN_CONCURRENCY: i64 = 16;
pub const MAX_SCAN_CONCURRENCY: i64 = 1024;
const SCAN_CONCURRENCY_ENV: &str = "LUX_SCAN_CONCURRENCY";
pub const MAX_PROBE_CONCURRENCY: i64 = 512;
const PROBE_CONCURRENCY_ENV: &str = "LUX_PROBE_CONCURRENCY";
pub const DEFAULT_FFMPEG_CONCURRENCY: i64 = 4;
pub const MAX_FFMPEG_CONCURRENCY: i64 = 4;
const FFMPEG_CONCURRENCY_ENV: &str = "LUX_FFMPEG_CONCURRENCY";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub http_addr: SocketAddr,
    pub config_dir: PathBuf,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let http_addr = env::var("LUX_HTTP_ADDR")
            .unwrap_or_else(|_| DEFAULT_HTTP_ADDR.to_owned())
            .parse()
            .map_err(|source| ConfigError::InvalidHttpAddr {
                value: env::var("LUX_HTTP_ADDR").unwrap_or_else(|_| DEFAULT_HTTP_ADDR.to_owned()),
                source,
            })?;
        let config_dir = env::var_os("LUX_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_DIR));
        scan_concurrency_from_env()?;
        probe_concurrency_override_from_env()?;
        ffmpeg_concurrency_from_env()?;
        proxy_url_from_env().map_err(|_| ConfigError::InvalidProxyUrl)?;

        Ok(Self {
            http_addr,
            config_dir,
        })
    }
}

pub fn scan_concurrency_from_env() -> Result<i64, ConfigError> {
    parse_scan_concurrency_override(env::var(SCAN_CONCURRENCY_ENV).ok().as_deref())
        .map(|value| value.unwrap_or(DEFAULT_SCAN_CONCURRENCY))
}

pub fn scan_concurrency_override_from_env() -> Result<Option<i64>, ConfigError> {
    parse_scan_concurrency_override(env::var(SCAN_CONCURRENCY_ENV).ok().as_deref())
}

pub fn probe_concurrency_override_from_env() -> Result<Option<i64>, ConfigError> {
    parse_probe_concurrency(env::var(PROBE_CONCURRENCY_ENV).ok().as_deref())
}

pub fn ffmpeg_concurrency_from_env() -> Result<i64, ConfigError> {
    parse_ffmpeg_concurrency(env::var(FFMPEG_CONCURRENCY_ENV).ok().as_deref())
        .map(|value| value.unwrap_or(DEFAULT_FFMPEG_CONCURRENCY))
}

pub fn ffmpeg_concurrency_override_from_env() -> Result<Option<i64>, ConfigError> {
    parse_ffmpeg_concurrency(env::var(FFMPEG_CONCURRENCY_ENV).ok().as_deref())
}

fn parse_scan_concurrency(configured: Option<&str>) -> Result<i64, ConfigError> {
    let Some(value) = configured.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(DEFAULT_SCAN_CONCURRENCY);
    };
    let parsed = value
        .parse::<i64>()
        .ok()
        .filter(|value| (1..=MAX_SCAN_CONCURRENCY).contains(value))
        .ok_or_else(|| ConfigError::InvalidScanConcurrency {
            value: value.to_owned(),
        })?;
    Ok(parsed)
}

fn parse_scan_concurrency_override(configured: Option<&str>) -> Result<Option<i64>, ConfigError> {
    let Some(value) = configured.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    parse_scan_concurrency(Some(value)).map(Some)
}

fn parse_probe_concurrency(configured: Option<&str>) -> Result<Option<i64>, ConfigError> {
    let Some(value) = configured.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let parsed = value
        .parse::<i64>()
        .ok()
        .filter(|value| (1..=MAX_PROBE_CONCURRENCY).contains(value))
        .ok_or_else(|| ConfigError::InvalidProbeConcurrency {
            value: value.to_owned(),
        })?;
    Ok(Some(parsed))
}

fn parse_ffmpeg_concurrency(configured: Option<&str>) -> Result<Option<i64>, ConfigError> {
    let Some(value) = configured.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let parsed = value
        .parse::<i64>()
        .ok()
        .filter(|value| (1..=MAX_FFMPEG_CONCURRENCY).contains(value))
        .ok_or_else(|| ConfigError::InvalidFfmpegConcurrency {
            value: value.to_owned(),
        })?;
    Ok(Some(parsed))
}

#[derive(Debug)]
pub enum ConfigError {
    InvalidHttpAddr {
        value: String,
        source: std::net::AddrParseError,
    },
    InvalidProxyUrl,
    InvalidScanConcurrency {
        value: String,
    },
    InvalidProbeConcurrency {
        value: String,
    },
    InvalidFfmpegConcurrency {
        value: String,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHttpAddr { value, source } => {
                write!(formatter, "invalid LUX_HTTP_ADDR '{value}': {source}")
            }
            Self::InvalidProxyUrl => {
                formatter.write_str(
                    "invalid LUX_PROXY_URL: expected an http, https, socks4, socks4a, socks5, or socks5h proxy URL",
                )
            }
            Self::InvalidScanConcurrency { value } => write!(
                formatter,
                "invalid LUX_SCAN_CONCURRENCY '{value}': expected an integer between 1 and {MAX_SCAN_CONCURRENCY}"
            ),
            Self::InvalidProbeConcurrency { value } => write!(
                formatter,
                "invalid LUX_PROBE_CONCURRENCY '{value}': expected an integer between 1 and {MAX_PROBE_CONCURRENCY}"
            ),
            Self::InvalidFfmpegConcurrency { value } => write!(
                formatter,
                "invalid LUX_FFMPEG_CONCURRENCY '{value}': expected an integer between 1 and {MAX_FFMPEG_CONCURRENCY}"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::{
        ConfigError, DEFAULT_SCAN_CONCURRENCY, MAX_PROBE_CONCURRENCY, MAX_SCAN_CONCURRENCY,
        parse_ffmpeg_concurrency, parse_probe_concurrency, parse_scan_concurrency,
    };

    #[test]
    fn scan_concurrency_defaults_to_16() {
        assert_eq!(parse_scan_concurrency(None).unwrap(), 16);
        assert_eq!(DEFAULT_SCAN_CONCURRENCY, 16);
    }

    #[test]
    fn scan_concurrency_accepts_values_through_1024() {
        assert_eq!(
            parse_scan_concurrency(Some("1024")).unwrap(),
            MAX_SCAN_CONCURRENCY
        );
    }

    #[test]
    fn scan_concurrency_override_distinguishes_unset_from_default() {
        assert_eq!(super::parse_scan_concurrency_override(None).unwrap(), None);
        assert_eq!(
            super::parse_scan_concurrency_override(Some("1024")).unwrap(),
            Some(MAX_SCAN_CONCURRENCY)
        );
    }

    #[test]
    fn scan_concurrency_rejects_zero_and_values_above_limit() {
        assert!(matches!(
            parse_scan_concurrency(Some("0")),
            Err(ConfigError::InvalidScanConcurrency { .. })
        ));
        assert!(matches!(
            parse_scan_concurrency(Some("1025")),
            Err(ConfigError::InvalidScanConcurrency { .. })
        ));
    }

    #[test]
    fn probe_concurrency_defaults_to_none_for_the_process_override() {
        assert_eq!(parse_probe_concurrency(None).unwrap(), None);
    }

    #[test]
    fn probe_concurrency_accepts_values_through_the_effective_limit() {
        assert_eq!(parse_probe_concurrency(Some("8")).unwrap(), Some(8));
        assert_eq!(
            parse_probe_concurrency(Some("512")).unwrap(),
            Some(MAX_PROBE_CONCURRENCY)
        );
    }

    #[test]
    fn probe_concurrency_rejects_zero_and_values_above_limit() {
        assert!(matches!(
            parse_probe_concurrency(Some("0")),
            Err(ConfigError::InvalidProbeConcurrency { .. })
        ));
        assert!(matches!(
            parse_probe_concurrency(Some("513")),
            Err(ConfigError::InvalidProbeConcurrency { .. })
        ));
    }

    #[test]
    fn ffmpeg_concurrency_accepts_trimmed_values_and_rejects_invalid_values() {
        assert_eq!(parse_ffmpeg_concurrency(None).unwrap(), None);
        assert_eq!(parse_ffmpeg_concurrency(Some(" 2 ")).unwrap(), Some(2));
        assert!(matches!(
            parse_ffmpeg_concurrency(Some("0")),
            Err(ConfigError::InvalidFfmpegConcurrency { .. })
        ));
        assert!(matches!(
            parse_ffmpeg_concurrency(Some("5")),
            Err(ConfigError::InvalidFfmpegConcurrency { .. })
        ));
        assert!(matches!(
            parse_ffmpeg_concurrency(Some("not-a-number")),
            Err(ConfigError::InvalidFfmpegConcurrency { .. })
        ));
    }
}
