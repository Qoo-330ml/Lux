use super::*;

impl ScanJobService {
    pub(in crate::application::scanner) async fn verify_manifest_postprocessing_target_roots(
        &self,
        job_id: &str,
    ) -> Result<(), ScanJobError> {
        let Some(manifest) = self
            .database
            .get_scan_manifest_postprocessing_state_by_job(job_id)
            .await?
        else {
            return Ok(());
        };
        if !matches!(manifest.workflow_version, 2 | 3) || manifest.discovery_format_version != 3 {
            return Ok(());
        }
        for root in manifest.roots {
            if !root.has_positive_rows {
                continue;
            }
            let root_path = PathBuf::from(&root.canonical_path);
            let root_matches = stat_manifest_root(root_path.clone())
                .await
                .is_ok_and(|observed| {
                    manifest_root_identity_matches(
                        root.expected_device,
                        root.expected_inode,
                        &observed,
                    )
                });
            if !root_matches {
                self.database
                    .mark_scan_manifest_root_unavailable(
                        &manifest.manifest_id,
                        &root.library_root_id,
                    )
                    .await?;
                self.record_event(
                    job_id,
                    "WARN",
                    "POSTPROCESSING_TARGET_ROOT_UNAVAILABLE",
                    "后处理目标根目录身份发生变化，已保留目标供恢复后重试",
                    "{}",
                )
                .await;
                return Err(ScannerError::RootIdentityChanged(root_path).into());
            }
            self.database
                .update_library_root_availability(&root.library_root_id, true)
                .await?;
        }
        Ok(())
    }

    pub(in crate::application::scanner) async fn publish_webhook_event(
        &self,
        job: &StoredScanJob,
        event_type: WebhookEventType,
        error_code: Option<&str>,
    ) {
        self.publish_webhook_event_with_data(job, event_type, error_code, json!({}))
            .await;
    }

    async fn publish_media_added_event(&self, job: &StoredScanJob, added_count: usize) {
        if added_count > 0 {
            self.publish_webhook_event_with_data(
                job,
                WebhookEventType::MediaAdded,
                None,
                json!({ "addedCount": added_count }),
            )
            .await;
        }
    }

    pub(in crate::application::scanner) async fn publish_webhook_event_with_data(
        &self,
        job: &StoredScanJob,
        event_type: WebhookEventType,
        error_code: Option<&str>,
        extra: Value,
    ) {
        let Some(webhooks) = self.webhooks.as_ref() else {
            return;
        };
        let occurred_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .unwrap_or(0);
        let dedupe_key = format!("scan:{}:{}", job.id, event_type.as_str());
        let library_name = self
            .database
            .find_library(&job.library_id)
            .await
            .ok()
            .flatten()
            .map(|library| library.name);
        let duration_seconds = job
            .started_at
            .map(|started_at| occurred_at.saturating_sub(started_at));
        let mut data = json!({
            "jobId": job.id,
            "libraryId": job.library_id,
            "libraryName": library_name,
            "jobType": job.job_type,
            "status": job.status,
            "processedCount": job.processed_count,
            "totalCount": job.total_count,
            "durationSeconds": duration_seconds,
            "errorCode": error_code,
        });
        if let (Value::Object(data), Value::Object(extra)) = (&mut data, extra) {
            data.extend(extra);
        }
        let result = webhooks
            .publish(event_type, &dedupe_key, occurred_at, data)
            .await;
        if result.is_err() {
            tracing::warn!(job_id = %job.id, event_type = event_type.as_str(), "failed to enqueue webhook event");
        }
    }

