use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use quick_xml::{
    Decoder, Writer,
    escape::{escape, unescape},
    events::{BytesEnd, BytesStart, BytesText, Event},
    reader::Reader,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
    sync::{Mutex, OnceCell},
};
use uuid::Uuid;

use crate::application::metadata::{
    MetadataField, MetadataSource, MetadataState, NfoError, NfoMetadata, find_nfo_path,
    nfo_fingerprint, nfo_fingerprint_from_stamp, parse_nfo, series_directory,
};
use crate::application::metadata_paths::{library_item_directory, metadata_root};
use crate::application::metadata_writeback::item_metadata_writeback_enabled;
use crate::application::people::ActorCredit;
use crate::application::probe::{MediaProbeResult, MediaStreamResult, StreamType};
use crate::storage::{
    Database, MediaMetadataUpdate, StorageError, StoredMediaSourcePath, StoredMediaWritebackContext,
};

#[derive(Clone, Default)]
pub(crate) struct LocalNfoProjectionCache {
    entries: Arc<Mutex<HashMap<PathBuf, Arc<OnceCell<CachedNfoProjectionResult>>>>>,
}

type CachedNfoProjectionResult = Result<Option<LocalNfoProjection>, CachedNfoProjectionError>;

#[derive(Clone)]
enum CachedNfoProjectionError {
    Io {
        path: PathBuf,
        kind: std::io::ErrorKind,
        message: String,
    },
    Parse(CachedNfoParseError),
    Other(String),
}

#[derive(Clone)]
enum CachedNfoParseError {
    TooLarge,
    TooManyEvents,
    FieldTooLarge,
    DocTypeNotAllowed,
    Unbalanced,
    Xml(String),
    Io {
        kind: std::io::ErrorKind,
        message: String,
    },
}

impl CachedNfoProjectionError {
    fn from_write_error(error: NfoWriteError) -> Self {
        match error {
            NfoWriteError::Io { path, source } => Self::Io {
                path,
                kind: source.kind(),
                message: source.to_string(),
            },
            NfoWriteError::Nfo(error) => Self::Parse(match error {
                NfoError::TooLarge => CachedNfoParseError::TooLarge,
                NfoError::TooManyEvents => CachedNfoParseError::TooManyEvents,
                NfoError::FieldTooLarge => CachedNfoParseError::FieldTooLarge,
                NfoError::DocTypeNotAllowed => CachedNfoParseError::DocTypeNotAllowed,
                NfoError::Unbalanced => CachedNfoParseError::Unbalanced,
                NfoError::Xml(message) => CachedNfoParseError::Xml(message),
                NfoError::Io(source) => CachedNfoParseError::Io {
                    kind: source.kind(),
                    message: source.to_string(),
                },
            }),
            other => Self::Other(other.to_string()),
        }
    }

    fn into_write_error(self) -> NfoWriteError {
        match self {
            Self::Io {
                path,
                kind,
                message,
            } => NfoWriteError::Io {
                path,
                source: std::io::Error::new(kind, message),
            },
            Self::Parse(error) => NfoWriteError::Nfo(match error {
                CachedNfoParseError::TooLarge => NfoError::TooLarge,
                CachedNfoParseError::TooManyEvents => NfoError::TooManyEvents,
                CachedNfoParseError::FieldTooLarge => NfoError::FieldTooLarge,
                CachedNfoParseError::DocTypeNotAllowed => NfoError::DocTypeNotAllowed,
                CachedNfoParseError::Unbalanced => NfoError::Unbalanced,
                CachedNfoParseError::Xml(message) => NfoError::Xml(message),
                CachedNfoParseError::Io { kind, message } => {
                    NfoError::Io(std::io::Error::new(kind, message))
                }
            }),
            Self::Other(message) => NfoWriteError::InvalidMetadata(message),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalNfoCredit {
    pub provider_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct LocalNfoDetails {
    pub rating: Option<f64>,
    pub votes: Option<i64>,
    pub tagline: Option<String>,
    pub premiered: Option<String>,
    #[serde(rename = "releaseDate")]
    pub release_date: Option<String>,
    pub runtime: Option<i32>,
    pub status: Option<String>,
    pub original_language: Option<String>,
    pub aired: Option<String>,
    pub last_air_date: Option<String>,
    pub website: Option<String>,
    pub set_name: Option<String>,
    pub set_id: Option<String>,
    pub certification: Option<String>,
    pub countries: Vec<String>,
    pub genres: Vec<String>,
    pub studios: Vec<String>,
    pub provider_ids: BTreeMap<String, String>,
    pub directors: Vec<LocalNfoCredit>,
    pub writers: Vec<LocalNfoCredit>,
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
    pub trailers: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalNfoProjection {
    pub metadata: NfoMetadata,
    pub details: LocalNfoDetails,
    pub actors: Vec<ActorCredit>,
}

/// Compatibility alias for callers that used the original movie-only name.
pub type MovieNfoCredit = LocalNfoCredit;
/// Compatibility alias for callers that used the original movie-only name.
pub type MovieNfoDetails = LocalNfoDetails;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MovieNfoMetadata {
    pub base: NfoMetadata,
    pub rating: Option<f64>,
    pub votes: Option<i64>,
    pub tagline: Option<String>,
    pub premiered: Option<String>,
    pub releasedate: Option<String>,
    pub last_air_date: Option<String>,
    pub runtime: Option<i32>,
    pub status: Option<String>,
    pub original_language: Option<String>,
    pub website: Option<String>,
    pub set_name: Option<String>,
    pub set_id: Option<String>,
    pub poster_url: Option<String>,
    pub fanart_url: Option<String>,
    pub certification: Option<String>,
    pub countries: Vec<String>,
    pub genres: Vec<String>,
    pub studios: Vec<String>,
    pub provider_ids: BTreeMap<String, String>,
    pub directors: Vec<MovieNfoCredit>,
    pub writers: Vec<MovieNfoCredit>,
    pub actors: Vec<ActorCredit>,
    pub trailers: Vec<String>,
}

const MAX_LOCAL_NFO_BYTES: usize = 1024 * 1024;
const MAX_LOCAL_NFO_EVENTS: usize = 20_000;
const LOCAL_NFO_CACHE_SCHEMA_VERSION: u8 = 1;
const MAX_MOVIE_NFO_ACTORS: usize = 100;
const MAX_MOVIE_NFO_STREAMS: usize = 128;
const MAX_MOVIE_ACTOR_FIELD_BYTES: usize = 256 * 1024;
const MAX_MOVIE_NFO_DETAILS_ITEMS: usize = 64;
const MAX_MOVIE_NFO_DETAILS_TEXT_BYTES: usize = 256 * 1024;
const MAX_MOVIE_NFO_DETAILS_URL_BYTES: usize = 2048;
const MAX_MOVIE_NFO_DETAILS_ID_BYTES: usize = 256;

/// Parses all local NFO projections in one bounded XML pass.
///
/// Background enrichment uses this entry point so base metadata, rich detail
/// fields, and actor relations always come from the same source revision.
pub fn parse_local_nfo_projection(bytes: &[u8]) -> Result<LocalNfoProjection, NfoError> {
    parse_local_nfo_projection_inner(bytes, false).map(|(projection, _)| projection)
}

pub(crate) fn parse_local_nfo_projection_with_semantic_fingerprint(
    bytes: &[u8],
) -> Result<(LocalNfoProjection, Vec<u8>), NfoError> {
    parse_local_nfo_projection_inner(bytes, true)
}

fn parse_local_nfo_projection_inner(
    bytes: &[u8],
    include_semantic_fingerprint: bool,
) -> Result<(LocalNfoProjection, Vec<u8>), NfoError> {
    if bytes.len() > MAX_LOCAL_NFO_BYTES {
        return Err(NfoError::TooLarge);
    }

    let mut reader = Reader::from_reader(Cursor::new(bytes));
    // Keep the established projection trimming behavior for callers that do
    // not need a semantic fingerprint. The semantic path reads full text and
    // applies the old trimming only to the metadata projection below.
    reader.config_mut().trim_text(!include_semantic_fingerprint);
    let mut buffer = Vec::new();
    let mut projection = LocalNfoProjection::default();
    let mut active_direct = None;
    let mut actor_depth = None;
    let mut active_actor = None;
    let mut current_actor = None;
    let mut depth = 0_usize;
    let mut event_count = 0_usize;
    let mut semantic_tokens = include_semantic_fingerprint.then(Vec::new);

    loop {
        event_count = event_count.saturating_add(1);
        if event_count > MAX_LOCAL_NFO_EVENTS {
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
                if let Some(tokens) = semantic_tokens.as_mut() {
                    tokens.push(NfoSemanticToken::start(&event, reader.decoder())?);
                }
                depth = depth.saturating_add(1);
                if depth == 2 && event.name().as_ref() == b"actor" {
                    actor_depth = Some(depth);
                    current_actor = Some(ParsedMovieActor::default());
                    active_direct = None;
                } else if actor_depth == Some(depth.saturating_sub(1)) {
                    active_actor =
                        movie_actor_field(event.name().as_ref()).map(|field| ActiveActorValue {
                            field,
                            text: String::new(),
                        });
                } else if depth == 2 {
                    active_direct = direct_value_kind(&event)?;
                }
            }
            Ok(Event::Text(event)) => {
                let decoded = event
                    .decode()
                    .map_err(|error| NfoError::Xml(error.to_string()))?;
                let value =
                    unescape(decoded.as_ref()).map_err(|error| NfoError::Xml(error.to_string()))?;
                if let Some(tokens) = semantic_tokens.as_mut() {
                    let content = event
                        .xml10_content()
                        .map_err(|error| NfoError::Xml(error.to_string()))?;
                    let semantic_value = unescape(content.as_ref())
                        .map_err(|error| NfoError::Xml(error.to_string()))?;
                    if depth != 0 || !is_xml_whitespace(&semantic_value) {
                        push_nfo_semantic_text(tokens, semantic_value.as_ref());
                    }
                }
                append_projection_text(
                    depth,
                    actor_depth,
                    active_direct.as_mut(),
                    active_actor.as_mut(),
                    value.as_ref(),
                )?;
            }
            Ok(Event::CData(event)) => {
                let value = event
                    .decode()
                    .map_err(|error| NfoError::Xml(error.to_string()))?;
                if let Some(tokens) = semantic_tokens.as_mut() {
                    let content = event
                        .xml10_content()
                        .map_err(|error| NfoError::Xml(error.to_string()))?;
                    push_nfo_semantic_text(tokens, content.as_ref());
                }
                append_projection_text(
                    depth,
                    actor_depth,
                    active_direct.as_mut(),
                    active_actor.as_mut(),
                    value.as_ref(),
                )?;
            }
            Ok(Event::End(event)) => {
                if let Some(tokens) = semantic_tokens.as_mut() {
                    let name = reader
                        .decoder()
                        .decode(event.name().as_ref())
                        .map_err(|error| NfoError::Xml(error.to_string()))?
                        .into_owned()
                        .into_bytes();
                    tokens.push(NfoSemanticToken::End(name));
                }
                if actor_depth == Some(depth) && event.name().as_ref() == b"actor" {
                    if let Some(actor) = current_actor.take() {
                        push_parsed_actor(&mut projection.actors, actor);
                    }
                    actor_depth = None;
                    active_actor = None;
                } else if actor_depth == Some(depth.saturating_sub(1)) {
                    if let (Some(actor), Some(active)) =
                        (current_actor.as_mut(), active_actor.take())
                    {
                        assign_movie_actor_field(
                            actor,
                            active.field,
                            active.text.trim().to_owned(),
                        )?;
                    }
                } else if depth == 2
                    && let Some(active) = active_direct.take()
                {
                    let raw_value = active.text.trim().to_owned();
                    assign_direct_value(&mut projection, active, &raw_value)?;
                }
                if depth == 0 {
                    return Err(NfoError::Unbalanced);
                }
                depth -= 1;
            }
            Ok(Event::Empty(event)) => {
                if let Some(tokens) = semantic_tokens.as_mut() {
                    let decoder = reader.decoder();
                    tokens.push(NfoSemanticToken::start(&event, decoder)?);
                    let name = decoder
                        .decode(event.name().as_ref())
                        .map_err(|error| NfoError::Xml(error.to_string()))?
                        .into_owned()
                        .into_bytes();
                    tokens.push(NfoSemanticToken::End(name));
                }
            }
            Ok(Event::PI(event)) => {
                if let Some(tokens) = semantic_tokens.as_mut() {
                    let decoder = reader.decoder();
                    let target = decoder
                        .decode(event.target())
                        .map_err(|error| NfoError::Xml(error.to_string()))?
                        .into_owned()
                        .into_bytes();
                    let content = decoder
                        .decode(event.content())
                        .map_err(|error| NfoError::Xml(error.to_string()))?;
                    let content = normalize_xml_10_line_endings(content.as_ref()).into_bytes();
                    tokens.push(NfoSemanticToken::ProcessingInstruction { target, content });
                }
            }
            Ok(Event::DocType(_)) => return Err(NfoError::DocTypeNotAllowed),
            Ok(Event::GeneralRef(event)) => {
                let value = match event
                    .resolve_char_ref()
                    .map_err(|error| NfoError::Xml(error.to_string()))?
                {
                    Some(character) => include_semantic_fingerprint.then(|| character.to_string()),
                    None => {
                        let name = event
                            .decode()
                            .map_err(|error| NfoError::Xml(error.to_string()))?;
                        let value = match name.as_ref() {
                            "amp" => "&",
                            "lt" => "<",
                            "gt" => ">",
                            "apos" => "'",
                            "quot" => "\"",
                            _ => {
                                return Err(NfoError::Xml(format!(
                                    "undeclared entity reference: &{name};"
                                )));
                            }
                        };
                        include_semantic_fingerprint.then(|| value.to_owned())
                    }
                };
                if let Some(value) = value {
                    if let Some(tokens) = semantic_tokens.as_mut() {
                        push_nfo_semantic_text(tokens, &value);
                    }
                    append_projection_text(
                        depth,
                        actor_depth,
                        active_direct.as_mut(),
                        active_actor.as_mut(),
                        &value,
                    )?;
                }
            }
            Ok(_) => {}
            Err(error) => return Err(NfoError::Xml(error.to_string())),
        }
        buffer.clear();
    }

    let semantic_fingerprint = semantic_tokens
        .as_deref()
        .map(nfo_semantic_fingerprint_from_tokens)
        .unwrap_or_default();
    Ok((projection, semantic_fingerprint))
}

#[derive(Clone, Debug)]
enum NfoSemanticToken {
    Start {
        name: Vec<u8>,
        attributes: Vec<(Vec<u8>, String)>,
        preserve_whitespace: Option<bool>,
    },
    End(Vec<u8>),
    Text(String),
    ProcessingInstruction {
        target: Vec<u8>,
        content: Vec<u8>,
    },
}

impl NfoSemanticToken {
    fn start(event: &BytesStart<'_>, decoder: Decoder) -> Result<Self, NfoError> {
        let mut attributes = event
            .attributes()
            .with_checks(true)
            .map(|attribute| {
                let attribute = attribute.map_err(|error| NfoError::Xml(error.to_string()))?;
                // XML 1.0 normalizes literal attribute whitespace before
                // resolving references. Doing this after unescaping would
                // incorrectly equate a literal newline (normalized to a
                // space) with `&#xA;` (which remains a newline).
                let raw_value = decoder
                    .decode(&attribute.value)
                    .map_err(|error| NfoError::Xml(error.to_string()))?;
                let normalized_value = normalize_xml_attribute_whitespace(raw_value.as_ref());
                let value = unescape(&normalized_value)
                    .map_err(|error| NfoError::Xml(error.to_string()))?
                    .into_owned();
                let name = decoder
                    .decode(attribute.key.as_ref())
                    .map_err(|error| NfoError::Xml(error.to_string()))?
                    .into_owned()
                    .into_bytes();
                Ok((name, value))
            })
            .collect::<Result<Vec<_>, NfoError>>()?;
        attributes.sort_by(|left, right| left.0.cmp(&right.0));
        let preserve_whitespace = attributes
            .iter()
            .find(|(name, _)| name.as_slice() == b"xml:space")
            .and_then(|(_, value)| match value.as_str() {
                "preserve" => Some(true),
                "default" => Some(false),
                _ => None,
            });
        let name = decoder
            .decode(event.name().as_ref())
            .map_err(|error| NfoError::Xml(error.to_string()))?
            .into_owned()
            .into_bytes();
        Ok(Self::Start {
            name,
            attributes,
            preserve_whitespace,
        })
    }
}

fn normalize_xml_attribute_whitespace(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                normalized.push(' ');
            }
            '\n' | '\t' => normalized.push(' '),
            character => normalized.push(character),
        }
    }
    normalized
}

fn normalize_xml_10_line_endings(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(character);
        }
    }
    normalized
}

