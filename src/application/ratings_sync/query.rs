//! Upstream lookup construction for the ratings sync.
//!
//! Everything here is pure: the caller hands over the identifiers Lux stored for an item and gets
//! back the exact `ratings.batch` element to send, or `None` when the item has no identifier the
//! provider accepts. Keeping it pure means the "never guess from a title alone" rules are unit
//! testable without a plugin process or a database.

use serde_json::{Value, json};

/// Longest identifier string the host forwards to a provider.
const MAX_ID_CHARS: usize = 64;
/// Accepted production-year window for a title lookup.
const MIN_TITLE_YEAR: i64 = 1870;
const MAX_TITLE_YEAR: i64 = 2200;
/// Upper bound for a season/episode number, matching the chapter lookup bounds.
const MAX_SEASON_NUMBER: i64 = 1_000;
const MAX_EPISODE_NUMBER: i64 = 10_000;

/// The identifier subset of `media_items.provider_ids_json` this sync understands.
///
/// Provider ids are stored as a JSON object whose values may be strings or integers, so both are
/// read; anything else is ignored rather than repaired.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ProviderIds {
    pub(crate) imdb: Option<String>,
    pub(crate) tmdb: Option<String>,
}

/// Parses `provider_ids_json`, keeping only the ids the provider resolves.
pub(crate) fn parse_provider_ids(raw: Option<&str>) -> ProviderIds {
    let Some(raw) = raw else {
        return ProviderIds::default();
    };
    let Ok(object) = serde_json::from_str::<serde_json::Map<String, Value>>(raw) else {
        return ProviderIds::default();
    };
    let mut ids = ProviderIds::default();
    for (name, value) in object {
        let value = match value {
            Value::String(value) => value,
            Value::Number(value) => value.to_string(),
            _ => continue,
        };
        if value.trim().is_empty() {
            continue;
        }
        match name.trim().to_ascii_lowercase().as_str() {
            "imdb" | "imdbid" => {
                if let Some(id) = normalize_imdb(&value) {
                    ids.imdb = Some(id);
                }
            }
            "tmdb" | "tmdbid" => {
                if let Some(id) = normalize_tmdb(&value) {
                    ids.tmdb = Some(id);
                }
            }
            _ => {}
        }
    }
    ids
}

/// What a series lookup learned about one series, cached for its episodes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SeriesLookup {
    pub(crate) imdb: Option<String>,
    pub(crate) trakt_id: Option<String>,
}

impl SeriesLookup {
    /// Builds the series identity from the stored provider ids.
    pub(crate) fn from_provider_ids(ids: &ProviderIds) -> Self {
        Self {
            imdb: ids.imdb.clone(),
            trakt_id: None,
        }
    }

    /// Remembers the numeric Trakt id a trustworthy series lookup returned.
    pub(crate) fn remember_trakt_id(&mut self, value: &Value) {
        if let Some(id) = normalize_trakt_id(value) {
            self.trakt_id = Some(id);
        }
    }
}

/// One element of the `ratings.batch` request.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RatingsQuery {
    MovieById {
        imdb: String,
    },
    MovieByTitle {
        title: String,
        tmdb: String,
        year: i64,
    },
    ShowById {
        imdb: String,
    },
    ShowByTitle {
        title: String,
        tmdb: String,
        year: i64,
    },
    Episode {
        show: String,
        season: i64,
        episode: i64,
    },
}

impl RatingsQuery {
    /// The JSON object the provider expects for this lookup.
    pub(crate) fn to_json(&self) -> Value {
        match self {
            Self::MovieById { imdb } => json!({ "kind": "movie", "imdb": imdb }),
            Self::MovieByTitle { title, tmdb, year } => {
                json!({ "kind": "movie", "title": title, "tmdb": tmdb, "year": year })
            }
            Self::ShowById { imdb } => json!({ "kind": "show", "imdb": imdb }),
            Self::ShowByTitle { title, tmdb, year } => {
                json!({ "kind": "show", "title": title, "tmdb": tmdb, "year": year })
            }
            Self::Episode {
                show,
                season,
                episode,
            } => json!({
                "kind": "episode",
                "show": show,
                "season": season,
                "episode": episode,
            }),
        }
    }
}

/// Builds the movie lookup, preferring the IMDb id and falling back to TMDb plus a year.
///
/// A bare title is never enough: without TMDb the provider can only guess, and the sync would then
/// write a rating that belongs to a different film.
pub(crate) fn movie_query(
    title: &str,
    ids: &ProviderIds,
    year: Option<i64>,
) -> Option<RatingsQuery> {
    if let Some(imdb) = ids.imdb.clone() {
        return Some(RatingsQuery::MovieById { imdb });
    }
    let (title, tmdb, year) = title_lookup(title, ids, year)?;
    Some(RatingsQuery::MovieByTitle { title, tmdb, year })
}

