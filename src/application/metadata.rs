use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fmt,
    io::Cursor,
    path::{Path, PathBuf},
};

use quick_xml::{escape::unescape, events::Event, reader::Reader};
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::{
    application::scanner::compute_file_fingerprint,
    domain::ids::LibraryId,
    storage::{Database, MediaMetadataUpdate, StorageError},
};

const MAX_NFO_BYTES: usize = 1024 * 1024;
const MAX_XML_EVENTS: usize = 20_000;
const MAX_FIELD_BYTES: usize = 256 * 1024;

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
            self.has_value(*field)
                && self
                    .provenance
                    .get(field)
                    .is_some_and(|source| *source != MetadataSource::Fallback)
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
    if bytes.len() > MAX_NFO_BYTES {
        return Err(NfoError::TooLarge);
    }
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut current_field = None;
    let mut metadata = NfoMetadata::default();
    let mut event_count = 0;
    let mut depth = 0_usize;
    loop {
        event_count += 1;
        if event_count > MAX_XML_EVENTS {
            return Err(NfoError::TooManyEvents);
        }
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => {
                if depth != 0 {
                    return Err(NfoError::Unbalanced);
                }
                break;
            }
            Ok(Event::Start(event)) => {
                depth = depth.saturating_add(1);
                current_field = recognized_field(event.name().as_ref());
            }
            Ok(Event::End(_)) => {
                if depth == 0 {
                    return Err(NfoError::Unbalanced);
                }
                depth -= 1;
                current_field = None;
            }
            Ok(Event::Text(event)) => {
                if let Some(field) = current_field {
                    let decoded = event
                        .decode()
                        .map_err(|error| NfoError::Xml(error.to_string()))?;
                    let value = unescape(decoded.as_ref())
                        .map_err(|error| NfoError::Xml(error.to_string()))?
                        .trim()
                        .to_owned();
                    if value.len() > MAX_FIELD_BYTES {
                        return Err(NfoError::FieldTooLarge);
                    }
                    assign_field(&mut metadata, field, value);
                }
            }
            Ok(Event::CData(event)) => {
                if let Some(field) = current_field {
                    let value = event
                        .decode()
                        .map_err(|error| NfoError::Xml(error.to_string()))?
                        .trim()
                        .to_owned();
                    if value.len() > MAX_FIELD_BYTES {
                        return Err(NfoError::FieldTooLarge);
                    }
                    assign_field(&mut metadata, field, value);
                }
            }
            Ok(Event::DocType(_)) => return Err(NfoError::DocTypeNotAllowed),
            Ok(_) => {}
            Err(error) => return Err(NfoError::Xml(error.to_string())),
        }
        buffer.clear();
    }
    Ok(metadata)
}

#[derive(Clone, Copy)]
enum NfoField {
    Title,
    OriginalTitle,
    Year,
    Overview,
}

fn recognized_field(name: &[u8]) -> Option<NfoField> {
    match name {
        b"title" => Some(NfoField::Title),
        b"originaltitle" | b"original_title" => Some(NfoField::OriginalTitle),
        b"year" => Some(NfoField::Year),
        b"plot" | b"overview" => Some(NfoField::Overview),
        _ => None,
    }
}