    pub(in crate::application::scanner) async fn run_to_completion_with_metadata_and_thumbnails_inner(
        &self,
        job_id: &str,
        batch_size: usize,
        probe: Option<MediaProbeService>,
        metadata: Option<MetadataReidentifyService>,
        thumbnails: Option<ThumbnailService>,
    ) -> Result<(), ScanJobError> {
        let manifest_workflow_version = self
            .database
            .get_scan_manifest_by_job(job_id)
            .await?
            .filter(|manifest| manifest.discovery_format_version == 3)
            .map(|manifest| manifest.workflow_version);
        let uses_local_metadata_outbox = manifest_workflow_version == Some(3);
        let defer_local_metadata_worker = matches!(manifest_workflow_version, Some(2 | 3));
        if uses_local_metadata_outbox
            && let Err(error) = self.start_local_metadata_outbox_worker().await
        {
            tracing::warn!(job_id, %error, "local metadata outbox worker did not start");
        }
        let mut scan_permit = self.acquire_scan_lock_for_job(job_id).await?;
        let mut local_metadata_worker =
            (!defer_local_metadata_worker).then(|| self.start_local_metadata_worker(job_id));
        let mut created_items = 0_usize;
        loop {
            let report = match self
                .run_batch_with_failure_handling(job_id, batch_size, true)
                .await
            {
                Ok(report) => report,
                Err(error) => {
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    return Err(error);
                }
            };
            if report.processed > 0
                && !report.completed
                && let Some(home) = &self.home
            {
                home.invalidate_scan_batch().await;
            }
            created_items = created_items.saturating_add(report.created_items);
            if !report.completed {
                if self.should_yield_to_realtime(job_id).await? {
                    drop(scan_permit);
                    scan_permit = self.acquire_scan_lock_for_job(job_id).await?;
                }
                tokio::task::yield_now().await;
                continue;
            }
            if report.status == "COMPLETED" {
                let Some(completed_job) = self.database.find_scan_job(job_id).await? else {
                    if self.cancellation_requested_in_memory(job_id) {
                        Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                        self.clear_cancellation_flag(job_id);
                        return Ok(());
                    }
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    return Err(ScanJobError::JobNotFound);
                };
                let incremental = completed_job.job_type == "INCREMENTAL_SCAN";
                if !incremental && defer_local_metadata_worker {
                    if let Err(error) = self
                        .materialize_manifest_postprocessing_targets_unlocked(job_id)
                        .await
                    {
                        Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                        return Err(error);
                    }
                    if let Err(error) = self
                        .verify_manifest_postprocessing_target_roots(job_id)
                        .await
                    {
                        Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                        return Err(error);
                    }
                    if !uses_local_metadata_outbox {
                        local_metadata_worker = Some(self.start_local_metadata_worker(job_id));
                    }
                }
                drop(scan_permit);
                if self
                    .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                    .await?
                {
                    return Ok(());
                }
                if incremental {
                    if let Err(error) = self.wait_for_local_metadata(job_id).await {
                        Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                        return Err(error);
                    }
                    if self
                        .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                        .await?
                    {
                        return Ok(());
                    }
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    self.run_probe_after_scan(job_id, probe).await?;
                    if self
                        .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                        .await?
                    {
                        return Ok(());
                    }
                    self.run_thumbnails_after_incremental_scan(job_id, thumbnails)
                        .await?;
                    if self
                        .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                        .await?
                    {
                        return Ok(());
                    }
                    for target_type in ["SOURCE", "ITEM"] {
                        self.database
                            .skip_pending_scan_job_target_stage(job_id, target_type, "THUMBNAIL")
                            .await?;
                    }
                    self.database
                        .clear_completed_scan_job_targets(job_id)
                        .await?;
                    let strm_probe_scheduled = if completed_job.auto_metadata_match {
                        if !uses_local_metadata_outbox
                            && self.metadata_selection.is_none()
                            && let Some(metadata) = metadata
                        {
                            self.schedule_online_metadata_after_incremental_scan(job_id, metadata)
                                .await;
                        }
                        if let Some(strm_probe) = self.strm_probe.clone() {
                            self.schedule_strm_probe_after_incremental_scan(
                                job_id,
                                &completed_job.library_id,
                                strm_probe,
                            )
                            .await
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    self.run_auto_library_cover_after_scan(job_id).await?;
                    self.flush_home_after_scan_terminal().await;
                    self.publish_media_added_event(&completed_job, created_items)
                        .await;
                    if !strm_probe_scheduled {
                        self.database.clear_scan_job_paths(job_id).await?;
                    }
                    return Ok(());
                }
                if let Err(error) = self.database.retry_failed_scan_job_targets(job_id).await {
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    return Err(error.into());
                }
                if uses_local_metadata_outbox {
                    for target_type in ["SOURCE", "ITEM"] {
                        self.database
                            .skip_pending_scan_job_target_stage(job_id, target_type, "METADATA")
                            .await?;
                    }
                }
                if let Err(error) = self.wait_for_local_metadata(job_id).await {
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    return Err(error);
                }
                if self
                    .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                    .await?
                {
                    return Ok(());
                }
                Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                self.update_activity(job_id, Some("媒体探测"), "POSTPROCESSING")
                    .await?;
                if defer_local_metadata_worker {
                    self.verify_manifest_postprocessing_target_roots(job_id)
                        .await?;
                }
                self.run_probe_after_scan(job_id, probe).await?;
                if self
                    .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                    .await?
                {
                    return Ok(());
                }
                self.update_activity(job_id, Some("本地元数据"), "POSTPROCESSING")
                    .await?;
                if defer_local_metadata_worker {
                    self.verify_manifest_postprocessing_target_roots(job_id)
                        .await?;
                }
                self.run_metadata_after_scan(job_id).await?;
                if self
                    .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                    .await?
                {
                    return Ok(());
                }
                self.update_activity(job_id, Some("媒体库封面"), "POSTPROCESSING")
                    .await?;
                self.run_auto_library_cover_after_scan(job_id).await?;
                if self
                    .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                    .await?
                {
                    return Ok(());
                }
                self.update_activity(job_id, Some("视频缩略图"), "POSTPROCESSING")
                    .await?;
                if defer_local_metadata_worker {
                    self.verify_manifest_postprocessing_target_roots(job_id)
                        .await?;
                }
                if uses_local_metadata_outbox && thumbnails.is_some() {
                    self.wait_for_local_metadata_images(job_id).await?;
                    if self
                        .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                        .await?
                    {
                        return Ok(());
                    }
                }
                self.run_thumbnails_after_scan(job_id, thumbnails).await?;
                if self
                    .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                    .await?
                {
                    return Ok(());
                }
                self.database
                    .clear_completed_scan_job_targets(job_id)
                    .await?;
                let completed = self
                    .database
                    .complete_scan_job_postprocessing(job_id)
                    .await?;
                if !completed {
                    if self.database.fail_scan_job_postprocessing(job_id).await? {
                        self.record_event(
                            job_id,
                            "ERROR",
                            "POSTPROCESSING_FAILED",
                            "扫描后处理失败，可重试未完成目标",
                            "{}",
                        )
                        .await;
                    }
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    self.flush_home_after_scan_terminal().await;
                    return Ok(());
                }
                if let Err(error) = self
                    .database
                    .cleanup_completed_scan_manifest_payloads()
                    .await
                {
                    tracing::warn!(job_id, %error, "completed scan manifest payload cleanup failed");
                }
                self.database.spawn_removed_media_item_purge();
                if completed_job.auto_metadata_match && !uses_local_metadata_outbox {
                    if let Some(metadata) = metadata {
                        self.schedule_online_metadata_after_scan(job_id, metadata)
                            .await;
                    }
                }
                self.flush_home_after_scan_terminal().await;
                self.publish_media_added_event(&completed_job, created_items)
                    .await;
            }
            Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
            return Ok(());
        }
    }

    async fn run_auto_library_cover_after_scan(&self, job_id: &str) -> Result<(), ScanJobError> {
        let Some(covers) = self.library_covers.as_ref() else {
            return Ok(());
        };
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Ok(());
        };
        let Ok(library_id) = job.library_id.parse::<LibraryId>() else {
            tracing::warn!(
                job_id,
                library_id = %job.library_id,
                "automatic library cover generation skipped for invalid library ID"
            );
            return Ok(());
        };
        match covers.generate_if_eligible(library_id).await {
            Ok(AutoLibraryCoverResult::Generated) => {
                self.record_event(
                    job_id,
                    "INFO",
                    "LIBRARY_COVER_GENERATED",
                    "已自动生成媒体库封面",
                    "{}",
                )
                .await;
            }
            Ok(
                AutoLibraryCoverResult::BelowThreshold
                | AutoLibraryCoverResult::ExistingCover
                | AutoLibraryCoverResult::TaskNotRegistered
                | AutoLibraryCoverResult::AlreadyHandled,
            ) => {}
            Err(error) => {
                tracing::warn!(job_id, %error, "automatic library cover generation failed");
                self.record_event(
                    job_id,
                    "ERROR",
                    "LIBRARY_COVER_FAILED",
                    "自动媒体库封面生成失败",
                    "{}",
                )
                .await;
            }
        }
        Ok(())
    }

    async fn acquire_scan_lock(&self) -> Result<OwnedSemaphorePermit, ScanJobError> {
        Arc::clone(&self.scan_lock)
            .acquire_owned()
            .await
            .map_err(|_| ScanJobError::ScanLockClosed)
    }

    pub(in crate::application::scanner) async fn acquire_scan_lock_for_job(
        &self,
        job_id: &str,
    ) -> Result<OwnedSemaphorePermit, ScanJobError> {
        let mut database_wait_retry = 0_usize;
        loop {
            let job = self.database.find_scan_job(job_id).await?;
            let Some(job) = job else {
                return self.acquire_scan_lock().await;
            };
            if job.job_type == "INCREMENTAL_SCAN"
                && self
                    .database
                    .has_unready_manifest_target_materialization_for_library(&job.library_id)
                    .await?
            {
                tokio::time::sleep(scan_lock_database_wait_backoff(database_wait_retry)).await;
                database_wait_retry = database_wait_retry.saturating_add(1);
                continue;
            }
            let is_postprocessing_target_materializer = job.job_type == "RECONCILE_LIBRARY"
                && job.status == "COMPLETED"
                && job.scan_phase == "POSTPROCESSING"
                && self
                    .database
                    .has_unready_manifest_target_materialization_for_library(&job.library_id)
                    .await?;
            let is_full_scan = (job.job_type != "INCREMENTAL_SCAN"
                && matches!(job.status.as_str(), "PENDING" | "RUNNING")
                && job.scan_phase != "POSTPROCESSING"
                && !job.cancel_requested)
                || is_postprocessing_target_materializer;
            let has_incremental = if is_postprocessing_target_materializer {
                self.database
                    .has_running_scan_job_type("INCREMENTAL_SCAN")
                    .await?
            } else if is_full_scan {
                self.database
                    .has_active_scan_job_type("INCREMENTAL_SCAN")
                    .await?
            } else {
                false
            };
            if is_full_scan && has_incremental {
                tokio::time::sleep(scan_lock_database_wait_backoff(database_wait_retry)).await;
                database_wait_retry = database_wait_retry.saturating_add(1);
                continue;
            }

            let permit = self.acquire_scan_lock().await?;
            let has_incremental = if is_postprocessing_target_materializer {
                self.database
                    .has_running_scan_job_type("INCREMENTAL_SCAN")
                    .await?
            } else if is_full_scan {
                self.database
                    .has_active_scan_job_type("INCREMENTAL_SCAN")
                    .await?
            } else {
                false
            };
            if !is_full_scan || !has_incremental {
                return Ok(permit);
            }
            drop(permit);
            tokio::time::sleep(scan_lock_database_wait_backoff(database_wait_retry)).await;
            database_wait_retry = database_wait_retry.saturating_add(1);
        }
    }

    async fn should_yield_to_realtime(&self, job_id: &str) -> Result<bool, ScanJobError> {
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Ok(false);
        };
        if job.job_type == "INCREMENTAL_SCAN"
            || !matches!(job.status.as_str(), "PENDING" | "RUNNING")
            || job.scan_phase == "POSTPROCESSING"
            || job.cancel_requested
        {
            return Ok(false);
        }
        self.database
            .has_active_scan_job_type("INCREMENTAL_SCAN")
            .await
            .map_err(ScanJobError::from)
    }