fn push_nfo_semantic_text(tokens: &mut Vec<NfoSemanticToken>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(NfoSemanticToken::Text(previous)) = tokens.last_mut() {
        previous.push_str(text);
    } else {
        tokens.push(NfoSemanticToken::Text(text.to_owned()));
    }
}

#[derive(Default)]
struct NfoSemanticElementState {
    token_index: usize,
    has_child_element: bool,
    has_non_whitespace_text: bool,
    preserve_whitespace: bool,
}

fn nfo_semantic_fingerprint_from_tokens(tokens: &[NfoSemanticToken]) -> Vec<u8> {
    let mut element_only_whitespace = vec![false; tokens.len()];
    let mut stack = Vec::<NfoSemanticElementState>::new();
    let mut inherited_preserve_whitespace = Vec::<bool>::new();
    for (token_index, token) in tokens.iter().enumerate() {
        match token {
            NfoSemanticToken::Start {
                preserve_whitespace,
                ..
            } => {
                if let Some(parent) = stack.last_mut() {
                    parent.has_child_element = true;
                }
                let inherited = inherited_preserve_whitespace
                    .last()
                    .copied()
                    .unwrap_or(false);
                let preserve_whitespace = preserve_whitespace.unwrap_or(inherited);
                stack.push(NfoSemanticElementState {
                    token_index,
                    preserve_whitespace,
                    ..NfoSemanticElementState::default()
                });
                inherited_preserve_whitespace.push(preserve_whitespace);
            }
            NfoSemanticToken::Text(text) => {
                if let Some(element) = stack.last_mut() {
                    element.has_non_whitespace_text |= !is_xml_whitespace(text);
                }
            }
            NfoSemanticToken::ProcessingInstruction { .. } => {}
            NfoSemanticToken::End(_) => {
                if let Some(element) = stack.pop() {
                    inherited_preserve_whitespace.pop();
                    element_only_whitespace[element.token_index] = element.has_child_element
                        && !element.has_non_whitespace_text
                        && !element.preserve_whitespace;
                }
            }
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(b"LUX-NFO-SEMANTIC-2\0");
    let mut skip_whitespace_stack = Vec::<bool>::new();
    for (token_index, token) in tokens.iter().enumerate() {
        match token {
            NfoSemanticToken::Start {
                name, attributes, ..
            } => {
                hasher.update([b'S']);
                hash_nfo_semantic_bytes(&mut hasher, name);
                hasher.update((attributes.len() as u64).to_be_bytes());
                for (name, value) in attributes {
                    hash_nfo_semantic_bytes(&mut hasher, name);
                    hash_nfo_semantic_bytes(&mut hasher, value.as_bytes());
                }
                skip_whitespace_stack.push(element_only_whitespace[token_index]);
            }
            NfoSemanticToken::End(name) => {
                hasher.update([b'E']);
                hash_nfo_semantic_bytes(&mut hasher, name);
                skip_whitespace_stack.pop();
            }
            NfoSemanticToken::Text(text) => {
                if skip_whitespace_stack.last().copied().unwrap_or(false) && is_xml_whitespace(text)
                {
                    continue;
                }
                hasher.update([b'T']);
                hash_nfo_semantic_bytes(&mut hasher, text.as_bytes());
            }
            NfoSemanticToken::ProcessingInstruction { target, content } => {
                hasher.update([b'P']);
                hash_nfo_semantic_bytes(&mut hasher, target);
                hash_nfo_semantic_bytes(&mut hasher, content);
            }
        }
    }
    hasher.finalize().to_vec()
}

fn hash_nfo_semantic_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn is_xml_whitespace(value: &str) -> bool {
    value.chars().all(is_xml_whitespace_character)
}

fn is_xml_whitespace_character(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\r' | '\n')
}

/// Reads the direct `<actor>` nodes used by Emby/Kodi local NFO files.
///
/// This intentionally only extracts the actor fields needed by the people
/// cache. The caller can run it during background metadata enrichment without
/// asking the detail endpoint to parse an untrusted XML document.
pub fn parse_local_nfo_actors(bytes: &[u8]) -> Result<Vec<ActorCredit>, NfoError> {
    parse_local_nfo_projection(bytes).map(|projection| projection.actors)
}

/// Compatibility wrapper for the original movie-only parser name.
pub fn parse_movie_nfo_actors(bytes: &[u8]) -> Result<Vec<ActorCredit>, NfoError> {
    parse_local_nfo_actors(bytes)
}

#[derive(Default)]
struct ParsedMovieActor {
    name: Option<String>,
    role: Option<String>,
    tmdb_id: Option<String>,
    provider: Option<String>,
    order: Option<i32>,
}

#[derive(Clone, Copy)]
enum MovieActorField {
    Name,
    Role,
    TmdbId,
    ImdbId,
    DoubanId,
    Order,
}

fn movie_actor_field(tag: &[u8]) -> Option<MovieActorField> {
    match tag {
        b"name" => Some(MovieActorField::Name),
        b"role" | b"character" => Some(MovieActorField::Role),
        b"tmdbid" => Some(MovieActorField::TmdbId),
        b"imdbid" => Some(MovieActorField::ImdbId),
        b"doubanid" => Some(MovieActorField::DoubanId),
        b"order" => Some(MovieActorField::Order),
        _ => None,
    }
}

fn assign_movie_actor_field(
    actor: &mut ParsedMovieActor,
    field: MovieActorField,
    value: String,
) -> Result<(), NfoError> {
    if value.len() > MAX_MOVIE_ACTOR_FIELD_BYTES {
        return Err(NfoError::FieldTooLarge);
    }
    if value.is_empty() {
        return Ok(());
    }
    match field {
        MovieActorField::Name => actor.name = Some(value),
        MovieActorField::Role => actor.role = Some(value),
        MovieActorField::TmdbId => {
            actor.tmdb_id = Some(value);
            actor.provider = Some("tmdb".to_owned());
        }
        MovieActorField::ImdbId => {
            actor.tmdb_id = Some(value);
            actor.provider = Some("imdb".to_owned());
        }
        MovieActorField::DoubanId => {
            actor.tmdb_id = Some(value);
            actor.provider = Some("douban".to_owned());
        }
        MovieActorField::Order => actor.order = value.parse::<i32>().ok(),
    }
    Ok(())
}

/// Reads rich direct-child fields shared by movie, series, season and episode NFO files.
///
/// This is deliberately a background-only parser. The detail endpoint reads
/// the JSON snapshot produced from this value instead of opening an NFO file.
pub fn parse_local_nfo_details(bytes: &[u8]) -> Result<LocalNfoDetails, NfoError> {
    parse_local_nfo_projection(bytes).map(|projection| projection.details)
}

/// Compatibility wrapper for the original movie-only parser name.
pub fn parse_movie_nfo_details(bytes: &[u8]) -> Result<LocalNfoDetails, NfoError> {
    parse_local_nfo_details(bytes)
}

struct ActiveDirectValue {
    base: Option<BaseNfoField>,
    rich: Option<RichValueKind>,
    text: String,
}

struct ActiveActorValue {
    field: MovieActorField,
    text: String,
}

#[derive(Clone, Copy)]
enum BaseNfoField {
    Title,
    OriginalTitle,
    Year,
    Overview,
}

fn direct_value_kind(event: &BytesStart<'_>) -> Result<Option<ActiveDirectValue>, NfoError> {
    let base = base_nfo_field(event.name().as_ref());
    let rich = rich_value_kind(event)?;
    if base.is_none() && rich.is_none() {
        return Ok(None);
    }
    Ok(Some(ActiveDirectValue {
        base,
        rich,
        text: String::new(),
    }))
}

fn base_nfo_field(name: &[u8]) -> Option<BaseNfoField> {
    match name {
        b"title" => Some(BaseNfoField::Title),
        b"originaltitle" | b"original_title" => Some(BaseNfoField::OriginalTitle),
        b"year" => Some(BaseNfoField::Year),
        b"plot" | b"overview" => Some(BaseNfoField::Overview),
        _ => None,
    }
}

fn append_projection_text(
    depth: usize,
    actor_depth: Option<usize>,
    direct: Option<&mut ActiveDirectValue>,
    actor: Option<&mut ActiveActorValue>,
    value: &str,
) -> Result<(), NfoError> {
    if depth == 2 {
        if let Some(direct) = direct {
            append_rich_text(&mut direct.text, value)?;
        }
    } else if actor_depth.is_some_and(|actor_depth| depth == actor_depth.saturating_add(1))
        && let Some(actor) = actor
    {
        append_rich_text(&mut actor.text, value)?;
    }
    Ok(())
}

fn assign_direct_value(
    projection: &mut LocalNfoProjection,
    active: ActiveDirectValue,
    raw_value: &str,
) -> Result<(), NfoError> {
    if let Some(base) = active.base {
        assign_base_value(&mut projection.metadata, base, raw_value)?;
    }
    if let Some(rich) = active.rich {
        assign_rich_value(&mut projection.details, rich, raw_value)?;
    }
    Ok(())
}

fn assign_base_value(
    metadata: &mut NfoMetadata,
    field: BaseNfoField,
    raw_value: &str,
) -> Result<(), NfoError> {
    let value = raw_value.trim();
    if value.is_empty() {
        return Ok(());
    }
    if value.len() > MAX_MOVIE_NFO_DETAILS_TEXT_BYTES {
        return Err(NfoError::FieldTooLarge);
    }
    match field {
        BaseNfoField::Title => metadata.title = Some(value.to_owned()),
        BaseNfoField::OriginalTitle => metadata.original_title = Some(value.to_owned()),
        BaseNfoField::Year => {
            metadata.production_year = value
                .parse::<i32>()
                .ok()
                .filter(|year| (1800..=2200).contains(year));
        }
        BaseNfoField::Overview => metadata.overview = Some(value.to_owned()),
    }
    Ok(())
}

fn push_parsed_actor(actors: &mut Vec<ActorCredit>, actor: ParsedMovieActor) {
    let Some(name) = actor.name.map(|value| value.trim().to_owned()) else {
        return;
    };
    if name.is_empty() || actors.len() >= MAX_MOVIE_NFO_ACTORS {
        return;
    }
    let id = actor
        .tmdb_id
        .map(|value| value.trim().to_owned())
        .unwrap_or_default();
    actors.push(ActorCredit {
        provider: actor.provider,
        identities: Vec::new(),
        id,
        name,
        character: actor.role,
        order: actor.order,
        profile_url: None,
        person: None,
    });
}

enum RichValueKind {
    Rating,
    Votes,
    Tagline,
    Premiered,
    ReleaseDate,
    Aired,
    LastAirDate,
    Runtime,
    Status,
    OriginalLanguage,
    Website,
    SetName,
    SetId,
    Certification,
    Country,
    Genre,
    Studio,
    Provider(String),
    Director(String),
    Writer(String),
    SeasonNumber,
    EpisodeNumber,
    Trailer,
}

fn rich_value_kind(event: &BytesStart<'_>) -> Result<Option<RichValueKind>, NfoError> {
    let event_name = event.name();
    let tag = event_name.as_ref();
    let kind = match tag {
        b"rating" => RichValueKind::Rating,
        b"votes" => RichValueKind::Votes,
        b"tagline" => RichValueKind::Tagline,
        b"premiered" => RichValueKind::Premiered,
        b"releasedate" => RichValueKind::ReleaseDate,
        b"aired" | b"airdate" => RichValueKind::Aired,
        b"lastaired" | b"lastairdate" | b"ended" | b"enddate" => RichValueKind::LastAirDate,
        b"runtime" => RichValueKind::Runtime,
        b"status" => RichValueKind::Status,
        b"language" => RichValueKind::OriginalLanguage,
        b"website" => RichValueKind::Website,
        b"set" => RichValueKind::SetName,
        b"setid" => RichValueKind::SetId,
        b"mpaa" => RichValueKind::Certification,
        b"country" => RichValueKind::Country,
        b"genre" => RichValueKind::Genre,
        b"studio" => RichValueKind::Studio,
        b"trailer" => RichValueKind::Trailer,
        b"director" => {
            RichValueKind::Director(attribute_value(event, b"tmdbid")?.unwrap_or_default())
        }
        b"writer" | b"credits" => {
            RichValueKind::Writer(attribute_value(event, b"tmdbid")?.unwrap_or_default())
        }
        b"season" | b"seasonnumber" => RichValueKind::SeasonNumber,
        b"episode" | b"episodenumber" => RichValueKind::EpisodeNumber,
        b"tmdbid" => RichValueKind::Provider("tmdb".to_owned()),
        b"imdbid" => RichValueKind::Provider("imdb".to_owned()),
        b"tvdbid" => RichValueKind::Provider("tvdb".to_owned()),
        b"wikidataid" => RichValueKind::Provider("wikidata".to_owned()),
        b"uniqueid" => {
            let Some(provider) = attribute_value(event, b"type")? else {
                return Ok(None);
            };
            let provider = provider.trim().to_ascii_lowercase();
            if !matches!(provider.as_str(), "tmdb" | "imdb" | "tvdb" | "wikidata") {
                return Ok(None);
            }
            RichValueKind::Provider(provider)
        }
        _ => return Ok(None),
    };
    Ok(Some(kind))
}

fn attribute_value(event: &BytesStart<'_>, name: &[u8]) -> Result<Option<String>, NfoError> {
    for attribute in event.attributes().with_checks(false) {
        let attribute = attribute.map_err(|error| NfoError::Xml(error.to_string()))?;
        if attribute.key.as_ref() == name {
            return attribute
                .unescape_value()
                .map(|value| Some(value.into_owned()))
                .map_err(|error| NfoError::Xml(error.to_string()));
        }
    }
    Ok(None)
}

fn append_rich_text(target: &mut String, value: &str) -> Result<(), NfoError> {
    if target.len().saturating_add(value.len()) > MAX_MOVIE_NFO_DETAILS_TEXT_BYTES {
        return Err(NfoError::FieldTooLarge);
    }
    target.push_str(value);
    Ok(())
}

fn assign_rich_value(
    details: &mut LocalNfoDetails,
    kind: RichValueKind,
    raw_value: &str,
) -> Result<(), NfoError> {
    if raw_value.is_empty() {
        return Ok(());
    }
    match kind {
        RichValueKind::Rating => {
            details.rating = raw_value
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite() && (0.0..=10.0).contains(value));
        }
        RichValueKind::Votes => {
            details.votes = raw_value.parse::<i64>().ok().filter(|value| *value >= 0);
        }
        RichValueKind::Runtime => {
            details.runtime = raw_value
                .parse::<i32>()
                .ok()
                .filter(|value| (0..=10_000).contains(value));
        }
        RichValueKind::Website => details.website = http_url(raw_value),
        RichValueKind::Trailer => {
            if let Some(value) = http_url(raw_value) {
                push_unique(&mut details.trailers, value);
            }
        }
        RichValueKind::Premiered => details.premiered = bounded_text(raw_value),
        RichValueKind::ReleaseDate => details.release_date = bounded_text(raw_value),
        RichValueKind::Aired => details.aired = bounded_text(raw_value),
        RichValueKind::LastAirDate => details.last_air_date = bounded_text(raw_value),
        RichValueKind::Tagline => details.tagline = bounded_text(raw_value),
        RichValueKind::Status => details.status = bounded_text(raw_value),
        RichValueKind::OriginalLanguage => details.original_language = bounded_text(raw_value),
        RichValueKind::SetName => details.set_name = bounded_text(raw_value),
        RichValueKind::SetId => details.set_id = bounded_id(raw_value),
        RichValueKind::Certification => details.certification = bounded_text(raw_value),
        RichValueKind::Country => push_bounded(&mut details.countries, raw_value),
        RichValueKind::Genre => push_bounded(&mut details.genres, raw_value),
        RichValueKind::Studio => push_bounded(&mut details.studios, raw_value),
        RichValueKind::Provider(provider) => {
            if let Some(value) = bounded_id(raw_value) {
                details.provider_ids.insert(provider, value);
            }
        }
        RichValueKind::Director(provider_id) => {
            push_credit(&mut details.directors, provider_id, raw_value);
        }
        RichValueKind::Writer(provider_id) => {
            push_credit(&mut details.writers, provider_id, raw_value);
        }
        RichValueKind::SeasonNumber => {
            details.season_number = raw_value
                .parse::<i32>()
                .ok()
                .filter(|value| (0..=10_000).contains(value));
        }
        RichValueKind::EpisodeNumber => {
            details.episode_number = raw_value
                .parse::<i32>()
                .ok()
                .filter(|value| (0..=100_000).contains(value));
        }
    }
    Ok(())
}

fn bounded_text(value: &str) -> Option<String> {
    (value.len() <= MAX_MOVIE_NFO_DETAILS_TEXT_BYTES).then(|| value.to_owned())
}

fn bounded_id(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= MAX_MOVIE_NFO_DETAILS_ID_BYTES).then(|| value.to_owned())
}

