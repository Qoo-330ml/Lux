use std::{
    fmt,
    io::Cursor,
    path::{Path, PathBuf},
};

use quick_xml::{escape::unescape, events::Event, reader::Reader};
use tokio::fs;

use crate::{
    domain::ids::LibraryId,
    storage::{Database, StorageError},
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
                match fs::read(&nfo_path)
                    .await
                    .map_err(NfoError::Io)
                    .and_then(|bytes| parse_nfo(&bytes))
                {
                    Ok(metadata) => {
                        self.database
                            .update_media_item_metadata(
                                &source.item_id,
                                metadata.title.as_deref(),
                                metadata.original_title.as_deref(),
                                metadata.overview.as_deref(),
                                metadata.production_year.map(i64::from),
                            )
                            .await?;
                        report.nfo_loaded += 1;
                    }
                    Err(_) => report.nfo_failed += 1,
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
    pub images_found: usize,
}

async fn find_nfo_path(media_path: &Path) -> Option<PathBuf> {
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
