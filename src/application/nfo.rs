use std::{
    collections::BTreeSet,
    fmt,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use quick_xml::{
    Writer,
    escape::escape,
    events::{BytesEnd, BytesStart, BytesText, Event},
    reader::Reader,
};
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
};
use uuid::Uuid;

use crate::application::metadata::{
    MetadataField, MetadataSource, MetadataState, NfoError, NfoMetadata, find_nfo_path,
    nfo_fingerprint, parse_nfo, series_directory,
};
use crate::storage::{Database, MediaMetadataUpdate, StorageError};

pub fn rewrite_nfo(original: &[u8], patch: &NfoMetadata) -> Result<Vec<u8>, NfoWriteError> {
    if original.is_empty() {
        return new_nfo(patch);
    }
    parse_nfo(original).map_err(NfoWriteError::Nfo)?;

    let mut reader = Reader::from_reader(original);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut saw_root = false;
    let mut active = None;
    let mut updated = BTreeSet::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => {
                if depth != 0 || !saw_root {
                    return Err(NfoWriteError::InvalidXml(
                        "NFO document does not contain a complete root element".to_owned(),
                    ));
                }
                break;
            }
            Ok(Event::Start(event)) => {
                if depth == 0 {
                    saw_root = true;
                }
                let field = (depth == 1)
                    .then(|| field_for_tag(event.name().as_ref()))
                    .flatten();
                writer
                    .write_event(Event::Start(event.to_owned()))
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                depth += 1;
                if let Some(field) = field {
                    if patch_value(patch, field).is_some() && !updated.contains(&field) {
                        active = Some(ActiveField {
                            field,
                            depth,
                            wrote_value: false,
                        });
                    }
                }
            }
            Ok(Event::Empty(event)) => {
                if depth == 0 {
                    saw_root = true;
                    writer
                        .write_event(Event::Start(event.to_owned()))
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                    append_missing_fields(&mut writer, patch, &mut updated)?;
                    writer
                        .write_event(Event::End(BytesEnd::new(
                            String::from_utf8_lossy(event.name().as_ref()).as_ref(),
                        )))
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                } else if depth == 1 {
                    let field = field_for_tag(event.name().as_ref());
                    if let Some(field) = field.filter(|field| patch_value(patch, *field).is_some())
                    {
                        if let Some(value) = patch_value(patch, field) {
                            write_field(&mut writer, field, &value)?;
                        }
                        updated.insert(field);
                    } else {
                        writer
                            .write_event(Event::Empty(event.to_owned()))
                            .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                    }
                } else {
                    writer
                        .write_event(Event::Empty(event.to_owned()))
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                }
            }
            Ok(Event::Text(event)) => {
                if let Some(active_field) = active.as_mut() {
                    if active_field.depth == depth {
                        if !active_field.wrote_value {
                            if let Some(value) = patch_value(patch, active_field.field) {
                                write_text(&mut writer, &value)?;
                                active_field.wrote_value = true;
                            }
                        }
                        buffer.clear();
                        continue;
                    }
                }
                writer
                    .write_event(Event::Text(event.to_owned()))
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
            }
            Ok(Event::CData(event)) => {
                if let Some(active_field) = active.as_mut() {
                    if active_field.depth == depth {
                        if !active_field.wrote_value {
                            if let Some(value) = patch_value(patch, active_field.field) {
                                write_text(&mut writer, &value)?;
                                active_field.wrote_value = true;
                            }
                        }
                        buffer.clear();
                        continue;
                    }
                }
                writer
                    .write_event(Event::CData(event.to_owned()))
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
            }
            Ok(Event::End(event)) => {
                if active
                    .as_ref()
                    .is_some_and(|active_field| active_field.depth == depth)
                {
                    if let Some(active_field) = active.take() {
                        if !active_field.wrote_value {
                            if let Some(value) = patch_value(patch, active_field.field) {
                                write_text(&mut writer, &value)?;
                            }
                        }
                        updated.insert(active_field.field);
                    }
                }
                if depth == 1 {
                    append_missing_fields(&mut writer, patch, &mut updated)?;
                }
                writer
                    .write_event(Event::End(event.to_owned()))
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                depth = depth.saturating_sub(1);
            }
            Ok(event) => {
                writer
                    .write_event(event.to_owned())
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
            }
            Err(error) => return Err(NfoWriteError::InvalidXml(error.to_string())),
        }
        buffer.clear();
    }
    Ok(writer.into_inner())
}

