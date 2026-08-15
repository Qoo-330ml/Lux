use std::fmt;

use tokio::task::JoinSet;
use uuid::Uuid;

use crate::{
    application::{
        admin_events::{AdminEventHub, AdminEventScope},
        candidates::{
            MetadataCandidateError, MetadataCandidateService, MetadataSelectionError,
            MetadataSelectionMode, MetadataSelectionService,
        },
        scraper::{ScraperError, ScraperResolver},
        tmdb_plugin::TmdbProvider,
    },
    observability::resources::ResourceMetrics,
    storage::{Database, StorageError, StoredMetadataReidentifyItem},
};

pub const METADATA_MATCH_CONCURRENCY: usize = 16;
const METADATA_JOB_ITEM_PAGE_SIZE: i64 = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataRefreshMode {
    Reidentify,
    FillMissing,
    FullRefresh,
}

impl MetadataRefreshMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reidentify => "REIDENTIFY",
            Self::FillMissing => "FILL_MISSING",
            Self::FullRefresh => "FULL_REFRESH",
        }
    }
}

#[derive(Clone)]
pub struct MetadataReidentifyService {
    database: Database,
    candidates: MetadataCandidateService,
    selection: Option<MetadataSelectionService>,
    tmdb: TmdbProvider,
    resolver: Option<ScraperResolver>,
    admin_events: AdminEventHub,
    resources: ResourceMetrics,
}

impl MetadataReidentifyService {
    pub fn new<T>(database: Database, tmdb: T) -> Self
    where
        T: Into<TmdbProvider>,
    {
        Self {
            candidates: MetadataCandidateService::new(database.clone()),
            selection: None,
            database,
            tmdb: tmdb.into(),
            resolver: None,
            admin_events: AdminEventHub::new(),
            resources: ResourceMetrics::new(),
        }
    }

    pub fn with_resolver<T>(database: Database, tmdb: T, resolver: ScraperResolver) -> Self
    where
        T: Into<TmdbProvider>,
    {
        Self::with_resolver_and_selection(database, tmdb, resolver, None)
    }

    pub fn with_selection<T>(
        database: Database,
        tmdb: T,
        selection: Option<MetadataSelectionService>,
    ) -> Self
    where
        T: Into<TmdbProvider>,
    {
        Self {
            candidates: MetadataCandidateService::new(database.clone()),
            selection,
            database,
            tmdb: tmdb.into(),
            resolver: None,
            admin_events: AdminEventHub::new(),
            resources: ResourceMetrics::new(),
        }
    }

    pub fn with_resolver_and_selection<T>(
        database: Database,
        tmdb: T,
        resolver: ScraperResolver,
        selection: Option<MetadataSelectionService>,
    ) -> Self
    where
        T: Into<TmdbProvider>,
    {
        Self {
            candidates: MetadataCandidateService::new(database.clone()),
            selection,
            database,
            tmdb: tmdb.into(),
            resolver: Some(resolver),
            admin_events: AdminEventHub::new(),
            resources: ResourceMetrics::new(),
        }
    }

    pub fn with_admin_events(mut self, admin_events: AdminEventHub) -> Self {
        self.admin_events = admin_events;
        self
    }

    pub fn with_resource_metrics(mut self, resources: ResourceMetrics) -> Self {
        self.resources = resources;
        self
    }

    pub async fn create_job(
        &self,
        item_ids: Vec<String>,
    ) -> Result<MetadataReidentifyJob, MetadataReidentifyError> {
        self.create_job_with_mode(item_ids, MetadataRefreshMode::Reidentify)
            .await
    }

    pub async fn create_item_refresh_job(
        &self,
        item_id: &str,
        mode: MetadataRefreshMode,
    ) -> Result<MetadataReidentifyJob, MetadataReidentifyError> {
        if matches!(mode, MetadataRefreshMode::Reidentify) {
            return Err(MetadataReidentifyError::InvalidRefreshMode);
        }
        let item_ids = self
            .database
            .list_metadata_refresh_item_ids(item_id)
            .await?;
        if item_ids.is_empty() {
            return Err(MetadataReidentifyError::ItemNotFound(item_id.to_owned()));
        }
        self.create_job_with_mode(item_ids, mode).await
    }

