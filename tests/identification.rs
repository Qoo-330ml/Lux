use luxd::application::{
    identification::{IdentificationDecision, IdentificationStatus, MovieIdentity, identify_movie},
    tmdb::TmdbMovieSummary,
};

fn candidate(id: i64, title: &str, year: Option<&str>) -> TmdbMovieSummary {
    TmdbMovieSummary {
        id,
        title: Some(title.to_owned()),
        original_title: None,
        overview: Some("stub".to_owned()),
        release_date: year.map(str::to_owned),
        original_language: Some("en".to_owned()),
    }
}

#[test]
fn identification_table_is_conservative_and_deterministic() {
    struct Case {
        name: &'static str,
        identity: MovieIdentity,
        candidates: Vec<TmdbMovieSummary>,
        expected_status: IdentificationStatus,
        expected_id: Option<i64>,
    }

    let cases = [
        Case {
            name: "provider id is exact",
            identity: MovieIdentity {
                title: "Different local title".to_owned(),
                year: None,
                provider_id: Some(7),
            },
            candidates: vec![candidate(7, "TMDb title", Some("1999-01-01"))],
            expected_status: IdentificationStatus::Confirmed,
            expected_id: Some(7),
        },
        Case {
            name: "title and year are high confidence",
            identity: MovieIdentity {
                title: "The Matrix".to_owned(),
                year: Some(1999),
                provider_id: None,
            },
            candidates: vec![candidate(603, "The Matrix", Some("1999-03-30"))],
            expected_status: IdentificationStatus::Confirmed,
            expected_id: Some(603),
        },
        Case {
            name: "normalized chinese title matches",
            identity: MovieIdentity {
                title: "流浪地球".to_owned(),
                year: Some(2019),
                provider_id: None,
            },
            candidates: vec![candidate(535167, "流浪地球", Some("2019-02-05"))],
            expected_status: IdentificationStatus::Confirmed,
            expected_id: Some(535167),
        },
        Case {
            name: "same title without year is pending",
            identity: MovieIdentity {
                title: "Dune".to_owned(),
                year: None,
                provider_id: None,
            },
            candidates: vec![
                candidate(438631, "Dune", Some("2021-09-03")),
                candidate(841, "Dune", Some("1984-12-14")),
            ],
            expected_status: IdentificationStatus::Pending,
            expected_id: None,
        },
        Case {
            name: "close title is pending even with year",
            identity: MovieIdentity {
                title: "The Matrix Reload".to_owned(),
                year: Some(1999),
                provider_id: None,
            },
            candidates: vec![candidate(603, "The Matrix", Some("1999-03-30"))],
            expected_status: IdentificationStatus::Pending,
            expected_id: None,
        },
        Case {
            name: "exact title without year is pending",
            identity: MovieIdentity {
                title: "Alien".to_owned(),
                year: None,
                provider_id: None,
            },
            candidates: vec![candidate(348, "Alien", Some("1979-05-25"))],
            expected_status: IdentificationStatus::Pending,
            expected_id: None,
        },
    ];

    for case in cases {
        let decision = identify_movie(&case.identity, &case.candidates);
        assert_eq!(decision.status, case.expected_status, "{}", case.name);
        assert_eq!(
            decision.candidate.as_ref().map(|candidate| candidate.id),
            case.expected_id,
            "{}",
            case.name
        );
        if case.expected_status == IdentificationStatus::Pending {
            assert!(matches!(
                decision,
                IdentificationDecision {
                    candidate: None,
                    ..
                }
            ));
        }
    }
}
