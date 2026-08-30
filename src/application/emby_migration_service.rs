use std::{
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{fs, io::AsyncWriteExt, sync::Mutex};
use uuid::Uuid;

use crate::{
    application::emby_migration::{
        EmbyMigrationPluginClient, EmbyMigrationSource, HistoryCapability, MigrationConnectionInfo,
        MigrationInputError, MigrationItem, MigrationItemPage, MigrationLibraryFolder,
        MigrationMergePolicy, MigrationScope, MigrationSourceFilter, MigrationUser,
        MigrationUserData, MigrationUserPage, MigrationUserStateFilter, StoredItemState,
        requested_fields_for_state_filters,
    },
    application::plugin_runtime::PluginRuntimeError,
    application::plugins::PluginServiceError,
    auth::users::{UserRecord, UserStore, UserStoreError, UserUpdate},
    storage::{
        Database, EmbyMigrationHandledItemBatch, EmbyMigrationImportRecordBatch,
        EmbyMigrationItemMatchBatch, EmbyMigrationItemPageBatch, EmbyMigrationJobProgress,
        EmbyMigrationPersonFavoriteBatch, EmbyMigrationPersonFavoriteStateBatch,
        EmbyMigrationUserItemStateBatch, EmbyMigrationUserItemStateFields,
        MigrationMediaIdentityLookup, MigrationPersonIdentityLookup, NewEmbyMigrationJob,
        StorageError, StoredEmbyMigrationImportRecord, StoredEmbyMigrationItemMatch,
        StoredEmbyMigrationJob, StoredEmbyMigrationPersonFavorite, StoredEmbyMigrationSource,
        StoredEmbyMigrationUserBinding, StoredEmbyMigrationUserLink, StoredMigrationMediaIdentity,
        StoredMigrationPersonIdentity, StoredPlaybackHistoryEvent,
    },
};

const SECRET_DIRECTORY: &str = "plugin-secrets/emby-migration";
const MAX_LABEL_LENGTH: usize = 128;
const MAX_JOB_PAGE_SIZE: i64 = 100;
const MAX_SELECTED_USER_COUNT: usize = 1_000;
const MAX_SELECTED_LIBRARY_COUNT: usize = 1_000;
const MAX_MIGRATION_PAGE_RECOVERY_RPCS: u64 = 32;
const MAX_SOURCE_RATE_LIMIT_RETRIES: u32 = 3;
const SOURCE_RATE_LIMIT_RETRY_BASE_DELAY: Duration = Duration::from_millis(250);
const SOURCE_RATE_LIMIT_RETRY_MAX_DELAY: Duration = Duration::from_secs(4);
const MAX_MEDIA_IDENTITY_CACHE_ENTRIES: usize = 64;
const MAX_MEDIA_IDENTITY_CACHE_IDENTITIES: usize = 8_192;
const MAX_PERSON_IDENTITY_CACHE_ENTRIES: usize = 64;
const MAX_PERSON_IDENTITY_CACHE_IDENTITIES: usize = 8_192;
const MAX_MIGRATION_USER_BINDING_BATCH_SIZE: usize = 100;

struct PreparedMigrationUser {
    link: StoredEmbyMigrationUserLink,
    binding: Option<StoredEmbyMigrationUserBinding>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateMigrationRequest {
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub merge_policy: MigrationMergePolicy,
    #[serde(default)]
    pub scope: MigrationScope,
    pub emby_user_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationJobView {
    pub id: String,
    pub source_label: String,
    pub source_base_url: String,
    pub status: String,
    pub phase: String,
    pub dry_run: bool,
    pub merge_policy: String,
    pub scope: MigrationScope,
    pub history_capability: String,
    pub processed_count: i64,
    pub total_count: i64,
    pub matched_count: i64,
    pub skipped_count: i64,
    pub failed_count: i64,
    pub cancel_requested: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationUserLinkView {
    pub job_id: String,
    pub emby_user_id: String,
    pub emby_username: String,
    pub lux_user_id: Option<String>,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationSourceUserView {
    pub id: String,
    pub name: String,
    pub is_disabled: bool,
    pub is_administrator: bool,
}

impl From<MigrationUser> for MigrationSourceUserView {
    fn from(user: MigrationUser) -> Self {
        Self {
            id: user.id,
            name: user.name,
            is_disabled: user.is_disabled,
            is_administrator: user.is_administrator,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationSourceUserPageView {
    pub users: Vec<MigrationSourceUserView>,
    pub total: i64,
}

impl From<StoredEmbyMigrationUserLink> for MigrationUserLinkView {
    fn from(link: StoredEmbyMigrationUserLink) -> Self {
        Self {
            job_id: link.job_id,
            emby_user_id: link.emby_user_id,
            emby_username: link.emby_username,
            lux_user_id: link.lux_user_id,
            status: link.status,
            error: link.error,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationItemMatchView {
    pub job_id: String,
    pub emby_item_id: String,
    pub emby_item_type: String,
    pub lux_item_id: Option<String>,
    pub match_method: String,
    pub confidence: Option<i64>,
    pub status: String,
    pub detail: serde_json::Value,
}

impl From<StoredEmbyMigrationItemMatch> for MigrationItemMatchView {
    fn from(item: StoredEmbyMigrationItemMatch) -> Self {
        Self {
            job_id: item.job_id,
            emby_item_id: item.emby_item_id,
            emby_item_type: item.emby_item_type,
            lux_item_id: item.lux_item_id,
            match_method: item.match_method,
            confidence: item.confidence,
            status: item.status,
            detail: serde_json::from_str(&item.detail_json).unwrap_or_else(|_| json!({})),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationImportRecordView {
    pub job_id: String,
    pub emby_user_id: String,
    pub emby_item_id: String,
    pub lux_user_id: String,
    pub lux_item_id: String,
    pub state_hash: String,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationPersonFavoriteView {
    pub job_id: String,
    pub emby_user_id: String,
    pub emby_person_id: String,
    pub emby_person_name: String,
    pub lux_user_id: Option<String>,
    pub lux_person_id: Option<String>,
    pub provider_ids: serde_json::Value,
    pub match_method: String,
    pub confidence: Option<i64>,
    pub status: String,
    pub state_hash: String,
    pub detail: serde_json::Value,
    pub error: Option<String>,
}

impl From<StoredEmbyMigrationPersonFavorite> for MigrationPersonFavoriteView {
    fn from(record: StoredEmbyMigrationPersonFavorite) -> Self {
        Self {
            job_id: record.job_id,
            emby_user_id: record.emby_user_id,
            emby_person_id: record.emby_person_id,
            emby_person_name: record.emby_person_name,
            lux_user_id: record.lux_user_id,
            lux_person_id: record.lux_person_id,
            provider_ids: serde_json::from_str(&record.provider_ids_json)
                .unwrap_or_else(|_| json!({})),
            match_method: record.match_method,
            confidence: record.confidence,
            status: record.status,
            state_hash: record.state_hash,
            detail: serde_json::from_str(&record.detail_json).unwrap_or_else(|_| json!({})),
            error: record.error,
        }
    }
}

impl From<StoredEmbyMigrationImportRecord> for MigrationImportRecordView {
    fn from(record: StoredEmbyMigrationImportRecord) -> Self {
        Self {
            job_id: record.job_id,
            emby_user_id: record.emby_user_id,
            emby_item_id: record.emby_item_id,
            lux_user_id: record.lux_user_id,
            lux_item_id: record.lux_item_id,
            state_hash: record.state_hash,
            status: record.status,
            error: record.error,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackHistoryEventView {
    pub id: String,
    pub user_id: String,
    pub item_id: String,
    pub event_type: String,
    pub position_ticks: i64,
    pub duration_ticks: Option<i64>,
    pub occurred_at: i64,
    pub source: String,
    pub source_event_key: String,
}

impl From<StoredPlaybackHistoryEvent> for PlaybackHistoryEventView {
    fn from(event: StoredPlaybackHistoryEvent) -> Self {
        Self {
            id: event.id,
            user_id: event.user_id,
            item_id: event.item_id,
            event_type: event.event_type,
            position_ticks: event.position_ticks,
            duration_ticks: event.duration_ticks,
            occurred_at: event.occurred_at,
            source: event.source,
            source_event_key: event.source_event_key,
        }
    }
}

impl From<StoredEmbyMigrationJob> for MigrationJobView {
    fn from(job: StoredEmbyMigrationJob) -> Self {
        Self {
            id: job.id,
            source_label: job.source_label,
            source_base_url: job.source_base_url,
            status: job.status,
            phase: job.phase,
            dry_run: job.dry_run,
            merge_policy: job.merge_policy,
            scope: migration_scope_from_json(&job.scope_json),
            history_capability: job.history_capability,
            processed_count: job.processed_count,
            total_count: job.total_count,
            matched_count: job.matched_count,
            skipped_count: job.skipped_count,
            failed_count: job.failed_count,
            cancel_requested: job.cancel_requested,
            error: job.error,
        }
    }
}

#[derive(Debug)]
pub enum EmbyMigrationServiceError {
    InvalidInput(MigrationInputError),
    Plugin(PluginServiceError),
    Storage(StorageError),
    User(UserStoreError),
    Io(std::io::Error),
    NotFound,
    InvalidState,
    AlreadyActive,
}

impl fmt::Display for EmbyMigrationServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(error) => error.fmt(formatter),
            Self::Plugin(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::User(error) => error.fmt(formatter),
            Self::Io(error) => write!(formatter, "migration secret storage failed: {error}"),
            Self::NotFound => formatter.write_str("migration job not found"),
            Self::InvalidState => formatter.write_str("migration job is not resumable"),
            Self::AlreadyActive => formatter.write_str("an Emby migration job is already active"),
        }
    }
}

impl std::error::Error for EmbyMigrationServiceError {}

impl From<MigrationInputError> for EmbyMigrationServiceError {
    fn from(error: MigrationInputError) -> Self {
        Self::InvalidInput(error)
    }
}

impl From<PluginServiceError> for EmbyMigrationServiceError {
    fn from(error: PluginServiceError) -> Self {
        Self::Plugin(error)
    }
}

impl From<StorageError> for EmbyMigrationServiceError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<UserStoreError> for EmbyMigrationServiceError {
    fn from(error: UserStoreError) -> Self {
        Self::User(error)
    }
}

impl From<std::io::Error> for EmbyMigrationServiceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Default)]
struct MigrationExecutionGate {
    creation: Mutex<()>,
    queued_jobs: Mutex<HashSet<String>>,
    runner: Mutex<()>,
}

impl MigrationExecutionGate {
    async fn claim(&self, job_id: &str) -> bool {
        self.queued_jobs.lock().await.insert(job_id.to_owned())
    }

    async fn release(&self, job_id: &str) {
        self.queued_jobs.lock().await.remove(job_id);
    }
}

#[derive(Clone)]
pub struct EmbyMigrationService {
    database: Database,
    plugin: EmbyMigrationPluginClient,
    config_dir: PathBuf,
    execution: Arc<MigrationExecutionGate>,
}

#[derive(Debug)]
struct RetriedSourceCall<T> {
    value: T,
    attempts: u64,
    rate_limited_responses: u64,
}

#[derive(Debug)]
struct RetriedSourceCallError {
    error: PluginServiceError,
    attempts: u64,
    rate_limited_responses: u64,
}

/// Retry only source responses explicitly classified as transient rate limits
/// or upstream 5xx failures. The retry budget is deliberately small and the
/// delay is capped so a degraded source cannot stall a migration indefinitely.
async fn retry_rate_limited_source_call<T, F, Fut>(
    mut operation: F,
) -> Result<RetriedSourceCall<T>, RetriedSourceCallError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, PluginServiceError>>,
{
    retry_rate_limited_source_call_with_limit(&mut operation, MAX_SOURCE_RATE_LIMIT_RETRIES).await
}

async fn retry_rate_limited_source_call_with_limit<T, F, Fut>(
    operation: &mut F,
    max_retries: u32,
) -> Result<RetriedSourceCall<T>, RetriedSourceCallError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, PluginServiceError>>,
{
    let mut attempts = 0_u64;
    let mut rate_limited_responses = 0_u64;
    loop {
        attempts += 1;
        match operation().await {
            Ok(value) => {
                return Ok(RetriedSourceCall {
                    value,
                    attempts,
                    rate_limited_responses,
                });
            }
            Err(error) if is_rate_limited_migration_response(&error) => {
                rate_limited_responses += 1;
                if rate_limited_responses > u64::from(max_retries) {
                    return Err(RetriedSourceCallError {
                        error,
                        attempts,
                        rate_limited_responses,
                    });
                }
                tokio::time::sleep(source_rate_limit_retry_delay(rate_limited_responses)).await;
            }
            Err(error) => {
                return Err(RetriedSourceCallError {
                    error,
                    attempts,
                    rate_limited_responses,
                });
            }
        }
    }
}

fn source_rate_limit_retry_delay(retry_number: u64) -> Duration {
    let exponent = retry_number.saturating_sub(1).min(31) as u32;
    let factor = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
    let delay_millis = 250_u64
        .saturating_mul(factor)
        .min(SOURCE_RATE_LIMIT_RETRY_MAX_DELAY.as_millis() as u64);
    SOURCE_RATE_LIMIT_RETRY_BASE_DELAY.max(Duration::from_millis(delay_millis))
}

impl EmbyMigrationService {
    pub fn new(
        database: Database,
        plugins: crate::application::plugins::PluginService,
        config_dir: PathBuf,
    ) -> Self {
        Self {
            database,
            plugin: EmbyMigrationPluginClient::new(plugins),
            config_dir,
            execution: Arc::new(MigrationExecutionGate::default()),
        }
    }

    pub async fn create_job(
        &self,
        created_by_user_id: &str,
        request: CreateMigrationRequest,
    ) -> Result<MigrationJobView, EmbyMigrationServiceError> {
        let emby_user_ids = normalize_selected_user_ids(&request.emby_user_ids)?;
        let scope = normalize_migration_scope(request.scope)?;
        let _creation_guard = self.execution.creation.lock().await;
        if self.database.has_active_emby_migration_job().await? {
            return Err(EmbyMigrationServiceError::AlreadyActive);
        }
        self.validate_target_libraries(&scope).await?;
        let scope_json =
            serde_json::to_string(&scope).map_err(|_| EmbyMigrationServiceError::InvalidState)?;
        let emby_user_ids_json = serde_json::to_string(&emby_user_ids)
            .map_err(|_| EmbyMigrationServiceError::InvalidState)?;
        let source = self.plugin.configured_source().await?;
        let source_url = source.validate()?;
        let source_base_url = source_url.to_string();
        let source_label = source_url
            .host_str()
            .ok_or(MigrationInputError::InvalidSourceUrl)?
            .to_owned();
        if source_label.chars().count() > MAX_LABEL_LENGTH {
            return Err(MigrationInputError::InvalidSourceUrl.into());
        }
        let job_id = Uuid::now_v7().to_string();
        let secret_ref = format!("emby-migration/{job_id}.json");
        self.write_secret(&secret_ref, &source).await?;
        let source = StoredEmbyMigrationSource {
            source_base_url: source_base_url.clone(),
            secret_ref: secret_ref.clone(),
            source_label: source_label.clone(),
            history_capability: "ITEM_STATE".to_owned(),
        };
        if let Err(error) = self.database.upsert_emby_migration_source(&source).await {
            let _ = self.remove_secret(&secret_ref).await;
            return Err(error.into());
        }
        if let Err(error) = self
            .database
            .insert_emby_migration_job(&NewEmbyMigrationJob {
                id: &job_id,
                created_by_user_id,
                source_label: &source_label,
                source_base_url: &source_base_url,
                secret_ref: &secret_ref,
                dry_run: request.dry_run,
                merge_policy: merge_policy_name(request.merge_policy),
                scope_json: &scope_json,
                emby_user_ids_json: &emby_user_ids_json,
            })
            .await
        {
            let _ = self.remove_secret(&secret_ref).await;
            return Err(error.into());
        }
        self.get_job(&job_id).await
    }

    async fn validate_target_libraries(
        &self,
        scope: &MigrationScope,
    ) -> Result<(), EmbyMigrationServiceError> {
        let Some(target_library_ids) = scope.target_library_ids.as_ref() else {
            return Ok(());
        };
        let enabled_library_ids = self
            .database
            .list_enabled_library_ids()
            .await?
            .into_iter()
            .collect::<HashSet<_>>();
        if target_library_ids
            .iter()
            .all(|library_id| enabled_library_ids.contains(library_id))
        {
            Ok(())
        } else {
            Err(MigrationInputError::InvalidIdentifier.into())
        }
    }

    pub async fn get_job(
        &self,
        job_id: &str,
    ) -> Result<MigrationJobView, EmbyMigrationServiceError> {
        self.database
            .find_emby_migration_job(job_id)
            .await?
            .map(MigrationJobView::from)
            .ok_or(EmbyMigrationServiceError::NotFound)
    }

    pub async fn list_jobs(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<MigrationJobView>, EmbyMigrationServiceError> {
        let limit = limit.clamp(1, MAX_JOB_PAGE_SIZE);
        Ok(self
            .database
            .list_emby_migration_jobs(offset.max(0), limit)
            .await?
            .into_iter()
            .map(MigrationJobView::from)
            .collect())
    }

    pub async fn count_jobs(&self) -> Result<i64, EmbyMigrationServiceError> {
        Ok(self.database.count_emby_migration_jobs().await?)
    }

    pub async fn list_user_links(
        &self,
        job_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<MigrationUserLinkView>, EmbyMigrationServiceError> {
        Ok(self
            .database
            .list_emby_migration_user_links(
                job_id,
                offset.max(0),
                limit.clamp(1, MAX_JOB_PAGE_SIZE),
            )
            .await?
            .into_iter()
            .map(MigrationUserLinkView::from)
            .collect())
    }

    pub async fn list_item_matches(
        &self,
        job_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<MigrationItemMatchView>, EmbyMigrationServiceError> {
        Ok(self
            .database
            .list_emby_migration_item_matches(
                job_id,
                offset.max(0),
                limit.clamp(1, MAX_JOB_PAGE_SIZE),
            )
            .await?
            .into_iter()
            .map(MigrationItemMatchView::from)
            .collect())
    }

    pub async fn list_import_records(
        &self,
        job_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<MigrationImportRecordView>, EmbyMigrationServiceError> {
        Ok(self
            .database
            .list_emby_migration_import_records(
                job_id,
                offset.max(0),
                limit.clamp(1, MAX_JOB_PAGE_SIZE),
            )
            .await?
            .into_iter()
            .map(MigrationImportRecordView::from)
            .collect())
    }

    pub async fn list_person_favorite_records(
        &self,
        job_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<MigrationPersonFavoriteView>, EmbyMigrationServiceError> {
        Ok(self
            .database
            .list_emby_migration_person_favorites(
                job_id,
                offset.max(0),
                limit.clamp(1, MAX_JOB_PAGE_SIZE),
            )
            .await?
            .into_iter()
            .map(MigrationPersonFavoriteView::from)
            .collect())
    }

    pub async fn list_playback_history(
        &self,
        user_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<PlaybackHistoryEventView>, EmbyMigrationServiceError> {
        Ok(self
            .database
            .list_playback_history_events(user_id, offset.max(0), limit.clamp(1, MAX_JOB_PAGE_SIZE))
            .await?
            .into_iter()
            .map(PlaybackHistoryEventView::from)
            .collect())
    }

    pub async fn cancel_job(&self, job_id: &str) -> Result<bool, EmbyMigrationServiceError> {
        Ok(self.database.request_emby_migration_cancel(job_id).await?)
    }

    pub async fn resume_job(&self, job_id: &str) -> Result<bool, EmbyMigrationServiceError> {
        let job = self
            .database
            .find_emby_migration_job(job_id)
            .await?
            .ok_or(EmbyMigrationServiceError::NotFound)?;
        if !matches!(job.status.as_str(), "PENDING" | "RUNNING" | "FAILED") {
            return Err(EmbyMigrationServiceError::InvalidState);
        }
        if job.status == "FAILED" {
            self.database
                .update_emby_migration_job_status(job_id, "PENDING", &job.phase, None)
                .await?;
        }
        Ok(true)
    }

    pub async fn test_connection(
        &self,
    ) -> Result<MigrationConnectionInfo, EmbyMigrationServiceError> {
        let source = self.plugin.configured_source().await?;
        Ok(self.plugin.test_connection(&source).await?)
    }

    pub async fn list_source_users(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<MigrationSourceUserPageView, EmbyMigrationServiceError> {
        self.list_source_users_filtered(offset, limit, None).await
    }

    pub async fn list_source_users_filtered(
        &self,
        offset: i64,
        limit: i64,
        search: Option<&str>,
    ) -> Result<MigrationSourceUserPageView, EmbyMigrationServiceError> {
        let source = self.plugin.configured_source().await?;
        let offset = offset.max(0);
        let limit = limit.clamp(1, MAX_JOB_PAGE_SIZE);
        let page = self
            .plugin
            .list_users_page(&source, offset, limit, search)
            .await?;
        Ok(project_source_user_page(page, offset, limit, search))
    }

    pub async fn authenticate_pending_user(
        &self,
        username: &str,
        password: &str,
    ) -> Result<bool, EmbyMigrationServiceError> {
        let Some(binding) = self
            .database
            .find_emby_migration_user_binding_by_username(username)
            .await?
        else {
            return Ok(false);
        };
        let secret_ref = if let Some(secret_ref) = binding.secret_ref.clone() {
            secret_ref
        } else {
            self.database
                .find_emby_migration_source(&binding.source_base_url)
                .await?
                .ok_or(EmbyMigrationServiceError::InvalidState)?
                .secret_ref
        };
        let path = self.config_dir.join("plugin-secrets").join(secret_ref);
        let contents = fs::read(path).await?;
        let source: EmbyMigrationSource = serde_json::from_slice(&contents)
            .map_err(|_| EmbyMigrationServiceError::InvalidState)?;
        let authenticated = self
            .plugin
            .authenticate_user(&source, username, password)
            .await?;
        if !authenticated.authenticated
            || authenticated.user_id.as_deref() != Some(binding.emby_user_id.as_str())
        {
            return Ok(false);
        }
        let user_store = UserStore::new(self.database.clone()).map_err(UserStoreError::from)?;
        user_store
            .update_user(
                &binding.lux_user_id,
                UserUpdate {
                    password: Some(password),
                    ..UserUpdate::default()
                },
            )
            .await?
            .ok_or(EmbyMigrationServiceError::NotFound)?;
        self.database
            .mark_emby_migration_password_ready(&binding.lux_user_id)
            .await?;
        Ok(true)
    }

    pub fn spawn(self: Arc<Self>, job_id: String) {
        tokio::spawn(async move {
            if !self.execution.claim(&job_id).await {
                return;
            }
            let _runner_guard = self.execution.runner.lock().await;
            let result = self.run(&job_id).await;
            drop(_runner_guard);
            self.execution.release(&job_id).await;
            if let Err(error) = result {
                tracing::error!(job_id = %job_id, %error, "Emby migration job stopped");
            }
        });
    }

    async fn write_secret(
        &self,
        secret_ref: &str,
        source: &EmbyMigrationSource,
    ) -> Result<(), EmbyMigrationServiceError> {
        let directory = self.config_dir.join(SECRET_DIRECTORY);
        fs::create_dir_all(&directory).await?;
        let relative_path = PathBuf::from(secret_ref);
        let path = self.config_dir.join("plugin-secrets").join(&relative_path);
        let temporary = path.with_extension(format!("tmp-{}", Uuid::now_v7()));
        let contents = serde_json::to_vec(&json!({
            "baseUrl": source.base_url,
            "apiKey": source.api_key,
            "allowPrivateNetwork": source.allow_private_network,
        }))
        .map_err(|_| EmbyMigrationServiceError::InvalidState)?;
        let mut file = fs::File::create(&temporary).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = file.metadata().await?.permissions();
            permissions.set_mode(0o600);
            fs::set_permissions(&temporary, permissions).await?;
        }
        file.write_all(&contents).await?;
        file.sync_all().await?;
        drop(file);
        fs::rename(&temporary, &path).await?;
        Ok(())
    }

    async fn remove_secret(&self, secret_ref: &str) -> Result<(), std::io::Error> {
        let path = self.config_dir.join("plugin-secrets").join(secret_ref);
        match fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn run(&self, job_id: &str) -> Result<(), EmbyMigrationServiceError> {
        let run_started = Instant::now();
        let mut source_rpc_calls = 0_u64;
        let mut source_rate_limited_rpc_calls = 0_u64;
        let mut database_transactions = 0_u64;
        let mut peak_source_page_records = 0_usize;
        let mut candidate_query_ms = 0_u128;
        let mut database_write_ms = 0_u128;
        let mut prefetched_source_pages = 0_u64;
        let mut consumed_prefetched_source_pages = 0_u64;
        let mut source_read_ms = 0_u128;
        let Some(job) = self.database.find_emby_migration_job(job_id).await? else {
            return Err(EmbyMigrationServiceError::NotFound);
        };
        if matches!(job.status.as_str(), "COMPLETED" | "CANCELLED") {
            return Ok(());
        }
        let source = self.read_source(&job).await?;
        let scope = migration_scope_from_json(&job.scope_json);
        let state_fields = state_fields_for_migration(&scope);
        let requested_state_fields =
            requested_fields_for_state_filters(scope.selected_item_state_filters());
        let requested_user_fields = user_fields_for_migration(&scope);
        let testing_started = Instant::now();
        self.database
            .update_emby_migration_job_status(job_id, "RUNNING", "TESTING", None)
            .await?;
        let connection =
            match retry_rate_limited_source_call(|| self.plugin.test_connection(&source)).await {
                Ok(result) => {
                    source_rpc_calls += result.attempts;
                    source_rate_limited_rpc_calls += result.rate_limited_responses;
                    result.value
                }
                Err(error) => {
                    tracing::warn!(
                        job_id = %job_id,
                        phase = "TESTING",
                        source_rpc_attempts = error.attempts,
                        source_rate_limited_rpc_calls = error.rate_limited_responses,
                        "Emby migration source call failed after bounded retries"
                    );
                    self.fail_job(job_id, "TESTING", &error.error.to_string())
                        .await?;
                    return Err(error.error.into());
                }
            };
        self.database
            .upsert_emby_migration_source(&StoredEmbyMigrationSource {
                source_base_url: job.source_base_url.clone(),
                secret_ref: job.secret_ref.clone(),
                source_label: job.source_label.clone(),
                history_capability: history_capability_name(connection.history_capability)
                    .to_owned(),
            })
            .await?;
        self.database
            .update_emby_migration_job_history_capability(
                job_id,
                history_capability_name(connection.history_capability),
            )
            .await?;
        let testing_ms = testing_started.elapsed().as_millis();
        if self.is_cancelled(job_id).await? {
            return self.cancelled(job_id, "TESTING").await;
        }

        let users_started = Instant::now();
        self.database
            .update_emby_migration_job_status(job_id, "RUNNING", "USERS", None)
            .await?;
        let selected_user_ids = selected_migration_user_ids(&job.emby_user_ids_json)?;
        let selected_user_ids_for_source = connection
            .supports_filtered_reads
            .then_some(selected_user_ids.as_slice());
        let requested_user_fields_for_source = connection
            .supports_filtered_reads
            .then_some(requested_user_fields.as_slice());
        let user_page = match retry_rate_limited_source_call(|| {
            self.plugin.list_users_filtered_with_fields(
                &source,
                selected_user_ids_for_source,
                requested_user_fields_for_source,
            )
        })
        .await
        {
            Ok(result) => {
                source_rpc_calls += result.attempts;
                source_rate_limited_rpc_calls += result.rate_limited_responses;
                result.value
            }
            Err(error) => {
                tracing::warn!(
                    job_id = %job_id,
                    phase = "USERS",
                    source_rpc_attempts = error.attempts,
                    source_rate_limited_rpc_calls = error.rate_limited_responses,
                    "Emby migration source call failed after bounded retries"
                );
                self.fail_job(job_id, "USERS", &error.error.to_string())
                    .await?;
                return Err(error.error.into());
            }
        };
        let library_folders = user_page.library_folders;
        let users = match select_migration_users(user_page.items, &job.emby_user_ids_json) {
            Ok(users) => users,
            Err(error) => {
                self.fail_job(
                    job_id,
                    "USERS",
                    "selected Emby users are no longer available",
                )
                .await?;
                return Err(error);
            }
        };
        let user_store = UserStore::new(self.database.clone()).map_err(UserStoreError::from)?;
        let mut source_usernames = users
            .iter()
            .filter_map(|user| {
                let name = user.name.trim();
                (!name.is_empty()).then(|| name.to_lowercase())
            })
            .collect::<Vec<_>>();
        source_usernames.sort_unstable();
        source_usernames.dedup();
        let mut lux_users_by_username = if job.dry_run {
            HashMap::new()
        } else {
            user_store
                .list_by_normalized_usernames(&source_usernames)
                .await?
                .into_iter()
                .map(|user| (user.username_normalized.clone(), user))
                .collect::<HashMap<_, _>>()
        };
        let mut user_links = Vec::with_capacity(users.len());
        let mut user_links_reports = Vec::with_capacity(users.len());
        let mut pending_user_bindings = Vec::with_capacity(MAX_MIGRATION_USER_BINDING_BATCH_SIZE);
        for user in &users {
            if self.is_cancelled(job_id).await? {
                self.flush_user_bindings(&mut pending_user_bindings, &mut database_write_ms)
                    .await?;
                return self.cancelled(job_id, "USERS").await;
            }
            let write_started = Instant::now();
            let prepared = self
                .prepare_user(
                    &user_store,
                    &mut lux_users_by_username,
                    &job,
                    user,
                    scope.user_profile,
                )
                .await?;
            database_write_ms += write_started.elapsed().as_millis();
            user_links.push((user.clone(), prepared.link.lux_user_id.clone()));
            user_links_reports.push(prepared.link);
            if let Some(binding) = prepared.binding {
                pending_user_bindings.push(binding);
                if pending_user_bindings.len() >= MAX_MIGRATION_USER_BINDING_BATCH_SIZE {
                    self.flush_user_bindings(&mut pending_user_bindings, &mut database_write_ms)
                        .await?;
                }
            }
        }
        self.flush_user_bindings(&mut pending_user_bindings, &mut database_write_ms)
            .await?;
        let write_started = Instant::now();
        self.database
            .upsert_emby_migration_user_links_batch(&user_links_reports)
            .await?;
        database_write_ms += write_started.elapsed().as_millis();
        let users_ms = users_started.elapsed().as_millis();

        let items_started = Instant::now();
        self.database
            .update_emby_migration_job_status(job_id, "RUNNING", "ITEMS", None)
            .await?;
        let target_library_ids = scope
            .target_library_ids
            .as_ref()
            .map(|ids| ids.iter().cloned().collect::<HashSet<_>>());
        let target_library_filter = target_library_ids.as_ref().map(|ids| {
            let mut ids = ids.iter().cloned().collect::<Vec<_>>();
            ids.sort_unstable();
            ids
        });
        // New jobs normally carry an explicit target whitelist.  Legacy jobs
        // may omit it; for those, a restricted source user can still be
        // safely narrowed to the Lux libraries that their source folders map
        // to.  Users with access to all source folders keep the legacy
        // unfiltered request when no explicit whitelist exists, because an
        // incomplete folder mapping must not hide a valid item.
        let source_filtering_enabled = migration_source_filtering_enabled(
            connection.supports_filtered_reads,
            &scope,
            target_library_ids.as_ref(),
            &users,
        );
        let lux_library_identities =
            if (scope.library_access || scope.item_state || scope.person_favorites)
                && library_folders.is_some()
                && (users.iter().any(|user| !user.enable_all_folders) || source_filtering_enabled)
            {
                Some(self.load_library_identities().await?)
            } else {
                None
            };
        // The library identity load already reads enabled library rows. Reuse
        // those IDs instead of issuing a second identical query.
        let enabled_library_ids = if let Some(libraries) = lux_library_identities.as_deref() {
            Some(libraries.iter().map(|library| library.id.clone()).collect())
        } else if let Some(target_library_ids) = target_library_ids.as_ref() {
            // Target IDs have already been validated against enabled libraries when the job
            // was created.  Reusing that whitelist avoids a second enabled-library scan for
            // migrations that are explicitly scoped to selected targets.
            Some(target_library_ids.iter().cloned().collect())
        } else if scope.library_access || scope.item_state {
            Some(self.database.list_enabled_library_ids().await?)
        } else {
            None
        };
        let enabled_library_id_set = enabled_library_ids
            .as_ref()
            .map(|ids| ids.iter().cloned().collect::<HashSet<_>>());
        // Resolve each source virtual folder once per migration.  The old
        // per-user path normalized every folder and Lux root repeatedly when
        // calculating both the ACL plan and the source-side filter.
        let source_library_mappings = library_folders.as_deref().map(|source_folders| {
            precompute_source_library_mappings(
                source_folders,
                lux_library_identities.as_deref().unwrap_or_default(),
            )
        });
        let mut processed = job.processed_count;
        let mut matched = job.matched_count;
        let mut skipped = job.skipped_count;
        let mut failed = job.failed_count;
        let mut total = job.total_count.max(users.len() as i64);
        let mut media_identity_cache = MigrationMediaIdentityCache::new(
            MAX_MEDIA_IDENTITY_CACHE_ENTRIES,
            MAX_MEDIA_IDENTITY_CACHE_IDENTITIES,
        );
        let mut media_identity_cache_hits = 0_u64;
        let mut person_identity_cache = MigrationPersonIdentityCache::new(
            MAX_PERSON_IDENTITY_CACHE_ENTRIES,
            MAX_PERSON_IDENTITY_CACHE_IDENTITIES,
        );
        let mut person_identity_cache_hits = 0_u64;
        let user_ids = user_links
            .iter()
            .map(|(user, _)| user.id.clone())
            .collect::<Vec<_>>();
        let resume_cursor = MigrationResumeCursor::parse(&job.cursor_json);
        let resume_user_index = resume_cursor.as_ref().and_then(|cursor| {
            cursor
                .user_id
                .as_deref()
                .and_then(|user_id| user_ids.iter().position(|id| id == user_id))
        });
        for (user_index, (user, lux_user_id)) in user_links.into_iter().enumerate() {
            if resume_user_index.is_some_and(|resume_index| user_index < resume_index) {
                continue;
            }
            // A new job starts with an empty handled-item table, and the in-memory set below
            // already de-duplicates overlapping PLAYED/FAVORITE/RESUMABLE pages.  Only a
            // resumed/partially processed user needs the durable lookup that protects against
            // source pagination changing between attempts.
            let lookup_handled_items = should_lookup_handled_items(
                resume_cursor.as_ref(),
                job.processed_count,
                resume_user_index,
                user_index,
            );
            let library_access_plan = if scope.library_access || scope.item_state {
                if user.enable_all_folders {
                    LibraryAccessPlan::Exact(
                        enabled_library_ids
                            .as_deref()
                            .unwrap_or_default()
                            .iter()
                            .cloned()
                            .collect(),
                    )
                } else if let Some(source_library_mappings) = source_library_mappings.as_deref() {
                    match map_enabled_library_ids_checked_with_mappings(
                        &user,
                        source_library_mappings,
                    ) {
                        Some(ids) => LibraryAccessPlan::Exact(ids),
                        None => LibraryAccessPlan::Unavailable,
                    }
                } else {
                    LibraryAccessPlan::Derived
                }
            } else {
                LibraryAccessPlan::Disabled
            };
            let library_access_plan =
                restrict_library_access_plan(library_access_plan, target_library_ids.as_ref());
            let source_library_ids = if source_filtering_enabled
                && (target_library_ids.is_some() || !user.enable_all_folders)
            {
                target_library_ids
                    .as_ref()
                    .or(enabled_library_id_set.as_ref())
                    .and_then(|selected_library_ids| {
                        source_library_filter_for_user(
                            &user,
                            source_library_mappings.as_deref()?,
                            selected_library_ids,
                        )
                    })
            } else {
                None
            };
            let mut seen_emby_item_ids = HashSet::new();
            let source_state_scope_has_candidates = source_filter_has_candidates(
                source_filtering_enabled,
                source_library_ids.as_deref(),
            );
            if scope.item_state && source_state_scope_has_candidates {
                for state_filter in scope.selected_item_state_filters().iter().copied() {
                    if let Some(cursor) = resume_cursor.as_ref() {
                        if cursor.user_id.as_deref() == Some(user.id.as_str()) {
                            if cursor.is_person_favorites() {
                                break;
                            }
                            if cursor.kind.as_deref() == Some("USER_STATE") {
                                let cursor_filter_index = cursor.state_filter.and_then(|filter| {
                                    scope
                                        .selected_item_state_filters()
                                        .iter()
                                        .position(|candidate| *candidate == filter)
                                });
                                let filter_index = scope
                                    .selected_item_state_filters()
                                    .iter()
                                    .position(|candidate| *candidate == state_filter)
                                    .unwrap_or_default();
                                if cursor_filter_index.is_some_and(|index| filter_index < index) {
                                    continue;
                                }
                            }
                        }
                    }
                    let filter_base = processed;
                    let mut filter_total_recorded = false;
                    let mut start_index = resume_cursor
                        .as_ref()
                        .filter(|cursor| {
                            cursor.user_id.as_deref() == Some(user.id.as_str())
                                && cursor.is_state(state_filter)
                        })
                        .map(|cursor| cursor.start_index)
                        .unwrap_or_default();
                    let mut prefetched_page: Option<MigrationPagePrefetch> = None;
                    loop {
                        if self.is_cancelled(job_id).await? {
                            drop(prefetched_page.take());
                            return self.cancelled(job_id, "ITEMS").await;
                        }
                        let recovered_page = if let Some(prefetched_page) = prefetched_page.take() {
                            consumed_prefetched_source_pages += 1;
                            prefetched_page.join().await?
                        } else {
                            self.recover_migration_page(
                                &source,
                                &user.id,
                                start_index,
                                500,
                                MigrationPageKind::UserState(state_filter),
                                MigrationSourceFilter {
                                    library_ids: source_library_ids.as_deref(),
                                    enabled: source_filtering_enabled,
                                    state_fields: Some(requested_state_fields.as_slice()),
                                },
                            )
                            .await?
                        };
                        source_rpc_calls += recovered_page.source_rpc_calls;
                        source_rate_limited_rpc_calls +=
                            recovered_page.source_rate_limited_rpc_calls;
                        source_read_ms += recovered_page.source_read_ms;
                        if !recovered_page.invalid_items.is_empty() {
                            let invalid_item_count = recovered_page
                                .invalid_items
                                .iter()
                                .map(|invalid| i64::from(invalid.range_limit))
                                .sum::<i64>();
                            processed += invalid_item_count;
                            failed += invalid_item_count;
                            tracing::warn!(
                                job_id = %job_id,
                                user_id = %user.id,
                                start_index,
                                invalid_items = invalid_item_count,
                                "skipping invalid Emby migration items and continuing"
                            );
                        }
                        let page = recovered_page.page;
                        let next_prefetched_page = should_prefetch_source_page(
                            recovered_page.source_rate_limited_rpc_calls,
                        )
                        .then(|| {
                            page.next_start_index
                                .filter(|next_start_index| *next_start_index > start_index)
                                .map(|next_start_index| {
                                    self.prefetch_migration_page(
                                        &source,
                                        &user.id,
                                        next_start_index,
                                        MigrationPageKind::UserState(state_filter),
                                        MigrationSourceFilter {
                                            library_ids: source_library_ids.as_deref(),
                                            enabled: source_filtering_enabled,
                                            state_fields: Some(requested_state_fields.as_slice()),
                                        },
                                    )
                                })
                        })
                        .flatten();
                        prefetched_source_pages += u64::from(next_prefetched_page.is_some());
                        peak_source_page_records = peak_source_page_records.max(page.items.len());
                        // Deduplicate before querying Lux.  The same source item commonly
                        // appears in more than one state-filter page; querying identities first
                        // would repeat the exact same provider/title lookup before the duplicate
                        // is discarded.
                        let mut state_items =
                            collect_recorded_state_items(page.items, &mut seen_emby_item_ids);
                        if !filter_total_recorded {
                            if let Some(page_total) = page.total_record_count {
                                total = total.max(filter_base + page_total as i64);
                                filter_total_recorded = true;
                            }
                        }
                        let mut item_matches = Vec::with_capacity(
                            state_items.len() + recovered_page.invalid_items.len(),
                        );
                        let mut states = Vec::new();
                        let mut import_records = Vec::new();
                        let mut page_item_ids = state_items
                            .iter()
                            .map(|item| item.id.clone())
                            .collect::<Vec<_>>();
                        page_item_ids.sort_unstable();
                        page_item_ids.dedup();
                        let already_handled_item_ids = if lookup_handled_items {
                            self.database
                                .list_emby_migration_handled_item_ids(
                                    job_id,
                                    &user.id,
                                    &page_item_ids,
                                )
                                .await?
                                .into_iter()
                                .collect::<HashSet<_>>()
                        } else {
                            HashSet::new()
                        };
                        // A resumed page may contain records that were committed before the
                        // previous attempt stopped.  Remove them before candidate lookup so a
                        // retry does not re-read Lux identities for work that will be skipped.
                        state_items =
                            retain_unhandled_state_items(state_items, &already_handled_item_ids);
                        let identity_index = if !state_items.is_empty() {
                            let candidate_started = Instant::now();
                            let identity_index = Some(
                                self.load_media_identity_index_cached(
                                    &state_items,
                                    target_library_filter.as_deref(),
                                    &mut media_identity_cache,
                                    &mut media_identity_cache_hits,
                                )
                                .await?,
                            );
                            candidate_query_ms += candidate_started.elapsed().as_millis();
                            identity_index
                        } else {
                            None
                        };
                        let mut handled_items = Vec::new();
                        for invalid in &recovered_page.invalid_items {
                            item_matches.push(EmbyMigrationItemMatchBatch {
                                emby_item_id: invalid_item_report_id(invalid),
                                emby_item_type: "UNKNOWN".to_owned(),
                                lux_item_id: None,
                                match_method: "UNMATCHED".to_owned(),
                                confidence: None,
                                status: "SKIPPED".to_owned(),
                                detail_json: invalid_item_report_detail(invalid),
                            });
                        }
                        for item in state_items {
                            let Some(user_data) = item.user_data.as_ref() else {
                                continue;
                            };
                            let Some(identity_index) = identity_index.as_ref() else {
                                continue;
                            };
                            handled_items.push(EmbyMigrationHandledItemBatch {
                                emby_user_id: user.id.clone(),
                                emby_item_id: item.id.clone(),
                            });
                            processed += 1;
                            let outcome = match_item(&item, identity_index);
                            let target_library_id = outcome
                                .lux_item_id
                                .as_deref()
                                .and_then(|item_id| identity_index.library_id(item_id));
                            let mut detail = migration_item_detail(&item, &outcome, identity_index);
                            let target_library_allowed = target_library_is_selected(
                                target_library_ids.as_ref(),
                                target_library_id.as_deref(),
                            );
                            let source_library_allowed =
                                library_access_plan.allows(target_library_id.as_deref());
                            let status = if outcome.lux_item_id.is_some() && !target_library_allowed
                            {
                                detail["migrationSkipReason"] = json!("TARGET_LIBRARY_EXCLUDED");
                                "SKIPPED"
                            } else if outcome.lux_item_id.is_some() && !source_library_allowed {
                                detail["migrationSkipReason"] =
                                    json!("SOURCE_LIBRARY_ACCESS_DENIED");
                                "SKIPPED"
                            } else {
                                outcome.status
                            };
                            let detail_json =
                                serde_json::to_string(&detail).unwrap_or_else(|_| "{}".to_owned());
                            item_matches.push(EmbyMigrationItemMatchBatch {
                                emby_item_id: item.id.clone(),
                                emby_item_type: item.item_type.clone(),
                                lux_item_id: outcome.lux_item_id.clone(),
                                match_method: outcome.method.to_owned(),
                                confidence: outcome.confidence,
                                status: status.to_owned(),
                                detail_json,
                            });
                            let Some(lux_item_id) = outcome.lux_item_id else {
                                skipped += 1;
                                continue;
                            };
                            if !target_library_allowed || !source_library_allowed {
                                skipped += 1;
                                continue;
                            }
                            matched += 1;
                            if job.dry_run {
                                continue;
                            }
                            let Some(lux_user_id) = lux_user_id.as_deref() else {
                                skipped += 1;
                                continue;
                            };
                            let incoming = incoming_state(user_data)?;
                            states.push(EmbyMigrationUserItemStateBatch {
                                user_id: lux_user_id.to_owned(),
                                item_id: lux_item_id.clone(),
                                position_ticks: incoming.position_ticks,
                                is_played: incoming.is_played,
                                is_favorite: incoming.is_favorite,
                                play_count: incoming.play_count,
                                last_played_at: incoming.last_played_at,
                            });
                            let state_hash = hex_selected_item_state(&incoming, state_fields)?;
                            import_records.push(EmbyMigrationImportRecordBatch {
                                emby_user_id: user.id.clone(),
                                emby_item_id: item.id.clone(),
                                lux_user_id: lux_user_id.to_owned(),
                                lux_item_id,
                                state_hash,
                                status: "IMPORTED".to_owned(),
                                error: None,
                            });
                        }
                        let next_cursor = next_migration_cursor(
                            &user_ids,
                            user_index,
                            MigrationPageKind::UserState(state_filter),
                            Some(state_filter),
                            page.next_start_index,
                            &scope,
                        );
                        let cursor_json = migration_cursor_json(next_cursor);
                        let write_started = Instant::now();
                        self.database
                            .commit_emby_migration_item_page(EmbyMigrationItemPageBatch {
                                job_id,
                                merge_policy: &job.merge_policy,
                                state_fields,
                                item_matches: &item_matches,
                                states: &states,
                                import_records: &import_records,
                                handled_items: &handled_items,
                                progress: EmbyMigrationJobProgress {
                                    id: job_id,
                                    cursor_json: &cursor_json,
                                    processed_count: processed,
                                    total_count: total,
                                    matched_count: matched,
                                    skipped_count: skipped,
                                    failed_count: failed,
                                },
                            })
                            .await?;
                        database_write_ms += write_started.elapsed().as_millis();
                        database_transactions += 1;
                        let Some(next_start_index) = page.next_start_index else {
                            break;
                        };
                        if next_start_index <= start_index {
                            break;
                        }
                        start_index = next_start_index;
                        prefetched_page = next_prefetched_page;
                    }
                }
            }
            if !job.dry_run && scope.library_access {
                if let Some(lux_user_id) = lux_user_id.as_deref() {
                    match &library_access_plan {
                        LibraryAccessPlan::Exact(allowed_library_ids) => {
                            let library_updates = enabled_library_ids
                                .as_deref()
                                .unwrap_or_default()
                                .iter()
                                .filter(|library_id| {
                                    target_library_is_selected(
                                        target_library_ids.as_ref(),
                                        Some(library_id),
                                    )
                                })
                                .map(|library_id| {
                                    (library_id.clone(), allowed_library_ids.contains(library_id))
                                })
                                .collect::<Vec<_>>();
                            let write_started = Instant::now();
                            self.database
                                .set_user_library_access_batch(lux_user_id, &library_updates)
                                .await?;
                            database_write_ms += write_started.elapsed().as_millis();
                        }
                        LibraryAccessPlan::Derived => {
                            let imported_library_ids = self
                                .database
                                .list_emby_migration_imported_library_ids(job_id, &user.id)
                                .await?;
                            let library_updates = imported_library_ids
                                .into_iter()
                                .filter(|library_id| {
                                    target_library_is_selected(
                                        target_library_ids.as_ref(),
                                        Some(library_id),
                                    )
                                })
                                .map(|library_id| (library_id, true))
                                .collect::<Vec<_>>();
                            let write_started = Instant::now();
                            self.database
                                .set_user_library_access_batch(lux_user_id, &library_updates)
                                .await?;
                            database_write_ms += write_started.elapsed().as_millis();
                        }
                        LibraryAccessPlan::Disabled | LibraryAccessPlan::Unavailable => {}
                    }
                }
            }

            if scope.person_favorites
                && source_filter_has_candidates(
                    source_filtering_enabled,
                    source_library_ids.as_deref(),
                )
            {
                let person_filter_base = processed;
                let mut person_total_recorded = false;
                let mut start_index = resume_cursor
                    .as_ref()
                    .filter(|cursor| {
                        cursor.user_id.as_deref() == Some(user.id.as_str())
                            && cursor.is_person_favorites()
                    })
                    .map(|cursor| cursor.start_index)
                    .unwrap_or_default();
                let mut prefetched_page: Option<MigrationPagePrefetch> = None;
                loop {
                    if self.is_cancelled(job_id).await? {
                        drop(prefetched_page.take());
                        return self.cancelled(job_id, "ITEMS").await;
                    }
                    let recovered_page = if let Some(prefetched_page) = prefetched_page.take() {
                        consumed_prefetched_source_pages += 1;
                        prefetched_page.join().await?
                    } else {
                        self.recover_migration_page(
                            &source,
                            &user.id,
                            start_index,
                            500,
                            MigrationPageKind::PersonFavorites,
                            MigrationSourceFilter {
                                library_ids: source_library_ids.as_deref(),
                                enabled: source_filtering_enabled,
                                state_fields: None,
                            },
                        )
                        .await?
                    };
                    source_rpc_calls += recovered_page.source_rpc_calls;
                    source_rate_limited_rpc_calls += recovered_page.source_rate_limited_rpc_calls;
                    source_read_ms += recovered_page.source_read_ms;
                    if !recovered_page.invalid_items.is_empty() {
                        let invalid_item_count = recovered_page
                            .invalid_items
                            .iter()
                            .map(|invalid| i64::from(invalid.range_limit))
                            .sum::<i64>();
                        processed += invalid_item_count;
                        failed += invalid_item_count;
                        tracing::warn!(
                            job_id = %job_id,
                            user_id = %user.id,
                            start_index,
                            invalid_items = invalid_item_count,
                            "skipping invalid Emby migration items and continuing"
                        );
                    }
                    let page = recovered_page.page;
                    let next_prefetched_page =
                        should_prefetch_source_page(recovered_page.source_rate_limited_rpc_calls)
                            .then(|| {
                                page.next_start_index
                                    .filter(|next_start_index| *next_start_index > start_index)
                                    .map(|next_start_index| {
                                        self.prefetch_migration_page(
                                            &source,
                                            &user.id,
                                            next_start_index,
                                            MigrationPageKind::PersonFavorites,
                                            MigrationSourceFilter {
                                                library_ids: source_library_ids.as_deref(),
                                                enabled: source_filtering_enabled,
                                                state_fields: None,
                                            },
                                        )
                                    })
                            })
                            .flatten();
                    prefetched_source_pages += u64::from(next_prefetched_page.is_some());
                    peak_source_page_records = peak_source_page_records.max(page.items.len());
                    if !person_total_recorded {
                        if let Some(page_total) = page.total_record_count {
                            total = total.max(person_filter_base + page_total as i64);
                            person_total_recorded = true;
                        }
                    }
                    let mut favorites = Vec::with_capacity(page.items.len());
                    let mut favorite_states = Vec::new();
                    favorites.extend(
                        recovered_page
                            .invalid_items
                            .iter()
                            .map(invalid_person_favorite_report),
                    );
                    let people = page
                        .items
                        .into_iter()
                        .filter(is_migratable_person_favorite)
                        .collect::<Vec<_>>();
                    let person_identity_index = if people.is_empty() {
                        MigrationPersonIdentityIndex::new(Vec::new())
                    } else {
                        let lookups = migration_person_identity_lookups(&people);
                        let cache_key = MigrationPersonIdentityCacheKey { lookups };
                        if let Some(index) = person_identity_cache.get(&cache_key) {
                            person_identity_cache_hits += 1;
                            index.clone()
                        } else {
                            let candidate_started = Instant::now();
                            let identities = self
                                .database
                                .list_migration_person_identity_candidates(&cache_key.lookups)
                                .await?;
                            candidate_query_ms += candidate_started.elapsed().as_millis();
                            let index = MigrationPersonIdentityIndex::new(identities);
                            person_identity_cache.insert(cache_key, index.clone());
                            index
                        }
                    };
                    for person in people {
                        let user_data = person.user_data.clone().unwrap_or(MigrationUserData {
                            playback_position_ticks: 0,
                            played: false,
                            is_favorite: true,
                            play_count: 0,
                            last_played_date: None,
                        });
                        processed += 1;
                        let outcome = self.match_person(&person, &person_identity_index);
                        let provider_ids_json = serde_json::to_string(&person.provider_ids)
                            .unwrap_or_else(|_| "{}".to_owned());
                        let detail_json = serde_json::to_string(&json!({
                            "sourceName": person.name,
                            "sourceType": "Person",
                            "providerIds": person.provider_ids,
                            "matchMethod": outcome.method,
                        }))
                        .unwrap_or_else(|_| "{}".to_owned());
                        let state_hash = hex_user_data_sha256(&user_data)?;
                        let mut status = outcome.status;
                        let mut error = None;
                        if outcome.lux_person_id.is_some() {
                            matched += 1;
                            if !job.dry_run {
                                if let Some(lux_user_id) = lux_user_id.as_deref() {
                                    if migration_merge_policy(&job.merge_policy)
                                        == MigrationMergePolicy::Skip
                                    {
                                        status = "SKIPPED";
                                    } else {
                                        let Some(lux_person_id) = outcome.lux_person_id.as_deref()
                                        else {
                                            return Err(EmbyMigrationServiceError::InvalidState);
                                        };
                                        favorite_states.push(
                                            EmbyMigrationPersonFavoriteStateBatch {
                                                user_id: lux_user_id.to_owned(),
                                                person_id: lux_person_id.to_owned(),
                                            },
                                        );
                                        status = "IMPORTED";
                                    }
                                } else {
                                    status = "SKIPPED";
                                    error = Some("no Lux user mapping".to_owned());
                                }
                            }
                        } else {
                            skipped += 1;
                        }
                        favorites.push(EmbyMigrationPersonFavoriteBatch {
                            emby_user_id: user.id.clone(),
                            emby_person_id: person.id.clone(),
                            emby_person_name: person.name.clone(),
                            lux_user_id: lux_user_id.clone(),
                            lux_person_id: outcome.lux_person_id.clone(),
                            provider_ids_json,
                            match_method: outcome.method.to_owned(),
                            confidence: outcome.confidence,
                            status: status.to_owned(),
                            state_hash,
                            detail_json,
                            error,
                        });
                    }
                    let next_cursor = next_migration_cursor(
                        &user_ids,
                        user_index,
                        MigrationPageKind::PersonFavorites,
                        None,
                        page.next_start_index,
                        &scope,
                    );
                    let cursor_json = migration_cursor_json(next_cursor);
                    let write_started = Instant::now();
                    self.database
                        .commit_emby_migration_person_page(
                            job_id,
                            &favorites,
                            &favorite_states,
                            &EmbyMigrationJobProgress {
                                id: job_id,
                                cursor_json: &cursor_json,
                                processed_count: processed,
                                total_count: total,
                                matched_count: matched,
                                skipped_count: skipped,
                                failed_count: failed,
                            },
                        )
                        .await?;
                    database_write_ms += write_started.elapsed().as_millis();
                    database_transactions += 1;
                    let Some(next_start_index) = page.next_start_index else {
                        break;
                    };
                    if next_start_index <= start_index {
                        break;
                    }
                    start_index = next_start_index;
                    prefetched_page = next_prefetched_page;
                }
            }
        }
        let items_ms = items_started.elapsed().as_millis();
        let finalizing_started = Instant::now();
        self.database
            .update_emby_migration_job_status(job_id, "RUNNING", "FINALIZING", None)
            .await?;
        self.database
            .update_emby_migration_job_progress(&EmbyMigrationJobProgress {
                id: job_id,
                cursor_json: "{}",
                processed_count: processed,
                total_count: total,
                matched_count: matched,
                skipped_count: skipped,
                failed_count: failed,
            })
            .await?;
        self.database
            .update_emby_migration_job_status(job_id, "COMPLETED", "FINALIZING", None)
            .await?;
        let finalizing_ms = finalizing_started.elapsed().as_millis();
        tracing::info!(
            job_id = %job_id,
            source_rpc_calls,
            source_rate_limited_rpc_calls,
            database_transactions,
            peak_source_page_records,
            testing_ms,
            users_ms,
            items_ms,
            finalizing_ms,
            candidate_query_ms,
            media_identity_cache_hits,
            person_identity_cache_hits,
            database_write_ms,
            prefetched_source_pages,
            consumed_prefetched_source_pages,
            source_read_ms,
            processed,
            matched,
            skipped,
            failed,
            elapsed_ms = run_started.elapsed().as_millis(),
            "Emby migration completed with bounded page batching"
        );
        Ok(())
    }

    async fn read_source(
        &self,
        job: &StoredEmbyMigrationJob,
    ) -> Result<EmbyMigrationSource, EmbyMigrationServiceError> {
        let path = self.config_dir.join("plugin-secrets").join(&job.secret_ref);
        let contents = fs::read(path).await?;
        serde_json::from_slice(&contents).map_err(|_| EmbyMigrationServiceError::InvalidState)
    }

    async fn prepare_user(
        &self,
        user_store: &UserStore,
        lux_users_by_username: &mut HashMap<String, UserRecord>,
        job: &StoredEmbyMigrationJob,
        source_user: &MigrationUser,
        sync_profile: bool,
    ) -> Result<PreparedMigrationUser, EmbyMigrationServiceError> {
        let source_user_name = source_user.name.trim();
        if source_user_name.is_empty() {
            return Ok(PreparedMigrationUser {
                link: StoredEmbyMigrationUserLink {
                    job_id: job.id.clone(),
                    emby_user_id: source_user.id.clone(),
                    emby_username: source_user.name.clone(),
                    lux_user_id: None,
                    status: "SKIPPED".to_owned(),
                    error: Some("empty Emby username".to_owned()),
                },
                binding: None,
            });
        }
        if job.dry_run {
            return Ok(PreparedMigrationUser {
                link: StoredEmbyMigrationUserLink {
                    job_id: job.id.clone(),
                    emby_user_id: source_user.id.clone(),
                    emby_username: source_user_name.to_owned(),
                    lux_user_id: None,
                    status: "SKIPPED".to_owned(),
                    error: Some("DRY_RUN".to_owned()),
                },
                binding: None,
            });
        }
        if source_user_name.chars().count() > 128 {
            return Err(UserStoreError::InvalidUsername.into());
        }
        let source_user_name_normalized = source_user_name.to_lowercase();
        let existing = lux_users_by_username
            .get(&source_user_name_normalized)
            .cloned();
        if !sync_profile {
            let Some(existing) = existing else {
                return Ok(PreparedMigrationUser {
                    link: StoredEmbyMigrationUserLink {
                        job_id: job.id.clone(),
                        emby_user_id: source_user.id.clone(),
                        emby_username: source_user_name.to_owned(),
                        lux_user_id: None,
                        status: "SKIPPED".to_owned(),
                        error: Some(
                            "Lux user does not exist and user profile migration is disabled"
                                .to_owned(),
                        ),
                    },
                    binding: None,
                });
            };
            return Ok(PreparedMigrationUser {
                link: StoredEmbyMigrationUserLink {
                    job_id: job.id.clone(),
                    emby_user_id: source_user.id.clone(),
                    emby_username: source_user_name.to_owned(),
                    lux_user_id: Some(existing.id.to_string()),
                    status: "LINKED".to_owned(),
                    error: None,
                },
                binding: None,
            });
        }
        let (lux_user, status) = match existing {
            Some(user) => (user, "LINKED"),
            None => {
                let placeholder = Uuid::now_v7().to_string();
                let user = user_store
                    .create_user(source_user_name, source_user_name, &placeholder, false)
                    .await?;
                (user, "AUTO_CREATED")
            }
        };
        let lux_user = if migration_user_profile_changed(&lux_user, source_user) {
            user_store
                .update_user(&lux_user.id.to_string(), migration_user_update(source_user))
                .await?
                .ok_or(EmbyMigrationServiceError::NotFound)?
        } else {
            lux_user
        };
        lux_users_by_username.insert(source_user_name_normalized, lux_user.clone());
        let binding = StoredEmbyMigrationUserBinding {
            lux_user_id: lux_user.id.to_string(),
            source_base_url: job.source_base_url.clone(),
            secret_ref: Some(job.secret_ref.clone()),
            emby_user_id: source_user.id.clone(),
            emby_username: source_user_name.to_owned(),
            password_pending: source_user.has_password && !source_user.is_disabled,
        };
        Ok(PreparedMigrationUser {
            link: StoredEmbyMigrationUserLink {
                job_id: job.id.clone(),
                emby_user_id: source_user.id.clone(),
                emby_username: source_user_name.to_owned(),
                lux_user_id: Some(lux_user.id.to_string()),
                status: status.to_owned(),
                error: None,
            },
            binding: Some(binding),
        })
    }

    async fn flush_user_bindings(
        &self,
        bindings: &mut Vec<StoredEmbyMigrationUserBinding>,
        database_write_ms: &mut u128,
    ) -> Result<(), EmbyMigrationServiceError> {
        if bindings.is_empty() {
            return Ok(());
        }
        let write_started = Instant::now();
        self.database
            .upsert_emby_migration_user_bindings_batch(bindings)
            .await?;
        *database_write_ms += write_started.elapsed().as_millis();
        bindings.clear();
        Ok(())
    }

    async fn load_library_identities(
        &self,
    ) -> Result<Vec<MigrationLuxLibraryIdentity>, EmbyMigrationServiceError> {
        Ok(self
            .database
            .list_enabled_library_identities()
            .await?
            .into_iter()
            .map(|library| MigrationLuxLibraryIdentity {
                id: library.id,
                name: library.name,
                root_paths: library.root_paths,
            })
            .collect())
    }

    async fn load_media_identity_index_with_lookups(
        &self,
        items: &[MigrationItem],
        lookups: &[MigrationMediaIdentityLookup],
        target_library_filter: Option<&[String]>,
    ) -> Result<MigrationMediaIdentityIndex, EmbyMigrationServiceError> {
        let mut identities = self
            .database
            .list_migration_media_identity_candidates_filtered(lookups, target_library_filter)
            .await?;

        // Keep the explicit TARGET_LIBRARY_EXCLUDED report for items that are
        // found only outside the selected whitelist.  This fallback is
        // intentionally limited to unresolved page items; normal matches never
        // read identities from excluded libraries.
        if target_library_filter.is_some() {
            let selected_index = MigrationMediaIdentityIndex::new(identities.clone());
            let fallback_items = items
                .iter()
                .filter(|item| needs_unfiltered_library_fallback(item, &selected_index))
                .cloned()
                .collect::<Vec<_>>();
            if !fallback_items.is_empty() {
                let fallback_lookups = migration_media_identity_lookups(&fallback_items);
                let mut seen_ids = identities
                    .iter()
                    .map(|identity| identity.id.clone())
                    .collect::<HashSet<_>>();
                for identity in self
                    .database
                    .list_migration_media_identity_candidates(&fallback_lookups)
                    .await?
                {
                    if seen_ids.insert(identity.id.clone()) {
                        identities.push(identity);
                    }
                }
            }
        }
        Ok(MigrationMediaIdentityIndex::new(identities))
    }

    async fn load_media_identity_index_cached(
        &self,
        items: &[MigrationItem],
        target_library_filter: Option<&[String]>,
        cache: &mut MigrationMediaIdentityCache,
        cache_hits: &mut u64,
    ) -> Result<MigrationMediaIdentityIndex, EmbyMigrationServiceError> {
        let key = MigrationMediaIdentityCacheKey {
            lookups: migration_media_identity_lookups(items),
            target_library_ids: target_library_filter.map(ToOwned::to_owned),
        };
        if let Some(index) = cache.get(&key) {
            *cache_hits += 1;
            return Ok(index.clone());
        }
        let index = self
            .load_media_identity_index_with_lookups(items, &key.lookups, target_library_filter)
            .await?;
        cache.insert(key, index.clone());
        Ok(index)
    }

    #[allow(dead_code)]
    async fn load_person_identity_index(
        &self,
    ) -> Result<MigrationPersonIdentityIndex, EmbyMigrationServiceError> {
        Ok(MigrationPersonIdentityIndex::new(
            self.database.list_migration_person_identities().await?,
        ))
    }

    fn prefetch_migration_page(
        &self,
        source: &EmbyMigrationSource,
        user_id: &str,
        start_index: u32,
        kind: MigrationPageKind,
        source_filter: MigrationSourceFilter<'_>,
    ) -> MigrationPagePrefetch {
        let service = self.clone();
        let source = source.clone();
        let user_id = user_id.to_owned();
        let library_ids = source_filter.library_ids.map(ToOwned::to_owned);
        let state_fields = source_filter.state_fields.map(ToOwned::to_owned);
        let enabled = source_filter.enabled;
        MigrationPagePrefetch::new(tokio::spawn(async move {
            service
                .recover_migration_page(
                    &source,
                    &user_id,
                    start_index,
                    500,
                    kind,
                    MigrationSourceFilter {
                        library_ids: library_ids.as_deref(),
                        enabled,
                        state_fields: state_fields.as_deref(),
                    },
                )
                .await
        }))
    }

    async fn recover_migration_page(
        &self,
        source: &EmbyMigrationSource,
        user_id: &str,
        start_index: u32,
        limit: u32,
        kind: MigrationPageKind,
        source_filter: MigrationSourceFilter<'_>,
    ) -> Result<RecoveredMigrationPage, EmbyMigrationServiceError> {
        let read_started = Instant::now();
        let mut pending_ranges = vec![(start_index, limit)];
        let mut pages = Vec::new();
        let mut invalid_items = Vec::new();
        let mut source_rpc_calls = 0_u64;
        let mut source_rate_limited_rpc_calls = 0_u64;

        while let Some((range_start, range_limit)) = pending_ranges.pop() {
            if source_rpc_calls >= MAX_MIGRATION_PAGE_RECOVERY_RPCS {
                invalid_items.push(InvalidMigrationItem {
                    user_id: user_id.to_owned(),
                    start_index: range_start,
                    range_limit,
                    kind,
                });
                invalid_items.extend(pending_ranges.drain(..).map(|(start_index, range_limit)| {
                    InvalidMigrationItem {
                        user_id: user_id.to_owned(),
                        start_index,
                        range_limit,
                        kind,
                    }
                }));
                break;
            }
            let remaining_rpc_budget = MAX_MIGRATION_PAGE_RECOVERY_RPCS
                .saturating_sub(source_rpc_calls)
                .saturating_sub(1);
            let max_retries =
                remaining_rpc_budget.min(u64::from(MAX_SOURCE_RATE_LIMIT_RETRIES)) as u32;
            let mut operation = || async {
                match kind {
                    MigrationPageKind::UserState(state_filter) => {
                        self.plugin
                            .user_state_filtered(
                                source,
                                user_id,
                                range_start,
                                range_limit,
                                state_filter,
                                source_filter,
                            )
                            .await
                    }
                    MigrationPageKind::PersonFavorites => {
                        self.plugin
                            .person_favorites_filtered(
                                source,
                                user_id,
                                range_start,
                                range_limit,
                                source_filter,
                            )
                            .await
                    }
                }
            };
            let result =
                retry_rate_limited_source_call_with_limit(&mut operation, max_retries).await;
            match result {
                Ok(result) => {
                    source_rpc_calls += result.attempts;
                    source_rate_limited_rpc_calls += result.rate_limited_responses;
                    pages.push((range_start, result.value));
                }
                Err(result) if is_invalid_migration_response(&result.error) && range_limit > 1 => {
                    source_rpc_calls += result.attempts;
                    source_rate_limited_rpc_calls += result.rate_limited_responses;
                    if let Some(((left_start, left_limit), (right_start, right_limit))) =
                        split_migration_page_range(range_start, range_limit)
                    {
                        pending_ranges.push((right_start, right_limit));
                        pending_ranges.push((left_start, left_limit));
                    }
                }
                Err(result) if is_invalid_migration_response(&result.error) => {
                    source_rpc_calls += result.attempts;
                    source_rate_limited_rpc_calls += result.rate_limited_responses;
                    invalid_items.push(InvalidMigrationItem {
                        user_id: user_id.to_owned(),
                        start_index: range_start,
                        range_limit,
                        kind,
                    });
                    pages.push((range_start, empty_migration_page(range_start)));
                }
                Err(result) => {
                    tracing::warn!(
                        user_id,
                        start_index = range_start,
                        source_rpc_attempts = result.attempts,
                        source_rate_limited_rpc_calls = result.rate_limited_responses,
                        "Emby migration page failed after bounded retries"
                    );
                    return Err(result.error.into());
                }
            }
        }

        let mut recovered =
            assemble_recovered_migration_page(start_index, limit, pages, invalid_items);
        recovered.source_rpc_calls = source_rpc_calls;
        recovered.source_rate_limited_rpc_calls = source_rate_limited_rpc_calls;
        recovered.source_read_ms = read_started.elapsed().as_millis();
        Ok(recovered)
    }

    fn match_person(
        &self,
        person: &MigrationItem,
        index: &MigrationPersonIdentityIndex,
    ) -> PersonMatchOutcome {
        let mut provider_matches = Vec::<(String, bool)>::new();
        for (source_key, source_value) in &person.provider_ids {
            let provider = normalize_person_provider(source_key);
            if let Some(targets) = index
                .by_provider
                .get(&(provider.clone(), source_value.clone()))
            {
                for target in targets {
                    provider_matches.push((target.clone(), provider.eq_ignore_ascii_case("tmdb")));
                }
            }
        }
        provider_matches.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        provider_matches.dedup_by(|left, right| {
            if left.0 == right.0 {
                left.1 |= right.1;
                true
            } else {
                false
            }
        });
        if provider_matches.len() == 1 {
            let (lux_person_id, is_tmdb) = provider_matches[0].clone();
            return PersonMatchOutcome {
                lux_person_id: Some(lux_person_id),
                method: if is_tmdb { "TMDB_ID" } else { "PROVIDER_ID" },
                confidence: Some(100),
                status: "MATCHED",
            };
        }
        if provider_matches.len() > 1 {
            return PersonMatchOutcome {
                lux_person_id: None,
                method: "CONFLICT",
                confidence: None,
                status: "CONFLICT",
            };
        }

        let normalized_name = normalize_person_name(&person.name);
        if normalized_name.is_empty() {
            return PersonMatchOutcome::unmatched();
        }
        let Some(matches) = index.by_name.get(&normalized_name) else {
            return PersonMatchOutcome::unmatched();
        };
        if matches.len() == 1 {
            return PersonMatchOutcome {
                lux_person_id: Some(matches[0].clone()),
                method: "NAME",
                confidence: Some(90),
                status: "MATCHED",
            };
        }
        if matches.len() > 1 {
            return PersonMatchOutcome {
                lux_person_id: None,
                method: "CONFLICT",
                confidence: None,
                status: "CONFLICT",
            };
        }
        PersonMatchOutcome::unmatched()
    }

    async fn is_cancelled(&self, job_id: &str) -> Result<bool, EmbyMigrationServiceError> {
        Ok(self
            .database
            .emby_migration_cancel_requested(job_id)
            .await?)
    }

    async fn cancelled(&self, job_id: &str, phase: &str) -> Result<(), EmbyMigrationServiceError> {
        self.database
            .update_emby_migration_job_status(job_id, "CANCELLED", phase, None)
            .await?;
        Ok(())
    }

    async fn fail_job(
        &self,
        job_id: &str,
        phase: &str,
        error: &str,
    ) -> Result<(), EmbyMigrationServiceError> {
        self.database
            .update_emby_migration_job_status(job_id, "FAILED", phase, Some(error))
            .await?;
        Ok(())
    }
}

fn needs_unfiltered_library_fallback(
    item: &MigrationItem,
    selected_index: &MigrationMediaIdentityIndex,
) -> bool {
    // A conflict is already a definitive result. Re-reading excluded
    // libraries cannot make it safe to guess a single target and only adds
    // another full candidate query for the same lookup.
    match_item(item, selected_index).status == "UNMATCHED"
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MigrationPageKind {
    UserState(MigrationUserStateFilter),
    PersonFavorites,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct MigrationResumeCursor {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    state_filter: Option<MigrationUserStateFilter>,
    #[serde(default)]
    start_index: u32,
}

impl MigrationResumeCursor {
    fn parse(value: &str) -> Option<Self> {
        let cursor = serde_json::from_str::<Self>(value).ok()?;
        if cursor.user_id.is_none() || cursor.kind.is_none() {
            return None;
        }
        Some(cursor)
    }

    fn is_state(&self, filter: MigrationUserStateFilter) -> bool {
        self.kind.as_deref() == Some("USER_STATE") && self.state_filter == Some(filter)
    }

    fn is_person_favorites(&self) -> bool {
        self.kind.as_deref() == Some("PERSON_FAVORITES")
    }
}

fn migration_cursor_json(cursor: Option<MigrationResumeCursor>) -> String {
    cursor
        .and_then(|cursor| serde_json::to_string(&cursor).ok())
        .unwrap_or_else(|| "{}".to_owned())
}

fn should_lookup_handled_items(
    cursor: Option<&MigrationResumeCursor>,
    processed_count: i64,
    resume_user_index: Option<usize>,
    current_user_index: usize,
) -> bool {
    match resume_user_index {
        Some(resume_user_index) => resume_user_index == current_user_index,
        // A malformed or stale cursor cannot identify a single selected user.  Preserve the
        // durable deduplication guard for every user when such a partially processed job is
        // resumed from the beginning.
        None => cursor.is_some() || processed_count > 0,
    }
}

fn next_migration_cursor(
    user_ids: &[String],
    user_index: usize,
    kind: MigrationPageKind,
    state_filter: Option<MigrationUserStateFilter>,
    next_start_index: Option<u32>,
    scope: &MigrationScope,
) -> Option<MigrationResumeCursor> {
    let current_user_id = user_ids.get(user_index)?.clone();
    if let Some(next_start_index) = next_start_index {
        return Some(MigrationResumeCursor {
            kind: Some(
                match kind {
                    MigrationPageKind::UserState(_) => "USER_STATE",
                    MigrationPageKind::PersonFavorites => "PERSON_FAVORITES",
                }
                .to_owned(),
            ),
            user_id: Some(current_user_id),
            state_filter,
            start_index: next_start_index,
        });
    }
    if matches!(kind, MigrationPageKind::UserState(_)) && scope.item_state {
        let current_filter = state_filter?;
        let next_filter = scope
            .selected_item_state_filters()
            .iter()
            .position(|filter| *filter == current_filter)
            .and_then(|index| scope.selected_item_state_filters().get(index + 1).copied());
        if let Some(next_filter) = next_filter {
            return Some(MigrationResumeCursor {
                kind: Some("USER_STATE".to_owned()),
                user_id: Some(current_user_id),
                state_filter: Some(next_filter),
                start_index: 0,
            });
        }
    }
    if scope.person_favorites && matches!(kind, MigrationPageKind::UserState(_)) {
        return Some(MigrationResumeCursor {
            kind: Some("PERSON_FAVORITES".to_owned()),
            user_id: Some(current_user_id),
            state_filter: None,
            start_index: 0,
        });
    }
    let next_user_id = user_ids.get(user_index + 1)?.clone();
    if scope.item_state {
        Some(MigrationResumeCursor {
            kind: Some("USER_STATE".to_owned()),
            user_id: Some(next_user_id),
            state_filter: Some(MigrationUserStateFilter::Played),
            start_index: 0,
        })
    } else if scope.person_favorites {
        Some(MigrationResumeCursor {
            kind: Some("PERSON_FAVORITES".to_owned()),
            user_id: Some(next_user_id),
            state_filter: None,
            start_index: 0,
        })
    } else {
        None
    }
}

impl MigrationPageKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::UserState(MigrationUserStateFilter::Played) => "USER_STATE_PLAYED",
            Self::UserState(MigrationUserStateFilter::Favorite) => "USER_STATE_FAVORITE",
            Self::UserState(MigrationUserStateFilter::Resumable) => "USER_STATE_RESUMABLE",
            Self::PersonFavorites => "PERSON_FAVORITES",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InvalidMigrationItem {
    user_id: String,
    start_index: u32,
    range_limit: u32,
    kind: MigrationPageKind,
}

struct RecoveredMigrationPage {
    page: MigrationItemPage,
    invalid_items: Vec<InvalidMigrationItem>,
    source_rpc_calls: u64,
    source_rate_limited_rpc_calls: u64,
    source_read_ms: u128,
}

/// Owns one read-ahead page and cancels it if the current page cannot be
/// committed.  Dropping a Tokio join handle normally detaches its task; that
/// would let a cancelled migration keep consuming source pages, so this
/// wrapper aborts an unconsumed read explicitly.
struct MigrationPagePrefetch {
    handle:
        Option<tokio::task::JoinHandle<Result<RecoveredMigrationPage, EmbyMigrationServiceError>>>,
}

impl MigrationPagePrefetch {
    fn new(
        handle: tokio::task::JoinHandle<Result<RecoveredMigrationPage, EmbyMigrationServiceError>>,
    ) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    async fn join(mut self) -> Result<RecoveredMigrationPage, EmbyMigrationServiceError> {
        let Some(handle) = self.handle.take() else {
            return Err(EmbyMigrationServiceError::InvalidState);
        };
        handle
            .await
            .map_err(|_| EmbyMigrationServiceError::InvalidState)?
    }
}

impl Drop for MigrationPagePrefetch {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.as_ref() {
            handle.abort();
        }
    }
}

fn assemble_recovered_migration_page(
    start_index: u32,
    requested_limit: u32,
    mut pages: Vec<(u32, MigrationItemPage)>,
    invalid_items: Vec<InvalidMigrationItem>,
) -> RecoveredMigrationPage {
    pages.sort_unstable_by_key(|(page_start, _)| *page_start);
    let total_record_count = pages.iter().find_map(|(_, page)| page.total_record_count);
    let requested_end = start_index.saturating_add(requested_limit);
    let next_start_index = match total_record_count {
        Some(total) => (requested_end < total).then_some(requested_end),
        None => pages
            .iter()
            .filter_map(|(_, page)| page.next_start_index)
            .max()
            .or_else(|| (requested_end > start_index).then_some(requested_end)),
    };
    let history_capability = pages
        .first()
        .map(|(_, page)| page.history_capability)
        .unwrap_or(HistoryCapability::ItemState);
    let items = pages.into_iter().flat_map(|(_, page)| page.items).collect();

    RecoveredMigrationPage {
        page: MigrationItemPage {
            items,
            start_index,
            total_record_count,
            next_start_index,
            history_capability,
        },
        invalid_items,
        source_rpc_calls: 0,
        source_rate_limited_rpc_calls: 0,
        source_read_ms: 0,
    }
}

fn is_invalid_migration_response(error: &PluginServiceError) -> bool {
    match error {
        PluginServiceError::InvalidResponse => true,
        PluginServiceError::Runtime(PluginRuntimeError::Plugin { code, .. }) => {
            code.eq_ignore_ascii_case("PLUGIN_INVALID_RESPONSE")
        }
        _ => false,
    }
}

fn is_rate_limited_migration_response(error: &PluginServiceError) -> bool {
    matches!(
        error,
        PluginServiceError::Runtime(PluginRuntimeError::Plugin { code, .. })
            if code.eq_ignore_ascii_case("PLUGIN_RATE_LIMITED")
    )
}

fn invalid_item_report_id(invalid: &InvalidMigrationItem) -> String {
    format!(
        "invalid:{}:{}:{}",
        invalid.kind.as_str(),
        invalid.user_id,
        invalid.start_index
    )
}

fn invalid_item_report_detail(invalid: &InvalidMigrationItem) -> String {
    serde_json::to_string(&json!({
        "reason": if invalid.range_limit > 1 {
            "PLUGIN_INVALID_RESPONSE_RECOVERY_BUDGET"
        } else {
            "PLUGIN_INVALID_RESPONSE"
        },
        "pageKind": invalid.kind.as_str(),
        "sourceUserId": invalid.user_id,
        "sourceStartIndex": invalid.start_index,
        "sourceRangeLimit": invalid.range_limit,
    }))
    .unwrap_or_else(|_| "{}".to_owned())
}

fn invalid_person_favorite_report(
    invalid: &InvalidMigrationItem,
) -> EmbyMigrationPersonFavoriteBatch {
    let report_id = invalid_item_report_id(invalid);
    EmbyMigrationPersonFavoriteBatch {
        emby_user_id: invalid.user_id.clone(),
        emby_person_id: report_id.clone(),
        emby_person_name: "Invalid Emby person response".to_owned(),
        lux_user_id: None,
        lux_person_id: None,
        provider_ids_json: "{}".to_owned(),
        match_method: "UNMATCHED".to_owned(),
        confidence: None,
        status: "SKIPPED".to_owned(),
        state_hash: report_id,
        detail_json: invalid_item_report_detail(invalid),
        error: Some("PLUGIN_INVALID_RESPONSE".to_owned()),
    }
}

fn empty_migration_page(start_index: u32) -> MigrationItemPage {
    MigrationItemPage {
        items: Vec::new(),
        start_index,
        total_record_count: None,
        next_start_index: None,
        history_capability: HistoryCapability::ItemState,
    }
}

fn split_migration_page_range(start_index: u32, limit: u32) -> Option<((u32, u32), (u32, u32))> {
    if limit <= 1 {
        return None;
    }
    let left_limit = limit / 2;
    let right_limit = limit - left_limit;
    Some((
        (start_index, left_limit),
        (start_index.saturating_add(left_limit), right_limit),
    ))
}

fn collect_recorded_state_items(
    items: Vec<MigrationItem>,
    seen_emby_item_ids: &mut HashSet<String>,
) -> Vec<MigrationItem> {
    items
        .into_iter()
        .filter_map(|item| {
            let has_recorded_state = item
                .user_data
                .as_ref()
                .is_some_and(MigrationUserData::has_recorded_state);
            if has_recorded_state && seen_emby_item_ids.insert(item.id.clone()) {
                Some(item)
            } else {
                None
            }
        })
        .collect()
}

fn retain_unhandled_state_items(
    items: Vec<MigrationItem>,
    handled_item_ids: &HashSet<String>,
) -> Vec<MigrationItem> {
    items
        .into_iter()
        .filter(|item| !handled_item_ids.contains(&item.id))
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MigrationLuxLibraryIdentity {
    id: String,
    name: String,
    root_paths: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MigrationSourceLibraryMapping {
    source_id: String,
    lux_library_id: Option<String>,
}

fn precompute_source_library_mappings(
    source_folders: &[MigrationLibraryFolder],
    lux_libraries: &[MigrationLuxLibraryIdentity],
) -> Vec<MigrationSourceLibraryMapping> {
    source_folders
        .iter()
        .map(|folder| MigrationSourceLibraryMapping {
            source_id: folder.id.clone(),
            lux_library_id: match_lux_library(folder, lux_libraries),
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LibraryAccessPlan {
    Disabled,
    Exact(HashSet<String>),
    Derived,
    Unavailable,
}

impl LibraryAccessPlan {
    /// Determines whether an item can be imported for the source user's mapped libraries.
    ///
    /// `Derived` is retained for older Emby sources that cannot return library folders: the
    /// existing import-derived ACL behavior remains compatible, while `Unavailable` is used
    /// for an explicit but ambiguous folder mapping and must deny writes.
    fn allows(&self, library_id: Option<&str>) -> bool {
        match self {
            Self::Disabled | Self::Derived => true,
            Self::Exact(library_ids) => {
                library_id.is_some_and(|library_id| library_ids.contains(library_id))
            }
            Self::Unavailable => false,
        }
    }
}

fn target_library_is_selected(
    selected_library_ids: Option<&HashSet<String>>,
    library_id: Option<&str>,
) -> bool {
    match selected_library_ids {
        None => true,
        Some(selected_library_ids) => {
            library_id.is_some_and(|library_id| selected_library_ids.contains(library_id))
        }
    }
}

fn restrict_library_access_plan(
    plan: LibraryAccessPlan,
    selected_library_ids: Option<&HashSet<String>>,
) -> LibraryAccessPlan {
    match (plan, selected_library_ids) {
        (LibraryAccessPlan::Exact(library_ids), Some(selected_library_ids)) => {
            LibraryAccessPlan::Exact(
                library_ids
                    .into_iter()
                    .filter(|library_id| selected_library_ids.contains(library_id))
                    .collect(),
            )
        }
        (plan, _) => plan,
    }
}

/// Returns the source virtual-folder IDs that can contribute to the selected
/// Lux libraries for a user.  `None` means the source did not provide enough
/// folder identity data to prove a safe source-side restriction; callers must
/// then retain the legacy target-side filtering behaviour.
#[cfg(test)]
fn source_library_ids_for_user(
    user: &MigrationUser,
    source_folders: Option<&[MigrationLibraryFolder]>,
    lux_libraries: &[MigrationLuxLibraryIdentity],
    selected_library_ids: &HashSet<String>,
) -> Option<Vec<String>> {
    let source_folders = source_folders?;
    let mappings = precompute_source_library_mappings(source_folders, lux_libraries);
    Some(source_library_ids_for_user_with_mappings(
        user,
        &mappings,
        selected_library_ids,
    ))
}

fn source_library_ids_for_user_with_mappings(
    user: &MigrationUser,
    source_library_mappings: &[MigrationSourceLibraryMapping],
    selected_library_ids: &HashSet<String>,
) -> Vec<String> {
    let mut source_ids = source_library_mappings
        .iter()
        .filter(|mapping| {
            user.enable_all_folders
                || user
                    .enabled_folders
                    .iter()
                    .any(|id| id == &mapping.source_id)
        })
        .filter_map(|mapping| {
            mapping
                .lux_library_id
                .as_ref()
                .filter(|lux_library_id| selected_library_ids.contains(*lux_library_id))
                .map(|_| mapping.source_id.clone())
        })
        .collect::<Vec<_>>();
    source_ids.sort_unstable();
    source_ids.dedup();
    source_ids
}

/// Return a source-side library filter only when the mapping is complete
/// enough to prove that it cannot hide an item that belongs to a selected
/// target library.  A user with `EnableAllFolders` can read every Emby
/// virtual folder, so an unknown mapping must not be interpreted as an empty
/// source scope: the item may still belong to a selected Lux library whose
/// path/name could not be matched.  Restricted users retain the existing
/// empty-filter behavior because an unmapped enabled folder is already denied
/// by the exact library-access plan.
fn source_library_filter_for_user(
    user: &MigrationUser,
    source_library_mappings: &[MigrationSourceLibraryMapping],
    selected_library_ids: &HashSet<String>,
) -> Option<Vec<String>> {
    if user.enable_all_folders
        && source_library_mappings
            .iter()
            .any(|mapping| mapping.lux_library_id.is_none())
    {
        return None;
    }
    Some(source_library_ids_for_user_with_mappings(
        user,
        source_library_mappings,
        selected_library_ids,
    ))
}

fn source_filter_has_candidates(
    filtered_reads: bool,
    source_library_ids: Option<&[String]>,
) -> bool {
    !filtered_reads || source_library_ids.is_none_or(|library_ids| !library_ids.is_empty())
}

fn migration_source_filtering_enabled(
    supports_filtered_reads: bool,
    scope: &MigrationScope,
    target_library_ids: Option<&HashSet<String>>,
    users: &[MigrationUser],
) -> bool {
    supports_filtered_reads
        && (scope.item_state || scope.person_favorites)
        && (target_library_ids.is_some() || users.iter().any(|user| !user.enable_all_folders))
}

fn should_prefetch_source_page(rate_limited_rpc_calls: u64) -> bool {
    rate_limited_rpc_calls == 0
}

#[cfg(test)]
fn map_enabled_library_ids(
    user: &MigrationUser,
    source_folders: Option<&[MigrationLibraryFolder]>,
    lux_libraries: &[MigrationLuxLibraryIdentity],
) -> HashSet<String> {
    let Some(source_folders) = source_folders else {
        return HashSet::new();
    };
    source_folders
        .iter()
        .filter(|folder| user.enabled_folders.iter().any(|id| id == &folder.id))
        .filter_map(|folder| match_lux_library(folder, lux_libraries))
        .collect()
}

#[cfg(test)]
fn map_enabled_library_ids_checked(
    user: &MigrationUser,
    source_folders: Option<&[MigrationLibraryFolder]>,
    lux_libraries: &[MigrationLuxLibraryIdentity],
) -> Option<HashSet<String>> {
    let source_folders = source_folders?;
    let mappings = precompute_source_library_mappings(source_folders, lux_libraries);
    map_enabled_library_ids_checked_with_mappings(user, &mappings)
}

fn map_enabled_library_ids_checked_with_mappings(
    user: &MigrationUser,
    source_library_mappings: &[MigrationSourceLibraryMapping],
) -> Option<HashSet<String>> {
    let selected = source_library_mappings.iter().filter(|mapping| {
        user.enabled_folders
            .iter()
            .any(|id| id == &mapping.source_id)
    });
    let mut mapped = HashSet::new();
    for mapping in selected {
        mapped.insert(mapping.lux_library_id.clone()?);
    }
    Some(mapped)
}

fn match_lux_library(
    source_folder: &MigrationLibraryFolder,
    lux_libraries: &[MigrationLuxLibraryIdentity],
) -> Option<String> {
    let normalized_name = normalize_title(&source_folder.name);
    let name_matches = lux_libraries
        .iter()
        .filter(|library| normalize_title(&library.name) == normalized_name)
        .collect::<Vec<_>>();
    let source_paths = source_folder
        .locations
        .iter()
        .map(|path| normalize_library_path(path))
        .filter(|path| !path.is_empty())
        .collect::<HashSet<_>>();
    if name_matches.len() == 1 {
        let library = name_matches[0];
        let path_matches = library
            .root_paths
            .iter()
            .map(|path| normalize_library_path(path))
            .any(|path| source_paths.contains(&path));
        if source_paths.is_empty() || path_matches {
            return Some(library.id.clone());
        }
    }
    let path_matches = lux_libraries
        .iter()
        .filter(|library| {
            library
                .root_paths
                .iter()
                .map(|path| normalize_library_path(path))
                .any(|path| source_paths.contains(&path))
        })
        .collect::<Vec<_>>();
    (path_matches.len() == 1).then(|| path_matches[0].id.clone())
}

fn normalize_library_path(value: &str) -> String {
    let value = value.trim().replace('\\', "/");
    if value == "/" {
        return value;
    }
    value.trim_end_matches('/').to_owned()
}

fn migration_user_update(source_user: &MigrationUser) -> UserUpdate<'_> {
    UserUpdate {
        is_disabled: Some(source_user.is_disabled),
        can_remote_access: Some(source_user.enable_remote_access),
        can_download: Some(source_user.enable_content_downloading),
        ..UserUpdate::default()
    }
}

fn migration_user_profile_changed(lux_user: &UserRecord, source_user: &MigrationUser) -> bool {
    lux_user.is_disabled != source_user.is_disabled
        || lux_user.can_remote_access != source_user.enable_remote_access
        || lux_user.can_download != source_user.enable_content_downloading
}

fn is_migratable_person_favorite(item: &MigrationItem) -> bool {
    item.item_type == "Person"
        && item
            .user_data
            .as_ref()
            .is_none_or(|user_data| user_data.is_favorite)
}

struct MatchOutcome {
    lux_item_id: Option<String>,
    method: &'static str,
    confidence: Option<i64>,
    status: &'static str,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct MigrationMediaIdentityCacheKey {
    lookups: Vec<MigrationMediaIdentityLookup>,
    target_library_ids: Option<Vec<String>>,
}

struct MigrationMediaIdentityCache {
    max_entries: usize,
    max_identities: usize,
    identity_count: usize,
    entries: HashMap<MigrationMediaIdentityCacheKey, MigrationMediaIdentityIndex>,
}

impl MigrationMediaIdentityCache {
    fn new(max_entries: usize, max_identities: usize) -> Self {
        Self {
            max_entries: max_entries.max(1),
            max_identities: max_identities.max(1),
            identity_count: 0,
            entries: HashMap::new(),
        }
    }

    fn get(&self, key: &MigrationMediaIdentityCacheKey) -> Option<&MigrationMediaIdentityIndex> {
        self.entries.get(key)
    }

    fn insert(&mut self, key: MigrationMediaIdentityCacheKey, index: MigrationMediaIdentityIndex) {
        let identity_count = index.identities.len();
        if identity_count > self.max_identities {
            return;
        }
        let existing_identity_count = self
            .entries
            .get(&key)
            .map(|previous| previous.identities.len())
            .unwrap_or_default();
        let projected_identity_count = self
            .identity_count
            .saturating_sub(existing_identity_count)
            .saturating_add(identity_count);
        if (!self.entries.contains_key(&key) && self.entries.len() >= self.max_entries)
            || projected_identity_count > self.max_identities
        {
            // A whole-cache eviction is deliberate: the cache is a bounded
            // acceleration layer for one migration run, not a second index.
            self.entries.clear();
            self.identity_count = 0;
        }
        if let Some(previous) = self.entries.insert(key, index) {
            self.identity_count = self
                .identity_count
                .saturating_sub(previous.identities.len());
        }
        self.identity_count = self.identity_count.saturating_add(identity_count);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct MigrationPersonIdentityCacheKey {
    lookups: Vec<MigrationPersonIdentityLookup>,
}

struct MigrationPersonIdentityCache {
    max_entries: usize,
    max_identities: usize,
    identity_count: usize,
    entries: HashMap<MigrationPersonIdentityCacheKey, MigrationPersonIdentityIndex>,
}

impl MigrationPersonIdentityCache {
    fn new(max_entries: usize, max_identities: usize) -> Self {
        Self {
            max_entries: max_entries.max(1),
            max_identities: max_identities.max(1),
            identity_count: 0,
            entries: HashMap::new(),
        }
    }

    fn get(&self, key: &MigrationPersonIdentityCacheKey) -> Option<&MigrationPersonIdentityIndex> {
        self.entries.get(key)
    }

    fn insert(
        &mut self,
        key: MigrationPersonIdentityCacheKey,
        index: MigrationPersonIdentityIndex,
    ) {
        let identity_count = index.identity_count();
        if identity_count > self.max_identities {
            return;
        }
        let existing_identity_count = self
            .entries
            .get(&key)
            .map(MigrationPersonIdentityIndex::identity_count)
            .unwrap_or_default();
        let projected_identity_count = self
            .identity_count
            .saturating_sub(existing_identity_count)
            .saturating_add(identity_count);
        if (!self.entries.contains_key(&key) && self.entries.len() >= self.max_entries)
            || projected_identity_count > self.max_identities
        {
            // The cache is a bounded acceleration layer for one migration run,
            // not a second copy of the canonical people index.
            self.entries.clear();
            self.identity_count = 0;
        }
        if let Some(previous) = self.entries.insert(key, index) {
            self.identity_count = self
                .identity_count
                .saturating_sub(previous.identity_count());
        }
        self.identity_count = self.identity_count.saturating_add(identity_count);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Clone)]
struct MigrationMediaIdentityIndex {
    identities: Vec<StoredMigrationMediaIdentity>,
    by_id: HashMap<String, usize>,
    by_provider: HashMap<(String, String, String), Vec<usize>>,
    by_title: HashMap<(String, String), Vec<usize>>,
}

impl MigrationMediaIdentityIndex {
    fn new(identities: Vec<StoredMigrationMediaIdentity>) -> Self {
        let mut by_provider = HashMap::new();
        let mut by_title = HashMap::new();
        let mut by_id = HashMap::new();
        for (index, identity) in identities.iter().enumerate() {
            by_id.insert(identity.id.clone(), index);
            by_title
                .entry((identity.item_type.clone(), normalize_title(&identity.title)))
                .or_insert_with(Vec::new)
                .push(index);
            let Some(provider_ids_json) = identity.provider_ids_json.as_deref() else {
                continue;
            };
            let Ok(provider_ids) = serde_json::from_str::<std::collections::BTreeMap<String, String>>(
                provider_ids_json,
            ) else {
                continue;
            };
            for (provider, value) in provider_ids {
                by_provider
                    .entry((
                        identity.item_type.clone(),
                        provider.to_ascii_lowercase(),
                        value,
                    ))
                    .or_insert_with(Vec::new)
                    .push(index);
            }
        }
        Self {
            identities,
            by_id,
            by_provider,
            by_title,
        }
    }

    fn library_id(&self, item_id: &str) -> Option<String> {
        self.by_id
            .get(item_id)
            .and_then(|index| self.identities.get(*index))
            .map(|identity| identity.library_id.clone())
    }
}

fn migration_media_identity_lookups(items: &[MigrationItem]) -> Vec<MigrationMediaIdentityLookup> {
    let mut lookups = items
        .iter()
        .filter_map(|item| {
            let item_type = match item.item_type.as_str() {
                "Movie" => "MOVIE",
                "Series" => "SERIES",
                "Season" => "SEASON",
                "Episode" => "EPISODE",
                _ => return None,
            };
            let mut provider_ids = item
                .provider_ids
                .iter()
                .map(|(provider, provider_id)| (provider.to_ascii_lowercase(), provider_id.clone()))
                .collect::<Vec<_>>();
            provider_ids.sort_unstable();
            provider_ids.dedup();
            Some(MigrationMediaIdentityLookup {
                item_type: item_type.to_owned(),
                title: item.name.clone(),
                title_pattern: migration_title_like_pattern(&item.name),
                production_year: item.production_year,
                season_number: item.parent_index_number,
                episode_number: item.index_number,
                provider_ids,
            })
        })
        .collect::<Vec<_>>();
    // A source page can contain duplicate records (for example when multiple
    // state filters overlap).  Identical lookup keys produce identical SQL
    // predicates, so collapse them before reaching storage to reduce bound
    // parameters and candidate rows without changing match semantics.
    lookups.sort_unstable_by(|left, right| {
        left.item_type
            .cmp(&right.item_type)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.title_pattern.cmp(&right.title_pattern))
            .then_with(|| left.production_year.cmp(&right.production_year))
            .then_with(|| left.season_number.cmp(&right.season_number))
            .then_with(|| left.episode_number.cmp(&right.episode_number))
            .then_with(|| left.provider_ids.cmp(&right.provider_ids))
    });
    lookups.dedup();
    lookups
}

fn migration_person_identity_lookups(
    items: &[MigrationItem],
) -> Vec<MigrationPersonIdentityLookup> {
    let mut lookups = items
        .iter()
        .filter_map(|item| {
            let normalized_name = normalize_person_name(&item.name);
            let mut provider_ids = item
                .provider_ids
                .iter()
                .map(|(provider, provider_id)| {
                    (normalize_person_provider(provider), provider_id.clone())
                })
                .collect::<Vec<_>>();
            provider_ids.sort_unstable();
            provider_ids.dedup();
            if normalized_name.is_empty() && provider_ids.is_empty() {
                None
            } else {
                Some(MigrationPersonIdentityLookup {
                    normalized_name,
                    provider_ids,
                })
            }
        })
        .collect::<Vec<_>>();
    lookups.sort_unstable_by(|left, right| {
        left.normalized_name
            .cmp(&right.normalized_name)
            .then_with(|| left.provider_ids.cmp(&right.provider_ids))
    });
    lookups.dedup();
    lookups
}

#[derive(Clone)]
struct MigrationPersonIdentityIndex {
    by_provider: HashMap<(String, String), Vec<String>>,
    by_name: HashMap<String, Vec<String>>,
    identity_count: usize,
}

impl MigrationPersonIdentityIndex {
    fn new(identities: Vec<StoredMigrationPersonIdentity>) -> Self {
        let mut by_provider: HashMap<(String, String), Vec<String>> = HashMap::new();
        let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
        let mut identity_ids = HashSet::new();
        for identity in identities {
            identity_ids.insert(identity.id.clone());
            let normalized_name = normalize_person_name(&identity.display_name);
            if !normalized_name.is_empty() {
                let ids = by_name.entry(normalized_name).or_default();
                if !ids.iter().any(|id| id == &identity.id) {
                    ids.push(identity.id.clone());
                }
            }
            if let (Some(provider), Some(provider_id)) = (identity.provider, identity.provider_id) {
                let ids = by_provider
                    .entry((normalize_person_provider(&provider), provider_id))
                    .or_default();
                if !ids.iter().any(|id| id == &identity.id) {
                    ids.push(identity.id.clone());
                }
            }
        }
        for ids in by_provider.values_mut() {
            ids.sort_unstable();
        }
        for ids in by_name.values_mut() {
            ids.sort_unstable();
        }
        Self {
            by_provider,
            by_name,
            identity_count: identity_ids.len(),
        }
    }

    fn identity_count(&self) -> usize {
        self.identity_count
    }
}

struct PersonMatchOutcome {
    lux_person_id: Option<String>,
    method: &'static str,
    confidence: Option<i64>,
    status: &'static str,
}

impl PersonMatchOutcome {
    fn unmatched() -> Self {
        Self {
            lux_person_id: None,
            method: "UNMATCHED",
            confidence: None,
            status: "UNMATCHED",
        }
    }
}

fn match_item(item: &MigrationItem, index: &MigrationMediaIdentityIndex) -> MatchOutcome {
    let expected_type = match item.item_type.as_str() {
        "Movie" => "MOVIE",
        "Series" => "SERIES",
        "Season" => "SEASON",
        "Episode" => "EPISODE",
        _ => return unmatched("unsupported item type"),
    };
    let mut provider_matches = HashSet::new();
    for (source_key, source_value) in &item.provider_ids {
        if let Some(candidates) = index.by_provider.get(&(
            expected_type.to_owned(),
            source_key.to_ascii_lowercase(),
            source_value.clone(),
        )) {
            provider_matches.extend(candidates.iter().copied());
        }
    }
    if provider_matches.len() == 1 {
        let method = if item
            .provider_ids
            .keys()
            .any(|key| key.eq_ignore_ascii_case("Tmdb"))
        {
            "TMDB_ID"
        } else {
            "PROVIDER_ID"
        };
        return MatchOutcome {
            lux_item_id: provider_matches
                .into_iter()
                .next()
                .and_then(|candidate_index| {
                    index
                        .identities
                        .get(candidate_index)
                        .map(|identity| identity.id.clone())
                }),
            method,
            confidence: Some(100),
            status: "MATCHED",
        };
    }
    if provider_matches.len() > 1 {
        return MatchOutcome {
            lux_item_id: None,
            method: "CONFLICT",
            confidence: None,
            status: "CONFLICT",
        };
    }

    let title = normalize_title(&item.name);
    if title.is_empty() {
        return unmatched("empty title");
    }
    let mut title_matches = index
        .by_title
        .get(&(expected_type.to_owned(), title))
        .cloned()
        .unwrap_or_default();
    title_matches.retain(|candidate_index| {
        let Some(identity) = index.identities.get(*candidate_index) else {
            return false;
        };
        (match (item.production_year, identity.production_year) {
            (Some(source_year), Some(target_year)) => (source_year - target_year).abs() <= 1,
            _ => true,
        }) && (expected_type != "EPISODE"
            || (item
                .index_number
                .is_none_or(|number| identity.episode_number == Some(number))
                && item
                    .parent_index_number
                    .is_none_or(|number| identity.season_number == Some(number))))
    });
    if title_matches.len() == 1 {
        return MatchOutcome {
            lux_item_id: title_matches.pop().and_then(|candidate_index| {
                index
                    .identities
                    .get(candidate_index)
                    .map(|identity| identity.id.clone())
            }),
            method: if expected_type == "EPISODE" {
                "EPISODE_KEY"
            } else {
                "TITLE_YEAR"
            },
            confidence: Some(if expected_type == "EPISODE" { 95 } else { 90 }),
            status: "MATCHED",
        };
    }
    if title_matches.len() > 1 {
        return MatchOutcome {
            lux_item_id: None,
            method: "CONFLICT",
            confidence: None,
            status: "CONFLICT",
        };
    }
    unmatched("no unique media match")
}

fn unmatched(_reason: &str) -> MatchOutcome {
    MatchOutcome {
        lux_item_id: None,
        method: "UNMATCHED",
        confidence: None,
        status: "UNMATCHED",
    }
}

fn migration_item_detail(
    item: &MigrationItem,
    outcome: &MatchOutcome,
    identity_index: &MigrationMediaIdentityIndex,
) -> serde_json::Value {
    let lux_identity = outcome
        .lux_item_id
        .as_deref()
        .and_then(|id| identity_index.by_id.get(id))
        .and_then(|index| identity_index.identities.get(*index));
    let lux_series_identity = lux_identity
        .and_then(|identity| identity.series_id.as_deref())
        .and_then(|series_id| identity_index.by_id.get(series_id))
        .and_then(|index| identity_index.identities.get(*index));

    json!({
        "sourceTitle": item.name,
        "sourceType": item.item_type,
        "productionYear": item.production_year,
        "luxTitle": lux_identity.map(|identity| identity.title.as_str()),
        "luxItemType": lux_identity.map(|identity| identity.item_type.as_str()),
        "luxSeriesTitle": lux_series_identity.map(|identity| identity.title.as_str()),
        "luxSeasonNumber": lux_identity.and_then(|identity| identity.season_number),
        "luxEpisodeNumber": lux_identity.and_then(|identity| identity.episode_number),
    })
}

fn normalize_title(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn migration_title_like_pattern(value: &str) -> String {
    let mut pattern = String::from("%");
    let mut in_run = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            pattern.push(character);
            in_run = true;
        } else if in_run {
            pattern.push('%');
            in_run = false;
        }
    }
    if in_run {
        pattern.push('%');
    }
    pattern
}

fn normalize_person_provider(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "tmdb" => "tmdb".to_owned(),
        "imdb" => "imdb".to_owned(),
        "tvdb" => "tvdb".to_owned(),
        value => value.to_owned(),
    }
}

fn normalize_person_name(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn incoming_state(data: &MigrationUserData) -> Result<StoredItemState, EmbyMigrationServiceError> {
    let last_played_at = data
        .last_played_date
        .as_deref()
        .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
        .map(|value| value.unix_timestamp());
    Ok(StoredItemState {
        position_ticks: data.playback_position_ticks,
        is_played: data.played,
        is_favorite: data.is_favorite,
        play_count: data.play_count,
        last_played_at,
    })
}

fn state_fields_for_migration(scope: &MigrationScope) -> EmbyMigrationUserItemStateFields {
    let mut fields = EmbyMigrationUserItemStateFields {
        position_ticks: false,
        is_played: false,
        is_favorite: false,
        play_count: false,
        last_played_at: false,
    };
    for filter in scope.selected_item_state_filters() {
        match filter {
            MigrationUserStateFilter::Played => {
                fields.is_played = true;
                fields.play_count = true;
                fields.last_played_at = true;
            }
            MigrationUserStateFilter::Favorite => fields.is_favorite = true,
            MigrationUserStateFilter::Resumable => fields.position_ticks = true,
        }
    }
    fields
}

fn user_fields_for_migration(scope: &MigrationScope) -> Vec<&'static str> {
    let mut fields = vec!["id", "name"];
    if scope.user_profile {
        fields.extend([
            "hasPassword",
            "isDisabled",
            "enableRemoteAccess",
            "enableContentDownloading",
        ]);
    }
    if scope.library_access || scope.item_state {
        fields.extend(["enableAllFolders", "enabledFolders", "libraryFolders"]);
    }
    fields
}

fn migration_merge_policy(value: &str) -> MigrationMergePolicy {
    match value {
        "OVERWRITE" => MigrationMergePolicy::Overwrite,
        "SKIP" => MigrationMergePolicy::Skip,
        _ => MigrationMergePolicy::Merge,
    }
}

fn migration_scope_from_json(value: &str) -> MigrationScope {
    serde_json::from_str(value).unwrap_or_default()
}

fn project_source_user_page(
    page: MigrationUserPage,
    offset: i64,
    limit: i64,
    search: Option<&str>,
) -> MigrationSourceUserPageView {
    let has_pagination = page.start_index.is_some()
        || page.total_record_count.is_some()
        || page.next_start_index.is_some();
    let (users, total) = if has_pagination {
        let total = page
            .total_record_count
            .unwrap_or_else(|| offset.saturating_add(page.items.len() as i64));
        (page.items, total)
    } else {
        // Legacy plugins ignore optional list bounds and return the complete
        // user list. Apply search and slicing locally for compatibility.
        let search_lower = search
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_lowercase);
        let filtered = page
            .items
            .into_iter()
            .filter(|user| {
                search_lower
                    .as_deref()
                    .is_none_or(|search| user.name.to_lowercase().contains(search))
            })
            .collect::<Vec<_>>();
        let total = filtered.len() as i64;
        let users = filtered
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect::<Vec<_>>();
        (users, total)
    };
    MigrationSourceUserPageView {
        users: users
            .into_iter()
            .map(MigrationSourceUserView::from)
            .collect(),
        total,
    }
}

fn normalize_migration_scope(
    mut scope: MigrationScope,
) -> Result<MigrationScope, MigrationInputError> {
    if let Some(item_state_filters) = scope.item_state_filters.take() {
        let item_state_filters = normalize_item_state_filters(&item_state_filters);
        if scope.item_state && item_state_filters.is_empty() {
            return Err(MigrationInputError::NoSelectedItemStateFilters);
        }
        if !scope.item_state && !item_state_filters.is_empty() {
            return Err(MigrationInputError::InvalidIdentifier);
        }
        if scope.item_state {
            scope.item_state_filters = Some(item_state_filters);
        }
    }
    if !scope.has_selected_category() {
        return Err(MigrationInputError::NoSelectedMigrationScope);
    }
    if !scope.requires_target_libraries() {
        if let Some(target_library_ids) = scope.target_library_ids.take() {
            // Validate supplied IDs even though these scopes do not consume them, then discard
            // the redundant whitelist so job creation does not perform an unnecessary library
            // lookup or carry irrelevant state into execution.
            normalize_selected_library_ids(&target_library_ids)?;
        }
        return Ok(scope);
    }
    if let Some(target_library_ids) = scope.target_library_ids.take() {
        let target_library_ids = normalize_selected_library_ids(&target_library_ids)?;
        if target_library_ids.is_empty() {
            return Err(MigrationInputError::NoSelectedTargetLibraries);
        }
        scope.target_library_ids = Some(target_library_ids);
    } else {
        // Missing whitelist is the documented compatibility mode for old clients and jobs.
        scope.target_library_ids = None;
    }
    Ok(scope)
}

fn normalize_item_state_filters(
    values: &[MigrationUserStateFilter],
) -> Vec<MigrationUserStateFilter> {
    MigrationUserStateFilter::ALL
        .into_iter()
        .filter(|filter| values.contains(filter))
        .collect()
}

fn normalize_selected_user_ids(values: &[String]) -> Result<Vec<String>, MigrationInputError> {
    let mut selected = Vec::with_capacity(values.len().min(MAX_SELECTED_USER_COUNT));
    let mut seen = HashSet::with_capacity(values.len());
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            return Err(MigrationInputError::InvalidIdentifier);
        }
        if value.len() > 256
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(MigrationInputError::InvalidIdentifier);
        }
        if seen.insert(value.to_owned()) {
            selected.push(value.to_owned());
        }
        if selected.len() > MAX_SELECTED_USER_COUNT {
            return Err(MigrationInputError::InvalidIdentifier);
        }
    }
    if selected.is_empty() {
        return Err(MigrationInputError::NoSelectedUsers);
    }
    Ok(selected)
}

fn normalize_selected_library_ids(values: &[String]) -> Result<Vec<String>, MigrationInputError> {
    let mut selected = Vec::with_capacity(values.len().min(MAX_SELECTED_LIBRARY_COUNT));
    let mut seen = HashSet::with_capacity(values.len());
    for value in values {
        let value = value.trim();
        if value.is_empty()
            || value.len() > 256
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(MigrationInputError::InvalidIdentifier);
        }
        if seen.insert(value.to_owned()) {
            selected.push(value.to_owned());
        }
        if selected.len() > MAX_SELECTED_LIBRARY_COUNT {
            return Err(MigrationInputError::InvalidIdentifier);
        }
    }
    Ok(selected)
}

fn select_migration_users(
    users: Vec<MigrationUser>,
    selected_user_ids_json: &str,
) -> Result<Vec<MigrationUser>, EmbyMigrationServiceError> {
    let selected_user_ids = selected_migration_user_ids(selected_user_ids_json)?;
    let users_by_id = users
        .into_iter()
        .map(|user| (user.id.clone(), user))
        .collect::<HashMap<_, _>>();
    selected_user_ids
        .iter()
        .map(|user_id| {
            users_by_id
                .get(user_id)
                .cloned()
                .ok_or(EmbyMigrationServiceError::InvalidState)
        })
        .collect()
}

fn selected_migration_user_ids(
    selected_user_ids_json: &str,
) -> Result<Vec<String>, EmbyMigrationServiceError> {
    serde_json::from_str::<Vec<String>>(selected_user_ids_json)
        .map_err(|_| EmbyMigrationServiceError::InvalidState)
        .and_then(|ids| normalize_selected_user_ids(&ids).map_err(Into::into))
}

fn hex_user_data_sha256(value: &MigrationUserData) -> Result<String, EmbyMigrationServiceError> {
    let bytes = serde_json::to_vec(value).map_err(|_| EmbyMigrationServiceError::InvalidState)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn hex_selected_item_state(
    value: &StoredItemState,
    fields: EmbyMigrationUserItemStateFields,
) -> Result<String, EmbyMigrationServiceError> {
    let bytes = serde_json::to_vec(&json!({
        "positionTicks": fields.position_ticks.then_some(value.position_ticks),
        "isPlayed": fields.is_played.then_some(value.is_played),
        "isFavorite": fields.is_favorite.then_some(value.is_favorite),
        "playCount": fields.play_count.then_some(value.play_count),
        "lastPlayedAt": fields.last_played_at.then_some(value.last_played_at),
    }))
    .map_err(|_| EmbyMigrationServiceError::InvalidState)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn merge_policy_name(policy: MigrationMergePolicy) -> &'static str {
    match policy {
        MigrationMergePolicy::Merge => "MERGE",
        MigrationMergePolicy::Overwrite => "OVERWRITE",
        MigrationMergePolicy::Skip => "SKIP",
    }
}

#[allow(dead_code)]
fn history_capability_name(capability: HistoryCapability) -> &'static str {
    match capability {
        HistoryCapability::ItemState => "ITEM_STATE",
        HistoryCapability::EventHistory => "EVENT_HISTORY",
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        application::{
            plugin_protocol::{
                EMBY_MIGRATION_CAPABILITY, PLUGIN_CATEGORY_MIGRATION, PLUGIN_TYPE_DATA_MIGRATION,
            },
            plugin_runtime::PluginRuntimeError,
            plugins::{EMBY_MIGRATION_PLUGIN_ID, PluginService},
        },
        config::Config,
    };

    use super::*;
    use std::collections::BTreeMap;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn dropping_migration_page_prefetch_aborts_source_read() {
        let completed = Arc::new(AtomicBool::new(false));
        let task_completed = Arc::clone(&completed);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            task_completed.store(true, Ordering::SeqCst);
            Err::<RecoveredMigrationPage, _>(EmbyMigrationServiceError::InvalidState)
        });
        let prefetch = MigrationPagePrefetch::new(handle);
        drop(prefetch);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!completed.load(Ordering::SeqCst));
    }

    #[test]
    fn selected_user_ids_are_required_and_deduplicated() {
        assert_eq!(
            normalize_selected_user_ids(&[
                " user-1 ".to_owned(),
                "user-1".to_owned(),
                "user-2".to_owned(),
            ])
            .expect("valid selected user IDs"),
            vec!["user-1", "user-2"]
        );
        assert!(matches!(
            normalize_selected_user_ids(&[]),
            Err(MigrationInputError::NoSelectedUsers)
        ));
    }

    #[test]
    fn selected_user_ids_reject_empty_or_unsafe_identifiers() {
        assert!(matches!(
            normalize_selected_user_ids(&["  ".to_owned()]),
            Err(MigrationInputError::InvalidIdentifier)
        ));
        assert!(matches!(
            normalize_selected_user_ids(&["user/1".to_owned()]),
            Err(MigrationInputError::InvalidIdentifier)
        ));
    }

    #[test]
    fn create_request_requires_user_ids() {
        assert!(serde_json::from_str::<CreateMigrationRequest>(r#"{"dryRun":false}"#).is_err());
    }

    #[test]
    fn create_request_defaults_scope_for_legacy_clients() {
        let request: CreateMigrationRequest =
            serde_json::from_str(r#"{"dryRun":false,"embyUserIds":["user-1"]}"#)
                .expect("legacy migration request should remain valid");
        assert_eq!(request.scope, MigrationScope::default());
    }

    fn test_migration_user(id: &str, name: &str) -> MigrationUser {
        MigrationUser {
            id: id.to_owned(),
            name: name.to_owned(),
            has_password: false,
            is_disabled: false,
            is_administrator: false,
            enable_all_folders: true,
            enabled_folders: Vec::new(),
            enable_remote_access: false,
            enable_content_downloading: false,
            primary_image_tag: None,
        }
    }

    #[test]
    fn legacy_source_user_pages_are_searched_and_sliced_locally() {
        let page = MigrationUserPage {
            items: vec![
                test_migration_user("1", "Alice"),
                test_migration_user("2", "Bob"),
            ],
            history_capability: HistoryCapability::ItemState,
            library_folders: None,
            start_index: None,
            total_record_count: None,
            next_start_index: None,
        };

        let projected = project_source_user_page(page, 0, 1, Some("ali"));
        assert_eq!(projected.total, 1);
        assert_eq!(projected.users[0].name, "Alice");
    }

    #[test]
    fn paged_source_user_metadata_is_kept_without_reslicing() {
        let page = MigrationUserPage {
            items: vec![test_migration_user("101", "User 101")],
            history_capability: HistoryCapability::ItemState,
            library_folders: None,
            start_index: Some(100),
            total_record_count: Some(250),
            next_start_index: Some(101),
        };

        let projected = project_source_user_page(page, 100, 100, None);
        assert_eq!(projected.total, 250);
        assert_eq!(projected.users[0].id, "101");
    }

    #[test]
    fn new_scope_requires_an_explicit_category_and_target_libraries_for_media_data() {
        let no_categories = MigrationScope {
            user_profile: false,
            library_access: false,
            item_state: false,
            item_state_filters: None,
            person_favorites: false,
            target_library_ids: Some(Vec::new()),
        };
        assert!(matches!(
            normalize_migration_scope(no_categories),
            Err(MigrationInputError::NoSelectedMigrationScope)
        ));

        let no_target_libraries = MigrationScope {
            item_state: true,
            ..MigrationScope {
                user_profile: false,
                library_access: false,
                item_state: false,
                item_state_filters: None,
                person_favorites: false,
                target_library_ids: Some(Vec::new()),
            }
        };
        assert!(matches!(
            normalize_migration_scope(no_target_libraries),
            Err(MigrationInputError::NoSelectedTargetLibraries)
        ));

        let legacy_scope = MigrationScope {
            item_state: true,
            ..MigrationScope::default()
        };
        assert_eq!(
            normalize_migration_scope(legacy_scope)
                .expect("legacy scope without a target whitelist stays valid")
                .target_library_ids,
            None
        );

        let profile_only_scope = MigrationScope {
            user_profile: true,
            library_access: false,
            item_state: false,
            item_state_filters: None,
            person_favorites: false,
            target_library_ids: Some(vec!["ignored-library".to_owned()]),
        };
        let normalized = normalize_migration_scope(profile_only_scope)
            .expect("profile-only scope should not need target libraries");
        assert_eq!(normalized.target_library_ids, None);
    }

    #[test]
    fn new_scope_rejects_an_enabled_media_state_scope_without_selected_fields() {
        let scope = MigrationScope {
            user_profile: false,
            library_access: false,
            item_state: true,
            item_state_filters: Some(Vec::new()),
            person_favorites: false,
            target_library_ids: Some(vec!["library-1".to_owned()]),
        };

        assert!(matches!(
            normalize_migration_scope(scope),
            Err(MigrationInputError::NoSelectedItemStateFilters)
        ));
    }

    #[test]
    fn selected_item_state_filters_only_enable_their_storage_columns() {
        let favorite_scope = MigrationScope {
            user_profile: false,
            library_access: false,
            item_state: true,
            item_state_filters: Some(vec![MigrationUserStateFilter::Favorite]),
            person_favorites: false,
            target_library_ids: Some(vec!["library-1".to_owned()]),
        };

        assert_eq!(
            state_fields_for_migration(&favorite_scope),
            EmbyMigrationUserItemStateFields {
                position_ticks: false,
                is_played: false,
                is_favorite: true,
                play_count: false,
                last_played_at: false,
            }
        );
        assert_eq!(
            state_fields_for_migration(&MigrationScope::default()),
            EmbyMigrationUserItemStateFields::all()
        );
    }

    #[test]
    fn migration_user_fields_follow_selected_scope() {
        let profile_only = MigrationScope {
            user_profile: true,
            library_access: false,
            item_state: false,
            item_state_filters: None,
            person_favorites: false,
            target_library_ids: None,
        };
        assert_eq!(
            user_fields_for_migration(&profile_only),
            vec![
                "id",
                "name",
                "hasPassword",
                "isDisabled",
                "enableRemoteAccess",
                "enableContentDownloading"
            ]
        );

        let media_only = MigrationScope {
            user_profile: false,
            library_access: false,
            item_state: true,
            item_state_filters: Some(vec![MigrationUserStateFilter::Favorite]),
            person_favorites: false,
            target_library_ids: Some(vec!["library-1".to_owned()]),
        };
        assert_eq!(
            user_fields_for_migration(&media_only),
            vec![
                "id",
                "name",
                "enableAllFolders",
                "enabledFolders",
                "libraryFolders"
            ]
        );
    }

    #[test]
    fn media_identity_page_cache_is_bounded_and_reuses_equivalent_keys() {
        let mut cache = MigrationMediaIdentityCache::new(2, 2);
        let first_key = MigrationMediaIdentityCacheKey {
            lookups: Vec::new(),
            target_library_ids: None,
        };
        let first_index = MigrationMediaIdentityIndex::new(Vec::new());

        cache.insert(first_key.clone(), first_index);
        assert!(cache.get(&first_key).is_some());

        for target_library_id in ["library-1", "library-2", "library-3"] {
            cache.insert(
                MigrationMediaIdentityCacheKey {
                    lookups: Vec::new(),
                    target_library_ids: Some(vec![target_library_id.to_owned()]),
                },
                MigrationMediaIdentityIndex::new(Vec::new()),
            );
        }

        assert!(cache.len() <= 2);

        let mut identity_limited_cache = MigrationMediaIdentityCache::new(4, 1);
        identity_limited_cache.insert(
            MigrationMediaIdentityCacheKey {
                lookups: Vec::new(),
                target_library_ids: None,
            },
            MigrationMediaIdentityIndex::new(vec![StoredMigrationMediaIdentity {
                id: "identity-1".to_owned(),
                library_id: "library-1".to_owned(),
                item_type: "MOVIE".to_owned(),
                title: "Film".to_owned(),
                production_year: None,
                provider_ids_json: None,
                series_id: None,
                season_number: None,
                episode_number: None,
            }]),
        );
        identity_limited_cache.insert(
            MigrationMediaIdentityCacheKey {
                lookups: Vec::new(),
                target_library_ids: Some(vec!["library-2".to_owned()]),
            },
            MigrationMediaIdentityIndex::new(vec![StoredMigrationMediaIdentity {
                id: "identity-2".to_owned(),
                library_id: "library-2".to_owned(),
                item_type: "MOVIE".to_owned(),
                title: "Film 2".to_owned(),
                production_year: None,
                provider_ids_json: None,
                series_id: None,
                season_number: None,
                episode_number: None,
            }]),
        );
        assert!(identity_limited_cache.len() <= 1);
    }

    #[test]
    fn person_identity_page_cache_is_bounded_and_reuses_equivalent_keys() {
        let mut cache = MigrationPersonIdentityCache::new(2, 2);
        let first_key = MigrationPersonIdentityCacheKey {
            lookups: vec![MigrationPersonIdentityLookup {
                normalized_name: "actor".to_owned(),
                provider_ids: vec![("tmdb".to_owned(), "42".to_owned())],
            }],
        };
        let first_index = MigrationPersonIdentityIndex::new(vec![]);

        cache.insert(first_key.clone(), first_index);
        assert!(cache.get(&first_key).is_some());

        for name in ["actor-a", "actor-b", "actor-c"] {
            cache.insert(
                MigrationPersonIdentityCacheKey {
                    lookups: vec![MigrationPersonIdentityLookup {
                        normalized_name: name.to_owned(),
                        provider_ids: Vec::new(),
                    }],
                },
                MigrationPersonIdentityIndex::new(Vec::new()),
            );
        }

        assert!(cache.len() <= 2);

        let mut identity_limited_cache = MigrationPersonIdentityCache::new(4, 1);
        identity_limited_cache.insert(
            MigrationPersonIdentityCacheKey {
                lookups: Vec::new(),
            },
            MigrationPersonIdentityIndex::new(vec![StoredMigrationPersonIdentity {
                id: "person-1".to_owned(),
                display_name: "Actor".to_owned(),
                provider: Some("tmdb".to_owned()),
                provider_id: Some("42".to_owned()),
            }]),
        );
        identity_limited_cache.insert(
            MigrationPersonIdentityCacheKey {
                lookups: vec![MigrationPersonIdentityLookup {
                    normalized_name: "actor-2".to_owned(),
                    provider_ids: Vec::new(),
                }],
            },
            MigrationPersonIdentityIndex::new(vec![StoredMigrationPersonIdentity {
                id: "person-2".to_owned(),
                display_name: "Actor 2".to_owned(),
                provider: None,
                provider_id: None,
            }]),
        );
        assert!(identity_limited_cache.len() <= 1);
    }

    #[test]
    fn favorite_only_state_hash_ignores_unselected_playback_values() {
        let fields = EmbyMigrationUserItemStateFields {
            position_ticks: false,
            is_played: false,
            is_favorite: true,
            play_count: false,
            last_played_at: false,
        };
        let first = StoredItemState {
            position_ticks: 12,
            is_played: true,
            is_favorite: true,
            play_count: 4,
            last_played_at: Some(100),
        };
        let second = StoredItemState {
            position_ticks: 99,
            is_played: false,
            is_favorite: true,
            play_count: 0,
            last_played_at: None,
        };

        assert_eq!(
            hex_selected_item_state(&first, fields).expect("first hash"),
            hex_selected_item_state(&second, fields).expect("second hash"),
        );
    }

    #[test]
    fn target_library_whitelist_restricts_state_and_acl_updates() {
        let target_libraries = HashSet::from(["library-allowed".to_owned()]);

        assert!(target_library_is_selected(
            Some(&target_libraries),
            Some("library-allowed")
        ));
        assert!(!target_library_is_selected(
            Some(&target_libraries),
            Some("library-excluded")
        ));
        assert!(!target_library_is_selected(Some(&target_libraries), None));
        assert!(target_library_is_selected(None, Some("library-legacy")));

        assert_eq!(
            restrict_library_access_plan(
                LibraryAccessPlan::Exact(HashSet::from([
                    "library-allowed".to_owned(),
                    "library-excluded".to_owned(),
                ])),
                Some(&target_libraries),
            ),
            LibraryAccessPlan::Exact(HashSet::from(["library-allowed".to_owned()])),
        );
    }

    #[test]
    fn source_library_filter_contains_only_selected_mapped_folders() {
        let user = MigrationUser {
            id: "emby-user".to_owned(),
            name: "Alice".to_owned(),
            has_password: false,
            is_disabled: false,
            is_administrator: false,
            enable_all_folders: false,
            enabled_folders: vec!["source-a".to_owned(), "source-b".to_owned()],
            enable_remote_access: false,
            enable_content_downloading: false,
            primary_image_tag: None,
        };
        let source_folders = vec![
            MigrationLibraryFolder {
                id: "source-a".to_owned(),
                name: "Movies".to_owned(),
                locations: vec!["/media/movies".to_owned()],
            },
            MigrationLibraryFolder {
                id: "source-b".to_owned(),
                name: "TV".to_owned(),
                locations: vec!["/media/tv".to_owned()],
            },
            MigrationLibraryFolder {
                id: "source-c".to_owned(),
                name: "Other".to_owned(),
                locations: vec!["/media/other".to_owned()],
            },
        ];
        let lux_libraries = vec![
            MigrationLuxLibraryIdentity {
                id: "lux-movies".to_owned(),
                name: "Movies".to_owned(),
                root_paths: vec!["/media/movies".to_owned()],
            },
            MigrationLuxLibraryIdentity {
                id: "lux-tv".to_owned(),
                name: "TV".to_owned(),
                root_paths: vec!["/media/tv".to_owned()],
            },
        ];

        let source_ids = source_library_ids_for_user(
            &user,
            Some(&source_folders),
            &lux_libraries,
            &HashSet::from(["lux-tv".to_owned()]),
        )
        .expect("source folder mapping should be available");
        assert_eq!(source_ids, vec!["source-b"]);

        let all_enabled_library_ids = HashSet::from(["lux-movies".to_owned(), "lux-tv".to_owned()]);
        let source_ids = source_library_ids_for_user(
            &user,
            Some(&source_folders),
            &lux_libraries,
            &all_enabled_library_ids,
        )
        .expect("legacy source folder mapping should be available");
        assert_eq!(source_ids, vec!["source-a", "source-b"]);
    }

    #[test]
    fn precomputed_source_folder_mapping_reuses_resolved_lux_ids() {
        let user = MigrationUser {
            id: "emby-user".to_owned(),
            name: "Alice".to_owned(),
            has_password: false,
            is_disabled: false,
            is_administrator: false,
            enable_all_folders: false,
            enabled_folders: vec!["source-a".to_owned(), "source-b".to_owned()],
            enable_remote_access: false,
            enable_content_downloading: false,
            primary_image_tag: None,
        };
        let source_folders = vec![
            MigrationLibraryFolder {
                id: "source-a".to_owned(),
                name: "Movies".to_owned(),
                locations: vec!["/media/movies".to_owned()],
            },
            MigrationLibraryFolder {
                id: "source-b".to_owned(),
                name: "TV".to_owned(),
                locations: vec!["/media/tv".to_owned()],
            },
        ];
        let lux_libraries = vec![
            MigrationLuxLibraryIdentity {
                id: "lux-movies".to_owned(),
                name: "Movies".to_owned(),
                root_paths: vec!["/media/movies".to_owned()],
            },
            MigrationLuxLibraryIdentity {
                id: "lux-tv".to_owned(),
                name: "TV".to_owned(),
                root_paths: vec!["/media/tv".to_owned()],
            },
        ];
        let mappings = precompute_source_library_mappings(&source_folders, &lux_libraries);

        assert_eq!(mappings.len(), 2);
        assert_eq!(mappings[0].lux_library_id.as_deref(), Some("lux-movies"));
        assert_eq!(mappings[1].lux_library_id.as_deref(), Some("lux-tv"));
        assert_eq!(
            map_enabled_library_ids_checked_with_mappings(&user, &mappings),
            Some(HashSet::from([
                "lux-movies".to_owned(),
                "lux-tv".to_owned(),
            ]))
        );
        assert_eq!(
            source_library_ids_for_user_with_mappings(
                &user,
                &mappings,
                &HashSet::from(["lux-tv".to_owned()]),
            ),
            vec!["source-b"]
        );
    }

    #[test]
    fn empty_filtered_source_scope_skips_state_reads() {
        assert!(!source_filter_has_candidates(true, Some(&[])));
        assert!(source_filter_has_candidates(true, None));
        assert!(source_filter_has_candidates(false, Some(&[])));
    }

    #[test]
    fn rate_limited_page_disables_the_next_read_ahead() {
        assert!(!should_prefetch_source_page(1));
        assert!(should_prefetch_source_page(0));
    }

    #[test]
    fn person_favorites_only_scope_enables_source_library_filtering() {
        let scope = MigrationScope {
            user_profile: false,
            library_access: false,
            item_state: false,
            item_state_filters: None,
            person_favorites: true,
            target_library_ids: None,
        };
        let user = MigrationUser {
            id: "emby-user".to_owned(),
            name: "Alice".to_owned(),
            has_password: false,
            is_disabled: false,
            is_administrator: false,
            enable_all_folders: false,
            enabled_folders: vec!["source-movies".to_owned()],
            enable_remote_access: false,
            enable_content_downloading: false,
            primary_image_tag: None,
        };

        assert!(migration_source_filtering_enabled(
            true,
            &scope,
            None,
            &[user]
        ));
    }

    #[test]
    fn all_folder_users_fall_back_to_target_filter_when_source_mapping_is_incomplete() {
        let user = MigrationUser {
            id: "emby-user".to_owned(),
            name: "Alice".to_owned(),
            has_password: false,
            is_disabled: false,
            is_administrator: false,
            enable_all_folders: true,
            enabled_folders: Vec::new(),
            enable_remote_access: false,
            enable_content_downloading: false,
            primary_image_tag: None,
        };
        let mappings = vec![
            MigrationSourceLibraryMapping {
                source_id: "source-movies".to_owned(),
                lux_library_id: Some("lux-movies".to_owned()),
            },
            MigrationSourceLibraryMapping {
                source_id: "source-unknown".to_owned(),
                lux_library_id: None,
            },
        ];

        assert_eq!(
            source_library_filter_for_user(
                &user,
                &mappings,
                &HashSet::from(["lux-movies".to_owned()]),
            ),
            None,
        );
    }

    #[tokio::test]
    async fn execution_gate_admits_a_job_once_until_it_is_released() {
        let gate = MigrationExecutionGate::default();

        assert!(gate.claim("job-1").await);
        assert!(!gate.claim("job-1").await);
        gate.release("job-1").await;
        assert!(gate.claim("job-1").await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn target_library_whitelist_skips_excluded_media_without_state_import_or_acl_write()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path().join("config");
        write_fake_emby_migration_plugin(&config_dir)?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: config_dir.clone(),
        })
        .await?;
        let user_id = Uuid::now_v7().to_string();
        let allowed_library_id = "allowed-library";
        let excluded_library_id = "excluded-library";
        let excluded_item_id = "excluded-item";
        database
            .insert_initial_user(&user_id, "alice", "Alice", "hash")
            .await?;
        for (library_id, name) in [
            (allowed_library_id, "Allowed library"),
            (excluded_library_id, "Excluded library"),
        ] {
            sqlx::query("INSERT INTO libraries (id, name, kind) VALUES (?, ?, 'MOVIE')")
                .bind(library_id)
                .bind(name)
                .execute(database.pool())
                .await?;
        }
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, production_year,
                 provider_ids_json, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 2024, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(excluded_item_id)
        .bind(excluded_library_id)
        .bind("Excluded film")
        .bind("excluded film")
        .bind(r#"{"tmdb":"42"}"#)
        .execute(database.pool())
        .await?;

        let plugins = PluginService::new(database.clone(), config_dir.clone());
        plugins.install(EMBY_MIGRATION_PLUGIN_ID).await?;
        plugins
            .update_dynamic_config(
                EMBY_MIGRATION_PLUGIN_ID,
                serde_json::Map::from_iter([
                    ("baseUrl".to_owned(), json!("http://emby.local:8096")),
                    ("apiKey".to_owned(), json!("test-api-key")),
                    ("allowPrivateNetwork".to_owned(), json!(true)),
                ]),
            )
            .await?;
        let service = EmbyMigrationService::new(database.clone(), plugins, config_dir);
        let job = service
            .create_job(
                &user_id,
                CreateMigrationRequest {
                    dry_run: false,
                    merge_policy: MigrationMergePolicy::Merge,
                    emby_user_ids: vec!["emby-user".to_owned()],
                    scope: MigrationScope {
                        user_profile: false,
                        library_access: true,
                        item_state: true,
                        item_state_filters: None,
                        person_favorites: false,
                        target_library_ids: Some(vec![allowed_library_id.to_owned()]),
                    },
                },
            )
            .await?;

        service.run(&job.id).await?;

        let matches = service.list_item_matches(&job.id, 0, 10).await?;
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].status, "SKIPPED");
        assert_eq!(matches[0].match_method, "TMDB_ID");
        assert_eq!(
            matches[0].detail["migrationSkipReason"],
            "TARGET_LIBRARY_EXCLUDED"
        );
        assert!(
            service
                .list_import_records(&job.id, 0, 10)
                .await?
                .is_empty()
        );
        assert!(
            database
                .find_user_item_state_for_migration(&user_id, excluded_item_id)
                .await?
                .is_none()
        );
        let excluded_acl_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_library_access
             WHERE user_id = ? AND library_id = ?",
        )
        .bind(&user_id)
        .bind(excluded_library_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(excluded_acl_rows, 0);
        let allowed_acl_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_library_access
             WHERE user_id = ? AND library_id = ? AND can_view = 1",
        )
        .bind(&user_id)
        .bind(allowed_library_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(allowed_acl_rows, 1);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn item_state_pages_use_bounded_read_ahead_and_preserve_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path().join("config");
        write_paged_emby_migration_plugin(&config_dir)?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: config_dir.clone(),
        })
        .await?;
        let lux_user_id = Uuid::now_v7().to_string();
        database
            .insert_initial_user(&lux_user_id, "alice", "Alice", "hash")
            .await?;
        sqlx::query("INSERT INTO libraries (id, name, kind) VALUES (?, ?, 'MOVIE')")
            .bind("movies-library")
            .bind("Movies")
            .execute(database.pool())
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, production_year,
                 provider_ids_json, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind("lux-movie")
        .bind("movies-library")
        .bind("Selected film")
        .bind("selected film")
        .bind(2024_i64)
        .bind(r#"{"tmdb":"42"}"#)
        .execute(database.pool())
        .await?;

        let plugins = PluginService::new(database.clone(), config_dir.clone());
        plugins.install(EMBY_MIGRATION_PLUGIN_ID).await?;
        plugins
            .update_dynamic_config(
                EMBY_MIGRATION_PLUGIN_ID,
                serde_json::Map::from_iter([
                    ("baseUrl".to_owned(), json!("http://emby.local:8096")),
                    ("apiKey".to_owned(), json!("test-api-key")),
                    ("allowPrivateNetwork".to_owned(), json!(true)),
                ]),
            )
            .await?;
        let service = EmbyMigrationService::new(database.clone(), plugins, config_dir.clone());
        let job = service
            .create_job(
                &lux_user_id,
                CreateMigrationRequest {
                    dry_run: false,
                    merge_policy: MigrationMergePolicy::Merge,
                    emby_user_ids: vec!["emby-user".to_owned()],
                    scope: MigrationScope {
                        user_profile: false,
                        library_access: false,
                        item_state: true,
                        item_state_filters: Some(vec![MigrationUserStateFilter::Played]),
                        person_favorites: false,
                        target_library_ids: Some(vec!["movies-library".to_owned()]),
                    },
                },
            )
            .await?;

        service.run(&job.id).await?;
        let matches = service.list_item_matches(&job.id, 0, 10).await?;
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].status, "MATCHED");
        let calls = tokio::fs::read_to_string(
            config_dir
                .join("plugins")
                .join(EMBY_MIGRATION_PLUGIN_ID)
                .join("migration-calls.jsonl"),
        )
        .await?;
        let state_calls = calls
            .lines()
            .filter(|line| line.contains("migration.user_state"))
            .collect::<Vec<_>>();
        assert_eq!(state_calls.len(), 2);
        assert!(state_calls[0].contains("\"startIndex\":0"));
        assert!(state_calls[1].contains("\"startIndex\":500"));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn favorite_only_migration_forwards_only_selected_source_filter()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path().join("config");
        write_filter_observing_emby_migration_plugin(&config_dir)?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: config_dir.clone(),
        })
        .await?;
        let lux_user_id = Uuid::now_v7().to_string();
        let library_id = "movies-library";
        database
            .insert_initial_user(&lux_user_id, "alice", "Alice", "hash")
            .await?;
        sqlx::query("INSERT INTO libraries (id, name, kind) VALUES (?, ?, 'MOVIE')")
            .bind(library_id)
            .bind("Movies")
            .execute(database.pool())
            .await?;
        sqlx::query(
            "INSERT INTO library_roots (
                id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES (?, ?, ?, ?, 1, 1)",
        )
        .bind("movies-root")
        .bind(library_id)
        .bind("/media/movies")
        .bind("/media/movies")
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, provider_ids_json,
                identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind("lux-movie")
        .bind(library_id)
        .bind("Favourite film")
        .bind("favourite film")
        .bind(r#"{"tmdb":"42"}"#)
        .execute(database.pool())
        .await?;

        let plugins = PluginService::new(database.clone(), config_dir.clone());
        plugins.install(EMBY_MIGRATION_PLUGIN_ID).await?;
        plugins
            .update_dynamic_config(
                EMBY_MIGRATION_PLUGIN_ID,
                serde_json::Map::from_iter([
                    ("baseUrl".to_owned(), json!("http://emby.local:8096")),
                    ("apiKey".to_owned(), json!("test-api-key")),
                    ("allowPrivateNetwork".to_owned(), json!(true)),
                ]),
            )
            .await?;
        let service = EmbyMigrationService::new(database.clone(), plugins, config_dir.clone());
        let job = service
            .create_job(
                &lux_user_id,
                CreateMigrationRequest {
                    dry_run: false,
                    merge_policy: MigrationMergePolicy::Merge,
                    emby_user_ids: vec!["emby-user".to_owned()],
                    scope: MigrationScope {
                        user_profile: false,
                        library_access: false,
                        item_state: true,
                        item_state_filters: Some(vec![MigrationUserStateFilter::Favorite]),
                        person_favorites: true,
                        // Omit the target whitelist to exercise the legacy
                        // compatibility path: a restricted user should
                        // still push its safely mapped source library.
                        target_library_ids: None,
                    },
                },
            )
            .await?;

        service.run(&job.id).await?;
        let calls = tokio::fs::read_to_string(
            config_dir
                .join("plugins")
                .join(EMBY_MIGRATION_PLUGIN_ID)
                .join("migration-calls.jsonl"),
        )
        .await?;
        let state_calls = calls
            .lines()
            .filter(|line| line.contains("migration.user_state"))
            .collect::<Vec<_>>();
        let user_calls = calls
            .lines()
            .filter(|line| line.contains("migration.list_users"))
            .collect::<Vec<_>>();
        assert_eq!(user_calls.len(), 1);
        assert!(user_calls[0].contains("\"userIds\":[\"emby-user\"]"));
        assert!(user_calls[0].contains(
            "\"userFields\":[\"id\",\"name\",\"enableAllFolders\",\"enabledFolders\",\"libraryFolders\"]"
        ));
        assert_eq!(state_calls.len(), 1);
        assert!(state_calls[0].contains("\"stateFilter\":\"FAVORITE\""));
        assert!(state_calls[0].contains("\"stateFields\":[\"isFavorite\"]"));
        assert!(state_calls[0].contains("\"sourceLibraryIds\":[\"source-movies\"]"));
        let person_calls = calls
            .lines()
            .filter(|line| line.contains("migration.person_favorites"))
            .collect::<Vec<_>>();
        assert_eq!(person_calls.len(), 1);
        assert!(person_calls[0].contains("\"sourceLibraryIds\":[\"source-movies\"]"));
        assert!(!calls.contains("\"stateFilter\":\"PLAYED\""));
        assert!(!calls.contains("\"stateFilter\":\"RESUMABLE\""));
        Ok(())
    }

    #[cfg(unix)]
    fn write_fake_emby_migration_plugin(
        config_dir: &std::path::Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use std::{fs, os::unix::fs::PermissionsExt};

        let plugin_root = config_dir.join(format!("plugins/{EMBY_MIGRATION_PLUGIN_ID}"));
        let binary_dir = plugin_root.join("binaries");
        fs::create_dir_all(&binary_dir)?;
        let entrypoint = binary_dir.join("plugin");
        fs::write(
            &entrypoint,
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
  case "$method" in
    migration.test)
      result='{"serverName":"Fixture Emby","historyCapability":"ITEM_STATE"}'
      ;;
    migration.list_users)
      result='{"items":[{"id":"emby-user","name":"Alice","hasPassword":false,"isDisabled":false,"isAdministrator":false,"enableAllFolders":true,"enabledFolders":[],"enableRemoteAccess":false,"enableContentDownloading":false,"primaryImageTag":null}],"historyCapability":"ITEM_STATE"}'
      ;;
    migration.user_state)
      result='{"items":[{"id":"emby-item","name":"Excluded film","itemType":"Movie","productionYear":2024,"providerIds":{"tmdb":"42"},"parentId":null,"seriesId":null,"seasonId":null,"indexNumber":null,"parentIndexNumber":null,"userData":{"playbackPositionTicks":1,"played":true,"isFavorite":false,"playCount":1,"lastPlayedDate":null}}],"startIndex":0,"totalRecordCount":1,"nextStartIndex":null,"historyCapability":"ITEM_STATE"}'
      ;;
    *)
      result='{"items":[],"startIndex":0,"totalRecordCount":0,"nextStartIndex":null,"historyCapability":"ITEM_STATE"}'
      ;;
  esac
  printf '{"id":"%s","result":%s}\n' "$id" "$result"
done
"#,
        )?;
        fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o700))?;
        fs::write(
            plugin_root.join("manifest.json"),
            serde_json::to_vec_pretty(&json!({
                "formatVersion": 1,
                "id": EMBY_MIGRATION_PLUGIN_ID,
                "name": "Fixture Emby migration",
                "version": "1.0.0",
                "apiVersion": 1,
                "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
                "type": PLUGIN_TYPE_DATA_MIGRATION,
                "category": PLUGIN_CATEGORY_MIGRATION,
                "capabilities": [EMBY_MIGRATION_CAPABILITY],
                "configFields": [
                    {"key": "baseUrl", "label": "Emby URL", "type": "text", "required": true},
                    {"key": "apiKey", "label": "Emby API key", "type": "password", "required": true, "sensitive": true},
                    {"key": "allowPrivateNetwork", "label": "Private network", "type": "toggle", "defaultValue": false}
                ],
                "permissions": {"network": ["example.invalid"], "filesystem": []},
                "files": []
            }))?,
        )?;
        Ok(())
    }

    #[cfg(unix)]
    fn write_paged_emby_migration_plugin(
        config_dir: &std::path::Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use std::{fs, os::unix::fs::PermissionsExt};

        write_fake_emby_migration_plugin(config_dir)?;
        let entrypoint = config_dir.join(format!(
            "plugins/{EMBY_MIGRATION_PLUGIN_ID}/binaries/plugin"
        ));
        fs::write(
            &entrypoint,
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
  start_index=$(printf '%s' "$line" | sed -n 's/.*"startIndex":\([0-9]*\).*/\1/p')
  printf '%s\n' "$line" >> migration-calls.jsonl
  case "$method" in
    migration.test)
      result='{"serverName":"Paged Fixture Emby","historyCapability":"ITEM_STATE","supportsFilteredReads":true}'
      ;;
    migration.list_users)
      result='{"items":[{"id":"emby-user","name":"Alice","hasPassword":false,"isDisabled":false,"isAdministrator":false,"enableAllFolders":true,"enabledFolders":[],"enableRemoteAccess":false,"enableContentDownloading":false,"primaryImageTag":null}],"historyCapability":"ITEM_STATE"}'
      ;;
    migration.user_state)
      if [ "$start_index" = "0" ]; then
          result='{"items":[{"id":"emby-item","name":"Selected film","itemType":"Movie","productionYear":2024,"providerIds":{"tmdb":"42"},"parentId":null,"seriesId":null,"seasonId":null,"indexNumber":null,"parentIndexNumber":null,"userData":{"playbackPositionTicks":1,"played":true,"isFavorite":false,"playCount":1,"lastPlayedDate":null}}],"startIndex":0,"totalRecordCount":1000,"nextStartIndex":500,"historyCapability":"ITEM_STATE"}'
      else
          result='{"items":[],"startIndex":500,"totalRecordCount":1000,"nextStartIndex":null,"historyCapability":"ITEM_STATE"}'
      fi
      ;;
    *)
      result='{"items":[],"startIndex":0,"totalRecordCount":0,"nextStartIndex":null,"historyCapability":"ITEM_STATE"}'
      ;;
  esac
  printf '{"id":"%s","result":%s}\n' "$id" "$result"
done
"#,
        )?;
        fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    #[cfg(unix)]
    fn write_filter_observing_emby_migration_plugin(
        config_dir: &std::path::Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use std::{fs, os::unix::fs::PermissionsExt};

        let plugin_root = config_dir.join(format!("plugins/{EMBY_MIGRATION_PLUGIN_ID}"));
        let binary_dir = plugin_root.join("binaries");
        fs::create_dir_all(&binary_dir)?;
        let entrypoint = binary_dir.join("plugin");
        fs::write(
            &entrypoint,
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
  printf '%s\n' "$line" >> migration-calls.jsonl
  case "$method" in
    plugin.hello)
      result='{"protocolVersion":1,"pluginId":"org.lux.emby-migration","capabilities":["migration.emby"]}'
      ;;
    migration.test)
      result='{"serverName":"Fixture Emby","historyCapability":"ITEM_STATE","supportsFilteredReads":true}'
      ;;
    migration.list_users)
      result='{"items":[{"id":"emby-user","name":"Alice","hasPassword":false,"isDisabled":false,"isAdministrator":false,"enableAllFolders":false,"enabledFolders":["source-movies"],"enableRemoteAccess":false,"enableContentDownloading":false,"primaryImageTag":null}],"historyCapability":"ITEM_STATE","libraryFolders":[{"id":"source-movies","name":"Movies","locations":["/media/movies"]}]}'
      ;;
    migration.user_state)
      case "$line" in
        *'"stateFilter":"FAVORITE"'*)
          result='{"items":[{"id":"emby-item","name":"Favourite film","itemType":"Movie","productionYear":null,"providerIds":{"tmdb":"42"},"parentId":null,"seriesId":null,"seasonId":null,"indexNumber":null,"parentIndexNumber":null,"userData":{"isFavorite":true}}],"startIndex":0,"totalRecordCount":1,"nextStartIndex":null,"historyCapability":"ITEM_STATE"}'
          ;;
        *)
          result='{"items":[],"startIndex":0,"totalRecordCount":0,"nextStartIndex":null,"historyCapability":"ITEM_STATE"}'
          ;;
      esac
      ;;
    migration.person_favorites)
      result='{"items":[],"startIndex":0,"totalRecordCount":0,"nextStartIndex":null,"historyCapability":"ITEM_STATE"}'
      ;;
    *)
      result='{"items":[],"startIndex":0,"totalRecordCount":0,"nextStartIndex":null,"historyCapability":"ITEM_STATE"}'
      ;;
  esac
  printf '{"id":"%s","result":%s}\n' "$id" "$result"
