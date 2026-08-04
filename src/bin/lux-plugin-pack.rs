use std::{
    env,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signer, SigningKey};
use luxd::application::plugin_protocol::{PluginManifest, PluginSignature};
use serde_json::json;
use sha2::{Digest, Sha256};
use zip::{ZipWriter, write::SimpleFileOptions};

struct Arguments {
    binary: PathBuf,
    output: PathBuf,
    version: String,
    platform: String,
    arch: String,
    key_id: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse(env::args().skip(1))?;
    let binary = fs::read(&arguments.binary)?;
    if binary.is_empty() {
        return Err("plugin binary is empty".into());
    }
    let binary_name = if arguments.platform == "windows" {
        "lux-plugin-tmdb.exe"
    } else {
        "lux-plugin-tmdb"
    };
    let relative_binary = format!(
        "binaries/{}-{}/{}",
        arguments.platform, arguments.arch, binary_name
    );
    let file_hash = format!("{:x}", Sha256::digest(&binary));
    let mut manifest = PluginManifest::from_value(json!({
        "formatVersion": 1,
        "id": "org.lux.tmdb",
        "name": "TMDb 元数据插件",
        "description": "从 TMDb 提供 Emby 风格电影、剧集和图片元数据。",
        "version": arguments.version,
        "apiVersion": 1,
        "runtime": {"kind": "process", "entrypoint": relative_binary},
        "type": "metadata",
        "category": "SCRAPER",
        "supportedItemTypes": ["Movie", "Series", "Season", "Episode", "Person", "BoxSet"],
        "capabilities": [
            "metadata.search",
            "metadata.details",
            "metadata.images",
            "metadata.externalIds",
            "metadata.trailers"
        ],
        "configFields": [{
            "key": "apiKey",
            "label": "TMDb API Key",
            "type": "password",
            "required": false,
            "sensitive": true,
            "description": "可选。留空时使用 Lux 内置的 TMDb Key。"
        }],
        "permissions": {
            "network": ["api.themoviedb.org", "image.tmdb.org"],
            "filesystem": ["plugin-cache"]
        },
        "files": [{"path": relative_binary, "sha256": file_hash}],
        "signature": {"algorithm": "ed25519", "keyId": arguments.key_id, "value": "placeholder"}
    }))?;
    let signing_key_hex = env::var("LUX_PLUGIN_SIGNING_KEY_HEX")
        .map_err(|_| "LUX_PLUGIN_SIGNING_KEY_HEX is not set")?;
    let signing_key = parse_signing_key(&signing_key_hex)?;
    let signature = signing_key.sign(&manifest.signing_payload()?);
    manifest.signature = PluginSignature {
        algorithm: "ed25519".to_owned(),
        key_id: arguments.key_id,
        value: BASE64.encode(signature.to_bytes()),
    };
    write_archive(&arguments.output, &relative_binary, &binary, &manifest)?;
    Ok(())
}

impl Arguments {
    fn parse(mut values: impl Iterator<Item = String>) -> Result<Self, Box<dyn std::error::Error>> {
        let mut binary = None;
        let mut output = None;
        let mut version = None;
        let mut platform = None;
        let mut arch = None;
        let mut key_id = None;
        while let Some(flag) = values.next() {
            let target = match flag.as_str() {
                "--binary" => &mut binary,
                "--output" => &mut output,
                "--version" => &mut version,
                "--platform" => &mut platform,
                "--arch" => &mut arch,
                "--key-id" => &mut key_id,
                "--help" => return Err(usage().into()),
                unknown => return Err(format!("unknown argument: {unknown}\n{}", usage()).into()),
            };
            *target = Some(
                values
                    .next()
                    .ok_or_else(|| format!("missing value for {flag}"))?,
            );
        }
        Ok(Self {
            binary: required(binary, "--binary")?.into(),
            output: required(output, "--output")?.into(),
            version: required(version, "--version")?,
            platform: required(platform, "--platform")?,
            arch: required(arch, "--arch")?,
            key_id: required(key_id, "--key-id")?,
        })
    }
}

fn required(value: Option<String>, name: &str) -> Result<String, Box<dyn std::error::Error>> {
    value.ok_or_else(|| format!("missing {name}\n{}", usage()).into())
}

fn usage() -> &'static str {
    "usage: lux-plugin-pack --binary PATH --output PATH --version SEMVER --platform NAME --arch NAME --key-id ID (private key from LUX_PLUGIN_SIGNING_KEY_HEX)"
}

fn parse_signing_key(value: &str) -> Result<SigningKey, Box<dyn std::error::Error>> {
    let bytes = decode_hex(value)?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "signing key must contain exactly 32 bytes")?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn decode_hex(value: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if value.len() % 2 != 0 {
        return Err("signing key hex must have an even length".into());
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|error| format!("invalid signing key hex: {error}").into())
        })
        .collect()
}

fn write_archive(
    output: &Path,
    relative_binary: &str,
    binary: &[u8],
    manifest: &PluginManifest,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(output)?;
    let mut archive = ZipWriter::new(file);
    let options = SimpleFileOptions::default();
    archive.start_file("manifest.json", options)?;
    archive.write_all(&serde_json::to_vec_pretty(manifest)?)?;
    archive.start_file("signature.json", options)?;
    archive.write_all(&serde_json::to_vec_pretty(&manifest.signature)?)?;
    archive.start_file(relative_binary, options)?;
    archive.write_all(binary)?;
    archive.finish()?;
    Ok(())
}
