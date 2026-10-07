use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use serde::{Deserialize, Serialize};
use tokio::{fs, sync::Semaphore, task::JoinSet};

use crate::{
    application::scanner::compute_file_fingerprint,
    application::{
        images::image_content_tag_and_dimensions_from_bytes,
        nfo::{
            LocalNfoMetadataStore, LocalNfoMetadataStoreError, nfo_content_fingerprint,
            parse_local_nfo_projection,
        },
        people::PeopleService,
    },
    domain::ids::LibraryId,
    storage::{
        Database, ItemImageBatchInsert, ItemImageInsert, MediaMetadataUpdate, StorageError,
        StoredItemImage, StoredMediaMetadata, StoredMediaSourcePath, StoredScanLocalMetadataSource,
        StoredSeriesMetadataSource,
    },
};

const LIBRARY_SOURCE_PAGE_SIZE: usize = 500;
pub const DEFAULT_SCAN_JOB_METADATA_BATCH_SIZE: usize = 16;
const MIN_SCAN_JOB_METADATA_BATCH_SIZE: usize = 8;
const MAX_SCAN_JOB_METADATA_BATCH_SIZE: usize = 32;
const LOCAL_IMAGE_READ_CONCURRENCY: usize = 16;
const LOCAL_IMAGE_ITEM_BATCH_SIZE: usize = 16;
static LOCAL_IMAGE_READ_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

fn merged_provider_ids_json(
    current_json: Option<&str>,
    incoming: &BTreeMap<String, String>,
) -> Option<String> {
    if incoming.is_empty() {
        return None;
    }
    let mut merged = current_json
        .and_then(|value| serde_json::from_str::<BTreeMap<String, String>>(value).ok())
        .unwrap_or_default();
    let mut changed = false;
    for (provider, provider_id) in incoming {
        let provider = provider.trim();
        let provider_id = provider_id.trim();
        if provider.is_empty()
            || provider_id.is_empty()
            || merged
                .keys()
                .any(|existing| existing.eq_ignore_ascii_case(provider))
        {
            continue;
        }
        merged.insert(provider.to_ascii_lowercase(), provider_id.to_owned());
        changed = true;
    }
    changed.then(|| serde_json::to_string(&merged).unwrap_or_default())
}

fn local_image_read_permits() -> Arc<Semaphore> {
    LOCAL_IMAGE_READ_PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(LOCAL_IMAGE_READ_CONCURRENCY)))
        .clone()
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NfoMetadata {
    pub title: Option<String>,
    pub original_title: Option<String>,
    pub production_year: Option<i32>,
    pub overview: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MetadataField {
    Title,
    OriginalTitle,
    Overview,
    ProductionYear,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MetadataSource {
    LocalNfo,
    #[serde(alias = "TMDB_LOCALIZED")]
    ScraperLocalized,
    Fallback,
    LockedLocal,
}

impl MetadataSource {
    const fn priority(self) -> u8 {
        match self {
            Self::Fallback => 1,
            Self::ScraperLocalized => 2,
            Self::LocalNfo => 3,
            Self::LockedLocal => 4,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataCandidate {
    pub source: MetadataSource,
    pub metadata: NfoMetadata,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataState {
    pub metadata: NfoMetadata,
    pub provenance: BTreeMap<MetadataField, MetadataSource>,
    pub locked_fields: BTreeSet<MetadataField>,
}

impl MetadataState {
    pub fn from_metadata(metadata: NfoMetadata) -> Self {
        let mut state = Self {
            metadata,
            ..Self::default()
        };
        for field in [
            MetadataField::Title,
            MetadataField::OriginalTitle,
            MetadataField::Overview,
            MetadataField::ProductionYear,
        ] {
            if state.has_value(field) {
                state.provenance.insert(field, MetadataSource::Fallback);
            }
        }
        state
    }

    pub fn from_persisted(
        metadata: NfoMetadata,
        provenance_json: Option<&str>,
        locked_fields_json: Option<&str>,
    ) -> Self {
        let mut state = Self::from_metadata(metadata);
        if let Some(raw) = provenance_json {
            if let Ok(provenance) =
                serde_json::from_str::<BTreeMap<MetadataField, MetadataSource>>(raw)
            {
                state.provenance.extend(provenance);
            } else if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
                let legacy_source = value
                    .get("source")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<MetadataSource>(value).ok());
                if let Some(source) = legacy_source {
                    for field in [
                        MetadataField::Title,
                        MetadataField::OriginalTitle,
                        MetadataField::Overview,
                        MetadataField::ProductionYear,
                    ] {
                        if state.has_value(field) {
                            state.provenance.insert(field, source);
                        }
                    }
                }
            }
        }
        if let Some(raw) = locked_fields_json {
            if let Ok(locked_fields) = serde_json::from_str::<BTreeSet<MetadataField>>(raw) {
                for field in locked_fields {
                    state.lock(field);
                }
            }
        }
        state
    }

    pub fn lock(&mut self, field: MetadataField) {
        self.locked_fields.insert(field);
        self.provenance.insert(field, MetadataSource::LockedLocal);
    }

    pub fn apply_automatic(&mut self, candidate: &MetadataCandidate) {
        for field in [
            MetadataField::Title,
            MetadataField::OriginalTitle,
            MetadataField::Overview,
            MetadataField::ProductionYear,
        ] {
            if self.locked_fields.contains(&field) {
                continue;
            }
            match field {
                MetadataField::Title => {
                    if let Some(value) = non_empty(candidate.metadata.title.as_deref()) {
                        self.apply_text(field, value, candidate.source);
                    }
                }
                MetadataField::OriginalTitle => {
                    if let Some(value) = non_empty(candidate.metadata.original_title.as_deref()) {
                        self.apply_text(field, value, candidate.source);
                    }
                }
                MetadataField::Overview => {
                    if let Some(value) = non_empty(candidate.metadata.overview.as_deref()) {
                        self.apply_text(field, value, candidate.source);
                    }
                }
                MetadataField::ProductionYear => {
                    if let Some(value) = candidate.metadata.production_year
                        && self.can_apply(field, candidate.source)
                    {
                        self.metadata.production_year = Some(value);
                        self.provenance.insert(field, candidate.source);
                    }
                }
            }
        }
    }

    pub fn apply_fill_missing(&mut self, candidate: &MetadataCandidate) {
        for field in [
            MetadataField::Title,
            MetadataField::OriginalTitle,
            MetadataField::Overview,
            MetadataField::ProductionYear,
        ] {
            if self.locked_fields.contains(&field) {
                continue;
            }
            self.apply_value(field, candidate, false);
        }
    }

    pub fn apply_refresh_unlocked(&mut self, candidate: &MetadataCandidate) {
        for field in [
            MetadataField::Title,
            MetadataField::OriginalTitle,
            MetadataField::Overview,
            MetadataField::ProductionYear,
        ] {
            if self.locked_fields.contains(&field) {
                continue;
            }
            self.apply_value(field, candidate, true);
        }
    }

    fn apply_value(&mut self, field: MetadataField, candidate: &MetadataCandidate, force: bool) {
        let source = candidate.source;
        match field {
            MetadataField::Title => {
                if let Some(value) = non_empty(candidate.metadata.title.as_deref())
                    && (force || self.can_fill(field, source))
                {
                    self.metadata.title = Some(value.to_owned());
                    self.provenance.insert(field, source);
                }
            }
            MetadataField::OriginalTitle => {
                if let Some(value) = non_empty(candidate.metadata.original_title.as_deref())
                    && (force || self.can_fill(field, source))
                {
                    self.metadata.original_title = Some(value.to_owned());
                    self.provenance.insert(field, source);
                }
            }
            MetadataField::Overview => {
                if let Some(value) = non_empty(candidate.metadata.overview.as_deref())
                    && (force || self.can_fill(field, source))
                {
                    self.metadata.overview = Some(value.to_owned());
                    self.provenance.insert(field, source);
                }
            }
            MetadataField::ProductionYear => {
                if let Some(value) = candidate.metadata.production_year
                    && (force || self.can_fill(field, source))
                {
                    self.metadata.production_year = Some(value);
                    self.provenance.insert(field, source);
                }
            }
        }
    }

    pub fn provenance_json(&self) -> String {
        serde_json::to_string(&self.provenance).unwrap_or_else(|_| "{}".to_owned())
    }

    pub fn locked_fields_json(&self) -> String {
        serde_json::to_string(&self.locked_fields).unwrap_or_else(|_| "[]".to_owned())
    }

    pub fn has_complete_fill_values(&self, fields: &[MetadataField]) -> bool {
        fields.iter().all(|field| {
            self.locked_fields.contains(field)
                || (self.has_value(*field)
                    && self
                        .provenance
                        .get(field)
                        .is_some_and(|source| *source != MetadataSource::Fallback))
        })
    }

    fn has_value(&self, field: MetadataField) -> bool {
        match field {
            MetadataField::Title => self
                .metadata
                .title
                .as_deref()
                .is_some_and(|v| !v.is_empty()),
            MetadataField::OriginalTitle => self
                .metadata
                .original_title
                .as_deref()
                .is_some_and(|v| !v.is_empty()),
            MetadataField::Overview => self
                .metadata
                .overview
                .as_deref()
                .is_some_and(|v| !v.is_empty()),
            MetadataField::ProductionYear => self.metadata.production_year.is_some(),
        }
    }

    fn can_apply(&self, field: MetadataField, source: MetadataSource) -> bool {
        let current = self
            .provenance
            .get(&field)
            .copied()
            .unwrap_or(MetadataSource::Fallback);
        !self.has_value(field) || source.priority() >= current.priority()
    }

    fn can_fill(&self, field: MetadataField, source: MetadataSource) -> bool {
        if !self.has_value(field) {
            return true;
        }
        let current = self
            .provenance
            .get(&field)
            .copied()
            .unwrap_or(MetadataSource::Fallback);
        source.priority() > current.priority()
    }

    fn apply_text(&mut self, field: MetadataField, value: &str, source: MetadataSource) {
        if !self.can_apply(field, source) {
            return;
        }
        match field {
            MetadataField::Title => self.metadata.title = Some(value.to_owned()),
            MetadataField::OriginalTitle => self.metadata.original_title = Some(value.to_owned()),
            MetadataField::Overview => self.metadata.overview = Some(value.to_owned()),
            MetadataField::ProductionYear => return,
        }
        self.provenance.insert(field, source);
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub fn parse_nfo(bytes: &[u8]) -> Result<NfoMetadata, NfoError> {
    parse_local_nfo_projection(bytes).map(|projection| projection.metadata)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageType {
    Poster,
    Fanart,
    Logo,
    Thumb,
    Banner,
    Disc,
    Art,
    Wallpaper,
}

impl ImageType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Poster => "POSTER",
            Self::Fanart => "FANART",
            Self::Logo => "LOGO",
            Self::Thumb => "THUMB",
            Self::Banner => "BANNER",
            Self::Disc => "DISC",
            Self::Art => "ART",
            Self::Wallpaper => "WALLPAPER",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalImage {
    pub image_type: ImageType,
    pub path: PathBuf,
}

pub fn find_local_images<I, P>(paths: I) -> Vec<LocalImage>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    collect_local_images(paths, None)
}

pub(crate) fn find_local_images_for_media<I, P>(paths: I, media_stem: &str) -> Vec<LocalImage>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    collect_local_images(paths, Some(media_stem))
}

fn collect_local_images<I, P>(paths: I, media_stem: Option<&str>) -> Vec<LocalImage>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let paths = paths
        .into_iter()
        .map(|path| path.as_ref().to_owned())
        .collect::<Vec<_>>();
    let mut images = Vec::new();
    if let Some(media_stem) = media_stem {
        for path in &paths {
            let Some(image_type) = image_type_for_media(path, media_stem) else {
                continue;
            };
            if image_type != ImageType::Fanart
                && images
                    .iter()
                    .any(|image: &LocalImage| image.image_type == image_type)
            {
                continue;
            }
            images.push(LocalImage {
                image_type,
                path: path.to_owned(),
            });
        }
    }
    for path in paths {
        let Some(image_type) = image_type_for(&path) else {
            continue;
        };
        if image_type != ImageType::Fanart
            && images
                .iter()
                .any(|image: &LocalImage| image.image_type == image_type)
        {
            continue;
        }
        images.push(LocalImage { image_type, path });
    }
    images
}

fn image_type_for(path: &Path) -> Option<ImageType> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "webp") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?.to_ascii_lowercase();
    image_type_for_stem(&stem)
}

fn image_type_for_media(path: &Path, media_stem: &str) -> Option<ImageType> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "webp") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    [
        ("poster", ImageType::Poster),
        ("fanart", ImageType::Fanart),
        ("backdrop", ImageType::Fanart),
        ("logo", ImageType::Logo),
        ("clearlogo", ImageType::Logo),
        ("thumb", ImageType::Thumb),
        ("thumbnail", ImageType::Thumb),
        ("banner", ImageType::Banner),
        ("disc", ImageType::Disc),
        ("discart", ImageType::Disc),
        ("art", ImageType::Art),
        ("artwork", ImageType::Art),
        ("wallpaper", ImageType::Wallpaper),
    ]
    .into_iter()
    .find_map(|(suffix, image_type)| {
        let expected = format!("{media_stem}-{suffix}");
        matches_indexed_stem(stem, &expected).then_some(image_type)
    })
}

