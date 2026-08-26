use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use tokio::{
    sync::{Mutex as AsyncMutex, Semaphore},
    task::JoinSet,
};
use uuid::Uuid;

use crate::{
    application::{
        actor_enrichment::ActorEnrichmentQueue,
        admin_events::{AdminEventHub, AdminEventScope},
        candidates::{
            ImageSelectionPolicy, MetadataCandidateError, MetadataCandidatePage,
            MetadataCandidateService, MetadataCandidateView, MetadataRequestPlan,
            MetadataSelectionError, MetadataSelectionMode, MetadataSelectionService,
        },
        scraper::{ResolvedScraper, ScraperError, ScraperProvider, ScraperResolver},
        webhooks::{WebhookEventType, WebhookService},
    },
    config::DatabaseBackend,
    observability::resources::ResourceMetrics,
    storage::{Database, StorageError, StoredMetadataReidentifyItem},
};

pub const METADATA_MATCH_CONCURRENCY: usize = 16;
const METADATA_GLOBAL_WORKER_LIMIT: usize = METADATA_MATCH_CONCURRENCY;
const SQLITE_METADATA_DEFAULT_CONCURRENCY: usize = 4;
const POSTGRES_METADATA_DEFAULT_CONCURRENCY: usize = 8;
const METADATA_JOB_ITEM_PAGE_SIZE: i64 = 100;
const METADATA_PROGRESS_EVENT_INTERVAL: Duration = Duration::from_secs(1);
const AUTO_MATCH_MIN_SCORE: f64 = 85.0;
const AUTO_MATCH_MIN_MARGIN: f64 = 5.0;

static METADATA_GLOBAL_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

fn metadata_global_permits() -> Arc<Semaphore> {
    METADATA_GLOBAL_PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(METADATA_GLOBAL_WORKER_LIMIT)))
        .clone()
}

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
    scraper: ScraperProvider,
    resolver: Option<ScraperResolver>,
    admin_events: AdminEventHub,
    resources: ResourceMetrics,
    webhooks: Option<WebhookService>,
    progress_events: MetadataProgressEventGate,
    worker_permits: Arc<Semaphore>,
    running_jobs: MetadataJobOwners,
    library_job_creation: Arc<AsyncMutex<()>>,
    actor_enrichment: ActorEnrichmentQueue,
}

#[derive(Clone, Default)]
struct MetadataProgressEventGate {
    last_published: Arc<Mutex<HashMap<String, Instant>>>,
}

impl MetadataProgressEventGate {
    fn should_publish(&self, job_id: &str) -> bool {
        let now = Instant::now();
        let Ok(mut last_published) = self.last_published.lock() else {
            return true;
        };
        if last_published
            .get(job_id)
            .is_some_and(|last| now.duration_since(*last) < METADATA_PROGRESS_EVENT_INTERVAL)
        {
            return false;
        }
        last_published.insert(job_id.to_owned(), now);
        true
    }

    fn clear(&self, job_id: &str) {
        if let Ok(mut last_published) = self.last_published.lock() {
            last_published.remove(job_id);
        }
    }
}

#[derive(Clone, Default)]
struct MetadataJobOwners {
    active: Arc<Mutex<HashSet<String>>>,
}

impl MetadataJobOwners {
    fn claim(&self, job_id: &str) -> Option<MetadataJobOwnerGuard> {
        let Ok(mut active) = self.active.lock() else {
            return None;
        };
        if !active.insert(job_id.to_owned()) {
            return None;
        }
        Some(MetadataJobOwnerGuard {
            job_id: job_id.to_owned(),
            active: Arc::clone(&self.active),
        })
    }
}

struct MetadataJobOwnerGuard {
    job_id: String,
    active: Arc<Mutex<HashSet<String>>>,
}

enum RefreshItemOutcome {
    Confirmed(i64),
    NeedsReview(i64),
}

