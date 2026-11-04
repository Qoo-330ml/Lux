//! Desensitized diagnostics for Emby image requests (LUX-882).
//!
//! Administrators have to tell from the NAS log whether a third-party Emby client sends size
//! hints and whether the answer came from the variant cache. One `info!` event per served image
//! request carries the request shape and the variant decision, and nothing that identifies the
//! caller: no token, no URL, no query string, no cookie, no IP, no full `User-Agent` and no item
//! id. The target is fixed so `RUST_LOG` can filter on it.

use axum::http::HeaderMap;

use crate::application::image_variants::{ImageVariantSelection, OriginalImageReason};

use super::emby_handlers::{EmbyImageDisplayQuery, emby_device_info_from_headers};
use super::normalize_image_type;

/// `RUST_LOG` target of the image diagnostics. Keep it stable; the deployment docs grep for it.
pub(super) const EMBY_IMAGE_DIAG_TARGET: &str = "luxd::emby_image_diag";

/// Upper bound for the client and device labels (`X-Emby-Client`, `Device=`, UA product name).
const CLIENT_LABEL_MAX_CHARS: usize = 32;
/// Upper bound for the `format` label.
const FORMAT_LABEL_MAX_CHARS: usize = 8;
const CLIENT_HEADER: &str = "x-emby-client";
const DEVICE_HEADER: &str = "x-emby-device-name";
const USER_AGENT_HEADER: &str = "user-agent";

/// Which branch of the Emby image route answered the request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EmbyImageAuthMode {
    /// A real Emby/Lux principal resolved from a token or API key.
    Principal,
    /// Untagged-tag path: the URL carried an image tag instead of a login (`?tag=`).
    Tag,
    /// Filmly compatibility branch, which serves untagged images without a login.
    FilmlyCompat,
}

impl EmbyImageAuthMode {
    pub(super) const fn for_request(has_principal: bool, filmly_compat: bool) -> Self {
        if has_principal {
            Self::Principal
        } else if filmly_compat {
            Self::FilmlyCompat
        } else {
            Self::Tag
        }
    }

    /// Stable label for the diagnostic log.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Principal => "principal",
            Self::Tag => "tag",
            Self::FilmlyCompat => "filmly_compat",
        }
    }
}

/// The desensitized facts of one Emby image request, captured before the image is resolved and
/// reported once the answer (variant or original) is known.
///
/// Only bounded labels and numbers are kept: the item id, the URL, the query string and the
/// credentials never reach this struct.
#[derive(Debug)]
pub(super) struct EmbyImageDiagnostics {
    client: Option<String>,
    device: Option<String>,
    image_type: Option<&'static str>,
    has_tag: bool,
    auth_mode: EmbyImageAuthMode,
    max_width: Option<u32>,
    max_height: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
    quality: Option<u32>,
    format: Option<String>,
    overlay: bool,
}

impl EmbyImageDiagnostics {
    pub(super) fn for_request(
        headers: &HeaderMap,
        display: &EmbyImageDisplayQuery,
        image_type: &str,
        has_tag: bool,
        auth_mode: EmbyImageAuthMode,
    ) -> Self {
        let (client, device) = client_and_device_labels(headers);
        Self {
            client,
            device,
            image_type: normalize_image_type(image_type),
            has_tag,
            auth_mode,
            max_width: display.max_width(),
            max_height: display.max_height(),
            width: display.width(),
            height: display.height(),
            quality: display.quality(),
            format: display.format().and_then(clean_format_label),
            overlay: display.has_overlay(),
        }
    }

    /// Emit the single diagnostic event for this request.
    ///
    /// Absent values are omitted by the `Option` field records, so a client that sends no
    /// `maxWidth` simply has no `max_width` field instead of a misleading zero.
    pub(super) fn record(&self, selection: ImageVariantSelection) {
        let outcome = if selection.is_variant() {
            "variant"
        } else {
            "original"
        };
        tracing::info!(
            target: EMBY_IMAGE_DIAG_TARGET,
            client = self.client.as_deref(),
            device = self.device.as_deref(),
            image_type = self.image_type,
            has_tag = self.has_tag,
            auth_mode = self.auth_mode.as_str(),
            max_width = self.max_width,
            max_height = self.max_height,
            width = self.width,
            height = self.height,
            quality = self.quality,
            format = self.format.as_deref(),
            overlay = self.overlay,
            outcome,
            variant_width = selection.variant_width(),
            reason = selection.original_reason().map(OriginalImageReason::as_str),
            "Emby image request diagnostics"
        );
    }
}

/// Client and device labels for the log, in the documented preference order.
fn client_and_device_labels(headers: &HeaderMap) -> (Option<String>, Option<String>) {
    let device_info = emby_device_info_from_headers(headers);
    let client = header_label(headers, CLIENT_HEADER)
        .or_else(|| clean_client_label(device_info.client.trim()))
        .or_else(|| header_str(headers, USER_AGENT_HEADER).and_then(user_agent_product_label));
    let device = header_label(headers, DEVICE_HEADER)
        .or_else(|| clean_client_label(device_info.device.trim()));
    (client, device)
}

