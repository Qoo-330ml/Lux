#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentRatingSystem {
    Mpa,
    TvParentalGuidelines,
    Bbfc,
}

impl ContentRatingSystem {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Mpa => "MPA",
            Self::TvParentalGuidelines => "TVPG",
            Self::Bbfc => "BBFC",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        if key.eq_ignore_ascii_case("MPA") {
            Some(Self::Mpa)
        } else if key.eq_ignore_ascii_case("TVPG") {
            Some(Self::TvParentalGuidelines)
        } else if key.eq_ignore_ascii_case("BBFC") {
            Some(Self::Bbfc)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy)]
struct RatingLevel {
    code: &'static str,
    order: u8,
}

fn levels(system: ContentRatingSystem) -> &'static [RatingLevel] {
    const MPA: &[RatingLevel] = &[
        RatingLevel {
            code: "G",
            order: 0,
        },
        RatingLevel {
            code: "PG",
            order: 1,
        },
        RatingLevel {
            code: "PG-13",
            order: 2,
        },
        RatingLevel {
            code: "R",
            order: 3,
        },
        RatingLevel {
            code: "NC-17",
            order: 4,
        },
    ];
    const TVPG: &[RatingLevel] = &[
        RatingLevel {
            code: "TV-Y",
            order: 0,
        },
        RatingLevel {
            code: "TV-G",
            order: 1,
        },
        RatingLevel {
            code: "TV-Y7",
            order: 2,
        },
        RatingLevel {
            code: "TV-PG",
            order: 3,
        },
        RatingLevel {
            code: "TV-14",
            order: 4,
        },
        RatingLevel {
            code: "TV-MA",
            order: 5,
        },
    ];
    const BBFC: &[RatingLevel] = &[
        RatingLevel {
            code: "U",
            order: 0,
        },
        RatingLevel {
            code: "PG",
            order: 1,
        },
        RatingLevel {
            code: "12",
            order: 2,
        },
        RatingLevel {
            code: "12A",
            order: 2,
        },
        RatingLevel {
            code: "15",
            order: 3,
        },
        RatingLevel {
            code: "18",
            order: 4,
        },
        RatingLevel {
            code: "R18",
            order: 5,
        },
    ];

    match system {
        ContentRatingSystem::Mpa => MPA,
        ContentRatingSystem::TvParentalGuidelines => TVPG,
        ContentRatingSystem::Bbfc => BBFC,
    }
}

pub fn normalize_rating_code(
    system: ContentRatingSystem,
    certification: &str,
) -> Option<&'static str> {
    let mut value = certification.trim();
    if value
        .get(.."Rated ".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Rated "))
    {
        value = value["Rated ".len()..].trim();
    }
    let normalized = value.to_ascii_uppercase();
    levels(system)
        .iter()
        .find(|level| level.code == normalized)
        .map(|level| level.code)
}

pub fn rating_is_at_or_below(
    system: ContentRatingSystem,
    certification: &str,
    maximum_rating: &str,
) -> Option<bool> {
    let certification = normalize_rating_code(system, certification)?;
    let maximum_rating = normalize_rating_code(system, maximum_rating)?;
    let levels = levels(system);
    let certification_order = levels
        .iter()
        .find(|level| level.code == certification)?
        .order;
    let maximum_order = levels
        .iter()
        .find(|level| level.code == maximum_rating)?
        .order;
    Some(certification_order <= maximum_order)
}

#[cfg(test)]
mod tests {
    use super::{ContentRatingSystem, normalize_rating_code, rating_is_at_or_below};

    #[test]
    fn rating_system_keys_are_stable_and_case_insensitive() {
        assert_eq!(
            ContentRatingSystem::from_key("MPA"),
            Some(ContentRatingSystem::Mpa)
        );
        assert_eq!(
            ContentRatingSystem::from_key("tvpg"),
            Some(ContentRatingSystem::TvParentalGuidelines)
        );
        assert_eq!(
            ContentRatingSystem::from_key("BBFC"),
            Some(ContentRatingSystem::Bbfc)
        );
        assert_eq!(ContentRatingSystem::from_key("unknown"), None);
        assert_eq!(ContentRatingSystem::Mpa.key(), "MPA");
        assert_eq!(ContentRatingSystem::TvParentalGuidelines.key(), "TVPG");
        assert_eq!(ContentRatingSystem::Bbfc.key(), "BBFC");
    }

    #[test]
    fn certification_codes_are_normalized_only_within_the_selected_system() {
        assert_eq!(
            normalize_rating_code(ContentRatingSystem::Mpa, " Rated pg-13 "),
            Some("PG-13")
        );
        assert_eq!(
            normalize_rating_code(ContentRatingSystem::TvParentalGuidelines, "tv-14"),
            Some("TV-14")
        );
        assert_eq!(
            normalize_rating_code(ContentRatingSystem::Bbfc, "12a"),
            Some("12A")
        );
        assert_eq!(
            normalize_rating_code(ContentRatingSystem::Mpa, "TV-14"),
            None
        );
        assert_eq!(
            normalize_rating_code(ContentRatingSystem::TvParentalGuidelines, "PG-13"),
            None
        );
        assert_eq!(
            normalize_rating_code(ContentRatingSystem::Bbfc, "Rated 21"),
            None
        );
        assert_eq!(normalize_rating_code(ContentRatingSystem::Mpa, "7.8"), None);
        assert_eq!(
            normalize_rating_code(ContentRatingSystem::Mpa, "US:PG-13"),
            None
        );
        assert_eq!(normalize_rating_code(ContentRatingSystem::Mpa, ""), None);
    }

    #[test]
    fn rating_maximum_is_inclusive_and_unknown_values_stay_unrated() {
        assert_eq!(
            rating_is_at_or_below(ContentRatingSystem::Mpa, "PG-13", "PG-13"),
            Some(true)
        );
        assert_eq!(
            rating_is_at_or_below(ContentRatingSystem::Mpa, "R", "PG-13"),
            Some(false)
        );
        assert_eq!(
            rating_is_at_or_below(ContentRatingSystem::TvParentalGuidelines, "TV-Y7", "TV-G"),
            Some(false)
        );
        assert_eq!(
            rating_is_at_or_below(ContentRatingSystem::Bbfc, "12A", "12"),
            Some(true)
        );
        assert_eq!(
            rating_is_at_or_below(ContentRatingSystem::Bbfc, "R18", "18"),
            Some(false)
        );
        assert_eq!(
            rating_is_at_or_below(ContentRatingSystem::Mpa, "unrated", "R"),
            None
        );
        assert_eq!(
            rating_is_at_or_below(ContentRatingSystem::Mpa, "PG", "unrated"),
            None
        );
    }
}
