//! Structural validation and entry-level inspection of Bilibili/Dandanplay
//! danmaku XML sidecars.
//!
//! The validity rules are shared with the Web parser
//! (`web/src/features/player/danmaku/parse.ts`) and pinned by the same test
//! vectors (`tests/fixtures/danmaku/validity-vectors.json`):
//!
//! - the root element is `<i>` and document type declarations are rejected;
//! - an entry is `<d p="time,mode,size,color,...">` with at least four `p`
//!   fields;
//! - `time` is finite and `0 <= time <= 86400`;
//! - `mode` is one of `1..=6` (advanced/mode 7, code/mode 8 and unknown modes
//!   are dropped entry by entry);
//! - `size` only has to be a finite number: the playback parser and the
//!   normalization job clamp it to `12..=64`, so an out-of-range size must not
//!   invalidate the entry;
//! - `color` is an integer in `0..=0xFFFFFF`;
//! - the decoded text is non-empty after trimming, at most 200 Unicode code
//!   points, and contains no ASCII control character.
//!
//! A file is valid when it is structurally sound and carries at least one
//! valid entry. Dirty entries are counted and skipped instead of failing the
//! whole file.

use std::fmt;

use quick_xml::{
    events::{BytesStart, Event},
    reader::Reader,
};

pub const MAX_DANMAKU_XML_BYTES: usize = 50 * 1024 * 1024;
/// Longest accepted danmaku text, counted in Unicode code points.
pub const MAX_DANMAKU_TEXT_CHARS: usize = 200;
pub const MIN_DANMAKU_FONT_SIZE: f64 = 12.0;
pub const MAX_DANMAKU_FONT_SIZE: f64 = 64.0;
pub const MAX_DANMAKU_TIME_SECONDS: f64 = 86_400.0;
pub const MIN_DANMAKU_MODE: f64 = 1.0;
pub const MAX_DANMAKU_MODE: f64 = 6.0;
pub const MAX_DANMAKU_COLOR: f64 = 0x00FF_FFFF as f64;

/// Entry-level outcome of inspecting one danmaku XML document.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DanmakuXmlStats {
    /// Entries the playback parser would render.
    pub valid_entries: usize,
    /// Entries the playback parser would skip.
    pub dropped_entries: usize,
}

impl DanmakuXmlStats {
    pub fn total_entries(&self) -> usize {
        self.valid_entries + self.dropped_entries
    }
}

pub fn validate_danmaku_xml(bytes: &[u8]) -> Result<(), DanmakuXmlError> {
    inspect_danmaku_xml(bytes).map(|_| ())
}

/// Validates the document and returns the per-entry counts.
///
/// Structural problems (empty payload, oversize, wrong root, DOCTYPE,
/// malformed XML) fail the whole document. A document with no valid entry is
/// [`DanmakuXmlError::MissingDanmaku`].
pub fn inspect_danmaku_xml(bytes: &[u8]) -> Result<DanmakuXmlStats, DanmakuXmlError> {
    let document = parse_danmaku_document(bytes)?;
    if document.entries.is_empty() {
        return Err(DanmakuXmlError::MissingDanmaku);
    }
    Ok(document.stats())
}

/// One `<d>` entry that passed the shared entry rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedDanmakuEntry {
    /// Value of the `p` attribute, ready to be written back (unescaped).
    pub(crate) attribute: String,
    /// Decoded, trimmed entry text.
    pub(crate) text: String,
    /// True when the entry only stayed valid because a field was corrected
    /// (currently: the font size was clamped to `12..=64`, or the text needed
    /// trimming / line-ending normalization).
    pub(crate) corrected: bool,
}

/// Result of walking a structurally valid danmaku document.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ParsedDanmakuDocument {
    pub(crate) entries: Vec<ParsedDanmakuEntry>,
    pub(crate) dropped_entries: usize,
}

impl ParsedDanmakuDocument {
    pub(crate) fn stats(&self) -> DanmakuXmlStats {
        DanmakuXmlStats {
            valid_entries: self.entries.len(),
            dropped_entries: self.dropped_entries,
        }
    }

