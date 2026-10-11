use super::*;

pub(in crate::application::scanner) const LOCAL_METADATA_IDLE_FALLBACK: Duration =
    Duration::from_secs(1);

pub(in crate::application::scanner) const LOCAL_METADATA_COMPLETION_FALLBACK: Duration =
    Duration::from_secs(5);

pub(in crate::application::scanner) const LOCAL_METADATA_JOB_REFRESH_FALLBACK: Duration =
    Duration::from_secs(5);

pub(in crate::application::scanner) const LOCAL_METADATA_BATCH_SIZE: usize = 16;

pub(in crate::application::scanner) const SCAN_LOCAL_METADATA_OUTBOX_IDLE_FALLBACK: Duration =
    Duration::from_millis(250);

pub(in crate::application::scanner) const SCAN_LOCAL_METADATA_RETRY_DELAY_SECONDS: i64 = 30;

pub(in crate::application::scanner) const MAX_SCAN_LOCAL_METADATA_SOURCE_IDS: usize = 256;

pub(in crate::application::scanner) const SCAN_LOCAL_METADATA_BACKFILL_PAGE_SIZE: usize = 16;

pub(in crate::application::scanner) const SCAN_LOCAL_METADATA_COMPLETENESS_CHECK_BATCH_SIZE: usize =
    512;

pub(in crate::application::scanner) const MAX_SCAN_LOCAL_METADATA_NFO_BATCHES_IN_FLIGHT: usize = 4;

pub(in crate::application::scanner) struct LocalMetadataWorkerHandle {
    pub(in crate::application::scanner) stop: watch::Sender<bool>,
    pub(in crate::application::scanner) task: JoinHandle<()>,
    pub(in crate::application::scanner) job_id: String,
    pub(in crate::application::scanner) notifications: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    #[cfg(test)]
    pub(in crate::application::scanner) idle_page_receiver: tokio::sync::mpsc::UnboundedReceiver<()>,
}

pub(in crate::application::scanner) fn notify_local_metadata_images_waiter(
    notifications: &Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    scan_job_id: &str,
) {
    if let Ok(registry) = notifications.lock()
        && let Some(notify) = registry.get(scan_job_id)
    {
        notify.notify_one();
    }
}

pub(in crate::application::scanner) struct LocalMetadataCompletionContext<'a> {
    pub(in crate::application::scanner) metadata_selection: Option<&'a MetadataSelectionService>,
    pub(in crate::application::scanner) metadata_reidentify: Option<&'a MetadataReidentifyService>,
    pub(in crate::application::scanner) home: Option<&'a HomeService>,
    pub(in crate::application::scanner) user_events: &'a UserEventHub,
}

pub(in crate::application::scanner) async fn process_scan_local_metadata_batch(
    database: &Database,
    enricher: &MetadataEnricher,
    home: Option<&HomeService>,
    user_events: &UserEventHub,
    batch: StoredScanLocalMetadataBatch,
) -> Option<(
    String,
    String,
    Vec<StoredScanLocalMetadataSource>,
    Vec<String>,
)> {
    let batch_id = batch.id.clone();
    let scan_job_id = batch.job_id.clone();
    let source_ids = match serde_json::from_str::<Vec<String>>(&batch.source_refs_json) {
        Ok(source_ids)
            if source_ids.len() == usize::try_from(batch.source_count).unwrap_or(usize::MAX)
                && !source_ids.is_empty()
                && source_ids.len() <= MAX_SCAN_LOCAL_METADATA_SOURCE_IDS
                && source_ids
                    .iter()
                    .all(|source_id| !source_id.trim().is_empty())
                && source_ids.iter().collect::<HashSet<_>>().len() == source_ids.len() =>
        {
            source_ids
        }
        _ => {
            fail_scan_local_metadata_batch(database, &batch_id, "invalid source reference batch")
                .await;
            return None;
        }
    };
    let non_retryable_item_ids =
        match serde_json::from_str::<Vec<String>>(&batch.non_retryable_item_ids_json) {
            Ok(item_ids)
                if item_ids.len() <= MAX_SCAN_LOCAL_METADATA_SOURCE_IDS
                    && item_ids.iter().all(|item_id| !item_id.trim().is_empty())
                    && item_ids.iter().collect::<HashSet<_>>().len() == item_ids.len() =>
            {
                item_ids
            }
            _ => {
                fail_scan_local_metadata_batch(database, &batch_id, "invalid NFO exclusion list")
                    .await;
                return None;
            }
        };

    let result = if batch.images_completed_at.is_some() {
        database
            .list_scan_local_metadata_sources(&source_ids)
            .await
            .map_err(|error| error.to_string())
    } else {
        match enricher
            .index_scan_local_metadata_batch_images(&source_ids)
            .await
        {
            Ok(batch) if !batch.report.failed_item_ids.is_empty() => Err(format!(
                "{} local image item(s) failed",
                batch.report.failed_item_ids.len()
            )),
            Ok(batch) => match database
                .mark_scan_local_metadata_images_complete(&batch_id)
                .await
            {
                Ok(true) => Ok(batch.sources),
                Ok(false) => Err("local metadata batch stopped before image completion".to_owned()),
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error.to_string()),
        }
    };
    if let Some(home) = home {
        home.invalidate();
    }
    user_events.publish_home_coalesced().await;

    match result {
        Ok(source_snapshot) => Some((
            batch_id,
            scan_job_id,
            source_snapshot,
            non_retryable_item_ids,
        )),
        Err(error) => {
            tracing::warn!(batch_id, %error, "local image batch failed and will be retried");
            fail_scan_local_metadata_batch(database, &batch_id, &error).await;
            None
        }
    }
}

