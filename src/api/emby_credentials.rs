use super::*;

#[derive(Default)]
struct QueryCredential {
    value: Option<String>,
    repeated: bool,
}

impl QueryCredential {
    fn add(&mut self, value: String) {
        self.repeated |= self.value.is_some();
        self.value = Some(value);
    }

    fn selected(&self) -> Option<Result<String, StatusCode>> {
        if self.repeated {
            return Some(Err(StatusCode::UNAUTHORIZED));
        }
        self.value
            .as_ref()
            .filter(|value| !value.is_empty())
            .map(|value| Ok(value.clone()))
    }
}

#[derive(Default)]
struct QueryCredentials {
    emby: QueryCredential,
    api_key: QueryCredential,
    legacy_api_key: QueryCredential,
    legacy_media_browser: QueryCredential,
    remaining: String,
}

impl QueryCredentials {
    fn parse(raw: &str) -> Self {
        let mut credentials = Self::default();
        let mut remaining = Vec::new();
        for part in raw.split('&') {
            let Some((name, value)) = url::form_urlencoded::parse(part.as_bytes()).next() else {
                remaining.push(part);
                continue;
            };
            let source = if name.eq_ignore_ascii_case("X-Emby-Token") {
                Some(&mut credentials.emby)
            } else if name.eq_ignore_ascii_case("api_key") {
                Some(&mut credentials.api_key)
            } else if name.eq_ignore_ascii_case("apiKey") {
                Some(&mut credentials.legacy_api_key)
            } else if name.eq_ignore_ascii_case("X-MediaBrowser-Token") {
                Some(&mut credentials.legacy_media_browser)
            } else {
                None
            };
            if let Some(source) = source {
                source.add(value.into_owned());
            } else {
                // Retain exact encoding and order, including repeated non-auth parameters.
                remaining.push(part);
            }
        }
        credentials.remaining = remaining.join("&");
        credentials
    }

    fn select(&self, headers: &HeaderMap) -> Result<Option<String>, StatusCode> {
        let header = |name: &str, parse: fn(&str) -> Option<String>| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .and_then(parse)
                .map(Ok)
        };
        // Measured against official Emby 4.10.1.0. Select before validating: an
        // invalid higher-priority credential must never fall back to a valid lower one.
        header("X-Emby-Token", emby_token_header_value)
            .or_else(|| self.emby.selected())
            .or_else(|| header("X-Emby-Authorization", emby_authorization_token))
            .or_else(|| header("Authorization", emby_authorization_token))
            .or_else(|| header("X-MediaBrowser-Token", emby_token_header_value))
            .or_else(|| self.api_key.selected())
            // Retain existing Lux extensions only when standard sources are empty.
            .or_else(|| header("X-Lux-Api-Key", emby_token_header_value))
            .or_else(|| header("X-Emby-Authentication", emby_authorization_token))
            .or_else(|| header("Authorization", emby_token_header_value))
            .or_else(|| self.legacy_api_key.selected())
            .or_else(|| self.legacy_media_browser.selected())
            .transpose()
    }
}

pub(super) async fn normalize_credentials(mut request: Request<Body>, next: Next) -> Response {
    request.headers_mut().remove(NORMALIZED_CREDENTIAL_HEADER);
    let query = QueryCredentials::parse(request.uri().query().unwrap_or_default());
    let token = match query.select(request.headers()) {
        Ok(token) => token,
        Err(status) => return status.into_response(),
    };
    if let Some(token) = token {
        // A playback-only token can appear in a media URL, which ends up in browser history and
        // proxy logs. It therefore resolves only on the playback allow-list, and only after every
        // route-level transformation (mounting, case folding) has settled.
        if crate::auth::emby::is_playback_token(&token)
            && !is_playback_allow_listed_path(request.uri().path())
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        let Ok(value) = HeaderValue::from_str(&token) else {
            return StatusCode::UNAUTHORIZED.into_response();
        };
        request
            .headers_mut()
            .insert(NORMALIZED_CREDENTIAL_HEADER, value);
    }
    // Query DTOs receive only non-credential data, preventing Serde alias collisions.
    let path_and_query = if query.remaining.is_empty() {
        request.uri().path().to_owned()
    } else {
        format!("{}?{}", request.uri().path(), query.remaining)
    };
    let Ok(path_and_query) = path_and_query.parse() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let mut parts = request.uri().clone().into_parts();
    parts.path_and_query = Some(path_and_query);
    let Ok(uri) = axum::http::Uri::from_parts(parts) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    *request.uri_mut() = uri;
    next.run(request).await
}

