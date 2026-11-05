//! Rating-only NFO writes.
//!
//! Rating sync (for example a scheduled Trakt refresh) must not rewrite the whole sidecar NFO the
//! way [`super::rewrite_movie_nfo`] does: that writer rebuilds many managed fields and, by design,
//! leaves the Emby/Kodi `<ratings>` container and `<criticrating>` alone. This module keeps a
//! much narrower contract:
//!
//! - only the direct children `rating`, `votes`, `criticrating` and `ratings` of the root element
//!   are replaced, so comments, unknown tags, cast lists, indentation and sibling order survive
//!   byte for byte;
//! - `<actor>` and every other nested element is never touched, even when it contains a `<rating>`
//!   of its own, and `<userrating>` is left alone;
//! - a Kodi multi-episode file with several `<episodedetails>` roots is only rewritten when the
//!   caller's season/episode pair selects exactly one block, otherwise the write is reported as
//!   ambiguous instead of guessing;
//! - writing never creates a new NFO. A synthesized rating-only sidecar would be picked up by the
//!   next scan as local metadata and could change how the item is identified.

use std::path::PathBuf;

use quick_xml::{
    Writer,
    escape::{escape, unescape},
    events::{BytesEnd, BytesStart, Event},
    reader::Reader,
};

use super::{
    MAX_MOVIE_NFO_DETAILS_ID_BYTES, MAX_NFO_RATING_ENTRIES, NfoWriteError, NfoWriteService,
    is_valid_xml_character, read_nfo_bounded, write_nfo_atomically_with_rewriter,
};

/// Highest score Lux stores; every rating is normalized to the 0-10 scale on read.
const MAX_RATING_SCORE: f64 = 10.0;
/// Upper bound for the `max` attribute of one container entry.
const MAX_RATING_SCALE: u32 = 1_000;
/// Root elements that may carry rating fields.
const RATING_ROOT_TAGS: [&str; 3] = ["movie", "tvshow", "episodedetails"];
/// Fallback indentation when the existing document does not indent its root children.
const DEFAULT_CHILD_INDENT: &str = "  ";

/// One `<rating>` entry inside an Emby/Jellyfin/Kodi `<ratings>` container.
#[derive(Clone, Debug, PartialEq)]
pub struct NfoRatingEntry {
    /// Provider name written to the `name` attribute, for example `trakt`.
    pub name: String,
    /// Score on the `max` scale.
    pub value: f64,
    /// Vote count written as the nested `<votes>` element; omitted when `None`.
    pub votes: Option<i64>,
    /// Scale the score is expressed in, written to the `max` attribute.
    pub max: u32,
    /// Marks the entry Jellyfin treats as authoritative (`default="true"`).
    pub is_default: bool,
}

/// Everything a rating-only write replaces.
///
/// `entries` is written exactly in the order the caller supplies. Order matters because Jellyfin
/// keeps the last non-`tomato` entry it reads, so callers put the primary source last.
#[derive(Clone, Debug, PartialEq)]
pub struct RatingsNfoPatch {
    /// `<ratings>` container entries, in caller order.
    pub entries: Vec<NfoRatingEntry>,
    /// Flat `<rating>` value (0-10); mirrors the primary source.
    pub rating: f64,
    /// Flat `<votes>` value; omitted when `None`.
    pub votes: Option<i64>,
    /// Flat `<criticrating>` value (0-10); omitted when `None`.
    pub critic_rating: Option<f64>,
}

/// What [`NfoWriteService::write_item_ratings`] did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RatingsWriteOutcome {
    /// The NFO was rewritten atomically.
    Written { path: PathBuf },
    /// The NFO already carried exactly this content (§ idempotent re-run), so nothing was written.
    Unchanged,
    /// The item has no sidecar NFO, a root of another kind, or a kind that has no own NFO.
    SkippedNoNfo,
    /// A multi-episode file could not be pinned to exactly one `<episodedetails>` block.
    SkippedAmbiguous,
}

/// Result of the pure rewrite step, before any filesystem work happens.
#[derive(Debug)]
pub(super) enum RatingsRewrite {
    Rewritten(Vec<u8>),
    Unchanged,
    NoNfo,
    Ambiguous,
}

/// Season/episode pair used to pick one `<episodedetails>` block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EpisodeNumbers {
    pub(super) season_number: Option<i64>,
    pub(super) episode_number: Option<i64>,
}

/// Replaces the rating fields of `original` and returns the new document bytes.
///
/// `root_tag` selects which root element kind is expected; a document whose root is a different
/// kind is reported as [`RatingsRewrite::NoNfo`] so the caller never rewrites an unrelated sidecar.
pub(super) fn rewrite_ratings_nfo(
    original: &[u8],
    patch: &RatingsNfoPatch,
    root_tag: &str,
    episode: Option<EpisodeNumbers>,
) -> Result<RatingsRewrite, NfoWriteError> {
    validate_ratings_patch(patch)?;
    if !RATING_ROOT_TAGS.contains(&root_tag) {
        return Err(NfoWriteError::InvalidMetadata(format!(
            "unsupported NFO root tag for a rating write: {root_tag}"
        )));
    }
    let blocks = scan_root_blocks(original)?;
    let target = match select_rating_block(&blocks, root_tag, episode) {
        BlockSelection::Target(index) => index,
        BlockSelection::NoNfo => return Ok(RatingsRewrite::NoNfo),
        BlockSelection::Ambiguous => return Ok(RatingsRewrite::Ambiguous),
    };
    let rewritten = rewrite_selected_block(original, patch, target)?;
    if rewritten == original {
        Ok(RatingsRewrite::Unchanged)
    } else {
        Ok(RatingsRewrite::Rewritten(rewritten))
    }
}

fn validate_ratings_patch(patch: &RatingsNfoPatch) -> Result<(), NfoWriteError> {
    if patch.entries.len() > MAX_NFO_RATING_ENTRIES {
        return Err(invalid_metadata(format!(
            "rating write carries more than {MAX_NFO_RATING_ENTRIES} entries"
        )));
    }
    if !valid_score(patch.rating, MAX_RATING_SCORE) {
        return Err(invalid_metadata(
            "rating write primary score is outside 0-10".to_owned(),
        ));
    }
    if let Some(votes) = patch.votes
        && votes < 0
    {
        return Err(invalid_metadata(
            "rating write vote count is negative".to_owned(),
        ));
    }
    if let Some(critic_rating) = patch.critic_rating
        && !valid_score(critic_rating, MAX_RATING_SCORE)
    {
        return Err(invalid_metadata(
            "rating write critic score is outside 0-10".to_owned(),
        ));
    }
    for entry in &patch.entries {
        let name = entry.name.trim();
        if name.is_empty()
            || name.len() > MAX_MOVIE_NFO_DETAILS_ID_BYTES
            || !name.chars().all(is_valid_xml_character)
        {
            return Err(invalid_metadata(
                "rating entry name is empty, too long, or not valid XML text".to_owned(),
            ));
        }
        if entry.max == 0 || entry.max > MAX_RATING_SCALE {
            return Err(invalid_metadata(format!(
                "rating entry scale is outside 1-{MAX_RATING_SCALE}"
            )));
        }
        if !valid_score(entry.value, f64::from(entry.max)) {
            return Err(invalid_metadata(
                "rating entry score is outside its declared scale".to_owned(),
            ));
        }
        if let Some(votes) = entry.votes
            && votes < 0
        {
            return Err(invalid_metadata(
                "rating entry vote count is negative".to_owned(),
            ));
        }
    }
    Ok(())
}

