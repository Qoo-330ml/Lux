use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tokio::fs;
use url::Url;
use uuid::Uuid;

use super::Config;

const DATABASE_CONFIG_FILE: &str = "database.json";
const DATABASE_SELECTION_MARKER_FILE: &str = ".database-selection-required";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DatabaseBackend {
    Sqlite,
    Postgres,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "backend", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DatabaseConfiguration {
    Sqlite,
    Postgres(PostgresConnection),
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostgresConnection {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: String,
    pub ssl_mode: String,
}

impl fmt::Debug for DatabaseConfiguration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite => formatter.write_str("DatabaseConfiguration::Sqlite"),
            Self::Postgres(connection) => formatter
                .debug_tuple("DatabaseConfiguration::Postgres")
                .field(&connection.redacted())
                .finish(),
        }
    }
}

impl fmt::Debug for PostgresConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.redacted().fmt(formatter)
    }
}

impl PostgresConnection {
    fn redacted(&self) -> RedactedPostgresConnection<'_> {
        RedactedPostgresConnection { connection: self }
    }
}

struct RedactedPostgresConnection<'a> {
    connection: &'a PostgresConnection,
}

impl fmt::Debug for RedactedPostgresConnection<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PostgresConnection")
            .field("host", &self.connection.host)
            .field("port", &self.connection.port)
            .field("database", &self.connection.database)
            .field("username", &self.connection.username)
            .field("password", &"[REDACTED]")
            .field("ssl_mode", &self.connection.ssl_mode)
            .finish()
    }
}

impl DatabaseConfiguration {
    pub fn backend(&self) -> DatabaseBackend {
        match self {
            Self::Sqlite => DatabaseBackend::Sqlite,
            Self::Postgres(_) => DatabaseBackend::Postgres,
        }
    }

