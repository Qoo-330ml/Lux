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
