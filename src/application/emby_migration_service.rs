use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::PathBuf,
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{fs, io::AsyncWriteExt};
use uuid::Uuid;

use crate::{
    application::emby_migration::{
        EmbyMigrationPluginClient, EmbyMigrationSource, HistoryCapability, MigrationConnectionInfo,
        MigrationInputError, MigrationItem, MigrationItemPage, MigrationLibraryFolder,
        MigrationMergePolicy, MigrationUser, MigrationUserData, MigrationUserStateFilter,
        StoredItemState, merge_item_state,
    },
    application::plugin_runtime::PluginRuntimeError,
    application::plugins::PluginServiceError,
    auth::users::{UserStore, UserStoreError, UserUpdate},
    storage::{
        Database, EmbyMigrationJobProgress, NewEmbyMigrationImportRecord,
        NewEmbyMigrationItemMatch, NewEmbyMigrationJob, NewEmbyMigrationPersonFavorite,
        NewImportedUserItemState, StorageError, StoredEmbyMigrationImportRecord,
        StoredEmbyMigrationItemMatch, StoredEmbyMigrationJob, StoredEmbyMigrationPersonFavorite,
        StoredEmbyMigrationSource, StoredEmbyMigrationUserBinding, StoredEmbyMigrationUserLink,
        StoredMigrationMediaIdentity, StoredPlaybackHistoryEvent,
    },
};

