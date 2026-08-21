use crate::application::{
    scraper::{ScraperGetRequest, ScraperItemType, ScraperSearchRequest, ScraperSearchResult},
    tmdb::{TmdbError, TmdbMovieSummary},
    tmdb_plugin::ScraperProvider,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MovieIdentity {
    pub title: String,
    pub year: Option<i32>,
    pub provider_id: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScraperMovieIdentity {
    pub title: String,
    pub year: Option<i32>,
    pub provider_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentificationStatus {
    Confirmed,
    Pending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentificationReason {
    ProviderIdExact,
    TitleYearExact,
    NoCandidate,
    ProviderIdNotFound,
    Ambiguous,
    LowConfidence,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentificationDecision<C = TmdbMovieSummary> {
    pub status: IdentificationStatus,
    pub candidate: Option<C>,
    pub score: u8,
    pub reason: IdentificationReason,
}

pub trait IdentificationCandidate: Clone {
    fn provider_id(&self) -> Option<String>;
    fn title(&self) -> Option<&str>;
    fn original_title(&self) -> Option<&str>;
    fn production_year(&self) -> Option<i32>;
    fn premiere_date(&self) -> Option<&str>;
}

impl IdentificationCandidate for TmdbMovieSummary {
    fn provider_id(&self) -> Option<String> {
        Some(self.id.to_string())
    }

    fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    fn original_title(&self) -> Option<&str> {
        self.original_title.as_deref()
    }

    fn production_year(&self) -> Option<i32> {
        release_year(self.release_date.as_deref())
    }

    fn premiere_date(&self) -> Option<&str> {
        self.release_date.as_deref()
    }
}

impl IdentificationCandidate for ScraperSearchResult {
    fn provider_id(&self) -> Option<String> {
        self.first_provider_id().map(str::to_owned)
    }

    fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    fn original_title(&self) -> Option<&str> {
        self.original_title.as_deref()
    }

    fn production_year(&self) -> Option<i32> {
        self.production_year
    }

    fn premiere_date(&self) -> Option<&str> {
        self.premiere_date.as_deref()
    }
}

#[derive(Clone)]
pub struct MovieIdentifier {
    client: ScraperProvider,
}

impl MovieIdentifier {
    pub fn new<T>(client: T) -> Self
    where
        T: Into<ScraperProvider>,
    {
        Self {
            client: client.into(),
        }
    }

    pub async fn identify(
        &self,
        identity: &MovieIdentity,
    ) -> Result<IdentificationDecision, TmdbError> {
        let candidates = if let Some(provider_id) = identity.provider_id {
            let details = self.client.movie_details(provider_id, "zh-CN").await?;
            vec![TmdbMovieSummary {
                id: details.id,
                title: details.title,
                original_title: details.original_title,
                overview: details.overview,
                release_date: details.release_date,
                original_language: details.original_language,
                vote_average: details.vote_average,
            }]
        } else {
            self.client
                .search_movies_with_english_fallback(&identity.title, identity.year)
                .await?
                .results
        };
        Ok(identify_movie(identity, &candidates))
    }

    pub async fn identify_scraper(
        &self,
        identity: &ScraperMovieIdentity,
    ) -> Result<IdentificationDecision<ScraperSearchResult>, TmdbError> {
        let candidates = if let Some(provider_id) = &identity.provider_id {
            let details = self
                .client
                .get_generic(ScraperGetRequest::new(
                    ScraperItemType::Movie,
                    provider_id.clone(),
                    "zh-CN",
                ))
                .await
                .map_err(|error| TmdbError::Transport(error.to_string()))?;
            vec![ScraperSearchResult {
                item_type: details.item_type,
                title: details.title,
                original_title: details.original_title,
                overview: details.overview,
                production_year: details.production_year,
                premiere_date: details.premiere_date,
                original_language: details.original_language,
                provider_ids: details.provider_ids,
                ..ScraperSearchResult::default()
            }]
        } else {
            self.client
                .search_generic(ScraperSearchRequest::new(
                    ScraperItemType::Movie,
                    &identity.title,
                    identity.year,
                    "zh-CN",
                ))
                .await
                .map_err(|error| TmdbError::Transport(error.to_string()))?
                .items
        };
        Ok(identify_scraper_movie(identity, &candidates))
    }
}

pub fn identify_movie<C>(identity: &MovieIdentity, candidates: &[C]) -> IdentificationDecision<C>
where
    C: IdentificationCandidate,
{
    identify_with_provider(
        &identity.title,
        identity.year,
        identity
            .provider_id
            .map(|value| value.to_string())
            .as_deref(),
        candidates,
    )
}

pub fn identify_scraper_movie<C>(
    identity: &ScraperMovieIdentity,
    candidates: &[C],
) -> IdentificationDecision<C>
where
    C: IdentificationCandidate,
{
    identify_with_provider(
        &identity.title,
        identity.year,
        identity.provider_id.as_deref(),
        candidates,
    )
}

fn identify_with_provider<C>(
    title: &str,
    year: Option<i32>,
    provider_id: Option<&str>,
    candidates: &[C],
) -> IdentificationDecision<C>
where
    C: IdentificationCandidate,
{
    if let Some(provider_id) = provider_id {
        if let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.provider_id().as_deref() == Some(provider_id))
        {
            return IdentificationDecision {
                status: IdentificationStatus::Confirmed,
                candidate: Some(candidate.clone()),
                score: 100,
                reason: IdentificationReason::ProviderIdExact,
            };
        }
        return pending(IdentificationReason::ProviderIdNotFound);
    }
    if title.trim().is_empty() || candidates.is_empty() {
        return pending(IdentificationReason::NoCandidate);
    }

    let normalized_title = normalize_title(title);
    if normalized_title.is_empty() {
        return pending(IdentificationReason::NoCandidate);
    }
    let mut scored = candidates
        .iter()
        .filter_map(|candidate| {
            let score = score_candidate(&normalized_title, year, candidate);
            (score > 0).then_some((score, candidate))
        })
        .collect::<Vec<_>>();
    if scored.is_empty() {
        return pending(IdentificationReason::LowConfidence);
    }
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.provider_id().cmp(&right.1.provider_id()))
    });
    let (top_score, top_candidate) = scored[0];
    let tied = scored.get(1).is_some_and(|(score, _)| *score == top_score);
    if top_score == 100 && !tied {
        return IdentificationDecision {
            status: IdentificationStatus::Confirmed,
            candidate: Some(top_candidate.clone()),
            score: top_score,
            reason: IdentificationReason::TitleYearExact,
        };
    }
    pending(if tied {
        IdentificationReason::Ambiguous
    } else {
        IdentificationReason::LowConfidence
    })
}

fn score_candidate(
    normalized_title: &str,
    year: Option<i32>,
    candidate: &impl IdentificationCandidate,
) -> u8 {
    let candidate_title = normalize_title(candidate.title().unwrap_or_default());
    let candidate_original = normalize_title(candidate.original_title().unwrap_or_default());
    let title_score =
        if normalized_title == candidate_title || normalized_title == candidate_original {
            80
        } else {
            let similarity = [candidate_title, candidate_original]
                .into_iter()
                .filter(|title| !title.is_empty())
                .map(|title| similarity_percent(normalized_title, &title))
                .max()
                .unwrap_or(0);
            if similarity >= 85 {
                65
            } else if similarity >= 70 {
                45
            } else {
                0
            }
        };
    if title_score == 0 {
        return 0;
    }
    let year_score = match (
        year,
        candidate
            .production_year()
            .or_else(|| release_year(candidate.premiere_date())),
    ) {
        (Some(expected), Some(actual)) if expected == actual => 20,
        (Some(_), Some(_)) => 0,
        _ => 0,
    };
    title_score + year_score
}

pub fn normalize_title(title: &str) -> String {
    title
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn release_year(value: Option<&str>) -> Option<i32> {
    let year = value?.get(..4)?.parse::<i32>().ok()?;
    (1800..=2200).contains(&year).then_some(year)
}

fn similarity_percent(left: &str, right: &str) -> u8 {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let max_length = left.len().max(right.len());
    if max_length == 0 {
        return 100;
    }
    let distance = levenshtein(&left, &right);
    let similarity = 100_usize.saturating_sub(distance.saturating_mul(100) / max_length);
    u8::try_from(similarity).unwrap_or(0)
}

fn levenshtein(left: &[char], right: &[char]) -> usize {
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    for (left_index, left_character) in left.iter().enumerate() {
        let mut current = vec![left_index + 1; right.len() + 1];
        for (right_index, right_character) in right.iter().enumerate() {
            current[right_index + 1] = if left_character == right_character {
                previous[right_index]
            } else {
                1 + previous[right_index]
                    .min(previous[right_index + 1])
                    .min(current[right_index])
            };
        }
        previous = current;
    }
    previous[right.len()]
}

fn pending<C>(reason: IdentificationReason) -> IdentificationDecision<C> {
    IdentificationDecision {
        status: IdentificationStatus::Pending,
        candidate: None,
        score: 0,
        reason,
    }
}