struct RefreshItemOptions {
    scraper_id: Option<String>,
    supplemental: bool,
    request_plan: Option<MetadataRequestPlan>,
    image_policy: Option<ImageSelectionPolicy>,
}

impl Drop for MetadataJobOwnerGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(&self.job_id);
        }
    }
}

impl MetadataReidentifyService {
    pub fn new<T>(database: Database, scraper: T) -> Self
    where
        T: Into<ScraperProvider>,
    {
        Self {
            candidates: MetadataCandidateService::new(database.clone()),
            selection: None,
            database,
            scraper: scraper.into(),
            resolver: None,
            admin_events: AdminEventHub::new(),
            resources: ResourceMetrics::new(),
            webhooks: None,
            progress_events: MetadataProgressEventGate::default(),
            worker_permits: metadata_global_permits(),
            running_jobs: MetadataJobOwners::default(),
            library_job_creation: Arc::new(AsyncMutex::new(())),
            actor_enrichment: ActorEnrichmentQueue::new(),
        }
    }

    pub fn with_resolver<T>(database: Database, scraper: T, resolver: ScraperResolver) -> Self
    where
        T: Into<ScraperProvider>,
    {
        Self::with_resolver_and_selection(database, scraper, resolver, None)
    }

    pub fn with_selection<T>(
        database: Database,
        scraper: T,
        selection: Option<MetadataSelectionService>,
    ) -> Self
    where
        T: Into<ScraperProvider>,
    {
        Self {
            candidates: MetadataCandidateService::new(database.clone()),
            selection,
            database,
            scraper: scraper.into(),
            resolver: None,
            admin_events: AdminEventHub::new(),
            resources: ResourceMetrics::new(),
            webhooks: None,
            progress_events: MetadataProgressEventGate::default(),
            worker_permits: metadata_global_permits(),
            running_jobs: MetadataJobOwners::default(),
            library_job_creation: Arc::new(AsyncMutex::new(())),
            actor_enrichment: ActorEnrichmentQueue::new(),
        }
    }

    pub fn with_resolver_and_selection<T>(
        database: Database,
        scraper: T,
        resolver: ScraperResolver,
        selection: Option<MetadataSelectionService>,
    ) -> Self
    where
        T: Into<ScraperProvider>,
    {
        Self {
            candidates: MetadataCandidateService::new(database.clone()),
            selection,
            database,
            scraper: scraper.into(),
            resolver: Some(resolver),
            admin_events: AdminEventHub::new(),
            resources: ResourceMetrics::new(),
            webhooks: None,
            progress_events: MetadataProgressEventGate::default(),
            worker_permits: metadata_global_permits(),
            running_jobs: MetadataJobOwners::default(),
            library_job_creation: Arc::new(AsyncMutex::new(())),
            actor_enrichment: ActorEnrichmentQueue::new(),
        }
    }

    pub fn with_admin_events(mut self, admin_events: AdminEventHub) -> Self {
        self.admin_events = admin_events;
        self
    }

    pub fn with_resource_metrics(mut self, resources: ResourceMetrics) -> Self {
        self.selection = self
            .selection
            .clone()
            .map(|selection| selection.with_resource_metrics(resources.clone()));
        self.scraper = self
            .scraper
            .clone()
            .with_resource_metrics(resources.clone());
        self.resolver = self
            .resolver
            .clone()
            .map(|resolver| resolver.with_resource_metrics(resources.clone()));
        self.resources = resources;
        self
    }