    pub(crate) fn total_entries(&self) -> usize {
        self.entries.len() + self.dropped_entries
    }
}

/// Structural validation plus entry classification.
///
/// Does **not** fail when every entry is dirty; callers decide whether that is
/// an error (`inspect_danmaku_xml`, `validate_danmaku_xml`) or a normalization
/// outcome (`normalize::normalize_danmaku_xml`).
pub(crate) fn parse_danmaku_document(
    bytes: &[u8],
) -> Result<ParsedDanmakuDocument, DanmakuXmlError> {
    if bytes.is_empty() {
        return Err(DanmakuXmlError::Empty);
    }
    if bytes.len() > MAX_DANMAKU_XML_BYTES {
        return Err(DanmakuXmlError::TooLarge);
    }

    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut saw_root = false;
    let mut document = ParsedDanmakuDocument::default();
    let mut entry: Option<EntryState> = None;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(event)) => {
                let name = local_name(event.name().as_ref()).to_vec();
                if depth == 0 {
                    if saw_root || name != b"i" {
                        return Err(DanmakuXmlError::InvalidRoot);
                    }
                    saw_root = true;
                } else if depth == 1 && name == b"d" {
                    entry = Some(EntryState::new(&event, depth));
                } else if let Some(state) = entry.as_mut() {
                    // Markup inside `<d>` is not plain danmaku text.
                    state.invalid = true;
                }
                depth = depth.saturating_add(1);
            }
            Ok(Event::Empty(event)) => {
                let name = local_name(event.name().as_ref()).to_vec();
                if depth == 0 {
                    if saw_root || name != b"i" {
                        return Err(DanmakuXmlError::InvalidRoot);
                    }
                    saw_root = true;
                } else if depth == 1 && name == b"d" {
                    // A self-closing `<d/>` has no text and no usable fields.
                    document.dropped_entries += 1;
                } else if let Some(state) = entry.as_mut() {
                    state.invalid = true;
                }
            }
            Ok(Event::End(_)) => {
                if depth == 0 {
                    return Err(DanmakuXmlError::InvalidXml(
                        "unexpected closing element".to_owned(),
                    ));
                }
                depth -= 1;
                if entry
                    .as_ref()
                    .is_some_and(|state| state.start_depth == depth)
                {
                    if let Some(state) = entry.take() {
                        match state.finish() {
                            Some(parsed) => document.entries.push(parsed),
                            None => document.dropped_entries += 1,
                        }
                    }
                }
            }
            Ok(Event::Text(event)) => {
                if let Some(state) = entry.as_mut() {
                    if depth == state.start_depth + 1 {
                        match event.decode() {
                            Ok(text) => state.text.push_str(text.as_ref()),
                            Err(_) => state.invalid = true,
                        }
                    } else {
                        state.invalid = true;
                    }
                }
            }
            Ok(Event::GeneralRef(event)) => {
                if let Some(state) = entry.as_mut() {
                    if depth == state.start_depth + 1 {
                        match event.decode() {
                            Ok(reference) => state.push_reference(reference.as_ref()),
                            Err(_) => state.invalid = true,
                        }
                    } else {
                        state.invalid = true;
                    }
                }
            }
            Ok(Event::CData(_) | Event::Comment(_) | Event::PI(_) | Event::Decl(_)) => {
                if let Some(state) = entry.as_mut() {
                    // CDATA, comments and processing instructions are markup, not
                    // text the Web parser would render for this entry.
                    state.invalid = true;
                }
            }
            Ok(Event::DocType(_)) => {
                return Err(DanmakuXmlError::InvalidXml(
                    "document type declarations are not allowed".to_owned(),
                ));
            }
            Err(error) => return Err(DanmakuXmlError::InvalidXml(error.to_string())),
        }
        buffer.clear();
    }
    if !saw_root || depth != 0 {
        return Err(DanmakuXmlError::InvalidXml(
            "danmaku XML document is incomplete".to_owned(),
        ));
    }
    Ok(document)
}

