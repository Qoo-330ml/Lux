use std::{fmt, io, path::PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs;
use url::Url;
use uuid::Uuid;

use super::Config;

const DATABASE_CONFIG_FILE: &str = "database.json";

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
        let path = self.config_dir.join(DATABASE_CONFIG_FILE);
        let contents = match fs::read(&path).await {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let legacy_path = self.config_dir.join("lux.db");
                return Ok(fs::metadata(legacy_path)
                    .await
                    .map(|_| DatabaseConfiguration::Sqlite)
                    .ok());
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

    pub async fn save_database_configuration(
        &self,
        configuration: &DatabaseConfiguration,
    ) -> Result<(), DatabaseConfigurationError> {
        configuration.validate()?;
        fs::create_dir_all(&self.config_dir)
            .await
            .map_err(|source| DatabaseConfigurationError::Io {
                path: self.config_dir.clone(),
                source,
            })?;

        let path = self.config_dir.join(DATABASE_CONFIG_FILE);
        let temporary_path = self
            .config_dir
            .join(format!(".database.json.{}.tmp", Uuid::now_v7()));
        let bytes = serde_json::to_vec_pretty(configuration).map_err(|source| {
            DatabaseConfigurationError::Invalid(format!("数据库配置无法序列化: {source}"))
        })?;
        fs::write(&temporary_path, bytes).await.map_err(|source| {
            DatabaseConfigurationError::Io {
                path: temporary_path.clone(),
                source,
            }
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temporary_path, std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(|source| DatabaseConfigurationError::Io {
                    path: temporary_path.clone(),
                    source,
                })?;
        }
        if let Err(source) = fs::rename(&temporary_path, &path).await {
            let _ = fs::remove_file(&temporary_path).await;
            return Err(DatabaseConfigurationError::Io { path, source });
        }
        Ok(())
    }
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