fn http_url(value: &str) -> Option<String> {
    let value = value.trim();
    (value.len() <= MAX_MOVIE_NFO_DETAILS_URL_BYTES
        && (value.starts_with("https://") || value.starts_with("http://")))
    .then(|| value.to_owned())
}

fn push_bounded(values: &mut Vec<String>, value: &str) {
    if values.len() >= MAX_MOVIE_NFO_DETAILS_ITEMS {
        return;
    }
    let Some(value) = bounded_text(value) else {
        return;
    };
    push_unique(values, value);
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value)
        && values.len() < MAX_MOVIE_NFO_DETAILS_ITEMS
    {
        values.push(value);
    }
}

fn push_credit(values: &mut Vec<LocalNfoCredit>, provider_id: String, name: &str) {
    let Some(name) = bounded_text(name) else {
        return;
    };
    if values.len() >= MAX_MOVIE_NFO_DETAILS_ITEMS {
        return;
    }
    if values
        .iter()
        .any(|credit| credit.provider_id == provider_id && credit.name == name)
    {
        return;
    }
    values.push(LocalNfoCredit { provider_id, name });
}

#[derive(Clone)]
pub struct LocalNfoMetadataStore {
    database: Database,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalNfoCacheEnvelope {
    schema_version: u8,
    semantic_fingerprint: Option<Vec<u8>>,
    relation_fingerprint: Option<Vec<u8>>,
    details: LocalNfoDetails,
}

fn decode_local_nfo_cache(
    json: &str,
) -> Result<(LocalNfoDetails, Option<Vec<u8>>, Option<Vec<u8>>), String> {
    let value =
        serde_json::from_str::<serde_json::Value>(json).map_err(|error| error.to_string())?;
    if value.get("schemaVersion").is_some() {
        let envelope = serde_json::from_value::<LocalNfoCacheEnvelope>(value)
            .map_err(|error| error.to_string())?;
        if envelope.schema_version != LOCAL_NFO_CACHE_SCHEMA_VERSION {
            return Err("unsupported local NFO cache schema version".to_owned());
        }
        let semantic_fingerprint = envelope
            .semantic_fingerprint
            .filter(|fingerprint| valid_nfo_content_fingerprint(fingerprint));
        let relation_fingerprint = envelope
            .relation_fingerprint
            .filter(|fingerprint| valid_nfo_content_fingerprint(fingerprint));
        return Ok((envelope.details, semantic_fingerprint, relation_fingerprint));
    }
    serde_json::from_value(value)
        .map(|details| (details, None, None))
        .map_err(|error| error.to_string())
}

fn encode_local_nfo_cache(
    details: &LocalNfoDetails,
    semantic_fingerprint: Option<&[u8]>,
    relation_fingerprint: Option<&[u8]>,
) -> Result<String, LocalNfoMetadataStoreError> {
    serde_json::to_string(&LocalNfoCacheEnvelope {
        schema_version: LOCAL_NFO_CACHE_SCHEMA_VERSION,
        semantic_fingerprint: semantic_fingerprint.map(<[u8]>::to_vec),
        relation_fingerprint: relation_fingerprint.map(<[u8]>::to_vec),
        details: details.clone(),
    })
    .map_err(|error| LocalNfoMetadataStoreError::Serialization(error.to_string()))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalNfoMetadataState {
    pub has_snapshot: bool,
    pub source_fingerprint: Option<Vec<u8>>,
}

impl LocalNfoMetadataStore {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn write_item(
        &self,
        item_id: &str,
        source_fingerprint: &[u8],
        details: &LocalNfoDetails,
    ) -> Result<(), LocalNfoMetadataStoreError> {
        let json = serde_json::to_string(details)
            .map_err(|error| LocalNfoMetadataStoreError::Serialization(error.to_string()))?;
        if json.len() > MAX_LOCAL_NFO_BYTES {
            return Err(LocalNfoMetadataStoreError::TooLarge);
        }
        self.database
            .update_media_item_nfo_metadata(item_id, Some(&json), Some(source_fingerprint))
            .await
            .map_err(LocalNfoMetadataStoreError::Storage)
    }

    pub(crate) async fn write_item_with_semantic_fingerprint(
        &self,
        item_id: &str,
        source_fingerprint: &[u8],
        semantic_fingerprint: Option<&[u8]>,
        relation_fingerprint: Option<&[u8]>,
        details: &LocalNfoDetails,
    ) -> Result<(), LocalNfoMetadataStoreError> {
        let json = encode_local_nfo_cache(details, semantic_fingerprint, relation_fingerprint)?;
        if json.len() > MAX_LOCAL_NFO_BYTES {
            return Err(LocalNfoMetadataStoreError::TooLarge);
        }
        self.database
            .update_media_item_nfo_metadata(item_id, Some(&json), Some(source_fingerprint))
            .await
            .map_err(LocalNfoMetadataStoreError::Storage)
    }

    pub async fn read_item(
        &self,
        item_id: &str,
    ) -> Result<Option<LocalNfoDetails>, LocalNfoMetadataStoreError> {
        let Some(json) = self
            .database
            .media_item_nfo_metadata_json(item_id)
            .await
            .map_err(LocalNfoMetadataStoreError::Storage)?
        else {
            return Ok(None);
        };
        if json.len() > MAX_LOCAL_NFO_BYTES {
            tracing::warn!(
                item_id,
                "derived local NFO cache is too large; clearing it for rebuild"
            );
            self.database
                .clear_media_item_nfo_metadata_if_json(item_id, &json)
                .await
                .map_err(LocalNfoMetadataStoreError::Storage)?;
            return Ok(None);
        }
        match decode_local_nfo_cache(&json) {
            Ok((details, _, _)) => Ok(Some(details)),
            Err(error) => {
                tracing::warn!(
                    item_id,
                    error = %error,
                    "derived local NFO cache is malformed; clearing it for rebuild"
                );
                self.database
                    .clear_media_item_nfo_metadata_if_json(item_id, &json)
                    .await
                    .map_err(LocalNfoMetadataStoreError::Storage)?;
                Ok(None)
            }
        }
    }

    pub async fn state(
        &self,
        item_id: &str,
    ) -> Result<LocalNfoMetadataState, LocalNfoMetadataStoreError> {
        let (has_snapshot, source_fingerprint) = self
            .database
            .media_item_nfo_metadata_state(item_id)
            .await
            .map_err(LocalNfoMetadataStoreError::Storage)?;
        Ok(LocalNfoMetadataState {
            has_snapshot,
            source_fingerprint,
        })
    }

    pub async fn is_current(
        &self,
        item_id: &str,
        source_fingerprint: &[u8],
    ) -> Result<bool, LocalNfoMetadataStoreError> {
        let state = self.state(item_id).await?;
        if !state.has_snapshot || state.source_fingerprint.as_deref() != Some(source_fingerprint) {
            return Ok(false);
        }
        Ok(self.read_item(item_id).await?.is_some())
    }

    pub async fn is_usable(&self, item_id: &str) -> Result<bool, LocalNfoMetadataStoreError> {
        Ok(self.read_item_if_usable(item_id).await?.is_some())
    }

    pub async fn read_item_if_usable(
        &self,
        item_id: &str,
    ) -> Result<Option<LocalNfoDetails>, LocalNfoMetadataStoreError> {
        let state = self.state(item_id).await?;
        if !state.has_snapshot
            || !state
                .source_fingerprint
                .as_deref()
                .is_some_and(valid_nfo_content_fingerprint)
        {
            return Ok(None);
        }
        self.read_item(item_id).await
    }

    pub(crate) async fn read_item_if_usable_with_fingerprint(
        &self,
        item_id: &str,
    ) -> Result<
        Option<(LocalNfoDetails, Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>)>,
        LocalNfoMetadataStoreError,
    > {
        let Some((json, source_fingerprint)) = self
            .database
            .media_item_nfo_metadata_snapshot(item_id)
            .await
            .map_err(LocalNfoMetadataStoreError::Storage)?
        else {
            return Ok(None);
        };
        if json.len() > MAX_LOCAL_NFO_BYTES {
            tracing::warn!(
                item_id,
                "derived local NFO cache is too large; clearing it for rebuild"
            );
            self.database
                .clear_media_item_nfo_metadata_if_json(item_id, &json)
                .await
                .map_err(LocalNfoMetadataStoreError::Storage)?;
            return Ok(None);
        }
        let Some(source_fingerprint) =
            source_fingerprint.filter(|value| valid_nfo_content_fingerprint(value))
        else {
            return Ok(None);
        };
        match decode_local_nfo_cache(&json) {
            Ok((details, semantic_fingerprint, relation_fingerprint)) => Ok(Some((
                details,
                source_fingerprint,
                semantic_fingerprint,
                relation_fingerprint,
            ))),
            Err(error) => {
                tracing::warn!(
                    item_id,
                    error = %error,
                    "derived local NFO cache is malformed; clearing it for rebuild"
                );
                self.database
                    .clear_media_item_nfo_metadata_if_json(item_id, &json)
                    .await
                    .map_err(LocalNfoMetadataStoreError::Storage)?;
                Ok(None)
            }
        }
    }

    pub async fn exists(&self, item_id: &str) -> Result<bool, LocalNfoMetadataStoreError> {
        self.database
            .media_item_nfo_metadata_state(item_id)
            .await
            .map(|(has_snapshot, _)| has_snapshot)
            .map_err(LocalNfoMetadataStoreError::Storage)
    }

    pub async fn clear_item(&self, item_id: &str) -> Result<(), LocalNfoMetadataStoreError> {
        self.database
            .update_media_item_nfo_metadata(item_id, None, None)
            .await
            .map_err(LocalNfoMetadataStoreError::Storage)
    }
}

pub(crate) fn nfo_content_fingerprint(bytes: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(b"LUX-NFO-CONTENT-1\0");
    hasher.update(bytes);
    hasher.finalize().to_vec()
}

fn valid_nfo_content_fingerprint(value: &[u8]) -> bool {
    value.len() == 32
}

#[derive(Debug)]
pub enum LocalNfoMetadataStoreError {
    Serialization(String),
    TooLarge,
    Storage(StorageError),
}

impl fmt::Display for LocalNfoMetadataStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialization(message) => formatter.write_str(message),
            Self::TooLarge => formatter.write_str("local NFO cache is too large"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LocalNfoMetadataStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Serialization(_) | Self::TooLarge => None,
        }
    }
}

impl From<StorageError> for LocalNfoMetadataStoreError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

/// Compatibility alias for the original movie-only store name.
pub type MovieNfoMetadataStore = LocalNfoMetadataStore;
/// Compatibility alias for the original movie-only store error name.
pub type MovieNfoMetadataStoreError = LocalNfoMetadataStoreError;

pub fn rewrite_nfo(original: &[u8], patch: &NfoMetadata) -> Result<Vec<u8>, NfoWriteError> {
    rewrite_nfo_with_root(original, patch, "movie", false)
}

