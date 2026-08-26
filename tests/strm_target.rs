use std::path::PathBuf;

use luxd::application::strm_target::{
    StrmAllowedRootsError, StrmTargetKind, canonical_local_strm_target_with_allowed_roots,
    classify_strm_target, normalize_strm_allowed_roots,
};

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
fn classifies_smb_and_ftp_targets() {
    assert_eq!(
        classify_strm_target("smb://nas/media/movie.mkv").kind,
        StrmTargetKind::Smb
    );
    assert_eq!(
        classify_strm_target("FTP://example.com/movie.mkv").kind,
        StrmTargetKind::Ftp
    );
}

#[test]
fn keeps_other_schemes_unsupported() {
    let target = classify_strm_target("rtsp://camera.example/live");

    assert_eq!(target.kind, StrmTargetKind::Unsupported);
    assert_eq!(target.value.as_deref(), Some("rtsp://camera.example/live"));
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

#[test]
fn normalizes_strm_allowed_roots_and_rejects_unsafe_values() {
    let normalized = normalize_strm_allowed_roots(&[
        " /CloudNAS/115-122 ".to_owned(),
        "/CloudNAS/115-122".to_owned(),
        "/CloudNAS/other".to_owned(),
    ])
    .expect("allowed roots should normalize");
    assert_eq!(
        normalized,
        vec!["/CloudNAS/115-122".to_owned(), "/CloudNAS/other".to_owned()]
    );

    for (value, error) in [
        ("relative/path", StrmAllowedRootsError::NotAbsolute),
        ("/", StrmAllowedRootsError::Root),
        (
            "/CloudNAS/../config",
            StrmAllowedRootsError::ParentTraversal,
        ),
    ] {
        assert_eq!(
            normalize_strm_allowed_roots(&[value.to_owned()]),
            Err(error)
        );
    }
}

#[tokio::test]
async fn resolves_targets_inside_additional_roots_but_not_outside_them()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let library_root = temp_dir.path().join("library");
    let allowed_root = temp_dir.path().join("allowed");
    let outside_root = temp_dir.path().join("outside");
    tokio::fs::create_dir_all(&library_root).await?;
    tokio::fs::create_dir_all(&allowed_root).await?;
    tokio::fs::create_dir_all(&outside_root).await?;
    let strm_path = library_root.join("movie.strm");
    let allowed_media = allowed_root.join("movie.mkv");
    let outside_media = outside_root.join("movie.mkv");
    tokio::fs::write(&strm_path, allowed_media.to_string_lossy().as_bytes()).await?;
    tokio::fs::write(&allowed_media, b"allowed").await?;
    tokio::fs::write(&outside_media, b"outside").await?;

    let resolved = canonical_local_strm_target_with_allowed_roots(
        library_root.to_str().ok_or("library root is not UTF-8")?,
        "movie.strm",
        allowed_media.to_str().ok_or("allowed media is not UTF-8")?,
        &[PathBuf::from(&allowed_root)],
    )
    .await?;
    assert_eq!(resolved, tokio::fs::canonicalize(&allowed_media).await?);

    let forbidden = canonical_local_strm_target_with_allowed_roots(
        library_root.to_str().ok_or("library root is not UTF-8")?,
        "movie.strm",
        outside_media.to_str().ok_or("outside media is not UTF-8")?,
        &[PathBuf::from(&allowed_root)],
    )
    .await;
    assert_eq!(
        forbidden,
        Err(luxd::application::strm_target::StrmLocalPathError::Forbidden)
    );

    let directory = canonical_local_strm_target_with_allowed_roots(
        library_root.to_str().ok_or("library root is not UTF-8")?,
        "movie.strm",
        allowed_root.to_str().ok_or("allowed root is not UTF-8")?,
        &[PathBuf::from(&allowed_root)],
    )
    .await;
    assert_eq!(
        directory,
        Err(luxd::application::strm_target::StrmLocalPathError::Forbidden)
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let symlink_path = allowed_root.join("outside.mkv");
        symlink(&outside_media, &symlink_path)?;
        let escaped = canonical_local_strm_target_with_allowed_roots(
            library_root.to_str().ok_or("library root is not UTF-8")?,
            "movie.strm",
            symlink_path.to_str().ok_or("symlink is not UTF-8")?,
            &[PathBuf::from(&allowed_root)],
        )
        .await;
        assert_eq!(
            escaped,
            Err(luxd::application::strm_target::StrmLocalPathError::Forbidden)
        );
    }

    Ok(())
}
