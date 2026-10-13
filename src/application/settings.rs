use std::{io::Write, path::Path};

use crate::{
    application::{home::HomeService, plugins::PluginService},
    storage::Database,
};
use tokio::fs;

pub const NETWORK_PROXY_URL_FILE: &str = "network_proxy_url";
pub const DEFAULT_LOGIN_BACKGROUND_SOURCE: &str = "STATIC";
pub const RECENTLY_ADDED_LOGIN_BACKGROUND_SOURCE: &str = "RECENTLY_ADDED";
pub const LOGIN_BACKGROUND_PLUGIN_SOURCE_PREFIX: &str = "PLUGIN:";

pub(crate) struct AdminSettingsWrite<'a> {
    pub(crate) resume_played_percent: i64,
    pub(crate) resume_min_ticks: i64,
    pub(crate) media_strategy_json: &'a str,
    pub(crate) force_admin_library_order: bool,
    pub(crate) login_background_source: &'a str,
    pub(crate) server_name: &'a str,
    pub(crate) refresh_login_background: bool,
}

pub(crate) async fn write_admin_settings(
    database: &Database,
    home: Option<&HomeService>,
    plugins: Option<&PluginService>,
    settings: AdminSettingsWrite<'_>,
) -> Result<(), ()> {
    database
        .set_server_settings(
            settings.resume_played_percent,
            settings.resume_min_ticks,
            settings.media_strategy_json,
            settings.force_admin_library_order,
            settings.login_background_source,
        )
        .await
        .map_err(|_| ())?;
    if settings.refresh_login_background
        && let Some(plugins) = plugins
    {
        plugins.request_login_background_refresh();
    }
    database
        .set_server_name(settings.server_name)
        .await
        .map_err(|_| ())?;
    if let Some(home) = home {
        home.invalidate();
    }
    Ok(())
}

pub fn is_valid_login_background_source(value: &str) -> bool {
    matches!(
        value,
        DEFAULT_LOGIN_BACKGROUND_SOURCE | RECENTLY_ADDED_LOGIN_BACKGROUND_SOURCE
    ) || login_background_plugin_id(value).is_some()
}

pub fn login_background_plugin_id(source: &str) -> Option<&str> {
    let plugin_id = source.strip_prefix(LOGIN_BACKGROUND_PLUGIN_SOURCE_PREFIX)?;
    if plugin_id.is_empty()
        || plugin_id.len() > 128
        || !plugin_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return None;
    }
    Some(plugin_id)
}

pub fn read_network_proxy_url(config_dir: &Path) -> Option<String> {
    read_secret(config_dir.join(NETWORK_PROXY_URL_FILE))
}

fn read_secret(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub async fn read_network_proxy_url_async(config_dir: &Path) -> Option<String> {
    fs::read_to_string(config_dir.join(NETWORK_PROXY_URL_FILE))
        .await
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub async fn write_network_proxy_url(
    config_dir: &Path,
    proxy_url: Option<&str>,
) -> std::io::Result<()> {
    write_secret_file(config_dir, NETWORK_PROXY_URL_FILE, proxy_url).await
}

async fn write_secret_file(
    config_dir: &Path,
    file_name: &str,
    value: Option<&str>,
) -> std::io::Result<()> {
    fs::create_dir_all(config_dir).await?;
    let path = config_dir.join(file_name);
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        if let Err(error) = fs::remove_file(path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error);
            }
        }
        return Ok(());
    };
    let value = format!("{value}\n");
    tokio::task::spawn_blocking(move || {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&path)?;
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        file.write_all(value.as_bytes())?;
        file.sync_all()
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?
}
