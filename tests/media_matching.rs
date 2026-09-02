use luxd::application::media_matching::{MediaKind, parse_media_name, title_candidates};

#[test]
fn parses_adjacent_year_from_series_directory_name() {
    let parsed = parse_media_name("暗夜与黎明2024", MediaKind::Series).expect("series name");

    assert_eq!(parsed.title, "暗夜与黎明");
    assert_eq!(parsed.production_year, Some(2024));
    assert_eq!(parsed.season, None);
    assert_eq!(parsed.episode, None);
}

#[test]
fn removes_episode_release_noise_and_keeps_sequence_numbers() {
    let parsed = parse_media_name(
        "暗夜与黎明 S01E01 H 265 AAC CHDWEB.strm",
        MediaKind::Episode,
    )
    .expect("episode name");

    assert_eq!(parsed.title, "暗夜与黎明");
    assert_eq!(parsed.season, Some(1));
    assert_eq!(parsed.episode, Some(1));
    assert!(!parsed.title.contains("265"));
    assert!(!parsed.title.contains("AAC"));
    assert!(!parsed.title.contains("CHDWEB"));
}

#[test]
fn parses_ascii_and_chinese_episode_markers() {
    let ascii =
        parse_media_name("Show 2x07 1080p WEB-DL.mkv", MediaKind::Episode).expect("ascii episode");
    assert_eq!((ascii.season, ascii.episode), (Some(2), Some(7)));

    let chinese =
        parse_media_name("剧名 第 3 季 第 12 集.mkv", MediaKind::Episode).expect("Chinese episode");
    assert_eq!((chinese.season, chinese.episode), (Some(3), Some(12)));
}

#[test]
fn generates_distinct_title_candidates_for_localized_search() {
    let candidates = title_candidates("暗夜与黎明 2");

    assert_eq!(candidates.first().map(String::as_str), Some("暗夜与黎明 2"));
    assert!(candidates.iter().any(|candidate| candidate == "暗夜与黎明"));
}

#[test]
fn parses_movie_filename_with_chinese_title_and_release_suffix() {
    let parsed = parse_media_name(
        "二毛 (2019) - 2160p - H.265 - AAC - test.mkv",
        MediaKind::Movie,
    )
    .expect("movie filename");

    assert_eq!(parsed.title, "二毛");
    assert_eq!(parsed.production_year, Some(2019));
}

#[test]
fn removes_cd_part_marker_from_movie_title() {
    let parsed = parse_media_name("FC22378556无码 cd1.mp4", MediaKind::Movie).expect("movie name");

    assert_eq!(parsed.title, "FC22378556无码");
}

#[test]
fn parses_chinese_source_variants_as_one_movie_title() {
    let with_watermark =
        parse_media_name("ABF-301 (118abf301)-有码-C.mp4", MediaKind::Movie).expect("movie name");
    let cracked =
        parse_media_name("ABF-301 (118abf301)-破解-C.mp4", MediaKind::Movie).expect("movie name");

    assert_eq!(with_watermark.title, "ABF 301 118abf301");
    assert_eq!(with_watermark.title, cracked.title);
    assert_eq!(with_watermark.sort_title, cracked.sort_title);
    assert_eq!(with_watermark.edition_name.as_deref(), Some("有码 C"));
    assert_eq!(cracked.edition_name.as_deref(), Some("破解 C"));
}

#[test]
fn parses_unlisted_parenthesized_suffix_as_a_source_variant() {
    let parsed =
        parse_media_name("A Film (2024)-Alternative-C.mp4", MediaKind::Movie).expect("movie name");

    assert_eq!(parsed.title, "A Film");
    assert_eq!(parsed.production_year, Some(2024));
    assert_eq!(parsed.edition_name.as_deref(), Some("Alternative C"));
}

#[test]
fn parses_emby_tmdb_id_tags_without_polluting_movie_title() {
    for (tag, expected_id) in [
        ("[tmdbid=36557]", "36557"),
        ("[tmdbid-36557]", "36557"),
        ("[tmdb=36557]", "36557"),
        ("[tmdb-36557]", "36557"),
        ("{tmdbid=36557}", "36557"),
        ("{tmdb-36557}", "36557"),
        ("{tmdb=36557}", "36557"),
        ("{tmdbid-36557}", "36557"),
    ] {
        let parsed = parse_media_name(&format!("Casino Royale (2006) {tag}.mkv"), MediaKind::Movie)
            .expect("movie name");

        assert_eq!(parsed.title, "Casino Royale");
        assert_eq!(parsed.production_year, Some(2006));
        assert_eq!(
            parsed.provider_ids.get("Tmdb"),
            Some(&expected_id.to_owned())
        );
    }
}

#[test]
fn parses_emby_tmdb_id_tags_from_series_and_episode_names() {
    let series = parse_media_name("The Vampire Diaries (2009) {tmdb-18148}", MediaKind::Series)
        .expect("series name");
    assert_eq!(series.title, "The Vampire Diaries");
    assert_eq!(series.provider_ids.get("Tmdb"), Some(&"18148".to_owned()));

    let episode = parse_media_name(
        "The Vampire Diaries S01E01 [tmdbid=18148].mkv",
        MediaKind::Episode,
    )
    .expect("episode name");
    assert_eq!(episode.title, "The Vampire Diaries");
    assert_eq!(episode.provider_ids.get("Tmdb"), Some(&"18148".to_owned()));
}