fn valid_score(value: f64, scale: f64) -> bool {
    value.is_finite() && value >= 0.0 && value <= scale
}

fn invalid_metadata(message: String) -> NfoWriteError {
    NfoWriteError::InvalidMetadata(message)
}

/// Renders one score for NFO text: at most three decimals, always at least one.
///
/// `6.564` stays `6.564`, `8.7` stays `8.7` and `7.0` never degrades into `7` or `7.000000001`.
fn format_rating_score(value: f64) -> String {
    let rounded = (value * 1000.0).round() / 1000.0;
    let mut text = format!("{rounded:.3}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.push('0');
    }
    text
}

/// One root element of the document, with the season/episode pair a Kodi multi-episode file uses
/// to tell its blocks apart.
#[derive(Clone, Debug)]
struct NfoRootBlock {
    tag: String,
    season: Option<i64>,
    episode: Option<i64>,
}

impl NfoRootBlock {
    /// True when the block declares a season/episode the selector contradicts.
    fn conflicts(&self, selector: EpisodeNumbers) -> bool {
        selector
            .season_number
            .is_some_and(|season| self.season.is_some_and(|declared| declared != season))
            || selector
                .episode_number
                .is_some_and(|episode| self.episode.is_some_and(|declared| declared != episode))
    }

    /// True when the block declares exactly the selected season/episode pair.
    fn matches(&self, selector: EpisodeNumbers) -> bool {
        selector
            .season_number
            .is_none_or(|season| self.season == Some(season))
            && selector
                .episode_number
                .is_none_or(|episode| self.episode == Some(episode))
    }
}

#[derive(Clone, Copy)]
enum EpisodeField {
    Season,
    Episode,
}

fn episode_field(tag: &[u8]) -> Option<EpisodeField> {
    match tag {
        b"season" | b"seasonnumber" => Some(EpisodeField::Season),
        b"episode" | b"episodenumber" => Some(EpisodeField::Episode),
        _ => None,
    }
}

fn root_name(event: &BytesStart<'_>) -> Result<String, NfoWriteError> {
    String::from_utf8(event.name().as_ref().to_vec())
        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))
}

/// Collects every top-level element and the season/episode numbers it declares.
fn scan_root_blocks(bytes: &[u8]) -> Result<Vec<NfoRootBlock>, NfoWriteError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut blocks: Vec<NfoRootBlock> = Vec::new();
    let mut active_field: Option<EpisodeField> = None;
    let mut text = String::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => {
                if depth != 0 {
                    return Err(NfoWriteError::InvalidXml(
                        "NFO document does not contain a complete root element".to_owned(),
                    ));
                }
                break;
            }
            Ok(Event::Start(event)) => {
                if depth == 0 {
                    blocks.push(NfoRootBlock {
                        tag: root_name(&event)?,
                        season: None,
                        episode: None,
                    });
                } else if depth == 1 {
                    active_field = episode_field(event.name().as_ref());
                    text.clear();
                }
                depth += 1;
            }
            Ok(Event::Empty(event)) => {
                if depth == 0 {
                    blocks.push(NfoRootBlock {
                        tag: root_name(&event)?,
                        season: None,
                        episode: None,
                    });
                }
            }
            Ok(Event::Text(event)) => {
                if depth == 2 && active_field.is_some() {
                    let decoded = event
                        .decode()
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                    let value = unescape(decoded.as_ref())
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                    text.push_str(value.as_ref());
                }
            }
            Ok(Event::End(_)) => {
                if depth == 0 {
                    return Err(NfoWriteError::InvalidXml(
                        "NFO document contains an unbalanced end tag".to_owned(),
                    ));
                }
                if depth == 2
                    && let Some(field) = active_field.take()
                    && let Some(block) = blocks.last_mut()
                {
                    let number = text.trim().parse::<i64>().ok();
                    match field {
                        EpisodeField::Season => block.season = number,
                        EpisodeField::Episode => block.episode = number,
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::DocType(_)) => {
                return Err(NfoWriteError::InvalidXml(
                    "NFO document type declarations are not supported".to_owned(),
                ));
            }
            Ok(_) => {}
            Err(error) => return Err(NfoWriteError::InvalidXml(error.to_string())),
        }
        buffer.clear();
    }
    Ok(blocks)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockSelection {
    Target(usize),
    NoNfo,
    Ambiguous,
}

/// Picks the one root element a rating write may touch.
///
/// A single matching block is written as long as it does not declare a different episode; a file
/// with several `<episodedetails>` roots (a Kodi multi-episode file) needs an exact, unique
/// season/episode match, because writing a rating into the wrong block would silently mislabel the
/// other episode.
fn select_rating_block(
    blocks: &[NfoRootBlock],
    root_tag: &str,
    episode: Option<EpisodeNumbers>,
) -> BlockSelection {
    let candidates: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block.tag == root_tag)
        .map(|(index, _)| index)
        .collect();
    let Some((first, rest)) = candidates.split_first() else {
        return BlockSelection::NoNfo;
    };
    let first = *first;
    if rest.is_empty() {
        return match episode {
            Some(selector) if blocks[first].conflicts(selector) => BlockSelection::Ambiguous,
            _ => BlockSelection::Target(first),
        };
    }
    let Some(selector) = episode else {
        return BlockSelection::Ambiguous;
    };
    let mut matched = std::iter::once(first)
        .chain(rest.iter().copied())
        .filter(|index| blocks[*index].matches(selector));
    match (matched.next(), matched.next()) {
        (Some(index), None) => BlockSelection::Target(index),
        _ => BlockSelection::Ambiguous,
    }
}

fn is_rating_child(tag: &[u8]) -> bool {
    matches!(tag, b"rating" | b"votes" | b"criticrating" | b"ratings")
}

fn is_whitespace_bytes(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
}