fn rewrite_nfo_with_root(
    original: &[u8],
    patch: &NfoMetadata,
    root_tag: &str,
    normalize_existing_root: bool,
) -> Result<Vec<u8>, NfoWriteError> {
    if original.is_empty() {
        return new_nfo(patch, root_tag);
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
                let mut event = event.to_owned();
                if normalize_existing_root && depth == 0 {
                    event.set_name(root_tag.as_bytes());
                }
                let field = (depth == 1)
                    .then(|| field_for_tag(event.name().as_ref()))
                    .flatten();
                writer
                    .write_event(Event::Start(event))
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
                let event = if normalize_existing_root && depth == 1 {
                    BytesEnd::new(root_tag)
                } else {
                    event.to_owned()
                };
                writer
                    .write_event(Event::End(event))
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

pub fn rewrite_movie_nfo(
    original: &[u8],
    patch: &MovieNfoMetadata,
) -> Result<Vec<u8>, NfoWriteError> {
    validate_movie_nfo_actors(patch)?;
    rewrite_rich_nfo(original, patch, "movie", false)
}

pub fn rewrite_nfo_probe_details(
    original: &[u8],
    probe: &MediaProbeResult,
) -> Result<Vec<u8>, NfoWriteError> {
    let movie = rewrite_movie_nfo(original, &MovieNfoMetadata::default())?;
    let streamdetails = serialize_probe_streamdetails(probe)?;
    rewrite_nfo_fileinfo(&movie, streamdetails.as_deref())
}

fn rewrite_movie_nfo_auxiliary_fields(
    original: &[u8],
    sort_title: Option<&str>,
    date_added: Option<&str>,
) -> Result<Vec<u8>, NfoWriteError> {
    parse_nfo(original).map_err(NfoWriteError::Nfo)?;
    let sort_title = sort_title
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= MAX_MOVIE_NFO_DETAILS_TEXT_BYTES)
        .filter(|value| value.chars().all(is_valid_xml_character));
    let date_added = date_added
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= MAX_MOVIE_NFO_DETAILS_TEXT_BYTES)
        .filter(|value| value.chars().all(is_valid_xml_character));
    let add_sort_title = sort_title.is_some() && !nfo_root_field_has_value(original, b"sorttitle")?;
    let add_date_added = date_added.is_some() && !nfo_root_field_has_value(original, b"dateadded")?;
    if !add_sort_title && !add_date_added {
        return Ok(original.to_vec());
    }

    let mut reader = Reader::from_reader(original);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut saw_root = false;
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
                writer
                    .write_event(Event::Start(event.to_owned()))
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                depth = depth.saturating_add(1);
            }
            Ok(Event::End(event)) => {
                if depth == 1 {
                    if add_sort_title && let Some(sort_title) = sort_title {
                        write_simple_element(&mut writer, "sorttitle", sort_title)?;
                    }
                    if add_date_added && let Some(date_added) = date_added {
                        write_simple_element(&mut writer, "dateadded", date_added)?;
                    }
                }
                writer
                    .write_event(Event::End(event.to_owned()))
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Empty(event)) => {
                if depth == 0 {
                    saw_root = true;
                }
                writer
                    .write_event(Event::Empty(event.to_owned()))
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
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
    let rewritten = writer.into_inner();
    if rewritten.len() > MAX_LOCAL_NFO_BYTES {
        return Err(NfoWriteError::InvalidMetadata(
            "NFO exceeds the output size limit".to_owned(),
        ));
    }
    parse_nfo(&rewritten).map_err(NfoWriteError::Nfo)?;
    Ok(rewritten)
}

fn nfo_root_field_has_value(original: &[u8], field: &[u8]) -> Result<bool, NfoWriteError> {
    let mut reader = Reader::from_reader(original);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut field_depth = None;
    let mut has_value = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => return Ok(false),
            Ok(Event::Start(event)) => {
                if depth == 1 && event.name().as_ref().eq_ignore_ascii_case(field) {
                    field_depth = Some(depth.saturating_add(1));
                }
                depth = depth.saturating_add(1);
            }
            Ok(Event::Empty(event)) => {
                if depth == 1 && event.name().as_ref().eq_ignore_ascii_case(field) {
                    has_value = false;
                }
            }
            Ok(Event::Text(event)) if field_depth == Some(depth) => {
                let decoded = event
                    .decode()
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                let value = unescape(decoded.as_ref())
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                has_value |= !value.trim().is_empty();
            }
            Ok(Event::CData(event)) if field_depth == Some(depth) => {
                let value = event
                    .decode()
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                has_value |= !value.trim().is_empty();
            }
            Ok(Event::End(_)) => {
                if field_depth == Some(depth) {
                    if has_value {
                        return Ok(true);
                    }
                    field_depth = None;
                }
                depth = depth.saturating_sub(1);
            }
            Ok(_) => {}
            Err(error) => return Err(NfoWriteError::InvalidXml(error.to_string())),
        }
        buffer.clear();
    }
}

fn nfo_date_added(unix_seconds: i64) -> Option<String> {
    let date = OffsetDateTime::from_unix_timestamp(unix_seconds).ok()?;
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        date.year(),
        u8::from(date.month()),
        date.day(),
        date.hour(),
        date.minute(),
        date.second()
    ))
}

pub fn rewrite_series_nfo(
    original: &[u8],
    patch: &MovieNfoMetadata,
) -> Result<Vec<u8>, NfoWriteError> {
    validate_movie_nfo_actors(patch)?;
    rewrite_rich_nfo(original, patch, "tvshow", true)
}

fn rewrite_rich_nfo(
    original: &[u8],
    patch: &MovieNfoMetadata,
    root_tag: &str,
    normalize_existing_root: bool,
) -> Result<Vec<u8>, NfoWriteError> {
    let base = rewrite_nfo_with_root(original, &patch.base, root_tag, normalize_existing_root)?;
    let mut reader = Reader::from_reader(base.as_slice());
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut saw_root = false;
    let mut skip_depth = None;
    let mut appended = false;

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
                if skip_depth.is_some() {
                    depth += 1;
                    buffer.clear();
                    continue;
                }
                if depth == 0 {
                    saw_root = true;
                }
                depth += 1;
                if depth == 2 && replace_rich_root_tag(&event, patch) {
                    skip_depth = Some(depth);
                    buffer.clear();
                    continue;
                }
                writer
                    .write_event(Event::Start(event.to_owned()))
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
            }
            Ok(Event::Empty(event)) => {
                if skip_depth.is_some() {
                    buffer.clear();
                    continue;
                }
                if depth == 0 {
                    saw_root = true;
                    let mut event = event.to_owned();
                    if normalize_existing_root {
                        event.set_name(root_tag.as_bytes());
                    }
                    writer
                        .write_event(Event::Start(event))
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                    append_movie_nfo_fields(&mut writer, patch)?;
                    appended = true;
                    writer
                        .write_event(Event::End(BytesEnd::new(root_tag)))
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                } else if depth == 1 && replace_rich_root_tag(&event, patch) {
                    buffer.clear();
                    continue;
                } else {
                    writer
                        .write_event(Event::Empty(event.to_owned()))
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                }
            }
            Ok(Event::End(event)) => {
                if skip_depth.is_some() {
                    if skip_depth == Some(depth) {
                        skip_depth = None;
                    }
                    depth = depth.saturating_sub(1);
                    buffer.clear();
                    continue;
                }
                if depth == 1 && !appended {
                    append_movie_nfo_fields(&mut writer, patch)?;
                    appended = true;
                }
                writer
                    .write_event(Event::End(event.to_owned()))
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                depth = depth.saturating_sub(1);
            }
            Ok(event) => {
                if skip_depth.is_none() {
                    writer
                        .write_event(event.to_owned())
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                }
            }
            Err(error) => return Err(NfoWriteError::InvalidXml(error.to_string())),
        }
        buffer.clear();
    }
    let rewritten = writer.into_inner();
    if !original.is_empty()
        && nfo_writeback_projection_fingerprint(original)?
            == nfo_writeback_projection_fingerprint(&rewritten)?
    {
        return Ok(original.to_vec());
    }
    Ok(rewritten)
}

fn nfo_writeback_projection_fingerprint(bytes: &[u8]) -> Result<Vec<u8>, NfoWriteError> {
    let projection = parse_local_nfo_projection(bytes)
        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
    let root = nfo_root_tag(bytes)?;
    let images = nfo_image_projection(bytes)?;
    let mut hasher = Sha256::new();
    hasher.update(b"LUX-NFO-SEMANTIC-1\0");
    hasher.update(format!("{root:?}|{projection:?}|{images:?}").as_bytes());
    Ok(hasher.finalize().to_vec())
}

fn nfo_root_tag(bytes: &[u8]) -> Result<String, NfoWriteError> {
    let mut reader = Reader::from_reader(bytes);
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) | Ok(Event::Empty(event)) => {
                return String::from_utf8(event.name().as_ref().to_vec())
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()));
            }
            Ok(Event::Eof) => {
                return Err(NfoWriteError::InvalidXml(
                    "NFO document does not contain a root element".to_owned(),
                ));
            }
            Ok(_) => {}
            Err(error) => return Err(NfoWriteError::InvalidXml(error.to_string())),
        }
        buffer.clear();
    }
}

fn nfo_image_projection(bytes: &[u8]) -> Result<Vec<(String, String, String)>, NfoWriteError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut active: Option<(usize, String, String, String)> = None;
    let mut images = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                depth = depth.saturating_add(1);
                let tag = event.name();
                let tag = tag.as_ref();
                if (depth == 2 && tag == b"thumb") || (depth == 3 && tag == b"thumb") {
                    let kind = if depth == 3 { "fanart" } else { "thumb" };
                    let aspect = attribute_value_bytes(&event, b"aspect")?.unwrap_or_default();
                    active = Some((depth, kind.to_owned(), aspect, String::new()));
                }
            }
            Ok(Event::Text(event)) if active.as_ref().is_some_and(|item| item.0 == depth) => {
                if let Some(item) = active.as_mut() {
                    let decoded = event
                        .decode()
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                    item.3.push_str(
                        &unescape(decoded.as_ref())
                            .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?,
                    );
                }
            }
            Ok(Event::End(_event)) => {
                if active.as_ref().is_some_and(|item| item.0 == depth) {
                    if let Some((_, kind, aspect, value)) = active.take() {
                        if !value.trim().is_empty() {
                            images.push((kind, aspect, value.trim().to_owned()));
                        }
                    }
                }
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(NfoWriteError::InvalidXml(error.to_string())),
        }
        buffer.clear();
    }
    images.sort();
    Ok(images)
}

fn attribute_value_bytes(
    event: &BytesStart<'_>,
    name: &[u8],
) -> Result<Option<String>, NfoWriteError> {
    for attribute in event.attributes().with_checks(false) {
        let attribute = attribute.map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
        if attribute.key.as_ref() == name {
            return attribute
                .unescape_value()
                .map(|value| Some(value.into_owned()))
                .map_err(|error| NfoWriteError::InvalidXml(error.to_string()));
        }
    }
    Ok(None)
}

fn rich_root_tag(tag: &[u8]) -> bool {
    matches!(
        tag,
        b"actor"
            | b"country"
            | b"genre"
            | b"studio"
            | b"director"
            | b"writer"
            | b"credits"
            | b"trailer"
            | b"rating"
            | b"votes"
            | b"tagline"
            | b"mpaa"
            | b"premiered"
            | b"releasedate"
            | b"lastaired"
            | b"lastairdate"
            | b"enddate"
            | b"ended"
            | b"runtime"
            | b"status"
            | b"language"
            | b"website"
            | b"set"
            | b"setid"
            | b"id"
            | b"thumb"
            | b"fanart"
            | b"uniqueid"
            | b"tmdbid"
            | b"imdbid"
            | b"tvdbid"
            | b"wikidataid"
    )
}

fn replace_rich_root_tag(event: &BytesStart<'_>, patch: &MovieNfoMetadata) -> bool {
    let tag = event.name();
    if tag.as_ref() == b"uniqueid" {
        let identity_type = event
            .attributes()
            .flatten()
            .find(|attribute| attribute.key.as_ref() == b"type")
            .map(|attribute| attribute.value.into_owned());
        return match identity_type.as_deref() {
            Some(b"official website") => patch
                .website
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
            Some(b"tmdb") => provider_id_is_present(b"tmdbid", patch),
            Some(b"imdb") => provider_id_is_present(b"imdbid", patch),
            Some(b"tvdb") => provider_id_is_present(b"tvdbid", patch),
            Some(b"wikidata") => provider_id_is_present(b"wikidataid", patch),
            _ => false,
        };
    }
    let tag = tag.as_ref();
    if !rich_root_tag(tag) {
        return false;
    }
    match tag {
        b"actor" => !patch.actors.is_empty(),
        b"country" => !patch.countries.is_empty(),
        b"genre" => !patch.genres.is_empty(),
        b"studio" => !patch.studios.is_empty(),
        b"director" => !patch.directors.is_empty(),
        b"writer" | b"credits" => !patch.writers.is_empty(),
        b"trailer" => !patch.trailers.is_empty(),
        b"rating" => patch.rating.is_some(),
        b"votes" => patch.votes.is_some(),
        b"tagline" => patch
            .tagline
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        b"mpaa" => patch
            .certification
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        b"premiered" => patch
            .premiered
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        b"releasedate" => patch
            .releasedate
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        b"lastaired" | b"lastairdate" | b"enddate" | b"ended" => patch
            .last_air_date
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        b"runtime" => patch.runtime.is_some(),
        b"status" => patch
            .status
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        b"language" => patch
            .original_language
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        b"website" => patch
            .website
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        b"id" => provider_id_is_present(b"imdbid", patch),
        b"set" => patch
            .set_name
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        b"setid" => patch
            .set_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        b"thumb" => patch
            .poster_url
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        b"fanart" => patch
            .fanart_url
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()),
        b"tmdbid" | b"imdbid" | b"tvdbid" | b"wikidataid" => provider_id_is_present(tag, patch),
        _ => false,
    }
}

fn provider_id_is_present(tag: &[u8], patch: &MovieNfoMetadata) -> bool {
    let provider = match tag {
        b"tmdbid" => "tmdb",
        b"imdbid" => "imdb",
        b"tvdbid" => "tvdb",
        b"wikidataid" => "wikidata",
        _ => return false,
    };
    patch
        .provider_ids
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case(provider) && !value.trim().is_empty())
}

