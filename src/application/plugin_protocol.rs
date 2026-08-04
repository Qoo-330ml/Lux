use std::{fmt, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PLUGIN_FORMAT_VERSION: u32 = 1;
pub const PLUGIN_API_VERSION: u32 = 1;

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
    pub signature: PluginSignature,
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
        if self.plugin_type != "metadata" {
            return Err(PluginManifestError::Invalid(
                "only metadata plugins are supported".to_owned(),
            ));
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
            if field.input_type != "text" && field.input_type != "password" {
                return Err(PluginManifestError::Invalid(format!(
                    "unsupported config field type: {}",
                    field.input_type
                )));
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
        self.signature.validate()
    }
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

#[derive(Clone, Debug, Serialize)]
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
