//! Derived image variants for display-sized artwork.
//!
//! The media library remains read-only. Variants are generated on first use and
//! written to the config directory, keyed by the media item, indexed image content tag and
//! target width. Every failure path falls back to the original image.

use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use image::{
    DynamicImage, ExtendedColorType, ImageDecoder, ImageReader, codecs::jpeg::JpegEncoder,
    imageops::FilterType, metadata::Orientation,
};
use tokio::{
    sync::{Semaphore, oneshot},
    time::timeout,
};
use uuid::Uuid;

use crate::application::{
    images::ResolvedImage, metadata_paths::item_relative_directory,
    settings::ImageVariantCacheSettings,
};

pub(crate) const IMAGE_VARIANT_WIDTHS: [u32; 3] = [360, 720, 1280];
const IMAGE_VARIANT_JPEG_QUALITY: u8 = 80;
const IMAGE_VARIANT_RESPONSE_BUDGET: Duration = Duration::from_millis(300);
const IMAGE_VARIANT_GENERATION_TIMEOUT: Duration = Duration::from_secs(10);
const IMAGE_VARIANT_FAILURE_COOLDOWN: Duration = Duration::from_secs(60);
const IMAGE_VARIANT_MAX_SOURCE_BYTES: u64 = 30 * 1024 * 1024;
const IMAGE_VARIANT_MAX_PIXELS: u64 = 40_000_000;
const MAX_FAILURE_ENTRIES: usize = 4096;
const CACHE_VERSION_DIRECTORY: &str = "v1";

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct VariantKey {
    item_directory: PathBuf,
    content_tag: String,
    width: u32,
}

impl VariantKey {
    fn etag(&self) -> String {
        format!("\"{}-w{}\"", self.content_tag, self.width)
    }
}

#[derive(Clone, Debug)]
struct LruEntry {
    bytes: u64,
    last_hit: u64,
}

#[derive(Default)]
struct LruIndex {
    entries: HashMap<VariantKey, LruEntry>,
    total_files: usize,
    total_bytes: u64,
    clock: u64,
}

impl LruIndex {
    fn record_hit(&mut self, key: &VariantKey, bytes: u64) {
        self.clock = self.clock.wrapping_add(1);
        match self.entries.get_mut(key) {
            Some(entry) => {
                self.total_bytes = self
                    .total_bytes
                    .saturating_sub(entry.bytes)
                    .saturating_add(bytes);
                entry.bytes = bytes;
                entry.last_hit = self.clock;
            }
            None => {
                self.entries.insert(
                    key.clone(),
                    LruEntry {
                        bytes,
                        last_hit: self.clock,
                    },
                );
                self.total_files = self.total_files.saturating_add(1);
                self.total_bytes = self.total_bytes.saturating_add(bytes);
            }
        }
    }

    fn eviction_candidates(
        &mut self,
        settings: &ImageVariantCacheSettings,
        keep: &VariantKey,
    ) -> Vec<VariantKey> {
        let max_files = usize::try_from(settings.max_files).unwrap_or(usize::MAX);
        let mut evicted = Vec::new();
        while self.total_files > max_files || self.total_bytes > settings.max_bytes {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(key, entry)| (entry.last_hit, *key == keep))
                .map(|(key, entry)| (key.clone(), entry.clone()))
            else {
                break;
            };
            if self.entries.len() <= 1 && oldest.0 == *keep {
                break;
            }
            self.entries.remove(&oldest.0);
            self.total_files = self.total_files.saturating_sub(1);
            self.total_bytes = self.total_bytes.saturating_sub(oldest.1.bytes);
            evicted.push(oldest.0);
        }
        evicted
    }
}

struct VariantRuntimeSettings {
    settings: ImageVariantCacheSettings,
    semaphore: Arc<Semaphore>,
}