    pub async fn create_fill_missing_job(
        &self,
        item_ids: Vec<String>,
    ) -> Result<MetadataReidentifyJob, MetadataReidentifyError> {
        self.create_job_with_mode(item_ids, MetadataRefreshMode::FillMissing)
            .await
    }

    async fn create_job_with_mode(
        &self,
        item_ids: Vec<String>,
        mode: MetadataRefreshMode,
    ) -> Result<MetadataReidentifyJob, MetadataReidentifyError> {
        let mut unique_ids = Vec::with_capacity(item_ids.len());
        for item_id in item_ids {
            if !unique_ids.iter().any(|existing| existing == &item_id) {
                unique_ids.push(item_id);
            }
        }
        if unique_ids.is_empty() || unique_ids.len() > 100 {
            return Err(MetadataReidentifyError::InvalidItemCount);
        }
        for item_id in &unique_ids {
            if self
                .database
                .find_media_item_metadata(item_id)
                .await?
                .is_none()
            {
                return Err(MetadataReidentifyError::ItemNotFound(item_id.clone()));
            }
        }
        let job_id = Uuid::now_v7().to_string();
        self.database
            .create_metadata_reidentify_job(&job_id, &unique_ids, mode.as_str())
            .await?;
        self.admin_events.publish(AdminEventScope::Jobs);
        self.get_job(&job_id).await
    }

    pub async fn create_library_job(
        &self,
        library_id: &str,
    ) -> Result<MetadataReidentifyJob, MetadataReidentifyError> {
        self.create_library_job_with_mode(library_id, MetadataRefreshMode::Reidentify)
            .await
    }

    pub async fn create_library_refresh_job(
        &self,
        library_id: &str,
        mode: MetadataRefreshMode,
    ) -> Result<MetadataReidentifyJob, MetadataReidentifyError> {
        if matches!(mode, MetadataRefreshMode::Reidentify) {
            return Err(MetadataReidentifyError::InvalidRefreshMode);
        }
        self.create_library_job_with_mode(library_id, mode).await
    }

    async fn create_library_job_with_mode(
        &self,
        library_id: &str,
        mode: MetadataRefreshMode,
    ) -> Result<MetadataReidentifyJob, MetadataReidentifyError> {
        let Some(library) = self.database.find_library(library_id).await? else {
            return Err(MetadataReidentifyError::ItemNotFound(library_id.to_owned()));
        };
        if !library.is_enabled {
            return Err(MetadataReidentifyError::ItemNotFound(library_id.to_owned()));
        }
        let mut item_ids = Vec::new();
        let mut offset = 0_i64;
        loop {
            let page = self
                .database
                .list_media_item_ids_for_library(library_id, offset, 500)
                .await?;
            if page.is_empty() {
                break;
            }
            let page_len = i64::try_from(page.len()).unwrap_or(500);
            item_ids.extend(page);
            offset = offset.saturating_add(page_len);
            if page_len < 500 {
                break;
            }
        }
        if item_ids.is_empty() {
            return Err(MetadataReidentifyError::InvalidItemCount);
        }
        let job_id = Uuid::now_v7().to_string();
        self.database
            .create_metadata_reidentify_library_job(&job_id, &item_ids, mode.as_str())
            .await?;
        self.admin_events.publish(AdminEventScope::Jobs);
        self.get_job(&job_id).await
    }