pub(in crate::application::scanner) async fn finish_scan_local_metadata_batch(
    database: &Database,
    enricher: &MetadataEnricher,
    context: LocalMetadataCompletionContext<'_>,
    batch_id: &str,
    scan_job_id: &str,
    source_snapshot: Vec<StoredScanLocalMetadataSource>,
    existing_non_retryable_item_ids: &[String],
) {
    let mut non_retryable_item_ids = existing_non_retryable_item_ids.to_vec();
    let result = match enricher
        .enrich_scan_local_metadata_batch_nfo(source_snapshot, &non_retryable_item_ids)
        .await
    {
        Ok(ScanLocalMetadataNfoBatch {
            report,
            source_identities,
        }) => {
            for item_id in &report.non_retryable_failed_item_ids {
                if !non_retryable_item_ids.contains(item_id) {
                    non_retryable_item_ids.push(item_id.clone());
                }
            }
            let retryable_failed_item_count = report.retryable_failed_item_count();
            if retryable_failed_item_count > 0 {
                Err(format!(
                    "{retryable_failed_item_count} local NFO item(s) failed"
                ))
            } else {
                let issue = (!non_retryable_item_ids.is_empty()).then(|| {
                    format!(
                        "{} non-retryable local NFO item(s) failed",
                        non_retryable_item_ids.len()
                    )
                });
                complete_local_metadata_completeness(
                    database,
                    context.metadata_selection,
                    context.metadata_reidentify,
                    LocalMetadataCompletenessTrigger::ScanJob(scan_job_id),
                    &source_identities,
                    &non_retryable_item_ids,
                    context.user_events,
                )
                .await
                .map(|()| issue)
            }
        }
        Err(error) => Err(error.to_string()),
    };
    if let Some(home) = context.home {
        home.invalidate();
    }
    context.user_events.publish_home_coalesced().await;

    match result {
        Ok(issue) => match database
            .complete_scan_local_metadata_batch_with_outcome(
                batch_id,
                issue.as_deref(),
                &non_retryable_item_ids,
            )
            .await
        {
            Ok(true) => {}
            Ok(false) => tracing::debug!(batch_id, "local metadata batch was already terminal"),
            Err(error) => {
                tracing::warn!(batch_id, %error, "local metadata batch completion could not be saved");
                fail_scan_local_metadata_batch_with_non_retryable_item_ids(
                    database,
                    batch_id,
                    "local metadata completion failed",
                    &non_retryable_item_ids,
                )
                .await;
            }
        },
        Err(error) => {
            tracing::warn!(batch_id, %error, "local NFO batch failed and will be retried");
            fail_scan_local_metadata_batch_with_non_retryable_item_ids(
                database,
                batch_id,
                &error,
                &non_retryable_item_ids,
            )
            .await;
        }
    }
}

async fn fail_scan_local_metadata_batch(database: &Database, batch_id: &str, error: &str) {
    let retry_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
        .checked_add(SCAN_LOCAL_METADATA_RETRY_DELAY_SECONDS)
        .unwrap_or(i64::MAX);
    if let Err(storage_error) = database
        .fail_scan_local_metadata_batch(batch_id, error, Some(retry_at))
        .await
    {
        tracing::warn!(batch_id, %storage_error, "local metadata batch retry could not be saved");
    }
}

async fn fail_scan_local_metadata_batch_with_non_retryable_item_ids(
    database: &Database,
    batch_id: &str,
    error: &str,
    non_retryable_item_ids: &[String],
) {
    let retry_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
        .checked_add(SCAN_LOCAL_METADATA_RETRY_DELAY_SECONDS)
        .unwrap_or(i64::MAX);
    if let Err(storage_error) = database
        .fail_scan_local_metadata_batch_with_non_retryable_item_ids(
            batch_id,
            error,
            non_retryable_item_ids,
            Some(retry_at),
        )
        .await
    {
        tracing::warn!(batch_id, %storage_error, "local metadata batch retry could not be saved");
    }
}

pub(in crate::application::scanner) async fn process_scan_local_metadata_backfill_images(
    enricher: &MetadataEnricher,
    home: Option<&HomeService>,
    user_events: &UserEventHub,
    page: &StoredScanLocalMetadataBackfillPage,
) -> Result<Vec<StoredScanLocalMetadataSource>, String> {
    let result = enricher
        .index_scan_local_metadata_batch_images(&page.entry_ids)
        .await;
    if let Some(home) = home {
        home.invalidate();
    }
    user_events.publish_home_coalesced().await;
    let batch = result.map_err(|error| error.to_string())?;
    if !batch.report.failed_item_ids.is_empty() {
        return Err(format!(
            "{} local image item(s) failed",
            batch.report.failed_item_ids.len()
        ));
    }
    Ok(batch.sources)
}