fn image_type_for_stem(stem: &str) -> Option<ImageType> {
    let stem = indexed_stem_base(stem);
    match stem {
        "poster" => Some(ImageType::Poster),
        "fanart" | "backdrop" => Some(ImageType::Fanart),
        "logo" | "clearlogo" => Some(ImageType::Logo),
        "thumb" | "thumbnail" => Some(ImageType::Thumb),
        "banner" => Some(ImageType::Banner),
        "disc" | "discart" => Some(ImageType::Disc),
        "art" | "artwork" => Some(ImageType::Art),
        "wallpaper" => Some(ImageType::Wallpaper),
        _ => None,
    }
}

fn indexed_stem_base(stem: &str) -> &str {
    if let Some((base, suffix)) = stem.rsplit_once('-')
        && !suffix.is_empty()
        && suffix.chars().all(|character| character.is_ascii_digit())
    {
        return base;
    }
    let digit_start = stem
        .char_indices()
        .find(|(_, character)| character.is_ascii_digit())
        .map(|(index, _)| index);
    if let Some(index) = digit_start
        && index > 0
        && stem[index..]
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return &stem[..index];
    }
    stem
}

fn matches_indexed_stem(stem: &str, base: &str) -> bool {
    if stem.eq_ignore_ascii_case(base) {
        return true;
    }
    let Some(suffix) = stem.get(base.len()..) else {
        return false;
    };
    if !stem[..base.len()].eq_ignore_ascii_case(base) {
        return false;
    }
    let suffix = suffix.strip_prefix('-').unwrap_or(suffix);
    !suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit())
}

#[derive(Debug)]
pub enum NfoError {
    TooLarge,
    TooManyEvents,
    FieldTooLarge,
    DocTypeNotAllowed,
    Unbalanced,
    Xml(String),
    Io(std::io::Error),
}

impl fmt::Display for NfoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => formatter.write_str("NFO exceeds size limit"),
            Self::TooManyEvents => formatter.write_str("NFO exceeds XML event limit"),
            Self::FieldTooLarge => formatter.write_str("NFO field exceeds size limit"),
            Self::DocTypeNotAllowed => formatter.write_str("NFO doctype is not allowed"),
            Self::Unbalanced => formatter.write_str("NFO XML tags are unbalanced"),
            Self::Xml(error) => write!(formatter, "invalid NFO XML: {error}"),
            Self::Io(error) => write!(formatter, "NFO read failed: {error}"),
        }
    }
}

impl std::error::Error for NfoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::TooLarge
            | Self::TooManyEvents
            | Self::FieldTooLarge
            | Self::DocTypeNotAllowed
            | Self::Unbalanced
            | Self::Xml(_) => None,
        }
    }
}

impl From<std::io::Error> for NfoError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone)]
pub struct MetadataEnricher {
    database: Database,
    people: Option<PeopleService>,
    local_nfo: Option<LocalNfoMetadataStore>,
}

#[derive(Default)]
struct SeriesEnrichmentContext {
    last_series_id: Option<String>,
    last_season_id: Option<String>,
    last_episode_id: Option<String>,
    directory_cache: DirectoryPathCache,
}

impl SeriesEnrichmentContext {
    fn tracking_hierarchy() -> Self {
        Self {
            last_series_id: Some(String::new()),
            last_season_id: Some(String::new()),
            last_episode_id: Some(String::new()),
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy)]
enum SeriesEnrichmentMode {
    ImagesAndNfo,
    ImagesOnly,
    NfoOnly,
}

impl SeriesEnrichmentMode {
    const fn process_images(self) -> bool {
        matches!(self, Self::ImagesAndNfo | Self::ImagesOnly)
    }