    pub async fn run(&self, job_id: &str) {
        let Ok(Some(job)) = self.database.find_metadata_reidentify_job(job_id).await else {
            return;
        };
        if matches!(job.status.as_str(), "COMPLETED" | "FAILED" | "CANCELLED") {
            return;
        }
        if job.cancel_requested {
            let _ = self
                .database
                .finish_metadata_reidentify_job(job_id, "CANCELLED", None)
                .await;
            return;
        }
        if job.status == "QUEUED"
            && !self
                .database
                .claim_metadata_reidentify_job(job_id)
                .await
                .unwrap_or(false)
        {
            if self
                .database
                .metadata_reidentify_job_cancel_requested(job_id)
                .await
                .unwrap_or(true)
            {
                let _ = self
                    .database
                    .finish_metadata_reidentify_job(job_id, "CANCELLED", None)
                    .await;
            }
            return;
        }
        let mode = match job.mode.as_str() {
            "FILL_MISSING" => MetadataRefreshMode::FillMissing,
            "FULL_REFRESH" => MetadataRefreshMode::FullRefresh,
            _ => MetadataRefreshMode::Reidentify,
        };
        let mut workers = JoinSet::new();
        let mut last_concurrency = None;
        loop {
            let concurrency = self
                .resources
                .background_concurrency(METADATA_MATCH_CONCURRENCY)
                .await;
            if last_concurrency != Some(concurrency) {
                tracing::info!(
                    job_id,
                    concurrency,
                    "metadata refresh worker concurrency adjusted"
                );
                last_concurrency = Some(concurrency);
            }
            while workers.len() < concurrency {
                if self
                    .database
                    .metadata_reidentify_job_cancel_requested(job_id)
                    .await
                    .unwrap_or(true)
                {
                    break;
                }
                let Ok(Some(item_id)) = self.database.next_metadata_reidentify_item(job_id).await
                else {
                    break;
                };
                if !self
                    .database
                    .claim_metadata_reidentify_item(job_id, &item_id)
                    .await
                    .unwrap_or(false)
                {
                    continue;
                }
                let service = self.clone();
                let job_id = job_id.to_owned();
                workers.spawn(async move {
                    service.process_item(&job_id, &item_id, mode).await;
                });
            }
            let Some(worker_result) = workers.join_next().await else {
                break;
            };
            if let Err(error) = worker_result {
                tracing::error!(
                    job_id,
                    worker_cancelled = error.is_cancelled(),
                    worker_panicked = error.is_panic(),
                    "metadata refresh worker stopped before recording its result"
                );
            }
        }
        let reconciliation_failed = match self
            .database
            .fail_running_metadata_reidentify_items(job_id, "WORKER_FAILED")
            .await
        {
            Ok(0) => false,
            Ok(count) => {
                tracing::error!(
                    job_id,
                    item_count = count,
                    "metadata refresh reconciled items left running by workers"
                );
                false
            }
            Err(_) => {
                tracing::error!(job_id, "metadata refresh could not reconcile running items");
                true
            }
        };
        let status = if self
            .database
            .metadata_reidentify_job_cancel_requested(job_id)
            .await
            .unwrap_or(true)
        {
            "CANCELLED"
        } else if reconciliation_failed {
            "FAILED"
        } else {
            match self
                .database
                .metadata_reidentify_job_has_failed_items(job_id)
                .await
            {
                Ok(true) => "FAILED",
                Ok(false) => "COMPLETED",
                Err(_) => "FAILED",
            }
        };
        if self
            .database
            .finish_metadata_reidentify_job(
                job_id,
                status,
                (status == "FAILED").then_some("ITEM_FAILED"),
            )
            .await
            .is_err()
        {
            tracing::error!(job_id, "metadata refresh job status could not be recorded");
        }
        self.admin_events.publish(AdminEventScope::Jobs);
    }

