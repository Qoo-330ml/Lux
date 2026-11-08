//! Decoding coverage for the `ratings.*` protocol owned by `org.lux.trakt-ratings`.
//!
//! The plugin lives in the `Lux-plugins` repository, so the fixtures here are hand-built JSON
//! objects that mirror the documented contract. Nothing in this module opens a socket, reads a
//! credential or touches a real Trakt URL: the point is that the host can decode what the plugin
//! is allowed to send, and that an unknown field never turns a usable answer into an error.

use luxd::application::plugin_protocol::{
    MAX_RATINGS_BATCH_ITEMS, RATINGS_BATCH_CAPABILITY, RATINGS_ERROR_NOT_CONFIGURED,
    RATINGS_ERROR_NOT_FOUND, RATINGS_ERROR_RATE_LIMITED, RATINGS_ERROR_UNAUTHORIZED,
    RATINGS_LOOKUP_CAPABILITY, RATINGS_SEARCH_CAPABILITY, RatingsBatchRpcRequest,
    RatingsBatchRpcResponse, RatingsLookupKind, RatingsLookupRpcRequest, RatingsRpcValidationError,
    RatingsSearchRpcResponse,
};
use serde_json::{Value, json};

/// One successful `ratings.batch` member, with the fields the plugin documents.
fn confident_batch() -> Value {
    json!({
        "results": [{
            "input": {"kind": "movie", "imdb": "tt0111161"},
            "ok": true,
            "result": {
                "traktId": 129,
                "matchedBy": "imdb",
                "lowConfidence": false,
                "minVotes": 10,
                "primary": {"source": "trakt", "rating": 8.7, "votes": 12345},
                "stats": {
                    "rating": 8.7,
                    "votes": 12345,
                    "watchers": 30000,
                    "plays": 40000,
                    "collectors": 5000,
                    "comments": 120
                },
                "distribution": {"1": 10, "5": 400, "10": 9000},
                "sources": [
                    {"source": "imdb", "rating": 9.3, "votes": 2500000},
                    {"source": "trakt", "rating": 8.7, "votes": 12345}
                ],
                "writeback": {
                    "confident": true,
                    "source": "trakt",
                    "rating": 8.7,
                    "votes": 12345,
                    "criticRating": 82.0
                }
            }
        }]
    })
}

#[test]
fn decodes_a_confident_batch_member() {
    let response: RatingsBatchRpcResponse =
        serde_json::from_value(confident_batch()).expect("confident batch should decode");
    assert_eq!(response.results.len(), 1);
    let entry = &response.results[0];
    assert!(entry.ok);
    assert_eq!(entry.input, json!({"kind": "movie", "imdb": "tt0111161"}));
    assert!(entry.error.is_none());

    let result = entry
        .result
        .as_ref()
        .expect("successful member has a result");
    assert_eq!(result.trakt_id, Some(129));
    assert_eq!(result.matched_by.as_deref(), Some("imdb"));
    assert!(!result.low_confidence);
    assert_eq!(result.min_votes, Some(10));
    assert_eq!(result.distribution.get("10"), Some(&9000));
    assert_eq!(result.sources.len(), 2);
    assert_eq!(result.sources[0].source.as_deref(), Some("imdb"));
    assert_eq!(
        result.primary.as_ref().and_then(|p| p.source.as_deref()),
        Some("trakt")
    );
    assert_eq!(
        result.stats.as_ref().and_then(|stats| stats.votes),
        Some(12345)
    );

    let writeback = result.writeback.as_ref().expect("writeback present");
    assert!(writeback.confident);
    assert_eq!(writeback.source.as_deref(), Some("trakt"));
    assert_eq!(writeback.rating, Some(8.7));
    assert_eq!(writeback.votes, Some(12345));
    assert_eq!(writeback.critic_rating, Some(82.0));
}

#[test]
fn a_non_confident_writeback_never_asks_the_host_to_overwrite_a_score() {
    let mut value = confident_batch();
    value["results"][0]["result"]["lowConfidence"] = json!(true);
    value["results"][0]["result"]["matchedBy"] = json!("title+year");
    let writeback = &mut value["results"][0]["result"]["writeback"];
    writeback["confident"] = json!(false);
    // A rejected match may still carry the score the provider guessed; the host must not use it.
    writeback["criticRating"] = Value::Null;

    let response: RatingsBatchRpcResponse =
        serde_json::from_value(value).expect("non confident batch should decode");
    let result = response.results[0]
        .result
        .as_ref()
        .expect("successful member has a result");
    assert!(result.low_confidence);
    let writeback = result.writeback.as_ref().expect("writeback present");
    assert!(!writeback.confident);
    assert_eq!(writeback.rating, Some(8.7));
    assert_eq!(
        writeback.critic_rating, None,
        "a null criticRating decodes to no critic score"
    );
}

#[test]
fn a_missing_writeback_or_critic_rating_decodes_without_a_score() {
    let mut value = confident_batch();
    value["results"][0]["result"]["writeback"] = json!({
        "confident": true,
        "source": "trakt",
        "rating": 8.7,
        "votes": 12345
    });
    let response: RatingsBatchRpcResponse =
        serde_json::from_value(value).expect("writeback without criticRating should decode");
    let writeback = response.results[0]
        .result
        .as_ref()
        .and_then(|result| result.writeback.as_ref())
        .expect("writeback present");
    assert!(writeback.confident);
    assert_eq!(writeback.critic_rating, None);

    let mut value = confident_batch();
    value["results"][0]["result"]
        .as_object_mut()
        .expect("result object")
        .remove("writeback");
    let response: RatingsBatchRpcResponse =
        serde_json::from_value(value).expect("a result without writeback should decode");
    let result = response.results[0]
        .result
        .as_ref()
        .expect("successful member has a result");
    assert!(result.writeback.is_none());
}

