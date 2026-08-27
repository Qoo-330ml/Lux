use std::{io::Write, path::Path};

use tokio::fs;

pub const NETWORK_PROXY_URL_FILE: &str = "network_proxy_url";

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