pub(in crate::application::scanner) async fn finish_scan_local_metadata_backfill_page(
    database: &Database,
    enricher: &MetadataEnricher,
    context: LocalMetadataCompletionContext<'_>,
    page: StoredScanLocalMetadataBackfillPage,
    source_snapshot: Vec<StoredScanLocalMetadataSource>,
) {
    let mut non_retryable_item_ids = page.non_retryable_item_ids.clone();
    let result = match enricher
        .enrich_scan_local_metadata_batch_nfo(source_snapshot, &page.non_retryable_item_ids)
        .await
    {
        Ok(ScanLocalMetadataNfoBatch {
            report,
            source_identities,
        }) => {
            for item_id in &report.non_retryable_failed_item_ids {
                if !non_retryable_item_ids.contains(item_id) {
                    non_retryable_item_ids.push(item_id.clone());
                }
            }
            let retryable_failed_item_count = report.retryable_failed_item_count();
            if retryable_failed_item_count > 0 {
                Err(format!(
                    "{retryable_failed_item_count} local NFO item(s) failed"
                ))
            } else {
                let issue = (!non_retryable_item_ids.is_empty()).then(|| {
                    format!(
                        "{} non-retryable local NFO item(s) failed",
                        non_retryable_item_ids.len()
                    )
                });
                complete_local_metadata_completeness(
                    database,
                    context.metadata_selection,
                    context.metadata_reidentify,
                    LocalMetadataCompletenessTrigger::LibrarySetting,
                    &source_identities,
                    &non_retryable_item_ids,
                    context.user_events,
                )
                .await
                .map(|()| issue)
            }
        }
        Err(error) => Err(error.to_string()),
    };
    if let Some(home) = context.home {
        home.invalidate();
    }
    context.user_events.publish_home_coalesced().await;

    match result {
        Ok(issue) => match database
            .complete_scan_local_metadata_backfill_page_with_optional_issue(&page, issue.as_deref())
            .await
        {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                library_root_id = %page.library_root_id,
                "local metadata backfill page was already terminal or changed"
            ),
            Err(error) => {
                tracing::warn!(
                    library_root_id = %page.library_root_id,
                    %error,
                    "local metadata backfill page completion could not be saved"
                );
                fail_scan_local_metadata_backfill_page(
                    database,
                    &page,
                    "local metadata backfill completion failed",
                    &non_retryable_item_ids,
                )
                .await;
            }
        },
        Err(error) => {
            tracing::warn!(
                library_root_id = %page.library_root_id,
                %error,
                "local metadata backfill NFO failed and will be retried"
            );
            fail_scan_local_metadata_backfill_page(
                database,
                &page,
                &error,
                &non_retryable_item_ids,
            )
            .await;
        }
    }
}