const SECRET_DIRECTORY: &str = "plugin-secrets/emby-migration";
const MAX_LABEL_LENGTH: usize = 128;
const MAX_JOB_PAGE_SIZE: i64 = 100;
const MAX_SELECTED_USER_COUNT: usize = 1_000;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateMigrationRequest {
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub merge_policy: MigrationMergePolicy,
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

#[derive(Clone)]
pub struct EmbyMigrationService {
    database: Database,
    plugin: EmbyMigrationPluginClient,
    config_dir: PathBuf,
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
        }
    }

    pub async fn create_job(
        &self,
        created_by_user_id: &str,
        request: CreateMigrationRequest,
    ) -> Result<MigrationJobView, EmbyMigrationServiceError> {
        let emby_user_ids = normalize_selected_user_ids(&request.emby_user_ids)?;
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
                emby_user_ids_json: &emby_user_ids_json,
            })
            .await
        {
            let _ = self.remove_secret(&secret_ref).await;
            return Err(error.into());
        }
        self.get_job(&job_id).await
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
            if let Err(error) = self.run(&job_id).await {
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
        let Some(job) = self.database.find_emby_migration_job(job_id).await? else {
            return Err(EmbyMigrationServiceError::NotFound);
        };
        if matches!(job.status.as_str(), "COMPLETED" | "CANCELLED") {
            return Ok(());
        }
        let source = self.read_source(&job).await?;
        self.database
            .update_emby_migration_job_status(job_id, "RUNNING", "TESTING", None)
            .await?;
        let connection = match self.plugin.test_connection(&source).await {
            Ok(connection) => connection,
            Err(error) => {
                self.fail_job(job_id, "TESTING", &error.to_string()).await?;
                return Err(error.into());
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
        if self.is_cancelled(job_id).await? {
            return self.cancelled(job_id, "TESTING").await;
        }

        self.database
            .update_emby_migration_job_status(job_id, "RUNNING", "USERS", None)
            .await?;
        let user_page = match self.plugin.list_users(&source).await {
            Ok(page) => page,
            Err(error) => {
                self.fail_job(job_id, "USERS", &error.to_string()).await?;
                return Err(error.into());
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
        let mut user_links = Vec::with_capacity(users.len());
        for user in &users {
            if self.is_cancelled(job_id).await? {
                return self.cancelled(job_id, "USERS").await;
            }
            let link = self.prepare_user(&user_store, &job, user).await?;
            self.database.upsert_emby_migration_user_link(&link).await?;
            user_links.push((user.clone(), link.lux_user_id.clone()));
        }

        self.database
            .update_emby_migration_job_status(job_id, "RUNNING", "ITEMS", None)
            .await?;
        let identity_index = MigrationMediaIdentityIndex::new(self.load_media_identities().await?);
        let lux_library_identities = self.load_library_identities().await?;
        let mut processed = job.processed_count;
        let mut matched = job.matched_count;
        let mut skipped = job.skipped_count;
        let mut failed = job.failed_count;
        let mut total = job.total_count.max(users.len() as i64);
        for (user, lux_user_id) in user_links {
            let mut accessible_library_ids = HashSet::new();
            let mut seen_emby_item_ids = HashSet::new();
            for state_filter in MigrationUserStateFilter::ALL {
                let filter_base = processed;
                let mut filter_total_recorded = false;
                let mut start_index = 0_u32;
                loop {
                    if self.is_cancelled(job_id).await? {
                        return self.cancelled(job_id, "ITEMS").await;
                    }
                    let recovered_page = self
                        .recover_migration_page(
                            &source,
                            &user.id,
                            start_index,
                            500,
                            MigrationPageKind::UserState(state_filter),
                        )
                        .await?;
                    if !recovered_page.invalid_items.is_empty() {
                        self.record_invalid_migration_items(job_id, &recovered_page.invalid_items)
                            .await?;
                        let invalid_item_count = recovered_page.invalid_items.len() as i64;
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
                    if !filter_total_recorded {
                        if let Some(page_total) = page.total_record_count {
                            total = total.max(filter_base + page_total as i64);
                            filter_total_recorded = true;
                        }
                    }
                    for item in page.items {
                        let Some(user_data) =
                            recorded_state_for_migration(&item, &mut seen_emby_item_ids)
                        else {
                            continue;
                        };
                        processed += 1;
                        let outcome = match_item(&item, &identity_index);
                        let detail = serde_json::to_string(&migration_item_detail(
                            &item,
                            &outcome,
                            &identity_index.identities,
                        ))
                        .unwrap_or_else(|_| "{}".to_owned());
                        self.database
                            .upsert_emby_migration_item_match(&NewEmbyMigrationItemMatch {
                                job_id,
                                emby_item_id: &item.id,
                                emby_item_type: &item.item_type,
                                lux_item_id: outcome.lux_item_id.as_deref(),
                                match_method: outcome.method,
                                confidence: outcome.confidence,
                                status: outcome.status,
                                detail_json: &detail,
                            })
                            .await?;
                        let Some(lux_item_id) = outcome.lux_item_id else {
                            skipped += 1;
                            continue;
                        };
                        matched += 1;
                        if let Some(library_id) =
                            self.database.find_item_library_id(&lux_item_id).await?
                        {
                            accessible_library_ids.insert(library_id);
                        }
                        if job.dry_run {
                            continue;
                        }
                        let Some(lux_user_id) = lux_user_id.as_deref() else {
                            skipped += 1;
                            continue;
                        };
                        let incoming = incoming_state(&user_data)?;
                        let existing = self
                            .database
                            .find_user_item_state_for_migration(lux_user_id, &lux_item_id)
                            .await?
                            .map(|state| StoredItemState {
                                position_ticks: state.position_ticks,
                                is_played: state.is_played,
                                is_favorite: state.is_favorite,
                                play_count: state.play_count,
                                last_played_at: state.last_played_at,
                            });
                        let merged = merge_item_state(
                            existing,
                            incoming,
                            migration_merge_policy(&job.merge_policy),
                        )
                        .ok_or(EmbyMigrationServiceError::InvalidState)?;
                        self.database
                            .upsert_imported_user_item_state(&NewImportedUserItemState {
                                user_id: lux_user_id,
                                item_id: &lux_item_id,
                                position_ticks: merged.position_ticks,
                                is_played: merged.is_played,
                                is_favorite: merged.is_favorite,
                                play_count: merged.play_count,
                                last_played_at: merged.last_played_at,
                            })
                            .await?;
                        let state_hash = hex_sha256(&user_data)?;
                        self.database
                            .upsert_emby_migration_import_record(&NewEmbyMigrationImportRecord {
                                job_id,
                                emby_user_id: &user.id,
                                emby_item_id: &item.id,
                                lux_user_id,
                                lux_item_id: &lux_item_id,
                                state_hash: &state_hash,
                                status: "IMPORTED",
                                error: None,
                            })
                            .await?;
                    }
                    self.database
                        .update_emby_migration_job_progress(&EmbyMigrationJobProgress {
                            id: job_id,
                            cursor_json: &serde_json::to_string(&json!({
                                "userId": user.id,
                                "stateFilter": state_filter,
                                "startIndex": page.start_index,
                            }))
                            .unwrap_or_else(|_| "{}".to_owned()),
                            processed_count: processed,
                            total_count: total,
                            matched_count: matched,
                            skipped_count: skipped,
                            failed_count: failed,
                        })
                        .await?;
                    let Some(next_start_index) = page.next_start_index else {
                        break;
                    };
                    if next_start_index <= start_index {
                        break;
                    }
                    start_index = next_start_index;
                }
            }
            if !job.dry_run {
                let (library_ids, exact_library_access) = if user.enable_all_folders {
                    (self.database.list_enabled_library_ids().await?, true)
                } else if let Some(source_folders) = library_folders.as_deref() {
                    let allowed_library_ids = map_enabled_library_ids(
                        &user,
                        Some(source_folders),
                        &lux_library_identities,
                    );
                    (allowed_library_ids.into_iter().collect(), true)
                } else {
                    (accessible_library_ids.into_iter().collect(), false)
                };
                let allowed_library_ids = library_ids.iter().cloned().collect::<HashSet<_>>();
                let enabled_library_ids = if exact_library_access {
                    self.database.list_enabled_library_ids().await?
                } else {
                    library_ids
                };
                for library_id in enabled_library_ids {
                    if let Some(lux_user_id) = lux_user_id.as_deref() {
                        self.database
                            .set_user_library_access(
                                lux_user_id,
                                &library_id,
                                !exact_library_access || allowed_library_ids.contains(&library_id),
                            )
                            .await?;
                    }
                }
            }

            let person_filter_base = processed;
            let mut person_total_recorded = false;
            let mut start_index = 0_u32;
            loop {
                if self.is_cancelled(job_id).await? {
                    return self.cancelled(job_id, "ITEMS").await;
                }
                let recovered_page = self
                    .recover_migration_page(
                        &source,
                        &user.id,
                        start_index,
                        500,
                        MigrationPageKind::PersonFavorites,
                    )
                    .await?;
                if !recovered_page.invalid_items.is_empty() {
                    self.record_invalid_migration_items(job_id, &recovered_page.invalid_items)
                        .await?;
                    let invalid_item_count = recovered_page.invalid_items.len() as i64;
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
                if !person_total_recorded {
                    if let Some(page_total) = page.total_record_count {
                        total = total.max(person_filter_base + page_total as i64);
                        person_total_recorded = true;
                    }
                }
                for person in page.items {
                    if person.item_type != "Person" {
                        continue;
                    }
                    let user_data = person.user_data.clone().unwrap_or(MigrationUserData {
                        playback_position_ticks: 0,
                        played: false,
                        is_favorite: true,
                        play_count: 0,
                        last_played_date: None,
                    });
                    if !user_data.is_favorite {
                        continue;
                    }
                    processed += 1;
                    let outcome = self.match_person(&person).await?;
                    let provider_ids_json = serde_json::to_string(&person.provider_ids)
                        .unwrap_or_else(|_| "{}".to_owned());
                    let detail_json = serde_json::to_string(&json!({
                        "sourceName": person.name,
                        "sourceType": "Person",
                        "providerIds": person.provider_ids,
                        "matchMethod": outcome.method,
                    }))
                    .unwrap_or_else(|_| "{}".to_owned());
                    let state_hash = hex_sha256(&user_data)?;
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
                                    self.database
                                        .set_user_person_favorite(
                                            lux_user_id,
                                            outcome
                                                .lux_person_id
                                                .as_deref()
                                                .ok_or(EmbyMigrationServiceError::InvalidState)?,
                                            true,
                                        )
                                        .await?;
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
                    self.database
                        .upsert_emby_migration_person_favorite(&NewEmbyMigrationPersonFavorite {
                            job_id,
                            emby_user_id: &user.id,
                            emby_person_id: &person.id,
                            emby_person_name: &person.name,
                            lux_user_id: lux_user_id.as_deref(),
                            lux_person_id: outcome.lux_person_id.as_deref(),
                            provider_ids_json: &provider_ids_json,
                            match_method: outcome.method,
                            confidence: outcome.confidence,
                            status,
                            state_hash: &state_hash,
                            detail_json: &detail_json,
                            error: error.as_deref(),
                        })
                        .await?;
                }
                self.database
                    .update_emby_migration_job_progress(&EmbyMigrationJobProgress {
                        id: job_id,
                        cursor_json: &serde_json::to_string(&json!({
                            "kind": "PERSON_FAVORITES",
                            "userId": user.id,
                            "startIndex": page.start_index,
                        }))
                        .unwrap_or_else(|_| "{}".to_owned()),
                        processed_count: processed,
                        total_count: total,
                        matched_count: matched,
                        skipped_count: skipped,
                        failed_count: failed,
                    })
                    .await?;
                let Some(next_start_index) = page.next_start_index else {
                    break;
                };
                if next_start_index <= start_index {
                    break;
                }
                start_index = next_start_index;
            }
        }
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
        job: &StoredEmbyMigrationJob,
        source_user: &MigrationUser,
    ) -> Result<StoredEmbyMigrationUserLink, EmbyMigrationServiceError> {
        let source_user_name = source_user.name.trim();
        if source_user_name.is_empty() {
            return Ok(StoredEmbyMigrationUserLink {
                job_id: job.id.clone(),
                emby_user_id: source_user.id.clone(),
                emby_username: source_user.name.clone(),
                lux_user_id: None,
                status: "SKIPPED".to_owned(),
                error: Some("empty Emby username".to_owned()),
            });
        }
        if job.dry_run {
            return Ok(StoredEmbyMigrationUserLink {
                job_id: job.id.clone(),
                emby_user_id: source_user.id.clone(),
                emby_username: source_user_name.to_owned(),
                lux_user_id: None,
                status: "SKIPPED".to_owned(),
                error: Some("DRY_RUN".to_owned()),
            });
        }
        let existing = user_store.find_by_username(source_user_name).await?;
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
        let lux_user = user_store
            .update_user(&lux_user.id.to_string(), migration_user_update(source_user))
            .await?
            .ok_or(EmbyMigrationServiceError::NotFound)?;
        self.database
            .upsert_emby_migration_user_binding(&StoredEmbyMigrationUserBinding {
                lux_user_id: lux_user.id.to_string(),
                source_base_url: job.source_base_url.clone(),
                secret_ref: Some(job.secret_ref.clone()),
                emby_user_id: source_user.id.clone(),
                emby_username: source_user_name.to_owned(),
                password_pending: source_user.has_password && !source_user.is_disabled,
            })
            .await?;
        Ok(StoredEmbyMigrationUserLink {
            job_id: job.id.clone(),
            emby_user_id: source_user.id.clone(),
            emby_username: source_user_name.to_owned(),
            lux_user_id: Some(lux_user.id.to_string()),
            status: status.to_owned(),
            error: None,
        })
    }

    async fn load_media_identities(
        &self,
    ) -> Result<Vec<StoredMigrationMediaIdentity>, EmbyMigrationServiceError> {
        let mut identities = Vec::new();
        let mut after_id = None;
        loop {
            let page = self
                .database
                .list_migration_media_identities(after_id.as_deref(), 500)
                .await?;
            if page.is_empty() {
                break;
            }
            after_id = page.last().map(|item| item.id.clone());
            let page_len = page.len();
            identities.extend(page);
            if page_len < 500 {
                break;
            }
        }
        Ok(identities)
    }

    async fn load_library_identities(
        &self,
    ) -> Result<Vec<MigrationLuxLibraryIdentity>, EmbyMigrationServiceError> {
        let libraries = self.database.list_libraries().await?;
        let library_ids = libraries
            .iter()
            .map(|library| library.id.clone())
            .collect::<Vec<_>>();
        let mut roots_by_library = self
            .database
            .list_library_roots_by_library_ids(&library_ids)
            .await?;
        Ok(libraries
            .into_iter()
            .filter(|library| library.is_enabled)
            .map(|library| MigrationLuxLibraryIdentity {
                id: library.id.clone(),
                name: library.name,
                root_paths: roots_by_library
                    .remove(&library.id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|root| root.canonical_path)
                    .collect(),
            })
            .collect())
    }

    async fn recover_migration_page(
        &self,
        source: &EmbyMigrationSource,
        user_id: &str,
        start_index: u32,
        limit: u32,
        kind: MigrationPageKind,
    ) -> Result<RecoveredMigrationPage, EmbyMigrationServiceError> {
        let mut pending_ranges = vec![(start_index, limit)];
        let mut pages = Vec::new();
        let mut invalid_items = Vec::new();

        while let Some((range_start, range_limit)) = pending_ranges.pop() {
            let result = match kind {
                MigrationPageKind::UserState(state_filter) => {
                    self.plugin
                        .user_state(source, user_id, range_start, range_limit, state_filter)
                        .await
                }
                MigrationPageKind::PersonFavorites => {
                    self.plugin
                        .person_favorites(source, user_id, range_start, range_limit)
                        .await
                }
            };
            match result {
                Ok(page) => pages.push((range_start, page)),
                Err(error) if is_invalid_migration_response(&error) && range_limit > 1 => {
                    if let Some(((left_start, left_limit), (right_start, right_limit))) =
                        split_migration_page_range(range_start, range_limit)
                    {
                        pending_ranges.push((right_start, right_limit));
                        pending_ranges.push((left_start, left_limit));
                    }
                }
                Err(error) if is_invalid_migration_response(&error) => {
                    invalid_items.push(InvalidMigrationItem {
                        user_id: user_id.to_owned(),
                        start_index: range_start,
                        kind,
                    });
                    pages.push((range_start, empty_migration_page(range_start)));
                }
                Err(error) => return Err(error.into()),
            }
        }

        Ok(assemble_recovered_migration_page(
            start_index,
            limit,
            pages,
            invalid_items,
        ))
    }

    async fn record_invalid_migration_items(
        &self,
        job_id: &str,
        invalid_items: &[InvalidMigrationItem],
    ) -> Result<(), EmbyMigrationServiceError> {
        for invalid in invalid_items {
            let report_id = invalid_item_report_id(invalid);
            let detail_json = invalid_item_report_detail(invalid);
            self.database
                .upsert_emby_migration_item_match(&NewEmbyMigrationItemMatch {
                    job_id,
                    emby_item_id: &report_id,
                    emby_item_type: "UNKNOWN",
                    lux_item_id: None,
                    match_method: "UNMATCHED",
                    confidence: None,
                    status: "SKIPPED",
                    detail_json: &detail_json,
                })
                .await?;
        }
        Ok(())
    }

    async fn match_person(
        &self,
        person: &MigrationItem,
    ) -> Result<PersonMatchOutcome, EmbyMigrationServiceError> {
        let mut provider_matches = Vec::<(String, bool)>::new();
        for (source_key, source_value) in &person.provider_ids {
            let provider = normalize_person_provider(source_key);
            if let Some(target) = self
                .database
                .find_canonical_person_by_identity(&provider, source_value)
                .await?
            {
                provider_matches.push((target.id, provider.eq_ignore_ascii_case("tmdb")));
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
            let (lux_person_id, is_tmdb) = provider_matches
                .pop()
                .ok_or(EmbyMigrationServiceError::InvalidState)?;
            return Ok(PersonMatchOutcome {
                lux_person_id: Some(lux_person_id),
                method: if is_tmdb { "TMDB_ID" } else { "PROVIDER_ID" },
                confidence: Some(100),
                status: "MATCHED",
            });
        }
        if provider_matches.len() > 1 {
            return Ok(PersonMatchOutcome {
                lux_person_id: None,
                method: "CONFLICT",
                confidence: None,
                status: "CONFLICT",
            });
        }

        let normalized_name = normalize_person_name(&person.name);
        if normalized_name.is_empty() {
            return Ok(PersonMatchOutcome::unmatched());
        }
        let matches = self
            .database
            .list_canonical_people_by_normalized_name(&normalized_name)
            .await?;
        if matches.len() == 1 {
            return Ok(PersonMatchOutcome {
                lux_person_id: Some(matches[0].id.clone()),
                method: "NAME",
                confidence: Some(90),
                status: "MATCHED",
            });
        }
        if matches.len() > 1 {
            return Ok(PersonMatchOutcome {
                lux_person_id: None,
                method: "CONFLICT",
                confidence: None,
                status: "CONFLICT",
            });
        }
        Ok(PersonMatchOutcome::unmatched())
    }

    async fn is_cancelled(&self, job_id: &str) -> Result<bool, EmbyMigrationServiceError> {
        Ok(self
            .database
            .find_emby_migration_job(job_id)
            .await?
            .is_some_and(|job| job.cancel_requested))
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MigrationPageKind {
    UserState(MigrationUserStateFilter),
    PersonFavorites,
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
    kind: MigrationPageKind,
}

struct RecoveredMigrationPage {
    page: MigrationItemPage,
    invalid_items: Vec<InvalidMigrationItem>,
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
        "reason": "PLUGIN_INVALID_RESPONSE",
        "pageKind": invalid.kind.as_str(),
        "sourceUserId": invalid.user_id,
        "sourceStartIndex": invalid.start_index,
    }))
    .unwrap_or_else(|_| "{}".to_owned())
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

fn recorded_state_for_migration(
    item: &MigrationItem,
    seen_emby_item_ids: &mut HashSet<String>,
) -> Option<MigrationUserData> {
    let user_data = item.user_data.clone()?;
    if !user_data.has_recorded_state() || !seen_emby_item_ids.insert(item.id.clone()) {
        return None;
    }
    Some(user_data)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MigrationLuxLibraryIdentity {
    id: String,
    name: String,
    root_paths: Vec<String>,
}

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

fn match_lux_library(
    source_folder: &MigrationLibraryFolder,
    lux_libraries: &[MigrationLuxLibraryIdentity],
) -> Option<String> {
    let normalized_name = normalize_title(&source_folder.name);
    let name_matches = lux_libraries
        .iter()
        .filter(|library| normalize_title(&library.name) == normalized_name)
        .collect::<Vec<_>>();
    if name_matches.len() == 1 {
        return name_matches.first().map(|library| library.id.clone());
    }

    let source_paths = source_folder
        .locations
        .iter()
        .map(|path| normalize_library_path(path))
        .filter(|path| !path.is_empty())
        .collect::<HashSet<_>>();
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

struct MatchOutcome {
    lux_item_id: Option<String>,
    method: &'static str,
    confidence: Option<i64>,
    status: &'static str,
}

struct MigrationMediaIdentityIndex {
    identities: Vec<StoredMigrationMediaIdentity>,
    by_provider: HashMap<(String, String, String), Vec<usize>>,
    by_title: HashMap<(String, String), Vec<usize>>,
}

impl MigrationMediaIdentityIndex {
    fn new(identities: Vec<StoredMigrationMediaIdentity>) -> Self {
        let mut by_provider = HashMap::new();
        let mut by_title = HashMap::new();
        for (index, identity) in identities.iter().enumerate() {
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
            by_provider,
            by_title,
        }
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
    identities: &[StoredMigrationMediaIdentity],
) -> serde_json::Value {
    let lux_identity = outcome
        .lux_item_id
        .as_deref()
        .and_then(|id| identities.iter().find(|identity| identity.id == id));
    let lux_series_identity = lux_identity
        .and_then(|identity| identity.series_id.as_deref())
        .and_then(|series_id| identities.iter().find(|identity| identity.id == series_id));

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

fn migration_merge_policy(value: &str) -> MigrationMergePolicy {
    match value {
        "OVERWRITE" => MigrationMergePolicy::Overwrite,
        "SKIP" => MigrationMergePolicy::Skip,
        _ => MigrationMergePolicy::Merge,
    }
}

fn normalize_selected_user_ids(values: &[String]) -> Result<Vec<String>, MigrationInputError> {
    let mut selected = Vec::with_capacity(values.len().min(MAX_SELECTED_USER_COUNT));
    let mut seen = HashSet::with_capacity(values.len());
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            continue;
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

fn select_migration_users(
    users: Vec<MigrationUser>,
    selected_user_ids_json: &str,
) -> Result<Vec<MigrationUser>, EmbyMigrationServiceError> {
    let selected_user_ids = serde_json::from_str::<Vec<String>>(selected_user_ids_json)
        .map_err(|_| EmbyMigrationServiceError::InvalidState)
        .and_then(|ids| normalize_selected_user_ids(&ids).map_err(Into::into))?;
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

fn hex_sha256(value: &MigrationUserData) -> Result<String, EmbyMigrationServiceError> {
    let bytes = serde_json::to_vec(value).map_err(|_| EmbyMigrationServiceError::InvalidState)?;
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
    use crate::application::plugin_runtime::PluginRuntimeError;

    use super::*;
    use std::collections::BTreeMap;

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
            Err(MigrationInputError::NoSelectedUsers)
        ));
        assert!(matches!(
            normalize_selected_user_ids(&["user/1".to_owned()]),
            Err(MigrationInputError::InvalidIdentifier)
        ));
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

    #[test]
    fn invalid_migration_item_report_has_stable_source_metadata() {
        let invalid = InvalidMigrationItem {
            user_id: "emby-user".to_owned(),
            start_index: 27365,
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

        assert!(recorded_state_for_migration(&item, &mut seen).is_some());
        assert!(recorded_state_for_migration(&item, &mut seen).is_none());
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

    fn identity(id: &str, provider_ids: &str) -> StoredMigrationMediaIdentity {
        StoredMigrationMediaIdentity {
            id: id.to_owned(),
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

        let detail = migration_item_detail(&item, &outcome, &identities);

        assert_eq!(detail["luxTitle"], "第十集");
        assert_eq!(detail["luxSeriesTitle"], "西游记");
        assert_eq!(detail["luxSeasonNumber"], 2);
        assert_eq!(detail["luxEpisodeNumber"], 10);
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
                    kind: MigrationPageKind::UserState(MigrationUserStateFilter::Played),
                },
                InvalidMigrationItem {
                    user_id: "emby-user".to_owned(),
                    start_index: 101,
                    kind: MigrationPageKind::UserState(MigrationUserStateFilter::Played),
                },
                InvalidMigrationItem {
                    user_id: "emby-user".to_owned(),
                    start_index: 102,
                    kind: MigrationPageKind::UserState(MigrationUserStateFilter::Played),
                },
                InvalidMigrationItem {
                    user_id: "emby-user".to_owned(),
                    start_index: 103,
                    kind: MigrationPageKind::UserState(MigrationUserStateFilter::Played),
                },
            ],
        );

        assert_eq!(page.page.total_record_count, None);
        assert_eq!(page.page.next_start_index, Some(104));
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
