use std::{
    collections::{BTreeMap, BTreeSet},
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
    TmdbLocalized,
    Fallback,
    LockedLocal,
}

impl MetadataSource {
    const fn priority(self) -> u8 {
        match self {
            Self::Fallback => 1,
            Self::TmdbLocalized => 2,
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
            if self.locked_fields.contains(&field) || self.has_value(field) {
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
                    && (force || !self.has_value(field))
                {
                    self.metadata.title = Some(value.to_owned());
                    self.provenance.insert(field, source);
                }
            }
            MetadataField::OriginalTitle => {
                if let Some(value) = non_empty(candidate.metadata.original_title.as_deref())
                    && (force || !self.has_value(field))
                {
                    self.metadata.original_title = Some(value.to_owned());
                    self.provenance.insert(field, source);
                }
            }
            MetadataField::Overview => {
                if let Some(value) = non_empty(candidate.metadata.overview.as_deref())
                    && (force || !self.has_value(field))
                {
                    self.metadata.overview = Some(value.to_owned());
                    self.provenance.insert(field, source);
                }
            }
            MetadataField::ProductionYear => {
                if let Some(value) = candidate.metadata.production_year
                    && (force || !self.has_value(field))
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
}

impl ImageType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Poster => "POSTER",
            Self::Fanart => "FANART",
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
        "fanart" => Some(ImageType::Fanart),
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
            .list_media_sources_for_library(&library_id.to_string())
            .await?;
        let mut report = MetadataReport::default();
        for source in sources {
            let media_path = PathBuf::from(&source.root_path).join(&source.relative_path);
            let nfo_path = find_nfo_path(&media_path).await;
            if let Some(nfo_path) = nfo_path {
                let fingerprint = nfo_fingerprint(&nfo_path).await.ok();
                let already_checked = if let Some(fingerprint) = fingerprint.as_deref() {
                    self.database
                        .media_item_metadata_fingerprint(&source.item_id)
                        .await?
                        .as_deref()
                        == Some(fingerprint)
                } else {
                    false
                };
                if already_checked {
                    report.nfo_skipped += 1;
                } else {
                    match fs::read(&nfo_path).await {
                        Err(_) => report.nfo_failed += 1,
                        Ok(bytes) => match parse_nfo(&bytes) {
                            Ok(metadata) => {
                                if let Some(fingerprint) = fingerprint.as_deref() {
                                    if let Some(current) = self
                                        .database
                                        .find_media_item_metadata(&source.item_id)
                                        .await?
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
                                                item_id: &source.item_id,
                                                title: state
                                                    .metadata
                                                    .title
                                                    .as_deref()
                                                    .unwrap_or(current.title.as_str()),
                                                original_title: state
                                                    .metadata
                                                    .original_title
                                                    .as_deref(),
                                                overview: state.metadata.overview.as_deref(),
                                                production_year: state
                                                    .metadata
                                                    .production_year
                                                    .map(i64::from),
                                                metadata_fingerprint: fingerprint,
                                                provenance_json: &provenance_json,
                                                locked_fields_json: &locked_fields_json,
                                            })
                                            .await?;
                                    }
                                }
                                report.nfo_loaded += 1;
                            }
                            Err(_) => {
                                if let Some(fingerprint) = fingerprint.as_deref() {
                                    self.database
                                        .mark_media_item_metadata_checked(
                                            &source.item_id,
                                            fingerprint,
                                        )
                                        .await?;
                                }
                                report.nfo_failed += 1;
                            }
                        },
                    }
                }
            }
            let mut entries = fs::read_dir(media_path.parent().unwrap_or(Path::new(".")))
                .await
                .map_err(|source| MetadataError::Io {
                    path: media_path.clone(),
                    source,
                })?;
            let mut image_paths = Vec::new();
            while let Some(entry) =
                entries
                    .next_entry()
                    .await
                    .map_err(|source| MetadataError::Io {
                        path: media_path.clone(),
                        source,
                    })?
            {
                image_paths.push(entry.path());
            }
            for image in find_local_images(image_paths) {
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
                let inserted = self
                    .database
                    .insert_item_image(
                        &source.item_id,
                        image.image_type.as_str(),
                        &image.path,
                        file_size,
                    )
                    .await?;
                if inserted {
                    report.images_found += 1;
                }
            }
        }
        Ok(report)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataReport {
    pub nfo_loaded: usize,
    pub nfo_failed: usize,
    pub nfo_skipped: usize,
    pub images_found: usize,
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