    pub(in crate::application::scanner) async fn effective_scan_concurrency(
        &self,
        configured: i64,
    ) -> usize {
        self.resources
            .background_concurrency(usize::try_from(configured).unwrap_or(1))
            .await
    }

    async fn run_thumbnails_after_scan(
        &self,
        job_id: &str,
        thumbnails: Option<ThumbnailService>,
    ) -> Result<(), ScanJobError> {
        if self.database.find_scan_job(job_id).await?.is_none() {
            return Ok(());
        }
        let Some(thumbnails) = thumbnails else {
            self.database
                .skip_pending_scan_job_target_stage(job_id, "ITEM", "THUMBNAIL")
                .await?;
            return Ok(());
        };
        self.database
            .ensure_scan_job_thumbnail_targets(job_id)
            .await?;
        let started = Instant::now();
        match thumbnails.generate_scan_job(job_id).await {
            Ok(report) if report.failed == 0 => {
                let items = report
                    .considered
                    .saturating_add(report.skipped_strm)
                    .saturating_add(report.skipped_policy);
                let elapsed_ms = started.elapsed().as_millis();
                let details = format!(
                    r#"{{"considered":{},"generated":{},"reused":{},"failed":{},"skippedStrm":{},"skippedPolicy":{},"elapsedMs":{},"itemsPerSecond":{}}}"#,
                    report.considered,
                    report.generated,
                    report.reused,
                    report.failed,
                    report.skipped_strm,
                    report.skipped_policy,
                    elapsed_ms,
                    throughput_per_second(items, elapsed_ms),
                );
                self.record_event(
                    job_id,
                    "INFO",
                    "THUMBNAIL_COMPLETED",
                    "视频缩略图任务完成",
                    &details,
                )
                .await;
            }
            Ok(report) => {
                let items = report
                    .considered
                    .saturating_add(report.skipped_strm)
                    .saturating_add(report.skipped_policy);
                let elapsed_ms = started.elapsed().as_millis();
                let details = format!(
                    r#"{{"considered":{},"generated":{},"reused":{},"failed":{},"skippedStrm":{},"skippedPolicy":{},"elapsedMs":{},"itemsPerSecond":{}}}"#,
                    report.considered,
                    report.generated,
                    report.reused,
                    report.failed,
                    report.skipped_strm,
                    report.skipped_policy,
                    elapsed_ms,
                    throughput_per_second(items, elapsed_ms),
                );
                self.record_event(
                    job_id,
                    "WARN",
                    "THUMBNAIL_FAILED",
                    "部分视频缩略图生成失败",
                    &details,
                )
                .await;
            }
            Err(error) => {
                tracing::warn!(job_id, %error, "scan thumbnail task failed");
                self.record_event(
                    job_id,
                    "ERROR",
                    "THUMBNAIL_FAILED",
                    "视频缩略图任务失败",
                    &format!(
                        r#"{{"elapsedMs":{},"itemsPerSecond":0}}"#,
                        started.elapsed().as_millis()
                    ),
                )
                .await;
            }
        }
        Ok(())
    }