pub(crate) struct ImageVariantService {
    cache_root: PathBuf,
    response_budget: Duration,
    settings: RwLock<VariantRuntimeSettings>,
    initialized: AtomicBool,
    lru: Arc<Mutex<LruIndex>>,
    inflight: Arc<Mutex<HashSet<VariantKey>>>,
    failures: Arc<Mutex<HashMap<VariantKey, Instant>>>,
    generation_count: AtomicUsize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VariantFile {
    pub(crate) path: PathBuf,
    pub(crate) content_length: u64,
    pub(crate) etag: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ImageVariantStats {
    pub(crate) files: usize,
    pub(crate) bytes: u64,
}

impl ImageVariantService {
    pub(crate) fn new(config_dir: &Path, settings: ImageVariantCacheSettings) -> Self {
        let cache_root = config_dir.join("image-cache").join(CACHE_VERSION_DIRECTORY);
        let concurrency = settings.concurrency;
        Self {
            cache_root,
            response_budget: IMAGE_VARIANT_RESPONSE_BUDGET,
            settings: RwLock::new(VariantRuntimeSettings {
                settings,
                semaphore: Arc::new(Semaphore::new(concurrency)),
            }),
            initialized: AtomicBool::new(false),
            lru: Arc::new(Mutex::new(LruIndex::default())),
            inflight: Arc::new(Mutex::new(HashSet::new())),
            failures: Arc::new(Mutex::new(HashMap::new())),
            generation_count: AtomicUsize::new(0),
        }
    }

    pub(crate) fn settings(&self) -> ImageVariantCacheSettings {
        self.settings
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .settings
            .clone()
    }

    pub(crate) fn enabled(&self) -> bool {
        self.settings().enabled
    }

    pub(crate) fn apply_settings(&self, settings: ImageVariantCacheSettings) {
        self.initialized.store(true, Ordering::Release);
        self.replace_settings(settings);
    }

    pub(crate) fn apply_initial_settings(&self, settings: ImageVariantCacheSettings) {
        if !self.initialized.swap(true, Ordering::AcqRel) {
            self.replace_settings(settings);
        }
    }

    fn replace_settings(&self, settings: ImageVariantCacheSettings) {
        let mut runtime = self
            .settings
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if runtime.settings.concurrency != settings.concurrency {
            runtime.semaphore = Arc::new(Semaphore::new(settings.concurrency));
        }
        runtime.settings = settings;
    }

    pub(crate) fn startup_scan(self: &Arc<Self>) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = service.scan_cache().await {
                tracing::warn!(
                    error = %error,
                    "image variant cache startup scan failed"
                );
            }
        });
    }

    pub(crate) fn stats(&self) -> ImageVariantStats {
        let lru = self
            .lru
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ImageVariantStats {
            files: lru.total_files,
            bytes: lru.total_bytes,
        }
    }

    #[cfg(test)]
    fn with_response_budget(mut self, budget: Duration) -> Self {
        self.response_budget = budget;
        self
    }

    #[cfg(test)]
    fn generation_count(&self) -> usize {
        self.generation_count.load(Ordering::Acquire)
    }

    pub(crate) async fn resolve_variant(
        self: &Arc<Self>,
        item_id: &str,
        source: &ResolvedImage,
        width: u32,
    ) -> Option<VariantFile> {
        let (settings, semaphore) = {
            let runtime = self
                .settings
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (runtime.settings.clone(), Arc::clone(&runtime.semaphore))
        };

        if !settings.enabled || !is_supported_image_type(&source.image_type) {
            return None;
        }
        if !IMAGE_VARIANT_WIDTHS.contains(&width) {
            return None;
        }
        let content_tag = valid_content_tag(source.content_tag.as_deref()?)?;
        let (source_width, source_height) = match (source.width, source.height) {
            (Some(width), Some(height)) if width > 0 && height > 0 => (width as u32, height as u32),
            _ => return None,
        };
        if source_width.max(source_height) <= width {
            return None;
        }
        if source.content_length > IMAGE_VARIANT_MAX_SOURCE_BYTES {
            return None;
        }

        let key = VariantKey {
            item_directory: item_relative_directory(item_id).ok()?,
            content_tag,
            width,
        };
        let path = self.variant_path(&key);
        if let Some(file) = self.existing_variant(&key, &path).await {
            return Some(file);
        }
        if self.in_failure_cooldown(&key) {
            return None;
        }

        {
            let mut inflight = self
                .inflight
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !inflight.insert(key.clone()) {
                return None;
            }
        }

        let (sender, receiver) = oneshot::channel();
        let service = Arc::clone(self);
        let source_path = source.path.clone();
        let generation_path = path.clone();
        let etag = key.etag();
        tokio::spawn(async move {
            let result = service
                .generate_for_key(&key, &source_path, &generation_path, semaphore, &settings)
                .await;
            {
                let mut inflight = service
                    .inflight
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                inflight.remove(&key);
            }
            if result.is_err() {
                service.record_failure(&key);
            }
            let _ = sender.send(result);
        });

        match timeout(self.response_budget, receiver).await {
            Ok(Ok(Ok(content_length))) => Some(VariantFile {
                path,
                content_length,
                etag,
            }),
            Ok(Ok(Err(error))) => {
                tracing::debug!(
                    item_id,
                    image_type = %source.image_type,
                    width,
                    error = %error,
                    "image variant generation fell back to the original image"
                );
                None
            }
            _ => None,
        }
    }

    async fn generate_for_key(
        &self,
        key: &VariantKey,
        source_path: &Path,
        target_path: &Path,
        semaphore: Arc<Semaphore>,
        settings: &ImageVariantCacheSettings,
    ) -> Result<u64, VariantGenerationError> {
        let permit = timeout(IMAGE_VARIANT_GENERATION_TIMEOUT, semaphore.acquire())
            .await
            .map_err(|_| VariantGenerationError::QueueTimeout)?
            .map_err(|_| VariantGenerationError::QueueClosed)?;
        let started = Instant::now();
        let source_path = source_path.to_path_buf();
        let target_path = target_path.to_path_buf();
        let generation_target = target_path.clone();
        let width = key.width;
        let generation = tokio::task::spawn_blocking(move || {
            generate_variant_blocking(&source_path, &generation_target, width)
        });
        let result = match timeout(IMAGE_VARIANT_GENERATION_TIMEOUT, generation).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(VariantGenerationError::Join),
            Err(_) => Err(VariantGenerationError::Timeout),
        };
        drop(permit);
        let content_length = result?;
        self.generation_count.fetch_add(1, Ordering::AcqRel);
        self.record_hit(key, content_length, &target_path, settings)
            .await;
        tracing::debug!(
            width = key.width,
            elapsed_ms = started.elapsed().as_millis(),
            bytes = content_length,
            "generated image variant"
        );
        Ok(content_length)
    }

    async fn existing_variant(&self, key: &VariantKey, path: &Path) -> Option<VariantFile> {
        let metadata = tokio::fs::metadata(path).await.ok()?;
        if !metadata.is_file() {
            return None;
        }
        if metadata.len() == 0 {
            let _ = tokio::fs::remove_file(path).await;
            return None;
        }
        let settings = self.settings();
        self.record_hit(key, metadata.len(), path, &settings).await;
        Some(VariantFile {
            path: path.to_path_buf(),
            content_length: metadata.len(),
            etag: key.etag(),
        })
    }

    async fn record_hit(
        &self,
        key: &VariantKey,
        bytes: u64,
        path: &Path,
        settings: &ImageVariantCacheSettings,
    ) {
        let evicted = {
            let mut lru = self
                .lru
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            lru.record_hit(key, bytes);
            lru.eviction_candidates(settings, key)
        };
        for evicted_key in evicted {
            let evicted_path = self.variant_path(&evicted_key);
            if evicted_path != path {
                let _ = tokio::fs::remove_file(&evicted_path).await;
            }
        }
    }

    fn in_failure_cooldown(&self, key: &VariantKey) -> bool {
        let mut failures = self
            .failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(failed_at) = failures.get(key).copied() else {
            return false;
        };
        if failed_at.elapsed() < IMAGE_VARIANT_FAILURE_COOLDOWN {
            return true;
        }
        failures.remove(key);
        false
    }

    fn record_failure(&self, key: &VariantKey) {
        let mut failures = self
            .failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if failures.len() >= MAX_FAILURE_ENTRIES {
            failures.clear();
        }
        failures.insert(key.clone(), Instant::now());
    }

    fn variant_path(&self, key: &VariantKey) -> PathBuf {
        self.cache_root
            .join(&key.item_directory)
            .join(format!("{}-w{}.jpg", key.content_tag, key.width))
    }

    async fn scan_cache(self: &Arc<Self>) -> std::io::Result<()> {
        let settings = self.settings();
        let mut scanned = Vec::new();
        if tokio::fs::symlink_metadata(&self.cache_root)
            .await
            .is_ok_and(|m| m.is_dir())
        {
            let mut shards = tokio::fs::read_dir(&self.cache_root).await?;
            while let Some(shard) = shards.next_entry().await? {
                let shard_name = shard.file_name();
                let shard_name = shard_name.to_string_lossy();
                if !shard.file_type().await?.is_dir()
                    || shard_name.len() != 2
                    || !shard_name
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    continue;
                }
                let mut items = tokio::fs::read_dir(shard.path()).await?;
                while let Some(item) = items.next_entry().await? {
                    let name = item.file_name();
                    let name = name.to_string_lossy();
                    let file_type = item.file_type().await?;
                    if file_type.is_file() {
                        // Old flat variants are disposable and cannot be mapped back to an item.
                        let legacy_variant = variant_file_parts(&name)
                            .is_some_and(|(tag, _)| tag[..2] == *shard_name);
                        if legacy_variant || is_temporary_file(&name) {
                            tokio::fs::remove_file(item.path()).await?;
                        }
                        continue;
                    }
                    let Ok(item_directory) = item_relative_directory(&name) else {
                        continue;
                    };
                    if !file_type.is_dir() || item.path() != self.cache_root.join(&item_directory) {
                        continue;
                    }
                    let mut files = tokio::fs::read_dir(item.path()).await?;
                    while let Some(entry) = files.next_entry().await? {
                        if !entry.file_type().await?.is_file() {
                            continue;
                        }
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        if is_temporary_file(&name) {
                            tokio::fs::remove_file(entry.path()).await?;
                            continue;
                        }
                        let Some((content_tag, width)) = variant_file_parts(&name) else {
                            continue;
                        };
                        let metadata = match entry.metadata().await {
                            Ok(metadata) => metadata,
                            Err(_) => continue,
                        };
                        if metadata.len() == 0 {
                            tokio::fs::remove_file(entry.path()).await?;
                            continue;
                        }
                        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
                        let key = VariantKey {
                            item_directory: item_directory.clone(),
                            content_tag,
                            width,
                        };
                        scanned.push((modified, key, metadata.len()));
                    }
                }
            }
        }
        scanned.sort_by_key(|(modified, _, _)| *modified);
        let evicted = {
            let mut lru = self
                .lru
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for (_, key, bytes) in &scanned {
                lru.record_hit(key, *bytes);
            }
            lru.eviction_candidates(
                &settings,
                &VariantKey {
                    item_directory: PathBuf::new(),
                    content_tag: String::new(),
                    width: 0,
                },
            )
        };
        for key in evicted {
            let _ = tokio::fs::remove_file(self.variant_path(&key)).await;
        }
        let stats = self.stats();
        tracing::info!(
            files = stats.files,
            bytes = stats.bytes,
            "image variant cache scan completed"
        );
        Ok(())
    }
}