    pub fn with_webhooks(mut self, webhooks: WebhookService) -> Self {
        self.webhooks = Some(webhooks);
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

    pub async fn enqueue_selected_actor_enrichment(
        &self,
        item_id: &str,
        candidate_id: &str,
    ) -> Result<(), MetadataReidentifyError> {
        let Some(selection) = self.selection.as_ref() else {
            return Ok(());
        };
        let candidate = self
            .database
            .find_metadata_candidate(item_id, candidate_id)
            .await?
            .ok_or(MetadataReidentifyError::Selection(
                MetadataSelectionError::CandidateNotFound,
            ))?;
        let scrapers = self
            .providers_for_item(item_id, false)
            .await
            .map_err(MetadataReidentifyError::Scraper)?
            .unwrap_or_default();
        let scraper = scrapers
            .into_iter()
            .find(|scraper| {
                scraper
                    .provider
                    .provider_key()
                    .eq_ignore_ascii_case(&candidate.provider)
            })
            .map(|scraper| scraper.provider)
            .unwrap_or_else(|| self.scraper.clone());
        if !self
            .actor_enrichment
            .enqueue(item_id, candidate_id, selection.clone(), scraper)
            .await
        {
            tracing::warn!(
                item_id,
                candidate_id,
                "actor metadata enrichment queue is full"
            );
        }
        Ok(())
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
        let _creation_guard = self.library_job_creation.lock().await;
        if let Some(job_id) = self
            .database
            .active_library_metadata_reidentify_job_id()
            .await?
        {
            return Err(MetadataReidentifyError::LibraryJobAlreadyActive(job_id));
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
            .create_metadata_reidentify_library_job(&job_id, library_id, &item_ids, mode.as_str())
            .await?;
        self.admin_events.publish(AdminEventScope::Jobs);
        self.get_job(&job_id).await
    }

    pub async fn run(&self, job_id: &str) {
        let Some(_owner) = self.running_jobs.claim(job_id) else {
            return;
        };
        let Ok(Some(job)) = self.database.find_metadata_reidentify_job(job_id).await else {
            return;
        };
        if matches!(
            job.status.as_str(),
            "COMPLETED" | "COMPLETED_WITH_ISSUES" | "DEFERRED" | "FAILED" | "CANCELLED"
        ) {
            return;
        }
        if job.cancel_requested {
            let _ = self
                .database
                .finish_metadata_reidentify_job(job_id, "CANCELLED", None)
                .await;
            self.publish_job_finished(job_id);
            return;
        }
        if job.status == "RUNNING"
            && self
                .database
                .requeue_running_metadata_reidentify_items(job_id)
                .await
                .is_err()
        {
            tracing::error!(
                job_id,
                "metadata refresh could not requeue interrupted items"
            );
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
        if matches!(mode, MetadataRefreshMode::FullRefresh) {
            self.scraper.clear_response_cache().await;
        }
        let mut workers = JoinSet::new();
        let mut last_concurrency = None;
        loop {
            let configured_concurrency =
                metadata_worker_default_concurrency(self.database.backend());
            let concurrency = metadata_worker_concurrency(
                self.resources
                    .metadata_concurrency(configured_concurrency)
                    .await,
            );
            if last_concurrency != Some(concurrency) {
                tracing::info!(
                    job_id,
                    concurrency,
                    "metadata refresh worker concurrency adjusted"
                );
                last_concurrency = Some(concurrency);
            }
            let mut queue_exhausted = false;
            while workers.len() < concurrency {
                let queue_wait_started = Instant::now();
                let Ok(worker_permit) = Arc::clone(&self.worker_permits).acquire_owned().await
                else {
                    queue_exhausted = true;
                    break;
                };
                if self
                    .database
                    .metadata_reidentify_job_cancel_requested(job_id)
                    .await
                    .unwrap_or(true)
                {
                    queue_exhausted = true;
                    break;
                }
                let Ok(Some(item_id)) = self.database.next_metadata_reidentify_item(job_id).await
                else {
                    queue_exhausted = true;
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
                self.resources
                    .record_metadata_stage("queue_wait", queue_wait_started.elapsed());
                workers.spawn(async move {
                    let _worker_permit = worker_permit;
                    service.process_item(&job_id, &item_id, mode).await;
                });
            }
            if workers.is_empty() && queue_exhausted {
                break;
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
                .metadata_reidentify_job_has_item_error(job_id, "SCRAPER_UNAVAILABLE")
                .await
            {
                Ok(true) => "DEFERRED",
                Ok(false) => match self
                    .database
                    .metadata_reidentify_job_has_failed_items(job_id)
                    .await
                {
                    Ok(true) => "COMPLETED_WITH_ISSUES",
                    Ok(false) => "COMPLETED",
                    Err(_) => "FAILED",
                },
                Err(_) => "FAILED",
            }
        };
        let error = match status {
            "FAILED" => Some("ITEM_FAILED"),
            "COMPLETED_WITH_ISSUES" => Some("ITEM_ISSUES"),
            "DEFERRED" => Some("DEFERRED_PROVIDER_UNAVAILABLE"),
            _ => None,
        };
        let finish_result = self
            .database
            .finish_metadata_reidentify_job(job_id, status, error)
            .await;
        if let Err(error) = finish_result {
            tracing::error!(job_id, %error, "metadata refresh job status could not be recorded");
            return;
        }
        if status == "FAILED" {
            self.publish_webhook(
                WebhookEventType::JobFailed,
                &format!("job-failed:{job_id}"),
                json!({
                    "jobId": job_id,
                    "jobType": "METADATA_REIDENTIFY",
                    "mode": job.mode,
                    "status": status,
                    "totalCount": job.total_count,
                    "processedCount": job.processed_count,
                    "errorCode": "ITEM_FAILED",
                }),
            )
            .await;
        }
        self.publish_job_finished(job_id);
    }

    async fn process_item(&self, job_id: &str, item_id: &str, mode: MetadataRefreshMode) {
        let item_started = Instant::now();
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
                    let request_plan = if matches!(mode, MetadataRefreshMode::FillMissing) {
                        if let Some(selection) = self.selection.as_ref() {
                            selection
                                .fill_missing_request_plan_for_current(item_id, &item)
                                .await
                                .map_err(MetadataReidentifyError::Selection)
                                .map(Some)
                        } else {
                            Ok(None)
                        }
                    } else {
                        Ok(None)
                    };
                    match request_plan {
                        Ok(Some(plan)) if metadata_request_plan_is_complete(plan) => Ok(0),
                        Ok(request_plan) => {
                            match self
                                .providers_for_item(
                                    item_id,
                                    !matches!(mode, MetadataRefreshMode::Reidentify),
                                )
                                .await
                            {
                                Ok(Some(providers)) => {
                                    self.refresh_with_scraper_roles(
                                        item_id,
                                        &item,
                                        mode,
                                        &providers,
                                        request_plan,
                                    )
                                    .await
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
        self.resources
            .record_metadata_stage("item_total", item_started.elapsed());
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
                self.publish_job_progress(job_id);
                if !matches!(mode, MetadataRefreshMode::Reidentify) {
                    self.publish_webhook(
                        WebhookEventType::MetadataUpdated,
                        &format!("metadata-updated:{job_id}:{item_id}"),
                        json!({
                            "jobId": job_id,
                            "itemId": item_id,
                            "mode": mode.as_str(),
                            "status": "COMPLETED",
                            "candidateCount": candidate_count,
                        }),
                    )
                    .await;
                }
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
                self.publish_job_progress(job_id);
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
                self.publish_job_progress(job_id);
            }
        }
    }

    async fn publish_webhook(&self, event_type: WebhookEventType, dedupe_key: &str, data: Value) {
        let Some(webhooks) = self.webhooks.as_ref() else {
            return;
        };
        if let Err(_error) = webhooks
            .publish(event_type, dedupe_key, unix_now(), data)
            .await
        {
            tracing::warn!(
                event_type = event_type.as_str(),
                "failed to enqueue webhook event"
            );
        }
    }

    fn publish_job_progress(&self, job_id: &str) {
        if self.progress_events.should_publish(job_id) {
            self.admin_events.publish(AdminEventScope::Jobs);
        }
    }

    fn publish_job_finished(&self, job_id: &str) {
        self.progress_events.clear(job_id);
        self.admin_events.publish(AdminEventScope::Jobs);
    }

    async fn providers_for_item(
        &self,
        item_id: &str,
        require_selected_scraper: bool,
    ) -> Result<Option<Vec<ResolvedScraper>>, ScraperError> {
        let Some(resolver) = &self.resolver else {
            return Ok(Some(vec![ResolvedScraper {
                scraper_id: self
                    .scraper
                    .plugin_id()
                    .unwrap_or(self.scraper.provider_key())
                    .to_owned(),
                role: crate::library::LibraryScraperRole::Primary,
                provider: self.scraper.clone(),
            }]));
        };
        let clients = resolver.for_item_ordered(item_id).await?;
        if require_selected_scraper && clients.is_empty() {
            return Ok(None);
        }
        if clients.is_empty() {
            return Ok(Some(vec![ResolvedScraper {
                scraper_id: self
                    .scraper
                    .plugin_id()
                    .unwrap_or(self.scraper.provider_key())
                    .to_owned(),
                role: crate::library::LibraryScraperRole::Primary,
                provider: self.scraper.clone(),
            }]));
        }
        Ok(Some(clients))
    }

    async fn refresh_with_scraper_roles(
        &self,
        item_id: &str,
        item: &crate::storage::StoredMediaMetadata,
        mode: MetadataRefreshMode,
        scrapers: &[ResolvedScraper],
        request_plan: Option<MetadataRequestPlan>,
    ) -> Result<i64, MetadataReidentifyError> {
        let mut candidate_count = 0_i64;
        let mut selected_scraper_id = None;
        let mut last_recoverable_error = None;
        let mut saw_needs_review = false;
        for scraper in scrapers.iter().filter(|scraper| {
            matches!(
                scraper.role,
                crate::library::LibraryScraperRole::Primary
                    | crate::library::LibraryScraperRole::Backup
                    | crate::library::LibraryScraperRole::Both
            )
        }) {
            match self
                .refresh_item(
                    item_id,
                    item,
                    mode,
                    &scraper.provider,
                    RefreshItemOptions {
                        scraper_id: Some(scraper.scraper_id.clone()),
                        supplemental: false,
                        request_plan,
                        image_policy: request_plan.and_then(|plan| plan.image_policy),
                    },
                )
                .await
            {
                Ok(RefreshItemOutcome::Confirmed(count)) => {
                    candidate_count = candidate_count.saturating_add(count);
                    selected_scraper_id = Some(scraper.scraper_id.as_str());
                    break;
                }
                Ok(RefreshItemOutcome::NeedsReview(count)) => {
                    candidate_count = candidate_count.saturating_add(count);
                    last_recoverable_error = Some(MetadataReidentifyError::LowConfidence);
                    saw_needs_review = true;
                }
                Err(error) if recoverable_scraper_attempt(&error) => {
                    last_recoverable_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        if selected_scraper_id.is_none() {
            if saw_needs_review {
                return Ok(candidate_count);
            }
            return Err(last_recoverable_error.unwrap_or(MetadataReidentifyError::LowConfidence));
        }
        if !matches!(mode, MetadataRefreshMode::Reidentify) {
            for scraper in scrapers.iter().filter(|scraper| {
                matches!(
                    scraper.role,
                    crate::library::LibraryScraperRole::Supplement
                        | crate::library::LibraryScraperRole::Both
                )
            }) {
                let supplemental_plan = if matches!(mode, MetadataRefreshMode::FillMissing) {
                    if let Some(selection) = self.selection.as_ref() {
                        Some(
                            selection
                                .fill_missing_request_plan(item_id)
                                .await
                                .map_err(MetadataReidentifyError::Selection)?,
                        )
                    } else {
                        None
                    }
                } else {
                    None
                };
                if supplemental_plan.is_some_and(metadata_request_plan_is_complete) {
                    break;
                }
                match self
                    .refresh_item(
                        item_id,
                        item,
                        mode,
                        &scraper.provider,
                        RefreshItemOptions {
                            scraper_id: None,
                            supplemental: true,
                            request_plan: supplemental_plan,
                            image_policy: supplemental_plan.and_then(|plan| plan.image_policy),
                        },
                    )
                    .await
                {
                    Ok(
                        RefreshItemOutcome::Confirmed(count)
                        | RefreshItemOutcome::NeedsReview(count),
                    ) => candidate_count = candidate_count.saturating_add(count),
                    Err(error) if recoverable_scraper_attempt(&error) => {}
                    Err(error) => tracing::warn!(
                        item_id,
                        scraper_id = %scraper.scraper_id,
                        %error,
                        "supplemental scraper attempt failed"
                    ),
                }
            }
        }
        Ok(candidate_count)
    }

    async fn refresh_item(
        &self,
        item_id: &str,
        item: &crate::storage::StoredMediaMetadata,
        mode: MetadataRefreshMode,
        provider: &ScraperProvider,
        options: RefreshItemOptions,
    ) -> Result<RefreshItemOutcome, MetadataReidentifyError> {
        let page = if matches!(mode, MetadataRefreshMode::FillMissing) {
            self.candidates
                .search_and_store_for_automatic_match_with_plan(
                    item_id,
                    &item.title,
                    item.production_year
                        .and_then(|year| i32::try_from(year).ok()),
                    provider,
                    options
                        .request_plan
                        .unwrap_or_else(MetadataRequestPlan::full),
                )
                .await
                .map_err(MetadataReidentifyError::Candidate)?
        } else {
            self.candidates
                .search_and_store_for_automatic_match_fresh(
                    item_id,
                    &item.title,
                    item.production_year
                        .and_then(|year| i32::try_from(year).ok()),
                    provider,
                )
                .await
                .map_err(MetadataReidentifyError::Candidate)?
        };
        if matches!(mode, MetadataRefreshMode::Reidentify) {
            return Ok(RefreshItemOutcome::Confirmed(
                i64::try_from(page.items.len()).unwrap_or(i64::MAX),
            ));
        }
        let Some(selection) = self.selection.as_ref() else {
            return Err(MetadataReidentifyError::SelectionUnavailable);
        };
        let Some(candidate) = best_pending_candidate(&page) else {
            return Err(MetadataReidentifyError::LowConfidence);
        };
        let needs_review = best_automatic_candidate(&page).is_none();
        if options.supplemental && needs_review {
            return Err(MetadataReidentifyError::LowConfidence);
        }
        let selection_mode = match mode {
            MetadataRefreshMode::FillMissing => MetadataSelectionMode::FillMissing,
            MetadataRefreshMode::FullRefresh => MetadataSelectionMode::RefreshUnlocked,
            MetadataRefreshMode::Reidentify => return Ok(RefreshItemOutcome::Confirmed(0)),
        };
        if needs_review {
            selection
                .select_for_review_with_scraper_and_policy(
                    item_id,
                    &candidate.id,
                    selection_mode,
                    options.scraper_id.as_deref(),
                    options.supplemental,
                    options.image_policy,
                )
                .await
        } else {
            selection
                .select_with_scraper_and_policy(
                    item_id,
                    &candidate.id,
                    selection_mode,
                    options.scraper_id.as_deref(),
                    options.supplemental,
                    options.image_policy,
                )
                .await
        }
        .map_err(MetadataReidentifyError::Selection)?;
        if !self
            .actor_enrichment
            .enqueue(item_id, &candidate.id, selection.clone(), provider.clone())
            .await
        {
            tracing::warn!(
                item_id,
                candidate_id = %candidate.id,
                "actor metadata enrichment queue is full"
            );
        }
        let candidate_count = i64::try_from(page.items.len()).unwrap_or(i64::MAX);
        Ok(if needs_review {
            RefreshItemOutcome::NeedsReview(candidate_count)
        } else {
            RefreshItemOutcome::Confirmed(candidate_count)
        })
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
            job_scope: job.job_scope,
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
        if !matches!(
            job.status.as_str(),
            "FAILED" | "CANCELLED" | "COMPLETED_WITH_ISSUES" | "DEFERRED"
        ) {
            return Err(MetadataReidentifyError::JobNotRetryable);
        }
        let retried = if job.job_scope == "LIBRARY" {
            let _creation_guard = self.library_job_creation.lock().await;
            if let Some(active_job_id) = self
                .database
                .active_library_metadata_reidentify_job_id()
                .await?
            {
                return Err(MetadataReidentifyError::LibraryJobAlreadyActive(
                    active_job_id,
                ));
            }
            self.database.retry_metadata_reidentify_job(job_id).await?
        } else {
            self.database.retry_metadata_reidentify_job(job_id).await?
        };
        if !retried {
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
    pub job_scope: String,
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
    LibraryJobAlreadyActive(String),
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
            Self::LibraryJobAlreadyActive(_) => "LIBRARY_JOB_ALREADY_ACTIVE",
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
            Self::LibraryJobAlreadyActive(job_id) => {
                write!(
                    formatter,
                    "a full-library metadata job is already active: {job_id}"
                )
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

fn best_automatic_candidate(page: &MetadataCandidatePage) -> Option<&MetadataCandidateView> {
    let candidate = best_pending_candidate(page)?;
    if candidate.score < AUTO_MATCH_MIN_SCORE {
        return None;
    }
    let second_score = page
        .items
        .iter()
        .filter(|item| item.status == "PENDING" && item.id != candidate.id)
        .filter(|item| {
            item.provider != candidate.provider || item.provider_id != candidate.provider_id
        })
        .map(|item| item.score)
        .max_by(f64::total_cmp);
    if second_score.is_some_and(|score| candidate.score - score < AUTO_MATCH_MIN_MARGIN) {
        return None;
    }
    Some(candidate)
}

fn best_pending_candidate(page: &MetadataCandidatePage) -> Option<&MetadataCandidateView> {
    page.items
        .iter()
        .filter(|candidate| candidate.status == "PENDING")
        .max_by(|left, right| left.score.total_cmp(&right.score))
}

fn recoverable_scraper_attempt(error: &MetadataReidentifyError) -> bool {
    matches!(
        error,
        MetadataReidentifyError::LowConfidence
            | MetadataReidentifyError::Scraper(_)
            | MetadataReidentifyError::Candidate(MetadataCandidateError::Scraper(_))
    )
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
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

fn metadata_worker_concurrency(recommended: usize) -> usize {
    recommended.clamp(1, METADATA_GLOBAL_WORKER_LIMIT)
}

fn metadata_worker_default_concurrency(backend: DatabaseBackend) -> usize {
    match backend {
        DatabaseBackend::Sqlite => SQLITE_METADATA_DEFAULT_CONCURRENCY,
        DatabaseBackend::Postgres => POSTGRES_METADATA_DEFAULT_CONCURRENCY,
    }
}

fn metadata_request_plan_is_complete(plan: MetadataRequestPlan) -> bool {
    !plan.needs_metadata
        && !plan.needs_images
        && !plan.needs_credits
        && !plan.needs_external_ids
        && !plan.needs_trailers
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::Value;

    use super::{
        AUTO_MATCH_MIN_MARGIN, AUTO_MATCH_MIN_SCORE, METADATA_GLOBAL_WORKER_LIMIT,
        MetadataCandidatePage, MetadataCandidateView, MetadataRequestPlan,
        best_automatic_candidate, metadata_global_permits, metadata_request_plan_is_complete,
        metadata_worker_concurrency, metadata_worker_default_concurrency,
    };
    use crate::config::DatabaseBackend;

    fn candidate(id: &str, score: f64) -> MetadataCandidateView {
        MetadataCandidateView {
            id: id.to_owned(),
            item_id: "item".to_owned(),
            item_title: "title".to_owned(),
            provider: "tmdb".to_owned(),
            provider_id: id.to_owned(),
            candidate: Value::Null,
            score,
            status: "PENDING".to_owned(),
            expires_at: None,
            field_diffs: Vec::new(),
        }
    }

    #[test]
    fn automatic_matching_chooses_the_highest_qualifying_score() {
        let page = MetadataCandidatePage {
            items: vec![
                candidate("lower", AUTO_MATCH_MIN_SCORE),
                candidate("higher", 90.0),
            ],
            total: 2,
            offset: 0,
            limit: 50,
        };

        assert_eq!(
            best_automatic_candidate(&page).map(|item| item.id.as_str()),
            Some("higher")
        );
    }

    #[test]
    fn automatic_matching_sends_close_candidates_to_review() {
        let page = MetadataCandidatePage {
            items: vec![candidate("first", 90.0), candidate("second", 90.0)],
            total: 2,
            offset: 0,
            limit: 50,
        };

        assert!(best_automatic_candidate(&page).is_none());
    }

    #[test]
    fn automatic_matching_ignores_duplicate_candidates_for_margin() {
        let mut duplicate = candidate("duplicate-1", 95.0);
        duplicate.provider_id = "same-provider-id".to_owned();
        let mut repeated = candidate("duplicate-2", 95.0);
        repeated.provider_id = "same-provider-id".to_owned();
        let page = MetadataCandidatePage {
            items: vec![duplicate, repeated],
            total: 2,
            offset: 0,
            limit: 50,
        };

        assert!(best_automatic_candidate(&page).is_some());
    }

    #[test]
    fn automatic_matching_sends_candidates_with_a_small_margin_to_review() {
        let page = MetadataCandidatePage {
            items: vec![
                candidate("first", 90.0),
                candidate("second", 90.0 - AUTO_MATCH_MIN_MARGIN + 0.1),
            ],
            total: 2,
            offset: 0,
            limit: 50,
        };

        assert!(best_automatic_candidate(&page).is_none());
    }

    #[test]
    fn automatic_matching_rejects_scores_below_threshold() {
        let page = MetadataCandidatePage {
            items: vec![candidate("low", AUTO_MATCH_MIN_SCORE - 0.1)],
            total: 1,
            offset: 0,
            limit: 50,
        };

        assert!(best_automatic_candidate(&page).is_none());
    }

    #[test]
    fn metadata_worker_concurrency_is_bounded_without_overthrottling() {
        assert_eq!(metadata_worker_concurrency(16), 16);
        assert_eq!(metadata_worker_concurrency(2), 2);
        assert_eq!(metadata_worker_concurrency(0), 1);
    }

    #[test]
    fn metadata_worker_defaults_match_database_write_capacity() {
        assert_eq!(
            metadata_worker_default_concurrency(DatabaseBackend::Sqlite),
            4
        );
        assert_eq!(
            metadata_worker_default_concurrency(DatabaseBackend::Postgres),
            8
        );
    }

    #[test]
    fn metadata_worker_permits_are_process_global_and_hard_capped() {
        let first = metadata_global_permits();
        let second = metadata_global_permits();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.available_permits(), METADATA_GLOBAL_WORKER_LIMIT);
    }

    #[test]
    fn complete_metadata_request_plans_stop_supplemental_work() {
        assert!(metadata_request_plan_is_complete(
            MetadataRequestPlan::default()
        ));
        assert!(!metadata_request_plan_is_complete(MetadataRequestPlan {
            needs_images: true,
            ..MetadataRequestPlan::default()
        }));
    }
}