    const fn process_nfo(self) -> bool {
        matches!(self, Self::ImagesAndNfo | Self::NfoOnly)
    }
}

fn local_metadata_source_path(source: &StoredScanLocalMetadataSource) -> StoredMediaSourcePath {
    StoredMediaSourcePath {
        source_id: source.source_id.clone(),
        item_id: source.item_id.clone(),
        probe_status: source.probe_status.clone(),
        root_path: source.root_path.clone(),
        relative_path: source.relative_path.clone(),
    }
}

fn split_scan_local_metadata_sources(
    sources: &[StoredScanLocalMetadataSource],
) -> (
    Vec<StoredMediaSourcePath>,
    Vec<StoredMediaSourcePath>,
    Vec<StoredSeriesMetadataSource>,
) {
    let mut movies = Vec::new();
    let mut home_videos = Vec::new();
    let mut episodes = Vec::new();
    for source in sources {
        match source.item_type.as_str() {
            "MOVIE" => movies.push(local_metadata_source_path(source)),
            "VIDEO" => home_videos.push(local_metadata_source_path(source)),
            "EPISODE" => {
                if let (Some(series_id), Some(season_id)) =
                    (source.series_id.as_ref(), source.season_id.as_ref())
                {
                    episodes.push(StoredSeriesMetadataSource {
                        series_id: series_id.clone(),
                        season_id: season_id.clone(),
                        episode_id: source.item_id.clone(),
                        season_number: source.season_number,
                        root_path: source.root_path.clone(),
                        relative_path: source.relative_path.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    episodes.sort_by(|left, right| {
        (&left.series_id, &left.season_id, &left.episode_id).cmp(&(
            &right.series_id,
            &right.season_id,
            &right.episode_id,
        ))
    });
    (movies, home_videos, episodes)
}

#[derive(Default)]
struct ScanLocalMetadataNfoSnapshot {
    nfo_paths_by_item: HashMap<String, PathBuf>,
    nfo_errors_by_item: HashMap<String, MetadataError>,
    metadata_by_item: HashMap<String, StoredMediaMetadata>,
}

enum NfoMetadataLookup<'a> {
    OnDemand,
    Snapshot(&'a mut HashMap<String, StoredMediaMetadata>),
}

async fn scan_local_metadata_nfo_paths(
    sources: &[StoredScanLocalMetadataSource],
) -> ScanLocalMetadataNfoSnapshot {
    let mut snapshot = ScanLocalMetadataNfoSnapshot::default();
    let mut seen_series = HashSet::<&str>::new();
    let mut seen_seasons = HashSet::<&str>::new();
    for source in sources {
        let root = Path::new(&source.root_path);
        let media_path = root.join(&source.relative_path);
        match source.item_type.as_str() {
            "MOVIE" => {
                if let Some(path) = find_nfo_path(&media_path).await {
                    snapshot
                        .nfo_paths_by_item
                        .insert(source.item_id.clone(), path);
                }
            }
            "VIDEO" => {
                let path = media_path.with_extension("nfo");
                match fs::try_exists(&path).await {
                    Ok(true) => {
                        snapshot
                            .nfo_paths_by_item
                            .insert(source.item_id.clone(), path);
                    }
                    Ok(false) => {}
                    Err(io_error) => {
                        snapshot.nfo_errors_by_item.insert(
                            source.item_id.clone(),
                            MetadataError::Io {
                                path,
                                source: io_error,
                            },
                        );
                    }
                }
            }
            "EPISODE" => {
                if let Some(path) = find_episode_nfo(&media_path).await {
                    snapshot
                        .nfo_paths_by_item
                        .insert(source.item_id.clone(), path);
                }
                let Some(series_dir) = series_directory(root, &source.relative_path) else {
                    continue;
                };
                if let Some(series_id) = source.series_id.as_deref()
                    && seen_series.insert(series_id)
                    && let Some(path) = find_tvshow_nfo(&series_dir).await
                {
                    snapshot
                        .nfo_paths_by_item
                        .insert(series_id.to_owned(), path);
                }
                if let (Some(season_id), Some(season_number)) =
                    (source.season_id.as_deref(), source.season_number)
                    && seen_seasons.insert(season_id)
                {
                    let season_dir = media_path.parent().unwrap_or(&series_dir);
                    if let Some(path) =
                        find_season_nfo(&series_dir, season_dir, season_number).await
                    {
                        snapshot
                            .nfo_paths_by_item
                            .insert(season_id.to_owned(), path);
                    }
                }
            }
            _ => {}
        }
    }
    snapshot
}

impl MetadataEnricher {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            people: None,
            local_nfo: None,
        }
    }

    pub fn with_people(mut self, people: PeopleService) -> Self {
        self.people = Some(people);
        self
    }

    pub fn with_nfo_store(mut self, local_nfo: LocalNfoMetadataStore) -> Self {
        self.local_nfo = Some(local_nfo);
        self
    }

    pub fn with_movie_nfo_store(self, local_nfo: LocalNfoMetadataStore) -> Self {
        self.with_nfo_store(local_nfo)
    }

    pub async fn enrich_incremental_scan(
        &self,
        scan_job_id: &str,
    ) -> Result<MetadataReport, MetadataError> {
        let mut report = MetadataReport::default();
        let movie_sources = self
            .database
            .list_movie_metadata_sources_for_incremental_scan(scan_job_id)
            .await?;
        self.enrich_movie_sources(movie_sources, &mut report).await;

        let home_video_sources = self
            .database
            .list_home_video_metadata_sources_for_incremental_scan(scan_job_id)
            .await?;
        self.enrich_home_video_sources(home_video_sources, &mut report)
            .await;

        let series_sources = self
            .database
            .list_series_metadata_sources_for_incremental_scan(scan_job_id)
            .await?;
        let mut series_context = SeriesEnrichmentContext::tracking_hierarchy();
        self.enrich_series_sources(
            series_sources,
            &mut report,
            &mut series_context,
            SeriesEnrichmentMode::ImagesAndNfo,
            None,
        )
        .await?;
        Ok(report)
    }

    pub async fn enrich_scan_job(
        &self,
        scan_job_id: &str,
    ) -> Result<MetadataReport, MetadataError> {
        let mut report = MetadataReport::default();
        loop {
            let sources = self
                .database
                .list_scan_job_target_movie_items_page(
                    scan_job_id,
                    LIBRARY_SOURCE_PAGE_SIZE as i64,
                    0,
                )
                .await?;
            if sources.is_empty() {
                break;
            }
            let item_ids = sources
                .iter()
                .map(|source| source.item_id.clone())
                .collect::<Vec<_>>();
            let mut batch_report = MetadataReport::default();
            self.enrich_movie_sources(sources, &mut batch_report).await;
            let failed_item_ids = batch_report.failed_item_ids.clone();
            report.merge(batch_report);
            self.database
                .mark_scan_job_target_stage(
                    scan_job_id,
                    "ITEM",
                    &failed_item_ids,
                    "METADATA",
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
                    "METADATA",
                    "DONE",
                )
                .await?;
        }

        loop {
            let sources = self
                .database
                .list_scan_job_target_home_video_items_page(
                    scan_job_id,
                    LIBRARY_SOURCE_PAGE_SIZE as i64,
                    0,
                )
                .await?;
            if sources.is_empty() {
                break;
            }
            let item_ids = sources
                .iter()
                .map(|source| source.item_id.clone())
                .collect::<Vec<_>>();
            let mut batch_report = MetadataReport::default();
            self.enrich_home_video_sources(sources, &mut batch_report)
                .await;
            let failed_item_ids = batch_report.failed_item_ids.clone();
            report.merge(batch_report);
            self.database
                .mark_scan_job_target_stage(
                    scan_job_id,
                    "ITEM",
                    &failed_item_ids,
                    "METADATA",
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
                    "METADATA",
                    "DONE",
                )
                .await?;
        }

        let mut series_context = SeriesEnrichmentContext::default();
        loop {
            let sources = self
                .database
                .list_scan_job_target_series_items_page(
                    scan_job_id,
                    LIBRARY_SOURCE_PAGE_SIZE as i64,
                    0,
                )
                .await?;
            if sources.is_empty() {
                break;
            }
            let item_ids = sources
                .iter()
                .map(|source| source.episode_id.clone())
                .collect::<Vec<_>>();
            let mut batch_report = MetadataReport::default();
            self.enrich_series_scan_job_sources(
                sources,
                &mut batch_report,
                &mut series_context,
                SeriesEnrichmentMode::ImagesAndNfo,
                None,
            )
            .await;
            let failed_item_ids = batch_report.failed_item_ids.clone();
            report.merge(batch_report);
            self.database
                .mark_scan_job_target_stage(
                    scan_job_id,
                    "ITEM",
                    &failed_item_ids,
                    "METADATA",
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
                    "METADATA",
                    "DONE",
                )
                .await?;
        }
        Ok(report)
    }

    pub async fn enrich_one_scan_job_target(
        &self,
        scan_job_id: &str,
    ) -> Result<Option<MetadataReport>, MetadataError> {
        let report = self.enrich_scan_job_batch(scan_job_id, 1).await?;
        Ok((report.items_processed > 0).then_some(report))
    }

    pub async fn enrich_scan_job_targets(
        &self,
        scan_job_id: &str,
        batch_size: usize,
    ) -> Result<MetadataReport, MetadataError> {
        // The target queries use offset 0 because successful targets move out of
        // the PENDING set when they are marked DONE or FAILED below.
        self.enrich_scan_job_batch(
            scan_job_id,
            batch_size.clamp(
                MIN_SCAN_JOB_METADATA_BATCH_SIZE,
                MAX_SCAN_JOB_METADATA_BATCH_SIZE,
            ),
        )
        .await
    }

    pub(crate) async fn index_scan_local_metadata_batch_images(
        &self,
        filesystem_entry_ids: &[String],
    ) -> Result<ScanLocalMetadataImageBatch, MetadataError> {
        let sources = self
            .database
            .list_scan_local_metadata_sources(filesystem_entry_ids)
            .await?;
        let (movies, _, episodes) = split_scan_local_metadata_sources(&sources);
        let mut report = MetadataReport::default();
        let mut directory_cache = DirectoryPathCache::default();
        if let Some((first_movie, remaining_movies)) = movies.split_first() {
            // Publish the first poster as soon as it is ready. Later pages use the
            // bounded multi-item transaction to reduce SQLite/PostgreSQL write churn.
            self.index_scan_local_movie_image_page(
                std::slice::from_ref(first_movie),
                &mut directory_cache,
                &mut report,
            )
            .await?;
            for page in remaining_movies.chunks(LOCAL_IMAGE_ITEM_BATCH_SIZE) {
                self.index_scan_local_movie_image_page(page, &mut directory_cache, &mut report)
                    .await?;
            }
        }
        let mut series_context = SeriesEnrichmentContext::tracking_hierarchy();
        self.enrich_series_scan_job_sources(
            episodes,
            &mut report,
            &mut series_context,
            SeriesEnrichmentMode::ImagesOnly,
            None,
        )
        .await;
        Ok(ScanLocalMetadataImageBatch { report, sources })
    }

    async fn index_scan_local_movie_image_page(
        &self,
        sources: &[StoredMediaSourcePath],
        directory_cache: &mut DirectoryPathCache,
        report: &mut MetadataReport,
    ) -> Result<(), MetadataError> {
        let mut items = Vec::with_capacity(sources.len());
        for source in sources {
            report.items_processed += 1;
            if let Some(item) =
                prepare_scan_local_movie_image_batch_item(source, directory_cache, report).await
            {
                items.push(item);
            }
        }
        if !items.is_empty() {
            report.images_found += self
                .database
                .insert_item_images_batch_at_indices(&items)
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn enrich_scan_local_metadata_batch_nfo(
        &self,
        source_snapshot: Vec<StoredScanLocalMetadataSource>,
        excluded_item_ids: &[String],
    ) -> Result<ScanLocalMetadataNfoBatch, MetadataError> {
        let excluded_item_ids = excluded_item_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut sources = source_snapshot
            .into_iter()
            .filter(|source| !excluded_item_ids.contains(source.item_id.as_str()))
            .collect::<Vec<_>>();
        let requested_source_identities = sources
            .iter()
            .map(|source| (source.item_id.clone(), source.source_id.clone()))
            .collect::<Vec<_>>();
        let current_item_ids = self
            .database
            .list_current_scan_local_metadata_item_ids(&requested_source_identities)
            .await?
            .into_iter()
            .collect::<HashSet<_>>();
        sources.retain(|source| current_item_ids.contains(&source.item_id));
        let mut nfo_snapshot = scan_local_metadata_nfo_paths(&sources).await;
        let mut metadata_item_ids = nfo_snapshot
            .nfo_paths_by_item
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        metadata_item_ids.sort_unstable();
        nfo_snapshot.metadata_by_item = self
            .database
            .list_media_item_metadata_by_ids(&metadata_item_ids)
            .await?;
        let source_identities = sources
            .iter()
            .map(|source| (source.item_id.clone(), source.source_id.clone()))
            .collect();
        let (movies, home_videos, episodes) = split_scan_local_metadata_sources(&sources);
        let mut report = MetadataReport::default();
        for source in movies {
            report.items_processed += 1;
            if let Some(nfo_path) = nfo_snapshot.nfo_paths_by_item.remove(&source.item_id) {
                self.enrich_nfo_item_best_effort_with_metadata(
                    &mut report,
                    &source.item_id,
                    &nfo_path,
                    NfoMetadataLookup::Snapshot(&mut nfo_snapshot.metadata_by_item),
                )
                .await;
            }
        }
        self.enrich_home_video_sources_with_snapshot(home_videos, &mut report, &mut nfo_snapshot)
            .await;
        let mut series_context = SeriesEnrichmentContext::tracking_hierarchy();
        self.enrich_series_scan_job_sources(
            episodes,
            &mut report,
            &mut series_context,
            SeriesEnrichmentMode::NfoOnly,
            Some(&mut nfo_snapshot),
        )
        .await;
        Ok(ScanLocalMetadataNfoBatch {
            report,
            source_identities,
        })
    }

    async fn enrich_scan_job_batch(
        &self,
        scan_job_id: &str,
        limit: usize,
    ) -> Result<MetadataReport, MetadataError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut report = MetadataReport::default();
        let movie_sources = self
            .database
            .list_scan_job_target_movie_items_page(scan_job_id, limit, 0)
            .await?;
        if !movie_sources.is_empty() {
            let item_ids = movie_sources
                .iter()
                .map(|source| source.item_id.clone())
                .collect::<Vec<_>>();
            let mut batch_report = MetadataReport::default();
            self.enrich_movie_sources(movie_sources, &mut batch_report)
                .await;
            let failed_item_ids = batch_report.failed_item_ids.clone();
            report.merge(batch_report);
            self.database
                .mark_scan_job_target_stage(
                    scan_job_id,
                    "ITEM",
                    &failed_item_ids,
                    "METADATA",
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
                    "METADATA",
                    "DONE",
                )
                .await?;
            report
                .locally_enriched_item_ids
                .extend(completed_item_ids.iter().cloned());
            return Ok(report);
        }

        let home_video_sources = self
            .database
            .list_scan_job_target_home_video_items_page(scan_job_id, limit, 0)
            .await?;
        if !home_video_sources.is_empty() {
            let item_ids = home_video_sources
                .iter()
                .map(|source| source.item_id.clone())
                .collect::<Vec<_>>();
            let mut batch_report = MetadataReport::default();
            self.enrich_home_video_sources(home_video_sources, &mut batch_report)
                .await;
            let failed_item_ids = batch_report.failed_item_ids.clone();
            report.merge(batch_report);
            self.database
                .mark_scan_job_target_stage(
                    scan_job_id,
                    "ITEM",
                    &failed_item_ids,
                    "METADATA",
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
                    "METADATA",
                    "DONE",
                )
                .await?;
            report
                .locally_enriched_item_ids
                .extend(completed_item_ids.iter().cloned());
            return Ok(report);
        }

        let series_sources = self
            .database
            .list_scan_job_target_series_items_page(scan_job_id, limit, 0)
            .await?;
        if series_sources.is_empty() {
            return Ok(report);
        }
        let item_ids = series_sources
            .iter()
            .map(|source| source.episode_id.clone())
            .collect::<Vec<_>>();
        let mut batch_report = MetadataReport::default();
        let mut series_context = SeriesEnrichmentContext::default();
        self.enrich_series_scan_job_sources(
            series_sources,
            &mut batch_report,
            &mut series_context,
            SeriesEnrichmentMode::ImagesAndNfo,
            None,
        )
        .await;
        let failed_item_ids = batch_report.failed_item_ids.clone();
        report.merge(batch_report);
        self.database
            .mark_scan_job_target_stage(scan_job_id, "ITEM", &failed_item_ids, "METADATA", "FAILED")
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
                "METADATA",
                "DONE",
            )
            .await?;
        report
            .locally_enriched_item_ids
            .extend(completed_item_ids.iter().cloned());
        Ok(report)
    }

    async fn enrich_series_scan_job_sources(
        &self,
        sources: Vec<StoredSeriesMetadataSource>,
        report: &mut MetadataReport,
        context: &mut SeriesEnrichmentContext,
        mode: SeriesEnrichmentMode,
        nfo_snapshot: Option<&mut ScanLocalMetadataNfoSnapshot>,
    ) {
        if let Err(error) = self
            .enrich_series_sources(sources, report, context, mode, nfo_snapshot)
            .await
        {
            tracing::warn!(%error, "local series metadata batch failed");
        }
    }

    pub async fn enrich_movie_library(
        &self,
        library_id: LibraryId,
    ) -> Result<MetadataReport, MetadataError> {
        let mut report = MetadataReport::default();
        let library_id = library_id.to_string();
        let mut offset = 0_i64;
        loop {
            let sources = self
                .database
                .list_movie_metadata_sources_page(
                    &library_id,
                    LIBRARY_SOURCE_PAGE_SIZE as i64,
                    offset,
                )
                .await?;
            let last_page = sources.len() < LIBRARY_SOURCE_PAGE_SIZE;
            self.enrich_movie_sources(sources, &mut report).await;
            if last_page {
                break;
            }
            offset = offset.saturating_add(LIBRARY_SOURCE_PAGE_SIZE as i64);
        }
        Ok(report)
    }

    async fn enrich_movie_sources(
        &self,
        sources: Vec<StoredMediaSourcePath>,
        report: &mut MetadataReport,
    ) {
        let mut directory_cache = DirectoryPathCache::default();
        for source_page in sources.chunks(LOCAL_IMAGE_ITEM_BATCH_SIZE) {
            let mut image_items = Vec::with_capacity(source_page.len());
            for source in source_page {
                report.items_processed += 1;
                let media_path = PathBuf::from(&source.root_path).join(&source.relative_path);
                match self.enrich_movie_nfo(&source.item_id, &media_path).await {
                    Ok(nfo_report) => {
                        let failed = nfo_report.nfo_failed > 0;
                        report.merge(nfo_report);
                        if failed {
                            report.mark_item_failed(&source.item_id);
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            item_id = %source.item_id,
                            %error,
                            "local movie NFO failed; continuing with images and remaining items"
                        );
                        report.nfo_failed += 1;
                        report.mark_item_error(&source.item_id, &error);
                    }
                }

                if let Some(item) =
                    prepare_scan_local_movie_image_batch_item(source, &mut directory_cache, report)
                        .await
                {
                    image_items.push(item);
                }
            }

            if image_items.is_empty() {
                continue;
            }
            match self
                .database
                .insert_item_images_batch_at_indices(&image_items)
                .await
            {
                Ok(images_found) => report.images_found += images_found,
                Err(batch_error) => {
                    tracing::warn!(
                        item_count = image_items.len(),
                        %batch_error,
                        "local movie image page failed; retrying items individually"
                    );
                    for item in &image_items {
                        match self
                            .database
                            .insert_item_images_batch_at_indices(std::slice::from_ref(item))
                            .await
                        {
                            Ok(images_found) => report.images_found += images_found,
                            Err(error) => {
                                tracing::warn!(
                                    item_id = %item.item_id,
                                    %error,
                                    "local movie image registration failed"
                                );
                                report.mark_item_failed(&item.item_id);
                            }
                        }
                    }
                }
            }
        }
    }

    async fn enrich_home_video_sources(
        &self,
        sources: Vec<StoredMediaSourcePath>,
        report: &mut MetadataReport,
    ) {
        for source in sources {
            report.items_processed += 1;
            let media_path = PathBuf::from(&source.root_path).join(&source.relative_path);
            let nfo_path = media_path.with_extension("nfo");
            match fs::try_exists(&nfo_path).await {
                Ok(true) => {
                    self.enrich_nfo_item_best_effort_with_metadata(
                        report,
                        &source.item_id,
                        &nfo_path,
                        NfoMetadataLookup::OnDemand,
                    )
                    .await;
                }
                Ok(false) => {}
                Err(io_error) => {
                    let error = MetadataError::Io {
                        path: nfo_path,
                        source: io_error,
                    };
                    tracing::warn!(
                        item_id = %source.item_id,
                        %error,
                        "local home video NFO failed; continuing with remaining items"
                    );
                    report.nfo_failed += 1;
                    report.mark_item_error(&source.item_id, &error);
                }
            }
        }
    }

    async fn enrich_home_video_sources_with_snapshot(
        &self,
        sources: Vec<StoredMediaSourcePath>,
        report: &mut MetadataReport,
        snapshot: &mut ScanLocalMetadataNfoSnapshot,
    ) {
        for source in sources {
            report.items_processed += 1;
            if let Some(error) = snapshot.nfo_errors_by_item.remove(&source.item_id) {
                tracing::warn!(
                    item_id = %source.item_id,
                    %error,
                    "local home video NFO could not be checked"
                );
                report.nfo_failed += 1;
                report.mark_item_error(&source.item_id, &error);
                continue;
            }
            if let Some(nfo_path) = snapshot.nfo_paths_by_item.remove(&source.item_id) {
                self.enrich_nfo_item_best_effort_with_metadata(
                    report,
                    &source.item_id,
                    &nfo_path,
                    NfoMetadataLookup::Snapshot(&mut snapshot.metadata_by_item),
                )
                .await;
            }
        }
    }

    pub async fn enrich_mixed_library(
        &self,
        library_id: LibraryId,
    ) -> Result<MetadataReport, MetadataError> {
        let mut report = self.enrich_movie_library(library_id).await?;
        report.merge(self.enrich_series_library(library_id).await?);
        Ok(report)
    }

    async fn enrich_movie_nfo(
        &self,
        item_id: &str,
        media_path: &Path,
    ) -> Result<MetadataReport, MetadataError> {
        let Some(nfo_path) = find_nfo_path(media_path).await else {
            return Ok(MetadataReport::default());
        };
        self.enrich_nfo_item(item_id, &nfo_path).await
    }

    pub async fn enrich_series_library(
        &self,
        library_id: LibraryId,
    ) -> Result<MetadataReport, MetadataError> {
        let mut report = MetadataReport::default();
        let library_id = library_id.to_string();
        let mut offset = 0_i64;
        let mut series_context = SeriesEnrichmentContext::default();
        loop {
            let sources = self
                .database
                .list_series_metadata_sources_page(
                    &library_id,
                    LIBRARY_SOURCE_PAGE_SIZE as i64,
                    offset,
                )
                .await?;
            let last_page = sources.len() < LIBRARY_SOURCE_PAGE_SIZE;
            self.enrich_series_sources(
                sources,
                &mut report,
                &mut series_context,
                SeriesEnrichmentMode::ImagesAndNfo,
                None,
            )
            .await?;
            if last_page {
                break;
            }
            offset = offset.saturating_add(LIBRARY_SOURCE_PAGE_SIZE as i64);
        }
        Ok(report)
    }

    async fn enrich_series_sources(
        &self,
        sources: Vec<StoredSeriesMetadataSource>,
        report: &mut MetadataReport,
        context: &mut SeriesEnrichmentContext,
        mode: SeriesEnrichmentMode,
        mut nfo_snapshot: Option<&mut ScanLocalMetadataNfoSnapshot>,
    ) -> Result<(), MetadataError> {
        let process_images = mode.process_images();
        let process_nfo = mode.process_nfo();
        let mut image_candidates = Vec::new();
        let mut queued_image_items = HashSet::new();
        for source in sources {
            report.items_processed += 1;
            let root = PathBuf::from(&source.root_path);
            let media_path = root.join(&source.relative_path);
            let Some(series_dir) = series_directory(&root, &source.relative_path) else {
                continue;
            };
            let series_paths = if process_images {
                match context.directory_cache.get(&series_dir).await {
                    Ok(paths) => Some(paths),
                    Err(error) => {
                        tracing::warn!(
                            item_id = %source.episode_id,
                            %error,
                            "local series directory could not be read"
                        );
                        report.mark_item_error(&source.episode_id, &error);
                        continue;
                    }
                }
            } else {
                None
            };
            let new_series = context
                .last_series_id
                .as_deref()
                .is_none_or(|id| id != source.series_id.as_str());
            if new_series {
                let nfo_path = if process_nfo {
                    match nfo_snapshot.as_deref_mut() {
                        Some(snapshot) => snapshot.nfo_paths_by_item.remove(&source.series_id),
                        None => find_tvshow_nfo(&series_dir).await,
                    }
                } else {
                    None
                };
                if let Some(nfo_path) = nfo_path {
                    self.enrich_nfo_item_best_effort_with_metadata(
                        report,
                        &source.series_id,
                        &nfo_path,
                        match nfo_snapshot.as_deref_mut() {
                            Some(snapshot) => {
                                NfoMetadataLookup::Snapshot(&mut snapshot.metadata_by_item)
                            }
                            None => NfoMetadataLookup::OnDemand,
                        },
                    )
                    .await;
                }
                if process_images && let Some(series_paths) = series_paths.as_ref() {
                    let images = find_series_images(series_paths, None);
                    if !images.is_empty() && queued_image_items.insert(source.series_id.clone()) {
                        image_candidates.push((source.series_id.clone(), images));
                    }
                }
                if let Some(last_series_id) = context.last_series_id.as_mut() {
                    *last_series_id = source.series_id.clone();
                }
                if let Some(last_season_id) = context.last_season_id.as_mut() {
                    last_season_id.clear();
                }
                if let Some(last_episode_id) = context.last_episode_id.as_mut() {
                    last_episode_id.clear();
                }
            }

            let season_number = source.season_number.unwrap_or_default();
            let season_dir = media_path.parent().unwrap_or(&series_dir);
            let new_season = context
                .last_season_id
                .as_deref()
                .is_none_or(|id| id != source.season_id.as_str());
            let mut season_paths = series_paths
                .as_ref()
                .map(|paths| paths.as_ref().clone())
                .unwrap_or_default();
            if process_images && season_dir != series_dir {
                let directory_paths = match context.directory_cache.get(season_dir).await {
                    Ok(paths) => paths,
                    Err(error) => {
                        tracing::warn!(
                            item_id = %source.episode_id,
                            %error,
                            "local season directory could not be read"
                        );
                        report.mark_item_error(&source.episode_id, &error);
                        continue;
                    }
                };
                season_paths = season_paths
                    .iter()
                    .filter(|path| is_prefixed_season_image(path, season_number))
                    .cloned()
                    .collect();
                season_paths.extend(directory_paths.iter().cloned());
            }
            if new_season {
                let nfo_path = if process_nfo {
                    match nfo_snapshot.as_deref_mut() {
                        Some(snapshot) => snapshot.nfo_paths_by_item.remove(&source.season_id),
                        None => find_season_nfo(&series_dir, season_dir, season_number).await,
                    }
                } else {
                    None
                };
                if let Some(nfo_path) = nfo_path {
                    self.enrich_nfo_item_best_effort_with_metadata(
                        report,
                        &source.season_id,
                        &nfo_path,
                        match nfo_snapshot.as_deref_mut() {
                            Some(snapshot) => {
                                NfoMetadataLookup::Snapshot(&mut snapshot.metadata_by_item)
                            }
                            None => NfoMetadataLookup::OnDemand,
                        },
                    )
                    .await;
                }
                if process_images {
                    let images = find_series_images(&season_paths, Some(season_number));
                    if !images.is_empty() && queued_image_items.insert(source.season_id.clone()) {
                        image_candidates.push((source.season_id.clone(), images));
                    }
                }
                if let Some(last_season_id) = context.last_season_id.as_mut() {
                    *last_season_id = source.season_id.clone();
                }
                if let Some(last_episode_id) = context.last_episode_id.as_mut() {
                    last_episode_id.clear();
                }
            }

            let new_episode = context
                .last_episode_id
                .as_deref()
                .is_none_or(|id| id != source.episode_id.as_str());
            if new_episode {
                let nfo_path = if process_nfo {
                    match nfo_snapshot.as_deref_mut() {
                        Some(snapshot) => snapshot.nfo_paths_by_item.remove(&source.episode_id),
                        None => find_episode_nfo(&media_path).await,
                    }
                } else {
                    None
                };
                if let Some(nfo_path) = nfo_path {
                    self.enrich_nfo_item_best_effort_with_metadata(
                        report,
                        &source.episode_id,
                        &nfo_path,
                        match nfo_snapshot.as_deref_mut() {
                            Some(snapshot) => {
                                NfoMetadataLookup::Snapshot(&mut snapshot.metadata_by_item)
                            }
                            None => NfoMetadataLookup::OnDemand,
                        },
                    )
                    .await;
                }
                if process_images {
                    let images = find_episode_images(&season_paths, &media_path);
                    if !images.is_empty() && queued_image_items.insert(source.episode_id.clone()) {
                        image_candidates.push((source.episode_id.clone(), images));
                    }
                }
                if let Some(last_episode_id) = context.last_episode_id.as_mut() {
                    *last_episode_id = source.episode_id.clone();
                }
            }
        }
        self.index_images_batch(image_candidates, report).await;
        Ok(())
    }

    async fn enrich_nfo_item_best_effort_with_metadata(
        &self,
        report: &mut MetadataReport,
        item_id: &str,
        nfo_path: &Path,
        metadata: NfoMetadataLookup<'_>,
    ) {
        let enriched = match metadata {
            NfoMetadataLookup::OnDemand => self.enrich_nfo_item(item_id, nfo_path).await,
            NfoMetadataLookup::Snapshot(metadata_by_item) => {
                self.enrich_nfo_item_with_metadata(
                    item_id,
                    nfo_path,
                    metadata_by_item.remove(item_id),
                )
                .await
            }
        };
        match enriched {
            Ok(nfo_report) => {
                let failed = nfo_report.nfo_failed > 0;
                report.merge(nfo_report);
                if failed {
                    report.mark_item_failed(item_id);
                }
            }
            Err(error) => {
                if let MetadataError::ConflictingMovieIdentity {
                    conflicting_item_id,
                    ..
                } = &error
                {
                    tracing::warn!(
                        item_id,
                        conflicting_item_id,
                        %error,
                        "local NFO enrichment failed; continuing with remaining metadata"
                    );
                } else {
                    tracing::warn!(
                        item_id,
                        %error,
                        "local NFO enrichment failed; continuing with remaining metadata"
                    );
                }
                report.nfo_failed += 1;
                report.mark_item_error(item_id, &error);
            }
        }
    }

    async fn enrich_nfo_item(
        &self,
        item_id: &str,
        nfo_path: &Path,
    ) -> Result<MetadataReport, MetadataError> {
        let metadata = self.database.find_media_item_metadata(item_id).await?;
        self.enrich_nfo_item_with_metadata(item_id, nfo_path, metadata)
            .await
    }

    async fn enrich_nfo_item_with_metadata(
        &self,
        item_id: &str,
        nfo_path: &Path,
        metadata: Option<StoredMediaMetadata>,
    ) -> Result<MetadataReport, MetadataError> {
        let mut report = MetadataReport::default();
        let fingerprint = nfo_fingerprint(nfo_path).await.ok();
        let already_checked = fingerprint.as_deref().is_some_and(|fingerprint| {
            metadata
                .as_ref()
                .and_then(|metadata| metadata.metadata_fingerprint.as_deref())
                == Some(fingerprint)
        });
        let cached_nfo = if let Some(local_nfo) = &self.local_nfo {
            local_nfo
                .read_item_if_usable(item_id)
                .await
                .map_err(MetadataError::NfoCache)?
        } else {
            None
        };
        let rich_cache_missing = self.local_nfo.is_some() && cached_nfo.is_none();
        let actor_relation_missing = if let Some(people) = &self.people {
            match people.nfo_relation_snapshot_exists(item_id).await {
                Ok(exists) => !exists,
                Err(error) => {
                    tracing::warn!(
                        item_id,
                        %error,
                        "local actor relation could not be checked; retrying NFO actor sync"
                    );
                    true
                }
            }
        } else {
            false
        };
        if already_checked && !rich_cache_missing && !actor_relation_missing {
            if let (Some(metadata), Some(details)) = (metadata.as_ref(), cached_nfo.as_ref())
                && local_nfo_defaults_missing(metadata, details)
            {
                self.database
                    .repair_local_nfo_defaults(
                        item_id,
                        &details.provider_ids,
                        local_nfo_premiere_date(details),
                    )
                    .await?;
            }
            report.nfo_skipped = 1;
            return Ok(report);
        }
        let bytes = match fs::read(nfo_path).await {
            Ok(bytes) => bytes,
            Err(_) => {
                report.nfo_failed = 1;
                return Ok(report);
            }
        };
        let projection = match parse_local_nfo_projection(&bytes) {
            Ok(projection) => projection,
            Err(_) => {
                if let Some(local_nfo) = &self.local_nfo {
                    local_nfo
                        .clear_item(item_id)
                        .await
                        .map_err(MetadataError::NfoCache)?;
                }
                if let Some(fingerprint) = fingerprint.as_deref() {
                    self.database
                        .mark_media_item_metadata_checked(item_id, fingerprint)
                        .await?;
                }
                report.nfo_failed = 1;
                return Ok(report);
            }
        };
        let current = metadata;
        if let Some(current) = current.as_ref() {
            let mut state = MetadataState::from_persisted(
                NfoMetadata {
                    title: Some(current.title.clone()),
                    original_title: current.original_title.clone(),
                    overview: current.overview.clone(),
                    production_year: current
                        .production_year
                        .and_then(|year| i32::try_from(year).ok()),
                },
                current.provenance_json.as_deref(),
                current.locked_fields_json.as_deref(),
            );
            state.apply_automatic(&MetadataCandidate {
                source: MetadataSource::LocalNfo,
                metadata: projection.metadata.clone(),
            });
            let title_changed = state.metadata.title.as_deref() != Some(current.title.as_str());
            let year_changed =
                state.metadata.production_year.map(i64::from) != current.production_year;
            if (title_changed || year_changed)
                && let Some(production_year) = state.metadata.production_year
                && let Some(conflicting_item_id) = self
                    .database
                    .movie_metadata_identity_conflict(
                        item_id,
                        &state
                            .metadata
                            .title
                            .as_deref()
                            .unwrap_or(&current.title)
                            .to_lowercase(),
                        i64::from(production_year),
                    )
                    .await?
            {
                return Err(MetadataError::ConflictingMovieIdentity {
                    item_id: item_id.to_owned(),
                    conflicting_item_id,
                });
            }
        }
        let source_fingerprint = nfo_content_fingerprint(&bytes);
        let local_rating = projection.details.rating;
        if let Some(local_nfo) = &self.local_nfo {
            let current = local_nfo
                .is_current(item_id, &source_fingerprint)
                .await
                .map_err(MetadataError::NfoCache)?;
            if !current {
                local_nfo
                    .write_item(item_id, &source_fingerprint, &projection.details)
                    .await
                    .map_err(MetadataError::NfoCache)?;
            }
        }
        let provider_ids_json = merged_provider_ids_json(
            current
                .as_ref()
                .and_then(|metadata| metadata.provider_ids_json.as_deref()),
            &projection.details.provider_ids,
        );
        if let Some(people) = &self.people {
            let relation_current = match people
                .item_actor_relation_is_current(item_id, &source_fingerprint)
                .await
            {
                Ok(current) => current,
                Err(error) => {
                    tracing::warn!(
                        item_id,
                        %error,
                        "local actor relation could not be checked; retrying NFO actor sync"
                    );
                    false
                }
            };
            if !relation_current {
                match people
                    .persist_nfo_item_actors(
                        item_id,
                        "tmdb",
                        &projection.actors,
                        &source_fingerprint,
                    )
                    .await
                {
                    Ok(actor_report) => {
                        if !actor_report.pending_assets.is_empty() {
                            tracing::warn!(
                                item_id,
                                pending_actors = actor_report.pending_assets.len(),
                                "local actor relation saved with pending person assets"
                            );
                        }
                    }
                    Err(error) => {
                        tracing::warn!(item_id, %error, "local NFO actors could not be persisted; relation remains retryable");
                    }
                }
            }
        }
        // Provider IDs, the local NFO cache, and actor relations are stored separately from
        // the metadata columns above, so this enrichment pass can reuse its initial snapshot
        // instead of issuing the same full metadata read a second time.
        if let Some(fingerprint) = fingerprint.as_deref()
            && let Some(current) = current.as_ref()
        {
            let mut state = MetadataState::from_persisted(
                NfoMetadata {
                    title: Some(current.title.clone()),
                    original_title: current.original_title.clone(),
                    overview: current.overview.clone(),
                    production_year: current
                        .production_year
                        .and_then(|year| i32::try_from(year).ok()),
                },
                current.provenance_json.as_deref(),
                current.locked_fields_json.as_deref(),
            );
            state.apply_automatic(&MetadataCandidate {
                source: MetadataSource::LocalNfo,
                metadata: projection.metadata,
            });
            let provenance_json = state.provenance_json();
            let locked_fields_json = state.locked_fields_json();
            self.database
                .update_media_item_metadata(MediaMetadataUpdate {
                    item_id,
                    title: state.metadata.title.as_deref().unwrap_or(&current.title),
                    original_title: state.metadata.original_title.as_deref(),
                    overview: state.metadata.overview.as_deref(),
                    production_year: state.metadata.production_year.map(i64::from),
                    premiere_date: local_nfo_premiere_date(&projection.details),
                    rating: local_rating,
                    rating_source: local_rating.map(|_| "NFO"),
                    provider_ids_json: provider_ids_json.as_deref(),
                    metadata_fingerprint: fingerprint,
                    provenance_json: &provenance_json,
                    locked_fields_json: &locked_fields_json,
                })
                .await?;
        }
        report.nfo_loaded = 1;
        Ok(report)
    }

    async fn index_images_batch(
        &self,
        candidates: Vec<(String, Vec<LocalImage>)>,
        report: &mut MetadataReport,
    ) {
        for page in candidates.chunks(LOCAL_IMAGE_ITEM_BATCH_SIZE) {
            let item_ids = page
                .iter()
                .map(|(item_id, _)| item_id.clone())
                .collect::<Vec<_>>();
            let indexed_images = match self.database.list_item_images_by_ids(&item_ids).await {
                Ok(images) => images,
                Err(error) => {
                    tracing::warn!(
                        item_count = page.len(),
                        %error,
                        "local series image page could not read existing images; retrying items individually"
                    );
                    self.index_images_individually(page, report).await;
                    continue;
                }
            };

            let mut inserts = Vec::with_capacity(page.len());
            for (item_id, images) in page {
                let indexed = indexed_images
                    .get(item_id)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                inserts
                    .push(prepare_local_image_batch_item(item_id, images.clone(), indexed).await);
            }

            match self
                .database
                .insert_item_images_batch_at_indices(&inserts)
                .await
            {
                Ok(images_found) => report.images_found += images_found,
                Err(batch_error) => {
                    tracing::warn!(
                        item_count = page.len(),
                        %batch_error,
                        "local series image page failed; retrying items individually"
                    );
                    self.index_images_individually(page, report).await;
                }
            }
        }
    }

    async fn index_images_individually(
        &self,
        candidates: &[(String, Vec<LocalImage>)],
        report: &mut MetadataReport,
    ) {
        for (item_id, images) in candidates {
            match self.index_images(item_id, images.clone()).await {
                Ok(images_found) => report.images_found += images_found,
                Err(error) => {
                    tracing::warn!(
                        item_id,
                        %error,
                        "local series images could not be indexed"
                    );
                    report.mark_item_error(item_id, &error);
                }
            }
        }
    }

    async fn index_images(
        &self,
        item_id: &str,
        images: Vec<LocalImage>,
    ) -> Result<usize, MetadataError> {
        let indexed_images = self.database.list_item_images(item_id).await?;
        let insert = prepare_local_image_batch_item(item_id, images, &indexed_images).await;
        self.database
            .insert_item_images_batch_at_indices(&[insert])
            .await
            .map_err(MetadataError::Storage)
    }
}

async fn prepare_local_image_batch_item(
    item_id: &str,
    images: Vec<LocalImage>,
    indexed_images: &[StoredItemImage],
) -> ItemImageBatchInsert {
    let images = images
        .into_iter()
        .filter(|image| {
            !(image.image_type == ImageType::Thumb
                && indexed_images.iter().any(|indexed| {
                    indexed.image_type.eq_ignore_ascii_case("FANART")
                        && Path::new(&indexed.local_path) == image.path
                }))
        })
        .collect::<Vec<_>>();
    let clear_poster_fallback = images
        .iter()
        .any(|image| matches!(image.image_type, ImageType::Poster | ImageType::Thumb));
    let prepared = prepare_local_images(images).await;
    let mut image_indexes = BTreeMap::<&'static str, i64>::new();
    let mut records = Vec::new();
    for result in prepared {
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                tracing::warn!(
                    item_id,
                    path = %error.path.display(),
                    error = %error.error,
                    "local series image could not be read; skipping image"
                );
                continue;
            }
        };
        let image_index = next_local_image_index(&mut image_indexes, prepared.image.image_type);
        records.push(ItemImageInsert {
            image_type: prepared.image.image_type.as_str().to_owned(),
            image_index,
            local_path: prepared.image.path.to_string_lossy().into_owned(),
            file_size: prepared.file_size,
            width: prepared.dimensions.map(|(width, _)| width),
            height: prepared.dimensions.map(|(_, height)| height),
            content_tag: prepared.content_tag,
            source: "LOCAL".to_owned(),
            source_url: None,
        });
    }
    ItemImageBatchInsert {
        item_id: item_id.to_owned(),
        images: records,
        clear_poster_fallback,
    }
}

async fn prepare_scan_local_movie_image_batch_item(
    source: &StoredMediaSourcePath,
    directory_cache: &mut DirectoryPathCache,
    report: &mut MetadataReport,
) -> Option<ItemImageBatchInsert> {
    let media_path = PathBuf::from(&source.root_path).join(&source.relative_path);
    let image_paths = match directory_cache
        .get(media_path.parent().unwrap_or(Path::new(".")))
        .await
    {
        Ok(paths) => paths,
        Err(error) => {
            tracing::warn!(item_id = %source.item_id, %error, "local movie image directory failed");
            report.mark_item_failed(&source.item_id);
            return None;
        }
    };
    let images = if let Some(media_stem) = media_path.file_stem().and_then(|value| value.to_str()) {
        find_local_images_for_media(image_paths.iter(), media_stem)
    } else {
        find_local_images(image_paths.iter())
    };
    let clear_poster_fallback = images
        .iter()
        .any(|image| matches!(image.image_type, ImageType::Poster | ImageType::Thumb));
    let prepared = prepare_local_images(images).await;
    let mut image_indexes = BTreeMap::<&'static str, i64>::new();
    let mut records = Vec::new();
    for result in prepared {
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                tracing::warn!(
                    item_id = %source.item_id,
                    path = %error.path.display(),
                    error = %error.error,
                    "local movie image could not be read; skipping image"
                );
                continue;
            }
        };
        let image_index = next_local_image_index(&mut image_indexes, prepared.image.image_type);
        records.push(ItemImageInsert {
            image_type: prepared.image.image_type.as_str().to_owned(),
            image_index,
            local_path: prepared.image.path.to_string_lossy().into_owned(),
            file_size: prepared.file_size,
            width: prepared.dimensions.map(|(width, _)| width),
            height: prepared.dimensions.map(|(_, height)| height),
            content_tag: prepared.content_tag,
            source: "LOCAL".to_owned(),
            source_url: None,
        });
    }
    Some(ItemImageBatchInsert {
        item_id: source.item_id.clone(),
        images: records,
        clear_poster_fallback,
    })
}

fn next_local_image_index(indexes: &mut BTreeMap<&'static str, i64>, image_type: ImageType) -> i64 {
    let key = image_type.as_str();
    if image_type != ImageType::Fanart {
        return 0;
    }
    let index = indexes.entry(key).or_default();
    let current = *index;
    *index = index.saturating_add(1);
    current
}

#[derive(Debug)]
struct PreparedLocalImage {
    image: LocalImage,
    file_size: i64,
    content_tag: String,
    dimensions: Option<(i32, i32)>,
}

#[derive(Debug)]
struct LocalImageReadError {
    path: PathBuf,
    error: std::io::Error,
}

async fn prepare_local_images(
    images: Vec<LocalImage>,
) -> Vec<Result<PreparedLocalImage, LocalImageReadError>> {
    let permits = local_image_read_permits();
    let mut pending = JoinSet::new();
    let mut results = (0..images.len())
        .map(|_| None)
        .collect::<Vec<Option<Result<PreparedLocalImage, LocalImageReadError>>>>();
    for (index, image) in images.into_iter().enumerate() {
        while pending.len() >= LOCAL_IMAGE_READ_CONCURRENCY {
            collect_local_image_task(&mut pending, &mut results).await;
        }
        let path = image.path.clone();
        let permit = match permits.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(error) => {
                results[index] = Some(Err(LocalImageReadError {
                    path,
                    error: std::io::Error::other(format!("image read semaphore closed: {error}")),
                }));
                continue;
            }
        };
        pending.spawn(async move {
            let _permit = permit;
            (index, prepare_local_image(image).await)
        });
    }
    while !pending.is_empty() {
        collect_local_image_task(&mut pending, &mut results).await;
    }
    results.into_iter().flatten().collect()
}