fn is_supported_image_type(image_type: &str) -> bool {
    matches!(image_type, "POSTER" | "THUMB" | "FANART")
}

fn valid_content_tag(value: &str) -> Option<String> {
    (value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then(|| value.to_owned())
}

fn is_temporary_file(name: &str) -> bool {
    name.starts_with(".lux-") && name.ends_with(".tmp")
}

fn variant_file_parts(name: &str) -> Option<(String, u32)> {
    let (content_tag, width) = name.split_once("-w")?;
    let width = width.strip_suffix(".jpg")?.parse::<u32>().ok()?;
    if !IMAGE_VARIANT_WIDTHS.contains(&width) {
        return None;
    }
    Some((valid_content_tag(content_tag)?, width))
}

#[derive(Debug)]
enum VariantGenerationError {
    QueueTimeout,
    QueueClosed,
    Timeout,
    Join,
    SourceTooLarge,
    Decode,
    NoResize,
    TooManyPixels,
    Encode,
    Write,
}

impl std::fmt::Display for VariantGenerationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::QueueTimeout => "variant generation queue timed out",
            Self::QueueClosed => "variant generation queue closed",
            Self::Timeout => "variant generation timed out",
            Self::Join => "variant generation task failed",
            Self::SourceTooLarge => "source image exceeds variant limits",
            Self::Decode => "source image could not be decoded",
            Self::NoResize => "source image is already within the target size",
            Self::TooManyPixels => "source image has too many pixels",
            Self::Encode => "variant encoding failed",
            Self::Write => "variant cache write failed",
        })
    }
}