    async fn schedule_online_metadata_after_scan(
        &self,
        scan_job_id: &str,
        metadata: MetadataReidentifyService,
    ) {
        let Some(scan_job) = self
            .database
            .find_scan_job(scan_job_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        let Some(library) = self
            .database
            .find_library(&scan_job.library_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        if library.scraper_id.as_deref().is_none() {
            return;
        }
        let Ok(library_id) = scan_job.library_id.parse::<LibraryId>() else {
            tracing::warn!(
                scan_job_id,
                library_id = %scan_job.library_id,
                "automatic metadata matching skipped for invalid library ID"
            );
            return;
        };
        let job = match metadata
            .create_library_refresh_job(&library_id.to_string(), MetadataRefreshMode::FillMissing)
            .await
        {
            Ok(job) => job,
            Err(MetadataReidentifyError::InvalidItemCount) => return,
            Err(_) => {
                tracing::warn!(
                    scan_job_id,
                    "scan completed but automatic metadata matching could not be queued"
                );
                self.record_event(
                    scan_job_id,
                    "ERROR",
                    "METADATA_AUTO_MATCH_QUEUE_FAILED",
                    "自动元数据匹配任务创建失败",
                    "{}",
                )
                .await;
                return;
            }
        };
        if let Err(error) = metadata.enqueue_fill_missing_job(&job).await {
            tracing::warn!(
                scan_job_id,
                %error,
                "scan completed but automatic metadata matching could not be dispatched"
            );
            self.record_event(
                scan_job_id,
                "ERROR",
                "METADATA_AUTO_MATCH_QUEUE_FAILED",
                "自动元数据匹配任务创建失败",
                "{}",
            )
            .await;
            return;
        }
        let details = format!(
            r#"{{"itemCount":{},"jobId":"{}","mode":"FILL_MISSING"}}"#,
            job.total_count, job.id
        );
        self.record_event(
            scan_job_id,
            "INFO",
            "METADATA_AUTO_MATCH_QUEUED",
            "已提交自动元数据匹配任务",
            &details,
        )
        .await;
    }

    async fn schedule_online_metadata_after_incremental_scan(
        &self,
        scan_job_id: &str,
        metadata: MetadataReidentifyService,
    ) {
        let Some(scan_job) = self
            .database
            .find_scan_job(scan_job_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        let Some(library) = self
            .database
            .find_library(&scan_job.library_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        if library
            .scraper_id
            .as_deref()
            .is_none_or(|scraper_id| scraper_id.trim().is_empty())
        {
            return;
        }
        let Ok(item_ids) = self
            .database
            .list_media_item_ids_for_incremental_scan(scan_job_id)
            .await
        else {
            tracing::warn!(
                scan_job_id,
                "incremental scan completed but affected media items could not be found"
            );
            return;
        };
        for item_ids in item_ids.chunks(100) {
            let job = match metadata.create_fill_missing_job(item_ids.to_vec()).await {
                Ok(job) => job,
                Err(_) => {
                    tracing::warn!(
                        scan_job_id,
                        item_count = item_ids.len(),
                        "incremental scan completed but automatic metadata matching could not be queued"
                    );
                    self.record_event(
                        scan_job_id,
                        "ERROR",
                        "METADATA_AUTO_MATCH_QUEUE_FAILED",
                        "自动元数据匹配任务创建失败",
                        "{}",
                    )
                    .await;
                    continue;
                }
            };
            if let Err(error) = metadata.enqueue_fill_missing_job(&job).await {
                tracing::warn!(
                    scan_job_id,
                    item_count = item_ids.len(),
                    %error,
                    "incremental scan completed but automatic metadata matching could not be dispatched"
                );
                self.record_event(
                    scan_job_id,
                    "ERROR",
                    "METADATA_AUTO_MATCH_QUEUE_FAILED",
                    "自动元数据匹配任务创建失败",
                    "{}",
                )
                .await;
                continue;
            }
            let details = format!(
                r#"{{"itemCount":{},"jobId":"{}","mode":"FILL_MISSING"}}"#,
                job.total_count, job.id
            );
            self.record_event(
                scan_job_id,
                "INFO",
                "METADATA_AUTO_MATCH_QUEUED",
                "已提交自动元数据匹配任务",
                &details,
            )
            .await;
        }
    }

    async fn schedule_strm_probe_after_incremental_scan(
        &self,
        scan_job_id: &str,
        library_id: &str,
        strm_probe: StrmProbeService,
    ) -> bool {
        let Ok(library_id) = library_id.parse::<LibraryId>() else {
            tracing::warn!(
                scan_job_id,
                library_id,
                "incremental scan completed but automatic STRM probe skipped for invalid library ID"
            );
            return false;
        };
        let job = match strm_probe
            .create_configured_incremental_job(scan_job_id, library_id)
            .await
        {
            Ok(Some(job)) => job,
            Ok(None) => return false,
            Err(error) => {
                tracing::warn!(
                    scan_job_id,
                    %error,
                    "incremental scan completed but automatic STRM probe could not be queued"
                );
                return false;
            }
        };
        let job_id = job.id.clone();
        let worker = strm_probe;
        let database = self.database.clone();
        let scan_job_id_for_cleanup = scan_job_id.to_owned();
        tokio::spawn(async move {
            if let Err(error) = worker.run(&job_id).await {
                tracing::error!(job_id = %job_id, %error, "automatic STRM probe stopped");
            }
            if let Err(error) = database
                .clear_scan_job_paths(&scan_job_id_for_cleanup)
                .await
            {
                tracing::warn!(
                    scan_job_id = %scan_job_id_for_cleanup,
                    %error,
                    "automatic STRM probe finished but scan paths could not be cleared"
                );
            }
        });
        self.record_event(
            scan_job_id,
            "INFO",
            "STRM_MEDIA_INFO_AUTO_QUEUED",
            "已提交新增 STRM 媒体信息识别任务",
            &format!(
                r#"{{"jobId":"{}","itemCount":{}}}"#,
                job.id, job.total_count
            ),
        )
        .await;
        true
    }

    async fn run_metadata_after_scan(&self, job_id: &str) -> Result<(), ScanJobError> {
        if self
            .database
            .get_scan_manifest_by_job(job_id)
            .await?
            .is_some_and(|manifest| manifest.workflow_version == 3)
        {
            return Ok(());
        }
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Ok(());
        };
        if self.database.find_library(&job.library_id).await?.is_none() {
            return Ok(());
        }
        let enricher = MetadataEnricher::new(self.database.clone())
            .with_resource_metrics(self.resources.clone());
        let enricher = match self.people.clone() {
            Some(people) => enricher.with_people(people),
            None => enricher,
        };
        let enricher = match self.local_nfo.clone() {
            Some(local_nfo) => enricher.with_nfo_store(local_nfo),
            None => enricher,
        };
        let started = Instant::now();
        let result = enricher.enrich_scan_job(job_id).await;
        match result {
            Ok(report) => {
                let items = report
                    .nfo_loaded
                    .saturating_add(report.nfo_failed)
                    .saturating_add(report.nfo_skipped);
                let elapsed_ms = started.elapsed().as_millis();
                let details = format!(
                    r#"{{"nfoLoaded":{},"nfoFailed":{},"nfoSkipped":{},"imagesFound":{},"elapsedMs":{},"itemsPerSecond":{}}}"#,
                    report.nfo_loaded,
                    report.nfo_failed,
                    report.nfo_skipped,
                    report.images_found,
                    elapsed_ms,
                    throughput_per_second(items, elapsed_ms),
                );
                self.record_event(
                    job_id,
                    "INFO",
                    "METADATA_COMPLETED",
                    "本地元数据处理完成",
                    &details,
                )
                .await;
            }
            Err(_) => {
                tracing::warn!(
                    job_id,
                    "scan completed but local metadata enrichment failed"
                );
                self.record_event(
                    job_id,
                    "ERROR",
                    "METADATA_FAILED",
                    "本地元数据处理失败",
                    &format!(
                        r#"{{"elapsedMs":{},"itemsPerSecond":0}}"#,
                        started.elapsed().as_millis()
                    ),
                )
                .await;
            }
        }
        Ok(())
    }

    async fn run_thumbnails_after_incremental_scan(
        &self,
        job_id: &str,
        thumbnails: Option<ThumbnailService>,
    ) -> Result<(), ScanJobError> {
        if self.database.find_scan_job(job_id).await?.is_none() {
            return Ok(());
        }
        let Some(thumbnails) = thumbnails else {
            return Ok(());
        };
        match thumbnails.generate_incremental_scan(job_id).await {
            Ok(report) if report.failed == 0 => {
                let details = format!(
                    r#"{{"considered":{},"generated":{},"reused":{},"failed":{},"skippedStrm":{},"skippedPolicy":{}}}"#,
                    report.considered,
                    report.generated,
                    report.reused,
                    report.failed,
                    report.skipped_strm,
                    report.skipped_policy,
                );
                self.record_event(
                    job_id,
                    "INFO",
                    "THUMBNAIL_COMPLETED",
                    "局部扫描视频缩略图任务完成",
                    &details,
                )
                .await;
            }
            Ok(report) => {
                let details = format!(
                    r#"{{"considered":{},"generated":{},"reused":{},"failed":{},"skippedStrm":{},"skippedPolicy":{}}}"#,
                    report.considered,
                    report.generated,
                    report.reused,
                    report.failed,
                    report.skipped_strm,
                    report.skipped_policy,
                );
                self.record_event(
                    job_id,
                    "WARN",
                    "THUMBNAIL_FAILED",
                    "局部扫描视频缩略图任务部分失败",
                    &details,
                )
                .await;
            }
            Err(error) => {
                tracing::warn!(job_id, %error, "incremental scan thumbnail task failed");
                self.record_event(
                    job_id,
                    "WARN",
                    "THUMBNAIL_FAILED",
                    "局部扫描视频缩略图任务失败",
                    "{}",
                )
                .await;
            }
        }
        Ok(())
    }

    async fn run_probe_after_scan(
        &self,
        job_id: &str,
        probe: Option<MediaProbeService>,
    ) -> Result<(), ScanJobError> {
        if self.database.find_scan_job(job_id).await?.is_none() {
            return Ok(());
        }
        let Some(probe) = probe else {
            self.database
                .skip_pending_scan_job_target_stage(job_id, "SOURCE", "PROBE")
                .await?;
            return Ok(());
        };
        let job = self
            .database
            .find_scan_job(job_id)
            .await?
            .ok_or(ScanJobError::JobNotFound)?;
        let started = Instant::now();
        match probe.probe_scan_job(job_id, &job.library_id).await {
            Ok(report) => {
                let items = report.attempted.saturating_add(report.skipped);
                let elapsed_ms = started.elapsed().as_millis();
                let details = format!(
                    r#"{{"attempted":{},"ready":{},"failed":{},"timedOut":{},"skipped":{},"elapsedMs":{},"itemsPerSecond":{}}}"#,
                    report.attempted,
                    report.ready,
                    report.failed,
                    report.timed_out,
                    report.skipped,
                    elapsed_ms,
                    throughput_per_second(items, elapsed_ms),
                );
                self.record_event(job_id, "INFO", "PROBE_COMPLETED", "媒体探测完成", &details)
                    .await;
                self.database
                    .skip_pending_scan_job_target_stage(job_id, "SOURCE", "PROBE")
                    .await?;
            }
            Err(error) => {
                tracing::warn!(job_id, %error, "scan completed but media probe failed");
                self.record_event(
                    job_id,
                    "ERROR",
                    "PROBE_FAILED",
                    "媒体探测任务失败",
                    &format!(
                        r#"{{"elapsedMs":{},"itemsPerSecond":0}}"#,
                        started.elapsed().as_millis()
                    ),
                )
                .await;
            }
        }
        Ok(())
    }
}