pub(in crate::application::scanner) async fn fail_scan_local_metadata_backfill_page(
    database: &Database,
    page: &StoredScanLocalMetadataBackfillPage,
    error: &str,
    non_retryable_item_ids: &[String],
) {
    let retry_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
        .checked_add(SCAN_LOCAL_METADATA_RETRY_DELAY_SECONDS)
        .unwrap_or(i64::MAX);
    if let Err(storage_error) = database
        .fail_scan_local_metadata_backfill_page_with_exclusions(
            page,
            error,
            non_retryable_item_ids,
            Some(retry_at),
        )
        .await
    {
        tracing::warn!(
            library_root_id = %page.library_root_id,
            %storage_error,
            "local metadata backfill retry could not be saved"
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::application::scanner) enum LocalMetadataCompletenessTrigger<'a> {
    ScanJob(&'a str),
    ScanJobPolicy(MetadataAutoMatchPolicy),
    LibrarySetting,
}

pub(in crate::application::scanner) fn metadata_auto_match_policy_for_scan_job(
    job: Option<&StoredScanJob>,
) -> MetadataAutoMatchPolicy {
    metadata_auto_match_policy_for_scan_job_snapshot(
        job.map(|job| job.job_type.as_str()),
        job.is_some_and(|job| job.auto_metadata_match),
    )
}

pub(in crate::application::scanner) fn metadata_auto_match_policy_for_scan_job_snapshot(
    job_type: Option<&str>,
    auto_metadata_match: bool,
) -> MetadataAutoMatchPolicy {
    match job_type {
        Some("INCREMENTAL_SCAN") => {
            if auto_metadata_match {
                MetadataAutoMatchPolicy::Enabled
            } else {
                MetadataAutoMatchPolicy::Disabled
            }
        }
        Some(_) => MetadataAutoMatchPolicy::UseLibrarySetting,
        None => MetadataAutoMatchPolicy::Disabled,
    }
}

pub(in crate::application::scanner) async fn resolve_local_metadata_auto_match_policy(
    database: &Database,
    trigger: LocalMetadataCompletenessTrigger<'_>,
) -> Result<MetadataAutoMatchPolicy, String> {
    match trigger {
        LocalMetadataCompletenessTrigger::LibrarySetting => {
            Ok(MetadataAutoMatchPolicy::UseLibrarySetting)
        }
        LocalMetadataCompletenessTrigger::ScanJobPolicy(policy) => Ok(policy),
        LocalMetadataCompletenessTrigger::ScanJob(scan_job_id) => {
            let job = database
                .find_scan_job(scan_job_id)
                .await
                .map_err(|error| error.to_string())?;
            Ok(metadata_auto_match_policy_for_scan_job(job.as_ref()))
        }
    }
}

type PendingLocalMetadataCompletenessCheck = (String, String, Vec<u8>, bool, bool);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::application::scanner) enum ScraperAvailabilityPreflight {
    Available,
    Unavailable,
    Failed,
}

impl ScraperAvailabilityPreflight {
    pub(in crate::application::scanner) fn from_check_result(
        result: Result<Option<bool>, ()>,
    ) -> Self {
        match result {
            Ok(Some(true)) => Self::Available,
            Ok(Some(false)) | Ok(None) => Self::Unavailable,
            Err(()) => Self::Failed,
        }
    }

    pub(in crate::application::scanner) const fn allows_fill_missing(self) -> bool {
        matches!(self, Self::Available | Self::Failed)
    }
}

pub(in crate::application::scanner) async fn complete_local_metadata_completeness(
    database: &Database,
    selection: Option<&MetadataSelectionService>,
    metadata_reidentify: Option<&MetadataReidentifyService>,
    trigger: LocalMetadataCompletenessTrigger<'_>,
    source_identities: &[(String, String)],
    excluded_item_ids: &[String],
    user_events: &UserEventHub,
) -> Result<(), String> {
    if selection.is_none() {
        return Ok(());
    }
    let source_identities = source_identities
        .iter()
        .filter(|(item_id, _)| !excluded_item_ids.iter().any(|excluded| excluded == item_id))
        .cloned()
        .collect::<Vec<_>>();
    let mut item_ids = database
        .list_current_scan_local_metadata_item_ids(&source_identities)
        .await
        .map_err(|error| error.to_string())?;
    item_ids.sort_unstable();
    item_ids.dedup();
    complete_local_metadata_completeness_for_item_ids(
        database,
        selection,
        metadata_reidentify,
        trigger,
        &item_ids,
        user_events,
    )
    .await
}

pub(in crate::application::scanner) async fn complete_local_metadata_completeness_for_item_ids(
    database: &Database,
    selection: Option<&MetadataSelectionService>,
    metadata_reidentify: Option<&MetadataReidentifyService>,
    trigger: LocalMetadataCompletenessTrigger<'_>,
    item_ids: &[String],
    user_events: &UserEventHub,
) -> Result<(), String> {
    let Some(selection) = selection else {
        return Ok(());
    };
    let auto_match_policy = resolve_local_metadata_auto_match_policy(database, trigger).await?;
    let metadata_by_item = database
        .list_active_media_item_metadata_with_libraries(item_ids)
        .await
        .map_err(|error| error.to_string())?;
    let plan_inputs = item_ids
        .iter()
        .filter_map(|item_id| {
            metadata_by_item
                .get(item_id)
                .map(|(_, current)| (item_id.as_str(), current))
        })
        .collect::<Vec<_>>();
    let plans_by_item = selection
        .local_metadata_completeness_plans(&plan_inputs)
        .await
        .map_err(|error| error.to_string())?;
    let scraper_check_item_ids =
        if auto_match_policy.allows_scraper_lookup() && metadata_reidentify.is_some() {
            item_ids
                .iter()
                .filter(|item_id| {
                    plans_by_item
                        .get(*item_id)
                        .and_then(Option::as_ref)
                        .is_some_and(|plan| plan.has_requestable_capability)
                })
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
    let (scraper_availability_by_item, scraper_availability_error) =
        if let Some(metadata_reidentify) = metadata_reidentify
            && !scraper_check_item_ids.is_empty()
        {
            match metadata_reidentify
                .has_selected_scrapers_for_items(&scraper_check_item_ids)
                .await
            {
                Ok(available) => (available, None),
                Err(error) => (HashMap::new(), Some(error.to_string())),
            }
        } else {
            (HashMap::new(), None)
        };
    let mut checks_by_library: BTreeMap<String, Vec<PendingLocalMetadataCompletenessCheck>> =
        BTreeMap::new();
    for item_id in item_ids {
        let Some((library_id, _current)) = metadata_by_item.get(item_id) else {
            continue;
        };
        let Some(plan) = plans_by_item.get(item_id).and_then(Option::as_ref) else {
            continue;
        };
        let eligible_for_fill_missing = if plan.has_requestable_capability
            && auto_match_policy.allows_scraper_lookup()
        {
            if metadata_reidentify.is_some() {
                let availability = scraper_availability_by_item.get(item_id);
                let preflight_result = if let Some(error) = scraper_availability_error.as_deref() {
                    tracing::warn!(
                        item_id = %item_id,
                        error,
                        "local metadata can confirm missing capabilities but scraper availability could not be checked"
                    );
                    Err(())
                } else {
                    match availability {
                        Some(Ok(available)) => Ok(Some(*available)),
                        None => Ok(None),
                        Some(Err(error)) => {
                            tracing::warn!(
                                item_id = %item_id,
                                %error,
                                "local metadata can confirm missing capabilities but scraper availability could not be checked"
                            );
                            Err(())
                        }
                    }
                };
                let preflight = ScraperAvailabilityPreflight::from_check_result(preflight_result);
                // The metadata worker can persist SCRAPER_UNAVAILABLE and apply its bounded retry.
                preflight.allows_fill_missing()
            } else {
                false
            }
        } else {
            false
        };
        let checks = checks_by_library.entry(library_id.clone()).or_default();
        checks.extend(plan.capabilities.iter().map(|(capability, is_missing)| {
            (
                item_id.clone(),
                capability.clone(),
                plan.input_fingerprint.clone(),
                *is_missing,
                eligible_for_fill_missing,
            )
        }));
    }

    let checked_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        });
    let mut scheduled_job_ids = Vec::new();
    for (library_id, checks) in checks_by_library {
        for check_batch in checks.chunks(SCAN_LOCAL_METADATA_COMPLETENESS_CHECK_BATCH_SIZE) {
            let results = check_batch
                .iter()
                .map(|(item_id, capability, input_fingerprint, is_missing, _)| {
                    NewItemMetadataCompletenessResult {
                        item_id,
                        capability,
                        input_fingerprint,
                        is_missing: *is_missing,
                        checked_at,
                    }
                })
                .collect::<Vec<_>>();
            let mut fill_missing_candidate_item_ids = check_batch
                .iter()
                .filter(|(_, _, _, is_missing, eligible)| *is_missing && *eligible)
                .map(|(item_id, _, _, _, _)| item_id.clone())
                .collect::<Vec<_>>();
            fill_missing_candidate_item_ids.sort_unstable();
            fill_missing_candidate_item_ids.dedup();
            let completion = database
                .complete_local_metadata_completeness_batch_with_policy(
                    &library_id,
                    &results,
                    &fill_missing_candidate_item_ids,
                    auto_match_policy,
                )
                .await
                .map_err(|error| error.to_string())?;
            scheduled_job_ids.extend(
                completion
                    .scheduled_job_ids
                    .into_iter()
                    .map(|job_id| (library_id.clone(), job_id)),
            );
        }
    }
    if let Some(metadata_reidentify) = metadata_reidentify.cloned()
        && !scheduled_job_ids.is_empty()
    {
        let mut completions = Vec::with_capacity(scheduled_job_ids.len());
        for (library_id, job_id) in scheduled_job_ids {
            match metadata_reidentify
                .enqueue_fill_missing_job_id(&job_id)
                .await
            {
                Ok(Some(completion)) => completions.push(completion),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        library_id,
                        job_id,
                        %error,
                        "local metadata completeness job could not be dispatched"
                    );
                }
            }
        }
        if !completions.is_empty() {
            let user_events = user_events.clone();
            tokio::spawn(async move {
                for completion in completions {
                    let _ = completion.await;
                    user_events.publish_home_coalesced().await;
                }
            });
        }
    }
    Ok(())
}

