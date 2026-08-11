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
pub const PLUGIN_TYPE_MEDIA_PROBE: &str = "media_probe";
pub const PLUGIN_TYPE_IP_LOCATION: &str = "ip_location";
pub const PLUGIN_TYPE_STRM_RESOLVER: &str = "strm_resolver";
pub const MEDIA_PROBE_CAPABILITY: &str = "media.probe";
pub const IP_LOCATION_CAPABILITY: &str = "ip.location";
pub const STRM_RESOLVE_CAPABILITY: &str = "strm.resolve";
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
