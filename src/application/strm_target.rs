use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrmTargetKind {
    Empty,
    Url,
    Path,
    Smb,
    Ftp,
    Unsupported,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StrmTarget {
    pub kind: StrmTargetKind,
    pub value: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrmLocalPathError {
    Missing,
    Forbidden,
}

impl fmt::Display for StrmLocalPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Missing => "STRM local target is missing",
            Self::Forbidden => "STRM local target is outside the allowed roots",
        })
    }
}

impl std::error::Error for StrmLocalPathError {}

pub async fn canonical_local_strm_target(
    root_path: &str,
    strm_relative_path: &str,
    target: &str,
) -> Result<std::path::PathBuf, StrmLocalPathError> {
    let root = tokio::fs::canonicalize(root_path)
        .await
        .map_err(|_| StrmLocalPathError::Missing)?;
    let strm_path = tokio::fs::canonicalize(root.join(strm_relative_path))
        .await
        .map_err(|_| StrmLocalPathError::Missing)?;
    if !strm_path.starts_with(&root) || strm_path == root {
        return Err(StrmLocalPathError::Forbidden);
    }
    let target_path = std::path::Path::new(target);
    let requested = if target_path.is_absolute() {
        target_path.to_owned()
    } else {
        strm_path
            .parent()
            .unwrap_or(root.as_path())
            .join(target_path)
    };
    let path = tokio::fs::canonicalize(requested)
        .await
        .map_err(|_| StrmLocalPathError::Missing)?;
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|_| StrmLocalPathError::Missing)?;
    if !metadata.is_file() {
        return Err(StrmLocalPathError::Missing);
    }
    Ok(path)
}

pub fn classify_strm_target(contents: &str) -> StrmTarget {
    let value = contents.lines().find_map(normalize_target_line);
    let Some(value) = value else {
        return StrmTarget {
            kind: StrmTargetKind::Empty,
            value: None,
        };
    };

    let kind = if is_http_url(&value) {
        StrmTargetKind::Url
    } else if has_scheme(&value, "smb") {
        StrmTargetKind::Smb
    } else if has_scheme(&value, "ftp") {
        StrmTargetKind::Ftp
    } else if is_path_target(&value) {
        StrmTargetKind::Path
    } else {
        StrmTargetKind::Unsupported
    };
    StrmTarget {
        kind,
        value: Some(value),
    }
}

fn normalize_target_line(line: &str) -> Option<String> {
    let line = line.trim().trim_start_matches('\u{feff}').trim();
    (!line.is_empty()).then(|| line.to_owned())
}

fn is_http_url(value: &str) -> bool {
    value
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        || value
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
}

fn has_scheme(value: &str, scheme: &str) -> bool {
    let expected = format!("{scheme}://");
    value
        .as_bytes()
        .get(..expected.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(expected.as_bytes()))
}

fn is_path_target(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with('\\')
        || is_windows_drive_path(value)
        || !has_uri_scheme(value)
}

fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn has_uri_scheme(value: &str) -> bool {
    let Some((scheme, _)) = value.split_once(':') else {
        return false;
    };
    let mut characters = scheme.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
}

#[cfg(test)]
mod tests {
    use super::{
        StrmLocalPathError, StrmTargetKind, canonical_local_strm_target, classify_strm_target,
    };

    #[test]
    fn classifies_http_paths_and_other_schemes() {
        assert_eq!(
            classify_strm_target(" HTTPS://media.example/video?id=7 "),
            super::StrmTarget {
                kind: StrmTargetKind::Url,
                value: Some("HTTPS://media.example/video?id=7".to_owned()),
            }
        );
        assert_eq!(
            classify_strm_target("/CloudNAS/media/movie.mp4"),
            super::StrmTarget {
                kind: StrmTargetKind::Path,
                value: Some("/CloudNAS/media/movie.mp4".to_owned()),
            }
        );
        assert_eq!(
            classify_strm_target("magnet:?xt=urn:btih:example"),
            super::StrmTarget {
                kind: StrmTargetKind::Unsupported,
                value: Some("magnet:?xt=urn:btih:example".to_owned()),
            }
        );
    }

    #[test]
    fn classifies_smb_and_ftp_uris_without_parsing_or_accessing_them() {
        assert_eq!(
            classify_strm_target("SMB://nas/media/movie.mkv"),
            super::StrmTarget {
                kind: StrmTargetKind::Smb,
                value: Some("SMB://nas/media/movie.mkv".to_owned()),
            }
        );
        assert_eq!(
            classify_strm_target("ftp://example.com/movie.mkv"),
            super::StrmTarget {
                kind: StrmTargetKind::Ftp,
                value: Some("ftp://example.com/movie.mkv".to_owned()),
            }
        );
    }

    #[test]
    fn preserves_first_non_empty_line_and_handles_empty_content() {
        assert_eq!(
            classify_strm_target("\n \n media/movie (4K).mp4\nignored"),
            super::StrmTarget {
                kind: StrmTargetKind::Path,
                value: Some("media/movie (4K).mp4".to_owned()),
            }
        );
        assert_eq!(
            classify_strm_target("\u{feff}\n \n"),
            super::StrmTarget {
                kind: StrmTargetKind::Empty,
                value: None,
            }
        );
    }

    #[tokio::test]
    async fn resolves_relative_targets_from_the_strm_directory_and_stays_in_root() {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be available");
        let root = temp_dir.path().join("library");
        let nested = root.join("nested");
        tokio::fs::create_dir_all(&nested)
            .await
            .expect("nested directory should be created");
        tokio::fs::write(root.join("movie.mkv"), b"movie")
            .await
            .expect("media file should be created");
        tokio::fs::write(nested.join("movie.strm"), b"../movie.mkv")
            .await
            .expect("strm file should be created");

        let resolved = canonical_local_strm_target(
            root.to_str().expect("root should be utf8"),
            "nested/movie.strm",
            "../movie.mkv",
        )
        .await;
        assert_eq!(
            resolved.expect("relative target should resolve"),
            root.join("movie.mkv")
                .canonicalize()
                .expect("path should canonicalize")
        );

        let outside = temp_dir.path().join("outside.mkv");
        tokio::fs::write(&outside, b"outside")
            .await
            .expect("outside file should be created");
        let outside_target = canonical_local_strm_target(
            root.to_str().expect("root should be utf8"),
            "nested/movie.strm",
            outside.to_str().expect("outside path should be utf8"),
        )
        .await;
        assert_eq!(outside_target, Err(StrmLocalPathError::Forbidden));
    }
}
