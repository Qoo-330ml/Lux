use std::fmt;

use uuid::Uuid;

use crate::{
    application::{
        candidates::{MetadataCandidateError, MetadataCandidateService},
        tmdb::TmdbClient,
    },
    storage::{Database, StorageError, StoredMetadataReidentifyItem},
};

#[derive(Clone)]
pub struct MetadataReidentifyService {
    database: Database,
    candidates: MetadataCandidateService,
    tmdb: TmdbClient,
}

impl MetadataReidentifyService {
    pub fn new(database: Database, tmdb: TmdbClient) -> Self {
        Self {
            candidates: MetadataCandidateService::new(database.clone()),
            database,
            tmdb,
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

    pub async fn run(&self, job_id: &str) {
        let Ok(Some(job)) = self.database.find_metadata_reidentify_job(job_id).await else {
            return;
        };
        if matches!(job.status.as_str(), "COMPLETED" | "FAILED" | "CANCELLED")
            || !self
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
                        self.candidates
                            .search_and_store(&item_id, &item.title, None, &self.tmdb)
                            .await
                            .map(|page| i64::try_from(page.items.len()).unwrap_or(i64::MAX))
                            .map_err(MetadataReidentifyError::Candidate)
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