pub async fn write_nfo_atomically(target: &Path, patch: &NfoMetadata) -> Result<(), NfoWriteError> {
    write_nfo_atomically_with_hook(target, patch, None).await
}

#[derive(Clone)]
pub struct NfoWriteService {
    database: Database,
}

impl NfoWriteService {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn write_item_nfo(
        &self,
        item_id: &str,
        patch: &NfoMetadata,
    ) -> Result<NfoWriteReport, NfoWriteError> {
        let kind = self
            .database
            .find_media_item_kind(item_id)
            .await?
            .ok_or(NfoWriteError::ItemNotFound)?;
        let source = match kind.item_type.as_str() {
            "MOVIE" | "EPISODE" => {
                self.database
                    .find_metadata_writeback_source_path(item_id)
                    .await?
            }
            "SERIES" | "SEASON" => {
                self.database
                    .find_first_episode_source_path(item_id)
                    .await?
            }
            _ => None,
        }
        .ok_or(NfoWriteError::ItemNotFound)?;
        let root = fs::canonicalize(&source.root_path)
            .await
            .map_err(|error| io_error(Path::new(&source.root_path), error))?;
        let media_path = root.join(&source.relative_path);
        let media_path = fs::canonicalize(&media_path)
            .await
            .map_err(|error| io_error(&media_path, error))?;
        if !media_path.starts_with(&root) {
            return Err(NfoWriteError::PathOutsideRoot(media_path));
        }
        let directory = media_path
            .parent()
            .ok_or_else(|| NfoWriteError::PathOutsideRoot(media_path.clone()))?;
        let directory = fs::canonicalize(directory)
            .await
            .map_err(|error| io_error(directory, error))?;
        if !directory.starts_with(&root) {
            return Err(NfoWriteError::PathOutsideRoot(directory));
        }
        let target = match kind.item_type.as_str() {
            "MOVIE" => find_nfo_path(&media_path)
                .await
                .unwrap_or_else(|| directory.join("movie.nfo")),
            "EPISODE" => find_episode_nfo_target(&media_path, &directory).await,
            "SERIES" => {
                let series_dir = series_directory(&root, &source.relative_path)
                    .ok_or_else(|| NfoWriteError::PathOutsideRoot(directory.clone()))?;
                let series_dir = fs::canonicalize(&series_dir)
                    .await
                    .map_err(|error| io_error(&series_dir, error))?;
                if !series_dir.starts_with(&root) {
                    return Err(NfoWriteError::PathOutsideRoot(series_dir));
                }
                series_dir.join("tvshow.nfo")
            }
            "SEASON" => find_season_nfo_target(&directory, kind.season_number).await,
            _ => return Err(NfoWriteError::ItemNotFound),
        };
        let target_parent = target.parent().unwrap_or_else(|| Path::new("."));
        let target_parent = fs::canonicalize(target_parent)
            .await
            .map_err(|error| io_error(target_parent, error))?;
        if !target_parent.starts_with(&root) {
            return Err(NfoWriteError::PathOutsideRoot(target_parent));
        }
        write_nfo_atomically(&target, patch).await?;
        let fingerprint = nfo_fingerprint(&target)
            .await
            .map_err(|error| io_error(&target, error))?;
        self.database
            .mark_media_item_metadata_checked(item_id, &fingerprint)
            .await?;
        Ok(NfoWriteReport {
            path: target,
            fingerprint,
        })
    }
}