async fn collect_local_image_task(
    pending: &mut JoinSet<(usize, Result<PreparedLocalImage, LocalImageReadError>)>,
    results: &mut [Option<Result<PreparedLocalImage, LocalImageReadError>>],
) {
    if let Some(result) = pending.join_next().await {
        match result {
            Ok((index, result)) => results[index] = Some(result),
            Err(error) => {
                tracing::error!(%error, "local image metadata worker panicked");
            }
        }
    }
}

async fn prepare_local_image(image: LocalImage) -> Result<PreparedLocalImage, LocalImageReadError> {
    let bytes = fs::read(&image.path)
        .await
        .map_err(|error| LocalImageReadError {
            path: image.path.clone(),
            error,
        })?;
    let file_size = i64::try_from(bytes.len()).map_err(|_| LocalImageReadError {
        path: image.path.clone(),
        error: std::io::Error::other(format!(
            "image is too large for storage: {} bytes",
            bytes.len()
        )),
    })?;
    let (content_tag, dimensions) = image_content_tag_and_dimensions_from_bytes(bytes)
        .await
        .map_err(|error| LocalImageReadError {
            path: image.path.clone(),
            error,
        })?;
    Ok(PreparedLocalImage {
        image,
        file_size,
        content_tag,
        dimensions,
    })
}