/// Builds the series lookup with the same rules as [`movie_query`].
///
/// Trakt only accepts a numeric id or an IMDb id for a `show`, so a TMDb-only series is looked up
/// by title and TMDb, and its episodes then reuse the Trakt id that lookup returned.
pub(crate) fn show_query(
    title: &str,
    ids: &ProviderIds,
    year: Option<i64>,
) -> Option<RatingsQuery> {
    if let Some(imdb) = ids.imdb.clone() {
        return Some(RatingsQuery::ShowById { imdb });
    }
    let (title, tmdb, year) = title_lookup(title, ids, year)?;
    Some(RatingsQuery::ShowByTitle { title, tmdb, year })
}

/// Builds the episode lookup from the series identity and the stored season/episode numbers.
pub(crate) fn episode_query(
    series: &SeriesLookup,
    season_number: Option<i64>,
    episode_number: Option<i64>,
) -> Option<RatingsQuery> {
    let show = series.imdb.clone().or_else(|| series.trakt_id.clone())?;
    let season = season_number.filter(|value| (0..=MAX_SEASON_NUMBER).contains(value))?;
    let episode = episode_number.filter(|value| (1..=MAX_EPISODE_NUMBER).contains(value))?;
    Some(RatingsQuery::Episode {
        show,
        season,
        episode,
    })
}

fn title_lookup(
    title: &str,
    ids: &ProviderIds,
    year: Option<i64>,
) -> Option<(String, String, i64)> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 512 {
        return None;
    }
    let tmdb = ids.tmdb.clone()?;
    let year = year.filter(|value| (MIN_TITLE_YEAR..=MAX_TITLE_YEAR).contains(value))?;
    Some((title.to_owned(), tmdb, year))
}

fn normalize_imdb(value: &str) -> Option<String> {
    let value = value.trim();
    let digits = value.strip_prefix("tt")?;
    if value.chars().count() > MAX_ID_CHARS
        || digits.is_empty()
        || !digits.chars().all(|character| character.is_ascii_digit())
    {
        return None;
    }
    Some(value.to_owned())
}

fn normalize_tmdb(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_ID_CHARS
        || !value.chars().all(|character| character.is_ascii_digit())
        || value.chars().all(|character| character == '0')
    {
        return None;
    }
    Some(value.to_owned())
}