    async fn process_item(&self, job_id: &str, item_id: &str, mode: MetadataRefreshMode) {
        let result = match self.database.find_media_item_metadata(item_id).await {
            Ok(Some(item)) => {
                if !matches!(
                    item.item_type.as_str(),
                    "MOVIE" | "SERIES" | "SEASON" | "EPISODE"
                ) {
                    Ok(0)
                } else if item.title.trim().is_empty() {
                    Err(MetadataReidentifyError::InvalidSearch)
                } else {
                    let skip = if matches!(mode, MetadataRefreshMode::FillMissing) {
                        if let Some(selection) = self.selection.as_ref() {
                            selection
                                .is_fill_missing_complete(item_id)
                                .await
                                .map_err(MetadataReidentifyError::Selection)
                        } else {
                            Ok(false)
                        }
                    } else {
                        Ok(false)
                    };
                    match skip {
                        Ok(true) => Ok(0),
                        Ok(false) => {
                            match self
                                .provider_for_item_with_requirement(
                                    item_id,
                                    !matches!(mode, MetadataRefreshMode::Reidentify),
                                )
                                .await
                            {
                                Ok(Some(provider)) => {
                                    self.refresh_item(item_id, &item, mode, &provider).await
                                }
                                Ok(None) => Ok(0),
                                Err(error) => Err(MetadataReidentifyError::Scraper(error)),
                            }
                        }
                        Err(error) => Err(error),
                    }
                }
            }
            Ok(None) => Err(MetadataReidentifyError::ItemNotFound(item_id.to_owned())),
            Err(error) => Err(MetadataReidentifyError::Storage(error)),
        };
        match result {
            Ok(candidate_count) => {
                let _ = self
                    .database
                    .finish_metadata_reidentify_item(
                        job_id,
                        item_id,
                        "COMPLETED",
                        candidate_count,
                        None,
                    )
                    .await;
                self.admin_events.publish(AdminEventScope::Jobs);
                self.admin_events.publish(AdminEventScope::Metadata);
            }
            Err(MetadataReidentifyError::LowConfidence) => {
                let code = MetadataReidentifyError::LowConfidence.code();
                if self
                    .database
                    .finish_metadata_reidentify_item(job_id, item_id, "COMPLETED", 0, Some(code))
                    .await
                    .is_err()
                {
                    tracing::error!(
                        job_id,
                        item_id,
                        error_code = code,
                        "metadata refresh item result could not be recorded"
                    );
                }
                self.admin_events.publish(AdminEventScope::Jobs);
                self.admin_events.publish(AdminEventScope::Metadata);
            }
            Err(error) => {
                let code = error.code();
                if self
                    .database
                    .finish_metadata_reidentify_item(job_id, item_id, "FAILED", 0, Some(code))
                    .await
                    .is_err()
                {
                    tracing::error!(
                        job_id,
                        item_id,
                        error_code = code,
                        "metadata refresh item result could not be recorded"
                    );
                }
                self.admin_events.publish(AdminEventScope::Jobs);
                self.admin_events.publish(AdminEventScope::Metadata);
            }
        }
    }

    async fn provider_for_item_with_requirement(
        &self,
        item_id: &str,
        require_selected_scraper: bool,
    ) -> Result<Option<TmdbProvider>, ScraperError> {
        let Some(resolver) = &self.resolver else {
            return Ok(Some(self.tmdb.clone()));
        };
        let client = resolver.for_item(item_id).await?;
        if require_selected_scraper && client.is_none() {
            return Ok(None);
        }
        Ok(Some(
            client
                .map(TmdbProvider::from_scraper)
                .unwrap_or_else(|| self.tmdb.clone()),
        ))
    }