impl ScanJobService {
    pub(in crate::application::scanner) async fn run_local_metadata_refresh(
        &self,
        entry_ids: Vec<String>,
    ) {
        // One refresh at a time: it is cheap per entry, but a burst of notifications must not
        // compete with scans for the database.
        static REFRESH_LOCK: OnceLock<Semaphore> = OnceLock::new();
        let Ok(_permit) = REFRESH_LOCK
            .get_or_init(|| Semaphore::new(1))
            .acquire()
            .await
        else {
            return;
        };
        let enricher = MetadataEnricher::new(self.database.clone())
            .with_resource_metrics(self.resources.clone());
        let enricher = match self.local_nfo.clone() {
            Some(local_nfo) => enricher.with_nfo_store(local_nfo),
            None => enricher,
        };
        let mut failed_items = 0_usize;
        for chunk in entry_ids.chunks(64) {
            let source_snapshot = match enricher.index_scan_local_metadata_batch_images(chunk).await
            {
                Ok(batch) => batch.sources,
                Err(error) => {
                    tracing::warn!(%error, "local metadata refresh could not index images");
                    failed_items += 1;
                    continue;
                }
            };
            match enricher
                .enrich_scan_local_metadata_batch_nfo(source_snapshot, &[])
                .await
            {
                Ok(ScanLocalMetadataNfoBatch {
                    report,
                    source_identities,
                }) => {
                    failed_items += report.failed_item_ids.len();
                    if let Err(error) = complete_local_metadata_completeness(
                        &self.database,
                        self.metadata_selection.as_ref(),
                        self.metadata_reidentify.as_ref(),
                        LocalMetadataCompletenessTrigger::LibrarySetting,
                        &source_identities,
                        &report.non_retryable_failed_item_ids,
                        &self.user_events,
                    )
                    .await
                    {
                        tracing::warn!(%error, "local metadata refresh could not update completeness");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "local metadata refresh could not read NFOs");
                    failed_items += 1;
                }
            }
        }
        tracing::info!(
            entries = entry_ids.len(),
            failed_items,
            "local metadata refresh finished"
        );
        if let Some(home) = &self.home {
            home.invalidate();
        }
        self.user_events.publish_home_coalesced().await;
    }

    pub(in crate::application::scanner) fn notify_local_metadata_outbox_worker(&self) {
        self.local_metadata_outbox_notify.notify_one();
    }