fn header_label(headers: &HeaderMap, name: &str) -> Option<String> {
    header_str(headers, name).and_then(|value| clean_client_label(value.trim()))
}

/// Product name of a `User-Agent`: the part before the first `/` or whitespace.
///
/// The full `User-Agent` is never kept, and neither is the version or anything after it, so a
/// token smuggled into a UA suffix cannot reach the log.
fn user_agent_product_label(user_agent: &str) -> Option<String> {
    let product = user_agent
        .split(|character: char| character == '/' || character.is_whitespace())
        .find(|token| !token.is_empty())?;
    clean_client_label(product)
}

/// Keep only ASCII alphanumerics and `-_.`, bounded to the documented label length.
fn clean_client_label(value: &str) -> Option<String> {
    let label: String = value
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
        .take(CLIENT_LABEL_MAX_CHARS)
        .collect();
    (!label.is_empty()).then_some(label)
}

/// `format` label: ASCII alphanumerics only, bounded to the documented length.
fn clean_format_label(value: &str) -> Option<String> {
    let label: String = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(FORMAT_LABEL_MAX_CHARS)
        .collect();
    (!label.is_empty()).then_some(label)
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Write},
        sync::{Arc, Mutex},
    };

    use axum::http::{HeaderName, HeaderValue};

    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).expect("header name"),
                HeaderValue::from_bytes(value.as_bytes()).expect("header value"),
            );
        }
        headers
    }

    #[test]
    fn explicit_client_headers_win_over_the_user_agent() {
        let (client, device) = client_and_device_labels(&headers(&[
            ("x-emby-client", "Infuse"),
            ("x-emby-device-name", "Living Room TV"),
            ("user-agent", "Mozilla/5.0 (Macintosh)"),
        ]));
        assert_eq!(client.as_deref(), Some("Infuse"));
        assert_eq!(device.as_deref(), Some("LivingRoomTV"));
    }

    #[test]
    fn authorization_client_and_device_are_read_when_the_headers_are_absent() {
        let (client, device) = client_and_device_labels(&headers(&[(
            "Authorization",
            "MediaBrowser Client=\"Emby\", Device=\"Hills\", DeviceId=\"abc\", Version=\"4.8.0.0\"",
        )]));
        assert_eq!(client.as_deref(), Some("Emby"));
        assert_eq!(device.as_deref(), Some("Hills"));
    }

    #[test]
    fn the_user_agent_product_name_is_the_last_resort() {
        let (client, device) = client_and_device_labels(&headers(&[(
            "user-agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15",
        )]));
        assert_eq!(client.as_deref(), Some("Mozilla"));
        assert_eq!(device, None);
        let (client, _) = client_and_device_labels(&headers(&[("user-agent", "Infuse/7.0.1")]));
        assert_eq!(client.as_deref(), Some("Infuse"));
        let (client, _) = client_and_device_labels(&headers(&[("user-agent", "   ")]));
        assert_eq!(client, None);
        let (client, _) = client_and_device_labels(&headers(&[]));
        assert_eq!(client, None);
    }

    #[test]
    fn hostile_labels_are_stripped_and_bounded() {
        // A UA carrying a token after 32 allowed characters must not leak the token.
        let user_agent = format!("{}{}", "A".repeat(CLIENT_LABEL_MAX_CHARS), "SECRET_TOKEN");
        let (client, _) = client_and_device_labels(&headers(&[("user-agent", &user_agent)]));
        let client = client.expect("client label");
        assert_eq!(client.len(), CLIENT_LABEL_MAX_CHARS);
        assert!(!client.contains("SECRET"));
        // Control characters never reach the log. (HTTP header values cannot carry them, so the
        // sanitizer is exercised directly.)
        assert_eq!(clean_client_label("Inf\u{7}use").as_deref(), Some("Infuse"));
        assert_eq!(clean_client_label("\u{0}\u{1}\u{7f}"), None);
        assert_eq!(clean_client_label(" \t "), None);
        // Only the product name of a token-bearing UA is kept.
        let (client, _) = client_and_device_labels(&headers(&[(
            "user-agent",
            "Infuse/7.0 api_key=supersecrettoken",
        )]));
        assert_eq!(client.as_deref(), Some("Infuse"));
        // A label made only of stripped characters falls through instead of logging junk.
        let (client, _) = client_and_device_labels(&headers(&[
            ("x-emby-client", "***"),
            ("user-agent", "Hills/2"),
        ]));
        assert_eq!(client.as_deref(), Some("Hills"));
        // Non-ASCII client names are dropped rather than passed through.
        let (client, _) = client_and_device_labels(&headers(&[("x-emby-client", "网易爆米花")]));
        assert_eq!(client, None);
    }

    #[test]
    fn format_labels_keep_only_alphanumerics_and_stay_short() {
        assert_eq!(clean_format_label("jpeg").as_deref(), Some("jpeg"));
        assert_eq!(
            clean_format_label("WEBP/png?x=1").as_deref(),
            Some("WEBPpngx")
        );
        assert_eq!(
            clean_format_label("abcdefghijkl").as_deref(),
            Some("abcdefgh")
        );
        assert_eq!(clean_format_label("//"), None);
        assert_eq!(clean_format_label(""), None);
    }

    #[test]
    fn auth_modes_name_the_three_serving_branches() {
        assert_eq!(
            EmbyImageAuthMode::for_request(true, false),
            EmbyImageAuthMode::Principal
        );
        // A principal always wins, even if the filmly branch was considered.
        assert_eq!(
            EmbyImageAuthMode::for_request(true, true),
            EmbyImageAuthMode::Principal
        );
        assert_eq!(
            EmbyImageAuthMode::for_request(false, true),
            EmbyImageAuthMode::FilmlyCompat
        );
        assert_eq!(
            EmbyImageAuthMode::for_request(false, false),
            EmbyImageAuthMode::Tag
        );
        assert_eq!(EmbyImageAuthMode::Principal.as_str(), "principal");
        assert_eq!(EmbyImageAuthMode::Tag.as_str(), "tag");
        assert_eq!(EmbyImageAuthMode::FilmlyCompat.as_str(), "filmly_compat");
    }

    #[test]
    fn diagnostics_never_carry_credentials_or_identifiers() {
        let display = EmbyImageDisplayQuery::parse(Some(
            "api_key=SECRET_TOKEN&tag=item-tag&maxWidth=300&quality=high&format=Png",
        ));
        let diagnostics = EmbyImageDiagnostics::for_request(
            &headers(&[(
                "Authorization",
                "MediaBrowser Token=\"SECRET_TOKEN\", Client=\"Infuse\", Device=\"Apple TV\"",
            )]),
            &display,
            "Primary",
            true,
            EmbyImageAuthMode::Principal,
        );
        let rendered = format!("{diagnostics:?}");
        assert!(!rendered.contains("SECRET_TOKEN"));
        assert!(!rendered.contains("item-tag"));
        assert_eq!(diagnostics.client.as_deref(), Some("Infuse"));
        assert_eq!(diagnostics.device.as_deref(), Some("AppleTV"));
        assert_eq!(diagnostics.image_type, Some("POSTER"));
        assert_eq!(diagnostics.auth_mode, EmbyImageAuthMode::Principal);
        assert_eq!(diagnostics.max_width, Some(300));
        assert_eq!(diagnostics.max_height, None);
        // A non-numeric quality is ignored, and a non-JPEG format is only reported.
        assert_eq!(diagnostics.quality, None);
        assert_eq!(diagnostics.format.as_deref(), Some("Png"));
        assert!(diagnostics.has_tag);
        assert!(!diagnostics.overlay);
    }

    struct CapturedWriterFactory(Arc<Mutex<Vec<u8>>>);

    struct CapturedWriter(Arc<Mutex<Vec<u8>>>);

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedWriterFactory {
        type Writer = CapturedWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CapturedWriter(Arc::clone(&self.0))
        }
    }

    impl Write for CapturedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut output = self
                .0
                .lock()
                .map_err(|_| io::Error::other("captured log buffer is unavailable"))?;
            output.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_recorded_event_carries_the_desensitized_fields() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::INFO)
            .with_writer(CapturedWriterFactory(Arc::clone(&captured)))
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        let _guard = tracing::dispatcher::set_default(&dispatch);

        let display = EmbyImageDisplayQuery::parse(Some(
            "api_key=SECRET_TOKEN&tag=item-tag&maxWidth=300&quality=90&format=jpg&blur=1",
        ));
        let diagnostics = EmbyImageDiagnostics::for_request(
            &headers(&[("x-emby-client", "Infuse")]),
            &display,
            "Primary",
            false,
            EmbyImageAuthMode::FilmlyCompat,
        );
        diagnostics.record(ImageVariantSelection::Variant(720));
        diagnostics.record(ImageVariantSelection::Original(
            OriginalImageReason::Overlay,
        ));

        let logs = String::from_utf8(
            captured
                .lock()
                .map_err(|_| "captured log buffer is poisoned")
                .expect("captured log buffer")
                .clone(),
        )
        .expect("captured logs are UTF-8");
        assert!(logs.contains("\"target\":\"luxd::emby_image_diag\""));
        assert!(logs.contains("\"outcome\":\"variant\""));
        assert!(logs.contains("\"variant_width\":720"));
        assert!(logs.contains("\"outcome\":\"original\""));
        assert!(logs.contains("\"reason\":\"overlay\""));
        assert!(logs.contains("\"client\":\"Infuse\""));
        assert!(logs.contains("\"image_type\":\"POSTER\""));
        assert!(logs.contains("\"auth_mode\":\"filmly_compat\""));
        assert!(logs.contains("\"max_width\":300"));
        assert!(logs.contains("\"quality\":90"));
        assert!(logs.contains("\"overlay\":true"));
        // The credentials and the tag never reach the log.
        assert!(!logs.contains("SECRET_TOKEN"));
        assert!(!logs.contains("item-tag"));
    }
}
