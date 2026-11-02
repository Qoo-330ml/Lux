//! Normalization of stored danmaku XML sidecars.
//!
//! `DANMAKU_NORMALIZE` rewrites a Lux-written sidecar into the canonical shape
//! the shared validity rules describe: invalid entries are dropped, the font
//! size is clamped to `12..=64`, the text is trimmed, and modes outside
//! `1..=6` are dropped. Everything else (entry order, `p` fields beyond the
//! first four, text of valid entries) is preserved so a normalization pass is
//! limited to what the rules actually require.
//!
//! The module is pure: it takes the sidecar bytes and returns the bytes that
//! should replace them. Reading, atomic writing and bookkeeping live in the
//! `job` module.

use super::xml::{DanmakuXmlError, ParsedDanmakuDocument, parse_danmaku_document};

/// Outcome of normalizing one sidecar's bytes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DanmakuNormalization {
    /// `<d>` entries in the original document, including the invalid ones.
    pub entries_before: usize,
    /// Entries the normalized document would keep.
    pub entries_after: usize,
    /// Replacement bytes; `None` means the sidecar is already canonical and
    /// must not be rewritten.
    pub output: Option<Vec<u8>>,
}

impl DanmakuNormalization {
    pub fn changed(&self) -> bool {
        self.output.is_some()
    }
}

/// Normalizes one sidecar.
///
/// Structural problems fail with [`DanmakuXmlError`]. A document whose entries
/// are all invalid returns `entries_after == 0` with no output: the caller
/// records the item as failed and must keep the original file untouched.
pub fn normalize_danmaku_xml(bytes: &[u8]) -> Result<DanmakuNormalization, DanmakuXmlError> {
    let document = parse_danmaku_document(bytes)?;
    normalize_parsed_document(document)
}

fn normalize_parsed_document(
    document: ParsedDanmakuDocument,
) -> Result<DanmakuNormalization, DanmakuXmlError> {
    let entries_before = document.total_entries();
    let entries_after = document.entries.len();
    if entries_after == 0 {
        return Ok(DanmakuNormalization {
            entries_before,
            entries_after,
            output: None,
        });
    }
    let needs_rewrite =
        document.dropped_entries > 0 || document.entries.iter().any(|entry| entry.corrected);
    let output = needs_rewrite.then(|| serialize_danmaku_document(&document));
    Ok(DanmakuNormalization {
        entries_before,
        entries_after,
        output,
    })
}

fn serialize_danmaku_document(document: &ParsedDanmakuDocument) -> Vec<u8> {
    let mut output = String::from("<i>\n");
    for entry in &document.entries {
        output.push_str("<d p=\"");
        escape_xml_attribute(&mut output, &entry.attribute);
        output.push_str("\">");
        escape_xml_text(&mut output, &entry.text);
        output.push_str("</d>\n");
    }
    output.push_str("</i>\n");
    output.into_bytes()
}

fn escape_xml_attribute(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            _ => output.push(character),
        }
    }
}

fn escape_xml_text(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_danmaku_xml;
    use crate::application::danmaku::xml::{inspect_danmaku_xml, validate_danmaku_xml};

    const CLEAN: &[u8] = br#"<i>
<d p="1,1,25,16777215,0,0,0,0">hello</d>
<d p="2,5,30,16711680,0,0,0,0">top</d>
</i>"#;

    #[test]
    fn a_canonical_sidecar_is_left_untouched() {
        let normalization = normalize_danmaku_xml(CLEAN).expect("clean sidecar");
        assert_eq!(normalization.entries_before, 2);
        assert_eq!(normalization.entries_after, 2);
        assert!(!normalization.changed());
        assert_eq!(normalization.output, None);
    }

    #[test]
    fn invalid_entries_are_dropped_and_counted() {
        let xml = br#"<i>
<d p="1,1,25,16777215">ok</d>
<d p="2,7,25,16777215">advanced</d>
<d p="3,1,25,16777216">bad color</d>
<d p="4,1,25,16777215"></d>
</i>"#;
        let normalization = normalize_danmaku_xml(xml).expect("sidecar");
        assert_eq!(normalization.entries_before, 4);
        assert_eq!(normalization.entries_after, 1);
        let output = normalization.output.expect("dirty sidecar is rewritten");
        assert_eq!(
            String::from_utf8(output.clone()).expect("utf-8"),
            "<i>\n<d p=\"1,1,25,16777215\">ok</d>\n</i>\n"
        );
        assert!(validate_danmaku_xml(&output).is_ok());
        // The rewrite is stable: normalizing the canonical output changes nothing.
        assert!(!normalize_danmaku_xml(&output).expect("output").changed());
    }

    #[test]
    fn font_size_is_clamped_to_the_playback_range() {
        let xml = br#"<i><d p="1,1,1,16777215">small</d><d p="2,1,100,16777215">large</d></i>"#;
        let normalization = normalize_danmaku_xml(xml).expect("sidecar");
        assert_eq!(normalization.entries_before, 2);
        assert_eq!(normalization.entries_after, 2);
        let output = normalization.output.expect("clamped sizes are rewritten");
        assert_eq!(
            String::from_utf8(output.clone()).expect("utf-8"),
            "<i>\n<d p=\"1,1,12,16777215\">small</d>\n<d p=\"2,1,64,16777215\">large</d>\n</i>\n"
        );
        assert_eq!(
            inspect_danmaku_xml(&output).expect("valid").valid_entries,
            2
        );
    }

    #[test]
    fn text_is_trimmed_and_entities_are_re_escaped() {
        let xml = br#"<i><d p="1,1,25,16777215">  a &amp; b  </d></i>"#;
        let normalization = normalize_danmaku_xml(xml).expect("sidecar");
        let output = normalization.output.expect("trimmed text is rewritten");
        assert_eq!(
            String::from_utf8(output.clone()).expect("utf-8"),
            "<i>\n<d p=\"1,1,25,16777215\">a &amp; b</d>\n</i>\n"
        );
        assert_eq!(normalize_danmaku_xml(&output).expect("stable").output, None);
    }

    #[test]
    fn a_sidecar_without_a_valid_entry_reports_zero_without_output() {
        let xml = br#"<i><d p="1,7,25,0">advanced</d><d p="2,1,25,0"></d></i>"#;
        let normalization = normalize_danmaku_xml(xml).expect("structurally valid");
        assert_eq!(normalization.entries_before, 2);
        assert_eq!(normalization.entries_after, 0);
        assert!(!normalization.changed());
    }
}
