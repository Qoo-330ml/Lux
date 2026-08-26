use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::Read,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs;

use crate::{
    application::scanner::compute_file_fingerprint,
    application::{
        images::read_image_dimensions,
        nfo::{
            LocalNfoMetadataStore, LocalNfoMetadataStoreError, nfo_content_fingerprint,
            parse_local_nfo_projection,
        },
        people::PeopleService,
    },
    domain::ids::LibraryId,
    storage::{
        Database, ItemImageMetadata, MediaMetadataUpdate, StorageError, StoredMediaSourcePath,
        StoredSeriesMetadataSource,
    },
};

const LIBRARY_SOURCE_PAGE_SIZE: usize = 500;

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
    ScraperLocalized,
    TmdbLocalized,
    Fallback,
    LockedLocal,
}

impl MetadataSource {
    const fn priority(self) -> u8 {
        match self {
            Self::Fallback => 1,
            Self::ScraperLocalized | Self::TmdbLocalized => 2,
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
            if images
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
        if images
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
        stem.eq_ignore_ascii_case(&expected).then_some(image_type)
    })
}

fn image_type_for_stem(stem: &str) -> Option<ImageType> {
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

        let series_sources = self
            .database
            .list_series_metadata_sources_for_incremental_scan(scan_job_id)
            .await?;
        let mut last_series_id = String::new();
        let mut last_season_id = String::new();
        let mut last_episode_id = String::new();
        self.enrich_series_sources(
            series_sources,
            &mut report,
            Some(&mut last_series_id),
            Some(&mut last_season_id),
            Some(&mut last_episode_id),
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

        let mut last_series_id = None;
        let mut last_season_id = None;
        let mut last_episode_id = None;
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
            if let Err(error) = self
                .enrich_series_sources(
                    sources,
                    &mut batch_report,
                    last_series_id.as_mut(),
                    last_season_id.as_mut(),
                    last_episode_id.as_mut(),
                )
                .await
            {
                self.database
                    .mark_scan_job_target_stage(
                        scan_job_id,
                        "ITEM",
                        &item_ids,
                        "METADATA",
                        "FAILED",
                    )
                    .await?;
                return Err(error);
            }
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
        for source in sources {
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
                    report.mark_item_failed(&source.item_id);
                }
            }

            match self.index_movie_images(&source.item_id, &media_path).await {
                Ok(images_found) => report.images_found += images_found,
                Err(error) => {
                    tracing::warn!(
                        item_id = %source.item_id,
                        %error,
                        "local movie image directory failed; continuing with remaining items"
                    );
                    report.mark_item_failed(&source.item_id);
                }
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

    async fn index_movie_images(
        &self,
        item_id: &str,
        media_path: &Path,
    ) -> Result<usize, MetadataError> {
        let image_paths =
            read_directory_paths(media_path.parent().unwrap_or(Path::new("."))).await?;
        let images =
            if let Some(media_stem) = media_path.file_stem().and_then(|value| value.to_str()) {
                find_local_images_for_media(image_paths, media_stem)
            } else {
                find_local_images(image_paths)
            };
        let has_primary_artwork = images
            .iter()
            .any(|image| matches!(image.image_type, ImageType::Poster | ImageType::Thumb));
        let mut inserted_count = 0;
        for image in images {
            let file_size = match fs::metadata(&image.path).await {
                Ok(metadata) => metadata.len(),
                Err(error) => {
                    tracing::warn!(
                        item_id,
                        path = %image.path.display(),
                        %error,
                        "local movie image could not be read; skipping image"
                    );
                    continue;
                }
            };
            let file_size = match i64::try_from(file_size) {
                Ok(file_size) => file_size,
                Err(_) => {
                    tracing::warn!(
                        item_id,
                        path = %image.path.display(),
                        file_size,
                        "local movie image is too large for storage; skipping image"
                    );
                    continue;
                }
            };
            let content_tag = match image_content_tag(&image.path).await {
                Ok(content_tag) => content_tag,
                Err(error) => {
                    tracing::warn!(
                        item_id,
                        path = %image.path.display(),
                        %error,
                        "local movie image fingerprint could not be read; skipping image"
                    );
                    continue;
                }
            };
            let dimensions = read_image_dimensions(&image.path).await;
            match self
                .database
                .insert_item_image(
                    item_id,
                    image.image_type.as_str(),
                    &image.path,
                    ItemImageMetadata {
                        file_size,
                        width: dimensions.map(|(width, _)| width),
                        height: dimensions.map(|(_, height)| height),
                        content_tag: &content_tag,
                        source: "LOCAL",
                    },
                )
                .await
            {
                Ok(true) => inserted_count += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        item_id,
                        path = %image.path.display(),
                        %error,
                        "local movie image indexing failed; skipping image"
                    );
                }
            }
        }
        if has_primary_artwork {
            self.database
                .set_poster_fallback_required(item_id, false)
                .await?;
        }
        Ok(inserted_count)
    }

    pub async fn enrich_series_library(
        &self,
        library_id: LibraryId,
    ) -> Result<MetadataReport, MetadataError> {
        let mut report = MetadataReport::default();
        let library_id = library_id.to_string();
        let mut offset = 0_i64;
        let mut last_series_id = None;
        let mut last_season_id = None;
        let mut last_episode_id = None;
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
                last_series_id.as_mut(),
                last_season_id.as_mut(),
                last_episode_id.as_mut(),
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
        last_series_id: Option<&mut String>,
        last_season_id: Option<&mut String>,
        last_episode_id: Option<&mut String>,
    ) -> Result<(), MetadataError> {
        let mut last_series_id = last_series_id;
        let mut last_season_id = last_season_id;
        let mut last_episode_id = last_episode_id;
        for source in sources {
            let root = PathBuf::from(&source.root_path);
            let media_path = root.join(&source.relative_path);
            let Some(series_dir) = series_directory(&root, &source.relative_path) else {
                continue;
            };
            let series_paths = read_directory_paths(&series_dir).await?;
            let new_series = last_series_id
                .as_deref()
                .is_none_or(|id| id != source.series_id.as_str());
            if new_series {
                if let Some(nfo_path) = find_tvshow_nfo(&series_dir).await {
                    self.enrich_nfo_item_best_effort(report, &source.series_id, &nfo_path)
                        .await;
                }
                report.images_found += self
                    .index_images(&source.series_id, find_series_images(&series_paths, None))
                    .await?;
                if let Some(last_series_id) = last_series_id.as_deref_mut() {
                    *last_series_id = source.series_id.clone();
                }
                if let Some(last_season_id) = last_season_id.as_deref_mut() {
                    last_season_id.clear();
                }
                if let Some(last_episode_id) = last_episode_id.as_deref_mut() {
                    last_episode_id.clear();
                }
            }

            let season_number = source.season_number.unwrap_or_default();
            let season_dir = media_path.parent().unwrap_or(&series_dir);
            let new_season = last_season_id
                .as_deref()
                .is_none_or(|id| id != source.season_id.as_str());
            let mut season_paths = series_paths;
            if season_dir != series_dir {
                let mut directory_paths = read_directory_paths(season_dir).await?;
                season_paths = season_paths
                    .iter()
                    .filter(|path| is_prefixed_season_image(path, season_number))
                    .cloned()
                    .collect();
                season_paths.append(&mut directory_paths);
            }
            if new_season {
                if let Some(nfo_path) =
                    find_season_nfo(&series_dir, season_dir, season_number).await
                {
                    self.enrich_nfo_item_best_effort(report, &source.season_id, &nfo_path)
                        .await;
                }
                report.images_found += self
                    .index_images(
                        &source.season_id,
                        find_series_images(&season_paths, Some(season_number)),
                    )
                    .await?;
                if let Some(last_season_id) = last_season_id.as_deref_mut() {
                    *last_season_id = source.season_id.clone();
                }
                if let Some(last_episode_id) = last_episode_id.as_deref_mut() {
                    last_episode_id.clear();
                }
            }

            let new_episode = last_episode_id
                .as_deref()
                .is_none_or(|id| id != source.episode_id.as_str());
            if new_episode {
                if let Some(nfo_path) = find_episode_nfo(&media_path).await {
                    self.enrich_nfo_item_best_effort(report, &source.episode_id, &nfo_path)
                        .await;
                }
                report.images_found += self
                    .index_images(
                        &source.episode_id,
                        find_episode_images(&season_paths, &media_path),
                    )
                    .await?;
                if let Some(last_episode_id) = last_episode_id.as_deref_mut() {
                    *last_episode_id = source.episode_id.clone();
                }
            }
        }
        Ok(())
    }

    async fn enrich_nfo_item_best_effort(
        &self,
        report: &mut MetadataReport,
        item_id: &str,
        nfo_path: &Path,
    ) {
        match self.enrich_nfo_item(item_id, nfo_path).await {
            Ok(nfo_report) => {
                let failed = nfo_report.nfo_failed > 0;
                report.merge(nfo_report);
                if failed {
                    report.mark_item_failed(item_id);
                }
            }
            Err(error) => {
                tracing::warn!(
                    item_id,
                    path = %nfo_path.display(),
                    %error,
                    "local NFO enrichment failed; continuing with remaining metadata"
                );
                report.nfo_failed += 1;
                report.mark_item_failed(item_id);
            }
        }
    }

    async fn enrich_nfo_item(
        &self,
        item_id: &str,
        nfo_path: &Path,
    ) -> Result<MetadataReport, MetadataError> {
        let mut report = MetadataReport::default();
        let fingerprint = nfo_fingerprint(nfo_path).await.ok();
        let already_checked = if let Some(fingerprint) = fingerprint.as_deref() {
            self.database
                .media_item_metadata_fingerprint(item_id)
                .await?
                .as_deref()
                == Some(fingerprint)
        } else {
            false
        };
        let rich_cache_missing = if let Some(local_nfo) = &self.local_nfo {
            !local_nfo
                .is_usable(item_id)
                .await
                .map_err(MetadataError::NfoCache)?
        } else {
            false
        };
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
        if let Some(current) = self.database.find_media_item_metadata(item_id).await? {
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
                && self
                    .database
                    .movie_metadata_identity_conflicts(
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
                });
            }
        }
        let source_fingerprint = nfo_content_fingerprint(&bytes);
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
        if let Some(fingerprint) = fingerprint.as_deref()
            && let Some(current) = self.database.find_media_item_metadata(item_id).await?
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
                    metadata_fingerprint: fingerprint,
                    provenance_json: &provenance_json,
                    locked_fields_json: &locked_fields_json,
                })
                .await?;
        }
        report.nfo_loaded = 1;
        Ok(report)
    }

    async fn index_images(
        &self,
        item_id: &str,
        images: Vec<LocalImage>,
    ) -> Result<usize, MetadataError> {
        let has_primary_artwork = images
            .iter()
            .any(|image| matches!(image.image_type, ImageType::Poster | ImageType::Thumb));
        let mut inserted_count = 0;
        for image in images {
            let file_size = fs::metadata(&image.path)
                .await
                .map_err(|source| MetadataError::Io {
                    path: image.path.clone(),
                    source,
                })?
                .len();
            let file_size =
                i64::try_from(file_size).map_err(|_| MetadataError::FileSizeOutOfRange {
                    path: image.path.clone(),
                    size: file_size,
                })?;
            let content_tag =
                image_content_tag(&image.path)
                    .await
                    .map_err(|source| MetadataError::Io {
                        path: image.path.clone(),
                        source,
                    })?;
            let dimensions = read_image_dimensions(&image.path).await;
            if self
                .database
                .insert_item_image(
                    item_id,
                    image.image_type.as_str(),
                    &image.path,
                    ItemImageMetadata {
                        file_size,
                        width: dimensions.map(|(width, _)| width),
                        height: dimensions.map(|(_, height)| height),
                        content_tag: &content_tag,
                        source: "LOCAL",
                    },
                )
                .await?
            {
                inserted_count += 1;
            }
        }
        if has_primary_artwork {
            self.database
                .set_poster_fallback_required(item_id, false)
                .await?;
        }
        Ok(inserted_count)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataReport {
    pub nfo_loaded: usize,
    pub nfo_failed: usize,
    pub nfo_skipped: usize,
    pub images_found: usize,
    pub(crate) failed_item_ids: Vec<String>,
}

impl MetadataReport {
    fn merge(&mut self, other: Self) {
        self.nfo_loaded += other.nfo_loaded;
        self.nfo_failed += other.nfo_failed;
        self.nfo_skipped += other.nfo_skipped;
        self.images_found += other.images_found;
        for item_id in other.failed_item_ids {
            self.mark_item_failed(&item_id);
        }
    }

    fn mark_item_failed(&mut self, item_id: &str) {
        if !self.failed_item_ids.iter().any(|failed| failed == item_id) {
            self.failed_item_ids.push(item_id.to_owned());
        }
    }
}

async fn image_content_tag(path: &Path) -> Result<String, std::io::Error> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    })
    .await
    .map_err(|error| std::io::Error::other(format!("image fingerprint worker failed: {error}")))?
}

