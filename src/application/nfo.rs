use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::Cursor,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use quick_xml::{
    Writer,
    escape::{escape, unescape},
    events::{BytesEnd, BytesStart, BytesText, Event},
    reader::Reader,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
};
use uuid::Uuid;

use crate::application::metadata::{
    MetadataField, MetadataSource, MetadataState, NfoError, NfoMetadata, find_nfo_path,
    nfo_fingerprint, parse_nfo, series_directory,
};
use crate::application::people::ActorCredit;
use crate::storage::{Database, MediaMetadataUpdate, StorageError};

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
const MAX_MOVIE_NFO_ACTORS: usize = 30;
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
    if bytes.len() > MAX_LOCAL_NFO_BYTES {
        return Err(NfoError::TooLarge);
    }

    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut projection = LocalNfoProjection::default();
    let mut active_direct = None;
    let mut actor_depth = None;
    let mut active_actor = None;
    let mut current_actor = None;
    let mut depth = 0_usize;
    let mut event_count = 0_usize;

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
                append_projection_text(
                    depth,
                    actor_depth,
                    active_direct.as_mut(),
                    active_actor.as_mut(),
                    value.as_ref(),
                )?;
            }
            Ok(Event::End(event)) => {
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
            Ok(Event::Empty(_)) => {}
            Ok(Event::DocType(_)) => return Err(NfoError::DocTypeNotAllowed),
            Ok(_) => {}
            Err(error) => return Err(NfoError::Xml(error.to_string())),
        }
        buffer.clear();
    }

    Ok(projection)
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
    order: Option<i32>,
}

#[derive(Clone, Copy)]
enum MovieActorField {
    Name,
    Role,
    TmdbId,
    Order,
}

fn movie_actor_field(tag: &[u8]) -> Option<MovieActorField> {
    match tag {
        b"name" => Some(MovieActorField::Name),
        b"role" | b"character" => Some(MovieActorField::Role),
        b"tmdbid" => Some(MovieActorField::TmdbId),
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
        MovieActorField::TmdbId => actor.tmdb_id = Some(value),
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
    let Some(id) = actor.tmdb_id.map(|value| value.trim().to_owned()) else {
        return;
    };
    let Some(name) = actor.name.map(|value| value.trim().to_owned()) else {
        return;
    };
    if id.is_empty() || name.is_empty() || actors.len() >= MAX_MOVIE_NFO_ACTORS {
        return;
    }
    actors.push(ActorCredit {
        id,
        name,
        character: actor.role,
        order: actor.order,
        profile_url: None,
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
        match serde_json::from_str(&json) {
            Ok(details) => Ok(Some(details)),
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
        let state = self.state(item_id).await?;
        if !state.has_snapshot
            || !state
                .source_fingerprint
                .as_deref()
                .is_some_and(valid_nfo_content_fingerprint)
        {
            return Ok(false);
        }
        Ok(self.read_item(item_id).await?.is_some())
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

pub fn rewrite_movie_nfo(
    original: &[u8],
    patch: &MovieNfoMetadata,
) -> Result<Vec<u8>, NfoWriteError> {
    let base = rewrite_nfo(original, &patch.base)?;
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
                if depth == 2 && replace_rich_root_tag(event.name().as_ref(), patch) {
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
                    writer
                        .write_event(Event::Start(event.to_owned()))
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                    append_movie_nfo_fields(&mut writer, patch)?;
                    appended = true;
                    writer
                        .write_event(Event::End(BytesEnd::new(
                            String::from_utf8_lossy(event.name().as_ref()).as_ref(),
                        )))
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                } else if depth == 1 && replace_rich_root_tag(event.name().as_ref(), patch) {
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
    Ok(writer.into_inner())
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
            | b"runtime"
            | b"status"
            | b"language"
            | b"website"
            | b"set"
            | b"setid"
            | b"thumb"
            | b"fanart"
            | b"uniqueid"
            | b"tmdbid"
            | b"imdbid"
            | b"tvdbid"
            | b"wikidataid"
    )
}

fn replace_rich_root_tag(tag: &[u8], patch: &MovieNfoMetadata) -> bool {
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
        b"uniqueid" | b"tmdbid" | b"imdbid" | b"tvdbid" | b"wikidataid" => {
            provider_id_is_present(tag, patch)
        }
        _ => false,
    }
}

fn provider_id_is_present(tag: &[u8], patch: &MovieNfoMetadata) -> bool {
    let provider = match tag {
        b"tmdbid" => "tmdb",
        b"imdbid" => "imdb",
        b"tvdbid" => "tvdb",
        b"wikidataid" => "wikidata",
        b"uniqueid" => {
            return patch.provider_ids.iter().any(|(name, value)| {
                !value.trim().is_empty()
                    && matches!(
                        name.to_ascii_lowercase().as_str(),
                        "tmdb" | "imdb" | "tvdb" | "wikidata"
                    )
            });
        }
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
    for actor in &patch.actors {
        if actor.id.trim().is_empty() || actor.name.trim().is_empty() {
            continue;
        }
        start_element(writer, "actor", None)?;
        write_simple_element(writer, "name", &actor.name)?;
        if let Some(character) = non_empty(actor.character.as_deref()) {
            write_simple_element(writer, "role", character)?;
        }
        write_simple_element(writer, "type", "Actor")?;
        write_simple_element(writer, "tmdbid", &actor.id)?;
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
        let mut uniqueid = BytesStart::new("uniqueid");
        uniqueid.push_attribute(("type", provider.as_str()));
        if provider == "tmdb" {
            uniqueid.push_attribute(("default", "true"));
        }
        writer
            .write_event(Event::Start(uniqueid))
            .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
        write_text(writer, id)?;
        end_element(writer, "uniqueid")?;
        write_simple_element(writer, tag, id)?;
    }
    Ok(())
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
        let target = self.item_nfo_target(item_id).await?;
        let write = write_nfo_atomically_with_hook(&target, patch, None).await?;
        self.finish_item_write(item_id, target, write).await
    }

    pub async fn write_item_movie_nfo(
        &self,
        item_id: &str,
        patch: &MovieNfoMetadata,
    ) -> Result<NfoWriteReport, NfoWriteError> {
        let target = self.item_nfo_target(item_id).await?;
        let write = write_nfo_atomically_with_rewriter(
            &target,
            |original| rewrite_movie_nfo(original, patch),
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
        let fingerprint = nfo_fingerprint(&target)
            .await
            .map_err(|error| io_error(&target, error))?;
        self.database
            .invalidate_media_item_nfo_metadata_if_source_changed(
                item_id,
                &write.content_fingerprint,
            )
            .await?;
        self.database
            .mark_media_item_metadata_checked(item_id, &fingerprint)
            .await?;
        Ok(NfoWriteReport {
            path: target,
            fingerprint,
            content_fingerprint: write.content_fingerprint,
            changed: write.changed,
        })
    }

    async fn item_nfo_target(&self, item_id: &str) -> Result<PathBuf, NfoWriteError> {
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
    pub content_fingerprint: Vec<u8>,
    pub changed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NfoFileWrite {
    content_fingerprint: Vec<u8>,
    changed: bool,
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
    let original = match fs::read(target).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(source) => return Err(io_error(target, source)),
    };
    let rewritten = rewrite(&original)?;
    let write = NfoFileWrite {
        content_fingerprint: nfo_content_fingerprint(&rewritten),
        changed: rewritten != original,
    };
    if !write.changed {
        return Ok(write);
    }
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
    result.map(|_| write)
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