fn assign_field(metadata: &mut NfoMetadata, field: NfoField, value: String) {
    if value.is_empty() {
        return;
    }
    match field {
        NfoField::Title => metadata.title = Some(value),
        NfoField::OriginalTitle => metadata.original_title = Some(value),
        NfoField::Year => {
            metadata.production_year = value
                .parse::<i32>()
                .ok()
                .filter(|year| (1800..=2200).contains(year));
        }
        NfoField::Overview => metadata.overview = Some(value),
    }
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
    let mut images = Vec::new();
    for path in paths {
        let path = path.as_ref();
        let Some(image_type) = image_type_for(path) else {
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
    images
}

fn image_type_for(path: &Path) -> Option<ImageType> {
    let stem = path.file_stem()?.to_str()?.to_ascii_lowercase();
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(extension.as_str(), "jpg" | "jpeg" | "png" | "webp") {
        return None;
    }
    match stem.as_str() {
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
}

impl MetadataEnricher {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn enrich_movie_library(
        &self,
        library_id: LibraryId,
    ) -> Result<MetadataReport, MetadataError> {
        let sources = self
            .database
            .list_movie_metadata_sources(&library_id.to_string())
            .await?;
        let mut report = MetadataReport::default();
        for source in sources {
            let media_path = PathBuf::from(&source.root_path).join(&source.relative_path);
            match self.enrich_movie_nfo(&source.item_id, &media_path).await {
                Ok(nfo_report) => report.merge(nfo_report),
                Err(error) => {
                    tracing::warn!(
                        item_id = %source.item_id,
                        %error,
                        "local movie NFO failed; continuing with images and remaining items"
                    );
                    report.nfo_failed += 1;
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
                }
            }
        }
        Ok(report)
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
        let mut report = MetadataReport::default();
        let Some(nfo_path) = find_nfo_path(media_path).await else {
            return Ok(report);
        };
        let fingerprint = nfo_fingerprint(&nfo_path).await.ok();
        let already_checked = if let Some(fingerprint) = fingerprint.as_deref() {
            self.database
                .media_item_metadata_fingerprint(item_id)
                .await?
                .as_deref()
                == Some(fingerprint)
        } else {
            false
        };
        if already_checked {
            report.nfo_skipped = 1;
            return Ok(report);
        }

        let bytes = match fs::read(&nfo_path).await {
            Ok(bytes) => bytes,
            Err(_) => {
                report.nfo_failed = 1;
                return Ok(report);
            }
        };
        let metadata = match parse_nfo(&bytes) {
            Ok(metadata) => metadata,
            Err(_) => {
                if let Some(fingerprint) = fingerprint.as_deref() {
                    self.database
                        .mark_media_item_metadata_checked(item_id, fingerprint)
                        .await?;
                }
                report.nfo_failed = 1;
                return Ok(report);
            }
        };
        if let Some(fingerprint) = fingerprint.as_deref()
            && let Some(current) = self.database.find_media_item_metadata(item_id).await?
        {
            let current_metadata = NfoMetadata {
                title: Some(current.title.clone()),
                original_title: current.original_title,
                overview: current.overview,
                production_year: current
                    .production_year
                    .and_then(|year| i32::try_from(year).ok()),
            };
            let mut state = MetadataState::from_persisted(
                current_metadata,
                current.provenance_json.as_deref(),
                current.locked_fields_json.as_deref(),
            );
            state.apply_automatic(&MetadataCandidate {
                source: MetadataSource::LocalNfo,
                metadata,
            });
            let provenance_json = state.provenance_json();
            let locked_fields_json = state.locked_fields_json();
            self.database
                .update_media_item_metadata(MediaMetadataUpdate {
                    item_id,
                    title: state
                        .metadata
                        .title
                        .as_deref()
                        .unwrap_or(current.title.as_str()),
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

    async fn index_movie_images(
        &self,
        item_id: &str,
        media_path: &Path,
    ) -> Result<usize, MetadataError> {
        let image_paths =
            read_directory_paths(media_path.parent().unwrap_or(Path::new("."))).await?;
        let mut inserted_count = 0;
        for image in find_local_images(image_paths) {
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
            match self
                .database
                .insert_item_image(item_id, image.image_type.as_str(), &image.path, file_size)
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
        Ok(inserted_count)
    }

    pub async fn enrich_series_library(
        &self,
        library_id: LibraryId,
    ) -> Result<MetadataReport, MetadataError> {
        let sources = self
            .database
            .list_series_metadata_sources(&library_id.to_string())
            .await?;
        let mut report = MetadataReport::default();
        let mut series_seen = HashSet::new();
        let mut seasons_seen = HashSet::new();
        let mut episodes_seen = HashSet::new();
        let mut season_image_paths = BTreeMap::<String, Vec<PathBuf>>::new();
        for source in sources {
            let root = PathBuf::from(&source.root_path);
            let media_path = root.join(&source.relative_path);
            let Some(series_dir) = series_directory(&root, &source.relative_path) else {
                continue;
            };
            let series_paths = read_directory_paths(&series_dir).await?;
            if series_seen.insert(source.series_id.clone()) {
                if let Some(nfo_path) = find_tvshow_nfo(&series_dir).await {
                    self.enrich_nfo_item_best_effort(&mut report, &source.series_id, &nfo_path)
                        .await;
                }
                report.images_found += self
                    .index_images(&source.series_id, find_series_images(&series_paths, None))
                    .await?;
            }

            let season_number = source.season_number.unwrap_or_default();
            let season_key = format!("{}:{season_number}", source.season_id);
            let season_dir = media_path.parent().unwrap_or(&series_dir);
            if !season_image_paths.contains_key(&source.season_id) {
                let mut paths = series_paths.clone();
                if season_dir != series_dir {
                    let mut directory_paths = read_directory_paths(season_dir).await?;
                    paths = series_paths
                        .iter()
                        .filter(|path| is_prefixed_season_image(path, season_number))
                        .cloned()
                        .collect();
                    paths.append(&mut directory_paths);
                }
                season_image_paths.insert(source.season_id.clone(), paths);
            }
            let Some(season_paths) = season_image_paths.get(&source.season_id) else {
                continue;
            };
            if seasons_seen.insert(season_key) {
                if let Some(nfo_path) =
                    find_season_nfo(&series_dir, season_dir, season_number).await
                {
                    self.enrich_nfo_item_best_effort(&mut report, &source.season_id, &nfo_path)
                        .await;
                }
                report.images_found += self
                    .index_images(
                        &source.season_id,
                        find_series_images(season_paths, Some(season_number)),
                    )
                    .await?;
            }

            if episodes_seen.insert(source.episode_id.clone()) {
                if let Some(nfo_path) = find_episode_nfo(&media_path).await {
                    self.enrich_nfo_item_best_effort(&mut report, &source.episode_id, &nfo_path)
                        .await;
                }
                report.images_found += self
                    .index_images(
                        &source.episode_id,
                        find_episode_images(season_paths, &media_path),
                    )
                    .await?;
            }
        }
        Ok(report)
    }

    async fn enrich_nfo_item_best_effort(
        &self,
        report: &mut MetadataReport,
        item_id: &str,
        nfo_path: &Path,
    ) {
        match self.enrich_nfo_item(item_id, nfo_path).await {
            Ok(nfo_report) => report.merge(nfo_report),
            Err(error) => {
                tracing::warn!(
                    item_id,
                    path = %nfo_path.display(),
                    %error,
                    "local NFO enrichment failed; continuing with remaining metadata"
                );
                report.nfo_failed += 1;
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
        if let Some(fingerprint) = fingerprint.as_deref()
            && self
                .database
                .media_item_metadata_fingerprint(item_id)
                .await?
                .as_deref()
                == Some(fingerprint)
        {
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
        let metadata = match parse_nfo(&bytes) {
            Ok(metadata) => metadata,
            Err(_) => {
                if let Some(fingerprint) = fingerprint.as_deref() {
                    self.database
                        .mark_media_item_metadata_checked(item_id, fingerprint)
                        .await?;
                }
                report.nfo_failed = 1;
                return Ok(report);
            }
        };
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
                metadata,
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
            if self
                .database
                .insert_item_image(item_id, image.image_type.as_str(), &image.path, file_size)
                .await?
            {
                inserted_count += 1;
            }
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
}

impl MetadataReport {
    fn merge(&mut self, other: Self) {
        self.nfo_loaded += other.nfo_loaded;
        self.nfo_failed += other.nfo_failed;
        self.nfo_skipped += other.nfo_skipped;
        self.images_found += other.images_found;
    }
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
    fs::try_exists(&same_name).await.ok()?.then_some(same_name)
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
        }
    }
}

impl std::error::Error for MetadataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::FileSizeOutOfRange { .. } => None,
            Self::Storage(error) => Some(error),
        }
    }
}

impl From<StorageError> for MetadataError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
