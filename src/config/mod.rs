use std::{env, net::SocketAddr, path::PathBuf};

use crate::network::proxy_url_from_env;

const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:8097";
const DEFAULT_CONFIG_DIR: &str = "./config";

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
        proxy_url_from_env().map_err(|_| ConfigError::InvalidProxyUrl)?;

        Ok(Self {
            http_addr,
            config_dir,
        })
    }
}

#[derive(Debug)]
pub enum ConfigError {
    InvalidHttpAddr {
        value: String,
        source: std::net::AddrParseError,
    },
    InvalidProxyUrl,
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
        }
    }
}

impl std::error::Error for ConfigError {}