#[test]
fn decodes_the_documented_error_codes() {
    for (code, expected) in [
        (RATINGS_ERROR_NOT_FOUND, "TRAKT_NOT_FOUND"),
        (RATINGS_ERROR_RATE_LIMITED, "TRAKT_RATE_LIMITED"),
        (RATINGS_ERROR_UNAUTHORIZED, "TRAKT_UNAUTHORIZED"),
        (RATINGS_ERROR_NOT_CONFIGURED, "TRAKT_NOT_CONFIGURED"),
    ] {
        assert_eq!(code, expected);
        let response: RatingsBatchRpcResponse = serde_json::from_value(json!({
            "results": [{
                "input": {"kind": "show", "imdb": "tt0903747"},
                "ok": false,
                "error": {"code": expected, "message": "upstream refused"}
            }]
        }))
        .expect("failed member should decode");
        let entry = &response.results[0];
        assert!(!entry.ok);
        assert!(entry.result.is_none());
        let error = entry.error.as_ref().expect("failed member has an error");
        assert_eq!(error.code, expected);
        assert_eq!(error.message.as_deref(), Some("upstream refused"));
    }
}

#[test]
fn unknown_fields_are_ignored_at_every_level() {
    let mut value = confident_batch();
    value["protocolVersion"] = json!(2);
    value["results"][0]["retryAfterSeconds"] = json!(0);
    value["results"][0]["result"]["futureScore"] = json!({"tmdb": 8.1});
    value["results"][0]["result"]["writeback"]["futureConfidence"] = json!(0.98);
    value["results"][0]["result"]["sources"][0]["futureLabel"] = json!("IMDb");
    value["results"][0]["result"]["stats"]["futureCounter"] = json!(7);

    let response: RatingsBatchRpcResponse =
        serde_json::from_value(value).expect("unknown fields must be ignored");
    let result = response.results[0]
        .result
        .as_ref()
        .expect("successful member has a result");
    assert_eq!(result.trakt_id, Some(129));
    assert_eq!(result.sources[0].rating, Some(9.3));

    let failed: RatingsBatchRpcResponse = serde_json::from_value(json!({
        "results": [{
            "input": {"kind": "movie", "imdb": "tt1"},
            "ok": false,
            "error": {"code": "TRAKT_NOT_FOUND", "message": "no match", "detail": {"source": "trakt"}}
        }],
        "futureEnvelope": true
    }))
    .expect("unknown error fields must be ignored");
    assert_eq!(
        failed.results[0]
            .error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("TRAKT_NOT_FOUND")
    );
}

#[test]
fn batch_requests_carry_at_most_the_documented_number_of_lookups() {
    let movie = RatingsLookupRpcRequest {
        kind: RatingsLookupKind::Movie,
        imdb: Some("tt0111161".to_owned()),
        ..RatingsLookupRpcRequest::default()
    };
    assert_eq!(
        serde_json::to_value(&movie).expect("lookup should serialize"),
        json!({"kind": "movie", "imdb": "tt0111161"})
    );

    let episode = RatingsLookupRpcRequest {
        kind: RatingsLookupKind::Episode,
        show: Some("tt0903747".to_owned()),
        season: Some(5),
        episode: Some(14),
        ..RatingsLookupRpcRequest::default()
    };
    assert_eq!(
        serde_json::to_value(&episode).expect("lookup should serialize"),
        json!({"kind": "episode", "show": "tt0903747", "season": 5, "episode": 14})
    );

    assert_eq!(
        RatingsBatchRpcRequest { items: Vec::new() }.validate(),
        Err(RatingsRpcValidationError::Empty)
    );
    assert!(
        RatingsBatchRpcRequest {
            items: vec![movie.clone(); MAX_RATINGS_BATCH_ITEMS],
        }
        .validate()
        .is_ok()
    );
    assert_eq!(
        RatingsBatchRpcRequest {
            items: vec![movie; MAX_RATINGS_BATCH_ITEMS + 1],
        }
        .validate(),
        Err(RatingsRpcValidationError::TooLarge)
    );
}

#[test]
fn decodes_a_search_response_and_exposes_the_capability_names() {
    let response: RatingsSearchRpcResponse = serde_json::from_value(json!({
        "items": [
            {"traktId": 129, "title": "The Shawshank Redemption", "year": 1994, "kind": "movie", "score": 0.98},
            {"traktId": 130, "title": "A different film", "year": 1995}
        ],
        "futureField": null
    }))
    .expect("search response should decode");
    assert_eq!(response.items.len(), 2);
    assert_eq!(response.items[0].trakt_id, Some(129));
    assert_eq!(response.items[0].score, Some(0.98));
    assert_eq!(response.items[1].score, None);

    assert_eq!(RATINGS_LOOKUP_CAPABILITY, "ratings.lookup");
    assert_eq!(RATINGS_BATCH_CAPABILITY, "ratings.batch");
    assert_eq!(RATINGS_SEARCH_CAPABILITY, "ratings.search");
}
