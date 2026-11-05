//! Validation of the untrusted `ratings.batch` response.
//!
//! The ratings plugin runs as a separate process and its payload comes from an external provider,
//! so nothing it returns is trusted: only the documented fields are read, every number is checked
//! against the range the NFO writer accepts, and a payload that fails any check is dropped instead
//! of being repaired. A dropped payload never writes a sidecar.
//!
//! The container order is preserved exactly as the plugin sent it. Jellyfin keeps the last
//! non-`tomato` entry it reads, and the plugin already sorted its primary source last, so
//! reordering here would change which score the server displays.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::application::nfo::{NfoRatingEntry, RatingsNfoPatch};

/// The plugin rejects a batch larger than this, so a larger echo is a protocol violation.
pub(crate) const MAX_BATCH_ITEMS: usize = 200;
/// Entries one `<ratings>` container may carry, matching the NFO writer's limit.
const MAX_RATING_ENTRIES: usize = 64;
/// Longest provider label accepted for one container entry.
const MAX_ENTRY_NAME_CHARS: usize = 64;
/// Scores are normalized to this scale before an NFO is written.
const MAX_SCORE: f64 = 10.0;
/// A vote count above this is not a real tally and would be a parsing artefact.
const MAX_VOTES: f64 = 1e15;

/// How one item of a batch failed, from the plugin's stable error code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ItemFailure {
    /// The provider has no such title: skip the item without retrying.
    NotFound,
    /// Rate limited: retry the item after the rest of the batch finished.
    RateLimited,
    /// A transient upstream failure: retry the item after the rest of the batch finished.
    Upstream,
    /// The provider rejected the request itself: retrying cannot help.
    Invalid,
    /// An error code this host does not know: count it as a failure, never retry blindly.
    Unknown,
    /// Credentials are missing or rejected: the whole run has to stop.
    Fatal,
}

/// Maps a plugin error code to the host reaction.
pub(crate) fn classify_error_code(code: &str) -> ItemFailure {
    match code.trim().to_ascii_uppercase().as_str() {
        "TRAKT_NOT_FOUND" => ItemFailure::NotFound,
        "TRAKT_RATE_LIMITED" => ItemFailure::RateLimited,
        "TRAKT_UPSTREAM_ERROR" => ItemFailure::Upstream,
        "TRAKT_INVALID_REQUEST" => ItemFailure::Invalid,
        "TRAKT_UNAUTHORIZED" | "TRAKT_NOT_CONFIGURED" => ItemFailure::Fatal,
        _ => ItemFailure::Unknown,
    }
}

/// One entry of the response `results` array.
#[derive(Clone, Debug)]
pub(crate) struct BatchItem {
    /// The request element the plugin echoed back.
    pub(crate) input: Value,
    /// The provider payload, or why this item failed.
    pub(crate) outcome: Result<Value, ItemFailure>,
}

/// Reads the response envelope.
///
/// Returns `None` when the payload does not match the protocol at all; the caller then stops the
/// run instead of guessing which item a broken entry belonged to.
pub(crate) fn parse_batch_response(value: &Value, sent: usize) -> Option<Vec<BatchItem>> {
    let object = value.as_object()?;
    let results = object.get("results")?.as_array()?;
    if results.len() > MAX_BATCH_ITEMS || results.len() > sent {
        return None;
    }
    let mut items = Vec::with_capacity(results.len());
    for entry in results {
        let entry = entry.as_object()?;
        let input = entry.get("input")?.clone();
        let ok = entry.get("ok")?.as_bool()?;
        let outcome = if ok {
            Ok(entry.get("result")?.clone())
        } else {
            let code = entry
                .get("error")?
                .as_object()?
                .get("code")?
                .as_str()?
                .to_owned();
            Err(classify_error_code(&code))
        };
        items.push(BatchItem { input, outcome });
    }
    Some(items)
}

/// What the host may do with one successful provider payload.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MatchOutcome {
    /// A rating patch the writer accepts.
    Patch(RatingsNfoPatch),
    /// The provider matched, but not confidently enough to overwrite a sidecar.
    NotConfident,
    /// The payload claims confidence but carries a value outside the accepted ranges.
    Invalid,
}

/// Whether the provider's match is trustworthy enough to reuse its identifiers.
///
/// A title-only match or a match the plugin itself flagged as low confidence is never trusted,
/// even when its payload claims confidence.
pub(crate) fn is_trusted_match(result: &Value) -> bool {
    let Some(object) = result.as_object() else {
        return false;
    };
    if object
        .get("lowConfidence")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    !object
        .get("matchedBy")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .eq_ignore_ascii_case("title+year")
}