/// Emby paths a playback-only token may call.
///
/// These are exactly what a 302 proxy needs in order to resolve the original `.strm` target: the
/// item lookup that exposes `MediaSources[].Path`, and the media endpoints themselves. The list is
/// intentionally closed: an unknown path is rejected rather than handed to a handler.
pub(super) fn is_playback_allow_listed_path(path: &str) -> bool {
    let path = path.trim();
    // Both the `/emby/...` mount and the legacy root-path Emby routes are served by the same
    // router, so either prefix is accepted. Everything else (Lux API, Web assets, SPA fallback)
    // rejects the token outright.
    let Some(relative) = strip_emby_prefix(path) else {
        return false;
    };
    if relative.is_empty() {
        return false;
    }
    let segments: Vec<&str> = relative.split('/').collect();
    // Emby clients and proxies are not consistent about casing, and the router normalises static
    // segments case-insensitively. Match the same way so the allow-list never disagrees with the
    // route the request will actually reach.
    let matches = |expected: &[&str]| {
        segments.len() == expected.len()
            && segments
                .iter()
                .zip(expected)
                .all(|(segment, expected)| segment.eq_ignore_ascii_case(expected))
    };
    if matches(&["Auth", "Keys"]) {
        return true;
    }
    match segments.as_slice() {
        [head, item, rest @ ..] if head.eq_ignore_ascii_case("videos") && !item.is_empty() => {
            is_playable_media_tail(rest)
        }
        [head, item] if head.eq_ignore_ascii_case("items") => !item.is_empty(),
        [head, user, leaf, item] => {
            head.eq_ignore_ascii_case("users")
                && leaf.eq_ignore_ascii_case("items")
                && !user.is_empty()
                && !item.is_empty()
        }
        _ => false,
    }
}

fn strip_emby_prefix(path: &str) -> Option<&str> {
    let path = path.strip_prefix('/')?;
    match path.split_once('/') {
        Some((first, rest)) if first.eq_ignore_ascii_case("emby") => Some(rest),
        _ => Some(path),
    }
}

/// Accepts `stream`, `stream.{container}`, `original`, `original.{container}` and the optional
/// `{media_source_id}` path segment Emby clients insert before them.
fn is_playable_media_tail(rest: &[&str]) -> bool {
    match rest {
        [endpoint] => is_playable_media_endpoint(endpoint),
        // `{media_source_id}` is optional, but it never appears together with a container suffix.
        [_media_source_id, endpoint] => is_playable_media_endpoint(endpoint),
        _ => false,
    }
}

fn is_playable_media_endpoint(endpoint: &str) -> bool {
    let (name, container) = match endpoint.split_once('.') {
        Some((name, container)) => (name, Some(container)),
        None => (endpoint, None),
    };
    if !matches!(name.to_ascii_lowercase().as_str(), "stream" | "original") {
        return false;
    }
    container.is_none_or(|container| {
        !container.is_empty() && container.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_normalization_retains_noncredential_encoding_and_duplicates() {
        let query = QueryCredentials::parse(
            "Fields=Path%2CMediaSources&%58-Emby-Token=user&MediaSourceId=first&api_key=proxy&MediaSourceId=second&opaque=a%2Bb+%26%3D",
        );
        assert_eq!(query.select(&HeaderMap::new()), Ok(Some("user".to_owned())));
        assert_eq!(
            query.remaining,
            "Fields=Path%2CMediaSources&MediaSourceId=first&MediaSourceId=second&opaque=a%2Bb+%26%3D"
        );
    }

    #[test]
    fn selected_query_credential_is_opaque_and_not_a_header_authorization_value() {
        let query = QueryCredentials::parse("api_key=Bearer+user");
        assert_eq!(
            query.select(&HeaderMap::new()),
            Ok(Some("Bearer user".to_owned()))
        );
    }

    #[test]
    fn repeated_selected_source_is_rejected_even_if_an_occurrence_is_empty() {
        let query = QueryCredentials::parse("api_key=&API_KEY=user");
        assert_eq!(
            query.select(&HeaderMap::new()),
            Err(StatusCode::UNAUTHORIZED)
        );
        let mut headers = HeaderMap::new();
        headers.insert("X-Emby-Token", HeaderValue::from_static("header"));
        assert_eq!(query.select(&headers), Ok(Some("header".to_owned())));
    }

    #[test]
    fn playback_allow_list_covers_what_a_302_proxy_calls() {
        for path in [
            "/emby/Items/123456",
            "/Items/123456",
            "/EMBY/items/123456",
            "/emby/Users/admin/Items/123456",
            "/emby/Videos/123456/stream",
            "/emby/Videos/123456/stream.mkv",
            "/Videos/123456/stream.mp4",
            "/emby/Videos/123456/123456/stream",
            "/emby/Videos/123456/mediasource_123456/stream.mp4",
            "/emby/Videos/123456/original",
            "/emby/Videos/123456/original.strm",
            "/emby/Auth/Keys",
        ] {
            assert!(is_playback_allow_listed_path(path), "{path}");
        }
    }

    #[test]
    fn playback_allow_list_rejects_everything_else() {
        for path in [
            "/",
            "/emby",
            "/emby/",
            "/Users",
            "/emby/Users",
            "/emby/Users/New",
            "/emby/System/Info",
            "/emby/Sessions",
            "/emby/Items",
            "/emby/Items/123456/Refresh",
            "/emby/Items/123456/Children",
            "/emby/Items/123456/Images/Primary",
            "/emby/Items/123456/PlaybackInfo",
            "/emby/Items/123456/Download",
            "/emby/Videos/123456/Download",
            "/emby/Videos/123456/master.m3u8",
            "/emby/Videos/123456/Subtitles/1/Stream",
            "/emby/Videos/123456/transcoding/abc/index.m3u8",
            "/emby/Library/Refresh",
            "/emby/not-a-route",
            "/api/v1/items/123456",
            "/api/v1/playback/bootstrap",
            "/Videos",
            "/Assets/app.js",
        ] {
            assert!(!is_playback_allow_listed_path(path), "{path}");
        }
    }
}