    pub(in crate::application::scanner) fn start_local_metadata_worker(
        &self,
        scan_job_id: &str,
    ) -> LocalMetadataWorkerHandle {
        let (stop, mut stop_receiver) = watch::channel(false);
        #[cfg(test)]
        let (idle_page_sender, idle_page_receiver) = tokio::sync::mpsc::unbounded_channel();
        let database = self.database.clone();
        let people = self.people.clone();
        let local_nfo = self.local_nfo.clone();
        let metadata_selection = self.metadata_selection.clone();
        let metadata_reidentify = self.metadata_reidentify.clone();
        let home = self.home.clone();
        let user_events = self.user_events.clone();
        let resources = self.resources.clone();
        let target_root_validator = self.clone();
        let scan_job_id = scan_job_id.to_owned();
        let notifications = Arc::clone(&self.metadata_notifications);
        let notify = Arc::new(Notify::new());
        if let Ok(mut registry) = notifications.lock() {
            registry.insert(scan_job_id.clone(), Arc::clone(&notify));
        }
        let worker_job_id = scan_job_id.clone();
        let task = tokio::spawn(async move {
            let enricher = MetadataEnricher::new(database.clone()).with_resource_metrics(resources);
            let enricher = match people {
                Some(people) => enricher.with_people(people),
                None => enricher,
            };
            let enricher = match local_nfo {
                Some(local_nfo) => enricher.with_nfo_store(local_nfo),
                None => enricher,
            };

            let mut loaded_lifecycle = false;
            loop {
                if *stop_receiver.borrow() {
                    return;
                }
                let page = match database
                    .load_scan_job_metadata_page(
                        &worker_job_id,
                        i64::try_from(LOCAL_METADATA_BATCH_SIZE).unwrap_or(i64::MAX),
                    )
                    .await
                {
                    Ok(page) => {
                        loaded_lifecycle = true;
                        page
                    }
                    Err(error) => {
                        tracing::warn!(
                            scan_job_id = %worker_job_id,
                            %error,
                            "local metadata worker could not load job lifecycle and target page"
                        );
                        let fallback = if loaded_lifecycle {
                            LOCAL_METADATA_JOB_REFRESH_FALLBACK
                        } else {
                            LOCAL_METADATA_IDLE_FALLBACK
                        };
                        tokio::select! {
                            changed = stop_receiver.changed() => {
                                if changed.is_err() || *stop_receiver.borrow() {
                                    return;
                                }
                            }
                            _ = notify.notified() => {}
                            _ = tokio::time::sleep(fallback) => {}
                        }
                        continue;
                    }
                };
                let Some(job_status) = page.job_status.as_deref() else {
                    return;
                };
                if matches!(job_status, "FAILED" | "CANCELLED") {
                    return;
                }
                let is_incremental_scan = page.job_type.as_deref() == Some("INCREMENTAL_SCAN");
                let completeness_policy = metadata_auto_match_policy_for_scan_job_snapshot(
                    page.job_type.as_deref(),
                    page.auto_metadata_match,
                );
                if page.has_pending {
                    if let Err(error) = target_root_validator
                        .verify_manifest_postprocessing_target_roots(&worker_job_id)
                        .await
                    {
                        tracing::warn!(
                            scan_job_id = %worker_job_id,
                            %error,
                            "local metadata worker deferred targets because a manifest root changed"
                        );
                        if let Err(mark_error) = database
                            .mark_pending_scan_job_metadata_targets_failed(
                                &worker_job_id,
                                &error.to_string(),
                            )
                            .await
                        {
                            tracing::warn!(
                                scan_job_id = %worker_job_id,
                                %mark_error,
                                "local metadata worker could not defer root-mismatched targets"
                            );
                        }
                        notify.notify_waiters();
                        continue;
                    }
                    match enricher
                        .enrich_scan_job_metadata_page(&worker_job_id, page)
                        .await
                    {
                        Ok(report) if report.items_processed > 0 => {
                            if is_incremental_scan
                                && !report.locally_enriched_item_ids.is_empty()
                                && let Err(error) =
                                    complete_local_metadata_completeness_for_item_ids(
                                        &database,
                                        metadata_selection.as_ref(),
                                        metadata_reidentify.as_ref(),
                                        LocalMetadataCompletenessTrigger::ScanJobPolicy(
                                            completeness_policy,
                                        ),
                                        &report.locally_enriched_item_ids,
                                        &user_events,
                                    )
                                    .await
                            {
                                tracing::warn!(
                                    scan_job_id = %worker_job_id,
                                    %error,
                                    "incremental local metadata completeness could not be saved"
                                );
                            }
                            if let Some(home) = &home {
                                home.invalidate_scan_batch().await;
                            }
                            notify.notify_waiters();
                        }
                        Ok(_) => {
                            // A pending target with no locally readable source
                            // cannot make progress. Mark it retryable so the
                            // completion waiter cannot spin forever; a later
                            // retry can reset FAILED targets after the source
                            // becomes available.
                            if let Err(error) = database
                                .mark_pending_scan_job_metadata_targets_failed(
                                    &worker_job_id,
                                    "local metadata source unavailable",
                                )
                                .await
                            {
                                tracing::warn!(
                                    scan_job_id = %worker_job_id,
                                    %error,
                                    "local metadata worker could not finish unavailable targets"
                                );
                            }
                            notify.notify_waiters();
                        }
                        Err(error) => {
                            tracing::warn!(
                                scan_job_id = %worker_job_id,
                                %error,
                                "local metadata worker failed; pending targets marked for retry"
                            );
                            if let Err(mark_error) = database
                                .mark_pending_scan_job_metadata_targets_failed(
                                    &worker_job_id,
                                    &error.to_string(),
                                )
                                .await
                            {
                                tracing::warn!(
                                    scan_job_id = %worker_job_id,
                                    %mark_error,
                                    "local metadata worker could not mark failed targets"
                                );
                            }
                            notify.notify_waiters();
                        }
                    }
                    continue;
                }
                #[cfg(test)]
                let _ = idle_page_sender.send(());
                let notified = notify.notified();
                tokio::select! {
                    changed = stop_receiver.changed() => {
                        if changed.is_err() || *stop_receiver.borrow() {
                            return;
                        }
                    }
                    _ = notified => {}
                    _ = tokio::time::sleep(LOCAL_METADATA_JOB_REFRESH_FALLBACK) => {}
                }
            }
        });
        LocalMetadataWorkerHandle {
            stop,
            task,
            job_id: scan_job_id,
            notifications,
            #[cfg(test)]
            idle_page_receiver,
        }
    }

    pub(in crate::application::scanner) fn notify_local_metadata_worker(&self, scan_job_id: &str) {
        if let Ok(registry) = self.metadata_notifications.lock()
            && let Some(notify) = registry.get(scan_job_id)
        {
            notify.notify_one();
        }
    }

    pub(in crate::application::scanner) async fn wait_for_local_metadata(
        &self,
        scan_job_id: &str,
    ) -> Result<(), ScanJobError> {
        let notify = self
            .metadata_notifications
            .lock()
            .ok()
            .and_then(|registry| registry.get(scan_job_id).cloned());
        loop {
            let notified = notify.as_ref().map(|notify| notify.notified());
            tokio::pin!(notified);
            if let Some(notified) = notified.as_mut().as_pin_mut() {
                notified.enable();
            }

            let (has_pending_targets, job_missing_or_cancelled) = self
                .database
                .local_metadata_completion_wait_state(scan_job_id)
                .await?;
            if !has_pending_targets {
                break;
            }
            let cancellation = self.cancellation_flag(scan_job_id);
            if job_missing_or_cancelled {
                cancellation.store(true, Ordering::Release);
                break;
            }
            if cancellation.load(Ordering::Acquire) {
                break;
            }

            if let Some(notified) = notified.as_mut().as_pin_mut() {
                tokio::select! {
                    _ = notified => {}
                    _ = tokio::time::sleep(LOCAL_METADATA_COMPLETION_FALLBACK) => {}
                }
            } else {
                tokio::time::sleep(LOCAL_METADATA_COMPLETION_FALLBACK).await;
            }
        }
        Ok(())
    }

