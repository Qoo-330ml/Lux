use super::*;

pub(crate) async fn require_admin(
    headers: &HeaderMap,
    state: &ReadyAppState,
    require_csrf: bool,
) -> Result<(), Response> {
    if let Some(principal) = resolve_shared_admin_api_key(headers, state).await? {
        if !principal.can_manage_server()
            || (state.remote_access.is_remote(
                header_str(headers, "x-lux-peer-ip"),
                header_str(headers, "x-forwarded-for"),
            ) && !principal.can_remote_access())
        {
            return Err(api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::PermissionDenied,
                "没有服务器管理权限",
            )
            .into_response());
        }
        return Ok(());
    }
    if users::lux_user_token_from_headers(headers).is_some() {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "客户端令牌不能调用管理员接口",
        )
        .into_response());
    }
    let user = require_web_user(headers, state).await?;
    if !user.can_manage_server {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "没有服务器管理权限",
        )
        .into_response());
    }
    if require_csrf && !users::has_client_token(headers) {
        let auth = state.auth();
        let Some(session_token) = request_cookie(headers, "lux_session") else {
            return Err(api_error(
                headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要登录",
            )
            .into_response());
        };
        let session = match auth.resolve(&session_token).await {
            Ok(Some(session)) => session,
            Ok(None) => {
                return Err(api_error(
                    headers,
                    StatusCode::UNAUTHORIZED,
                    lux::ApiErrorCode::AuthenticationRequired,
                    "需要登录",
                )
                .into_response());
            }
            Err(_) => {
                return Err(api_error(
                    headers,
                    StatusCode::SERVICE_UNAVAILABLE,
                    lux::ApiErrorCode::DatabaseUnavailable,
                    "认证暂时不可用",
                )
                .into_response());
            }
        };
        let Some(csrf_token) = headers
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok())
        else {
            return Err(api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::CsrfFailed,
                "CSRF 校验失败",
            )
            .into_response());
        };
        if !auth.verify_csrf(&session, csrf_token) {
            return Err(api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::CsrfFailed,
                "CSRF 校验失败",
            )
            .into_response());
        }
    }
    Ok(())
}

pub(crate) async fn require_admin_web_session(
    headers: &HeaderMap,
    state: &ReadyAppState,
    require_csrf: bool,
) -> Result<(), Response> {
    if users::has_client_token(headers) {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "客户端令牌不能管理 API Key",
        )
        .into_response());
    }
    require_admin(headers, state, require_csrf).await
}

pub(crate) async fn resolve_shared_admin_api_key(
    headers: &HeaderMap,
    state: &ReadyAppState,
) -> Result<Option<crate::auth::users::AuthenticationPrincipal>, Response> {
    let Some(candidate) = lux_api_key_from_headers(headers) else {
        return Ok(None);
    };
    let service = state.admin_api_key();
    service.resolve_principal(&candidate).await.map_err(|_| {
        api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "认证暂时不可用",
        )
        .into_response()
    })
}

pub(crate) fn lux_api_key_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("X-Lux-Api-Key")
        .or_else(|| headers.get("X-Emby-Token"))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            headers
                .get("Authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().strip_prefix("Bearer "))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
}

/// Media paths whose clients may be unable to attach an `X-Lux-Api-Key` header.
const QUERY_API_KEY_PATH_SUFFIXES: [&str; 8] = [
    "/stream",
    "/download",
    "/direct",
    "/range",
    "/hls/",
    "/image",
    "/images",
    "/subtitle",
];

/// A token in the URL leaks through history, `Referer` and proxy logs, so the query-string
/// fallback is limited to read-only media requests.
fn query_api_key_is_allowed(method: &Method, path: &str) -> bool {
    if !matches!(*method, Method::GET | Method::HEAD) {
        return false;
    }
    if path.starts_with("/api/v1/admin") {
        return false;
    }
    // The Lux API and the Emby surface share one rule: a token may travel in the URL only for
    // read-only media requests. Emby clients address streams, images and downloads on both
    // `/Videos/...` and `/emby/Videos/...`, so the path-prefix check must not be Lux-only.
    if path.starts_with("/api/") && !path.starts_with("/api/v1") {
        return false;
    }
    let path = path.to_ascii_lowercase();
    QUERY_API_KEY_PATH_SUFFIXES
        .iter()
        .any(|suffix| path.ends_with(suffix) || path.contains(suffix))
}

