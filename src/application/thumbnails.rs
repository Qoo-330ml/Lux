use std::{
    collections::HashSet,
    fmt,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{Arc, OnceLock},
    time::Duration,
};

use sha2::{Digest, Sha256};
use tokio::{fs, process::Command, sync::Semaphore, task::JoinSet, time::timeout};
use uuid::Uuid;

use crate::{
    application::{
        images::{
            acquire_image_write_lock, canonical_thumbnail_path, first_available_thumbnail_path,
            is_fallback_image_source, read_image_dimensions_from_bytes, write_image_atomically,
        },
        thumbnail_policy::ThumbnailScrapingMode,
    },
    config::{
        DEFAULT_FFMPEG_CONCURRENCY, MAX_FFMPEG_CONCURRENCY, ffmpeg_concurrency_override_from_env,
    },
    domain::ids::LibraryId,
    storage::{Database, ItemImageMetadata, StorageError, StoredThumbnailSource},
};

const DEFAULT_FRAME: &str = "00:03:01";
const MAX_THUMBNAIL_BYTES: u64 = 50 * 1024 * 1024;
const POSTER_FILTER: &str =
    "scale=600:900:force_original_aspect_ratio=increase,crop=600:900,setsar=1";
const THUMBNAIL_FILTER: &str =
    "scale=1280:720:force_original_aspect_ratio=increase,crop=1280:720,setsar=1";
const LIBRARY_SOURCE_PAGE_SIZE: usize = 500;
const THUMBNAIL_WORKER_CONCURRENCY: usize = 4;
const GLOBAL_FFMPEG_CONCURRENCY: usize = MAX_FFMPEG_CONCURRENCY as usize;
static GLOBAL_FFMPEG_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

fn global_ffmpeg_permits() -> Arc<Semaphore> {
    GLOBAL_FFMPEG_PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(configured_ffmpeg_concurrency())))
        .clone()
}

fn configured_ffmpeg_concurrency() -> usize {
    match ffmpeg_concurrency_override_from_env() {
        Ok(Some(value)) => usize::try_from(value).unwrap_or(GLOBAL_FFMPEG_CONCURRENCY),
        Ok(None) => DEFAULT_FFMPEG_CONCURRENCY as usize,
        Err(error) => {
            tracing::warn!(%error, "invalid ffmpeg concurrency; using the safe default bound");
            DEFAULT_FFMPEG_CONCURRENCY as usize
        }
    }
}

#[derive(Clone)]
pub struct ThumbnailService {
    database: Database,
    ffmpeg_binary: PathBuf,
    timeout: Duration,
    ffmpeg_permits: Arc<Semaphore>,
}

impl ThumbnailService {
    pub fn new(database: Database) -> Self {
        Self::with_runner(database, PathBuf::from("ffmpeg"), Duration::from_secs(30))
    }