#[derive(Clone)]
pub struct MetadataWriteService {
    database: Database,
    nfo: NfoWriteService,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataWriteRequest {
    pub title: String,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub production_year: Option<i32>,
    pub locked_fields: BTreeSet<MetadataField>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataWriteResult {
    pub title: String,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub production_year: Option<i32>,
    pub locked_fields: BTreeSet<MetadataField>,
}

impl MetadataWriteService {
    pub fn new(database: Database) -> Self {
        Self {
            nfo: NfoWriteService::new(database.clone()),
            database,
        }
    }

    pub async fn write_item_metadata(
        &self,
        item_id: &str,
        request: MetadataWriteRequest,
    ) -> Result<MetadataWriteResult, NfoWriteError> {
        let current = self
            .database
            .find_media_item_metadata(item_id)
            .await?
            .ok_or(NfoWriteError::ItemNotFound)?;
        let mut title = request.title.trim().to_owned();
        if title.is_empty() {
            return Err(NfoWriteError::InvalidMetadata(
                "title must not be empty".to_owned(),
            ));
        }
        if title.len() > 512 {
            return Err(NfoWriteError::InvalidMetadata(
                "title is too long".to_owned(),
            ));
        }
        let original_title = normalize_metadata_text(request.original_title, 512)?;
        let overview = normalize_metadata_text(request.overview, 256 * 1024)?;
        if let Some(year) = request.production_year
            && !(1800..=2200).contains(&year)
        {
            return Err(NfoWriteError::InvalidMetadata(
                "production year is out of range".to_owned(),
            ));
        }

        let mut state = MetadataState::from_persisted(
            NfoMetadata {
                title: Some(current.title),
                original_title: current.original_title,
                overview: current.overview,
                production_year: current
                    .production_year
                    .and_then(|year| i32::try_from(year).ok()),
            },
            current.provenance_json.as_deref(),
            current.locked_fields_json.as_deref(),
        );
        state.metadata = NfoMetadata {
            title: Some(std::mem::take(&mut title)),
            original_title: original_title.clone(),
            overview: overview.clone(),
            production_year: request.production_year,
        };
        state.locked_fields = request.locked_fields;
        for field in [
            MetadataField::Title,
            MetadataField::OriginalTitle,
            MetadataField::Overview,
            MetadataField::ProductionYear,
        ] {
            let has_value = match field {
                MetadataField::Title => state.metadata.title.is_some(),
                MetadataField::OriginalTitle => state.metadata.original_title.is_some(),
                MetadataField::Overview => state.metadata.overview.is_some(),
                MetadataField::ProductionYear => state.metadata.production_year.is_some(),
            };
            if !has_value {
                state.provenance.remove(&field);
            } else if state.locked_fields.contains(&field) {
                state.provenance.insert(field, MetadataSource::LockedLocal);
            } else {
                state.provenance.insert(field, MetadataSource::LocalNfo);
            }
        }

        let report = self.nfo.write_item_nfo(item_id, &state.metadata).await?;
        let provenance_json = state.provenance_json();
        let locked_fields_json = state.locked_fields_json();
        self.database
            .update_media_item_metadata(MediaMetadataUpdate {
                item_id,
                title: state.metadata.title.as_deref().unwrap_or_default(),
                original_title: state.metadata.original_title.as_deref(),
                overview: state.metadata.overview.as_deref(),
                production_year: state.metadata.production_year.map(i64::from),
                metadata_fingerprint: &report.fingerprint,
                provenance_json: &provenance_json,
                locked_fields_json: &locked_fields_json,
            })
            .await?;
        Ok(MetadataWriteResult {
            title: state.metadata.title.unwrap_or_default(),
            original_title: state.metadata.original_title,
            overview: state.metadata.overview,
            production_year: state.metadata.production_year,
            locked_fields: state.locked_fields,
        })
    }
}

fn normalize_metadata_text(
    value: Option<String>,
    max_bytes: usize,
) -> Result<Option<String>, NfoWriteError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim().to_owned();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > max_bytes {
        return Err(NfoWriteError::InvalidMetadata(
            "metadata field is too long".to_owned(),
        ));
    }
    Ok(Some(value))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NfoWriteReport {
    pub path: PathBuf,
    pub fingerprint: Vec<u8>,
}

async fn write_nfo_atomically_with_hook(
    target: &Path,
    patch: &NfoMetadata,
    before_replace: Option<fn(&Path) -> std::io::Result<()>>,
) -> Result<(), NfoWriteError> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let target_is_symlink = fs::symlink_metadata(target)
        .await
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false);
    if target_is_symlink {
        return Err(NfoWriteError::SymlinkTarget(target.to_owned()));
    }
    let before = file_stamp(target).await?;
    let original = match fs::read(target).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(source) => return Err(io_error(target, source)),
    };
    let rewritten = rewrite_nfo(&original, patch)?;
    let temporary = parent.join(format!(".lux-{}.nfo.tmp", Uuid::now_v7()));
    let result = async {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .map_err(|source| io_error(&temporary, source))?;
        file.write_all(&rewritten)
            .await
            .map_err(|source| io_error(&temporary, source))?;
        file.sync_all()
            .await
            .map_err(|source| io_error(&temporary, source))?;
        drop(file);
        if let Some(before_replace) = before_replace {
            before_replace(target).map_err(|source| io_error(target, source))?;
        }
        let current_stamp = file_stamp(target).await?;
        let current_content = match fs::read(target).await {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => return Err(io_error(target, source)),
        };
        let unchanged = match (&before, current_content.as_ref()) {
            (None, None) => true,
            (Some(before), Some(current)) => current == &original && current_stamp == Some(*before),
            _ => false,
        };
        if !unchanged {
            return Err(NfoWriteError::ConcurrentModification(target.to_owned()));
        }
        fs::rename(&temporary, target)
            .await
            .map_err(|source| io_error(target, source))?;
        let directory = fs::File::open(parent)
            .await
            .map_err(|source| io_error(parent, source))?;
        directory
            .sync_all()
            .await
            .map_err(|source| io_error(parent, source))?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result
}