/// Entry while its `<d>` element is still open.
struct EntryState {
    start_depth: usize,
    attribute: Option<String>,
    text: String,
    invalid: bool,
}

impl EntryState {
    fn new(event: &BytesStart<'_>, start_depth: usize) -> Self {
        Self {
            start_depth,
            attribute: danmaku_p_attribute(event),
            text: String::new(),
            invalid: false,
        }
    }

    /// Appends a decoded entity/numeric reference.
    ///
    /// The mapping matches `decodeXmlText` in the Web parser: the five XML
    /// predefined entities and numeric references are decoded, anything else
    /// is kept verbatim.
    fn push_reference(&mut self, reference: &str) {
        let lowered = reference.to_ascii_lowercase();
        match lowered.as_str() {
            "lt" => self.text.push('<'),
            "gt" => self.text.push('>'),
            "amp" => self.text.push('&'),
            "quot" => self.text.push('"'),
            "apos" => self.text.push('\''),
            _ => match decode_numeric_reference(&lowered) {
                Some(character) => self.text.push(character),
                None => {
                    self.text.push('&');
                    self.text.push_str(reference);
                    self.text.push(';');
                }
            },
        }
    }

    fn finish(self) -> Option<ParsedDanmakuEntry> {
        if self.invalid {
            return None;
        }
        let attribute = self.attribute?;
        normalize_entry(attribute, &self.text)
    }
}

fn decode_numeric_reference(lowered: &str) -> Option<char> {
    let digits = if let Some(hex) = lowered.strip_prefix("#x") {
        return u32::from_str_radix(hex, 16)
            .ok()
            .filter(|value| *value <= 0x10_FFFF)
            .and_then(char::from_u32);
    } else {
        lowered.strip_prefix('#')?
    };
    let digits: String = digits.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits
        .parse::<u32>()
        .ok()
        .filter(|value| *value <= 0x10_FFFF)
        .and_then(char::from_u32)
}

fn danmaku_p_attribute(event: &BytesStart<'_>) -> Option<String> {
    let attributes = event.attributes();
    for attribute in attributes.flatten() {
        if !attribute
            .key
            .local_name()
            .as_ref()
            .eq_ignore_ascii_case(b"p")
        {
            continue;
        }
        return std::str::from_utf8(attribute.value.as_ref())
            .ok()
            .map(str::to_owned);
    }
    None
}

/// Applies the shared entry rules and builds the canonical entry.
fn normalize_entry(attribute: String, raw_text: &str) -> Option<ParsedDanmakuEntry> {
    let mut fields: Vec<String> = attribute.split(',').map(str::to_owned).collect();
    if fields.len() < 4 {
        return None;
    }
    let time = parse_danmaku_number(&fields[0])?;
    if !(0.0..=MAX_DANMAKU_TIME_SECONDS).contains(&time) {
        return None;
    }
    let mode = parse_danmaku_number(&fields[1])?;
    if mode.fract() != 0.0 || !(MIN_DANMAKU_MODE..=MAX_DANMAKU_MODE).contains(&mode) {
        return None;
    }
    let mut size = parse_danmaku_number(&fields[2])?;
    let color = parse_danmaku_number(&fields[3])?;
    if color.fract() != 0.0 || !(0.0..=MAX_DANMAKU_COLOR).contains(&color) {
        return None;
    }
    let normalized_text = raw_text.replace("\r\n", "\n").replace('\r', "\n");
    let text = trim_danmaku_text(&normalized_text);
    if text.is_empty()
        || text.chars().count() > MAX_DANMAKU_TEXT_CHARS
        || text.chars().any(is_control_character)
    {
        return None;
    }
    let mut corrected = text != raw_text;
    if !(MIN_DANMAKU_FONT_SIZE..=MAX_DANMAKU_FONT_SIZE).contains(&size) {
        size = size.clamp(MIN_DANMAKU_FONT_SIZE, MAX_DANMAKU_FONT_SIZE);
        fields[2] = format_danmaku_number(size);
        corrected = true;
    }
    Some(ParsedDanmakuEntry {
        attribute: fields.join(","),
        text: text.to_owned(),
        corrected,
    })
}