done
"#,
        )?;
        fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o700))?;
        fs::write(
            plugin_root.join("manifest.json"),
            serde_json::to_vec_pretty(&json!({
                "formatVersion": 1,
                "id": EMBY_MIGRATION_PLUGIN_ID,
                "name": "Fixture Emby migration with filtering",
                "version": "1.0.0",
                "apiVersion": 1,
                "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
                "type": PLUGIN_TYPE_DATA_MIGRATION,
                "category": PLUGIN_CATEGORY_MIGRATION,
                "capabilities": [EMBY_MIGRATION_CAPABILITY],
                "configFields": [
                    {"key": "baseUrl", "label": "Emby URL", "type": "text", "required": true},
                    {"key": "apiKey", "label": "Emby API key", "type": "password", "required": true, "sensitive": true},
                    {"key": "allowPrivateNetwork", "label": "Private network", "type": "toggle", "defaultValue": false}
                ],
                "permissions": {"network": ["example.invalid"], "filesystem": []},
                "files": []
            }))?,
        )?;
        Ok(())
    }

    #[test]
    fn migration_cursor_advances_to_next_page_without_replaying_successful_page() {
        let users = vec!["user-1".to_owned(), "user-2".to_owned()];
        let scope = MigrationScope::default();
        let cursor = next_migration_cursor(
            &users,
            0,
            MigrationPageKind::UserState(MigrationUserStateFilter::Played),
            Some(MigrationUserStateFilter::Played),
            Some(500),
            &scope,
        )
        .expect("next page cursor");
        assert_eq!(cursor.user_id.as_deref(), Some("user-1"));
        assert_eq!(cursor.kind.as_deref(), Some("USER_STATE"));
        assert_eq!(cursor.state_filter, Some(MigrationUserStateFilter::Played));
        assert_eq!(cursor.start_index, 500);
    }

    #[test]
    fn migration_cursor_moves_from_last_state_page_to_person_favorites() {
        let users = vec!["user-1".to_owned()];
        let scope = MigrationScope::default();
        let cursor = next_migration_cursor(
            &users,
            0,
            MigrationPageKind::UserState(MigrationUserStateFilter::Resumable),
            Some(MigrationUserStateFilter::Resumable),
            None,
            &scope,
        )
        .expect("person favorites cursor");
        assert!(cursor.is_person_favorites());
        assert_eq!(cursor.start_index, 0);
    }

    #[test]
    fn migration_cursor_skips_unselected_item_state_filters() {
        let users = vec!["user-1".to_owned()];
        let scope = MigrationScope {
            user_profile: false,
            library_access: false,
            item_state: true,
            item_state_filters: Some(vec![MigrationUserStateFilter::Favorite]),
            person_favorites: false,
            target_library_ids: Some(vec!["library-1".to_owned()]),
        };

        assert_eq!(
            next_migration_cursor(
                &users,
                0,
                MigrationPageKind::UserState(MigrationUserStateFilter::Favorite),
                Some(MigrationUserStateFilter::Favorite),
                None,
                &scope,
            ),
            None
        );
    }

    #[test]
    fn invalid_or_empty_migration_cursor_starts_from_beginning() {
        assert_eq!(MigrationResumeCursor::parse("{}"), None);
        assert_eq!(MigrationResumeCursor::parse("not-json"), None);
        let cursor = MigrationResumeCursor::parse(
            r#"{"kind":"USER_STATE","userId":"user-1","stateFilter":"PLAYED","startIndex":500}"#,
        )
        .expect("valid cursor");
        assert!(cursor.is_state(MigrationUserStateFilter::Played));
    }

    #[test]
    fn fresh_users_skip_durable_handled_item_lookup() {
        assert!(!should_lookup_handled_items(None, 0, None, 0));
        assert!(should_lookup_handled_items(None, 3, None, 0));

        let cursor = MigrationResumeCursor {
            kind: Some("USER_STATE".to_owned()),
            user_id: Some("user-1".to_owned()),
            state_filter: Some(MigrationUserStateFilter::Played),
            start_index: 500,
        };
        assert!(should_lookup_handled_items(Some(&cursor), 0, Some(0), 0));
        assert!(!should_lookup_handled_items(Some(&cursor), 0, Some(0), 1));
        assert!(should_lookup_handled_items(Some(&cursor), 0, None, 0));
    }

    #[test]
    fn migration_user_selection_excludes_unselected_users() {
        let users = vec![
            migration_test_user("user-1", "Alice"),
            migration_test_user("user-2", "Bob"),
        ];

        let selected = select_migration_users(users, r#"["user-2"]"#)
            .expect("selected user should be present");

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "user-2");
    }

    fn migration_test_user(id: &str, name: &str) -> MigrationUser {
        MigrationUser {
            id: id.to_owned(),
            name: name.to_owned(),
            has_password: false,
            is_disabled: false,
            is_administrator: false,
            enable_all_folders: true,
            enabled_folders: Vec::new(),
            enable_remote_access: false,
            enable_content_downloading: false,
            primary_image_tag: None,
        }
    }

    fn migration_test_item(id: &str, name: &str) -> MigrationItem {
        MigrationItem {
            id: id.to_owned(),
            name: name.to_owned(),
            item_type: "Movie".to_owned(),
            production_year: Some(2024),
            provider_ids: BTreeMap::new(),
            parent_id: None,
            series_id: None,
            season_id: None,
            index_number: None,
            parent_index_number: None,
            user_data: Some(MigrationUserData {
                playback_position_ticks: 1,
                played: true,
                is_favorite: false,
                play_count: 1,
                last_played_date: None,
            }),
        }
    }

    #[test]
    fn plugin_invalid_response_is_recoverable_when_returned_by_rpc() {
        assert!(is_invalid_migration_response(
            &PluginServiceError::InvalidResponse
        ));
        assert!(is_invalid_migration_response(&PluginServiceError::Runtime(
            PluginRuntimeError::Plugin {
                code: "PLUGIN_INVALID_RESPONSE".to_owned(),
                message: "invalid item".to_owned(),
            }
        )));
        assert!(!is_invalid_migration_response(
            &PluginServiceError::Runtime(PluginRuntimeError::Plugin {
                code: "PLUGIN_AUTH_FAILED".to_owned(),
                message: "authentication failed".to_owned(),
            })
        ));
    }

    #[tokio::test]
    async fn rate_limited_source_calls_retry_with_a_bounded_attempt_count() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let operation_attempts = Arc::clone(&attempts);
        let result = retry_rate_limited_source_call(|| {
            let attempt = operation_attempts.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if attempt < 3 {
                    Err(PluginServiceError::Runtime(PluginRuntimeError::Plugin {
                        code: "PLUGIN_RATE_LIMITED".to_owned(),
                        message: "retry".to_owned(),
                    }))
                } else {
                    Ok(attempt)
                }
            }
        })
        .await
        .expect("a transient source limit should recover");

        assert_eq!(result.value, 3);
        assert_eq!(result.attempts, 3);
        assert_eq!(result.rate_limited_responses, 2);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn rate_limited_source_calls_stop_after_the_retry_budget() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let operation_attempts = Arc::clone(&attempts);
        let result = retry_rate_limited_source_call(|| {
            operation_attempts.fetch_add(1, Ordering::SeqCst);
            async {
                Err::<(), _>(PluginServiceError::Runtime(PluginRuntimeError::Plugin {
                    code: "PLUGIN_RATE_LIMITED".to_owned(),
                    message: "retry".to_owned(),
                }))
            }
        })
        .await
        .expect_err("a permanently rate-limited source must stop");

        assert_eq!(
            result.attempts,
            u64::from(MAX_SOURCE_RATE_LIMIT_RETRIES) + 1
        );
        assert_eq!(
            result.rate_limited_responses,
            u64::from(MAX_SOURCE_RATE_LIMIT_RETRIES) + 1
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            usize::try_from(MAX_SOURCE_RATE_LIMIT_RETRIES + 1).expect("small retry budget")
        );
    }

    #[test]
    fn invalid_migration_item_report_has_stable_source_metadata() {
        let invalid = InvalidMigrationItem {
            user_id: "emby-user".to_owned(),
            start_index: 27365,
            range_limit: 1,
            kind: MigrationPageKind::UserState(MigrationUserStateFilter::Played),
        };

        assert_eq!(
            invalid_item_report_id(&invalid),
            "invalid:USER_STATE_PLAYED:emby-user:27365"
        );
        let detail: serde_json::Value =
            serde_json::from_str(&invalid_item_report_detail(&invalid)).expect("valid JSON");
        assert_eq!(detail["reason"], "PLUGIN_INVALID_RESPONSE");
        assert_eq!(detail["sourceStartIndex"], 27365);
        assert_eq!(detail["pageKind"], "USER_STATE_PLAYED");
    }

    #[test]
    fn recovery_budget_reports_unresolved_ranges_instead_of_single_items() {
        let invalid = InvalidMigrationItem {
            user_id: "emby-user".to_owned(),
            start_index: 64,
            range_limit: 32,
            kind: MigrationPageKind::UserState(MigrationUserStateFilter::Favorite),
        };
        let detail: serde_json::Value =
            serde_json::from_str(&invalid_item_report_detail(&invalid)).expect("valid JSON");
        assert_eq!(detail["reason"], "PLUGIN_INVALID_RESPONSE_RECOVERY_BUDGET");
        assert_eq!(detail["sourceRangeLimit"], 32);
    }

    #[test]
    fn duplicate_state_filter_results_are_claimed_once() {
        let item = MigrationItem {
            id: "emby-1".to_owned(),
            name: "The Film".to_owned(),
            item_type: "Movie".to_owned(),
            production_year: Some(2024),
            provider_ids: BTreeMap::new(),
            parent_id: None,
            series_id: None,
            season_id: None,
            index_number: None,
            parent_index_number: None,
            user_data: Some(MigrationUserData {
                playback_position_ticks: 0,
                played: true,
                is_favorite: true,
                play_count: 1,
                last_played_date: None,
            }),
        };
        let mut seen = HashSet::new();

        assert_eq!(
            collect_recorded_state_items(vec![item.clone()], &mut seen).len(),
            1
        );
        assert!(collect_recorded_state_items(vec![item], &mut seen).is_empty());
    }

    #[test]
    fn duplicate_state_items_are_removed_before_identity_lookup() {
        let item = MigrationItem {
            id: "emby-1".to_owned(),
            name: "The Film".to_owned(),
            item_type: "Movie".to_owned(),
            production_year: Some(2024),
            provider_ids: BTreeMap::new(),
            parent_id: None,
            series_id: None,
            season_id: None,
            index_number: None,
            parent_index_number: None,
            user_data: Some(MigrationUserData {
                playback_position_ticks: 0,
                played: true,
                is_favorite: false,
                play_count: 1,
                last_played_date: None,
            }),
        };
        let duplicate = item.clone();
        let mut seen = HashSet::new();

        let unique = collect_recorded_state_items(vec![item, duplicate], &mut seen);

        assert_eq!(unique.len(), 1);
        assert_eq!(unique[0].id, "emby-1");
    }

    #[test]
    fn handled_state_items_are_removed_before_identity_lookup() {
        let items = vec![
            migration_test_item("emby-1", "The Film"),
            migration_test_item("emby-2", "Another Film"),
        ];
        let handled = HashSet::from([String::from("emby-1")]);

        let unhandled = retain_unhandled_state_items(items, &handled);

        assert_eq!(
            unhandled
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["emby-2"]
        );
    }

    #[test]
    fn duplicate_media_identity_lookups_are_collapsed() {
        let items = vec![
            migration_test_item("emby-1", "The Film"),
            migration_test_item("emby-2", "The Film"),
        ];

        let lookups = migration_media_identity_lookups(&items);

        assert_eq!(lookups.len(), 1);
    }

    #[test]
    fn person_identity_index_is_needed_only_for_favorite_people() {
        let mut item = MigrationItem {
            id: "emby-person".to_owned(),
            name: "演员甲".to_owned(),
            item_type: "Person".to_owned(),
            production_year: None,
            provider_ids: BTreeMap::new(),
            parent_id: None,
            series_id: None,
            season_id: None,
            index_number: None,
            parent_index_number: None,
            user_data: None,
        };

        assert!(is_migratable_person_favorite(&item));

        item.user_data = Some(MigrationUserData {
            playback_position_ticks: 0,
            played: false,
            is_favorite: false,
            play_count: 0,
            last_played_date: None,
        });
        assert!(!is_migratable_person_favorite(&item));

        item.item_type = "Movie".to_owned();
        item.user_data = None;
        assert!(!is_migratable_person_favorite(&item));
    }

    #[test]
    fn enabled_source_folders_map_to_unique_lux_libraries() {
        let user = MigrationUser {
            id: "emby-user".to_owned(),
            name: "Alice".to_owned(),
            has_password: false,
            is_disabled: false,
            is_administrator: false,
            enable_all_folders: false,
            enabled_folders: vec!["emby-movies".to_owned()],
            enable_remote_access: false,
            enable_content_downloading: false,
            primary_image_tag: None,
        };
        let source_folders = vec![MigrationLibraryFolder {
            id: "emby-movies".to_owned(),
            name: "Movies".to_owned(),
            locations: vec!["/media/movies".to_owned()],
        }];
        let lux_libraries = vec![MigrationLuxLibraryIdentity {
            id: "lux-movies".to_owned(),
            name: "Movies".to_owned(),
            root_paths: vec!["/media/movies".to_owned()],
        }];

        assert_eq!(
            map_enabled_library_ids(&user, Some(&source_folders), &lux_libraries),
            HashSet::from(["lux-movies".to_owned()])
        );

        let lux_libraries = vec![
            MigrationLuxLibraryIdentity {
                id: "lux-other".to_owned(),
                name: "Movies".to_owned(),
                root_paths: vec!["/media/other".to_owned()],
            },
            MigrationLuxLibraryIdentity {
                id: "lux-movies".to_owned(),
                name: "Movies".to_owned(),
                root_paths: vec!["/media/movies".to_owned()],
            },
        ];
        assert_eq!(
            map_enabled_library_ids(&user, Some(&source_folders), &lux_libraries),
            HashSet::from(["lux-movies".to_owned()])
        );
    }

    #[test]
    fn ambiguous_source_folder_mapping_is_unavailable_for_permission_sync() {
        let mut user = migration_test_user("emby-user", "Alice");
        user.enable_all_folders = false;
        user.enabled_folders = vec!["emby-movies".to_owned()];
        let source_folders = vec![MigrationLibraryFolder {
            id: "emby-movies".to_owned(),
            name: "Movies".to_owned(),
            locations: vec!["/media/movies".to_owned()],
        }];
        let lux_libraries = vec![MigrationLuxLibraryIdentity {
            id: "lux-one".to_owned(),
            name: "Movies".to_owned(),
            root_paths: vec!["/media/one".to_owned()],
        }];
        assert_eq!(
            map_enabled_library_ids_checked(&user, Some(&source_folders), &lux_libraries),
            None
        );
    }

    #[test]
    fn migration_user_permissions_do_not_promote_emby_admins() {
        let user = MigrationUser {
            id: "emby-user".to_owned(),
            name: "Alice".to_owned(),
            has_password: false,
            is_disabled: true,
            is_administrator: true,
            enable_all_folders: false,
            enabled_folders: Vec::new(),
            enable_remote_access: true,
            enable_content_downloading: true,
            primary_image_tag: None,
        };

        let update = migration_user_update(&user);

        assert_eq!(update.is_disabled, Some(true));
        assert_eq!(update.can_remote_access, Some(true));
        assert_eq!(update.can_download, Some(true));
        assert_eq!(update.is_admin, None);
        assert_eq!(update.can_manage_server, None);
    }

    #[test]
    fn unchanged_user_profile_skips_the_profile_update() {
        let lux_user = UserRecord {
            id: Uuid::now_v7().into(),
            username_normalized: "alice".to_owned(),
            display_name: "Alice".to_owned(),
            is_disabled: false,
            is_admin: false,
            can_manage_server: false,
            can_remote_access: true,
            can_download: false,
        };
        let source_user = MigrationUser {
            id: "emby-user".to_owned(),
            name: "Alice".to_owned(),
            has_password: false,
            is_disabled: false,
            is_administrator: true,
            enable_all_folders: true,
            enabled_folders: Vec::new(),
            enable_remote_access: true,
            enable_content_downloading: false,
            primary_image_tag: None,
        };
        assert!(!migration_user_profile_changed(&lux_user, &source_user));

        let mut changed_source_user = source_user;
        changed_source_user.enable_content_downloading = true;
        assert!(migration_user_profile_changed(
            &lux_user,
            &changed_source_user
        ));
    }

    fn identity(id: &str, provider_ids: &str) -> StoredMigrationMediaIdentity {
        StoredMigrationMediaIdentity {
            id: id.to_owned(),
            library_id: "library-1".to_owned(),
            item_type: "MOVIE".to_owned(),
            title: "The Film".to_owned(),
            production_year: Some(2024),
            provider_ids_json: Some(provider_ids.to_owned()),
            series_id: None,
            season_number: None,
            episode_number: None,
        }
    }

    #[test]
    fn provider_id_match_is_unique_and_strongest() {
        let item = MigrationItem {
            id: "emby-1".to_owned(),
            name: "Different title".to_owned(),
            item_type: "Movie".to_owned(),
            production_year: None,
            provider_ids: BTreeMap::from([(String::from("Tmdb"), String::from("42"))]),
            parent_id: None,
            series_id: None,
            season_id: None,
            index_number: None,
            parent_index_number: None,
            user_data: None,
        };
        let index = MigrationMediaIdentityIndex::new(vec![identity("lux-1", r#"{"tmdb":"42"}"#)]);
        let outcome = match_item(&item, &index);
        assert_eq!(outcome.lux_item_id.as_deref(), Some("lux-1"));
        assert_eq!(outcome.method, "TMDB_ID");
        assert_eq!(outcome.status, "MATCHED");
    }

    #[test]
    fn ambiguous_title_match_is_not_guessed() {
        let item = MigrationItem {
            id: "emby-1".to_owned(),
            name: "The Film".to_owned(),
            item_type: "Movie".to_owned(),
            production_year: Some(2024),
            provider_ids: BTreeMap::new(),
            parent_id: None,
            series_id: None,
            season_id: None,
            index_number: None,
            parent_index_number: None,
            user_data: None,
        };
        let index = MigrationMediaIdentityIndex::new(vec![
            identity("lux-1", "{}"),
            identity("lux-2", "{}"),
        ]);
        let outcome = match_item(&item, &index);
        assert_eq!(outcome.lux_item_id, None);
        assert_eq!(outcome.status, "CONFLICT");
    }

    #[test]
    fn selected_library_conflicts_do_not_trigger_unfiltered_fallback() {
        let item = MigrationItem {
            id: "emby-1".to_owned(),
            name: "The Film".to_owned(),
            item_type: "Movie".to_owned(),
            production_year: Some(2024),
            provider_ids: BTreeMap::from([(String::from("Tmdb"), String::from("42"))]),
            parent_id: None,
            series_id: None,
            season_id: None,
            index_number: None,
            parent_index_number: None,
            user_data: None,
        };
        let index = MigrationMediaIdentityIndex::new(vec![
            identity("lux-1", r#"{"tmdb":"42"}"#),
            identity("lux-2", r#"{"tmdb":"42"}"#),
        ]);

        assert_eq!(match_item(&item, &index).status, "CONFLICT");
        assert!(!needs_unfiltered_library_fallback(&item, &index));
    }

    #[test]
    fn migration_match_detail_includes_lux_series_context() {
        let item = MigrationItem {
            id: "emby-episode-1".to_owned(),
            name: "第十集".to_owned(),
            item_type: "Episode".to_owned(),
            production_year: Some(1986),
            provider_ids: BTreeMap::new(),
            parent_id: None,
            series_id: Some("emby-series-1".to_owned()),
            season_id: Some("emby-season-2".to_owned()),
            index_number: Some(10),
            parent_index_number: Some(2),
            user_data: None,
        };
        let identities = vec![
            StoredMigrationMediaIdentity {
                id: "lux-series-1".to_owned(),
                library_id: "library-1".to_owned(),
                item_type: "SERIES".to_owned(),
                title: "西游记".to_owned(),
                production_year: Some(1986),
                provider_ids_json: None,
                series_id: None,
                season_number: None,
                episode_number: None,
            },
            StoredMigrationMediaIdentity {
                id: "lux-episode-1".to_owned(),
                library_id: "library-1".to_owned(),
                item_type: "EPISODE".to_owned(),
                title: "第十集".to_owned(),
                production_year: Some(1986),
                provider_ids_json: None,
                series_id: Some("lux-series-1".to_owned()),
                season_number: Some(2),
                episode_number: Some(10),
            },
        ];
        let outcome = MatchOutcome {
            lux_item_id: Some("lux-episode-1".to_owned()),
            method: "EPISODE_KEY",
            confidence: Some(95),
            status: "MATCHED",
        };

        let detail = migration_item_detail(
            &item,
            &outcome,
            &MigrationMediaIdentityIndex::new(identities),
        );

        assert_eq!(detail["luxTitle"], "第十集");
        assert_eq!(detail["luxSeriesTitle"], "西游记");
        assert_eq!(detail["luxSeasonNumber"], 2);
        assert_eq!(detail["luxEpisodeNumber"], 10);
    }

    #[test]
    fn person_identity_index_groups_provider_and_name_matches_once() {
        let index = MigrationPersonIdentityIndex::new(vec![
            StoredMigrationPersonIdentity {
                id: "person-1".to_owned(),
                display_name: "演员 甲".to_owned(),
                provider: Some("Tmdb".to_owned()),
                provider_id: Some("42".to_owned()),
            },
            StoredMigrationPersonIdentity {
                id: "person-1".to_owned(),
                display_name: "演员 甲".to_owned(),
                provider: Some("Imdb".to_owned()),
                provider_id: Some("nm42".to_owned()),
            },
        ]);

        assert_eq!(
            index
                .by_provider
                .get(&(String::from("tmdb"), String::from("42"))),
            Some(&vec![String::from("person-1")])
        );
        assert_eq!(
            index.by_name.get("演员甲"),
            Some(&vec![String::from("person-1")])
        );
    }

    #[test]
    fn person_identity_lookups_are_deduplicated_and_normalized() {
        let mut first = MigrationItem {
            id: "person-1".to_owned(),
            name: "Actor A".to_owned(),
            item_type: "Person".to_owned(),
            production_year: None,
            provider_ids: BTreeMap::from([("TMDB".to_owned(), "42".to_owned())]),
            parent_id: None,
            series_id: None,
            season_id: None,
            index_number: None,
            parent_index_number: None,
            user_data: None,
        };
        let second = first.clone();
        first.name = " actor a ".to_owned();

        let lookups = migration_person_identity_lookups(&[first, second]);
        assert_eq!(lookups.len(), 1);
        assert_eq!(lookups[0].normalized_name, "actora");
        assert_eq!(
            lookups[0].provider_ids,
            vec![("tmdb".to_owned(), "42".to_owned())]
        );
    }

    #[test]
    fn recovered_migration_page_keeps_items_and_advances_past_invalid_entries() {
        let page = assemble_recovered_migration_page(
            100,
            4,
            vec![
                (
                    100,
                    migration_page_with_items(&["item-100", "item-101"], 100, 104),
                ),
                (103, migration_page_with_items(&["item-103"], 103, 104)),
            ],
            vec![InvalidMigrationItem {
                user_id: "emby-user".to_owned(),
                start_index: 102,
                range_limit: 1,
                kind: MigrationPageKind::UserState(MigrationUserStateFilter::Played),
            }],
        );

        assert_eq!(
            page.page
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["item-100", "item-101", "item-103"]
        );
        assert_eq!(page.page.start_index, 100);
        assert_eq!(page.page.next_start_index, None);
        assert_eq!(page.invalid_items.len(), 1);
    }

    #[test]
    fn recovered_migration_page_advances_when_all_requested_items_are_invalid() {
        let page = assemble_recovered_migration_page(
            100,
            4,
            vec![
                (100, empty_migration_page(100)),
                (101, empty_migration_page(101)),
                (102, empty_migration_page(102)),
                (103, empty_migration_page(103)),
            ],
            vec![
                InvalidMigrationItem {
                    user_id: "emby-user".to_owned(),
                    start_index: 100,
                    range_limit: 1,
                    kind: MigrationPageKind::UserState(MigrationUserStateFilter::Played),
                },
                InvalidMigrationItem {
                    user_id: "emby-user".to_owned(),
                    start_index: 101,
                    range_limit: 1,
                    kind: MigrationPageKind::UserState(MigrationUserStateFilter::Played),
                },
                InvalidMigrationItem {
                    user_id: "emby-user".to_owned(),
                    start_index: 102,
                    range_limit: 1,
                    kind: MigrationPageKind::UserState(MigrationUserStateFilter::Played),
                },
                InvalidMigrationItem {
                    user_id: "emby-user".to_owned(),
                    start_index: 103,
                    range_limit: 1,
                    kind: MigrationPageKind::UserState(MigrationUserStateFilter::Played),
                },
            ],
        );

        assert_eq!(page.page.total_record_count, None);
        assert_eq!(page.page.next_start_index, Some(104));
    }

    #[test]
    fn invalid_person_page_becomes_a_skipped_person_favorite_report() {
        let report = invalid_person_favorite_report(&InvalidMigrationItem {
            user_id: "emby-user".to_owned(),
            start_index: 42,
            range_limit: 1,
            kind: MigrationPageKind::PersonFavorites,
        });

        assert_eq!(
            report.emby_person_id,
            "invalid:PERSON_FAVORITES:emby-user:42"
        );
        assert_eq!(report.status, "SKIPPED");
        assert_eq!(report.error.as_deref(), Some("PLUGIN_INVALID_RESPONSE"));
        assert!(report.lux_user_id.is_none());
        assert!(report.lux_person_id.is_none());
    }

    #[test]
    fn invalid_migration_page_ranges_split_until_single_item() {
        assert_eq!(
            split_migration_page_range(27_000, 500),
            Some(((27_000, 250), (27_250, 250)))
        );
        assert_eq!(split_migration_page_range(27_360, 1), None);
    }

    fn migration_page_with_items(
        ids: &[&str],
        start_index: u32,
        total_record_count: u32,
    ) -> MigrationItemPage {
        MigrationItemPage {
            items: ids
                .iter()
                .map(|id| MigrationItem {
                    id: (*id).to_owned(),
                    name: (*id).to_owned(),
                    item_type: "Movie".to_owned(),
                    production_year: None,
                    provider_ids: BTreeMap::new(),
                    parent_id: None,
                    series_id: None,
                    season_id: None,
                    index_number: None,
                    parent_index_number: None,
                    user_data: None,
                })
                .collect(),
            start_index,
            total_record_count: Some(total_record_count),
            next_start_index: Some(start_index + ids.len() as u32),
            history_capability: HistoryCapability::ItemState,
        }
    }

    #[test]
    fn invalid_last_played_date_is_non_fatal() {
        let data = MigrationUserData {
            playback_position_ticks: 10,
            played: true,
            is_favorite: false,
            play_count: 1,
            last_played_date: Some("not-a-date".to_owned()),
        };
        let state = incoming_state(&data).expect("valid numeric state should be accepted");
        assert_eq!(state.last_played_at, None);
        assert_eq!(state.position_ticks, 10);
    }
}