pub(crate) async fn nfo_fingerprint(path: &Path) -> Result<Vec<u8>, std::io::Error> {
    let metadata = fs::metadata(path).await?;
    let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0);
    let path = path.to_string_lossy();
    Ok(compute_file_fingerprint(
        &path,
        size,
        modified_at,
        None,
        None,
    ))
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
            None => match stem.as_str() {
                "poster" => ImageType::Poster,
                "fanart" | "backdrop" => ImageType::Fanart,
                "logo" | "clearlogo" => ImageType::Logo,
                "thumb" | "thumbnail" => ImageType::Thumb,
                "banner" => ImageType::Banner,
                "disc" | "discart" => ImageType::Disc,
                "art" | "artwork" => ImageType::Art,
                "wallpaper" => ImageType::Wallpaper,
                _ => continue,
            },
            Some(number) => {
                let prefix = format!("season{number}");
                let padded_prefix = format!("season{number:02}");
                let is_poster = stem == "poster"
                    || stem == format!("{prefix}-poster")
                    || stem == format!("{padded_prefix}-poster");
                let is_fanart = stem == "fanart"
                    || stem == "backdrop"
                    || stem == format!("{prefix}-fanart")
                    || stem == format!("{padded_prefix}-fanart")
                    || stem == format!("{prefix}-backdrop")
                    || stem == format!("{padded_prefix}-backdrop");
                if is_poster {
                    ImageType::Poster
                } else if is_fanart {
                    ImageType::Fanart
                } else {
                    continue;
                }
            }
        };
        if images
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
            "poster" => ImageType::Poster,
            "fanart" | "backdrop" => ImageType::Fanart,
            "thumb" | "thumbnail" => ImageType::Thumb,
            "logo" | "clearlogo" => ImageType::Logo,
            "banner" => ImageType::Banner,
            "disc" | "discart" => ImageType::Disc,
            "art" | "artwork" => ImageType::Art,
            "wallpaper" => ImageType::Wallpaper,
            _ => continue,
        };
        if images
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
    .any(|candidate| stem == candidate)
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
    },
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
            Self::ConflictingMovieIdentity { item_id } => write!(
                formatter,
                "local movie NFO conflicts with another movie identity: {item_id}"
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