    async fn refresh_item(
        &self,
        item_id: &str,
        item: &crate::storage::StoredMediaMetadata,
        mode: MetadataRefreshMode,
        provider: &TmdbProvider,
    ) -> Result<i64, MetadataReidentifyError> {
        let page = self
            .candidates
            .search_and_store(
                item_id,
                &item.title,
                item.production_year
                    .and_then(|year| i32::try_from(year).ok()),
                provider,
            )
            .await
            .map_err(MetadataReidentifyError::Candidate)?;
        if matches!(mode, MetadataRefreshMode::Reidentify) {
            return Ok(i64::try_from(page.items.len()).unwrap_or(i64::MAX));
        }
        let Some(selection) = self.selection.as_ref() else {
            return Err(MetadataReidentifyError::SelectionUnavailable);
        };
        let Some(candidate) = page
            .items
            .iter()
            .filter(|candidate| candidate.status == "PENDING")
            .max_by(|left, right| left.score.total_cmp(&right.score))
        else {
            return Err(MetadataReidentifyError::LowConfidence);
        };
        if candidate.score < 80.0 {
            return Err(MetadataReidentifyError::LowConfidence);
        }
        let selection_mode = match mode {
            MetadataRefreshMode::FillMissing => MetadataSelectionMode::FillMissing,
            MetadataRefreshMode::FullRefresh => MetadataSelectionMode::RefreshUnlocked,
            MetadataRefreshMode::Reidentify => return Ok(0),
        };
        selection
            .select(item_id, &candidate.id, selection_mode)
            .await
            .map_err(MetadataReidentifyError::Selection)?;
        Ok(i64::try_from(page.items.len()).unwrap_or(i64::MAX))
    }

    pub async fn get_job(
        &self,
        job_id: &str,
    ) -> Result<MetadataReidentifyJob, MetadataReidentifyError> {
        let Some(job) = self.database.find_metadata_reidentify_job(job_id).await? else {
            return Err(MetadataReidentifyError::JobNotFound);
        };
        let items = self
            .database
            .list_metadata_reidentify_items(job_id, 0, METADATA_JOB_ITEM_PAGE_SIZE)
            .await?;
        Ok(MetadataReidentifyJob {
            id: job.id,
            status: job.status,
            mode: job.mode,
            processed_count: job.processed_count,
            total_count: job.total_count,
            error: job.error,
            created_at: job.created_at,
            updated_at: job.updated_at,
            started_at: job.started_at,
            finished_at: job.finished_at,
            items: items.into_iter().map(metadata_reidentify_item).collect(),
            cancel_requested: job.cancel_requested,
            library_id: job.library_id,
            pending_count: job.pending_count,
        })
    }

    pub async fn cancel(&self, job_id: &str) -> Result<(), MetadataReidentifyError> {
        let Some(job) = self.database.find_metadata_reidentify_job(job_id).await? else {
            return Err(MetadataReidentifyError::JobNotFound);
        };
        if !matches!(job.status.as_str(), "QUEUED" | "RUNNING") {
            return Err(MetadataReidentifyError::JobNotCancelable);
        }
        if !self
            .database
            .request_metadata_reidentify_job_cancel(job_id)
            .await?
        {
            return Err(MetadataReidentifyError::JobNotCancelable);
        }
        self.admin_events.publish(AdminEventScope::Jobs);
        Ok(())
    }

    pub async fn retry_job(
        &self,
        job_id: &str,
    ) -> Result<MetadataReidentifyJob, MetadataReidentifyError> {
        let job = self.get_job(job_id).await?;
        if !matches!(job.status.as_str(), "FAILED" | "CANCELLED")
            || !self.database.retry_metadata_reidentify_job(job_id).await?
        {
            return Err(MetadataReidentifyError::JobNotRetryable);
        }
        self.admin_events.publish(AdminEventScope::Jobs);
        self.get_job(job_id).await
    }

