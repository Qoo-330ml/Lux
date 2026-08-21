use super::strm_target::{StrmTargetKind, classify_strm_target};

/// Validate a supported target passed from a STRM file to the supervised probe plugin.
///
/// The legacy function name is kept for existing host/plugin call sites. The target is only
/// classified lexically; no network or filesystem operation is performed here.
pub fn validate_remote_media_url(value: &str) -> bool {
    value.chars().count() <= 8 * 1024
        && matches!(
            classify_strm_target(value).kind,
            StrmTargetKind::Url | StrmTargetKind::Path | StrmTargetKind::Smb | StrmTargetKind::Ftp
        )
}

#[cfg(test)]
mod tests {
    use super::validate_remote_media_url;

    #[test]
    fn accepts_only_supported_strm_targets_for_background_probe() {
        for target in [
            "https://media.example/movie.mkv",
            "/media/movies/movie.mkv",
            "smb://nas/media/movie.mkv",
            "ftp://example.com/movie.mkv",
        ] {
            assert!(
                validate_remote_media_url(target),
                "target should be accepted: {target}"
            );
        }
        for target in ["", "rtsp://camera.example/live", "magnet:?xt=urn:btih:test"] {
            assert!(
                !validate_remote_media_url(target),
                "target should be rejected: {target}"
            );
        }
    }
}
