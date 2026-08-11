use luxd::application::strm_target::{StrmTargetKind, classify_strm_target};

#[test]
fn classifies_http_targets_without_validating_or_rewriting_them() {
    let target = classify_strm_target("\u{feff}  HTTPS://media.example/video?id=7  ");

    assert_eq!(target.kind, StrmTargetKind::Url);
    assert_eq!(
        target.value.as_deref(),
        Some("HTTPS://media.example/video?id=7")
    );
}

#[test]
fn classifies_posix_and_relative_paths_without_treating_them_as_urls() {
    for value in [
        "/CloudNAS/115-122/media/movie (4K).mp4",
        "media/movie (4K).mp4",
    ] {
        let target = classify_strm_target(value);

        assert_eq!(target.kind, StrmTargetKind::Path);
        assert_eq!(target.value.as_deref(), Some(value));
    }
}

#[test]
fn classifies_windows_and_unc_paths_as_paths() {
    for value in [r"C:\Media\movie.mkv", r"\\nas\media\movie.mkv"] {
        let target = classify_strm_target(value);

        assert_eq!(target.kind, StrmTargetKind::Path);
        assert_eq!(target.value.as_deref(), Some(value));
    }
}

#[test]
fn keeps_other_schemes_opaque() {
    let target = classify_strm_target("magnet:?xt=urn:btih:example");

    assert_eq!(target.kind, StrmTargetKind::Opaque);
    assert_eq!(target.value.as_deref(), Some("magnet:?xt=urn:btih:example"));
}

#[test]
fn selects_only_the_first_non_empty_line_and_handles_empty_content() {
    let target = classify_strm_target("\n \n /CloudNAS/115/movie.mp4\nignored");
    assert_eq!(target.kind, StrmTargetKind::Path);
    assert_eq!(target.value.as_deref(), Some("/CloudNAS/115/movie.mp4"));

    let empty = classify_strm_target("\u{feff}\n \n");
    assert_eq!(empty.kind, StrmTargetKind::Empty);
    assert_eq!(empty.value, None);
}