fn append_movie_nfo_fields(
    writer: &mut Writer<Vec<u8>>,
    patch: &MovieNfoMetadata,
) -> Result<(), NfoWriteError> {
    for actor in patch.actors.iter().take(MAX_MOVIE_NFO_ACTORS) {
        start_element(writer, "actor", None)?;
        write_simple_element(writer, "name", actor.name.trim())?;
        if let Some(character) = non_empty(actor.character.as_deref()) {
            write_simple_element(writer, "role", character)?;
        }
        write_simple_element(writer, "type", "Actor")?;
        if !actor.id.trim().is_empty() {
            let tag = match actor.provider.as_deref().map(str::trim) {
                Some("imdb") => "imdbid",
                Some("douban") => "doubanid",
                _ => "tmdbid",
            };
            write_simple_element(writer, tag, actor.id.trim())?;
        }
        if let Some(order) = actor.order {
            write_simple_element(writer, "order", &order.to_string())?;
        }
        end_element(writer, "actor")?;
    }
    for director in &patch.directors {
        write_credit_element(writer, "director", director)?;
    }
    for writer_credit in &patch.writers {
        write_credit_element(writer, "writer", writer_credit)?;
    }
    for writer_credit in &patch.writers {
        write_credit_element(writer, "credits", writer_credit)?;
    }
    for trailer in patch
        .trailers
        .iter()
        .filter_map(|value| non_empty(Some(value.as_str())))
    {
        write_simple_element(writer, "trailer", trailer)?;
    }
    if let Some(rating) = patch.rating {
        write_simple_element(writer, "rating", &rating.to_string())?;
    }
    if let Some(votes) = patch.votes {
        write_simple_element(writer, "votes", &votes.to_string())?;
    }
    if let Some(tagline) = non_empty(patch.tagline.as_deref()) {
        write_simple_element(writer, "tagline", tagline)?;
    }
    if let Some(certification) = non_empty(patch.certification.as_deref()) {
        write_simple_element(writer, "mpaa", certification)?;
    }
    if let Some(premiered) = non_empty(patch.premiered.as_deref()) {
        write_simple_element(writer, "premiered", premiered)?;
    }
    if let Some(releasedate) = non_empty(patch.releasedate.as_deref()) {
        write_simple_element(writer, "releasedate", releasedate)?;
    }
    if let Some(last_air_date) = non_empty(patch.last_air_date.as_deref()) {
        write_simple_element(writer, "lastaired", last_air_date)?;
    }
    if let Some(runtime) = patch.runtime {
        write_simple_element(writer, "runtime", &runtime.to_string())?;
    }
    if let Some(status) = non_empty(patch.status.as_deref()) {
        write_simple_element(writer, "status", status)?;
    }
    if let Some(language) = non_empty(patch.original_language.as_deref()) {
        write_simple_element(writer, "language", language)?;
    }
    if let Some(website) = non_empty(patch.website.as_deref()) {
        write_simple_element(writer, "website", website)?;
        write_uniqueid(writer, "official website", website, false)?;
    }
    if let Some(set_name) = non_empty(patch.set_name.as_deref()) {
        write_simple_element(writer, "set", set_name)?;
    }
    if let Some(set_id) = non_empty(patch.set_id.as_deref()) {
        write_simple_element(writer, "setid", set_id)?;
    }
    if let Some(poster_url) = non_empty(patch.poster_url.as_deref()) {
        let mut thumb = BytesStart::new("thumb");
        thumb.push_attribute(("aspect", "poster"));
        writer
            .write_event(Event::Start(thumb))
            .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
        write_text(writer, poster_url)?;
        end_element(writer, "thumb")?;
    }
    if let Some(fanart_url) = non_empty(patch.fanart_url.as_deref()) {
        start_element(writer, "fanart", None)?;
        write_simple_element(writer, "thumb", fanart_url)?;
        end_element(writer, "fanart")?;
    }
    for country in patch
        .countries
        .iter()
        .filter_map(|value| non_empty(Some(value.as_str())))
    {
        write_simple_element(writer, "country", country)?;
    }
    for genre in patch
        .genres
        .iter()
        .filter_map(|value| non_empty(Some(value.as_str())))
    {
        write_simple_element(writer, "genre", genre)?;
    }
    for studio in patch
        .studios
        .iter()
        .filter_map(|value| non_empty(Some(value.as_str())))
    {
        write_simple_element(writer, "studio", studio)?;
    }
    append_provider_ids(writer, &patch.provider_ids)?;
    Ok(())
}

fn probe_has_serializable_streams(probe: &MediaProbeResult) -> bool {
    probe
        .streams
        .iter()
        .take(MAX_MOVIE_NFO_STREAMS)
        .next()
        .is_some()
}

fn serialize_probe_streamdetails(
    probe: &MediaProbeResult,
) -> Result<Option<Vec<u8>>, NfoWriteError> {
    if !probe_has_serializable_streams(probe) {
        return Ok(None);
    }
    let mut writer = Writer::new(Vec::new());
    start_element(&mut writer, "streamdetails", None)?;
    let duration_seconds = probe
        .duration_ticks
        .filter(|ticks| *ticks > 0)
        .map(|ticks| ticks as f64 / 10_000_000.0)
        .filter(|seconds| *seconds <= 315_576_000.0);
    for stream in probe.streams.iter().take(MAX_MOVIE_NFO_STREAMS) {
        match stream.stream_type {
            StreamType::Video => write_video_stream(&mut writer, stream, probe, duration_seconds)?,
            StreamType::Audio => write_audio_stream(&mut writer, stream)?,
            StreamType::Subtitle => write_subtitle_stream(&mut writer, stream)?,
        }
    }
    end_element(&mut writer, "streamdetails")?;
    Ok(Some(writer.into_inner()))
}

fn rewrite_nfo_fileinfo(
    original: &[u8],
    streamdetails: Option<&[u8]>,
) -> Result<Vec<u8>, NfoWriteError> {
    parse_nfo(original).map_err(NfoWriteError::Nfo)?;
    let mut reader = Reader::from_reader(original);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut saw_root = false;
    let mut fileinfo_depth = None;
    let mut skipped_streamdetails_depth = None;
    let mut saw_fileinfo = false;
    let mut wrote_streamdetails = false;

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
                if skipped_streamdetails_depth.is_some() {
                    depth = depth.saturating_add(1);
                    buffer.clear();
                    continue;
                }
                if depth == 0 {
                    saw_root = true;
                }
                if fileinfo_depth == Some(depth)
                    && event.name().as_ref().eq_ignore_ascii_case(b"streamdetails")
                {
                    depth = depth.saturating_add(1);
                    skipped_streamdetails_depth = Some(depth);
                    buffer.clear();
                    continue;
                }
                let starts_fileinfo =
                    depth == 1 && event.name().as_ref().eq_ignore_ascii_case(b"fileinfo");
                writer
                    .write_event(Event::Start(event.to_owned()))
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                depth = depth.saturating_add(1);
                if starts_fileinfo {
                    fileinfo_depth = Some(depth);
                    saw_fileinfo = true;
                }
            }
            Ok(Event::Empty(event)) => {
                if skipped_streamdetails_depth.is_some() {
                    buffer.clear();
                    continue;
                }
                if fileinfo_depth == Some(depth)
                    && event.name().as_ref().eq_ignore_ascii_case(b"streamdetails")
                {
                    write_optional_xml_fragment(
                        &mut writer,
                        streamdetails,
                        &mut wrote_streamdetails,
                    );
                    buffer.clear();
                    continue;
                }
                if depth == 1 && event.name().as_ref().eq_ignore_ascii_case(b"fileinfo") {
                    saw_fileinfo = true;
                    if !wrote_streamdetails && let Some(streamdetails) = streamdetails {
                        let fileinfo_name = std::str::from_utf8(event.name().as_ref())
                            .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?
                            .to_owned();
                        writer
                            .write_event(Event::Start(event.to_owned()))
                            .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                        writer.get_mut().extend_from_slice(streamdetails);
                        writer
                            .write_event(Event::End(BytesEnd::new(fileinfo_name)))
                            .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                        wrote_streamdetails = true;
                    } else {
                        writer
                            .write_event(Event::Empty(event.to_owned()))
                            .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                    }
                    buffer.clear();
                    continue;
                }
                if depth == 0 {
                    saw_root = true;
                    let name = std::str::from_utf8(event.name().as_ref())
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?
                        .to_owned();
                    writer
                        .write_event(Event::Start(event.to_owned()))
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                    writer
                        .write_event(Event::End(BytesEnd::new(name)))
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                } else {
                    writer
                        .write_event(Event::Empty(event.to_owned()))
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                }
            }
            Ok(Event::End(event)) => {
                if skipped_streamdetails_depth == Some(depth) {
                    skipped_streamdetails_depth = None;
                    write_optional_xml_fragment(
                        &mut writer,
                        streamdetails,
                        &mut wrote_streamdetails,
                    );
                    depth = depth.saturating_sub(1);
                    buffer.clear();
                    continue;
                }
                if skipped_streamdetails_depth.is_some() {
                    depth = depth.saturating_sub(1);
                    buffer.clear();
                    continue;
                }
                if fileinfo_depth == Some(depth)
                    && event.name().as_ref().eq_ignore_ascii_case(b"fileinfo")
                {
                    write_optional_xml_fragment(
                        &mut writer,
                        streamdetails,
                        &mut wrote_streamdetails,
                    );
                    fileinfo_depth = None;
                } else if depth == 1
                    && !saw_fileinfo
                    && let Some(streamdetails) = streamdetails
                {
                    writer
                        .write_event(Event::Start(BytesStart::new("fileinfo")))
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                    writer.get_mut().extend_from_slice(streamdetails);
                    writer
                        .write_event(Event::End(BytesEnd::new("fileinfo")))
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                    saw_fileinfo = true;
                    wrote_streamdetails = true;
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

    let result = writer.into_inner();
    if result.len() > MAX_LOCAL_NFO_BYTES {
        return Err(NfoWriteError::InvalidMetadata(
            "NFO exceeds the output size limit".to_owned(),
        ));
    }
    parse_nfo(&result).map_err(NfoWriteError::Nfo)?;
    Ok(result)
}

fn write_optional_xml_fragment(
    writer: &mut Writer<Vec<u8>>,
    fragment: Option<&[u8]>,
    already_written: &mut bool,
) {
    if *already_written {
        return;
    }
    if let Some(fragment) = fragment {
        writer.get_mut().extend_from_slice(fragment);
    }
    *already_written = true;
}

fn write_video_stream(
    writer: &mut Writer<Vec<u8>>,
    stream: &MediaStreamResult,
    probe: &MediaProbeResult,
    duration_seconds: Option<f64>,
) -> Result<(), NfoWriteError> {
    start_element(writer, "video", None)?;
    if let Some(codec) = normalized_probe_codec(stream.codec.as_deref()) {
        write_simple_element(writer, "codec", codec)?;
        write_simple_element(writer, "micodec", codec)?;
    }
    let has_single_video = probe
        .streams
        .iter()
        .filter(|stream| stream.stream_type == StreamType::Video)
        .take(2)
        .count()
        == 1;
    if let Some(value) = probe_detail_integer(stream, "BitRate").or_else(|| {
        has_single_video
            .then_some(probe.bitrate)
            .flatten()
            .filter(|bitrate| *bitrate > 0 && *bitrate <= 100_000_000_000)
    }) {
        write_simple_element(writer, "bitrate", &value.to_string())?;
    }
    for (detail, tag) in [("Width", "width"), ("Height", "height")] {
        if let Some(value) =
            probe_detail_integer(stream, detail).filter(|value| (1..=100_000).contains(value))
        {
            write_simple_element(writer, tag, &value.to_string())?;
        }
    }
    if let Some(aspect) =
        probe_detail_text(stream, "AspectRatio").filter(|value| valid_aspect_ratio(value))
    {
        write_simple_element(writer, "aspect", aspect)?;
        write_simple_element(writer, "aspectratio", aspect)?;
    }
    if let Some(frame_rate) = probe_detail_frame_rate(stream, "RealFrameRate") {
        write_simple_element(writer, "framerate", &frame_rate)?;
    }
    write_optional_stream_text(writer, "language", stream.language.as_deref())?;
    if let Some(scan_type) = probe_detail_text(stream, "ScanType").and_then(normalize_scan_type) {
        write_simple_element(writer, "scantype", scan_type)?;
    }
    write_bool_element(writer, "default", stream.is_default)?;
    write_bool_element(writer, "forced", stream.is_forced)?;
    if let Some(seconds) = duration_seconds {
        let rounded_minutes = (seconds / 60.0).round();
        if rounded_minutes.is_finite() && rounded_minutes <= i64::MAX as f64 {
            write_simple_element(writer, "duration", &(rounded_minutes as i64).to_string())?;
        }
        write_simple_element(writer, "durationinseconds", &format_probe_seconds(seconds))?;
    }
    end_element(writer, "video")
}

fn write_audio_stream(
    writer: &mut Writer<Vec<u8>>,
    stream: &MediaStreamResult,
) -> Result<(), NfoWriteError> {
    start_element(writer, "audio", None)?;
    if let Some(codec) = normalized_probe_codec(stream.codec.as_deref()) {
        write_simple_element(writer, "codec", codec)?;
        write_simple_element(writer, "micodec", codec)?;
    }
    write_optional_stream_text(writer, "language", stream.language.as_deref())?;
    if let Some(channels) =
        probe_detail_integer(stream, "Channels").filter(|value| (1..=64).contains(value))
    {
        write_simple_element(writer, "channels", &channels.to_string())?;
    }
    if let Some(sample_rate) =
        probe_detail_integer(stream, "SampleRate").filter(|value| (1..=768_000).contains(value))
    {
        write_simple_element(writer, "samplingrate", &sample_rate.to_string())?;
    }
    write_bool_element(writer, "default", stream.is_default)?;
    write_bool_element(writer, "forced", stream.is_forced)?;
    end_element(writer, "audio")
}

fn write_subtitle_stream(
    writer: &mut Writer<Vec<u8>>,
    stream: &MediaStreamResult,
) -> Result<(), NfoWriteError> {
    start_element(writer, "subtitle", None)?;
    if let Some(codec) = normalized_probe_codec(stream.codec.as_deref()) {
        write_simple_element(writer, "codec", codec)?;
        write_simple_element(writer, "micodec", codec)?;
    }
    write_optional_stream_text(writer, "language", stream.language.as_deref())?;
    write_bool_element(writer, "default", stream.is_default)?;
    write_bool_element(writer, "forced", stream.is_forced)?;
    end_element(writer, "subtitle")
}

fn normalized_probe_codec(value: Option<&str>) -> Option<&str> {
    let value = value?.trim();
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return None;
    }
    Some(match value {
        "hdmv_pgs_subtitle" => "PGSSUB",
        "dvd_subtitle" => "VOBSUB",
        _ => value,
    })
}

fn probe_detail_text<'a>(stream: &'a MediaStreamResult, key: &str) -> Option<&'a str> {
    stream
        .details
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| {
            !value.is_empty() && value.len() <= 256 && value.chars().all(is_valid_xml_character)
        })
}

fn probe_detail_integer(stream: &MediaStreamResult, key: &str) -> Option<i64> {
    let value = stream.details.get(key)?;
    let value = value.as_i64().or_else(|| {
        let value = value.as_str()?;
        if value.len() > 20 {
            return None;
        }
        value.parse::<i64>().ok()
    })?;
    (value > 0).then_some(value)
}