/// Turns one successful `ratings.batch` result into a rating patch, or explains why not.
pub(crate) fn classify_match(result: &Value) -> MatchOutcome {
    let Some(object) = result.as_object() else {
        return MatchOutcome::Invalid;
    };
    if !is_trusted_match(result) {
        return MatchOutcome::NotConfident;
    }
    let Some(writeback) = object.get("writeback").and_then(Value::as_object) else {
        return MatchOutcome::NotConfident;
    };
    if writeback.get("confident").and_then(Value::as_bool) != Some(true) {
        return MatchOutcome::NotConfident;
    }
    let Some(nfo) = writeback.get("nfo").and_then(Value::as_object) else {
        // `writeback.nfo == null` means the provider had nothing to write.
        return MatchOutcome::NotConfident;
    };
    match patch_from_nfo(nfo) {
        Some(patch) => MatchOutcome::Patch(patch),
        None => MatchOutcome::Invalid,
    }
}

/// The numeric Trakt id of a trusted lookup, for the episode queries of the same series.
pub(crate) fn trakt_id_of(result: &Value) -> Option<&Value> {
    result.as_object()?.get("traktId")
}

fn patch_from_nfo(nfo: &serde_json::Map<String, Value>) -> Option<RatingsNfoPatch> {
    let entries = match nfo.get("ratings") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            if items.len() > MAX_RATING_ENTRIES {
                return None;
            }
            let mut entries = Vec::with_capacity(items.len());
            for item in items {
                entries.push(rating_entry(item)?);
            }
            entries
        }
        Some(_) => return None,
    };
    let rating = score(nfo.get("rating")?)?;
    let votes = optional_votes(nfo.get("votes"))?;
    let critic_rating = match nfo.get("criticRating") {
        None | Some(Value::Null) => None,
        Some(value) => Some(score(value)?),
    };
    Some(RatingsNfoPatch {
        entries,
        rating,
        votes,
        critic_rating,
    })
}