    pub fn with_runner(
        database: Database,
        ffmpeg_binary: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Self {
        Self {
            database,
            ffmpeg_binary: ffmpeg_binary.into(),
            timeout,
            ffmpeg_permits: global_ffmpeg_permits(),
        }
    }

    pub async fn generate_library(
        &self,
        library_id: LibraryId,
    ) -> Result<ThumbnailReport, ThumbnailError> {
        let mut seen_items = HashSet::new();
        let mut report = ThumbnailReport::default();
        let global_strategy = self.database.media_strategy_settings().await?;
        let library_id = library_id.to_string();
        let mut offset = 0_i64;
        loop {
            let candidates = self
                .database
                .list_local_thumbnail_sources_for_library_page(
                    &library_id,
                    LIBRARY_SOURCE_PAGE_SIZE as i64,
                    offset,
                )
                .await?;
            let last_page = candidates.len() < LIBRARY_SOURCE_PAGE_SIZE;
            self.generate_sources(
                candidates,
                global_strategy.as_deref(),
                &mut seen_items,
                &mut report,
                false,
            )
            .await?;
            if last_page {
                break;
            }
            offset = offset.saturating_add(LIBRARY_SOURCE_PAGE_SIZE as i64);
        }
        Ok(report)
    }

    pub async fn generate_incremental_scan(
        &self,
        scan_job_id: &str,
    ) -> Result<ThumbnailReport, ThumbnailError> {
        let mut seen_items = HashSet::new();
        let mut report = ThumbnailReport::default();
        let global_strategy = self.database.media_strategy_settings().await?;
        let mut offset = 0_i64;
        loop {
            let candidates = self
                .database
                .list_local_thumbnail_sources_for_incremental_scan_page(
                    scan_job_id,
                    LIBRARY_SOURCE_PAGE_SIZE as i64,
                    offset,
                )
                .await?;
            let last_page = candidates.len() < LIBRARY_SOURCE_PAGE_SIZE;
            self.generate_sources(
                candidates,
                global_strategy.as_deref(),
                &mut seen_items,
                &mut report,
                false,
            )
            .await?;
            if last_page {
                break;
            }
            offset = offset.saturating_add(LIBRARY_SOURCE_PAGE_SIZE as i64);
        }
        Ok(report)
    }

    pub async fn generate_scan_job(
        &self,
        scan_job_id: &str,
    ) -> Result<ThumbnailReport, ThumbnailError> {
        let mut report = ThumbnailReport::default();
        let global_strategy = self.database.media_strategy_settings().await?;
        loop {
            let candidates = self
                .database
                .list_scan_job_thumbnail_sources_page(
                    scan_job_id,
                    LIBRARY_SOURCE_PAGE_SIZE as i64,
                    0,
                )
                .await?;
            if candidates.is_empty() {
                break;
            }
            let item_ids = candidates
                .iter()
                .map(|candidate| candidate.item_id.clone())
                .collect::<Vec<_>>();
            let mut seen_items = HashSet::new();
            self.generate_sources(
                candidates,
                global_strategy.as_deref(),
                &mut seen_items,
                &mut report,
                false,
            )
            .await?;
            let failed_item_ids = report.failed_item_ids.clone();
            self.database
                .mark_scan_job_target_stage(
                    scan_job_id,
                    "ITEM",
                    &failed_item_ids,
                    "THUMBNAIL",
                    "FAILED",
                )
                .await?;
            let completed_item_ids = item_ids
                .into_iter()
                .filter(|item_id| !failed_item_ids.iter().any(|failed| failed == item_id))
                .collect::<Vec<_>>();
            self.database
                .mark_scan_job_target_stage(
                    scan_job_id,
                    "ITEM",
                    &completed_item_ids,
                    "THUMBNAIL",
                    "DONE",
                )
                .await?;
        }
        Ok(report)
    }

    pub(crate) async fn generate_item(
        &self,
        item_id: &str,
    ) -> Result<ThumbnailReport, ThumbnailError> {
        let Some(source) = self.database.find_local_thumbnail_source(item_id).await? else {
            return Ok(ThumbnailReport::default());
        };
        let global_strategy = self.database.media_strategy_settings().await?;
        let mut report = ThumbnailReport::default();
        let mut seen_items = HashSet::new();
        self.generate_sources(
            vec![source],
            global_strategy.as_deref(),
            &mut seen_items,
            &mut report,
            true,
        )
        .await?;
        Ok(report)
    }

    pub(crate) async fn scraper_first_retry_is_applicable(
        &self,
        item_id: &str,
    ) -> Result<bool, ThumbnailError> {
        let Some(source) = self.database.find_local_thumbnail_source(item_id).await? else {
            return Ok(false);
        };
        if source.scraper_id.is_none() || is_strm_path(&source.relative_path) {
            return Ok(false);
        }
        let global_strategy = self.database.media_strategy_settings().await?;
        Ok(ThumbnailScrapingMode::from_strategy_json(
            source.library_media_strategy_json.as_deref(),
            global_strategy.as_deref(),
        ) == ThumbnailScrapingMode::ScraperFirst)
    }

    pub(crate) async fn scraper_first_images_missing(
        &self,
        item_id: &str,
    ) -> Result<bool, ThumbnailError> {
        let Some(source) = self.database.find_local_thumbnail_source(item_id).await? else {
            return Ok(false);
        };
        let (_source_path, _target_path, root_path) = resolve_media_paths(&source).await?;
        let indexed_images = self.database.list_item_images(item_id).await?;
        for image_type in ["POSTER", "THUMB"] {
            if matches!(
                indexed_image_availability(&indexed_images, image_type, &root_path).await?,
                ImageAvailability::Missing | ImageAvailability::Fallback
            ) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn generate_sources(
        &self,
        candidates: Vec<StoredThumbnailSource>,
        global_strategy: Option<&str>,
        seen_items: &mut HashSet<String>,
        report: &mut ThumbnailReport,
        allow_scraper_first_screenshots: bool,
    ) -> Result<(), ThumbnailError> {
        let mut pending = JoinSet::new();
        for candidate in candidates {
            if !seen_items.insert(candidate.item_id.clone()) {
                continue;
            }
            let mode = ThumbnailScrapingMode::from_strategy_json(
                candidate.library_media_strategy_json.as_deref(),
                global_strategy,
            );
            if !mode.allows_screenshots() {
                report.skipped_policy += 1;
                continue;
            }
            if is_strm_path(&candidate.relative_path) {
                report.skipped_strm += 1;
                continue;
            }
            if !allow_scraper_first_screenshots
                && mode == ThumbnailScrapingMode::ScraperFirst
                && candidate.scraper_id.is_some()
                && self
                    .should_defer_scraper_first_screenshot(&candidate)
                    .await?
            {
                report.skipped_policy += 1;
                continue;
            }
            report.considered += 1;
            while pending.len() >= THUMBNAIL_WORKER_CONCURRENCY {
                self.collect_thumbnail_task(&mut pending, report).await;
            }
            let service = self.clone();
            pending.spawn(async move {
                let item_id = candidate.item_id.clone();
                let result = service.generate_for_source(&candidate, mode).await;
                (item_id, result)
            });
        }
        while !pending.is_empty() {
            self.collect_thumbnail_task(&mut pending, report).await;
        }
        Ok(())
    }

    async fn should_defer_scraper_first_screenshot(
        &self,
        source: &StoredThumbnailSource,
    ) -> Result<bool, ThumbnailError> {
        let Some(retry) = self
            .database
            .find_thumbnail_scraper_retry(&source.item_id)
            .await?
        else {
            return Ok(true);
        };
        Ok(retry.status != "COMPLETE" && retry.attempt_count < 3)
    }

    async fn collect_thumbnail_task(
        &self,
        pending: &mut JoinSet<(String, Result<ThumbnailOutcome, ThumbnailFileError>)>,
        report: &mut ThumbnailReport,
    ) {
        let Some(result) = pending.join_next().await else {
            return;
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(%error, "thumbnail worker panicked");
                report.failed += 1;
                return;
            }
        };
        match result.1 {
            Ok(ThumbnailOutcome::Generated) => report.generated += 1,
            Ok(ThumbnailOutcome::Reused) => report.reused += 1,
            Err(error) => {
                report.failed += 1;
                report.mark_item_failed(&result.0);
                tracing::warn!(
                    item_id = %result.0,
                    error = %error,
                    "thumbnail generation failed"
                );
            }
        }
    }

    async fn generate_for_source(
        &self,
        source: &StoredThumbnailSource,
        mode: ThumbnailScrapingMode,
    ) -> Result<ThumbnailOutcome, ThumbnailFileError> {
        let (source_path, _target_path, root_path) = resolve_media_paths(source).await?;
        let _image_write_lock = acquire_image_write_lock(&source.item_id).await;
        let indexed_images = self
            .database
            .list_item_images(&source.item_id)
            .await
            .map_err(ThumbnailFileError::Storage)?;

        let poster_target = first_available_poster_path(&source_path, &indexed_images)
            .ok_or(ThumbnailFileError::TargetUnavailable)?;
        let thumbnail_target = first_available_thumbnail_path(&source_path, &indexed_images)
            .ok_or(ThumbnailFileError::TargetUnavailable)?;
        let poster_availability =
            indexed_image_availability(&indexed_images, "POSTER", &root_path).await?;
        let thumbnail_availability =
            indexed_image_availability(&indexed_images, "THUMB", &root_path).await?;
        let poster_plan = self
            .plan_artwork(
                "POSTER",
                poster_target,
                POSTER_FILTER,
                poster_availability,
                mode,
            )
            .await?;
        let thumbnail_plan = self
            .plan_artwork(
                "THUMB",
                thumbnail_target,
                THUMBNAIL_FILTER,
                thumbnail_availability,
                mode,
            )
            .await?;

        if poster_availability == ImageAvailability::Missing
            && thumbnail_availability == ImageAvailability::Missing
            && poster_plan.action == ArtworkAction::Generate
            && thumbnail_plan.action == ArtworkAction::Generate
        {
            return match self
                .generate_pair(&source.item_id, &source_path, &poster_plan, &thumbnail_plan)
                .await
            {
                Ok(()) => Ok(ThumbnailOutcome::Generated),
                Err(PairGenerationError::Process(ThumbnailFileError::Exit(_))) => {
                    tracing::debug!(item_id = %source.item_id, "falling back to single-output ffmpeg");
                    let poster_outcome = self
                        .execute_artwork_plan(&source.item_id, &source_path, &poster_plan)
                        .await?;
                    let thumbnail_outcome = self
                        .execute_artwork_plan(&source.item_id, &source_path, &thumbnail_plan)
                        .await?;
                    Ok(
                        if poster_outcome == ArtworkOutcome::Generated
                            || thumbnail_outcome == ArtworkOutcome::Generated
                        {
                            ThumbnailOutcome::Generated
                        } else {
                            ThumbnailOutcome::Reused
                        },
                    )
                }
                Err(PairGenerationError::Process(error)) => Err(error),
                Err(PairGenerationError::Output(error)) => Err(error),
            };
        }

        let mut generated = false;
        for plan in [poster_plan, thumbnail_plan] {
            if self
                .execute_artwork_plan(&source.item_id, &source_path, &plan)
                .await?
                == ArtworkOutcome::Generated
            {
                generated = true;
            }
        }
        Ok(if generated {
            ThumbnailOutcome::Generated
        } else {
            ThumbnailOutcome::Reused
        })
    }

    async fn plan_artwork(
        &self,
        image_type: &'static str,
        target_path: PathBuf,
        filter: &'static str,
        availability: ImageAvailability,
        mode: ThumbnailScrapingMode,
    ) -> Result<ArtworkPlan, ThumbnailFileError> {
        let should_generate = matches!(availability, ImageAvailability::Missing)
            || (mode.prefers_screenshots() && matches!(availability, ImageAvailability::Scraper));
        if !should_generate {
            return Ok(ArtworkPlan {
                image_type,
                target_path,
                filter,
                action: ArtworkAction::Keep,
            });
        }
        let action = if matches!(availability, ImageAvailability::Missing) {
            match existing_target_bytes(&target_path).await? {
                Some(bytes) => ArtworkAction::RegisterExisting { bytes },
                None => ArtworkAction::Generate,
            }
        } else {
            ArtworkAction::Generate
        };
        Ok(ArtworkPlan {
            image_type,
            target_path,
            filter,
            action,
        })
    }

    async fn execute_artwork_plan(
        &self,
        item_id: &str,
        source_path: &Path,
        plan: &ArtworkPlan,
    ) -> Result<ArtworkOutcome, ThumbnailFileError> {
        if plan.action == ArtworkAction::Keep {
            return Ok(ArtworkOutcome::Reused);
        }
        if let ArtworkAction::RegisterExisting { bytes } = &plan.action {
            self.register_image(item_id, plan.image_type, &plan.target_path, bytes)
                .await?;
            return Ok(ArtworkOutcome::Reused);
        }
        self.generate_single(item_id, source_path, plan).await?;
        Ok(ArtworkOutcome::Generated)
    }

    async fn generate_single(
        &self,
        item_id: &str,
        source_path: &Path,
        plan: &ArtworkPlan,
    ) -> Result<(), ThumbnailFileError> {
        let temporary = temporary_artwork_path(&plan.target_path, plan.image_type);
        let result = async {
            self.run_ffmpeg(source_path, &temporary, plan.filter)
                .await?;
            let bytes = read_validated_output(&temporary).await?;
            self.write_and_register(item_id, plan.image_type, &plan.target_path, &bytes)
                .await
        }
        .await;
        let _ = fs::remove_file(&temporary).await;
        result
    }

    async fn generate_pair(
        &self,
        item_id: &str,
        source_path: &Path,
        poster: &ArtworkPlan,
        thumbnail: &ArtworkPlan,
    ) -> Result<(), PairGenerationError> {
        let poster_temporary = temporary_artwork_path(&poster.target_path, poster.image_type);
        let thumbnail_temporary =
            temporary_artwork_path(&thumbnail.target_path, thumbnail.image_type);
        let result = async {
            self.run_ffmpeg_pair(
                source_path,
                &poster_temporary,
                &thumbnail_temporary,
                poster.filter,
                thumbnail.filter,
            )
            .await
            .map_err(PairGenerationError::Process)?;
            let poster_bytes = read_validated_output(&poster_temporary)
                .await
                .map_err(PairGenerationError::Output)?;
            let thumbnail_bytes = read_validated_output(&thumbnail_temporary)
                .await
                .map_err(PairGenerationError::Output)?;
            self.write_and_register(
                item_id,
                poster.image_type,
                &poster.target_path,
                &poster_bytes,
            )
            .await
            .map_err(PairGenerationError::Output)?;
            self.write_and_register(
                item_id,
                thumbnail.image_type,
                &thumbnail.target_path,
                &thumbnail_bytes,
            )
            .await
            .map_err(PairGenerationError::Output)
        }
        .await;
        let _ = fs::remove_file(&poster_temporary).await;
        let _ = fs::remove_file(&thumbnail_temporary).await;
        result
    }

    async fn write_and_register(
        &self,
        item_id: &str,
        image_type: &str,
        target_path: &Path,
        bytes: &[u8],
    ) -> Result<(), ThumbnailFileError> {
        write_image_atomically(target_path, bytes)
            .await
            .map_err(|error| ThumbnailFileError::Write(error.to_string()))?;
        self.register_image(item_id, image_type, target_path, bytes)
            .await
    }

    async fn run_ffmpeg(
        &self,
        source_path: &Path,
        output_path: &Path,
        filter: &str,
    ) -> Result<(), ThumbnailFileError> {
        let _permit = self
            .ffmpeg_permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ThumbnailFileError::FfmpegLimit)?;
        let mut child = Command::new(&self.ffmpeg_binary)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-y",
                "-ss",
                DEFAULT_FRAME,
                "-i",
            ])
            .arg(source_path)
            .args(["-vf", filter, "-frames:v", "1", "-an", "-f", "image2"])
            .arg(output_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(ThumbnailFileError::ProcessIo)?;
        let status = match timeout(self.timeout, child.wait()).await {
            Ok(result) => result.map_err(ThumbnailFileError::ProcessIo)?,
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(ThumbnailFileError::Timeout);
            }
        };
        if status.success() {
            Ok(())
        } else {
            Err(ThumbnailFileError::Exit(status.code()))
        }
    }

    async fn run_ffmpeg_pair(
        &self,
        source_path: &Path,
        poster_output: &Path,
        thumbnail_output: &Path,
        poster_filter: &str,
        thumbnail_filter: &str,
    ) -> Result<(), ThumbnailFileError> {
        let _permit = self
            .ffmpeg_permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ThumbnailFileError::FfmpegLimit)?;
        let filter_complex = format!(
            "[0:v:0]split=2[poster_source][thumbnail_source];[poster_source]{poster_filter}[poster];[thumbnail_source]{thumbnail_filter}[thumbnail]"
        );
        let mut child = Command::new(&self.ffmpeg_binary)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-y",
                "-ss",
                DEFAULT_FRAME,
                "-i",
            ])
            .arg(source_path)
            .args(["-filter_complex", &filter_complex, "-map", "[poster]"])
            .args(["-frames:v", "1", "-an", "-f", "image2"])
            .arg(poster_output)
            .args(["-map", "[thumbnail]"])
            .args(["-frames:v", "1", "-an", "-f", "image2"])
            .arg(thumbnail_output)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(ThumbnailFileError::ProcessIo)?;
        let status = match timeout(self.timeout, child.wait()).await {
            Ok(result) => result.map_err(ThumbnailFileError::ProcessIo)?,
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(ThumbnailFileError::Timeout);
            }
        };
        if status.success() {
            Ok(())
        } else {
            Err(ThumbnailFileError::Exit(status.code()))
        }
    }

    async fn register_image(
        &self,
        item_id: &str,
        image_type: &str,
        path: &Path,
        bytes: &[u8],
    ) -> Result<(), ThumbnailFileError> {
        let file_size =
            i64::try_from(bytes.len()).map_err(|_| ThumbnailFileError::OutputTooLarge)?;
        let content_tag = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let dimensions = read_image_dimensions_from_bytes(bytes).await;
        self.database
            .upsert_item_image_at_index(
                item_id,
                image_type,
                0,
                path,
                ItemImageMetadata {
                    file_size,
                    width: dimensions.map(|(width, _)| width),
                    height: dimensions.map(|(_, height)| height),
                    content_tag: &content_tag,
                    source: "FFMPEG",
                    source_url: None,
                },
            )
            .await
            .map(|_| ())
            .map_err(ThumbnailFileError::Storage)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ArtworkAction {
    Keep,
    RegisterExisting { bytes: Vec<u8> },
    Generate,
}

struct ArtworkPlan {
    image_type: &'static str,
    target_path: PathBuf,
    filter: &'static str,
    action: ArtworkAction,
}

#[derive(Debug)]
enum PairGenerationError {
    Process(ThumbnailFileError),
    Output(ThumbnailFileError),
}

fn temporary_artwork_path(target_path: &Path, image_type: &str) -> PathBuf {
    target_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!(".lux-{}-{image_type}.tmp.jpg", Uuid::now_v7()))
}