/// Parses a `p` field the way `Number(value)` does for the inputs danmaku
/// sidecars actually use: surrounding whitespace is ignored, an empty field is
/// `0`, `0x` decimals are hexadecimal, and everything else must be a finite
/// decimal (including exponent notation).
fn parse_danmaku_number(value: &str) -> Option<f64> {
    let trimmed = value.trim_matches(is_javascript_whitespace);
    if trimmed.is_empty() {
        return Some(0.0);
    }
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return u64::from_str_radix(hex, 16)
            .ok()
            .map(|value| value as f64)
            .filter(|value| value.is_finite());
    }
    trimmed
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn format_danmaku_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 9.0e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// `String.prototype.trim` also strips a byte-order mark; Rust's `trim` does
/// not.
fn trim_danmaku_text(value: &str) -> &str {
    value.trim_matches(is_javascript_whitespace)
}

fn is_javascript_whitespace(value: char) -> bool {
    value.is_whitespace() || value == '\u{feff}'
}

fn is_control_character(value: char) -> bool {
    matches!(
        value,
        '\u{0000}'..='\u{0008}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000E}'..='\u{001F}'
            | '\u{007F}'
    )
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|value| *value == b':').next().unwrap_or(name)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DanmakuXmlError {
    Empty,
    TooLarge,
    InvalidRoot,
    MissingDanmaku,
    InvalidXml(String),
}

impl fmt::Display for DanmakuXmlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("danmaku XML is empty"),
            Self::TooLarge => formatter.write_str("danmaku XML is too large"),
            Self::InvalidRoot => formatter.write_str("danmaku XML root must be <i>"),
            Self::MissingDanmaku => formatter.write_str("danmaku XML has no valid <d> entries"),
            Self::InvalidXml(message) => write!(formatter, "danmaku XML is invalid: {message}"),
        }
    }
}

impl std::error::Error for DanmakuXmlError {}

#[cfg(test)]
mod tests {
    use super::{
        DanmakuXmlError, MAX_DANMAKU_XML_BYTES, inspect_danmaku_xml, validate_danmaku_xml,
    };

    const SHARED_VECTORS: &str =
        include_str!("../../../tests/fixtures/danmaku/validity-vectors.json");

    #[test]
    fn shared_validity_vectors_match_the_web_parser() {
        let vectors: serde_json::Value =
            serde_json::from_str(SHARED_VECTORS).expect("validity vectors are valid JSON");
        let vectors = vectors["vectors"]
            .as_array()
            .expect("validity vectors expose a vectors array");
        assert!(!vectors.is_empty());
        for vector in vectors {
            let name = vector["name"].as_str().expect("vector name");
            let xml = vector["xml"].as_str().expect("vector xml");
            let expected_valid = vector["valid"].as_u64().expect("vector valid") as usize;
            let expected_dropped = vector["dropped"].as_u64().expect("vector dropped") as usize;
            assert_eq!(
                inspect_danmaku_xml(xml.as_bytes()),
                Ok(super::DanmakuXmlStats {
                    valid_entries: expected_valid,
                    dropped_entries: expected_dropped,
                }),
                "vector {name}"
            );
            assert_eq!(
                validate_danmaku_xml(xml.as_bytes()).is_ok(),
                expected_valid > 0,
                "vector {name}"
            );
        }
    }

    #[test]
    fn counts_valid_and_dropped_entries_instead_of_failing_the_file() {
        let xml = br#"<i>
            <d p="1,1,25,16777215,0,0,0,0">ok</d>
            <d p="1,7,25,16777215,0,0,0,0">advanced</d>
            <d p="1,1,25,16777215,0,0,0,0">ok too</d>
        </i>"#;
        assert_eq!(
            inspect_danmaku_xml(xml),
            Ok(super::DanmakuXmlStats {
                valid_entries: 2,
                dropped_entries: 1,
            })
        );
        assert!(validate_danmaku_xml(xml).is_ok());
    }