fn new_nfo(patch: &NfoMetadata) -> Result<Vec<u8>, NfoWriteError> {
    let mut writer = Writer::new(Vec::new());
    writer
        .write_event(Event::Start(BytesStart::new("movie")))
        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
    let mut updated = BTreeSet::new();
    append_missing_fields(&mut writer, patch, &mut updated)?;
    writer
        .write_event(Event::End(BytesEnd::new("movie")))
        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
    Ok(writer.into_inner())
}

fn append_missing_fields(
    writer: &mut Writer<Vec<u8>>,
    patch: &NfoMetadata,
    updated: &mut BTreeSet<MetadataField>,
) -> Result<(), NfoWriteError> {
    for field in [
        MetadataField::Title,
        MetadataField::OriginalTitle,
        MetadataField::Overview,
        MetadataField::ProductionYear,
    ] {
        if updated.contains(&field) {
            continue;
        }
        if let Some(value) = patch_value(patch, field) {
            write_field(writer, field, &value)?;
            updated.insert(field);
        }
    }
    Ok(())
}

fn write_field(
    writer: &mut Writer<Vec<u8>>,
    field: MetadataField,
    value: &str,
) -> Result<(), NfoWriteError> {
    writer
        .write_event(Event::Start(BytesStart::new(field_tag(field))))
        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
    write_text(writer, value)?;
    writer
        .write_event(Event::End(BytesEnd::new(field_tag(field))))
        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
    Ok(())
}

fn write_text(writer: &mut Writer<Vec<u8>>, value: &str) -> Result<(), NfoWriteError> {
    let escaped = escape(value).into_owned();
    writer
        .write_event(Event::Text(BytesText::from_escaped(escaped)))
        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
    Ok(())
}

fn patch_value(patch: &NfoMetadata, field: MetadataField) -> Option<String> {
    match field {
        MetadataField::Title => patch.title.clone(),
        MetadataField::OriginalTitle => patch.original_title.clone(),
        MetadataField::Overview => patch.overview.clone(),
        MetadataField::ProductionYear => patch.production_year.map(|year| year.to_string()),
    }
    .filter(|value| !value.trim().is_empty())
}

async fn find_episode_nfo_target(media_path: &Path, directory: &Path) -> PathBuf {
    let same_name = media_path.with_extension("nfo");
    if fs::try_exists(&same_name).await.unwrap_or(false) {
        return same_name;
    }
    let episode_nfo = directory.join("episode.nfo");
    if fs::try_exists(&episode_nfo).await.unwrap_or(false) {
        return episode_nfo;
    }
    same_name
}

