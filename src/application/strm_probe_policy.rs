/// Validate only the opaque target passed from a STRM file to ffprobe.
///
/// The legacy function name is kept for existing host/plugin call sites, but
/// the target is intentionally not parsed as a URL. STRM entries may contain
/// private or public addresses, hostnames, or paths.
pub fn validate_remote_media_url(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= 8 * 1024
}
