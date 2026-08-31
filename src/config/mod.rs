use std::{env, net::SocketAddr, path::PathBuf};

use crate::network::proxy_url_from_env;

pub mod database;

pub use database::{
    DatabaseBackend, DatabaseConfiguration, DatabaseConfigurationError, PostgresConnection,
};

const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:8097";
const DEFAULT_CONFIG_DIR: &str = "./config";
pub const DEFAULT_SCAN_CONCURRENCY: i64 = 32;
pub const MAX_SCAN_CONCURRENCY: i64 = 1024;
const SCAN_CONCURRENCY_ENV: &str = "LUX_SCAN_CONCURRENCY";

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
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::{
        ConfigError, DEFAULT_SCAN_CONCURRENCY, MAX_SCAN_CONCURRENCY, parse_scan_concurrency,
    };

    #[test]
    fn scan_concurrency_defaults_to_32() {
        assert_eq!(
            parse_scan_concurrency(None).unwrap(),
            DEFAULT_SCAN_CONCURRENCY
        );
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
}
