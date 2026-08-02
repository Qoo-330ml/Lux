use std::{io::Write, path::Path};

use tokio::fs;

pub const TMDB_TOKEN_FILE: &str = "tmdb_read_access_token";

pub fn read_tmdb_token(config_dir: &Path) -> Option<String> {
    std::fs::read_to_string(config_dir.join(TMDB_TOKEN_FILE))
        .ok()
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
}

pub async fn write_tmdb_token(config_dir: &Path, token: &str) -> std::io::Result<()> {
    fs::create_dir_all(config_dir).await?;
    let path = config_dir.join(TMDB_TOKEN_FILE);
    let token = format!("{}\n", token.trim());
    tokio::task::spawn_blocking(move || {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&path)?;
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        file.write_all(token.as_bytes())?;
        file.sync_all()
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?
}