    pub(in crate::application::scanner) async fn wait_for_local_metadata_images(
        &self,
        scan_job_id: &str,
    ) -> Result<(), ScanJobError> {
        let notify =
            self.local_metadata_image_notifications
                .lock()
                .ok()
                .map(|mut notifications| {
                    Arc::clone(
                        notifications
                            .entry(scan_job_id.to_owned())
                            .or_insert_with(|| Arc::new(Notify::new())),
                    )
                });
        while self
            .database
            .has_pending_scan_local_metadata_images(scan_job_id)
            .await?
        {
            let cancellation = self.cancellation_flag(scan_job_id);
            if self
                .cancellation_requested(scan_job_id, false, &cancellation)
                .await?
            {
                break;
            }
            if let Some(notify) = &notify {
                tokio::select! {
                    _ = notify.notified() => {}
                    _ = tokio::time::sleep(LOCAL_METADATA_IDLE_FALLBACK) => {}
                }
            } else {
                tokio::time::sleep(LOCAL_METADATA_IDLE_FALLBACK).await;
            }
        }
        if let Ok(mut notifications) = self.local_metadata_image_notifications.lock() {
            notifications.remove(scan_job_id);
        }
        Ok(())
    }

    pub(in crate::application::scanner) async fn stop_local_metadata_worker(
        worker: &mut Option<LocalMetadataWorkerHandle>,
    ) {
        let Some(worker) = worker.take() else {
            return;
        };
        let _ = worker.stop.send(true);
        let _ = worker.task.await;
        if let Ok(mut notifications) = worker.notifications.lock() {
            notifications.remove(&worker.job_id);
        }
    }

    pub(in crate::application::scanner) async fn finish_cancellation_if_requested(
        &self,
        job_id: &str,
        local_metadata_worker: &mut Option<LocalMetadataWorkerHandle>,
    ) -> Result<bool, ScanJobError> {
        let cancellation = self.cancellation_flag(job_id);
        if !self
            .cancellation_requested(job_id, false, &cancellation)
            .await?
        {
            return Ok(false);
        }
        Self::stop_local_metadata_worker(local_metadata_worker).await;
        self.cancel_running_job(job_id).await?;
        Ok(true)
    }

    pub async fn run_to_completion(
        &self,
        job_id: &str,
        batch_size: usize,
        probe: Option<MediaProbeService>,
    ) -> Result<(), ScanJobError> {
        self.run_to_completion_with_metadata_and_thumbnails(job_id, batch_size, probe, None, None)
            .await
    }

    pub async fn run_to_completion_with_metadata(
        &self,
        job_id: &str,
        batch_size: usize,
        probe: Option<MediaProbeService>,
        metadata: Option<MetadataReidentifyService>,
    ) -> Result<(), ScanJobError> {
        self.run_to_completion_with_metadata_and_thumbnails(
            job_id, batch_size, probe, metadata, None,
        )
        .await
    }