fn probe_detail_frame_rate(stream: &MediaStreamResult, key: &str) -> Option<String> {
    let raw = probe_detail_text(stream, key)?;
    if raw.len() > 64 {
        return None;
    }
    let rate = if let Some((numerator, denominator)) = raw.split_once('/') {
        let denominator = denominator.parse::<f64>().ok()?;
        if denominator <= 0.0 {
            return None;
        }
        numerator.parse::<f64>().ok()? / denominator
    } else {
        raw.parse::<f64>().ok()?
    };
    if rate.is_finite() && (0.0..=1000.0).contains(&rate) && rate > 0.0 {
        Some(
            format!("{rate:.6}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_owned(),
        )
    } else {
        None
    }
}

fn format_probe_seconds(seconds: f64) -> String {
    if seconds.fract().abs() < f64::EPSILON {
        return format!("{seconds:.0}");
    }
    format!("{seconds:.3}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn valid_aspect_ratio(value: &str) -> bool {
    let Some((width, height)) = value.split_once(':') else {
        return false;
    };
    width
        .parse::<u32>()
        .ok()
        .is_some_and(|width| (1..=100_000).contains(&width))
        && height
            .parse::<u32>()
            .ok()
            .is_some_and(|height| (1..=100_000).contains(&height))
}

fn normalize_scan_type(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "progressive" => Some("progressive"),
        "interlaced" | "tt" | "bb" | "tb" | "bt" => Some("interlaced"),
        _ => None,
    }
}

fn is_valid_xml_character(character: char) -> bool {
    matches!(
        character,
        '\u{9}'..='\u{D}'
            | '\u{20}'..='\u{D7FF}'
            | '\u{E000}'..='\u{FFFD}'
            | '\u{10000}'..='\u{10FFFF}'
    )
}

fn write_optional_stream_text(
    writer: &mut Writer<Vec<u8>>,
    tag: &str,
    value: Option<&str>,
) -> Result<(), NfoWriteError> {
    if let Some(value) = non_empty(value)
        .filter(|value| value.len() <= 64 && value.chars().all(is_valid_xml_character))
    {
        write_simple_element(writer, tag, value)?;
    }
    Ok(())
}

fn write_bool_element(
    writer: &mut Writer<Vec<u8>>,
    tag: &str,
    value: bool,
) -> Result<(), NfoWriteError> {
    write_simple_element(writer, tag, if value { "True" } else { "False" })
}

fn validate_movie_nfo_actors(patch: &MovieNfoMetadata) -> Result<(), NfoWriteError> {
    for actor in patch.actors.iter().take(MAX_MOVIE_NFO_ACTORS) {
        if actor.name.trim().is_empty() {
            return Err(NfoWriteError::InvalidMetadata(
                "movie actor requires a name".to_owned(),
            ));
        }
    }
    Ok(())
}

fn write_credit_element(
    writer: &mut Writer<Vec<u8>>,
    tag: &str,
    credit: &MovieNfoCredit,
) -> Result<(), NfoWriteError> {
    if credit.provider_id.trim().is_empty() || credit.name.trim().is_empty() {
        return Ok(());
    }
    let mut start = BytesStart::new(tag);
    start.push_attribute(("tmdbid", credit.provider_id.as_str()));
    writer
        .write_event(Event::Start(start))
        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
    write_text(writer, &credit.name)?;
    end_element(writer, tag)
}

fn append_provider_ids(
    writer: &mut Writer<Vec<u8>>,
    provider_ids: &BTreeMap<String, String>,
) -> Result<(), NfoWriteError> {
    for (provider, id) in provider_ids {
        let Some(id) = non_empty(Some(id.as_str())) else {
            continue;
        };
        let provider = provider.to_ascii_lowercase();
        let Some(tag) = (match provider.as_str() {
            "tmdb" => Some("tmdbid"),
            "imdb" => Some("imdbid"),
            "tvdb" => Some("tvdbid"),
            "wikidata" => Some("wikidataid"),
            _ => None,
        }) else {
            continue;
        };
        write_uniqueid(writer, &provider, id, provider == "tmdb")?;
        write_simple_element(writer, tag, id)?;
        if provider == "imdb" {
            write_simple_element(writer, "id", id)?;
        }
    }
    Ok(())
}

fn write_uniqueid(
    writer: &mut Writer<Vec<u8>>,
    provider: &str,
    id: &str,
    default: bool,
) -> Result<(), NfoWriteError> {
    let mut uniqueid = BytesStart::new("uniqueid");
    uniqueid.push_attribute(("type", provider));
    if default {
        uniqueid.push_attribute(("default", "true"));
    }
    writer
        .write_event(Event::Start(uniqueid))
        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
    write_text(writer, id)?;
    end_element(writer, "uniqueid")
}

fn start_element(
    writer: &mut Writer<Vec<u8>>,
    tag: &str,
    attribute: Option<(&str, &str)>,
) -> Result<(), NfoWriteError> {
    let mut start = BytesStart::new(tag);
    if let Some((name, value)) = attribute {
        start.push_attribute((name, value));
    }
    writer
        .write_event(Event::Start(start))
        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))
}

fn end_element(writer: &mut Writer<Vec<u8>>, tag: &str) -> Result<(), NfoWriteError> {
    writer
        .write_event(Event::End(BytesEnd::new(tag)))
        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))
}