    pub fn validate(&self) -> Result<(), DatabaseConfigurationError> {
        let Self::Postgres(connection) = self else {
            return Ok(());
        };

        validate_text("PostgreSQL host", &connection.host, 255)?;
        validate_identifier("PostgreSQL database", &connection.database)?;
        validate_identifier("PostgreSQL username", &connection.username)?;
        if connection.port == 0 {
            return Err(DatabaseConfigurationError::Invalid(
                "PostgreSQL 端口无效".to_owned(),
            ));
        }
        if !matches!(
            connection.ssl_mode.as_str(),
            "disable" | "prefer" | "require" | "verify-ca" | "verify-full"
        ) {
            return Err(DatabaseConfigurationError::Invalid(
                "PostgreSQL SSL 模式无效".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn postgres_url(&self) -> Result<Option<String>, DatabaseConfigurationError> {
        let Self::Postgres(connection) = self else {
            return Ok(None);
        };
        self.validate()?;

        let mut url = Url::parse("postgresql://localhost/lux").map_err(|_| {
            DatabaseConfigurationError::Invalid("无法构造 PostgreSQL 连接地址".to_owned())
        })?;
        url.set_host(Some(&connection.host)).map_err(|_| {
            DatabaseConfigurationError::Invalid("PostgreSQL 主机地址无效".to_owned())
        })?;
        url.set_port(Some(connection.port))
            .map_err(|_| DatabaseConfigurationError::Invalid("PostgreSQL 端口无效".to_owned()))?;
        url.set_username(&connection.username)
            .map_err(|_| DatabaseConfigurationError::Invalid("PostgreSQL 用户名无效".to_owned()))?;
        url.set_password(Some(&connection.password))
            .map_err(|_| DatabaseConfigurationError::Invalid("PostgreSQL 密码无效".to_owned()))?;
        url.set_path(&format!("/{}", connection.database));
        url.set_query(Some(&format!("sslmode={}", connection.ssl_mode)));
        Ok(Some(url.to_string()))
    }
}

impl Config {
    pub async fn load_database_configuration(
        &self,
    ) -> Result<Option<DatabaseConfiguration>, DatabaseConfigurationError> {
        let explicit = self.load_explicit_database_configuration().await?;
        if explicit.is_some() {
            return Ok(explicit);
        }
        Ok(self
            .has_legacy_sqlite_database()
            .await
            .then_some(DatabaseConfiguration::Sqlite))
    }

    pub async fn load_explicit_database_configuration(
        &self,
    ) -> Result<Option<DatabaseConfiguration>, DatabaseConfigurationError> {
        let path = self.config_dir.join(DATABASE_CONFIG_FILE);
        let contents = match fs::read(&path).await {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(source) => return Err(DatabaseConfigurationError::Io { path, source }),
        };
        let configuration: DatabaseConfiguration =
            serde_json::from_slice(&contents).map_err(|source| {
                DatabaseConfigurationError::Invalid(format!("数据库配置文件格式无效: {source}"))
            })?;
        configuration.validate()?;
        Ok(Some(configuration))
    }

    pub async fn has_legacy_sqlite_database(&self) -> bool {
        if fs::metadata(self.config_dir.join("lux.db")).await.is_err() {
            return false;
        }
        fs::metadata(self.config_dir.join(DATABASE_SELECTION_MARKER_FILE))
            .await
            .is_err()
    }

    /// True while the setup wizard still has to pick a database, so the one currently open is only
    /// a placeholder.
    pub async fn database_selection_pending(&self) -> bool {
        fs::metadata(self.config_dir.join(DATABASE_SELECTION_MARKER_FILE))
            .await
            .is_ok()
    }

    pub async fn mark_database_selection_pending(&self) -> Result<(), DatabaseConfigurationError> {
        self.prepare_config_directory().await?;
        self.write_private_configuration_file(DATABASE_SELECTION_MARKER_FILE, b"pending".to_vec())
            .await
    }

    pub async fn save_database_configuration(
        &self,
        configuration: &DatabaseConfiguration,
    ) -> Result<(), DatabaseConfigurationError> {
        configuration.validate()?;
        let bytes = serde_json::to_vec_pretty(configuration).map_err(|source| {
            DatabaseConfigurationError::Invalid(format!("数据库配置无法序列化: {source}"))
        })?;
        self.prepare_config_directory().await?;
        self.write_private_configuration_file(DATABASE_CONFIG_FILE, bytes)
            .await?;
        let _ = fs::remove_file(self.config_dir.join(DATABASE_SELECTION_MARKER_FILE)).await;
        Ok(())
    }

    async fn prepare_config_directory(&self) -> Result<(), DatabaseConfigurationError> {
        let config_dir = self.config_dir.clone();
        match tokio::task::spawn_blocking(move || prepare_private_config_directory(&config_dir))
            .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(source)) => Err(DatabaseConfigurationError::Io {
                path: self.config_dir.clone(),
                source,
            }),
            Err(error) => Err(DatabaseConfigurationError::Io {
                path: self.config_dir.clone(),
                source: io::Error::other(error.to_string()),
            }),
        }
    }

    async fn write_private_configuration_file(
        &self,
        file_name: &str,
        bytes: Vec<u8>,
    ) -> Result<(), DatabaseConfigurationError> {
        let target = self.config_dir.join(file_name);
        let temporary = self.config_dir.join(temporary_file_name(file_name));
        let target_for_write = target.clone();
        let temporary_for_write = temporary.clone();
        match tokio::task::spawn_blocking(move || {
            write_private_file_atomically(&target_for_write, &temporary_for_write, &bytes)
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(source)) => Err(DatabaseConfigurationError::Io {
                path: target,
                source,
            }),
            Err(error) => Err(DatabaseConfigurationError::Io {
                path: target,
                source: io::Error::other(error.to_string()),
            }),
        }
    }
}

fn temporary_file_name(file_name: &str) -> String {
    format!(
        ".{}.{}.tmp",
        file_name.trim_start_matches('.'),
        Uuid::now_v7()
    )
}

/// Creates a private file and atomically replaces `target` with it, removing `temporary` on every
/// failure path so a partially written database configuration never survives.
fn write_private_file_atomically(target: &Path, temporary: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;

    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        #[cfg(test)]
        maybe_fail_at(AtomicWriteFailure::BeforeSync)?;
        file.sync_all()?;
        drop(file);
        #[cfg(test)]
        maybe_fail_at(AtomicWriteFailure::BeforeRename)?;
        std::fs::rename(temporary, target)?;
        #[cfg(unix)]
        if let Some(parent) = target.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

/// Creates the configuration directory as owner-only when this process has to create it. Existing
/// directories keep their host-provided permissions (Docker and NAS mounts commonly arrive as
/// 0755), except that world-writable directories lose shared write access.
fn prepare_private_config_directory(config_dir: &Path) -> io::Result<()> {
    match std::fs::metadata(config_dir) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "database configuration path is not a directory",
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(config_dir)?;
        }
        Err(error) => return Err(error),
    }
    restrict_shared_write_access(config_dir)
}

#[cfg(unix)]
fn restrict_shared_write_access(config_dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let permissions = std::fs::metadata(config_dir)?.permissions();
    let mode = permissions.mode();
    if mode & 0o002 != 0 && mode & 0o1000 == 0 {
        std::fs::set_permissions(
            config_dir,
            std::fs::Permissions::from_mode((mode & 0o7777) & !0o022),
        )?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_shared_write_access(_config_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum AtomicWriteFailure {
    BeforeSync,
    BeforeRename,
}

#[cfg(test)]
thread_local! {
    static INJECTED_FAILURE: std::cell::Cell<Option<AtomicWriteFailure>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn maybe_fail_at(point: AtomicWriteFailure) -> io::Result<()> {
    if INJECTED_FAILURE.with(std::cell::Cell::get) == Some(point) {
        return Err(io::Error::other(
            "injected database configuration write failure",
        ));
    }
    Ok(())
}

fn validate_text(
    field: &str,
    value: &str,
    max_chars: usize,
) -> Result<(), DatabaseConfigurationError> {
    if value.trim().is_empty()
        || value.chars().count() > max_chars
        || value.chars().any(char::is_control)
    {
        return Err(DatabaseConfigurationError::Invalid(format!("{field} 无效")));
    }
    Ok(())
}

fn validate_identifier(field: &str, value: &str) -> Result<(), DatabaseConfigurationError> {
    validate_text(field, value, 63)?;
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.'))
    {
        return Err(DatabaseConfigurationError::Invalid(format!("{field} 无效")));
    }
    Ok(())
}

#[derive(Debug)]
pub enum DatabaseConfigurationError {
    Io { path: PathBuf, source: io::Error },
    Invalid(String),
}

impl fmt::Display for DatabaseConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "数据库配置 '{}' 操作失败: {source}",
                    path.display()
                )
            }
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for DatabaseConfigurationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Invalid(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::path::Path;

    use tempfile::tempdir;

    use super::{
        AtomicWriteFailure, INJECTED_FAILURE, prepare_private_config_directory,
        write_private_file_atomically,
    };

    #[cfg(unix)]
    fn permissions(path: &Path) -> Result<u32, Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
    }

    #[test]
    fn atomic_write_installs_an_owner_only_file_without_temporary_residue()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let target = directory.path().join("database.json");
        let temporary = directory.path().join(".database.json.test.tmp");

        write_private_file_atomically(&target, &temporary, b"replacement")?;

        assert_eq!(std::fs::read(&target)?, b"replacement");
        assert!(!temporary.exists());
        #[cfg(unix)]
        assert_eq!(permissions(&target)?, 0o600);
        Ok(())
    }

    #[test]
    fn atomic_write_removes_the_temporary_file_when_rename_fails()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let target = directory.path().join("database.json");
        std::fs::create_dir(&target)?;
        let temporary = directory.path().join(".database.json.test.tmp");

        assert!(write_private_file_atomically(&target, &temporary, b"replacement").is_err());

        assert!(target.is_dir());
        assert!(!temporary.exists());
        Ok(())
    }