async fn find_season_nfo_target(directory: &Path, season_number: Option<i64>) -> PathBuf {
    let number = season_number.unwrap_or_default();
    let generic = directory.join("season.nfo");
    if fs::try_exists(&generic).await.unwrap_or(false) {
        return generic;
    }
    let names = if number == 0 {
        vec!["specials.nfo".to_owned(), "season00.nfo".to_owned()]
    } else {
        vec![
            format!("season{number:02}.nfo"),
            format!("season{number}.nfo"),
        ]
    };
    for name in &names {
        let path = directory.join(name);
        if fs::try_exists(&path).await.unwrap_or(false) {
            return path;
        }
    }
    directory.join(&names[0])
}

fn field_for_tag(tag: &[u8]) -> Option<MetadataField> {
    match tag {
        b"title" => Some(MetadataField::Title),
        b"originaltitle" | b"original_title" => Some(MetadataField::OriginalTitle),
        b"year" => Some(MetadataField::ProductionYear),
        b"plot" | b"overview" => Some(MetadataField::Overview),
        _ => None,
    }
}

fn field_tag(field: MetadataField) -> &'static str {
    match field {
        MetadataField::Title => "title",
        MetadataField::OriginalTitle => "originaltitle",
        MetadataField::Overview => "plot",
        MetadataField::ProductionYear => "year",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileStamp {
    size: u64,
    modified_at: u128,
}

async fn file_stamp(path: &Path) -> Result<Option<FileStamp>, NfoWriteError> {
    let metadata = match fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(path, source)),
    };
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    Ok(Some(FileStamp {
        size: metadata.len(),
        modified_at,
    }))
}

fn io_error(path: &Path, source: std::io::Error) -> NfoWriteError {
    NfoWriteError::Io {
        path: path.to_owned(),
        source,
    }
}

#[derive(Debug)]
pub enum NfoWriteError {
    Nfo(NfoError),
    ItemNotFound,
    InvalidMetadata(String),
    InvalidXml(String),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    SymlinkTarget(PathBuf),
    PathOutsideRoot(PathBuf),
    ConcurrentModification(PathBuf),
    Storage(StorageError),
}

impl fmt::Display for NfoWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nfo(error) => error.fmt(formatter),
            Self::ItemNotFound => formatter.write_str("media item has no local media source"),
            Self::InvalidMetadata(message) => formatter.write_str(message),
            Self::InvalidXml(error) => write!(formatter, "NFO rewrite failed: {error}"),
            Self::Io { path, source } => {
                write!(formatter, "NFO write '{}': {source}", path.display())
            }
            Self::SymlinkTarget(path) => {
                write!(formatter, "NFO target is a symlink: {}", path.display())
            }
            Self::PathOutsideRoot(path) => {
                write!(
                    formatter,
                    "NFO path is outside the library root: {}",
                    path.display()
                )
            }
            Self::ConcurrentModification(path) => {
                write!(formatter, "NFO changed while writing: {}", path.display())
            }
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for NfoWriteError {}

impl From<NfoError> for NfoWriteError {
    fn from(error: NfoError) -> Self {
        Self::Nfo(error)
    }
}

impl From<StorageError> for NfoWriteError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

struct ActiveField {
    field: MetadataField,
    depth: usize,
    wrote_value: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mutate_target(path: &Path) -> std::io::Result<()> {
        std::fs::write(path, b"<movie><title>external</title></movie>")
    }

    #[tokio::test]
    async fn concurrent_change_is_rejected_before_atomic_replace() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("movie.nfo");
        tokio::fs::write(&target, b"<movie><title>old</title></movie>")
            .await
            .expect("initial nfo");

        let result = write_nfo_atomically_with_hook(
            &target,
            &NfoMetadata {
                title: Some("new".to_owned()),
                ..NfoMetadata::default()
            },
            Some(mutate_target),
        )
        .await;

        assert!(matches!(
            result,
            Err(NfoWriteError::ConcurrentModification(_))
        ));
        let content = tokio::fs::read_to_string(&target).await.expect("target");
        assert!(content.contains("external"));
    }

    #[test]
    fn oversized_metadata_text_is_rejected_instead_of_being_dropped() {
        let value = Some("x".repeat(513));
        let result = normalize_metadata_text(value, 512);

        assert!(matches!(
            result,
            Err(NfoWriteError::InvalidMetadata(message)) if message == "metadata field is too long"
        ));
    }
}
