use std::fmt;

use uuid::Uuid;

use crate::{
    application::{
        candidates::{MetadataCandidateError, MetadataCandidateService},
        scraper::{ScraperError, ScraperResolver},
        tmdb_plugin::TmdbProvider,
    },
    storage::{Database, StorageError, StoredMetadataReidentifyItem},
};

#[derive(Clone)]
pub struct MetadataReidentifyService {
    database: Database,
    candidates: MetadataCandidateService,
    tmdb: TmdbProvider,
    resolver: Option<ScraperResolver>,
}

impl MetadataReidentifyService {
    pub fn new<T>(database: Database, tmdb: T) -> Self
    where
        T: Into<TmdbProvider>,
    {
        Self {
            candidates: MetadataCandidateService::new(database.clone()),
            database,
            tmdb: tmdb.into(),
            resolver: None,
        }
    }

    pub fn with_resolver<T>(database: Database, tmdb: T, resolver: ScraperResolver) -> Self
    where
        T: Into<TmdbProvider>,
    {
        Self {
            candidates: MetadataCandidateService::new(database.clone()),
            database,
            tmdb: tmdb.into(),
            resolver: Some(resolver),
        }
    }

    pub async fn create_job(
        &self,
        item_ids: Vec<String>,
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
            .create_metadata_reidentify_job(&job_id, &unique_ids)
            .await?;
        self.get_job(&job_id).await
    }

    pub async fn create_library_jobs(
        &self,
        library_id: &str,
    ) -> Result<MetadataReidentifyBatch, MetadataReidentifyError> {
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
        let total_count = i64::try_from(item_ids.len()).unwrap_or(i64::MAX);
        let mut jobs = Vec::new();
        for chunk in item_ids.chunks(100) {
            jobs.push(self.create_job(chunk.to_vec()).await?);
        }
        Ok(MetadataReidentifyBatch { jobs, total_count })
    }

    pub async fn run(&self, job_id: &str) {
        let Ok(Some(job)) = self.database.find_metadata_reidentify_job(job_id).await else {
            return;
        };
        if matches!(job.status.as_str(), "COMPLETED" | "FAILED" | "CANCELLED") {
            return;
        }
        if job.status == "QUEUED"
            && !self
                .database
                .claim_metadata_reidentify_job(job_id)
                .await
                .unwrap_or(false)
        {
            return;
        }
        while let Ok(Some(item_id)) = self.database.next_metadata_reidentify_item(job_id).await {
            if !self
                .database
                .claim_metadata_reidentify_item(job_id, &item_id)
                .await
                .unwrap_or(false)
            {
                continue;
            }
            let result = match self.database.find_media_item_metadata(&item_id).await {
                Ok(Some(item)) => {
                    if item.title.trim().is_empty() {
                        Err(MetadataReidentifyError::InvalidSearch)
                    } else {
                        match self.provider_for_item(&item_id).await {
                            Ok(provider) => self
                                .candidates
                                .search_and_store(&item_id, &item.title, None, &provider)
                                .await
                                .map(|page| i64::try_from(page.items.len()).unwrap_or(i64::MAX))
                                .map_err(MetadataReidentifyError::Candidate),
                            Err(error) => Err(MetadataReidentifyError::Scraper(error)),
                        }
                    }
                }
                Ok(None) => Err(MetadataReidentifyError::ItemNotFound(item_id.clone())),
                Err(error) => Err(MetadataReidentifyError::Storage(error)),
            };
            match result {
                Ok(candidate_count) => {
                    let _ = self
                        .database
                        .finish_metadata_reidentify_item(
                            job_id,
                            &item_id,
                            "COMPLETED",
                            candidate_count,
                            None,
                        )
                        .await;
                }
                Err(error) => {
                    let code = error.code();
                    let _ = self
                        .database
                        .finish_metadata_reidentify_item(job_id, &item_id, "FAILED", 0, Some(code))
                        .await;
                }
            }
        }
        let status = match self.database.list_metadata_reidentify_items(job_id).await {
            Ok(items) if items.iter().any(|item| item.status == "FAILED") => "FAILED",
            Ok(_) => "COMPLETED",
            Err(_) => "FAILED",
        };
        let _ = self
            .database
            .finish_metadata_reidentify_job(
                job_id,
                status,
                (status == "FAILED").then_some("ITEM_FAILED"),
            )
            .await;
    }

    async fn provider_for_item(&self, item_id: &str) -> Result<TmdbProvider, ScraperError> {
        let Some(resolver) = &self.resolver else {
            return Ok(self.tmdb.clone());
        };
        resolver.for_item(item_id).await.map(|client| {
            client
                .map(TmdbProvider::from_scraper)
                .unwrap_or_else(|| self.tmdb.clone())
        })
    }

    pub async fn get_job(
        &self,
        job_id: &str,
    ) -> Result<MetadataReidentifyJob, MetadataReidentifyError> {
        let Some(job) = self.database.find_metadata_reidentify_job(job_id).await? else {
            return Err(MetadataReidentifyError::JobNotFound);
        };
        let items = self.database.list_metadata_reidentify_items(job_id).await?;
        Ok(MetadataReidentifyJob {
            id: job.id,
            status: job.status,
            processed_count: job.processed_count,
            total_count: job.total_count,
            error: job.error,
            created_at: job.created_at,
            updated_at: job.updated_at,
            started_at: job.started_at,
            finished_at: job.finished_at,
            items: items.into_iter().map(metadata_reidentify_item).collect(),
        })
    }

    pub async fn retry_job(
        &self,
        job_id: &str,
    ) -> Result<MetadataReidentifyJob, MetadataReidentifyError> {
        let job = self.get_job(job_id).await?;
        if job.status != "FAILED" || !self.database.retry_metadata_reidentify_job(job_id).await? {
            return Err(MetadataReidentifyError::JobNotRetryable);
        }
        self.get_job(job_id).await
    }

    pub async fn active_job_ids(&self) -> Result<Vec<String>, StorageError> {
        self.database
            .list_active_metadata_reidentify_job_ids()
            .await
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataReidentifyBatch {
    pub jobs: Vec<MetadataReidentifyJob>,
    pub total_count: i64,
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
    pub items: Vec<MetadataReidentifyItem>,
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
    InvalidSearch,
    ItemNotFound(String),
    JobNotFound,
    JobNotRetryable,
    Candidate(MetadataCandidateError),
    Scraper(ScraperError),
    Storage(StorageError),
}

impl MetadataReidentifyError {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidItemCount => "INVALID_ITEM_COUNT",
            Self::InvalidSearch => "INVALID_SEARCH",
            Self::ItemNotFound(_) => "ITEM_NOT_FOUND",
            Self::JobNotFound => "JOB_NOT_FOUND",
            Self::JobNotRetryable => "JOB_NOT_RETRYABLE",
            Self::Candidate(MetadataCandidateError::Tmdb(_)) => "TMDB_UNAVAILABLE",
            Self::Candidate(MetadataCandidateError::InvalidSearch) => "INVALID_SEARCH",
            Self::Candidate(_) => "CANDIDATE_ERROR",
            Self::Scraper(_) => "SCRAPER_UNAVAILABLE",
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
            Self::InvalidSearch => formatter.write_str("metadata reidentify search is invalid"),
            Self::ItemNotFound(id) => write!(formatter, "media item not found: {id}"),
            Self::JobNotFound => formatter.write_str("metadata reidentify job not found"),
            Self::JobNotRetryable => {
                formatter.write_str("metadata reidentify job is not retryable")
            }
            Self::Candidate(error) => error.fmt(formatter),
            Self::Scraper(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MetadataReidentifyError {}

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
