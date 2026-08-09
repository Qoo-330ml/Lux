use crate::{
    config::{Config, DatabaseBackend, DatabaseConfiguration, DatabaseConfigurationError},
    storage::{Database, StorageError},
};

#[derive(Clone)]
pub struct DatabaseSetupService {
    config: Config,
    current_backend: DatabaseBackend,
}

impl DatabaseSetupService {
    pub fn new(config: Config, current_backend: DatabaseBackend) -> Self {
        Self {
            config,
            current_backend,
        }
    }

    pub async fn status(&self) -> Result<DatabaseSetupStatus, DatabaseSetupError> {
        let selected = self.config.load_explicit_database_configuration().await?;
        Ok(DatabaseSetupStatus {
            configured: selected.is_some(),
            backend: selected.as_ref().map(DatabaseConfiguration::backend),
            current_backend: self.current_backend,
            restart_required: selected
                .as_ref()
                .is_some_and(|configuration| configuration.backend() != self.current_backend),
        })
    }

    pub async fn test(
        &self,
        configuration: &DatabaseConfiguration,
    ) -> Result<(), DatabaseSetupError> {
        Database::test_configuration(configuration)
            .await
            .map_err(DatabaseSetupError::Storage)
    }

    pub async fn select(
        &self,
        configuration: &DatabaseConfiguration,
    ) -> Result<DatabaseSelectionResult, DatabaseSetupError> {
        self.test(configuration).await?;
        self.config
            .save_database_configuration(configuration)
            .await?;
        Ok(DatabaseSelectionResult {
            backend: configuration.backend(),
            restart_required: configuration.backend() != self.current_backend,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseSetupStatus {
    pub configured: bool,
    pub backend: Option<DatabaseBackend>,
    pub current_backend: DatabaseBackend,
    pub restart_required: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DatabaseSelectionResult {
    pub backend: DatabaseBackend,
    pub restart_required: bool,
}

#[derive(Debug)]
pub enum DatabaseSetupError {
    Configuration(DatabaseConfigurationError),
    Storage(StorageError),
}

impl From<DatabaseConfigurationError> for DatabaseSetupError {
    fn from(error: DatabaseConfigurationError) -> Self {
        Self::Configuration(error)
    }
}

impl std::fmt::Display for DatabaseSetupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for DatabaseSetupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Configuration(error) => Some(error),
            Self::Storage(error) => Some(error),
        }
    }
}