    pub async fn active_job_ids(&self) -> Result<Vec<String>, StorageError> {
        self.database
            .list_active_metadata_reidentify_job_ids()
            .await
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataReidentifyJob {
    pub id: String,
    pub status: String,
    pub processed_count: i64,
    pub total_count: i64,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub mode: String,
    pub items: Vec<MetadataReidentifyItem>,
    pub cancel_requested: bool,
    pub library_id: Option<String>,
    pub pending_count: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataReidentifyItem {
    pub job_id: String,
    pub item_id: String,
    pub status: String,
    pub candidate_count: i64,
    pub error: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug)]
pub enum MetadataReidentifyError {
    InvalidItemCount,
    InvalidRefreshMode,
    InvalidSearch,
    ItemNotFound(String),
    JobNotFound,
    JobNotRetryable,
    JobNotCancelable,
    Candidate(MetadataCandidateError),
    Scraper(ScraperError),
    Selection(MetadataSelectionError),
    SelectionUnavailable,
    LowConfidence,
    Storage(StorageError),
}

impl MetadataReidentifyError {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidItemCount => "INVALID_ITEM_COUNT",
            Self::InvalidRefreshMode => "INVALID_REFRESH_MODE",
            Self::InvalidSearch => "INVALID_SEARCH",
            Self::ItemNotFound(_) => "ITEM_NOT_FOUND",
            Self::JobNotFound => "JOB_NOT_FOUND",
            Self::JobNotRetryable => "JOB_NOT_RETRYABLE",
            Self::JobNotCancelable => "JOB_NOT_CANCELABLE",
            Self::Candidate(MetadataCandidateError::Tmdb(_)) => "TMDB_UNAVAILABLE",
            Self::Candidate(MetadataCandidateError::InvalidSearch) => "INVALID_SEARCH",
            Self::Candidate(MetadataCandidateError::ItemNotFound) => "ITEM_NOT_FOUND",
            Self::Candidate(MetadataCandidateError::InvalidCandidateJson(_)) => "CANDIDATE_ERROR",
            Self::Candidate(MetadataCandidateError::Scraper(_)) => "SCRAPER_UNAVAILABLE",
            Self::Candidate(MetadataCandidateError::Storage(_)) => "STORAGE_ERROR",
            Self::Scraper(_) => "SCRAPER_UNAVAILABLE",
            Self::Selection(MetadataSelectionError::Nfo(_)) => "METADATA_NFO_WRITE_FAILED",
            Self::Selection(MetadataSelectionError::Image(_)) => "METADATA_IMAGE_WRITE_FAILED",
            Self::Selection(MetadataSelectionError::People(_)) => "METADATA_PEOPLE_WRITE_FAILED",
            Self::Selection(MetadataSelectionError::Storage(_)) => "METADATA_STORAGE_FAILED",
            Self::Selection(_) => "METADATA_WRITE_FAILED",
            Self::SelectionUnavailable => "METADATA_WRITE_UNAVAILABLE",
            Self::LowConfidence => "LOW_CONFIDENCE",
            Self::Storage(_) => "STORAGE_ERROR",
        }
    }
}

impl fmt::Display for MetadataReidentifyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidItemCount => {
                formatter.write_str("metadata reidentify item count is invalid")
            }
            Self::InvalidRefreshMode => formatter.write_str("metadata refresh mode is invalid"),
            Self::InvalidSearch => formatter.write_str("metadata reidentify search is invalid"),
            Self::ItemNotFound(id) => write!(formatter, "media item not found: {id}"),
            Self::JobNotFound => formatter.write_str("metadata reidentify job not found"),
            Self::JobNotRetryable => {
                formatter.write_str("metadata reidentify job is not retryable")
            }
            Self::JobNotCancelable => {
                formatter.write_str("metadata reidentify job is not cancelable")
            }
            Self::Candidate(error) => error.fmt(formatter),
            Self::Scraper(error) => error.fmt(formatter),
            Self::Selection(error) => error.fmt(formatter),
            Self::SelectionUnavailable => {
                formatter.write_str("metadata selection service is unavailable")
            }
            Self::LowConfidence => formatter.write_str("metadata candidate confidence is too low"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MetadataReidentifyError {}

impl From<MetadataSelectionError> for MetadataReidentifyError {
    fn from(error: MetadataSelectionError) -> Self {
        Self::Selection(error)
    }
}

impl From<StorageError> for MetadataReidentifyError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

fn metadata_reidentify_item(item: StoredMetadataReidentifyItem) -> MetadataReidentifyItem {
    MetadataReidentifyItem {
        job_id: item.job_id,
        item_id: item.item_id,
        status: item.status,
        candidate_count: item.candidate_count,
        error: item.error,
        updated_at: item.updated_at,
    }
}