    pub async fn run_to_completion_with_metadata_and_thumbnails(
        &self,
        job_id: &str,
        batch_size: usize,
        probe: Option<MediaProbeService>,
        metadata: Option<MetadataReidentifyService>,
        thumbnails: Option<ThumbnailService>,
    ) -> Result<(), ScanJobError> {
        if batch_size == 0 {
            return Err(ScanJobError::InvalidBatchSize);
        }
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            // Library deletion cascades queued jobs. A stale worker wakeup after that
            // cascade has nothing left to run and should finish quietly.
            return Ok(());
        };
        let Some(_run_guard) = self.track_scan_job_run(job_id, &job.library_id) else {
            self.cancel_running_job(job_id).await?;
            return Ok(());
        };
        let _full_scan_permit = if job.job_type == "RECONCILE_LIBRARY" {
            Some(
                Arc::clone(&self.full_scan_queue)
                    .acquire_owned()
                    .await
                    .map_err(|_| ScanJobError::ScanLockClosed)?,
            )
        } else {
            None
        };
        let result = self
            .run_to_completion_with_metadata_and_thumbnails_inner(
                job_id, batch_size, probe, metadata, thumbnails,
            )
            .await;
        if result.is_err() {
            match self.database.fail_scan_job_postprocessing(job_id).await {
                Ok(true) => {
                    self.record_event(
                        job_id,
                        "ERROR",
                        "POSTPROCESSING_FAILED",
                        "扫描后处理未完成，可在条件恢复后重试",
                        "{}",
                    )
                    .await;
                }
                Ok(false) => {}
                Err(error) => {
                    self.flush_home_after_scan_terminal().await;
                    return Err(error.into());
                }
            }
            self.flush_home_after_scan_terminal().await;
        }
        result
    }

    pub async fn start_local_metadata_outbox_worker(&self) -> Result<(), ScanJobError> {
        if self
            .local_metadata_outbox_worker_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        if let Err(error) = self
            .database
            .requeue_interrupted_scan_local_metadata_batches()
            .await
        {
            self.local_metadata_outbox_worker_started
                .store(false, Ordering::Release);
            return Err(error.into());
        }
        if let Err(error) = self
            .database
            .requeue_interrupted_scan_local_metadata_backfills()
            .await
        {
            self.local_metadata_outbox_worker_started
                .store(false, Ordering::Release);
            return Err(error.into());
        }
        if let Err(error) = self
            .database
            .ensure_scan_local_metadata_backfill_roots()
            .await
        {
            self.local_metadata_outbox_worker_started
                .store(false, Ordering::Release);
            return Err(error.into());
        }

        let database = self.database.clone();
        let local_nfo = self.local_nfo.clone();
        let metadata_selection = self.metadata_selection.clone();
        let metadata_reidentify = self.metadata_reidentify.clone();
        let home = self.home.clone();
        let user_events = self.user_events.clone();
        let resources = self.resources.clone();
        let outbox_notify = Arc::clone(&self.local_metadata_outbox_notify);
        let image_notifications = Arc::clone(&self.local_metadata_image_notifications);
        tokio::spawn(async move {
            let enricher = MetadataEnricher::new(database.clone()).with_resource_metrics(resources);
            let enricher = match local_nfo {
                Some(local_nfo) => enricher.with_nfo_store(local_nfo),
                None => enricher,
            };
            let mut nfo_tasks = JoinSet::new();
            loop {
                if nfo_tasks.len() >= MAX_SCAN_LOCAL_METADATA_NFO_BATCHES_IN_FLIGHT {
                    if let Some(Err(error)) = nfo_tasks.join_next().await {
                        tracing::error!(%error, "local metadata NFO worker task panicked");
                    }
                    continue;
                }
                match database.claim_next_scan_local_metadata_batch().await {
                    Ok(Some(batch)) => {
                        if let Some((
                            batch_id,
                            scan_job_id,
                            source_snapshot,
                            non_retryable_item_ids,
                        )) = process_scan_local_metadata_batch(
                            &database,
                            &enricher,
                            home.as_ref(),
                            &user_events,
                            batch,
                        )
                        .await
                        {
                            notify_local_metadata_images_waiter(&image_notifications, &scan_job_id);
                            let database = database.clone();
                            let enricher = enricher.clone();
                            let metadata_selection = metadata_selection.clone();
                            let metadata_reidentify = metadata_reidentify.clone();
                            let home = home.clone();
                            let user_events = user_events.clone();
                            nfo_tasks.spawn(async move {
                                let context = LocalMetadataCompletionContext {
                                    metadata_selection: metadata_selection.as_ref(),
                                    metadata_reidentify: metadata_reidentify.as_ref(),
                                    home: home.as_ref(),
                                    user_events: &user_events,
                                };
                                finish_scan_local_metadata_batch(
                                    &database,
                                    &enricher,
                                    context,
                                    &batch_id,
                                    &scan_job_id,
                                    source_snapshot,
                                    &non_retryable_item_ids,
                                )
                                .await;
                                batch_id
                            });
                        }
                    }
                    Ok(None) => {
                        match database
                            .claim_next_scan_local_metadata_backfill_page(
                                SCAN_LOCAL_METADATA_BACKFILL_PAGE_SIZE,
                            )
                            .await
                        {
                            Ok(Some(page)) => {
                                match process_scan_local_metadata_backfill_images(
                                    &enricher,
                                    home.as_ref(),
                                    &user_events,
                                    &page,
                                )
                                .await
                                {
                                    Ok(source_snapshot) => {
                                        let database = database.clone();
                                        let enricher = enricher.clone();
                                        let metadata_selection = metadata_selection.clone();
                                        let metadata_reidentify = metadata_reidentify.clone();
                                        let home = home.clone();
                                        let user_events = user_events.clone();
                                        let library_root_id = page.library_root_id.clone();
                                        nfo_tasks.spawn(async move {
                                            let context = LocalMetadataCompletionContext {
                                                metadata_selection: metadata_selection.as_ref(),
                                                metadata_reidentify: metadata_reidentify.as_ref(),
                                                home: home.as_ref(),
                                                user_events: &user_events,
                                            };
                                            finish_scan_local_metadata_backfill_page(
                                                &database,
                                                &enricher,
                                                context,
                                                page,
                                                source_snapshot,
                                            )
                                            .await;
                                            library_root_id
                                        });
                                    }
                                    Err(error) => {
                                        tracing::warn!(
                                            library_root_id = %page.library_root_id,
                                            %error,
                                            "local metadata backfill image stage failed and will be retried"
                                        );
                                        fail_scan_local_metadata_backfill_page(
                                            &database,
                                            &page,
                                            &error,
                                            &[],
                                        )
                                        .await;
                                    }
                                }
                            }
                            Ok(None) => {
                                if nfo_tasks.is_empty() {
                                    tokio::select! {
                                        _ = outbox_notify.notified() => {}
                                        _ = tokio::time::sleep(SCAN_LOCAL_METADATA_OUTBOX_IDLE_FALLBACK) => {}
                                    }
                                } else {
                                    tokio::select! {
                                        result = nfo_tasks.join_next() => {
                                            if let Some(Err(error)) = result {
                                                tracing::error!(%error, "local metadata worker task panicked");
                                            }
                                        }
                                        _ = outbox_notify.notified() => {}
                                        _ = tokio::time::sleep(SCAN_LOCAL_METADATA_OUTBOX_IDLE_FALLBACK) => {}
                                    }
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error, "local metadata backfill claim failed; retrying");
                                tokio::time::sleep(SCAN_LOCAL_METADATA_OUTBOX_IDLE_FALLBACK).await;
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "local metadata outbox claim failed; retrying");
                        tokio::time::sleep(SCAN_LOCAL_METADATA_OUTBOX_IDLE_FALLBACK).await;
                    }
                }
            }
        });
        Ok(())
    }
}