async fn read_validated_output(path: &Path) -> Result<Vec<u8>, ThumbnailFileError> {
    let metadata = fs::symlink_metadata(path)
        .await
        .map_err(|error| ThumbnailFileError::io(path, error))?;
    if !metadata.is_file() {
        return Err(ThumbnailFileError::InvalidOutput);
    }
    if metadata.len() == 0 {
        return Err(ThumbnailFileError::InvalidOutput);
    }
    if metadata.len() > MAX_THUMBNAIL_BYTES {
        return Err(ThumbnailFileError::OutputTooLarge);
    }
    let bytes = fs::read(path)
        .await
        .map_err(|error| ThumbnailFileError::io(path, error))?;
    let byte_length = u64::try_from(bytes.len()).map_err(|_| ThumbnailFileError::OutputTooLarge)?;
    if byte_length > MAX_THUMBNAIL_BYTES {
        return Err(ThumbnailFileError::OutputTooLarge);
    }
    if bytes.is_empty() || !is_jpeg(&bytes) {
        return Err(ThumbnailFileError::InvalidOutput);
    }
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArtworkOutcome {
    Generated,
    Reused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageAvailability {
    Missing,
    Fallback,
    Local,
    Scraper,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ThumbnailReport {
    pub considered: usize,
    pub generated: usize,
    pub reused: usize,
    pub failed: usize,
    pub skipped_strm: usize,
    pub skipped_policy: usize,
    pub(crate) failed_item_ids: Vec<String>,
}

impl ThumbnailReport {
    fn mark_item_failed(&mut self, item_id: &str) {
        if !self.failed_item_ids.iter().any(|failed| failed == item_id) {
            self.failed_item_ids.push(item_id.to_owned());
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ThumbnailOutcome {
    Generated,
    Reused,
}

async fn resolve_media_paths(
    source: &StoredThumbnailSource,
) -> Result<(PathBuf, PathBuf, PathBuf), ThumbnailFileError> {
    let root_path = fs::canonicalize(&source.root_path)
        .await
        .map_err(|error| ThumbnailFileError::io(Path::new(&source.root_path), error))?;
    let relative_path = Path::new(&source.relative_path);
    if !safe_relative_path(relative_path) {
        return Err(ThumbnailFileError::InvalidRelativePath);
    }
    let source_path = fs::canonicalize(root_path.join(relative_path))
        .await
        .map_err(|error| ThumbnailFileError::io(&root_path, error))?;
    if !source_path.starts_with(&root_path) {
        return Err(ThumbnailFileError::OutsideRoot);
    }
    let metadata = fs::metadata(&source_path)
        .await
        .map_err(|error| ThumbnailFileError::io(&source_path, error))?;
    if !metadata.is_file() {
        return Err(ThumbnailFileError::SourceNotFile);
    }
    let parent = source_path
        .parent()
        .ok_or(ThumbnailFileError::InvalidSourcePath)?;
    if !parent.starts_with(&root_path) {
        return Err(ThumbnailFileError::OutsideRoot);
    }
    let target_path =
        canonical_thumbnail_path(&source_path).ok_or(ThumbnailFileError::InvalidSourcePath)?;
    Ok((source_path, target_path, root_path))
}

fn first_available_poster_path(
    media_path: &Path,
    indexed_images: &[crate::storage::StoredItemImage],
) -> Option<PathBuf> {
    let stem = media_path.file_stem()?.to_str()?;
    (0..1000).find_map(|variant| {
        let suffix = if variant == 0 {
            "poster".to_owned()
        } else {
            format!("poster-{variant}")
        };
        let candidate = media_path.with_file_name(format!("{stem}-{suffix}.jpg"));
        (!image_path_is_owned_by_other_type(indexed_images, "POSTER", &candidate))
            .then_some(candidate)
    })
}

fn image_path_is_owned_by_other_type(
    indexed_images: &[crate::storage::StoredItemImage],
    image_type: &str,
    path: &Path,
) -> bool {
    indexed_images.iter().any(|image| {
        !image.image_type.eq_ignore_ascii_case(image_type) && Path::new(&image.local_path) == path
    })
}

async fn indexed_image_availability(
    indexed_images: &[crate::storage::StoredItemImage],
    image_type: &str,
    root_path: &Path,
) -> Result<ImageAvailability, ThumbnailFileError> {
    let Some(image) = indexed_images
        .iter()
        .find(|image| image.image_type.eq_ignore_ascii_case(image_type) && image.image_index == 0)
    else {
        return Ok(ImageAvailability::Missing);
    };
    let path = Path::new(&image.local_path);
    if image_path_is_owned_by_other_type(indexed_images, image_type, path) {
        return Ok(ImageAvailability::Missing);
    }
    if !usable_image_path(path, root_path).await? {
        return Ok(ImageAvailability::Missing);
    }
    if is_fallback_image_source(&image.source) {
        Ok(ImageAvailability::Fallback)
    } else if image.source.eq_ignore_ascii_case("LOCAL") {
        Ok(ImageAvailability::Local)
    } else {
        Ok(ImageAvailability::Scraper)
    }
}

fn safe_relative_path(path: &Path) -> bool {
    !path.is_absolute()
        && path.components().all(|component| {
            !matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
}

async fn usable_image_path(path: &Path, root_path: &Path) -> Result<bool, ThumbnailFileError> {
    let metadata = match fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(ThumbnailFileError::io(path, error)),
    };
    if metadata.file_type().is_symlink() {
        return Err(ThumbnailFileError::SymlinkTarget);
    }
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_THUMBNAIL_BYTES {
        return Ok(false);
    }
    let canonical = fs::canonicalize(path)
        .await
        .map_err(|error| ThumbnailFileError::io(path, error))?;
    Ok(canonical.starts_with(root_path))
}

async fn existing_target_bytes(path: &Path) -> Result<Option<Vec<u8>>, ThumbnailFileError> {
    let metadata = match fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ThumbnailFileError::io(path, error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ThumbnailFileError::TargetUnavailable);
    }
    if metadata.len() == 0 || metadata.len() > MAX_THUMBNAIL_BYTES {
        return Ok(None);
    }
    let bytes = fs::read(path)
        .await
        .map_err(|error| ThumbnailFileError::io(path, error))?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_THUMBNAIL_BYTES) {
        return Ok(None);
    }
    if !is_jpeg(&bytes) {
        return Ok(None);
    }
    Ok(Some(bytes))
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9])
}

fn is_strm_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"))
}

#[derive(Debug)]
enum ThumbnailFileError {
    Exit(Option<i32>),
    InvalidOutput,
    InvalidRelativePath,
    InvalidSourcePath,
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    OutsideRoot,
    OutputTooLarge,
    ProcessIo(std::io::Error),
    FfmpegLimit,
    Storage(StorageError),
    SymlinkTarget,
    TargetUnavailable,
    Timeout,
    Write(String),
    SourceNotFile,
}

impl ThumbnailFileError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_owned(),
            source,
        }
    }
}