fn write_simple_element(
    writer: &mut Writer<Vec<u8>>,
    tag: &str,
    value: &str,
) -> Result<(), NfoWriteError> {
    if value.trim().is_empty() {
        return Ok(());
    }
    start_element(writer, tag, None)?;
    write_text(writer, value)?;
    end_element(writer, tag)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub async fn write_nfo_atomically(target: &Path, patch: &NfoMetadata) -> Result<(), NfoWriteError> {
    write_nfo_atomically_with_hook(target, patch, None)
        .await
        .map(|_| ())
}

pub async fn write_movie_nfo_atomically(
    target: &Path,
    patch: &MovieNfoMetadata,
) -> Result<(), NfoWriteError> {
    write_nfo_atomically_with_rewriter(target, |original| rewrite_movie_nfo(original, patch), None)
        .await
        .map(|_| ())
}

pub async fn write_series_nfo_atomically(
    target: &Path,
    patch: &MovieNfoMetadata,
) -> Result<(), NfoWriteError> {
    write_nfo_atomically_with_rewriter(target, |original| rewrite_series_nfo(original, patch), None)
        .await
        .map(|_| ())
}

#[derive(Clone)]
pub struct NfoWriteService {
    database: Database,
    config_dir: Option<PathBuf>,
}

impl NfoWriteService {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            config_dir: None,
        }
    }

    pub fn new_with_config_dir(database: Database, config_dir: PathBuf) -> Self {
        Self {
            database,
            config_dir: Some(config_dir),
        }
    }

    pub async fn read_item_projection(
        &self,
        item_id: &str,
    ) -> Result<Option<LocalNfoProjection>, NfoWriteError> {
        let target = self.item_nfo_target(item_id).await?;
        self.read_item_projection_at_target(&target).await
    }

    pub(crate) async fn read_item_projection_with_writeback_context_cached(
        &self,
        season_number: Option<i64>,
        context: &StoredMediaWritebackContext,
        cache: &LocalNfoProjectionCache,
    ) -> Result<(Option<LocalNfoProjection>, bool), NfoWriteError> {
        let source = context.source.as_ref().ok_or(NfoWriteError::ItemNotFound)?;
        let target = self
            .item_nfo_target_from_source(&context.item_type, season_number, source)
            .await?;
        let cell = {
            let mut entries = cache.entries.lock().await;
            entries
                .entry(target.clone())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };
        let mut loaded = false;
        let projection = cell
            .get_or_init(|| async {
                loaded = true;
                self.read_item_projection_at_target(&target)
                    .await
                    .map_err(CachedNfoProjectionError::from_write_error)
            })
            .await;
        let projection = projection
            .clone()
            .map_err(CachedNfoProjectionError::into_write_error)?;
        Ok((projection, loaded))
    }

    pub(crate) async fn read_item_projection_with_writeback_context(
        &self,
        season_number: Option<i64>,
        context: &crate::storage::StoredMediaWritebackContext,
    ) -> Result<Option<LocalNfoProjection>, NfoWriteError> {
        let source = context.source.as_ref().ok_or(NfoWriteError::ItemNotFound)?;
        let target = self
            .item_nfo_target_from_source(&context.item_type, season_number, source)
            .await?;
        self.read_item_projection_at_target(&target).await
    }

    async fn read_item_projection_at_target(
        &self,
        target: &Path,
    ) -> Result<Option<LocalNfoProjection>, NfoWriteError> {
        let bytes = match fs::read(&target).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(target, error)),
        };
        parse_local_nfo_projection(&bytes)
            .map(Some)
            .map_err(NfoWriteError::Nfo)
    }

    pub async fn write_item_nfo(
        &self,
        item_id: &str,
        patch: &NfoMetadata,
    ) -> Result<NfoWriteReport, NfoWriteError> {
        let target = self.item_nfo_target(item_id).await?;
        let write = write_nfo_atomically_with_hook(&target, patch, None).await?;
        self.finish_item_write(item_id, target, write).await
    }

    pub async fn write_item_movie_nfo(
        &self,
        item_id: &str,
        patch: &MovieNfoMetadata,
    ) -> Result<NfoWriteReport, NfoWriteError> {
        let Some((item_type, season_number, sort_title, added_at, source)) = self
            .database
            .find_movie_nfo_writeback_context(item_id)
            .await?
        else {
            return Err(NfoWriteError::ItemNotFound);
        };
        let source = source.ok_or(NfoWriteError::ItemNotFound)?;
        let target = self
            .item_nfo_target_from_source(&item_type, season_number, &source)
            .await?;
        let sort_title = non_empty(sort_title.as_deref()).map(str::to_owned);
        let date_added = added_at.and_then(nfo_date_added);
        let write = write_nfo_atomically_with_rewriter(
            &target,
            |original| {
                let rich = rewrite_movie_nfo(original, patch)?;
                rewrite_movie_nfo_auxiliary_fields(
                    &rich,
                    sort_title.as_deref(),
                    date_added.as_deref(),
                )
            },
            None,
        )
        .await?;
        self.finish_item_write(item_id, target, write).await
    }

    pub async fn write_item_probe_details(
        &self,
        item_id: &str,
        source_id: &str,
        probe: &MediaProbeResult,
    ) -> Result<bool, NfoWriteError> {
        let Some((item_type, _, sort_title, added_at, writeback_source)) = self
            .database
            .find_movie_nfo_writeback_context(item_id)
            .await?
        else {
            return Ok(false);
        };
        if item_type != "MOVIE" {
            return Ok(false);
        }
        let Some(writeback_source) = writeback_source else {
            return Ok(false);
        };
        let is_strm = Path::new(&writeback_source.relative_path)
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"));
        if writeback_source.source_id != source_id || is_strm {
            return Ok(false);
        }
        let target = self
            .item_nfo_target_from_source("MOVIE", None, &writeback_source)
            .await?;
        let sort_title = non_empty(sort_title.as_deref()).map(str::to_owned);
        let date_added = added_at.and_then(nfo_date_added);
        let write = write_nfo_atomically_with_rewriter(
            &target,
            |original| {
                let details = rewrite_nfo_probe_details(original, probe)?;
                rewrite_movie_nfo_auxiliary_fields(
                    &details,
                    sort_title.as_deref(),
                    date_added.as_deref(),
                )
            },
            None,
        )
        .await?;
        self.finish_item_write(item_id, target, write).await?;
        Ok(true)
    }

    pub async fn write_item_series_nfo(
        &self,
        item_id: &str,
        patch: &MovieNfoMetadata,
    ) -> Result<NfoWriteReport, NfoWriteError> {
        let target = self.item_nfo_target(item_id).await?;
        let write = write_nfo_atomically_with_rewriter(
            &target,
            |original| rewrite_series_nfo(original, patch),
            None,
        )
        .await?;
        self.finish_item_write(item_id, target, write).await
    }

    async fn finish_item_write(
        &self,
        item_id: &str,
        target: PathBuf,
        write: NfoFileWrite,
    ) -> Result<NfoWriteReport, NfoWriteError> {
        let NfoFileWrite {
            content_fingerprint,
            content,
            changed,
            file_fingerprint,
        } = write;
        if !changed {
            let fingerprint = match file_fingerprint {
                Some(fingerprint) => fingerprint,
                None => nfo_fingerprint(&target)
                    .await
                    .map_err(|error| io_error(&target, error))?,
            };
            return Ok(NfoWriteReport {
                path: target,
                fingerprint,
                content_fingerprint,
                changed,
            });
        }
        self.mirror_item_nfo_if_enabled(item_id, &target, &content)
            .await?;
        let fingerprint = nfo_fingerprint(&target)
            .await
            .map_err(|error| io_error(&target, error))?;
        self.database
            .sync_media_item_nfo_state(item_id, &content_fingerprint, &fingerprint)
            .await?;
        Ok(NfoWriteReport {
            path: target,
            fingerprint,
            content_fingerprint,
            changed,
        })
    }

    async fn mirror_item_nfo_if_enabled(
        &self,
        item_id: &str,
        source: &Path,
        content: &[u8],
    ) -> Result<(), NfoWriteError> {
        let Some(config_dir) = self.config_dir.as_deref() else {
            return Ok(());
        };
        if !item_metadata_writeback_enabled(&self.database, item_id).await? {
            return Ok(());
        }
        let metadata_root_path = metadata_root(config_dir);
        reject_metadata_symlinks(&metadata_root_path).await?;
        let metadata_directory = library_item_directory(config_dir, item_id)
            .map_err(|error| NfoWriteError::InvalidMetadata(error.to_string()))?;
        fs::create_dir_all(&metadata_directory)
            .await
            .map_err(|error| io_error(&metadata_directory, error))?;
        reject_metadata_symlinks(&metadata_directory).await?;
        let canonical_root = fs::canonicalize(&metadata_root_path)
            .await
            .map_err(|error| io_error(&metadata_root_path, error))?;
        let canonical_directory = fs::canonicalize(&metadata_directory)
            .await
            .map_err(|error| io_error(&metadata_directory, error))?;
        if !canonical_directory.starts_with(&canonical_root) {
            return Err(NfoWriteError::PathOutsideRoot(canonical_directory));
        }
        let file_name = source
            .file_name()
            .ok_or_else(|| NfoWriteError::PathOutsideRoot(source.to_owned()))?;
        let target = canonical_directory.join(file_name);
        write_nfo_atomically_with_rewriter(&target, |_| Ok(content.to_owned()), None).await?;
        Ok(())
    }

    async fn item_nfo_target(&self, item_id: &str) -> Result<PathBuf, NfoWriteError> {
        let kind = self
            .database
            .find_media_item_kind(item_id)
            .await?
            .ok_or(NfoWriteError::ItemNotFound)?;
        let source = match kind.item_type.as_str() {
            "MOVIE" | "EPISODE" | "VIDEO" => {
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
        self.item_nfo_target_from_source(&kind.item_type, kind.season_number, &source)
            .await
    }

    async fn item_nfo_target_from_source(
        &self,
        item_type: &str,
        season_number: Option<i64>,
        source: &StoredMediaSourcePath,
    ) -> Result<PathBuf, NfoWriteError> {
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
        let target = match item_type {
            "MOVIE" => find_nfo_path(&media_path)
                .await
                .unwrap_or_else(|| directory.join("movie.nfo")),
            "EPISODE" => find_episode_nfo_target(&media_path, &directory).await,
            "VIDEO" => media_path.with_extension("nfo"),
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
            "SEASON" => find_season_nfo_target(&directory, season_number).await,
            _ => return Err(NfoWriteError::ItemNotFound),
        };
        let target_parent = target.parent().unwrap_or_else(|| Path::new("."));
        let target_parent = fs::canonicalize(target_parent)
            .await
            .map_err(|error| io_error(target_parent, error))?;
        if !target_parent.starts_with(&root) {
            return Err(NfoWriteError::PathOutsideRoot(target_parent));
        }
        Ok(target)
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

    pub fn new_with_config_dir(database: Database, config_dir: PathBuf) -> Self {
        Self {
            nfo: NfoWriteService::new_with_config_dir(database.clone(), config_dir),
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
                premiere_date: None,
                rating: None,
                rating_source: None,
                provider_ids_json: None,
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
    pub content_fingerprint: Vec<u8>,
    pub changed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NfoFileWrite {
    content_fingerprint: Vec<u8>,
    content: Vec<u8>,
    changed: bool,
    file_fingerprint: Option<Vec<u8>>,
}

async fn write_nfo_atomically_with_hook(
    target: &Path,
    patch: &NfoMetadata,
    before_replace: Option<fn(&Path) -> std::io::Result<()>>,
) -> Result<NfoFileWrite, NfoWriteError> {
    write_nfo_atomically_with_rewriter(
        target,
        |original| rewrite_nfo(original, patch),
        before_replace,
    )
    .await
}

async fn write_nfo_atomically_with_rewriter<F>(
    target: &Path,
    rewrite: F,
    before_replace: Option<fn(&Path) -> std::io::Result<()>>,
) -> Result<NfoFileWrite, NfoWriteError>
where
    F: Fn(&[u8]) -> Result<Vec<u8>, NfoWriteError>,
{
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let target_is_symlink = fs::symlink_metadata(target)
        .await
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false);
    if target_is_symlink {
        return Err(NfoWriteError::SymlinkTarget(target.to_owned()));
    }
    let before = file_stamp(target).await?;
    let file_fingerprint =
        before.map(|stamp| nfo_fingerprint_from_stamp(target, stamp.size, stamp.modified_at));
    let original = match fs::read(target).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(source) => return Err(io_error(target, source)),
    };
    let rewritten = rewrite(&original)?;
    let write = NfoFileWrite {
        content_fingerprint: nfo_content_fingerprint(&rewritten),
        content: rewritten.clone(),
        changed: rewritten != original,
        file_fingerprint,
    };
    if !write.changed {
        return Ok(write);
    }
    let temporary = parent.join(format!(".lux-{}.nfo.tmp", Uuid::now_v7()));
    crate::application::internal_write::register(&temporary);
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
        crate::application::internal_write::register(target);
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
        crate::application::internal_write::finalize(
            target,
            crate::application::internal_write::file_stamp(target)
                .await
                .ok()
                .flatten(),
            &rewritten,
        );
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result.map(|_| write)
}

fn new_nfo(patch: &NfoMetadata, root_tag: &str) -> Result<Vec<u8>, NfoWriteError> {
    let mut writer = Writer::new(Vec::new());
    writer
        .write_event(Event::Start(BytesStart::new(root_tag)))
        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
    let mut updated = BTreeSet::new();
    append_missing_fields(&mut writer, patch, &mut updated)?;
    writer
        .write_event(Event::End(BytesEnd::new(root_tag)))
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

async fn reject_metadata_symlinks(path: &Path) -> Result<(), NfoWriteError> {
    let mut current = Some(path.to_owned());
    while let Some(candidate) = current {
        match fs::symlink_metadata(&candidate).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(NfoWriteError::SymlinkTarget(candidate));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(NfoWriteError::Io {
                    path: candidate,
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotADirectory,
                        "metadata path component is not a directory",
                    ),
                });
            }
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                current = candidate.parent().map(Path::to_owned);
            }
            Err(source) => return Err(io_error(&candidate, source)),
        }
    }
    Ok(())
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
    use crate::{
        application::{libraries::LibraryService, scanner::LibraryScanner},
        config::Config,
        library::LibraryKind,
    };

    #[test]
    fn local_nfo_cache_decodes_legacy_details_and_versioned_semantic_state() {
        let details = LocalNfoDetails::default();
        let legacy = serde_json::to_string(&details).expect("legacy cache json");
        let (legacy_details, legacy_semantic, legacy_relation) =
            decode_local_nfo_cache(&legacy).expect("legacy cache");
        assert_eq!(legacy_details, details);
        assert_eq!(legacy_semantic, None);
        assert_eq!(legacy_relation, None);

        let fingerprint = [7_u8; 32];
        let versioned = encode_local_nfo_cache(&details, Some(&fingerprint), Some(&fingerprint))
            .expect("versioned cache json");
        let (versioned_details, semantic, relation) =
            decode_local_nfo_cache(&versioned).expect("versioned cache");
        assert_eq!(versioned_details, details);
        assert_eq!(semantic.as_deref(), Some(fingerprint.as_slice()));
        assert_eq!(relation.as_deref(), Some(fingerprint.as_slice()));
    }

    #[test]
    fn local_nfo_semantic_fingerprint_ignores_lexical_changes_and_keeps_unknown_xml() {
        let original = br#"<movie><title>Title &amp; text</title><actor sortorder="0" tmdbid="9"><name>Actor</name></actor><extension key="x">preserved</extension><empty/></movie>"#;
        let equivalent = br#"<?xml version="1.0"?>
<movie>
  <!-- ignored editor comment -->
  <title><![CDATA[Title & text]]></title>
  <actor tmdbid='9' sortorder='0'><name>Actor</name></actor>
  <extension key='x'>preserved</extension><empty></empty>
</movie>"#;
        let (original_projection, original_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(original).expect("original NFO");
        let (equivalent_projection, equivalent_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(equivalent)
                .expect("equivalent NFO");
        assert_eq!(original_fingerprint, equivalent_fingerprint);
        assert_eq!(original_projection, equivalent_projection);
        let (_, ordinary_fingerprint) =
            parse_local_nfo_projection_inner(original, false).expect("ordinary projection mode");
        assert!(ordinary_fingerprint.is_empty());

        let bare_root = b"<movie><extension/></movie>";
        let root_with_misc_whitespace = b" \n<movie><extension/></movie>\r\n ";
        let (_, bare_root_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(bare_root).expect("bare root");
        let (_, root_whitespace_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(root_with_misc_whitespace)
                .expect("root surrounding whitespace");
        assert_eq!(bare_root_fingerprint, root_whitespace_fingerprint);

        let pi_crlf = b"<movie><?app data='first\r\nsecond'?></movie>";
        let pi_lf = b"<movie><?app data='first\nsecond'?></movie>";
        let pi_other_target = b"<movie><?other data='first\nsecond'?></movie>";
        let (_, pi_crlf_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(pi_crlf)
                .expect("CRLF processing instruction");
        let (_, pi_lf_fingerprint) = parse_local_nfo_projection_with_semantic_fingerprint(pi_lf)
            .expect("LF processing instruction");
        let (_, pi_other_target_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(pi_other_target)
                .expect("different processing-instruction target");
        assert_eq!(pi_crlf_fingerprint, pi_lf_fingerprint);
        assert_ne!(pi_lf_fingerprint, pi_other_target_fingerprint);

        let lf_content = b"<movie><extension>first\nsecond</extension></movie>";
        let crlf_content = b"<movie><extension>first\r\nsecond</extension></movie>";
        let (_, lf_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(lf_content).expect("LF content");
        let (_, crlf_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(crlf_content)
                .expect("CRLF content");
        assert_eq!(lf_fingerprint, crlf_fingerprint);
        let referenced_lf_content = b"<movie><extension>first&#xA;second</extension></movie>";
        let (_, referenced_lf_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(referenced_lf_content)
                .expect("character-reference LF content");
        assert_eq!(lf_fingerprint, referenced_lf_fingerprint);

        let mixed_one_space = b"<movie><extension>left <child/> right</extension></movie>";
        let mixed_two_spaces = b"<movie><extension>left  <child/> right</extension></movie>";
        let (_, mixed_one_space_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(mixed_one_space)
                .expect("one mixed-content space");
        let (_, mixed_two_spaces_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(mixed_two_spaces)
                .expect("two mixed-content spaces");
        assert_ne!(mixed_one_space_fingerprint, mixed_two_spaces_fingerprint);

        let whitespace_cdata = b"<movie><extension><![CDATA[ \n  ]]><child/></extension></movie>";
        let whitespace_text = b"<movie><extension> \n  <child/></extension></movie>";
        let (_, whitespace_cdata_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(whitespace_cdata)
                .expect("CDATA formatting whitespace");
        let (_, whitespace_text_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(whitespace_text)
                .expect("text formatting whitespace");
        assert_eq!(whitespace_cdata_fingerprint, whitespace_text_fingerprint);

        let preserved_cdata =
            b"<movie xml:space=\"preserve\"><extension><![CDATA[ \n  ]]><child/></extension></movie>";
        let preserved_text =
            b"<movie xml:space=\"preserve\"><extension> \n  <child/></extension></movie>";
        let (_, preserved_cdata_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(preserved_cdata)
                .expect("preserved CDATA whitespace");
        let (_, preserved_text_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(preserved_text)
                .expect("preserved text whitespace");
        assert_eq!(preserved_cdata_fingerprint, preserved_text_fingerprint);

        let preserved_one_space =
            b"<movie xml:space=\"preserve\"><extension> <child/></extension></movie>";
        let preserved_two_spaces =
            b"<movie xml:space=\"preserve\"><extension>  <child/></extension></movie>";
        let (_, preserved_one_space_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(preserved_one_space)
                .expect("one preserved space");
        let (_, preserved_two_spaces_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(preserved_two_spaces)
                .expect("two preserved spaces");
        assert_ne!(
            preserved_one_space_fingerprint,
            preserved_two_spaces_fingerprint
        );

        let default_one_space = b"<movie><extension> <child/></extension></movie>";
        let default_two_spaces = b"<movie><extension>  <child/></extension></movie>";
        let (_, default_one_space_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(default_one_space)
                .expect("one element-only formatting space");
        let (_, default_two_spaces_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(default_two_spaces)
                .expect("two element-only formatting spaces");
        assert_eq!(
            default_one_space_fingerprint,
            default_two_spaces_fingerprint
        );
        let reset_preserve = b"<movie xml:space=\"preserve\"><extension xml:space=\"default\"> <child/></extension></movie>";
        let reset_default = b"<movie xml:space=\"preserve\"><extension xml:space=\"default\">  <child/></extension></movie>";
        let (_, reset_preserve_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(reset_preserve)
                .expect("nested xml:space default");
        let (_, reset_default_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(reset_default)
                .expect("nested xml:space default formatting");
        assert_eq!(reset_preserve_fingerprint, reset_default_fingerprint);

        let attribute_lf = b"<movie><extension value=\"line\nbreak\"/></movie>";
        let attribute_crlf = b"<movie><extension value=\"line\r\nbreak\"/></movie>";
        let attribute_char_ref = b"<movie><extension value=\"line&#xA;break\"/></movie>";
        let (_, attribute_lf_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(attribute_lf)
                .expect("literal LF attribute");
        let (_, attribute_crlf_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(attribute_crlf)
                .expect("literal CRLF attribute");
        let (_, attribute_char_ref_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(attribute_char_ref)
                .expect("character-reference LF attribute");
        assert_eq!(attribute_lf_fingerprint, attribute_crlf_fingerprint);
        assert_ne!(attribute_lf_fingerprint, attribute_char_ref_fingerprint);

        let attribute_literal_tab = b"<movie><extension value=\"left\tright\"/></movie>";
        let attribute_char_ref_tab = b"<movie><extension value=\"left&#x9;right\"/></movie>";
        let (_, attribute_literal_tab_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(attribute_literal_tab)
                .expect("literal TAB attribute");
        let (_, attribute_char_ref_tab_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(attribute_char_ref_tab)
                .expect("character-reference TAB attribute");
        assert_ne!(
            attribute_literal_tab_fingerprint,
            attribute_char_ref_tab_fingerprint
        );

        let equivalent_attribute_refs = b"<movie><extension value=\"A&amp;B&#10;C\"/></movie>";
        let equivalent_numeric_attribute_refs =
            b"<movie><extension value=\"A&#38;B&#xA;C\"/></movie>";
        let (_, equivalent_attribute_ref_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(equivalent_attribute_refs)
                .expect("named attribute references");
        let (_, equivalent_numeric_attribute_ref_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(equivalent_numeric_attribute_refs)
                .expect("numeric attribute references");
        assert_eq!(
            equivalent_attribute_ref_fingerprint,
            equivalent_numeric_attribute_ref_fingerprint
        );

        let duplicate_attribute = b"<movie><extension key=\"x\" key=\"y\"/></movie>";
        assert!(matches!(
            parse_local_nfo_projection_with_semantic_fingerprint(duplicate_attribute),
            Err(NfoError::Xml(_))
        ));
        assert!(matches!(
            parse_local_nfo_projection_with_semantic_fingerprint(
                b"<!DOCTYPE movie [<!ENTITY secret 'value'>]><movie><extension>&secret;</extension></movie>"
            ),
            Err(NfoError::DocTypeNotAllowed)
        ));
        let escaped_unknown_name = b"<movie><extension>&amp;missing;</extension></movie>";
        let cdata_unknown_name = b"<movie><extension><![CDATA[&missing;]]></extension></movie>";
        let (_, escaped_unknown_name_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(escaped_unknown_name)
                .expect("escaped literal entity-like text");
        let (_, cdata_unknown_name_fingerprint) =
            parse_local_nfo_projection_with_semantic_fingerprint(cdata_unknown_name)
                .expect("CDATA literal entity-like text");
        assert_eq!(
            escaped_unknown_name_fingerprint,
            cdata_unknown_name_fingerprint
        );
        let undeclared_entity = b"<movie><extension>&missing;</extension></movie>";
        assert!(matches!(
            parse_local_nfo_projection_with_semantic_fingerprint(undeclared_entity),
            Err(NfoError::Xml(_))
        ));
        assert!(matches!(
            parse_local_nfo_projection(undeclared_entity),
            Err(NfoError::Xml(_))
        ));
        assert!(matches!(
            parse_local_nfo_projection_with_semantic_fingerprint(b"<movie><extension></movie>"),
            Err(NfoError::Xml(_))
        ));

        let projection_source = b"<movie><title>  Lead <emphasis>ignored</emphasis> tail  </title><plot>  Intro <b>omitted</b> outro  </plot></movie>";
        let established_projection =
            parse_local_nfo_projection(projection_source).expect("established projection");
        let (semantic_projection, _) =
            parse_local_nfo_projection_with_semantic_fingerprint(projection_source)
                .expect("semantic projection");
        // The cache-enabled path keeps untrimmed XML text events so character
        // references and CDATA map to the same field value. The projection-only
        // path retains its historical per-event trimming behavior.
        assert_eq!(
            established_projection.metadata.title.as_deref(),
            Some("Leadtail")
        );
        assert_eq!(
            semantic_projection.metadata.title.as_deref(),
            Some("Lead  tail")
        );
        assert_eq!(
            established_projection.metadata.overview.as_deref(),
            Some("Introoutro")
        );
        assert_eq!(
            semantic_projection.metadata.overview.as_deref(),
            Some("Intro  outro")
        );

        for changed in [
            br#"<movie><title>Title &amp; text</title><actor sortorder="0" tmdbid="9"><name>Actor</name></actor><extension key="y">preserved</extension><empty/></movie>"#.as_slice(),
            br#"<movie><title>Title &amp; text</title><actor sortorder="0" tmdbid="9"><name>Actor</name></actor><extension key="x">changed</extension><empty/></movie>"#.as_slice(),
            br#"<movie><title>Title &amp; text</title><actor sortorder="0" tmdbid="9"><name>Actor</name></actor><extension key="x">preserved<child/></extension><empty/></movie>"#.as_slice(),
            br#"<movie><title>Changed &amp; text</title><actor sortorder="0" tmdbid="9"><name>Actor</name></actor><extension key="x">preserved</extension><empty/></movie>"#.as_slice(),
        ] {
            let (_, changed_fingerprint) =
                parse_local_nfo_projection_with_semantic_fingerprint(changed)
                    .expect("changed NFO");
            assert_ne!(original_fingerprint, changed_fingerprint);
        }
    }

    #[tokio::test]
    async fn shared_episode_nfo_projection_is_cached_per_page_and_keeps_errors_typed()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let root = directory.path().join("media");
        let show = root.join("Show");
        fs::create_dir_all(&show).await?;
        fs::write(show.join("S01E01.mkv"), b"one").await?;
        fs::write(show.join("S01E02.mkv"), b"two").await?;
        fs::write(
            show.join("episode.nfo"),
            b"<episodedetails><title>Shared episode metadata</title></episodedetails>",
        )
        .await?;
        let other_show = root.join("Other");
        fs::create_dir_all(&other_show).await?;
        fs::write(other_show.join("S01E01.mkv"), b"other").await?;
        fs::write(
            other_show.join("episode.nfo"),
            b"<episodedetails><title>Independent episode metadata</title></episodedetails>",
        )
        .await?;
        let database = Database::connect(&config).await?;
        let service = NfoWriteService::new(database);
        let root_path = root.to_string_lossy().into_owned();
        let request = |item_id: &str, filename: &str| {
            (
                item_id.to_owned(),
                Some(1),
                StoredMediaWritebackContext {
                    item_type: "EPISODE".to_owned(),
                    source: Some(StoredMediaSourcePath {
                        source_id: format!("source-{item_id}"),
                        item_id: item_id.to_owned(),
                        probe_status: "DONE".to_owned(),
                        root_path: root_path.clone(),
                        relative_path: format!("Show/{filename}"),
                    }),
                },
            )
        };

        let (_, season_a, context_a) = request("episode-1", "S01E01.mkv");
        let (_, season_b, context_b) = request("episode-2", "S01E02.mkv");
        let cache = LocalNfoProjectionCache::default();
        let (first, second) = tokio::join!(
            service
                .read_item_projection_with_writeback_context_cached(season_a, &context_a, &cache,),
            service
                .read_item_projection_with_writeback_context_cached(season_b, &context_b, &cache,),
        );
        let (projection_a, loaded_a) = first?;
        let (projection_b, loaded_b) = second?;
        let projection_a = projection_a.ok_or("shared episode sidecar was not found")?;
        let projection_b = projection_b.ok_or("shared episode sidecar was not found")?;
        assert_eq!(
            projection_a.metadata.title.as_deref(),
            Some("Shared episode metadata")
        );
        assert_eq!(
            projection_b.metadata.title.as_deref(),
            Some("Shared episode metadata")
        );
        assert_ne!(loaded_a, loaded_b, "shared sidecar should be loaded once");

        let (_, _, context_other) = request("episode-other", "../Other/S01E01.mkv");
        let (other_projection, loaded_other) = service
            .read_item_projection_with_writeback_context_cached(Some(1), &context_other, &cache)
            .await?;
        assert!(
            loaded_other,
            "a different canonical sidecar gets its own initializer"
        );
        assert_eq!(
            other_projection.and_then(|projection| projection.metadata.title),
            Some("Independent episode metadata".to_owned())
        );

        fs::write(
            show.join("episode.nfo"),
            b"<episodedetails><title>Updated page metadata</title></episodedetails>",
        )
        .await?;
        let next_page_cache = LocalNfoProjectionCache::default();
        let (next_a, next_b) = tokio::join!(
            service.read_item_projection_with_writeback_context_cached(
                season_a,
                &context_a,
                &next_page_cache,
            ),
            service.read_item_projection_with_writeback_context_cached(
                season_b,
                &context_b,
                &next_page_cache,
            ),
        );
        for result in [next_a, next_b] {
            let (projection, _) = result?;
            assert_eq!(
                projection.and_then(|projection| projection.metadata.title),
                Some("Updated page metadata".to_owned())
            );
        }

        fs::write(show.join("episode.nfo"), b"<episodedetails><title>Broken").await?;
        let error_cache = LocalNfoProjectionCache::default();
        let (error_a, error_b) = tokio::join!(
            service.read_item_projection_with_writeback_context_cached(
                season_a,
                &context_a,
                &error_cache,
            ),
            service.read_item_projection_with_writeback_context_cached(
                season_b,
                &context_b,
                &error_cache,
            ),
        );
        assert!(matches!(error_a, Err(NfoWriteError::Nfo(_))));
        assert!(matches!(error_b, Err(NfoWriteError::Nfo(_))));
        Ok(())
    }

    #[tokio::test]
    async fn no_op_nfo_write_reuses_the_file_stamp_fingerprint()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("movie.nfo");
        fs::write(&target, b"<movie><title>Example</title></movie>").await?;

        let write =
            write_nfo_atomically_with_rewriter(&target, |original| Ok(original.to_vec()), None)
                .await?;

        assert!(!write.changed);
        assert_eq!(
            write.file_fingerprint,
            Some(nfo_fingerprint(&target).await?)
        );
        Ok(())
    }

    #[tokio::test]
    async fn nfo_projection_reuses_a_preloaded_writeback_context()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let root = directory.path().join("Movies");
        fs::create_dir_all(&root).await?;
        fs::write(root.join("Example.Movie.2020.mkv"), b"fixture").await?;
        fs::write(
            root.join("Example.Movie.2020.nfo"),
            b"<movie><title>Example Movie</title></movie>",
        )
        .await?;
        let database = Database::connect(&config).await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        LibraryService::new(database.clone())
            .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
            .await?;
        LibraryScanner::new(database.clone())
            .scan_movie_library(library.id)
            .await?;
        let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
            .fetch_one(database.pool())
            .await?;
        let contexts = database
            .list_media_item_writeback_contexts_by_ids(std::slice::from_ref(&item_id))
            .await?;
        let context = contexts.get(&item_id).ok_or("writeback context")?;
        let writer = NfoWriteService::new(database.clone());

        database.reset_query_count();
        let from_context = writer
            .read_item_projection_with_writeback_context(None, context)
            .await?
            .ok_or("NFO projection from context")?;
        assert_eq!(
            from_context.metadata.title.as_deref(),
            Some("Example Movie")
        );
        assert_eq!(database.query_count(), 0);

        database.reset_query_count();
        let from_item = writer
            .read_item_projection(&item_id)
            .await?
            .ok_or("NFO projection from item")?;
        assert_eq!(from_item.details, from_context.details);
        assert_eq!(database.query_count(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn probe_nfo_write_reuses_context_and_preserves_source_guards()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let root = directory.path().join("Movies");
        fs::create_dir_all(&root).await?;
        fs::write(root.join("Example.Movie.2020.mkv"), b"fixture").await?;
        let target = root.join("movie.nfo");
        fs::write(
            &target,
            b"<movie><title>Example</title><custom>keep</custom></movie>",
        )
        .await?;
        let database = Database::connect(&config).await?;
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        libraries
            .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
            .await?;
        LibraryScanner::new(database.clone())
            .scan_movie_library(library.id)
            .await?;
        let (item_id, source_id): (String, String) =
            sqlx::query_as("SELECT item_id, id FROM media_sources LIMIT 1")
                .fetch_one(database.pool())
                .await?;
        let probe = MediaProbeResult {
            container: Some("mkv".to_owned()),
            source_size: Some(100),
            duration_ticks: Some(600_000_000),
            bitrate: Some(500_000),
            streams: vec![],
        };
        sqlx::query("UPDATE media_items SET sort_title = ?, added_at = 0 WHERE id = ?")
            .bind("00 example movie")
            .bind(&item_id)
            .execute(database.pool())
            .await?;
        sqlx::query("UPDATE libraries SET media_strategy_json = ? WHERE id = ?")
            .bind(r#"{"images":{"writeToMetadata":true}}"#)
            .bind(library.id.to_string())
            .execute(database.pool())
            .await?;
        let writer =
            NfoWriteService::new_with_config_dir(database.clone(), config.config_dir.clone());
        database.reset_query_count();
        assert!(
            writer
                .write_item_probe_details(&item_id, &source_id, &probe)
                .await?
        );
        assert_eq!(
            database.query_count(),
            3,
            "item context, mirror policy, and combined state sync"
        );
        let content = fs::read_to_string(&target).await?;
        assert!(content.contains("<custom>keep</custom>"));
        assert!(content.contains("<sorttitle>00 example movie</sorttitle>"));
        assert!(content.contains("<dateadded>1970-01-01 00:00:00</dateadded>"));
        assert!(root.join("movie.nfo").exists());
        let mirror = library_item_directory(&config.config_dir, &item_id)?.join("movie.nfo");
        assert_eq!(fs::read_to_string(&mirror).await?, content);

        database.reset_query_count();
        assert!(
            writer
                .write_item_probe_details(&item_id, &source_id, &probe)
                .await?
        );
        assert_eq!(
            database.query_count(),
            1,
            "an unchanged NFO skips mirror policy lookup and state sync"
        );
        assert_eq!(fs::read_to_string(&target).await?, content);
        assert_eq!(fs::read_to_string(&mirror).await?, content);

        database.reset_query_count();
        writer
            .write_item_movie_nfo(
                &item_id,
                &MovieNfoMetadata {
                    base: NfoMetadata {
                        title: Some("Example with details".to_owned()),
                        ..NfoMetadata::default()
                    },
                    ..MovieNfoMetadata::default()
                },
            )
            .await?;
        assert_eq!(
            database.query_count(),
            3,
            "movie item context, mirror policy, and combined state sync"
        );
        let movie_content = fs::read_to_string(&target).await?;
        assert!(
            movie_content.contains("<sorttitle>00 example movie</sorttitle>"),
            "{movie_content}"
        );
        assert!(
            movie_content.contains("<dateadded>1970-01-01 00:00:00</dateadded>"),
            "{movie_content}"
        );

        database.reset_query_count();
        assert!(
            !writer
                .write_item_probe_details(&item_id, "wrong-source", &probe)
                .await?
        );
        assert_eq!(database.query_count(), 1);
        assert_eq!(fs::read_to_string(&target).await?, movie_content);

        sqlx::query("UPDATE media_items SET item_type = 'VIDEO' WHERE id = ?")
            .bind(&item_id)
            .execute(database.pool())
            .await?;
        assert!(
            !writer
                .write_item_probe_details(&item_id, &source_id, &probe)
                .await?
        );
        assert!(
            !writer
                .write_item_probe_details("missing-item", &source_id, &probe)
                .await?
        );
        writer
            .write_item_movie_nfo(
                &item_id,
                &MovieNfoMetadata {
                    base: NfoMetadata {
                        title: Some("Video item".to_owned()),
                        ..NfoMetadata::default()
                    },
                    ..MovieNfoMetadata::default()
                },
            )
            .await?;
        let video_nfo = fs::read_to_string(root.join("Example.Movie.2020.nfo")).await?;
        assert!(!video_nfo.contains("<sorttitle>"), "{video_nfo}");
        assert!(!video_nfo.contains("<dateadded>"), "{video_nfo}");

        sqlx::query("UPDATE media_items SET item_type = 'MOVIE', removed_at = 1 WHERE id = ?")
            .bind(&item_id)
            .execute(database.pool())
            .await?;
        fs::remove_file(&target).await?;
        fs::remove_file(root.join("Example.Movie.2020.nfo")).await?;
        writer
            .write_item_movie_nfo(
                &item_id,
                &MovieNfoMetadata {
                    base: NfoMetadata {
                        title: Some("Removed movie".to_owned()),
                        ..NfoMetadata::default()
                    },
                    ..MovieNfoMetadata::default()
                },
            )
            .await?;
        let removed_movie_nfo = fs::read_to_string(&target).await?;
        assert!(
            !removed_movie_nfo.contains("<sorttitle>"),
            "{removed_movie_nfo}"
        );
        assert!(
            !removed_movie_nfo.contains("<dateadded>"),
            "{removed_movie_nfo}"
        );
        fs::write(&target, &movie_content).await?;
        fs::write(&mirror, &movie_content).await?;

        sqlx::query("UPDATE media_items SET removed_at = NULL WHERE id = ?")
            .bind(&item_id)
            .execute(database.pool())
            .await?;
        sqlx::query("UPDATE filesystem_entries SET relative_path = 'Example.strm' WHERE id = (SELECT filesystem_entry_id FROM media_sources WHERE id = ?)")
            .bind(&source_id).execute(database.pool()).await?;
        assert!(
            !writer
                .write_item_probe_details(&item_id, &source_id, &probe)
                .await?
        );
        sqlx::query("DELETE FROM media_sources WHERE id = ?")
            .bind(&source_id)
            .execute(database.pool())
            .await?;
        assert!(
            !writer
                .write_item_probe_details(&item_id, &source_id, &probe)
                .await?
        );
        assert_eq!(fs::read_to_string(&target).await?, movie_content);
        Ok(())
    }

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