pub(crate) async fn normalize_lux_api_key_query(request: Request<Body>, next: Next) -> Response {
    let mut request = request;
    if query_api_key_is_allowed(request.method(), request.uri().path())
        && !request.headers().contains_key("X-Lux-Api-Key")
        && !request.headers().contains_key("X-Emby-Token")
        && !request.headers().contains_key("X-MediaBrowser-Token")
        && !request.headers().contains_key("Authorization")
        && let Some(query) = request.uri().query()
        && let Some(key) =
            url::form_urlencoded::parse(query.as_bytes()).find_map(|(name, value)| {
                name.eq_ignore_ascii_case("api_key")
                    .then_some(value.into_owned())
            })
        && let Ok(value) = HeaderValue::from_str(&key)
    {
        request.headers_mut().insert("X-Lux-Api-Key", value);
    }
    next.run(request).await
}

pub(crate) fn api_error(
    headers: &HeaderMap,
    status: StatusCode,
    code: lux::ApiErrorCode,
    message: &str,
) -> (StatusCode, Json<Value>) {
    let request_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    tracing::Span::current().record("errorCode", code.as_str());
    let body = lux::ApiError::new(code, message, request_id);
    (
        status,
        Json(json!({
            "error": {
                "code": body.code,
                "message": body.message,
                "requestId": body.request_id
            }
        })),
    )
}

pub(crate) fn user_avatar_error(headers: &HeaderMap, error: UserAvatarError) -> Response {
    match error {
        UserAvatarError::UnsupportedContentType | UserAvatarError::InvalidContent => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "头像格式无效，仅支持 JPEG、PNG 或 WebP",
        )
        .into_response(),
        UserAvatarError::TooLarge { .. } => api_error(
            headers,
            StatusCode::PAYLOAD_TOO_LARGE,
            lux::ApiErrorCode::InvalidRequest,
            "头像不能超过 5 MiB",
        )
        .into_response(),
        UserAvatarError::InvalidPath(_) | UserAvatarError::Io { .. } => api_error(
            headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "头像暂时无法保存",
        )
        .into_response(),
    }
}

pub(crate) fn request_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value.split(';').find_map(|part| {
                let (cookie_name, cookie_value) = part.trim().split_once('=')?;
                (cookie_name == name).then(|| cookie_value.to_owned())
            })
        })
}

pub(crate) fn secure_cookie_for_request(headers: &HeaderMap, policy: &RemoteAccessPolicy) -> bool {
    policy.is_secure_request(
        header_str(headers, "x-lux-peer-ip"),
        header_str(headers, "x-forwarded-proto"),
    )
}

pub(crate) fn build_cookie(
    name: &str,
    value: &str,
    http_only: bool,
    max_age: Option<i64>,
    secure: bool,
) -> Option<HeaderValue> {
    let mut cookie = format!("{name}={value}; Path=/;");
    if secure {
        cookie.push_str(" Secure;");
    }
    cookie.push_str(" SameSite=Lax");
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    if let Some(max_age) = max_age {
        cookie.push_str(&format!("; Max-Age={max_age}"));
    }
    HeaderValue::from_str(&cookie).ok()
}

#[cfg(test)]
mod query_api_key_tests {
    use super::query_api_key_is_allowed;
    use axum::http::Method;

    #[test]
    fn query_tokens_are_limited_to_read_only_media_requests() {
        for (method, path) in [
            (Method::GET, "/api/v1/items/abc/stream"),
            (Method::HEAD, "/api/v1/items/abc/stream"),
            (Method::GET, "/api/v1/items/abc/hls/index.m3u8"),
            (Method::GET, "/api/v1/items/abc/download"),
            (Method::GET, "/api/v1/items/abc/image"),
            (Method::GET, "/api/v1/items/abc/subtitle/1"),
            (Method::GET, "/Videos/abc/stream"),
            (Method::GET, "/Videos/abc/stream.mkv"),
            (Method::GET, "/emby/Videos/abc/stream"),
            (Method::GET, "/Audio/abc/stream"),
            (Method::GET, "/Items/abc/Images/Primary"),
        ] {
            assert!(query_api_key_is_allowed(&method, path), "{method} {path}");
        }
        for (method, path) in [
            (Method::POST, "/api/v1/items/abc/stream"),
            (Method::DELETE, "/api/v1/items/abc/download"),
            (Method::GET, "/api/v1/admin/users"),
            (Method::GET, "/api/v1/items"),
            (Method::GET, "/api/v1/auth/me"),
            (Method::GET, "/Search/Hints"),
            (Method::GET, "/Items/abc"),
            (Method::POST, "/Videos/abc/stream"),
        ] {
            assert!(!query_api_key_is_allowed(&method, path), "{method} {path}");
        }
    }
}