/// Validates decoded dimensions before any pixel data is allocated.
fn check_variant_dimensions(
    (width, height): (u32, u32),
) -> Result<(u32, u32), VariantGenerationError> {
    let pixels = u64::from(width) * u64::from(height);
    if pixels == 0 {
        return Err(VariantGenerationError::Decode);
    }
    if pixels > IMAGE_VARIANT_MAX_PIXELS {
        return Err(VariantGenerationError::TooManyPixels);
    }
    Ok((width, height))
}

fn generate_variant_blocking(
    source_path: &Path,
    target_path: &Path,
    target_width: u32,
) -> Result<u64, VariantGenerationError> {
    let metadata = std::fs::metadata(source_path).map_err(|_| VariantGenerationError::Write)?;
    if !metadata.is_file() || metadata.len() > IMAGE_VARIANT_MAX_SOURCE_BYTES {
        return Err(VariantGenerationError::SourceTooLarge);
    }
    let bytes = std::fs::read(source_path).map_err(|_| VariantGenerationError::Decode)?;
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| VariantGenerationError::Decode)?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| VariantGenerationError::Decode)?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    // Reject oversized images from their header and cap the decoder allocation before any
    // pixel data is materialized; checking after DynamicImage::from_decoder would let a small
    // file with huge declared dimensions allocate hundreds of megabytes first.
    let (width, height) = check_variant_dimensions(decoder.dimensions())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(width);
    limits.max_image_height = Some(height);
    limits.max_alloc = Some(IMAGE_VARIANT_MAX_PIXELS.saturating_mul(4));
    decoder
        .set_limits(limits)
        .map_err(|_| VariantGenerationError::Decode)?;
    let mut image =
        DynamicImage::from_decoder(decoder).map_err(|_| VariantGenerationError::Decode)?;
    image.apply_orientation(orientation);
    if image.width().max(image.height()) <= target_width {
        return Err(VariantGenerationError::NoResize);
    }
    let resized = image.resize(target_width, target_width, FilterType::Lanczos3);
    let rgb = resized.to_rgb8();
    let mut encoded = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut encoded, IMAGE_VARIANT_JPEG_QUALITY);
    encoder
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(|_| VariantGenerationError::Encode)?;

    let parent = target_path.parent().ok_or(VariantGenerationError::Write)?;
    std::fs::create_dir_all(parent).map_err(|_| VariantGenerationError::Write)?;
    let temporary = parent.join(format!(".lux-{}.tmp", Uuid::new_v4()));
    let write_result = (|| -> std::io::Result<()> {
        std::fs::write(&temporary, &encoded)?;
        std::fs::rename(&temporary, target_path)
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return Err(VariantGenerationError::Write);
    }
    Ok(encoded.len() as u64)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use image::{ImageFormat, Rgb, RgbImage};

    use super::*;

    fn settings(enabled: bool) -> ImageVariantCacheSettings {
        ImageVariantCacheSettings {
            enabled,
            max_files: 100,
            max_bytes: 64 * 1024 * 1024,
            concurrency: 2,
        }
    }

    fn service(dir: &Path, settings: ImageVariantCacheSettings) -> Arc<ImageVariantService> {
        // Debug builds encode far slower than release builds, so tests wait for
        // the background generation instead of asserting on the 300 ms budget.
        Arc::new(
            ImageVariantService::new(dir, settings).with_response_budget(Duration::from_secs(30)),
        )
    }

    fn write_jpeg(path: &Path, width: u32, height: u32) {
        let image = RgbImage::from_pixel(width, height, Rgb([20, 40, 80]));
        image.save(path).expect("test jpeg should be written");
    }

    fn source(path: PathBuf, width: u32, height: u32, content_tag: String) -> ResolvedImage {
        let content_length = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        ResolvedImage {
            id: "item".to_owned(),
            image_type: "POSTER".to_owned(),
            path,
            content_type: "image/jpeg",
            content_length,
            etag: "\"original\"".to_owned(),
            content_tag: Some(content_tag),
            width: Some(i64::from(width)),
            height: Some(i64::from(height)),
        }
    }

    #[test]
    fn oversized_dimensions_are_rejected_before_decoding() {
        assert!(matches!(
            check_variant_dimensions((0, 120)),
            Err(VariantGenerationError::Decode)
        ));
        assert!(matches!(
            check_variant_dimensions((8_000, 6_000)),
            Err(VariantGenerationError::TooManyPixels)
        ));
        assert_eq!(
            check_variant_dimensions((1_920, 1_080)).expect("supported dimensions"),
            (1_920, 1_080)
        );
    }

    #[tokio::test]
    async fn generates_a_bounded_variant_and_keeps_aspect_ratio() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_path = temp.path().join("poster.jpg");
        write_jpeg(&source_path, 1536, 1024);
        let cache = temp.path().join("config");
        let service = service(&cache, settings(true));
        let image = source(source_path, 1536, 1024, "a".repeat(64));
        let variant = service.resolve_variant("media-item", &image, 360).await;
        let variant = variant.expect("variant should be generated");
        let metadata_directory =
            crate::application::metadata_paths::library_item_directory(&cache, "media-item")
                .expect("metadata directory");
        let relative = metadata_directory
            .strip_prefix(cache.join("metadata/library"))
            .expect("relative item directory");
        assert_eq!(
            variant.path,
            cache
                .join("image-cache/v1")
                .join(relative)
                .join(format!("{}-w360.jpg", "a".repeat(64)))
        );
        let (width, height) = image::image_dimensions(&variant.path).expect("variant dimensions");
        assert_eq!(width, 360);
        assert!(height.abs_diff(240) <= 1);
        assert_eq!(variant.etag, format!("\"{}-w360\"", "a".repeat(64)));
    }

    #[tokio::test]
    async fn refuses_to_upscale_small_sources() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_path = temp.path().join("small.jpg");
        write_jpeg(&source_path, 300, 200);
        let service = service(&temp.path().join("config"), settings(true));
        let image = source(source_path, 300, 200, "b".repeat(64));
        assert!(
            service
                .resolve_variant("media-item", &image, 360)
                .await
                .is_none()
        );
        assert_eq!(service.generation_count(), 0);
    }

    #[tokio::test]
    async fn concurrent_requests_generate_the_same_key_once() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_path = temp.path().join("poster.jpg");
        write_jpeg(&source_path, 1024, 1536);
        let service = service(&temp.path().join("config"), settings(true));
        let image = Arc::new(source(source_path, 1024, 1536, "c".repeat(64)));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let service = Arc::clone(&service);
            let image = Arc::clone(&image);
            tasks.push(tokio::spawn(async move {
                service
                    .resolve_variant("media-item", &image, 360)
                    .await
                    .is_some()
            }));
        }
        for task in tasks {
            let _ = task.await.expect("task");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(service.generation_count(), 1);
    }

    #[tokio::test]
    async fn fails_closed_for_corrupt_sources_and_does_not_retry_immediately() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_path = temp.path().join("broken.jpg");
        fs::write(&source_path, b"not an image").expect("broken fixture");
        let service = service(&temp.path().join("config"), settings(true));
        let image = source(source_path, 1024, 1536, "d".repeat(64));
        assert!(
            service
                .resolve_variant("media-item", &image, 360)
                .await
                .is_none()
        );
        assert!(
            service
                .resolve_variant("media-item", &image, 360)
                .await
                .is_none()
        );
        assert_eq!(service.generation_count(), 0);
    }

    #[tokio::test]
    async fn evicts_the_oldest_file_when_the_file_limit_is_reached() {
        let temp = tempfile::tempdir().expect("temp dir");
        let first = temp.path().join("first.jpg");
        let second = temp.path().join("second.jpg");
        write_jpeg(&first, 800, 1200);
        write_jpeg(&second, 800, 1200);
        let mut limits = settings(true);
        limits.max_files = 100;
        let service = service(&temp.path().join("config"), limits.clone());
        let first_image = source(first, 800, 1200, "e".repeat(64));
        let second_image = source(second, 800, 1200, "f".repeat(64));
        let first_variant = service
            .resolve_variant("media-item", &first_image, 360)
            .await
            .expect("first variant");
        assert!(first_variant.path.exists());
        limits.max_files = 1;
        // Directly tighten the limit the same way an admin update would.
        service.apply_settings(limits);
        let second_variant = service
            .resolve_variant("media-item", &second_image, 360)
            .await
            .expect("second variant");
        assert!(second_variant.path.exists());
        assert!(!first_variant.path.exists());
    }

    #[tokio::test]
    async fn startup_scan_removes_empty_and_temporary_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        let cache = temp.path().join("config");
        let shard = cache
            .join("image-cache")
            .join(CACHE_VERSION_DIRECTORY)
            .join(item_relative_directory("media-item").expect("item directory"));
        fs::create_dir_all(&shard).expect("shard");
        let tag = "a".repeat(64);
        let empty = shard.join(format!("{tag}-w360.jpg"));
        fs::write(&empty, b"").expect("empty");
        let temporary = shard.join(".lux-test.tmp");
        fs::write(&temporary, b"partial").expect("temporary");
        let service = service(&cache, settings(true));
        service.scan_cache().await.expect("scan");
        assert!(!empty.exists());
        assert!(!temporary.exists());
    }

    #[tokio::test]
    async fn disabled_switch_never_generates() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_path = temp.path().join("poster.jpg");
        write_jpeg(&source_path, 800, 1200);
        let service = service(&temp.path().join("config"), settings(false));
        let image = source(source_path, 800, 1200, "1".repeat(64));
        assert!(
            service
                .resolve_variant("media-item", &image, 360)
                .await
                .is_none()
        );
        assert_eq!(service.generation_count(), 0);
    }

    #[tokio::test]
    async fn unsupported_image_types_keep_the_original() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_path = temp.path().join("poster.jpg");
        write_jpeg(&source_path, 800, 1200);
        let service = service(&temp.path().join("config"), settings(true));
        for image_type in ["LOGO", "ART", "BANNER", "DISC", "WALLPAPER"] {
            let mut image = source(source_path.clone(), 800, 1200, "5".repeat(64));
            image.image_type = image_type.to_owned();
            assert!(
                service
                    .resolve_variant("media-item", &image, 360)
                    .await
                    .is_none(),
                "{image_type} must not generate variants"
            );
        }
        assert_eq!(service.generation_count(), 0);
    }

    #[tokio::test]
    async fn invalid_content_tags_never_touch_the_cache() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_path = temp.path().join("poster.jpg");
        write_jpeg(&source_path, 800, 1200);
        let service = service(&temp.path().join("config"), settings(true));
        let mut image = source(source_path.clone(), 800, 1200, "g".repeat(64));
        image.content_tag = Some("../../etc/passwd".to_owned());
        assert!(
            service
                .resolve_variant("media-item", &image, 360)
                .await
                .is_none()
        );
        image.content_tag = Some("A".repeat(64));
        assert!(
            service
                .resolve_variant("media-item", &image, 360)
                .await
                .is_none()
        );
        assert_eq!(service.generation_count(), 0);
        assert!(!temp.path().join("config").join("image-cache").exists());
    }

    #[tokio::test]
    async fn applies_exif_orientation_before_resizing() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source_path = temp.path().join("rotated.jpg");
        write_jpeg_with_orientation(&source_path, 1200, 600, 6);
        let service = service(&temp.path().join("config"), settings(true));
        let image = source(source_path, 1200, 600, "2".repeat(64));
        let variant = service
            .resolve_variant("media-item", &image, 360)
            .await
            .expect("variant");
        let (width, height) = image::image_dimensions(&variant.path).expect("dimensions");
        assert_eq!(height, 360);
        assert!(width.abs_diff(180) <= 1);
    }

    #[tokio::test]
    async fn byte_limit_evicts_old_entries() {
        let temp = tempfile::tempdir().expect("temp dir");
        let first = temp.path().join("first.jpg");
        let second = temp.path().join("second.jpg");
        write_jpeg(&first, 1600, 1200);
        write_jpeg(&second, 1600, 1200);
        let service = service(&temp.path().join("config"), settings(true));
        let first_image = source(first, 1600, 1200, "3".repeat(64));
        let second_image = source(second, 1600, 1200, "4".repeat(64));
        let first_variant = service
            .resolve_variant("media-item", &first_image, 360)
            .await
            .expect("first variant");
        let mut limit = first_variant.content_length + 1024;
        let mut limits = settings(true);
        limits.max_bytes = limit;
        service.apply_settings(limits.clone());
        let second_variant = service
            .resolve_variant("media-item", &second_image, 360)
            .await
            .expect("second variant");
        assert!(second_variant.content_length > 1024);
        assert!(!first_variant.path.exists());
        assert_eq!(service.stats().files, 1);
        // Leave headroom: the retained entry is the one that is still within
        // the configured budget for normal-sized variants.
        limit = second_variant.content_length;
        limits.max_bytes = limit;
        service.apply_settings(limits);
        assert!(service.stats().bytes >= second_variant.content_length);
    }

    #[tokio::test]
    async fn identical_images_are_isolated_by_media_item_and_width() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("poster.jpg");
        write_jpeg(&path, 1200, 800);
        let cache = service(&temp.path().join("config"), settings(true));
        let image = source(path, 1200, 800, "a".repeat(64));
        let first = cache
            .resolve_variant("first-item", &image, 360)
            .await
            .expect("first");
        let second = cache
            .resolve_variant("second-item", &image, 360)
            .await
            .expect("second");
        let wider = cache
            .resolve_variant("first-item", &image, 720)
            .await
            .expect("wider");
        assert_ne!(first.path, second.path);
        assert_ne!(first.path, wider.path);
        assert_eq!(
            fs::read(&first.path).expect("first bytes"),
            fs::read(&second.path).expect("second bytes")
        );
        assert_eq!(cache.stats().files, 3);
        let mut limits = settings(true);
        limits.max_files = 1;
        cache.apply_settings(limits);
        cache
            .resolve_variant("first-item", &image, 360)
            .await
            .expect("hit");
        assert!(first.path.exists());
        assert!(!second.path.exists());
        assert!(!wider.path.exists());
    }

    #[tokio::test]
    async fn invalid_item_ids_never_touch_the_cache() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("poster.jpg");
        write_jpeg(&path, 1200, 800);
        let cache = service(&temp.path().join("config"), settings(true));
        let image = source(path, 1200, 800, "a".repeat(64));
        for item_id in ["", "..", "../escape", "/absolute", "a/b", "a\\b"] {
            assert!(cache.resolve_variant(item_id, &image, 360).await.is_none());
        }
        assert!(!cache.cache_root.exists());
    }

    #[tokio::test]
    async fn startup_scan_restores_item_entries_and_removes_only_recognized_legacy_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        let config = temp.path().join("config");
        let cache = service(&config, settings(true));
        let item = cache
            .cache_root
            .join(item_relative_directory("media-item").expect("item"));
        fs::create_dir_all(&item).expect("item directory");
        let tag = "a".repeat(64);
        let name = format!("{tag}-w360.jpg");
        let current = item.join(&name);
        fs::write(&current, b"cached jpeg").expect("current");
        let legacy_dir = cache.cache_root.join("aa");
        fs::create_dir_all(&legacy_dir).expect("legacy directory");
        let legacy = legacy_dir.join(&name);
        let unrelated = legacy_dir.join("notes.txt");
        fs::write(&legacy, b"old cache").expect("legacy");
        fs::write(&unrelated, b"unrelated").expect("unrelated");
        let misplaced = legacy_dir.join("media-item");
        fs::create_dir_all(&misplaced).expect("misplaced item");
        fs::write(misplaced.join(&name), b"not indexed").expect("misplaced");
        cache.scan_cache().await.expect("scan");
        assert_eq!(cache.stats().files, 1);
        assert_eq!(cache.stats().bytes, 11);
        assert!(current.exists());
        assert!(!legacy.exists());
        assert!(unrelated.exists());
        let mut limits = settings(true);
        limits.max_files = 0;
        cache.apply_settings(limits);
        cache.scan_cache().await.expect("scan with budget");
        assert!(!current.exists());
        assert_eq!(cache.stats().files, 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_scan_does_not_follow_item_or_file_symlinks() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().expect("temp dir");
        let cache = service(&temp.path().join("config"), settings(true));
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).expect("outside");
        let name = format!("{}-w360.jpg", "a".repeat(64));
        fs::write(outside.join(&name), b"outside").expect("outside cache");
        let item = cache
            .cache_root
            .join(item_relative_directory("media-item").expect("item"));
        fs::create_dir_all(item.parent().expect("shard")).expect("shard");
        symlink(&outside, &item).expect("item symlink");
        cache.scan_cache().await.expect("scan");
        assert_eq!(cache.stats().files, 0);
        fs::remove_file(&item).expect("remove symlink");
        fs::create_dir_all(&item).expect("item directory");
        symlink(outside.join(&name), item.join(&name)).expect("file symlink");
        cache.scan_cache().await.expect("scan");
        assert_eq!(cache.stats().files, 0);
        assert!(outside.join(name).exists());
    }

    fn write_jpeg_with_orientation(path: &Path, width: u32, height: u32, orientation: u16) {
        let image = RgbImage::from_pixel(width, height, Rgb([30, 60, 90]));
        let mut encoded = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut encoded), ImageFormat::Jpeg)
            .expect("jpeg encode");
        let exif = exif_segment(orientation);
        let mut with_exif = Vec::with_capacity(encoded.len() + exif.len());
        with_exif.extend_from_slice(&encoded[..2]);
        with_exif.extend_from_slice(&exif);
        with_exif.extend_from_slice(&encoded[2..]);
        fs::write(path, with_exif).expect("write oriented jpeg");
    }

    fn exif_segment(orientation: u16) -> Vec<u8> {
        // Minimal little-endian TIFF header with one Orientation tag.
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x0112u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&[0, 0]);
        tiff.extend_from_slice(&0u32.to_le_bytes());
        let mut payload = Vec::new();
        payload.extend_from_slice(b"Exif\0\0");
        payload.extend_from_slice(&tiff);
        let mut segment = Vec::new();
        segment.extend_from_slice(&[0xFF, 0xE1]);
        segment.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        segment.extend_from_slice(&payload);
        segment
    }
}
