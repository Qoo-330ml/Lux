use std::{fmt, path::Path};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PLUGIN_FORMAT_VERSION: u32 = 1;
pub const PLUGIN_API_VERSION: u32 = 1;
pub const PLUGIN_CATEGORY_SCRAPER: &str = "SCRAPER";
pub const PLUGIN_CATEGORY_MEDIA: &str = "MEDIA";
pub const PLUGIN_CATEGORY_NETWORK: &str = "NETWORK";
pub const PLUGIN_CATEGORY_NOTIFICATION: &str = "NOTIFICATION";
pub const PLUGIN_CATEGORY_MIGRATION: &str = "MIGRATION";
pub const PLUGIN_TYPE_MEDIA_PROBE: &str = "media_probe";
pub const PLUGIN_TYPE_IP_LOCATION: &str = "ip_location";
pub const PLUGIN_TYPE_STRM_RESOLVER: &str = "strm_resolver";
pub const PLUGIN_TYPE_CHAPTER_DETECTOR: &str = "chapter_detector";
pub const PLUGIN_TYPE_NOTIFICATION: &str = "notification";
pub const PLUGIN_TYPE_DATA_MIGRATION: &str = "data_migration";
pub const PLUGIN_TYPE_DANMAKU: &str = "danmaku";
pub const MEDIA_PROBE_CAPABILITY: &str = "media.probe";
pub const IP_LOCATION_CAPABILITY: &str = "ip.location";
pub const STRM_RESOLVE_CAPABILITY: &str = "strm.resolve";
pub const CHAPTER_DETECT_CAPABILITY: &str = "chapters.detect";
pub const CHAPTER_LOOKUP_CAPABILITY: &str = "chapters.lookup";
pub const NOTIFICATION_SEND_CAPABILITY: &str = "notification.send";
pub const DANMAKU_MATCH_CAPABILITY: &str = "danmaku.match";
pub const EMBY_MIGRATION_CAPABILITY: &str = "migration.emby";
pub const STRM_RESOLVE_METHOD: &str = "strm.resolve";
pub const CHAPTER_DETECT_METHOD: &str = "chapters.detect";
pub const CHAPTER_LOOKUP_METHOD: &str = "chapters.lookup";
pub const NOTIFICATION_SEND_METHOD: &str = "notification.send";
pub const DANMAKU_MATCH_METHOD: &str = "danmaku.match";
pub const MIGRATION_TEST_METHOD: &str = "migration.test";
pub const MIGRATION_LIST_USERS_METHOD: &str = "migration.list_users";
pub const MIGRATION_LIST_ITEMS_METHOD: &str = "migration.list_items";
pub const MIGRATION_USER_STATE_METHOD: &str = "migration.user_state";
pub const MIGRATION_PERSON_FAVORITES_METHOD: &str = "migration.person_favorites";
pub const MIGRATION_AUTHENTICATE_USER_METHOD: &str = "migration.authenticate_user";
pub const CHAPTER_FINGERPRINT_SAMPLE_RATE: u32 = 11_025;
pub const CHAPTER_FINGERPRINT_POINT_DURATION_TICKS: i64 = 1_238_095;
pub const CONFIG_OPTIONS_SOURCE_MEDIA_LIBRARIES: &str = "media-libraries";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub format_version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub version: String,
    pub api_version: u32,
    pub runtime: PluginRuntime,
    #[serde(rename = "type")]
    pub plugin_type: String,
    #[serde(default = "default_plugin_category")]
    pub category: String,
    #[serde(default)]
    pub supported_item_types: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub config_fields: Vec<PluginConfigField>,
    #[serde(default)]
    pub permissions: PluginPermissions,
    #[serde(default)]
    pub files: Vec<PluginFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<PluginSignature>,
}

