#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrmTargetKind {
    Empty,
    Url,
    Path,
    Opaque,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StrmTarget {
    pub kind: StrmTargetKind,
    pub value: Option<String>,
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
    } else if is_path_target(&value) {
        StrmTargetKind::Path
    } else {
        StrmTargetKind::Opaque
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
    use super::{StrmTargetKind, classify_strm_target};

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
                kind: StrmTargetKind::Opaque,
                value: Some("magnet:?xt=urn:btih:example".to_owned()),
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
}