fn local_nfo_premiere_date(details: &crate::application::nfo::LocalNfoDetails) -> Option<&str> {
    details
        .premiered
        .as_deref()
        .or(details.release_date.as_deref())
        .or(details.aired.as_deref())
}

fn local_nfo_defaults_missing(
    metadata: &StoredMediaMetadata,
    details: &crate::application::nfo::LocalNfoDetails,
) -> bool {
    let provider_id_missing =
        merged_provider_ids_json(metadata.provider_ids_json.as_deref(), &details.provider_ids)
            .is_some();
    let premiere_date_missing = local_nfo_premiere_date(details)
        .filter(|value| !value.trim().is_empty())
        .is_some_and(|_| {
            metadata
                .premiere_date
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
        });

    provider_id_missing || premiere_date_missing
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataReport {
    pub nfo_loaded: usize,
    pub nfo_failed: usize,
    pub nfo_skipped: usize,
    pub images_found: usize,
    pub items_processed: usize,
    pub(crate) locally_enriched_item_ids: Vec<String>,
    pub(crate) failed_item_ids: Vec<String>,
    pub(crate) non_retryable_failed_item_ids: Vec<String>,
}

pub(crate) struct ScanLocalMetadataNfoBatch {
    pub(crate) report: MetadataReport,
    // Each pair is (item_id, preferred source_id) from the NFO stage's source snapshot.
    pub(crate) source_identities: Vec<(String, String)>,
}

pub(crate) struct ScanLocalMetadataImageBatch {
    pub(crate) report: MetadataReport,
    // Reused by NFO processing after a lightweight preferred-source validation.
    pub(crate) sources: Vec<StoredScanLocalMetadataSource>,
}

impl MetadataReport {
    fn merge(&mut self, other: Self) {
        self.nfo_loaded += other.nfo_loaded;
        self.nfo_failed += other.nfo_failed;
        self.nfo_skipped += other.nfo_skipped;
        self.images_found += other.images_found;
        self.items_processed += other.items_processed;
        self.locally_enriched_item_ids
            .extend(other.locally_enriched_item_ids);
        for item_id in other.failed_item_ids {
            self.mark_item_failed(&item_id);
        }
        for item_id in other.non_retryable_failed_item_ids {
            self.mark_item_non_retryable_failed(&item_id);
        }
    }

    fn mark_item_failed(&mut self, item_id: &str) {
        if !self.failed_item_ids.iter().any(|failed| failed == item_id) {
            self.failed_item_ids.push(item_id.to_owned());
        }
    }

    fn mark_item_non_retryable_failed(&mut self, item_id: &str) {
        self.mark_item_failed(item_id);
        if !self
            .non_retryable_failed_item_ids
            .iter()
            .any(|failed| failed == item_id)
        {
            self.non_retryable_failed_item_ids.push(item_id.to_owned());
        }
    }

    fn mark_item_error(&mut self, item_id: &str, error: &MetadataError) {
        if error.is_retryable() {
            self.mark_item_failed(item_id);
        } else {
            self.mark_item_non_retryable_failed(item_id);
        }
    }

    pub(crate) fn retryable_failed_item_count(&self) -> usize {
        self.failed_item_ids
            .iter()
            .filter(|item_id| {
                !self
                    .non_retryable_failed_item_ids
                    .iter()
                    .any(|failed| failed == *item_id)
            })
            .count()
    }
}

pub(crate) async fn nfo_fingerprint(path: &Path) -> Result<Vec<u8>, std::io::Error> {
    let metadata = fs::metadata(path).await?;
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    Ok(nfo_fingerprint_from_stamp(
        path,
        metadata.len(),
        modified_at,
    ))
}

pub(crate) fn nfo_fingerprint_from_stamp(path: &Path, size: u64, modified_at: u128) -> Vec<u8> {
    let size = i64::try_from(size).unwrap_or(i64::MAX);
    let modified_at = i64::try_from(modified_at).unwrap_or(0);
    let path = path.to_string_lossy();
    compute_file_fingerprint(&path, size, modified_at, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn movie_library_image_registration_batches_multiple_items()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let config = crate::config::Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let media_root = directory.path().join("Movies");
        let mut fanart_png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([0, 255, 0, 255]),
        ))
        .write_to(&mut fanart_png, image::ImageFormat::Png)?;
        for (folder, stem) in [
            ("Movie One (2024)", "Movie.One.2024"),
            ("Movie Two (2024)", "Movie.Two.2024"),
        ] {
            let movie_dir = media_root.join(folder);
            tokio::fs::create_dir_all(&movie_dir).await?;
            tokio::fs::write(movie_dir.join(format!("{stem}.mkv")), b"media").await?;
            tokio::fs::write(movie_dir.join("fanart.png"), fanart_png.get_ref()).await?;
        }

        let database = Database::connect(&config).await?;
        let libraries = crate::application::libraries::LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", crate::library::LibraryKind::Movie, false)
            .await?;
        libraries
            .add_root(
                library.id,
                media_root.to_str().ok_or("non-UTF8 media root")?,
            )
            .await?;
        crate::application::scanner::LibraryScanner::new(database.clone())
            .scan_movie_library(library.id)
            .await?;

        let enricher = MetadataEnricher::new(database.clone());
        database.reset_query_count();
        let report = enricher.enrich_movie_library(library.id).await?;

        assert_eq!(report.items_processed, 2);
        assert_eq!(report.images_found, 2);
        assert_eq!(
            database.query_count(),
            2,
            "one source page query and one shared image insert should cover both movies"
        );
        let images: Vec<(String, String)> = sqlx::query_as(
            "SELECT media_items.title, item_images.image_type
             FROM item_images
             JOIN media_items ON media_items.id = item_images.item_id
             ORDER BY media_items.title",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(
            images,
            vec![
                ("Movie One".to_owned(), "FANART".to_owned()),
                ("Movie Two".to_owned(), "FANART".to_owned())
            ]
        );

        let item_ids: Vec<(String, String)> =
            sqlx::query_as("SELECT id, title FROM media_items WHERE library_id = ? ORDER BY title")
                .bind(library.id.to_string())
                .fetch_all(database.pool())
                .await?;
        let blocked_item_id = item_ids.first().ok_or("missing first movie")?.0.clone();
        sqlx::query(
            "CREATE TRIGGER reject_one_movie_image BEFORE INSERT ON item_images
             WHEN NEW.item_id = (SELECT id FROM media_items WHERE title = 'Movie One')
             BEGIN SELECT RAISE(ABORT, 'injected item image failure'); END",
        )
        .execute(database.pool())
        .await?;
        let mut changed_fanart = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([0, 0, 255, 255]),
        ))
        .write_to(&mut changed_fanart, image::ImageFormat::Png)?;
        tokio::fs::write(
            media_root.join("Movie Two (2024)").join("fanart.png"),
            changed_fanart.get_ref(),
        )
        .await?;

        let recovery_report = enricher.enrich_movie_library(library.id).await?;
        assert_eq!(recovery_report.images_found, 1);
        assert!(recovery_report.failed_item_ids.contains(&blocked_item_id));
        Ok(())
    }

    #[tokio::test]
    async fn series_library_image_registration_batches_multiple_items()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let config = crate::config::Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let media_root = directory.path().join("Series");
        let mut poster_png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([255, 0, 0, 255]),
        ))
        .write_to(&mut poster_png, image::ImageFormat::Png)?;
        for (series, episode) in [
            ("First Show", "First.Show.S01E01"),
            ("Second Show", "Second.Show.S01E01"),
        ] {
            let series_dir = media_root.join(series);
            let season_dir = series_dir.join("Season 01");
            tokio::fs::create_dir_all(&season_dir).await?;
            tokio::fs::write(series_dir.join("poster.png"), poster_png.get_ref()).await?;
            tokio::fs::write(season_dir.join(format!("{episode}.mkv")), b"media").await?;
        }

        let database = Database::connect(&config).await?;
        let libraries = crate::application::libraries::LibraryService::new(database.clone());
        let library = libraries
            .create_library("Series", crate::library::LibraryKind::Series, false)
            .await?;
        libraries
            .add_root(
                library.id,
                media_root.to_str().ok_or("non-UTF8 media root")?,
            )
            .await?;
        crate::application::scanner::LibraryScanner::new(database.clone())
            .scan_series_library(library.id)
            .await?;

        let enricher = MetadataEnricher::new(database.clone());
        database.reset_query_count();
        let report = enricher.enrich_series_library(library.id).await?;

        assert_eq!(report.items_processed, 2);
        assert_eq!(report.images_found, 2);
        assert_eq!(
            database.query_count(),
            4,
            "one source page query, one existing-image read, one shared insert, and one fallback update should cover both series"
        );
        let images: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT media_items.title, item_images.image_type, item_images.local_path
             FROM item_images
             JOIN media_items ON media_items.id = item_images.item_id
             ORDER BY media_items.title",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(images.len(), 2);
        assert!(images.iter().all(|(title, image_type, path)| {
            image_type == "POSTER"
                && path.ends_with("poster.png")
                && (title == "First Show" || title == "Second Show")
        }));

        let first_series_id: String = sqlx::query_scalar(
            "SELECT id FROM media_items WHERE library_id = ? AND title = 'First Show'",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?;
        let first_tag_before: Option<String> = sqlx::query_scalar(
            "SELECT content_tag FROM item_images WHERE item_id = ? AND image_type = 'POSTER'",
        )
        .bind(&first_series_id)
        .fetch_one(database.pool())
        .await?;
        let second_tag_before: Option<String> = sqlx::query_scalar(
            "SELECT content_tag FROM item_images
             WHERE item_id = (SELECT id FROM media_items WHERE library_id = ? AND title = 'Second Show')
               AND image_type = 'POSTER'",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?;
        sqlx::query(
            "CREATE TRIGGER reject_one_series_image BEFORE INSERT ON item_images
             WHEN NEW.item_id = (SELECT id FROM media_items WHERE title = 'First Show')
             BEGIN SELECT RAISE(ABORT, 'injected series image failure'); END",
        )
        .execute(database.pool())
        .await?;
        let mut changed_poster = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([0, 0, 255, 255]),
        ))
        .write_to(&mut changed_poster, image::ImageFormat::Png)?;
        for series in ["First Show", "Second Show"] {
            tokio::fs::write(
                media_root.join(series).join("poster.png"),
                changed_poster.get_ref(),
            )
            .await?;
        }

        let recovery_report = enricher.enrich_series_library(library.id).await?;
        assert_eq!(recovery_report.images_found, 1);
        assert!(recovery_report.failed_item_ids.contains(&first_series_id));
        let first_tag_after: Option<String> = sqlx::query_scalar(
            "SELECT content_tag FROM item_images WHERE item_id = ? AND image_type = 'POSTER'",
        )
        .bind(&first_series_id)
        .fetch_one(database.pool())
        .await?;
        let second_tag_after: Option<String> = sqlx::query_scalar(
            "SELECT content_tag FROM item_images
             WHERE item_id = (SELECT id FROM media_items WHERE library_id = ? AND title = 'Second Show')
               AND image_type = 'POSTER'",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?;
        assert_eq!(first_tag_after, first_tag_before);
        assert_ne!(second_tag_after, second_tag_before);
        Ok(())
    }

    #[tokio::test]
    async fn scan_local_metadata_nfo_retains_home_video_path_errors()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let blocking_parent = directory.path().join("not-a-directory");
        tokio::fs::write(&blocking_parent, b"file").await?;
        let source = StoredScanLocalMetadataSource {
            source_id: "source-1".to_owned(),
            item_id: "video-1".to_owned(),
            item_type: "VIDEO".to_owned(),
            probe_status: "READY".to_owned(),
            series_id: None,
            season_id: None,
            season_number: None,
            root_path: directory
                .path()
                .to_str()
                .ok_or("non-UTF8 temporary path")?
                .to_owned(),
            relative_path: "not-a-directory/video.mp4".to_owned(),
        };

        let lookups = scan_local_metadata_nfo_paths(&[source]).await;

        assert!(matches!(
            lookups.nfo_errors_by_item.get("video-1"),
            Some(MetadataError::Io { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn scan_local_metadata_nfo_reuses_the_image_stage_source_lookup()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let config = crate::config::Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let media_root = directory.path().join("Movies");
        for (folder, stem, title) in [
            ("Movie One (2024)", "Movie.One.2024", "Movie One"),
            ("Movie Two (2024)", "Movie.Two.2024", "Movie Two"),
        ] {
            let movie_dir = media_root.join(folder);
            tokio::fs::create_dir_all(&movie_dir).await?;
            tokio::fs::write(movie_dir.join(format!("{stem}.mkv")), b"media").await?;
            tokio::fs::write(
                movie_dir.join(format!("{stem}.nfo")),
                format!("<movie><title>{title}</title></movie>"),
            )
            .await?;
        }
        let no_nfo_dir = media_root.join("Movie Three (2024)");
        tokio::fs::create_dir_all(&no_nfo_dir).await?;
        tokio::fs::write(no_nfo_dir.join("Movie.Three.2024.mkv"), b"media").await?;

        let database = Database::connect(&config).await?;
        let libraries = crate::application::libraries::LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", crate::library::LibraryKind::Movie, false)
            .await?;
        libraries
            .add_root(
                library.id,
                media_root.to_str().ok_or("non-UTF8 media root")?,
            )
            .await?;
        crate::application::scanner::LibraryScanner::new(database.clone())
            .scan_movie_library(library.id)
            .await?;
        let entry_ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM filesystem_entries
             WHERE relative_path LIKE 'Movie % (2024)/%.mkv' ORDER BY relative_path",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(entry_ids.len(), 3);

        let enricher = MetadataEnricher::new(database.clone());
        let image_batch = enricher
            .index_scan_local_metadata_batch_images(&entry_ids)
            .await?;
        database.reset_query_count();

        let batch = enricher
            .enrich_scan_local_metadata_batch_nfo(image_batch.sources, &[])
            .await?;

        assert_eq!(batch.report.items_processed, 3);
        assert_eq!(batch.report.nfo_loaded, 2);
        assert_eq!(
            database.query_count(),
            4,
            "two NFOs should share one metadata lookup and one source validation query"
        );

        let no_nfo_entry_id: String = sqlx::query_scalar(
            "SELECT id FROM filesystem_entries WHERE relative_path LIKE 'Movie Three %' LIMIT 1",
        )
        .fetch_one(database.pool())
        .await?;
        let no_nfo_image_batch = enricher
            .index_scan_local_metadata_batch_images(std::slice::from_ref(&no_nfo_entry_id))
            .await?;
        database.reset_query_count();
        let no_nfo_batch = enricher
            .enrich_scan_local_metadata_batch_nfo(no_nfo_image_batch.sources, &[])
            .await?;
        assert_eq!(no_nfo_batch.report.items_processed, 1);
        assert_eq!(no_nfo_batch.report.nfo_loaded, 0);
        assert_eq!(
            database.query_count(),
            1,
            "a batch without NFO files should skip the media metadata lookup"
        );

        let stale_image_batch = enricher
            .index_scan_local_metadata_batch_images(std::slice::from_ref(&entry_ids[0]))
            .await?;
        sqlx::query(
            "UPDATE filesystem_entries SET is_missing = 1
             WHERE id = ?",
        )
        .bind(&entry_ids[0])
        .execute(database.pool())
        .await?;
        database.reset_query_count();
        let stale_nfo_batch = enricher
            .enrich_scan_local_metadata_batch_nfo(stale_image_batch.sources, &[])
            .await?;
        assert_eq!(stale_nfo_batch.report.items_processed, 0);
        assert!(stale_nfo_batch.source_identities.is_empty());
        assert_eq!(
            database.query_count(),
            1,
            "stale preferred sources should be rejected by the same bounded validation"
        );
        Ok(())
    }

    #[tokio::test]
    async fn series_image_registration_rolls_back_when_fallback_update_fails()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let config = crate::config::Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = crate::application::libraries::LibraryService::new(database.clone())
            .create_library("Artwork", crate::library::LibraryKind::Series, false)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (id, library_id, item_type, title, sort_title,
                identification_status, poster_fallback_required)
             VALUES ('artwork-item', ?, 'SERIES', 'Show', 'show', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        sqlx::query(
            "CREATE TRIGGER reject_artwork_fallback BEFORE UPDATE OF poster_fallback_required
             ON media_items BEGIN SELECT RAISE(ABORT, 'fallback failure'); END",
        )
        .execute(database.pool())
        .await?;
        let poster = directory.path().join("poster.jpg");
        tokio::fs::write(&poster, b"local image fixture").await?;
        let enricher = MetadataEnricher::new(database.clone());
        assert!(
            enricher
                .index_images(
                    "artwork-item",
                    vec![LocalImage {
                        image_type: ImageType::Poster,
                        path: poster.clone(),
                    }]
                )
                .await
                .is_err()
        );
        assert!(database.list_item_images("artwork-item").await?.is_empty());
        sqlx::query("DROP TRIGGER reject_artwork_fallback")
            .execute(database.pool())
            .await?;
        assert_eq!(
            enricher
                .index_images(
                    "artwork-item",
                    vec![LocalImage {
                        image_type: ImageType::Poster,
                        path: poster,
                    }]
                )
                .await?,
            1
        );
        let fallback: i64 = sqlx::query_scalar(
            "SELECT poster_fallback_required FROM media_items WHERE id = 'artwork-item'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(fallback, 0);
        Ok(())
    }

    #[tokio::test]
    async fn unchanged_nfo_with_complete_defaults_skips_repair_query()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::application::nfo::LocalNfoDetails;

        let directory = tempfile::tempdir()?;
        let config = crate::config::Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = crate::application::libraries::LibraryService::new(database.clone())
            .create_library("Movies", crate::library::LibraryKind::Movie, false)
            .await?;
        let item_id = "unchanged-nfo-item";
        let nfo_path = directory.path().join("movie.nfo");
        let nfo_bytes = b"<movie><title>Local title</title><tmdbid>42</tmdbid><premiered>2026-01-02</premiered></movie>";
        tokio::fs::write(&nfo_path, nfo_bytes).await?;
        let fingerprint = nfo_fingerprint(&nfo_path).await?;
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status,
                provider_ids_json, premiere_date, metadata_fingerprint
             ) VALUES (?, ?, 'MOVIE', 'Local title', 'local title', 'LOCAL_CONFIRMED', ?, ?, ?)",
        )
        .bind(item_id)
        .bind(library.id.to_string())
        .bind(r#"{"tmdb":"99"}"#)
        .bind("2025-01-02")
        .bind(fingerprint)
        .execute(database.pool())
        .await?;

        let details = LocalNfoDetails {
            premiered: Some("2026-01-02".to_owned()),
            provider_ids: BTreeMap::from([("tmdb".to_owned(), "42".to_owned())]),
            ..LocalNfoDetails::default()
        };
        LocalNfoMetadataStore::new(database.clone())
            .write_item(item_id, &nfo_content_fingerprint(nfo_bytes), &details)
            .await?;

        let enricher = MetadataEnricher::new(database.clone())
            .with_nfo_store(LocalNfoMetadataStore::new(database.clone()));
        database.reset_query_count();
        let report = enricher.enrich_nfo_item(item_id, &nfo_path).await?;

        assert_eq!(report.nfo_skipped, 1);
        assert_eq!(
            database.query_count(),
            3,
            "unchanged NFO with complete defaults should only read metadata and its rich cache"
        );
        let retained_provider_ids: String =
            sqlx::query_scalar("SELECT provider_ids_json FROM media_items WHERE id = ?")
                .bind(item_id)
                .fetch_one(database.pool())
                .await?;
        assert_eq!(retained_provider_ids, r#"{"tmdb":"99"}"#);
        let retained_premiere_date: Option<String> =
            sqlx::query_scalar("SELECT premiere_date FROM media_items WHERE id = ?")
                .bind(item_id)
                .fetch_one(database.pool())
                .await?;
        assert_eq!(retained_premiere_date.as_deref(), Some("2025-01-02"));
        Ok(())
    }

    #[test]
    fn merges_only_new_provider_ids() {
        let current = Some(r#"{"tmdb":"603"}"#);
        let incoming = BTreeMap::from([
            ("TMDB".to_owned(), "999".to_owned()),
            ("imdb".to_owned(), "tt0133093".to_owned()),
        ]);

        assert_eq!(
            merged_provider_ids_json(current, &incoming).as_deref(),
            Some(r#"{"imdb":"tt0133093","tmdb":"603"}"#)
        );
    }

    #[test]
    fn returns_none_when_incoming_ids_are_already_known() {
        let current = Some(r#"{"tmdb":"603"}"#);
        let incoming = BTreeMap::from([(String::from("TMDB"), String::from("603"))]);

        assert!(merged_provider_ids_json(current, &incoming).is_none());
    }

    #[tokio::test]
    async fn directory_path_cache_reuses_a_directory_snapshot()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let directory = temp_dir.path().join("series");
        tokio::fs::create_dir(&directory).await?;
        tokio::fs::write(directory.join("episode-1.mkv"), b"episode").await?;

        let mut cache = DirectoryPathCache::default();
        let initial = cache.get(&directory).await?.to_vec();
        tokio::fs::write(directory.join("episode-2.mkv"), b"episode").await?;
        let cached = cache.get(&directory).await?.to_vec();

        assert_eq!(cache.len(), 1);
        assert_eq!(cached, initial);
        Ok(())
    }

    #[test]
    fn conflicting_movie_identity_error_is_non_retryable_and_names_both_items() {
        let error = MetadataError::ConflictingMovieIdentity {
            item_id: "current-item".to_owned(),
            conflicting_item_id: "conflicting-item".to_owned(),
        };
        let message = error.to_string();
        assert!(!error.is_retryable());
        assert!(message.contains("current_item_id=current-item"));
        assert!(message.contains("conflicting_item_id=conflicting-item"));
    }
}

pub(crate) async fn find_nfo_path(media_path: &Path) -> Option<PathBuf> {
    let directory = media_path.parent()?;
    let movie_nfo = directory.join("movie.nfo");
    if fs::try_exists(&movie_nfo).await.ok()? {
        return Some(movie_nfo);
    }
    let same_name = media_path.with_extension("nfo");
    if fs::try_exists(&same_name).await.ok()? {
        return Some(same_name);
    }
    find_directory_nfo(directory).await
}

async fn find_directory_nfo(directory: &Path) -> Option<PathBuf> {
    let mut entries = fs::read_dir(directory).await.ok()?;
    let mut candidates = Vec::new();
    loop {
        match entries.next_entry().await {
            Ok(Some(entry)) => {
                let file_type = entry.file_type().await.ok()?;
                let is_nfo = file_type.is_file()
                    && entry
                        .path()
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("nfo"));
                if is_nfo {
                    candidates.push(entry.path());
                }
            }
            Ok(None) => break,
            Err(_) => return None,
        }
    }
    candidates.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    candidates.into_iter().next()
}

async fn read_directory_paths(directory: &Path) -> Result<Vec<PathBuf>, MetadataError> {
    let mut entries = fs::read_dir(directory)
        .await
        .map_err(|source| MetadataError::Io {
            path: directory.to_owned(),
            source,
        })?;
    let mut paths = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| MetadataError::Io {
            path: directory.to_owned(),
            source,
        })?
    {
        paths.push(entry.path());
    }
    paths.sort();
    Ok(paths)
}

#[derive(Default)]
struct DirectoryPathCache {
    entries: HashMap<PathBuf, Arc<Vec<PathBuf>>>,
}

impl DirectoryPathCache {
    async fn get(&mut self, directory: &Path) -> Result<Arc<Vec<PathBuf>>, MetadataError> {
        if let Some(paths) = self.entries.get(directory) {
            return Ok(Arc::clone(paths));
        }
        let paths = Arc::new(read_directory_paths(directory).await?);
        self.entries
            .insert(directory.to_owned(), Arc::clone(&paths));
        Ok(paths)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

pub(crate) fn series_directory(root: &Path, relative_path: &str) -> Option<PathBuf> {
    let mut series_dir = root.to_owned();
    let mut saw_series_component = false;
    for component in Path::new(relative_path).parent()?.components() {
        let value = component.as_os_str();
        let value_text = value.to_str()?;
        if is_season_directory(value_text) {
            return saw_series_component.then_some(series_dir);
        }
        series_dir.push(value);
        saw_series_component = true;
    }
    None
}

fn is_season_directory(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized == "specials" {
        return true;
    }
    let Some(number) = normalized
        .strip_prefix("season")
        .or_else(|| normalized.strip_prefix('s'))
    else {
        return false;
    };
    let number = number.trim();
    let number = number
        .split_once('(')
        .and_then(|(prefix, suffix)| suffix.strip_suffix(')').map(|_| prefix.trim()))
        .unwrap_or(number);
    !number.is_empty() && number.chars().all(|character| character.is_ascii_digit())
}

async fn find_tvshow_nfo(series_dir: &Path) -> Option<PathBuf> {
    let path = series_dir.join("tvshow.nfo");
    fs::try_exists(&path).await.ok()?.then_some(path)
}

async fn find_season_nfo(
    series_dir: &Path,
    season_dir: &Path,
    season_number: i64,
) -> Option<PathBuf> {
    let names = if season_number == 0 {
        vec!["season00.nfo".to_owned(), "specials.nfo".to_owned()]
    } else {
        vec![
            format!("season{season_number:02}.nfo"),
            format!("season{season_number}.nfo"),
        ]
    };
    let mut candidates = Vec::new();
    for name in names {
        candidates.push(season_dir.join(&name));
        candidates.push(series_dir.join(&name));
    }
    candidates.push(season_dir.join("season.nfo"));
    for candidate in candidates {
        if fs::try_exists(&candidate).await.ok()? {
            return Some(candidate);
        }
    }
    None
}

async fn find_episode_nfo(media_path: &Path) -> Option<PathBuf> {
    let same_name = media_path.with_extension("nfo");
    if fs::try_exists(&same_name).await.ok()? {
        return Some(same_name);
    }
    let episode_nfo = media_path.parent()?.join("episode.nfo");
    fs::try_exists(&episode_nfo)
        .await
        .ok()?
        .then_some(episode_nfo)
}

fn find_series_images(paths: &[PathBuf], season_number: Option<i64>) -> Vec<LocalImage> {
    let mut images = Vec::new();
    for path in paths {
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        if !matches!(
            extension.to_ascii_lowercase().as_str(),
            "jpg" | "jpeg" | "png" | "webp"
        ) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let stem = stem.to_ascii_lowercase();
        let image_type = match season_number {
            None => {
                let Some(image_type) = image_type_for_stem(&stem) else {
                    continue;
                };
                image_type
            }
            Some(number) => {
                let prefix = format!("season{number}");
                let padded_prefix = format!("season{number:02}");
                let is_poster = matches_indexed_stem(&stem, "poster")
                    || matches_indexed_stem(&stem, &format!("{prefix}-poster"))
                    || matches_indexed_stem(&stem, &format!("{padded_prefix}-poster"));
                let is_fanart = matches_indexed_stem(&stem, "fanart")
                    || matches_indexed_stem(&stem, "backdrop")
                    || matches_indexed_stem(&stem, &format!("{prefix}-fanart"))
                    || matches_indexed_stem(&stem, &format!("{padded_prefix}-fanart"))
                    || matches_indexed_stem(&stem, &format!("{prefix}-backdrop"))
                    || matches_indexed_stem(&stem, &format!("{padded_prefix}-backdrop"));
                if is_poster {
                    ImageType::Poster
                } else if is_fanart {
                    ImageType::Fanart
                } else {
                    continue;
                }
            }
        };
        if image_type != ImageType::Fanart
            && images
                .iter()
                .any(|image: &LocalImage| image.image_type == image_type)
        {
            continue;
        }
        images.push(LocalImage {
            image_type,
            path: path.clone(),
        });
    }
    images
}

fn prefers_canonical_episode_thumbnail(
    candidate: &Path,
    existing: &Path,
    episode_prefix: &str,
) -> bool {
    let candidate_suffix = candidate
        .file_stem()
        .and_then(|value| value.to_str())
        .and_then(|value| {
            value
                .to_ascii_lowercase()
                .strip_prefix(episode_prefix)
                .map(str::to_owned)
        });
    let existing_suffix = existing
        .file_stem()
        .and_then(|value| value.to_str())
        .and_then(|value| {
            value
                .to_ascii_lowercase()
                .strip_prefix(episode_prefix)
                .map(str::to_owned)
        });
    candidate_suffix
        .as_deref()
        .is_some_and(|suffix| matches_indexed_stem(suffix, "thumbnail"))
        && existing_suffix
            .as_deref()
            .is_some_and(|suffix| matches_indexed_stem(suffix, "thumb"))
}

fn find_episode_images(paths: &[PathBuf], media_path: &Path) -> Vec<LocalImage> {
    let Some(episode_stem) = media_path.file_stem().and_then(|value| value.to_str()) else {
        return Vec::new();
    };
    let prefix = format!("{}-", episode_stem.to_ascii_lowercase());
    let mut images = Vec::new();
    for path in paths {
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        if !matches!(
            extension.to_ascii_lowercase().as_str(),
            "jpg" | "jpeg" | "png" | "webp"
        ) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let stem = stem.to_ascii_lowercase();
        let Some(suffix) = stem.strip_prefix(&prefix) else {
            continue;
        };
        let image_type = match suffix {
            value if matches_indexed_stem(value, "poster") => ImageType::Poster,
            value
                if matches_indexed_stem(value, "fanart")
                    || matches_indexed_stem(value, "backdrop") =>
            {
                ImageType::Fanart
            }
            value
                if matches_indexed_stem(value, "thumb")
                    || matches_indexed_stem(value, "thumbnail") =>
            {
                ImageType::Thumb
            }
            value
                if matches_indexed_stem(value, "logo")
                    || matches_indexed_stem(value, "clearlogo") =>
            {
                ImageType::Logo
            }
            value if matches_indexed_stem(value, "banner") => ImageType::Banner,
            value
                if matches_indexed_stem(value, "disc")
                    || matches_indexed_stem(value, "discart") =>
            {
                ImageType::Disc
            }
            value
                if matches_indexed_stem(value, "art") || matches_indexed_stem(value, "artwork") =>
            {
                ImageType::Art
            }
            value if matches_indexed_stem(value, "wallpaper") => ImageType::Wallpaper,
            _ => continue,
        };
        if image_type != ImageType::Fanart {
            if let Some(existing) = images
                .iter_mut()
                .find(|image: &&mut LocalImage| image.image_type == image_type)
            {
                if image_type == ImageType::Thumb
                    && prefers_canonical_episode_thumbnail(path, &existing.path, &prefix)
                {
                    existing.path = path.clone();
                }
                continue;
            }
        }
        images.push(LocalImage {
            image_type,
            path: path.clone(),
        });
    }
    images
}

fn is_prefixed_season_image(path: &Path, season_number: i64) -> bool {
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return false;
    };
    let stem = stem.to_ascii_lowercase();
    let prefix = format!("season{season_number}");
    let padded_prefix = format!("season{season_number:02}");
    [
        format!("{prefix}-poster"),
        format!("{prefix}-fanart"),
        format!("{prefix}-backdrop"),
        format!("{padded_prefix}-poster"),
        format!("{padded_prefix}-fanart"),
        format!("{padded_prefix}-backdrop"),
    ]
    .into_iter()
    .any(|candidate| matches_indexed_stem(&stem, &candidate))
}

#[derive(Debug)]
pub enum MetadataError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    FileSizeOutOfRange {
        path: PathBuf,
        size: u64,
    },
    Storage(StorageError),
    NfoCache(LocalNfoMetadataStoreError),
    ConflictingMovieIdentity {
        item_id: String,
        conflicting_item_id: String,
    },
}

impl MetadataError {
    fn is_retryable(&self) -> bool {
        !matches!(self, Self::ConflictingMovieIdentity { .. })
    }
}

impl fmt::Display for MetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "metadata path '{}': {source}", path.display())
            }
            Self::FileSizeOutOfRange { path, size } => write!(
                formatter,
                "metadata file '{}' is too large for storage: {size} bytes",
                path.display()
            ),
            Self::Storage(error) => error.fmt(formatter),
            Self::NfoCache(error) => error.fmt(formatter),
            Self::ConflictingMovieIdentity {
                item_id,
                conflicting_item_id,
            } => write!(
                formatter,
                "local movie NFO conflicts with another movie identity: current_item_id={item_id}, conflicting_item_id={conflicting_item_id}"
            ),
        }
    }
}

impl std::error::Error for MetadataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::FileSizeOutOfRange { .. } => None,
            Self::Storage(error) => Some(error),
            Self::NfoCache(error) => Some(error),
            Self::ConflictingMovieIdentity { .. } => None,
        }
    }
}

impl From<StorageError> for MetadataError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