impl PluginManifest {
    pub fn from_value(value: Value) -> Result<Self, PluginManifestError> {
        let manifest: Self = serde_json::from_value(value)
            .map_err(|error| PluginManifestError::Invalid(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), PluginManifestError> {
        if self.format_version != PLUGIN_FORMAT_VERSION {
            return Err(PluginManifestError::Invalid(format!(
                "unsupported formatVersion {}",
                self.format_version
            )));
        }
        if self.api_version != PLUGIN_API_VERSION {
            return Err(PluginManifestError::Invalid(format!(
                "unsupported apiVersion {}",
                self.api_version
            )));
        }
        validate_identifier("id", &self.id, 128)?;
        validate_text("name", &self.name, 256)?;
        validate_semver(&self.version)?;
        validate_identifier("category", &self.category, 64)?;
        match self.plugin_type.as_str() {
            "metadata" => {}
            PLUGIN_TYPE_MEDIA_PROBE => {
                if self.category != PLUGIN_CATEGORY_MEDIA {
                    return Err(PluginManifestError::Invalid(
                        "media probe plugins must use the MEDIA category".to_owned(),
                    ));
                }
                if !self
                    .capabilities
                    .iter()
                    .any(|capability| capability == MEDIA_PROBE_CAPABILITY)
                {
                    return Err(PluginManifestError::Invalid(
                        "media probe plugins must declare media.probe".to_owned(),
                    ));
                }
            }
            PLUGIN_TYPE_IP_LOCATION => {
                if self.category != PLUGIN_CATEGORY_NETWORK {
                    return Err(PluginManifestError::Invalid(
                        "ip location plugins must use the NETWORK category".to_owned(),
                    ));
                }
                if !self
                    .capabilities
                    .iter()
                    .any(|capability| capability == IP_LOCATION_CAPABILITY)
                {
                    return Err(PluginManifestError::Invalid(
                        "ip location plugins must declare ip.location".to_owned(),
                    ));
                }
            }
            PLUGIN_TYPE_STRM_RESOLVER => {
                if self.category != PLUGIN_CATEGORY_MEDIA {
                    return Err(PluginManifestError::Invalid(
                        "STRM resolver plugins must use the MEDIA category".to_owned(),
                    ));
                }
                if !self
                    .capabilities
                    .iter()
                    .any(|capability| capability == STRM_RESOLVE_CAPABILITY)
                {
                    return Err(PluginManifestError::Invalid(
                        "STRM resolver plugins must declare strm.resolve".to_owned(),
                    ));
                }
            }
            PLUGIN_TYPE_CHAPTER_DETECTOR => {
                if self.category != PLUGIN_CATEGORY_MEDIA {
                    return Err(PluginManifestError::Invalid(
                        "chapter detector plugins must use the MEDIA category".to_owned(),
                    ));
                }
                if !self.capabilities.iter().any(|capability| {
                    capability == CHAPTER_DETECT_CAPABILITY
                        || capability == CHAPTER_LOOKUP_CAPABILITY
                }) {
                    return Err(PluginManifestError::Invalid(
                        "chapter detector plugins must declare chapters.detect or chapters.lookup"
                            .to_owned(),
                    ));
                }
            }
            PLUGIN_TYPE_NOTIFICATION => {
                if self.category != PLUGIN_CATEGORY_NOTIFICATION {
                    return Err(PluginManifestError::Invalid(
                        "notification plugins must use the NOTIFICATION category".to_owned(),
                    ));
                }
                if !self
                    .capabilities
                    .iter()
                    .any(|capability| capability == NOTIFICATION_SEND_CAPABILITY)
                {
                    return Err(PluginManifestError::Invalid(
                        "notification plugins must declare notification.send".to_owned(),
                    ));
                }
            }
            PLUGIN_TYPE_DANMAKU => {
                if self.category != PLUGIN_CATEGORY_MEDIA {
                    return Err(PluginManifestError::Invalid(
                        "danmaku plugins must use the MEDIA category".to_owned(),
                    ));
                }
                if !self
                    .capabilities
                    .iter()
                    .any(|capability| capability == DANMAKU_MATCH_CAPABILITY)
                {
                    return Err(PluginManifestError::Invalid(
                        "danmaku plugins must declare danmaku.match".to_owned(),
                    ));
                }
            }
            PLUGIN_TYPE_DATA_MIGRATION => {
                if self.category != PLUGIN_CATEGORY_MIGRATION {
                    return Err(PluginManifestError::Invalid(
                        "data migration plugins must use the MIGRATION category".to_owned(),
                    ));
                }
                if !self
                    .capabilities
                    .iter()
                    .any(|capability| capability == EMBY_MIGRATION_CAPABILITY)
                {
                    return Err(PluginManifestError::Invalid(
                        "data migration plugins must declare migration.emby".to_owned(),
                    ));
                }
            }
            _ => {
                return Err(PluginManifestError::Invalid(format!(
                    "unsupported plugin type: {}",
                    self.plugin_type
                )));
            }
        }
        self.runtime.validate()?;
        if self.supported_item_types.len() > 32 || self.capabilities.len() > 64 {
            return Err(PluginManifestError::Invalid(
                "manifest declares too many item types or capabilities".to_owned(),
            ));
        }
        for field in &self.config_fields {
            validate_identifier("config field key", &field.key, 64)?;
            validate_text("config field label", &field.label, 128)?;
            if !matches!(
                field.input_type.as_str(),
                "text" | "password" | "select" | "toggle" | "number"
            ) {
                return Err(PluginManifestError::Invalid(format!(
                    "unsupported config field type: {}",
                    field.input_type
                )));
            }
            if field.input_type == "select" {
                if field.options.is_empty() == field.options_source.is_none()
                    || field.options.len() > 256
                {
                    return Err(PluginManifestError::Invalid(
                        "select config field must declare options or an optionsSource".to_owned(),
                    ));
                }
                for option in &field.options {
                    validate_identifier("config option value", &option.value, 128)?;
                    validate_text("config option label", &option.label, 128)?;
                }
                if let Some(source) = field.options_source.as_deref()
                    && source != CONFIG_OPTIONS_SOURCE_MEDIA_LIBRARIES
                {
                    return Err(PluginManifestError::Invalid(
                        "unsupported config optionsSource".to_owned(),
                    ));
                }
            } else if field.multiple || !field.options.is_empty() || field.options_source.is_some()
            {
                return Err(PluginManifestError::Invalid(
                    "only select config fields may declare options, multiple or optionsSource"
                        .to_owned(),
                ));
            }
            if field.input_type == "number" {
                if field
                    .minimum
                    .is_some_and(|minimum| field.maximum.is_some_and(|maximum| minimum > maximum))
                {
                    return Err(PluginManifestError::Invalid(
                        "number config field minimum exceeds maximum".to_owned(),
                    ));
                }
                if let Some(default_value) = field.default_value.as_ref() {
                    let Some(default_value) = default_value.as_i64() else {
                        return Err(PluginManifestError::Invalid(
                            "number config field defaultValue must be an integer".to_owned(),
                        ));
                    };
                    if field.minimum.is_some_and(|minimum| default_value < minimum)
                        || field.maximum.is_some_and(|maximum| default_value > maximum)
                    {
                        return Err(PluginManifestError::Invalid(
                            "number config field defaultValue is outside its range".to_owned(),
                        ));
                    }
                }
            }
        }
        self.permissions.validate()?;
        for file in &self.files {
            validate_relative_path("manifest file", &file.path)?;
            if !is_sha256(&file.sha256) {
                return Err(PluginManifestError::Invalid(format!(
                    "invalid SHA-256 for {}",
                    file.path.display()
                )));
            }
        }
        if let Some(signature) = &self.signature {
            signature.validate()?;
        }
        Ok(())
    }

    pub fn signing_payload(&self) -> Result<Vec<u8>, PluginManifestError> {
        let mut value = serde_json::to_value(self)
            .map_err(|error| PluginManifestError::Invalid(error.to_string()))?;
        let object = value.as_object_mut().ok_or_else(|| {
            PluginManifestError::Invalid("plugin manifest must serialize as an object".to_owned())
        })?;
        object.remove("signature");
        serde_json::to_vec(&value).map_err(|error| PluginManifestError::Invalid(error.to_string()))
    }

    pub fn verify_signature(&self, key: &VerifyingKey) -> Result<(), PluginManifestError> {
        let signature = self.signature.as_ref().ok_or_else(|| {
            PluginManifestError::Invalid("plugin manifest has no signature".to_owned())
        })?;
        let signature_bytes = BASE64.decode(&signature.value).map_err(|error| {
            PluginManifestError::Invalid(format!("invalid signature encoding: {error}"))
        })?;
        let signature = Signature::from_slice(&signature_bytes)
            .map_err(|error| PluginManifestError::Invalid(format!("invalid signature: {error}")))?;
        let payload = self.signing_payload()?;
        key.verify(&payload, &signature).map_err(|error| {
            PluginManifestError::Invalid(format!("signature verification failed: {error}"))
        })
    }
}

fn default_plugin_category() -> String {
    PLUGIN_CATEGORY_SCRAPER.to_owned()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRuntime {
    pub kind: String,
    pub entrypoint: String,
}

impl PluginRuntime {
    fn validate(&self) -> Result<(), PluginManifestError> {
        if self.kind != "process" {
            return Err(PluginManifestError::Invalid(format!(
                "unsupported runtime kind: {}",
                self.kind
            )));
        }
        validate_relative_path("entrypoint", Path::new(&self.entrypoint))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginConfigField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub input_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub sensitive: bool,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default)]
    pub options: Vec<PluginConfigOption>,
    #[serde(default)]
    pub options_source: Option<String>,
    #[serde(default)]
    pub default_value: Option<Value>,
    #[serde(default)]
    pub minimum: Option<i64>,
    #[serde(default)]
    pub maximum: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginConfigOption {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginPermissions {
    #[serde(default)]
    pub network: Vec<String>,
    #[serde(default)]
    pub filesystem: Vec<String>,
}

impl PluginPermissions {
    fn validate(&self) -> Result<(), PluginManifestError> {
        if self.network.len() > 32 || self.filesystem.len() > 16 {
            return Err(PluginManifestError::Invalid(
                "manifest declares too many permissions".to_owned(),
            ));
        }
        for host in &self.network {
            validate_text("network permission", host, 255)?;
            if host.contains('/') || host.contains(' ') || host.contains('@') {
                return Err(PluginManifestError::Invalid(format!(
                    "invalid network permission: {host}"
                )));
            }
        }
        for path in &self.filesystem {
            validate_identifier("filesystem permission", path, 64)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginFile {
    pub path: std::path::PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSignature {
    pub algorithm: String,
    pub key_id: String,
    pub value: String,
}

impl PluginSignature {
    fn validate(&self) -> Result<(), PluginManifestError> {
        if self.algorithm != "ed25519" {
            return Err(PluginManifestError::Invalid(format!(
                "unsupported signature algorithm: {}",
                self.algorithm
            )));
        }
        validate_identifier("signature keyId", &self.key_id, 128)?;
        validate_text("signature value", &self.value, 4096)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRequest {
    pub id: String,
    pub method: String,
    pub params: Value,
}

impl PluginRequest {
    pub fn new(id: impl Into<String>, method: impl Into<String>, params: Value) -> Self {
        Self {
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginResponse {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PluginRpcError>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRpcError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DanmakuMatchRpcRequest {
    pub file_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DanmakuMatchStatus {
    Matched,
    NoMatch,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DanmakuMatchRpcResult {
    pub status: DanmakuMatchStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xml_base64: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpLocationRpcRequest {
    pub ip: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpLocationRpcResult {
    pub ip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub province: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub district: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub street: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaProbeRpcResult {
    pub container: Option<String>,
    pub source_size: Option<i64>,
    pub duration_ticks: Option<i64>,
    pub bitrate: Option<i64>,
    pub streams: Vec<MediaProbeRpcStream>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_jpeg_base64: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChapterDetectRpcRequest {
    pub episodes: Vec<ChapterFingerprintRpcEpisode>,
    pub intro_window_ticks: i64,
    pub credits_window_ticks: i64,
    pub minimum_match_duration_ticks: i64,
    pub match_threshold: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChapterFingerprintRpcEpisode {
    pub key: String,
    pub sample_rate: u32,
    pub fingerprint_point_duration_ticks: i64,
    pub intro_fingerprint_base64: String,
    pub credits_fingerprint_base64: String,
    pub intro_window_start_ticks: i64,
    pub credits_window_start_ticks: i64,
    pub intro_window_duration_ticks: i64,
    pub credits_window_duration_ticks: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChapterDetectRpcResult {
    #[serde(default)]
    pub markers: Vec<ChapterDetectRpcMarker>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChapterLookupRpcRequest {
    pub episodes: Vec<ChapterLookupRpcEpisode>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChapterLookupRpcEpisode {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmdb_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvdb_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb_id: Option<String>,
    pub season_number: i64,
    pub episode_number: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ticks: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChapterLookupRpcResult {
    #[serde(default)]
    pub markers: Vec<ChapterDetectRpcMarker>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChapterDetectRpcMarker {
    pub key: String,
    pub marker_type: ChapterDetectMarkerType,
    pub start_position_ticks: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub confidence: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChapterDetectMarkerType {
    IntroStart,
    IntroEnd,
    CreditsStart,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrmResolveRpcRequest {
    pub target: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StrmResolveStatus {
    Resolved,
    Unsupported,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrmResolveRpcResult {
    pub status: StrmResolveStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// The provider-neutral request sent to an installed notification plugin.
///
/// `event` is already filtered by Lux's notification event allowlist. `config`
/// contains only non-secret target settings; the optional `secret` is supplied
/// through the RPC request and is never persisted in the event or database.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotificationSendRpcRequest {
    pub event: Value,
    pub target: Value,
    pub config: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NotificationSendStatus {
    Delivered,
    Retryable,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotificationSendRpcResult {
    pub status: NotificationSendStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaProbeRpcStream {
    pub stream_index: i64,
    pub stream_type: MediaProbeRpcStreamType,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
    pub details: std::collections::BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MediaProbeRpcStreamType {
    Video,
    Audio,
    Subtitle,
}

#[derive(Debug)]
pub enum PluginManifestError {
    Invalid(String),
}

impl fmt::Display for PluginManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PluginManifestError {}

fn validate_identifier(
    field: &str,
    value: &str,
    max_len: usize,
) -> Result<(), PluginManifestError> {
    if value.is_empty()
        || value.len() > max_len
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(PluginManifestError::Invalid(format!(
            "invalid {field}: {value}"
        )));
    }
    Ok(())
}

fn validate_text(field: &str, value: &str, max_len: usize) -> Result<(), PluginManifestError> {
    if value.trim().is_empty() || value.chars().count() > max_len {
        return Err(PluginManifestError::Invalid(format!("invalid {field}")));
    }
    Ok(())
}

fn validate_semver(value: &str) -> Result<(), PluginManifestError> {
    let mut parts = value.split('.');
    let valid = (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }) && parts.next().is_none();
    if valid {
        Ok(())
    } else {
        Err(PluginManifestError::Invalid(format!(
            "invalid semantic version: {value}"
        )))
    }
}

fn validate_relative_path(field: &str, path: &Path) -> Result<(), PluginManifestError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        })
    {
        return Err(PluginManifestError::Invalid(format!(
            "invalid {field} path: {}",
            path.display()
        )));
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::{
        NotificationSendRpcRequest, NotificationSendRpcResult, NotificationSendStatus,
        PluginManifest,
    };
    use serde_json::json;

    fn notification_manifest(category: &str, capabilities: &[&str]) -> PluginManifest {
        serde_json::from_value(json!({
            "formatVersion": 1,
            "id": "org.example.notify",
            "name": "Example notifier",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "bin/notify"},
            "type": "notification",
            "category": category,
            "capabilities": capabilities,
        }))
        .expect("notification manifest should deserialize")
    }

    #[test]
    fn data_migration_manifest_requires_migration_category_and_capability() {
        let manifest: PluginManifest = serde_json::from_value(json!({
            "formatVersion": 1,
            "id": "org.lux.emby-migration",
            "name": "Emby migration",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "bin/emby-migration"},
            "type": "data_migration",
            "category": "MIGRATION",
            "capabilities": ["migration.emby"]
        }))
        .expect("migration manifest should deserialize");
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn notification_manifest_requires_notification_category_and_capability() {
        assert!(
            notification_manifest("NOTIFICATION", &["notification.send"])
                .validate()
                .is_ok()
        );
        assert!(
            notification_manifest("NETWORK", &["notification.send"])
                .validate()
                .is_err()
        );
        assert!(
            notification_manifest("NOTIFICATION", &[])
                .validate()
                .is_err()
        );
    }

    #[test]
    fn notification_rpc_round_trip_keeps_secret_out_of_event() {
        let request = NotificationSendRpcRequest {
            event: json!({
                "eventId": "event-1",
                "eventType": "MEDIA_ADDED",
                "data": {"libraryId": "library-1"}
            }),
            target: json!({
                "url": "https://hooks.example.test/lux",
                "allowPrivateNetwork": false,
            }),
            config: json!({"chatId": "chat-1"}),
            secret: Some("provider-secret".to_owned()),
        };
        let encoded = serde_json::to_value(&request).expect("request should serialize");
        assert_eq!(encoded["config"]["chatId"], "chat-1");
        assert_eq!(encoded["target"]["allowPrivateNetwork"], false);
        assert_eq!(encoded["secret"], "provider-secret");
        assert!(encoded["event"].get("secret").is_none());

        let result = NotificationSendRpcResult {
            status: NotificationSendStatus::Retryable,
            provider_request_id: None,
            retry_after_seconds: Some(30),
            error_code: Some("RATE_LIMITED".to_owned()),
        };
        let decoded: NotificationSendRpcResult =
            serde_json::from_value(serde_json::to_value(result).expect("result should serialize"))
                .expect("result should deserialize");
        assert_eq!(decoded.status, NotificationSendStatus::Retryable);
        assert_eq!(decoded.retry_after_seconds, Some(30));
    }
}