/// Normalizes the numeric Trakt id a plugin returned, rejecting anything else.
fn normalize_trakt_id(value: &Value) -> Option<String> {
    let digits = match value {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.trim().to_owned(),
        _ => return None,
    };
    if digits.is_empty()
        || digits.len() > MAX_ID_CHARS
        || !digits.chars().all(|character| character.is_ascii_digit())
        || digits.chars().all(|character| character == '0')
    {
        return None;
    }
    Some(digits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(imdb: Option<&str>, tmdb: Option<&str>) -> ProviderIds {
        ProviderIds {
            imdb: imdb.map(str::to_owned),
            tmdb: tmdb.map(str::to_owned),
        }
    }

    #[test]
    fn reads_string_and_integer_provider_ids() {
        assert_eq!(
            parse_provider_ids(Some(r#"{"imdb":"tt0111161","tmdb":278}"#)),
            ids(Some("tt0111161"), Some("278"))
        );
        assert_eq!(
            parse_provider_ids(Some(r#"{"IMDB":" tt0111161 ","TmdbId":"278"}"#)),
            ids(Some("tt0111161"), Some("278"))
        );
        assert_eq!(
            parse_provider_ids(Some(r#"{"imdb":"not-an-id","tmdb":0,"tvdb":1}"#)),
            ProviderIds::default()
        );
        assert_eq!(parse_provider_ids(Some("not json")), ProviderIds::default());
        assert_eq!(parse_provider_ids(None), ProviderIds::default());
    }

    #[test]
    fn movie_lookup_prefers_imdb_and_never_uses_a_bare_title() {
        assert_eq!(
            movie_query(
                "The Shawshank Redemption",
                &ids(Some("tt0111161"), Some("278")),
                Some(1994)
            ),
            Some(RatingsQuery::MovieById {
                imdb: "tt0111161".to_owned()
            })
        );
        assert_eq!(
            movie_query(
                "The Shawshank Redemption",
                &ids(None, Some("278")),
                Some(1994)
            ),
            Some(RatingsQuery::MovieByTitle {
                title: "The Shawshank Redemption".to_owned(),
                tmdb: "278".to_owned(),
                year: 1994,
            })
        );
        assert_eq!(
            movie_query("Title only", &ids(None, None), Some(1994)),
            None
        );
        assert_eq!(
            movie_query("Title only", &ids(None, Some("278")), None),
            None
        );
        assert_eq!(
            movie_query("   ", &ids(None, Some("278")), Some(1994)),
            None
        );
        assert_eq!(
            movie_query("Old", &ids(None, Some("278")), Some(1200)),
            None
        );
    }

    #[test]
    fn show_lookup_matches_the_movie_rules() {
        assert_eq!(
            show_query("Breaking Bad", &ids(Some("tt0903747"), None), None),
            Some(RatingsQuery::ShowById {
                imdb: "tt0903747".to_owned()
            })
        );
        assert_eq!(
            show_query("Breaking Bad", &ids(None, Some("1396")), Some(2008)),
            Some(RatingsQuery::ShowByTitle {
                title: "Breaking Bad".to_owned(),
                tmdb: "1396".to_owned(),
                year: 2008,
            })
        );
        assert_eq!(
            show_query("Breaking Bad", &ids(None, Some("1396")), None),
            None
        );
    }

    #[test]
    fn episodes_use_the_series_imdb_before_the_cached_trakt_id() {
        let mut series = SeriesLookup {
            imdb: Some("tt0903747".to_owned()),
            trakt_id: Some("1388".to_owned()),
        };
        assert_eq!(
            episode_query(&series, Some(1), Some(2)),
            Some(RatingsQuery::Episode {
                show: "tt0903747".to_owned(),
                season: 1,
                episode: 2,
            })
        );

        series.imdb = None;
        assert_eq!(
            episode_query(&series, Some(5), Some(14)),
            Some(RatingsQuery::Episode {
                show: "1388".to_owned(),
                season: 5,
                episode: 14,
            })
        );

        let unknown = SeriesLookup::default();
        assert_eq!(episode_query(&unknown, Some(1), Some(1)), None);
        assert_eq!(episode_query(&series, None, Some(1)), None);
        assert_eq!(episode_query(&series, Some(1), None), None);
        assert_eq!(episode_query(&series, Some(1), Some(0)), None);
        assert_eq!(episode_query(&series, Some(1001), Some(1)), None);
        assert_eq!(episode_query(&series, Some(1), Some(10_001)), None);
    }

    #[test]
    fn series_lookup_remembers_only_numeric_trakt_ids() {
        let mut series = SeriesLookup::from_provider_ids(&ids(None, Some("1396")));
        assert_eq!(series.imdb, None);
        series.remember_trakt_id(&json!("1388"));
        assert_eq!(series.trakt_id.as_deref(), Some("1388"));
        series.remember_trakt_id(&json!(1396));
        assert_eq!(series.trakt_id.as_deref(), Some("1396"));
        series.remember_trakt_id(&json!("tt0903747"));
        assert_eq!(
            series.trakt_id.as_deref(),
            Some("1396"),
            "a non numeric id must not replace a cached one"
        );
        series.remember_trakt_id(&json!(0));
        assert_eq!(series.trakt_id.as_deref(), Some("1396"));
    }

    #[test]
    fn queries_serialize_to_the_documented_shape() {
        assert_eq!(
            RatingsQuery::MovieById {
                imdb: "tt1".to_owned()
            }
            .to_json(),
            json!({"kind": "movie", "imdb": "tt1"})
        );
        assert_eq!(
            RatingsQuery::MovieByTitle {
                title: "Film".to_owned(),
                tmdb: "7".to_owned(),
                year: 2019,
            }
            .to_json(),
            json!({"kind": "movie", "title": "Film", "tmdb": "7", "year": 2019})
        );
        assert_eq!(
            RatingsQuery::ShowById {
                imdb: "tt2".to_owned()
            }
            .to_json(),
            json!({"kind": "show", "imdb": "tt2"})
        );
        assert_eq!(
            RatingsQuery::ShowByTitle {
                title: "Show".to_owned(),
                tmdb: "8".to_owned(),
                year: 2008,
            }
            .to_json(),
            json!({"kind": "show", "title": "Show", "tmdb": "8", "year": 2008})
        );
        assert_eq!(
            RatingsQuery::Episode {
                show: "tt2".to_owned(),
                season: 1,
                episode: 2,
            }
            .to_json(),
            json!({"kind": "episode", "show": "tt2", "season": 1, "episode": 2})
        );
    }
}