fn rating_entry(value: &Value) -> Option<NfoRatingEntry> {
    let object = value.as_object()?;
    let name = object.get("name")?.as_str()?.trim();
    if name.is_empty()
        || name.chars().count() > MAX_ENTRY_NAME_CHARS
        || !name.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
    {
        return None;
    }
    let value = score(object.get("value")?)?;
    let max = object.get("max")?.as_f64()?;
    if max != MAX_SCORE {
        return None;
    }
    let votes = optional_votes(object.get("votes"))?;
    Some(NfoRatingEntry {
        name: name.to_owned(),
        value,
        votes,
        // `max` was just checked to equal the only scale the writer accepts.
        max: MAX_SCORE as u32,
        is_default: object
            .get("default")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn score(value: &Value) -> Option<f64> {
    let value = value.as_f64()?;
    valid_score(value).then_some(value)
}

fn valid_score(value: f64) -> bool {
    value.is_finite() && (0.0..=MAX_SCORE).contains(&value)
}

fn optional_votes(value: Option<&Value>) -> Option<Option<i64>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(value) => Some(Some(vote_count(value)?)),
    }
}

fn vote_count(value: &Value) -> Option<i64> {
    let value = value.as_f64()?;
    if !value.is_finite() || value < 0.0 || value > MAX_VOTES || value.fract() != 0.0 {
        return None;
    }
    Some(value as i64)
}

/// Canonical text for one request element, used to correlate a result with what was asked.
///
/// `serde_json` keeps objects in key order, so the very same lookup always produces the very same
/// string.
pub(crate) fn canonical_json(value: &Value) -> String {
    value.to_string()
}

/// Builds the correlation map from a canonical request element to the batch positions that asked
/// for it. A batch may repeat one lookup for several items.
pub(crate) fn index_by_input(keys: &[String]) -> BTreeMap<&str, Vec<usize>> {
    let mut index = BTreeMap::<&str, Vec<usize>>::new();
    for (position, key) in keys.iter().enumerate() {
        index.entry(key.as_str()).or_default().push(position);
    }
    index
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use super::*;

    fn writeback(nfo: Value) -> Value {
        json!({
            "traktId": 12,
            "matchedBy": "imdb",
            "lowConfidence": false,
            "writeback": {
                "confident": true,
                "source": "trakt",
                "rating": 8.1,
                "votes": 100,
                "criticRating": 7.2,
                "nfo": nfo
            }
        })
    }

    fn trusted_nfo() -> Value {
        json!({
            "ratings": [
                {"name": "tmdb", "value": 7.4, "votes": 1200, "max": 10, "default": false},
                {"name": "trakt", "value": 6.564, "votes": 280, "max": 10, "default": true}
            ],
            "xml": "<irrelevant/>",
            "rating": 6.564,
            "votes": 280,
            "criticRating": 7.2
        })
    }

    fn patch_of(value: Value) -> RatingsNfoPatch {
        match classify_match(&value) {
            MatchOutcome::Patch(patch) => patch,
            other => panic!("expected a patch, got {other:?}"),
        }
    }

    #[test]
    fn classifies_every_documented_error_code() {
        assert_eq!(
            classify_error_code("TRAKT_NOT_FOUND"),
            ItemFailure::NotFound
        );
        assert_eq!(
            classify_error_code(" trakt_rate_limited "),
            ItemFailure::RateLimited
        );
        assert_eq!(
            classify_error_code("TRAKT_UPSTREAM_ERROR"),
            ItemFailure::Upstream
        );
        assert_eq!(
            classify_error_code("TRAKT_INVALID_REQUEST"),
            ItemFailure::Invalid
        );
        assert_eq!(
            classify_error_code("TRAKT_UNAUTHORIZED"),
            ItemFailure::Fatal
        );
        assert_eq!(
            classify_error_code("TRAKT_NOT_CONFIGURED"),
            ItemFailure::Fatal
        );
        assert_eq!(classify_error_code("SOMETHING_ELSE"), ItemFailure::Unknown);
    }

    #[test]
    fn maps_a_confident_writeback_to_a_patch_without_reordering_entries() {
        let patch = patch_of(writeback(trusted_nfo()));

        assert_eq!(patch.rating, 6.564);
        assert_eq!(patch.votes, Some(280));
        assert_eq!(patch.critic_rating, Some(7.2));
        let names = patch
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["tmdb", "trakt"]);
        assert_eq!(patch.entries[1].value, 6.564);
        assert_eq!(patch.entries[1].max, 10);
        assert!(patch.entries[1].is_default);
        assert_eq!(patch.entries[0].votes, Some(1200));
    }

    #[test]
    fn refuses_to_write_without_confidence() {
        let mut not_confident = writeback(trusted_nfo());
        not_confident["writeback"]["confident"] = json!(false);
        assert_eq!(classify_match(&not_confident), MatchOutcome::NotConfident);

        let missing_nfo = json!({
            "matchedBy": "imdb",
            "writeback": {"confident": true, "nfo": null}
        });
        assert_eq!(classify_match(&missing_nfo), MatchOutcome::NotConfident);

        let mut low_confidence = writeback(trusted_nfo());
        low_confidence["lowConfidence"] = json!(true);
        assert_eq!(classify_match(&low_confidence), MatchOutcome::NotConfident);

        let mut title_only = writeback(trusted_nfo());
        title_only["matchedBy"] = json!("title+year");
        assert_eq!(classify_match(&title_only), MatchOutcome::NotConfident);

        let mut no_writeback = writeback(trusted_nfo());
        no_writeback
            .as_object_mut()
            .expect("object")
            .remove("writeback");
        assert_eq!(classify_match(&no_writeback), MatchOutcome::NotConfident);
    }

    #[test]
    fn rejects_hostile_scores_votes_and_names() {
        let mut over_ten = trusted_nfo();
        over_ten["rating"] = json!(10.1);
        assert_eq!(classify_match(&writeback(over_ten)), MatchOutcome::Invalid);

        let mut negative = trusted_nfo();
        negative["criticRating"] = json!(-0.1);
        assert_eq!(classify_match(&writeback(negative)), MatchOutcome::Invalid);

        assert!(!valid_score(f64::NAN), "NaN must never reach a patch");
        assert!(!valid_score(f64::INFINITY));
        assert!(!valid_score(-1.0));
        assert!(!valid_score(10.0001));
        assert!(valid_score(0.0));
        assert!(valid_score(10.0));

        let mut negative_votes = trusted_nfo();
        negative_votes["votes"] = json!(-3);
        assert_eq!(
            classify_match(&writeback(negative_votes)),
            MatchOutcome::Invalid
        );

        let mut fractional_votes = trusted_nfo();
        fractional_votes["votes"] = json!(1.5);
        assert_eq!(
            classify_match(&writeback(fractional_votes)),
            MatchOutcome::Invalid
        );

        let mut long_name = trusted_nfo();
        long_name["ratings"][0]["name"] = json!("a".repeat(65));
        assert_eq!(classify_match(&writeback(long_name)), MatchOutcome::Invalid);

        let mut bad_name = trusted_nfo();
        bad_name["ratings"][0]["name"] = json!("Trakt & Co");
        assert_eq!(classify_match(&writeback(bad_name)), MatchOutcome::Invalid);

        let mut wrong_scale = trusted_nfo();
        wrong_scale["ratings"][0]["max"] = json!(5);
        assert_eq!(
            classify_match(&writeback(wrong_scale)),
            MatchOutcome::Invalid
        );

        let mut entry_over_ten = trusted_nfo();
        entry_over_ten["ratings"][0]["value"] = json!(11);
        assert_eq!(
            classify_match(&writeback(entry_over_ten)),
            MatchOutcome::Invalid
        );

        let mut entry_negative_votes = trusted_nfo();
        entry_negative_votes["ratings"][0]["votes"] = json!(-1);
        assert_eq!(
            classify_match(&writeback(entry_negative_votes)),
            MatchOutcome::Invalid
        );

        let mut too_many = trusted_nfo();
        too_many["ratings"] = Value::Array(
            (0..=MAX_RATING_ENTRIES)
                .map(|index| json!({"name": format!("p{index}"), "value": 1.0, "max": 10}))
                .collect(),
        );
        assert_eq!(classify_match(&writeback(too_many)), MatchOutcome::Invalid);
    }

    #[test]
    fn accepts_an_empty_container_and_a_missing_vote_count() {
        let patch = patch_of(writeback(json!({"ratings": [], "rating": 8.5})));
        assert!(patch.entries.is_empty());
        assert_eq!(patch.rating, 8.5);
        assert_eq!(patch.votes, None);
        assert_eq!(patch.critic_rating, None);

        let patch = patch_of(writeback(json!({"rating": 8.5, "votes": 12.0})));
        assert_eq!(
            patch.votes,
            Some(12),
            "an integral float vote count is fine"
        );
    }

    #[test]
    fn reads_the_response_envelope_and_rejects_broken_shapes() {
        let response = json!({
            "count": 2,
            "ok": 1,
            "failed": 1,
            "results": [
                {"ok": true, "input": {"kind": "movie", "imdb": "tt1"}, "result": {"matchedBy": "imdb"}},
                {"ok": false, "input": {"kind": "movie", "imdb": "tt2"},
                 "error": {"code": "TRAKT_NOT_FOUND", "message": "nope"}}
            ]
        });
        let items = parse_batch_response(&response, 2).expect("valid envelope");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].input, json!({"kind": "movie", "imdb": "tt1"}));
        assert!(items[0].outcome.is_ok());
        assert_eq!(items[1].outcome, Err(ItemFailure::NotFound));

        assert!(parse_batch_response(&json!({"results": []}), 0).is_some());
        assert!(
            parse_batch_response(&json!({"results": []}), 1).is_some(),
            "a missing result is a failed item, not a broken envelope"
        );
        assert!(parse_batch_response(&json!({"results": [{}]}), 1).is_none());
        assert!(parse_batch_response(&json!({}), 1).is_none());
        assert!(parse_batch_response(&json!([]), 1).is_none());
        assert!(
            parse_batch_response(
                &json!({"results": [{"ok": true, "input": 1, "result": {}}]}),
                0
            )
            .is_none(),
            "more results than requests is a protocol violation"
        );
        let oversized: Vec<Value> = (0..=MAX_BATCH_ITEMS)
            .map(|_| json!({"ok": true, "input": {}, "result": {}}))
            .collect();
        assert!(parse_batch_response(&json!({"results": oversized}), 500).is_none());
    }

    #[test]
    fn correlates_results_by_their_echoed_input() {
        let keys = vec![
            r#"{"kind":"movie","imdb":"tt1"}"#.to_owned(),
            r#"{"kind":"show","imdb":"tt2"}"#.to_owned(),
            r#"{"kind":"movie","imdb":"tt1"}"#.to_owned(),
        ];
        let index = index_by_input(&keys);
        assert_eq!(
            index.get(r#"{"kind":"movie","imdb":"tt1"}"#),
            Some(&vec![0, 2])
        );
        assert_eq!(index.get(r#"{"kind":"show","imdb":"tt2"}"#), Some(&vec![1]));
        assert_eq!(index.get(r#"{"kind":"movie","imdb":"tt9"}"#), None);

        let mut object = Map::new();
        object.insert("kind".to_owned(), json!("movie"));
        object.insert("imdb".to_owned(), json!("tt1"));
        assert_eq!(
            canonical_json(&Value::Object(object)),
            r#"{"imdb":"tt1","kind":"movie"}"#
        );
    }
}