    fn assert_injected_failure_keeps_previous_file(
        failure: AtomicWriteFailure,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let target = directory.path().join("database.json");
        std::fs::write(&target, b"previous")?;
        let temporary = directory.path().join(".database.json.test.tmp");
        INJECTED_FAILURE.with(|injected| injected.set(Some(failure)));

        let result = write_private_file_atomically(&target, &temporary, b"replacement");
        INJECTED_FAILURE.with(|injected| injected.set(None));

        assert!(result.is_err());
        assert_eq!(std::fs::read(&target)?, b"previous");
        assert!(!temporary.exists());
        Ok(())
    }

    #[test]
    fn atomic_write_cleans_up_after_a_write_failure() -> Result<(), Box<dyn std::error::Error>> {
        assert_injected_failure_keeps_previous_file(AtomicWriteFailure::BeforeSync)
    }

    #[test]
    fn atomic_write_cleans_up_after_a_rename_failure() -> Result<(), Box<dyn std::error::Error>> {
        assert_injected_failure_keeps_previous_file(AtomicWriteFailure::BeforeRename)
    }

    #[cfg(unix)]
    #[test]
    fn new_config_directory_is_owner_only() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let config_dir = directory.path().join("nested").join("config");

        prepare_private_config_directory(&config_dir)?;

        assert_eq!(permissions(&config_dir)?, 0o700);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn existing_shared_config_directory_is_not_rejected() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir()?;
        let config_dir = directory.path().join("config");
        std::fs::create_dir(&config_dir)?;
        std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o755))?;

        prepare_private_config_directory(&config_dir)?;

        assert_eq!(permissions(&config_dir)?, 0o755);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn world_writable_config_directory_is_tightened() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir()?;
        let config_dir = directory.path().join("config");
        std::fs::create_dir(&config_dir)?;
        std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o777))?;

        prepare_private_config_directory(&config_dir)?;

        assert_eq!(permissions(&config_dir)?, 0o755);
        Ok(())
    }
}
