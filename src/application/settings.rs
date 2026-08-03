use std::{io::Write, path::Path};

use tokio::fs;

pub const TMDB_TOKEN_FILE: &str = "tmdb_read_access_token";
pub const TMDB_API_KEY_FILE: &str = "tmdb_api_key";

pub fn read_tmdb_api_key(config_dir: &Path) -> Option<String> {
    read_secret(config_dir.join(TMDB_API_KEY_FILE))
}

pub fn read_tmdb_token(config_dir: &Path) -> Option<String> {
    read_secret(config_dir.join(TMDB_TOKEN_FILE))
}

fn read_secret(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub async fn write_tmdb_api_key(config_dir: &Path, api_key: Option<&str>) -> std::io::Result<()> {
    write_secret_file(config_dir, TMDB_API_KEY_FILE, api_key).await
}

pub async fn write_tmdb_token(config_dir: &Path, token: &str) -> std::io::Result<()> {
    write_secret_file(config_dir, TMDB_TOKEN_FILE, Some(token)).await
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