    #[test]
    fn rejects_structurally_invalid_documents() {
        assert_eq!(inspect_danmaku_xml(b""), Err(DanmakuXmlError::Empty));
        assert_eq!(
            inspect_danmaku_xml(&vec![b'x'; MAX_DANMAKU_XML_BYTES + 1]),
            Err(DanmakuXmlError::TooLarge)
        );
        assert_eq!(
            inspect_danmaku_xml(b"<html>not danmaku</html>"),
            Err(DanmakuXmlError::InvalidRoot)
        );
        assert!(matches!(
            inspect_danmaku_xml(b"<i><d p=\"1,1,25,0\">x"),
            Err(DanmakuXmlError::InvalidXml(_))
        ));
        assert!(matches!(
            inspect_danmaku_xml(
                b"<?xml version=\"1.0\"?><!DOCTYPE i [<!ENTITY x \"y\">]><i><d p=\"1,1,25,0\">x</d></i>"
            ),
            Err(DanmakuXmlError::InvalidXml(_))
        ));
    }

    #[test]
    fn a_file_without_any_valid_entry_is_invalid() {
        let all_dirty = br#"<i><d p="1,7,25,0">advanced</d><d p="1,1,25,0"></d></i>"#;
        assert_eq!(
            inspect_danmaku_xml(all_dirty),
            Err(DanmakuXmlError::MissingDanmaku)
        );
        assert_eq!(
            inspect_danmaku_xml(b"<i><chatserver>chat.bilibili.com</chatserver></i>"),
            Err(DanmakuXmlError::MissingDanmaku)
        );
        assert_eq!(
            inspect_danmaku_xml(b"<i></i>"),
            Err(DanmakuXmlError::MissingDanmaku)
        );
    }

    #[test]
    fn entry_rules_are_applied_field_by_field() {
        let cases: &[(&str, usize)] = &[
            (r#"<d p="1,1,25,16777215,0,0,0,0">ok</d>"#, 1),
            (r#"<d p="1,1,25,16777215">ok</d>"#, 1),
            (r#"<d p="1,1,25">ok</d>"#, 0),
            (r#"<d p="1,7,25,16777215">adv</d>"#, 0),
            (r#"<d p="1,8,25,16777215">code</d>"#, 0),
            (r#"<d p="1,0,25,16777215">none</d>"#, 0),
            (r#"<d p="1,1,1,16777215">small</d>"#, 1),
            (r#"<d p="1,1,999,16777215">large</d>"#, 1),
            (r#"<d p="1,1,NaN,16777215">nan</d>"#, 0),
            (r#"<d p="-1,1,25,16777215">negative</d>"#, 0),
            (r#"<d p="86400,1,25,16777215">max</d>"#, 1),
            (r#"<d p="86401,1,25,16777215">too late</d>"#, 0),
            (r#"<d p="1,1,25,16777216">color</d>"#, 0),
            (r#"<d p="1,1,25,-1">color</d>"#, 0),
            (r#"<d p="1,1,25,12.5">color</d>"#, 0),
            (r#"<d p="1,1,25,16777215"> </d>"#, 0),
            (r#"<d p="1,1,25,16777215">&#1;</d>"#, 0),
            (r#"<d p="1,1,25,16777215">&amp;ok</d>"#, 1),
        ];
        for (entry, expected) in cases {
            let xml = format!("<i>{entry}</i>");
            let stats = inspect_danmaku_xml(xml.as_bytes()).unwrap_or(super::DanmakuXmlStats {
                valid_entries: 0,
                dropped_entries: 1,
            });
            assert_eq!(stats.valid_entries, *expected, "entry {entry}");
        }
    }

    #[test]
    fn text_longer_than_two_hundred_code_points_is_dropped() {
        let ok = "弹".repeat(200);
        let too_long = "弹".repeat(201);
        let xml = format!(
            "<i><d p=\"1,1,25,16777215\">{ok}</d><d p=\"1,1,25,16777215\">{too_long}</d></i>"
        );
        assert_eq!(
            inspect_danmaku_xml(xml.as_bytes()),
            Ok(super::DanmakuXmlStats {
                valid_entries: 1,
                dropped_entries: 1,
            })
        );
    }
}