/// Streams `original` back out with the managed rating children of one root element replaced.
///
/// Whitespace between root children is buffered so a deleted element also removes the indentation
/// in front of it; that is what keeps a repeated write byte-identical instead of appending blank
/// lines or stacking indentation every time.
fn rewrite_selected_block(
    original: &[u8],
    patch: &RatingsNfoPatch,
    target_block: usize,
) -> Result<Vec<u8>, NfoWriteError> {
    let mut reader = Reader::from_reader(original);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut block_index = 0_usize;
    let mut patching = false;
    let mut skip_depth: Option<usize> = None;
    let mut pending_whitespace: Option<Vec<u8>> = None;
    let mut child_indent = String::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => {
                if depth != 0 {
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
                    patching = block_index == target_block;
                    block_index += 1;
                } else if patching && depth == 1 && is_rating_child(event.name().as_ref()) {
                    pending_whitespace = None;
                    depth += 1;
                    skip_depth = Some(depth);
                    buffer.clear();
                    continue;
                }
                flush_whitespace(&mut writer, &mut pending_whitespace, &mut child_indent);
                writer
                    .write_event(Event::Start(event.to_owned()))
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                depth += 1;
            }
            Ok(Event::Empty(event)) => {
                if skip_depth.is_some() {
                    buffer.clear();
                    continue;
                }
                if depth == 0 {
                    // A self-closing root (`<movie />`) is expanded so the rating elements fit.
                    let chosen = block_index == target_block;
                    block_index += 1;
                    patching = false;
                    if chosen {
                        // Rebuild the start tag instead of reusing the `Empty` buffer: it still
                        // contains the whitespace in front of `/>`, which would leak into the
                        // reopened element as `<movie >`.
                        let name = root_name(&event)?;
                        let mut start = BytesStart::new(name.as_str());
                        for attribute in event.attributes().with_checks(false) {
                            let attribute = attribute
                                .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                            start.push_attribute(attribute);
                        }
                        writer
                            .write_event(Event::Start(start))
                            .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                        append_rating_elements(&mut writer, patch, &child_indent);
                        writer
                            .write_event(Event::End(BytesEnd::new(name)))
                            .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                    } else {
                        writer
                            .write_event(Event::Empty(event.to_owned()))
                            .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                    }
                } else if patching && depth == 1 && is_rating_child(event.name().as_ref()) {
                    pending_whitespace = None;
                } else {
                    flush_whitespace(&mut writer, &mut pending_whitespace, &mut child_indent);
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
                if depth == 1 && patching {
                    pending_whitespace = None;
                    append_rating_elements(&mut writer, patch, &child_indent);
                    patching = false;
                } else if depth == 1 {
                    flush_whitespace(&mut writer, &mut pending_whitespace, &mut child_indent);
                }
                writer
                    .write_event(Event::End(event.to_owned()))
                    .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Text(event)) => {
                if skip_depth.is_some() {
                    buffer.clear();
                    continue;
                }
                if depth == 1 && is_whitespace_bytes(&event) {
                    pending_whitespace
                        .get_or_insert_with(Vec::new)
                        .extend_from_slice(&event);
                } else {
                    flush_whitespace(&mut writer, &mut pending_whitespace, &mut child_indent);
                    writer
                        .write_event(Event::Text(event.to_owned()))
                        .map_err(|error| NfoWriteError::InvalidXml(error.to_string()))?;
                }
            }
            Ok(Event::DocType(_)) => {
                return Err(NfoWriteError::InvalidXml(
                    "NFO document type declarations are not supported".to_owned(),
                ));
            }
            Ok(event) => {
                if skip_depth.is_some() {
                    buffer.clear();
                    continue;
                }
                flush_whitespace(&mut writer, &mut pending_whitespace, &mut child_indent);
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

/// Writes buffered inter-child whitespace and remembers the sibling indentation it implies.
fn flush_whitespace(
    writer: &mut Writer<Vec<u8>>,
    pending: &mut Option<Vec<u8>>,
    indent: &mut String,
) {
    let Some(bytes) = pending.take() else {
        return;
    };
    if let Some(position) = bytes.iter().rposition(|byte| *byte == b'\n') {
        let tail = &bytes[position + 1..];
        if !tail.is_empty() {
            *indent = String::from_utf8_lossy(tail).into_owned();
        }
    }
    // The writer has no indentation state here, so the buffered bytes go out exactly as read.
    writer.get_mut().extend_from_slice(&bytes);
}

/// Renders the replacement children and appends them to the document buffer.
///
/// The order is fixed: the `<ratings>` container first, then the flat `<rating>`, `<votes>` and
/// `<criticrating>`. Jellyfin reads the last non-`tomato` container entry into its single
/// community rating, so callers pass their primary source last and this function never reorders.
fn append_rating_elements(writer: &mut Writer<Vec<u8>>, patch: &RatingsNfoPatch, indent: &str) {
    let indent = if indent.is_empty() {
        DEFAULT_CHILD_INDENT
    } else {
        indent
    };
    let mut fragment = String::new();
    if !patch.entries.is_empty() {
        fragment.push('\n');
        fragment.push_str(indent);
        fragment.push_str("<ratings>");
        for entry in &patch.entries {
            fragment.push('\n');
            fragment.push_str(indent);
            fragment.push_str(indent);
            fragment.push_str("<rating name=\"");
            fragment.push_str(escape(&entry.name).as_ref());
            fragment.push_str("\" max=\"");
            fragment.push_str(entry.max.to_string().as_str());
            fragment.push('"');
            if entry.is_default {
                fragment.push_str(" default=\"true\"");
            }
            fragment.push_str("><value>");
            fragment.push_str(escape(format_rating_score(entry.value)).as_ref());
            fragment.push_str("</value>");
            if let Some(votes) = entry.votes {
                fragment.push_str("<votes>");
                fragment.push_str(votes.to_string().as_str());
                fragment.push_str("</votes>");
            }
            fragment.push_str("</rating>");
        }
        fragment.push('\n');
        fragment.push_str(indent);
        fragment.push_str("</ratings>");
    }
    fragment.push('\n');
    fragment.push_str(indent);
    fragment.push_str("<rating>");
    fragment.push_str(escape(format_rating_score(patch.rating)).as_ref());
    fragment.push_str("</rating>");
    if let Some(votes) = patch.votes {
        fragment.push('\n');
        fragment.push_str(indent);
        fragment.push_str("<votes>");
        fragment.push_str(votes.to_string().as_str());
        fragment.push_str("</votes>");
    }
    if let Some(critic_rating) = patch.critic_rating {
        fragment.push('\n');
        fragment.push_str(indent);
        fragment.push_str("<criticrating>");
        fragment.push_str(escape(format_rating_score(critic_rating)).as_ref());
        fragment.push_str("</criticrating>");
    }
    fragment.push('\n');
    writer.get_mut().extend_from_slice(fragment.as_bytes());
}

impl NfoWriteService {
    /// Replaces only the rating fields of an existing sidecar NFO for `item_id`.
    ///
    /// Movies, series and episodes are supported; a season or home video has no rating-bearing NFO
    /// of its own and reports [`RatingsWriteOutcome::SkippedNoNfo`]. The write never creates a new
    /// NFO, keeps the existing atomic write (symlink refusal, concurrent-modification detection,
    /// internal-write marker) and refreshes the stored NFO fingerprint exactly like the other
    /// writers do, so the scanner does not treat Lux's own write as an external change.
    pub async fn write_item_ratings(
        &self,
        item_id: &str,
        patch: &RatingsNfoPatch,
    ) -> Result<RatingsWriteOutcome, NfoWriteError> {
        validate_ratings_patch(patch)?;
        let Some((item_type, season_number, _sort_title, _added_at, source)) = self
            .database
            .find_movie_nfo_writeback_context(item_id)
            .await?
        else {
            return Err(NfoWriteError::ItemNotFound);
        };
        let root_tag = match item_type.as_str() {
            "MOVIE" => "movie",
            "SERIES" => "tvshow",
            "EPISODE" => "episodedetails",
            _ => return Ok(RatingsWriteOutcome::SkippedNoNfo),
        };
        let Some(source) = source else {
            return Ok(RatingsWriteOutcome::SkippedNoNfo);
        };
        let episode = if item_type == "EPISODE" {
            let (season_number, episode_number) = self
                .database
                .find_media_item_season_episode_numbers(item_id)
                .await?
                .ok_or(NfoWriteError::ItemNotFound)?;
            Some(EpisodeNumbers {
                season_number,
                episode_number,
            })
        } else {
            None
        };
        let target = self
            .item_nfo_target_from_source(&item_type, season_number, &source)
            .await?;
        let Some(original) = read_nfo_bounded(&target).await? else {
            return Ok(RatingsWriteOutcome::SkippedNoNfo);
        };
        match rewrite_ratings_nfo(&original, patch, root_tag, episode)? {
            RatingsRewrite::NoNfo => return Ok(RatingsWriteOutcome::SkippedNoNfo),
            RatingsRewrite::Ambiguous => return Ok(RatingsWriteOutcome::SkippedAmbiguous),
            RatingsRewrite::Unchanged => return Ok(RatingsWriteOutcome::Unchanged),
            RatingsRewrite::Rewritten(_) => {}
        }
        let write = write_nfo_atomically_with_rewriter(
            &target,
            |current| {
                match rewrite_ratings_nfo(current, patch, root_tag, episode)? {
                    RatingsRewrite::Rewritten(bytes) => Ok(bytes),
                    // The file changed between planning and the atomic replace. Leave it alone
                    // instead of writing ratings computed from content that no longer exists.
                    RatingsRewrite::Unchanged
                    | RatingsRewrite::NoNfo
                    | RatingsRewrite::Ambiguous => Ok(current.to_vec()),
                }
            },
            None,
        )
        .await?;
        let report = self.finish_item_write(item_id, target, write).await?;
        if report.changed {
            Ok(RatingsWriteOutcome::Written { path: report.path })
        } else {
            Ok(RatingsWriteOutcome::Unchanged)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tokio::fs;

    use super::*;
    use crate::{
        application::{
            libraries::LibraryService, nfo::parse_local_nfo_projection, scanner::LibraryScanner,
        },
        config::Config,
        library::LibraryKind,
        storage::Database,
    };

    fn entry(
        name: &str,
        value: f64,
        votes: Option<i64>,
        max: u32,
        is_default: bool,
    ) -> NfoRatingEntry {
        NfoRatingEntry {
            name: name.to_owned(),
            value,
            votes,
            max,
            is_default,
        }
    }

    fn sample_patch() -> RatingsNfoPatch {
        RatingsNfoPatch {
            entries: vec![
                entry("tmdb", 7.4, Some(1_200), 10, false),
                entry("trakt", 6.564, Some(280), 10, true),
            ],
            rating: 6.564,
            votes: Some(280),
            critic_rating: Some(7.2),
        }
    }

    fn rewrite_document(original: &str, patch: &RatingsNfoPatch, root_tag: &str) -> RatingsRewrite {
        rewrite_ratings_nfo(original.as_bytes(), patch, root_tag, None).expect("rewrite")
    }

    fn rewritten_document(original: &str, patch: &RatingsNfoPatch, root_tag: &str) -> String {
        match rewrite_document(original, patch, root_tag) {
            RatingsRewrite::Rewritten(bytes) => String::from_utf8(bytes).expect("utf8"),
            other => panic!("expected a rewrite, got {other:?}"),
        }
    }

    const MOVIE_WITH_EXTRAS: &str = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n",
        "<movie>\n",
        "  <!-- hand written note -->\n",
        "  <title>Example</title>\n",
        "  <actor>\n",
        "    <name>Someone</name>\n",
        "    <rating>9.1</rating>\n",
        "    <votes>12</votes>\n",
        "  </actor>\n",
        "  <unknown attr=\"1\">keep &amp; this</unknown>\n",
        "  <userrating>8</userrating>\n",
        "</movie>\n",
    );

    const APPENDED_RATING_ELEMENTS: &str = concat!(
        "  <ratings>\n",
        "    <rating name=\"tmdb\" max=\"10\"><value>7.4</value><votes>1200</votes></rating>\n",
        "    <rating name=\"trakt\" max=\"10\" default=\"true\"><value>6.564</value>",
        "<votes>280</votes></rating>\n",
        "  </ratings>\n",
        "  <rating>6.564</rating>\n",
        "  <votes>280</votes>\n",
        "  <criticrating>7.2</criticrating>\n",
    );

    #[test]
    fn rewrite_preserves_comments_unknown_tags_nested_ratings_and_indentation() {
        let expected = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <movie>\n\
             \x20 <!-- hand written note -->\n\
             \x20 <title>Example</title>\n\
             \x20 <actor>\n\
             \x20   <name>Someone</name>\n\
             \x20   <rating>9.1</rating>\n\
             \x20   <votes>12</votes>\n\
             \x20 </actor>\n\
             \x20 <unknown attr=\"1\">keep &amp; this</unknown>\n\
             \x20 <userrating>8</userrating>\n\
             {APPENDED_RATING_ELEMENTS}\
             </movie>\n"
        );

        let rewritten = rewritten_document(MOVIE_WITH_EXTRAS, &sample_patch(), "movie");

        assert_eq!(rewritten, expected);
        assert!(
            rewritten.contains("<rating>9.1</rating>"),
            "an actor rating must survive: {rewritten}"
        );
        assert!(
            rewritten.contains("<userrating>8</userrating>"),
            "user ratings are not Lux-managed: {rewritten}"
        );
    }

    #[test]
    fn rewrite_replaces_flat_and_container_ratings_without_duplicates() {
        let original = concat!(
            "<movie>\n",
            "  <title>Example</title>\n",
            "  <rating>3.1</rating>\n",
            "  <votes>9</votes>\n",
            "  <criticrating>2.0</criticrating>\n",
            "  <ratings>\n",
            "    <rating name=\"imdb\" max=\"10\" default=\"true\">\n",
            "      <value>3.1</value>\n",
            "      <votes>9</votes>\n",
            "    </rating>\n",
            "    <rating name=\"tomato\" max=\"10\"><value>2.2</value></rating>\n",
            "  </ratings>\n",
            "  <plot>After.</plot>\n",
            "</movie>\n",
        );
        let expected = format!(
            "<movie>\n  <title>Example</title>\n  <plot>After.</plot>\n{APPENDED_RATING_ELEMENTS}</movie>\n"
        );

        let rewritten = rewritten_document(original, &sample_patch(), "movie");

        assert_eq!(rewritten, expected);
        assert!(!rewritten.contains("imdb"), "stale entry kept: {rewritten}");
        assert!(
            !rewritten.contains("tomato"),
            "stale entry kept: {rewritten}"
        );
        assert!(!rewritten.contains("3.1"), "stale score kept: {rewritten}");
        assert_eq!(rewritten.matches("<ratings>").count(), 1);
        assert_eq!(rewritten.matches("</ratings>").count(), 1);
        assert_eq!(rewritten.matches("\n  <rating>").count(), 1);
        assert_eq!(rewritten.matches("\n  <votes>").count(), 1);
        assert_eq!(rewritten.matches("\n  <criticrating>").count(), 1);
    }

    #[test]
    fn rewrite_is_idempotent() {
        let once = rewritten_document(MOVIE_WITH_EXTRAS, &sample_patch(), "movie");
        // Re-applying the patch reports `Unchanged`, and the pure block rewrite proves the bytes
        // are stable rather than only the outcome being stable.
        assert!(matches!(
            rewrite_document(&once, &sample_patch(), "movie"),
            RatingsRewrite::Unchanged
        ));
        let rewritten_again =
            rewrite_selected_block(once.as_bytes(), &sample_patch(), 0).expect("block rewrite");
        assert_eq!(String::from_utf8(rewritten_again).expect("utf8"), once);
        assert_eq!(once.matches("<ratings>").count(), 1);
    }

    #[test]
    fn rewrite_formats_scores_and_escapes_rating_names() {
        let patch = RatingsNfoPatch {
            entries: vec![entry("trakt & \"quoted\" <tag>", 8.7, None, 10, true)],
            rating: 8.7,
            votes: None,
            critic_rating: None,
        };

        let rewritten = rewritten_document(
            "<movie>\n  <title>Example</title>\n</movie>\n",
            &patch,
            "movie",
        );

        assert!(
            rewritten.contains(concat!(
                "  <ratings>\n",
                "    <rating name=\"trakt &amp; &quot;quoted&quot; &lt;tag&gt;\" max=\"10\" ",
                "default=\"true\"><value>8.7</value></rating>\n",
                "  </ratings>\n",
                "  <rating>8.7</rating>\n",
            )),
            "{rewritten}"
        );
        assert!(!rewritten.contains("<votes>"), "{rewritten}");
        assert!(!rewritten.contains("<criticrating>"), "{rewritten}");
    }

    #[test]
    fn rewrite_formats_scores_with_up_to_three_and_at_least_one_decimal() {
        assert_eq!(format_rating_score(6.564), "6.564");
        assert_eq!(format_rating_score(8.7), "8.7");
        assert_eq!(format_rating_score(7.0), "7.0");
        assert_eq!(format_rating_score(0.0), "0.0");
        assert_eq!(format_rating_score(9.9999), "10.0");
        assert_eq!(format_rating_score(10.0), "10.0");
        assert_eq!(format_rating_score(1.0 / 3.0), "0.333");
    }

    #[test]
    fn rewrite_rejects_invalid_patches() {
        let too_many = RatingsNfoPatch {
            entries: (0..=MAX_NFO_RATING_ENTRIES)
                .map(|index| entry(&format!("provider{index}"), 5.0, None, 10, false))
                .collect(),
            ..sample_patch()
        };
        assert!(rewrite_ratings_nfo(b"<movie />", &too_many, "movie", None).is_err());

        let out_of_range = RatingsNfoPatch {
            rating: 11.0,
            ..sample_patch()
        };
        assert!(rewrite_ratings_nfo(b"<movie />", &out_of_range, "movie", None).is_err());

        let empty_name = RatingsNfoPatch {
            entries: vec![entry("  ", 5.0, None, 10, false)],
            ..sample_patch()
        };
        assert!(rewrite_ratings_nfo(b"<movie />", &empty_name, "movie", None).is_err());

        let negative_votes = RatingsNfoPatch {
            votes: Some(-1),
            ..sample_patch()
        };
        assert!(rewrite_ratings_nfo(b"<movie />", &negative_votes, "movie", None).is_err());

        let zero_scale = RatingsNfoPatch {
            entries: vec![entry("trakt", 5.0, None, 0, false)],
            ..sample_patch()
        };
        assert!(rewrite_ratings_nfo(b"<movie />", &zero_scale, "movie", None).is_err());

        let over_scale = RatingsNfoPatch {
            entries: vec![entry("trakt", 11.0, None, 10, false)],
            ..sample_patch()
        };
        assert!(rewrite_ratings_nfo(b"<movie />", &over_scale, "movie", None).is_err());

        let negative_entry_votes = RatingsNfoPatch {
            entries: vec![entry("trakt", 5.0, Some(-2), 10, false)],
            ..sample_patch()
        };
        assert!(rewrite_ratings_nfo(b"<movie />", &negative_entry_votes, "movie", None).is_err());

        let out_of_range_critic = RatingsNfoPatch {
            critic_rating: Some(-0.5),
            ..sample_patch()
        };
        assert!(rewrite_ratings_nfo(b"<movie />", &out_of_range_critic, "movie", None).is_err());

        assert!(matches!(
            rewrite_ratings_nfo(b"<movie />", &sample_patch(), "season", None),
            Err(NfoWriteError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn rewrite_supports_tvshow_roots_and_expands_a_self_closing_root() {
        let rewritten = rewritten_document(
            "<tvshow>\n  <title>Show</title>\n</tvshow>\n",
            &sample_patch(),
            "tvshow",
        );
        assert!(rewritten.starts_with("<tvshow>\n  <title>Show</title>\n  <ratings>\n"));
        assert!(rewritten.ends_with("</tvshow>\n"));

        let empty_root = rewritten_document("<movie />", &sample_patch(), "movie");
        assert!(
            empty_root.starts_with("<movie>\n  <ratings>\n"),
            "{empty_root}"
        );
        assert!(empty_root.ends_with("</movie>"), "{empty_root}");
        assert!(matches!(
            rewrite_document(&empty_root, &sample_patch(), "movie"),
            RatingsRewrite::Unchanged
        ));
    }

    #[test]
    fn rewrite_reports_documents_without_a_matching_root() {
        assert!(matches!(
            rewrite_document("", &sample_patch(), "movie"),
            RatingsRewrite::NoNfo
        ));
        assert!(matches!(
            rewrite_document(" \n\t", &sample_patch(), "movie"),
            RatingsRewrite::NoNfo
        ));
        assert!(matches!(
            rewrite_document(
                "<movie><title>Example</title></movie>",
                &sample_patch(),
                "tvshow"
            ),
            RatingsRewrite::NoNfo
        ));
        assert!(matches!(
            rewrite_ratings_nfo(
                b"<movie><title>Example</title>",
                &sample_patch(),
                "movie",
                None
            ),
            Err(NfoWriteError::InvalidXml(_))
        ));
    }

    const MULTI_EPISODE: &str = concat!(
        "<episodedetails>\n",
        "  <title>One</title>\n",
        "  <season>1</season>\n",
        "  <episode>1</episode>\n",
        "  <rating>9.9</rating>\n",
        "</episodedetails>\n",
        "<episodedetails>\n",
        "  <title>Two</title>\n",
        "  <season>1</season>\n",
        "  <episode>2</episode>\n",
        "</episodedetails>\n",
    );

    #[test]
    fn rewrite_targets_only_the_matching_episode_block() {
        let selector = EpisodeNumbers {
            season_number: Some(1),
            episode_number: Some(2),
        };
        let rewritten = match rewrite_ratings_nfo(
            MULTI_EPISODE.as_bytes(),
            &sample_patch(),
            "episodedetails",
            Some(selector),
        )
        .expect("rewrite")
        {
            RatingsRewrite::Rewritten(bytes) => String::from_utf8(bytes).expect("utf8"),
            other => panic!("expected a rewrite, got {other:?}"),
        };

        assert!(
            rewritten.starts_with(concat!(
                "<episodedetails>\n",
                "  <title>One</title>\n",
                "  <season>1</season>\n",
                "  <episode>1</episode>\n",
                "  <rating>9.9</rating>\n",
                "</episodedetails>\n",
            )),
            "the other episode block must stay byte identical: {rewritten}"
        );
        let second_block = rewritten
            .split_once("</episodedetails>\n")
            .expect("first block")
            .1;
        assert!(second_block.contains("<title>Two</title>"), "{rewritten}");
        assert!(second_block.contains("<ratings>"), "{rewritten}");
        assert!(
            second_block.contains("<rating>6.564</rating>"),
            "{rewritten}"
        );
        assert_eq!(rewritten.matches("<ratings>").count(), 1, "{rewritten}");
    }

    #[test]
    fn rewrite_reports_ambiguous_episode_blocks() {
        let unmatched = EpisodeNumbers {
            season_number: Some(1),
            episode_number: Some(3),
        };
        assert!(matches!(
            rewrite_ratings_nfo(
                MULTI_EPISODE.as_bytes(),
                &sample_patch(),
                "episodedetails",
                Some(unmatched)
            )
            .expect("rewrite"),
            RatingsRewrite::Ambiguous
        ));
        assert!(matches!(
            rewrite_ratings_nfo(
                MULTI_EPISODE.as_bytes(),
                &sample_patch(),
                "episodedetails",
                None
            )
            .expect("rewrite"),
            RatingsRewrite::Ambiguous
        ));

        let duplicate = concat!(
            "<episodedetails><season>1</season><episode>2</episode></episodedetails>\n",
            "<episodedetails><season>1</season><episode>2</episode></episodedetails>\n",
        );
        assert!(matches!(
            rewrite_ratings_nfo(
                duplicate.as_bytes(),
                &sample_patch(),
                "episodedetails",
                Some(EpisodeNumbers {
                    season_number: Some(1),
                    episode_number: Some(2),
                })
            )
            .expect("rewrite"),
            RatingsRewrite::Ambiguous
        ));

        let single_foreign_block =
            "<episodedetails><season>1</season><episode>5</episode></episodedetails>\n";
        assert!(matches!(
            rewrite_ratings_nfo(
                single_foreign_block.as_bytes(),
                &sample_patch(),
                "episodedetails",
                Some(EpisodeNumbers {
                    season_number: Some(1),
                    episode_number: Some(3),
                })
            )
            .expect("rewrite"),
            RatingsRewrite::Ambiguous
        ));
        assert!(matches!(
            rewrite_ratings_nfo(
                single_foreign_block.as_bytes(),
                &sample_patch(),
                "episodedetails",
                Some(EpisodeNumbers {
                    season_number: Some(1),
                    episode_number: Some(5),
                })
            )
            .expect("rewrite"),
            RatingsRewrite::Rewritten(_)
        ));
        assert!(matches!(
            rewrite_ratings_nfo(
                b"<episodedetails />",
                &sample_patch(),
                "episodedetails",
                Some(EpisodeNumbers {
                    season_number: Some(1),
                    episode_number: Some(5),
                })
            )
            .expect("rewrite"),
            RatingsRewrite::Rewritten(_)
        ));
    }

    #[test]
    fn written_ratings_are_read_back_by_the_projection_parser() {
        let rewritten = rewritten_document(MOVIE_WITH_EXTRAS, &sample_patch(), "movie");
        let projection = parse_local_nfo_projection(rewritten.as_bytes()).expect("projection");
        assert_eq!(projection.details.rating, Some(6.564));
        assert_eq!(projection.details.votes, Some(280));

        let container_only = rewritten
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !(trimmed.starts_with("<rating>")
                    || trimmed.starts_with("<votes>")
                    || trimmed.starts_with("<criticrating>"))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let projection = parse_local_nfo_projection(container_only.as_bytes()).expect("projection");
        // The container reader keeps two decimals, so 6.564 becomes 6.56; the point of the
        // assertion is that the `default="true"` trakt entry wins instead of the 7.4 tmdb entry.
        assert_eq!(
            projection.details.rating,
            Some(6.56),
            "the default=\"true\" entry must win over the tmdb entry: {container_only}"
        );
        assert_eq!(projection.details.votes, Some(280));
    }

    fn mutate_ratings_target(path: &Path) -> std::io::Result<()> {
        std::fs::write(path, "<movie><title>external</title></movie>")
    }

    #[tokio::test]
    async fn atomic_ratings_write_detects_concurrent_modification() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("movie.nfo");
        tokio::fs::write(&target, "<movie><title>old</title></movie>")
            .await
            .expect("initial nfo");

        let result = super::super::write_nfo_atomically_with_rewriter(
            &target,
            |original| match rewrite_ratings_nfo(original, &sample_patch(), "movie", None)? {
                RatingsRewrite::Rewritten(bytes) => Ok(bytes),
                _ => Ok(original.to_vec()),
            },
            Some(mutate_ratings_target),
        )
        .await;

        assert!(matches!(
            result,
            Err(NfoWriteError::ConcurrentModification(_))
        ));
        let content = tokio::fs::read_to_string(&target).await.expect("target");
        assert!(content.contains("external"), "{content}");
    }

    struct ScannedLibrary {
        _directory: tempfile::TempDir,
        database: Database,
        root: std::path::PathBuf,
    }

    async fn scan_movie_library(nfo: Option<&str>) -> ScannedLibrary {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("address"),
            config_dir: directory.path().join("config"),
        };
        let root = directory.path().join("Movies");
        fs::create_dir_all(&root).await.expect("movies");
        fs::write(root.join("Example.Movie.2020.mkv"), b"fixture")
            .await
            .expect("media");
        if let Some(nfo) = nfo {
            fs::write(root.join("Example.Movie.2020.nfo"), nfo)
                .await
                .expect("nfo");
        }
        let database = Database::connect(&config).await.expect("database");
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, false)
            .await
            .expect("library");
        libraries
            .add_root(library.id, root.to_str().expect("utf8 root"))
            .await
            .expect("root");
        LibraryScanner::new(database.clone())
            .scan_movie_library(library.id)
            .await
            .expect("scan");
        ScannedLibrary {
            _directory: directory,
            database,
            root,
        }
    }

    async fn scan_series_library(nfo: &str) -> ScannedLibrary {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("address"),
            config_dir: directory.path().join("config"),
        };
        let root = directory.path().join("Shows");
        let season = root.join("Example Show (2020)").join("Season 01");
        fs::create_dir_all(&season).await.expect("season");
        fs::write(season.join("Example.Show.S01E01.mkv"), b"fixture")
            .await
            .expect("episode one");
        fs::write(season.join("Example.Show.S01E02.mkv"), b"fixture")
            .await
            .expect("episode two");
        fs::write(season.join("episode.nfo"), nfo)
            .await
            .expect("episode nfo");
        let database = Database::connect(&config).await.expect("database");
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Shows", LibraryKind::Series, false)
            .await
            .expect("library");
        libraries
            .add_root(library.id, root.to_str().expect("utf8 root"))
            .await
            .expect("root");
        LibraryScanner::new(database.clone())
            .scan_series_library(library.id)
            .await
            .expect("scan");
        ScannedLibrary {
            _directory: directory,
            database,
            root,
        }
    }

    async fn item_id(database: &Database, item_type: &str) -> String {
        sqlx::query_scalar::<_, String>(
            "SELECT id FROM media_items WHERE item_type = ? ORDER BY id LIMIT 1",
        )
        .bind(item_type)
        .fetch_one(database.pool())
        .await
        .expect("item id")
    }

    async fn episode_item_id(database: &Database, episode_number: i64) -> String {
        sqlx::query_scalar::<_, String>(
            "SELECT id FROM media_items WHERE item_type = 'EPISODE' AND episode_number = ?",
        )
        .bind(episode_number)
        .fetch_one(database.pool())
        .await
        .expect("episode item id")
    }

    #[tokio::test]
    async fn write_item_ratings_rewrites_a_movie_nfo_once() {
        let scanned = scan_movie_library(Some(MOVIE_WITH_EXTRAS)).await;
        let target = scanned.root.join("Example.Movie.2020.nfo");
        let writer = NfoWriteService::new(scanned.database.clone());
        let movie_id = item_id(&scanned.database, "MOVIE").await;

        let outcome = writer
            .write_item_ratings(&movie_id, &sample_patch())
            .await
            .expect("write ratings");
        assert_eq!(
            outcome,
            RatingsWriteOutcome::Written {
                path: target.clone()
            }
        );

        let content = fs::read_to_string(&target).await.expect("nfo");
        assert!(
            content.contains("<criticrating>7.2</criticrating>"),
            "{content}"
        );
        let projection = parse_local_nfo_projection(content.as_bytes()).expect("projection");
        assert_eq!(projection.details.rating, Some(6.564));
        assert_eq!(projection.details.votes, Some(280));
        let stored = stored_nfo_state(&scanned.database, &movie_id).await;
        assert!(
            stored.1.is_some(),
            "the write must refresh the stored NFO fingerprint: {stored:?}"
        );

        let again = writer
            .write_item_ratings(&movie_id, &sample_patch())
            .await
            .expect("write ratings again");
        assert_eq!(again, RatingsWriteOutcome::Unchanged);
        assert_eq!(fs::read_to_string(&target).await.expect("nfo"), content);
        assert_eq!(
            stored_nfo_state(&scanned.database, &movie_id).await,
            stored,
            "an unchanged write must not touch the stored NFO state"
        );
        assert!(
            crate::application::images::should_suppress_internal_image_write(&target).await,
            "the watcher must recognize the Lux-owned NFO write"
        );
    }

    async fn stored_nfo_state(
        database: &Database,
        item_id: &str,
    ) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
        sqlx::query_as(
            "SELECT nfo_metadata_fingerprint, metadata_fingerprint FROM media_items WHERE id = ?",
        )
        .bind(item_id)
        .fetch_one(database.pool())
        .await
        .expect("stored nfo state")
    }

    #[tokio::test]
    async fn write_item_ratings_skips_items_without_a_usable_nfo() {
        let scanned = scan_movie_library(None).await;
        let writer = NfoWriteService::new(scanned.database.clone());
        let movie_id = item_id(&scanned.database, "MOVIE").await;

        let outcome = writer
            .write_item_ratings(&movie_id, &sample_patch())
            .await
            .expect("write ratings");
        assert_eq!(outcome, RatingsWriteOutcome::SkippedNoNfo);
        assert!(
            !scanned.root.join("movie.nfo").exists(),
            "a rating-only write must never create an NFO"
        );
        assert!(!scanned.root.join("Example.Movie.2020.nfo").exists());

        assert!(matches!(
            writer
                .write_item_ratings("missing-item", &sample_patch())
                .await,
            Err(NfoWriteError::ItemNotFound)
        ));
    }

    #[tokio::test]
    async fn item_has_nfo_reports_existing_sidecars_without_creating_one() {
        let scanned = scan_movie_library(Some(MOVIE_WITH_EXTRAS)).await;
        let writer = NfoWriteService::new(scanned.database.clone());
        let movie_id = item_id(&scanned.database, "MOVIE").await;

        assert!(
            writer.item_has_nfo(&movie_id).await.expect("has nfo"),
            "the scanned movie has a sibling NFO"
        );
        assert!(
            !writer.item_has_nfo("missing-item").await.expect("missing"),
            "an unknown item has no NFO"
        );

        let without_nfo = scan_movie_library(None).await;
        let writer = NfoWriteService::new(without_nfo.database.clone());
        let movie_id = item_id(&without_nfo.database, "MOVIE").await;
        assert!(
            !writer.item_has_nfo(&movie_id).await.expect("no nfo"),
            "a movie without a sidecar must report false"
        );
        assert!(
            !without_nfo.root.join("movie.nfo").exists(),
            "item_has_nfo must never create the fallback movie.nfo"
        );

        let episodes = scan_series_library(
            "<episodedetails>\n  <title>One</title>\n  <season>1</season>\n  <episode>1</episode>\n</episodedetails>\n",
        )
        .await;
        let writer = NfoWriteService::new(episodes.database.clone());
        assert!(
            writer
                .item_has_nfo(&episode_item_id(&episodes.database, 1).await)
                .await
                .expect("episode nfo"),
            "the episode sidecar exists"
        );
        assert!(
            !writer
                .item_has_nfo(&item_id(&episodes.database, "SERIES").await)
                .await
                .expect("series nfo"),
            "a series without tvshow.nfo reports false"
        );
        assert!(
            !writer
                .item_has_nfo(&item_id(&episodes.database, "SEASON").await)
                .await
                .expect("season nfo"),
            "a season has no rating-bearing NFO of its own"
        );
    }

    #[tokio::test]
    async fn write_item_ratings_skips_item_kinds_without_an_own_nfo() {
        let scanned = scan_movie_library(Some(MOVIE_WITH_EXTRAS)).await;
        let writer = NfoWriteService::new(scanned.database.clone());
        let movie_id = item_id(&scanned.database, "MOVIE").await;
        sqlx::query("UPDATE media_items SET item_type = 'VIDEO' WHERE id = ?")
            .bind(&movie_id)
            .execute(scanned.database.pool())
            .await
            .expect("item type");

        let outcome = writer
            .write_item_ratings(&movie_id, &sample_patch())
            .await
            .expect("write ratings");
        assert_eq!(outcome, RatingsWriteOutcome::SkippedNoNfo);
        assert_eq!(
            fs::read_to_string(scanned.root.join("Example.Movie.2020.nfo"))
                .await
                .expect("nfo"),
            MOVIE_WITH_EXTRAS
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_item_ratings_rejects_a_symlinked_nfo() {
        let scanned = scan_movie_library(Some(MOVIE_WITH_EXTRAS)).await;
        let writer = NfoWriteService::new(scanned.database.clone());
        let movie_id = item_id(&scanned.database, "MOVIE").await;
        let real = scanned.root.join("real.nfo");
        let link = scanned.root.join("Example.Movie.2020.nfo");
        fs::rename(&link, &real).await.expect("move nfo");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        assert!(matches!(
            writer.write_item_ratings(&movie_id, &sample_patch()).await,
            Err(NfoWriteError::SymlinkTarget(_))
        ));
        assert_eq!(
            fs::read_to_string(&real).await.expect("real nfo"),
            MOVIE_WITH_EXTRAS
        );
    }

    #[tokio::test]
    async fn write_item_ratings_writes_series_and_episodes() {
        let episode_nfo = concat!(
            "<episodedetails>\n",
            "  <title>One</title>\n",
            "  <season>1</season>\n",
            "  <episode>1</episode>\n",
            "  <rating>9.9</rating>\n",
            "</episodedetails>\n",
            "<episodedetails>\n",
            "  <title>Two</title>\n",
            "  <season>1</season>\n",
            "  <episode>2</episode>\n",
            "</episodedetails>\n",
        );
        let scanned = scan_series_library(episode_nfo).await;
        let writer = NfoWriteService::new(scanned.database.clone());
        let season_dir = scanned.root.join("Example Show (2020)").join("Season 01");
        let series_nfo = scanned.root.join("Example Show (2020)").join("tvshow.nfo");
        fs::write(
            &series_nfo,
            "<tvshow>\n  <title>Example Show</title>\n</tvshow>\n",
        )
        .await
        .expect("series nfo");

        let series_id = item_id(&scanned.database, "SERIES").await;
        assert_eq!(
            writer
                .write_item_ratings(&series_id, &sample_patch())
                .await
                .expect("series ratings"),
            RatingsWriteOutcome::Written {
                path: series_nfo.clone()
            }
        );
        let series_content = fs::read_to_string(&series_nfo).await.expect("series nfo");
        assert!(series_content.contains("<tvshow>"));
        assert!(series_content.contains("<ratings>"));

        let second_episode = episode_item_id(&scanned.database, 2).await;
        assert_eq!(
            writer
                .write_item_ratings(&second_episode, &sample_patch())
                .await
                .expect("episode ratings"),
            RatingsWriteOutcome::Written {
                path: season_dir.join("episode.nfo")
            }
        );
        let episode_content = fs::read_to_string(season_dir.join("episode.nfo"))
            .await
            .expect("episode nfo");
        assert!(
            episode_content.starts_with(concat!(
                "<episodedetails>\n",
                "  <title>One</title>\n",
                "  <season>1</season>\n",
                "  <episode>1</episode>\n",
                "  <rating>9.9</rating>\n",
                "</episodedetails>\n",
            )),
            "the other episode block must not change: {episode_content}"
        );
        assert_eq!(episode_content.matches("<ratings>").count(), 1);

        let first_episode = episode_item_id(&scanned.database, 1).await;
        assert!(matches!(
            writer
                .write_item_ratings(&first_episode, &sample_patch())
                .await
                .expect("first episode ratings"),
            RatingsWriteOutcome::Written { .. }
        ));
        let episode_content = fs::read_to_string(season_dir.join("episode.nfo"))
            .await
            .expect("episode nfo");
        assert_eq!(episode_content.matches("<ratings>").count(), 2);
        assert!(!episode_content.contains("<rating>9.9</rating>"));

        let season_id = item_id(&scanned.database, "SEASON").await;
        assert_eq!(
            writer
                .write_item_ratings(&season_id, &sample_patch())
                .await
                .expect("season ratings"),
            RatingsWriteOutcome::SkippedNoNfo
        );
    }
}