impl fmt::Display for ThumbnailFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exit(code) => write!(formatter, "ffmpeg exited with code {code:?}"),
            Self::InvalidOutput => formatter.write_str("ffmpeg did not produce a JPEG image"),
            Self::InvalidRelativePath => formatter.write_str("media relative path is invalid"),
            Self::InvalidSourcePath => formatter.write_str("media source path is invalid"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::OutsideRoot => formatter.write_str("media path is outside its library root"),
            Self::OutputTooLarge => formatter.write_str("thumbnail output is too large"),
            Self::ProcessIo(error) => write!(formatter, "ffmpeg process: {error}"),
            Self::FfmpegLimit => formatter.write_str("ffmpeg concurrency limit is closed"),
            Self::Storage(error) => write!(formatter, "thumbnail storage: {error}"),
            Self::SymlinkTarget => formatter.write_str("thumbnail path is a symlink"),
            Self::TargetUnavailable => {
                formatter.write_str("thumbnail target is not a regular file")
            }
            Self::Timeout => formatter.write_str("ffmpeg timed out"),
            Self::Write(error) => write!(formatter, "thumbnail write: {error}"),
            Self::SourceNotFile => formatter.write_str("media source is not a regular file"),
        }
    }
}

#[derive(Debug)]
pub enum ThumbnailError {
    Storage(StorageError),
    ImageAvailabilityUnavailable,
}

impl fmt::Display for ThumbnailError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(formatter),
            Self::ImageAvailabilityUnavailable => {
                formatter.write_str("thumbnail image availability could not be checked")
            }
        }
    }
}

impl std::error::Error for ThumbnailError {}

impl From<StorageError> for ThumbnailError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<ThumbnailFileError> for ThumbnailError {
    fn from(_: ThumbnailFileError) -> Self {
        Self::ImageAvailabilityUnavailable
    }
}
