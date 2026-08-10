use std::{
    env,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use luxd::application::{
    plugin_protocol::{
        IP_LOCATION_CAPABILITY, PLUGIN_CATEGORY_NETWORK, PLUGIN_TYPE_IP_LOCATION, PluginManifest,
    },
    settings::{tmdb_api_base_url_options, tmdb_language_options},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use zip::{ZipWriter, write::SimpleFileOptions};

struct Arguments {
    plugin: PluginKind,
    binary: PathBuf,
    output: PathBuf,
    version: String,
    platform: String,
    arch: String,
}

#[derive(Clone, Copy)]
enum PluginKind {
    Tmdb,
    MediaInfo,
    IpHiofd,
    QooIp138,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse(env::args().skip(1))?;
    let binary = fs::read(&arguments.binary)?;
    if binary.is_empty() {
        return Err("plugin binary is empty".into());
    }
    let binary_name = match (arguments.plugin, arguments.platform.as_str()) {
        (PluginKind::Tmdb, "windows") => "lux-plugin-tmdb.exe",
        (PluginKind::Tmdb, _) => "lux-plugin-tmdb",
        (PluginKind::MediaInfo, "windows") => "lux-plugin-strm-media-info.exe",
        (PluginKind::MediaInfo, _) => "lux-plugin-strm-media-info",
        (PluginKind::IpHiofd, "windows") => "lux-plugin-ip-hiofd.exe",
        (PluginKind::IpHiofd, _) => "lux-plugin-ip-hiofd",
        (PluginKind::QooIp138, "windows") => "lux-plugin-qoo-ip138.exe",
        (PluginKind::QooIp138, _) => "lux-plugin-qoo-ip138",
    };
    let relative_binary = format!(
        "binaries/{}-{}/{}",
        arguments.platform, arguments.arch, binary_name
    );
    let file_hash = format!("{:x}", Sha256::digest(&binary));
    let manifest = PluginManifest::from_value(manifest_value(
        arguments.plugin,
        &arguments.version,
        &relative_binary,
        &file_hash,
    ))?;
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
        let mut plugin = PluginKind::Tmdb;
        while let Some(flag) = values.next() {
            if flag == "--plugin" {
                plugin = match values
                    .next()
                    .ok_or_else(|| "missing value for --plugin".to_owned())?
                    .as_str()
                {
                    "tmdb" => PluginKind::Tmdb,
                    "strm-media-info" | "media-info" => PluginKind::MediaInfo,
                    "ip-hiofd" => PluginKind::IpHiofd,
                    "qoo-ip138" => PluginKind::QooIp138,
                    value => return Err(format!("unsupported plugin: {value}").into()),
                };
                continue;
            }
            let target = match flag.as_str() {
                "--binary" => &mut binary,
                "--output" => &mut output,
                "--version" => &mut version,
                "--platform" => &mut platform,
                "--arch" => &mut arch,
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
            plugin,
            binary: required(binary, "--binary")?.into(),
            output: required(output, "--output")?.into(),
            version: required(version, "--version")?,
            platform: required(platform, "--platform")?,
            arch: required(arch, "--arch")?,
        })
    }
}

fn required(value: Option<String>, name: &str) -> Result<String, Box<dyn std::error::Error>> {
    value.ok_or_else(|| format!("missing {name}\n{}", usage()).into())
}

fn usage() -> &'static str {
    "usage: lux-plugin-pack [--plugin tmdb|strm-media-info|ip-hiofd|qoo-ip138] --binary PATH --output PATH --version SEMVER --platform NAME --arch NAME"
}

fn manifest_value(
    plugin: PluginKind,
    version: &str,
    relative_binary: &str,
    file_hash: &str,
) -> serde_json::Value {
    let language_options = tmdb_language_options()
        .into_iter()
        .map(|option| json!({"value": option.value, "label": option.label}))
        .collect::<Vec<_>>();
    let api_base_url_options = tmdb_api_base_url_options()
        .into_iter()
        .map(|option| json!({"value": option.value, "label": option.label}))
        .collect::<Vec<_>>();
    match plugin {
        PluginKind::Tmdb => json!({
            "formatVersion": 1,
            "id": "org.lux.tmdb",
            "name": "TMDb 元数据插件",
            "description": "从 TMDb 提供 Emby 风格电影、剧集和图片元数据。",
            "version": version,
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": relative_binary},
            "type": "metadata",
            "category": "SCRAPER",
            "supportedItemTypes": ["Movie", "Series", "Season", "Episode", "Person", "BoxSet"],
            "capabilities": [
                "metadata.search",
                "metadata.get",
                "metadata.images",
                "metadata.credits",
                "metadata.externalIds",
                "metadata.trailers"
            ],
            "configFields": [
                {
                    "key": "apiKey",
                    "label": "TMDb API Key",
                    "type": "password",
                    "required": false,
                    "sensitive": true,
                    "description": "可选。留空时使用 Lux 内置的 TMDb Key。"
                },
                {
                    "key": "preferredLanguage",
                    "label": "首选语言",
                    "type": "select",
                    "required": true,
                    "options": language_options.clone()
                },
                {
                    "key": "languageFallbackEnabled",
                    "label": "TMDb 语言回退",
                    "type": "toggle",
                    "required": false
                },
                {
                    "key": "fallbackLanguages",
                    "label": "备选语言顺序",
                    "type": "select",
                    "multiple": true,
                    "required": false,
                    "options": language_options
                },
                {
                    "key": "alternateApiEnabled",
                    "label": "替代 API 地址",
                    "type": "toggle",
                    "required": false,
                    "description": "开启后使用下方地址访问 TMDb，默认使用官方地址。"
                },
                {
                    "key": "apiBaseUrl",
                    "label": "TMDb API 地址",
                    "type": "select",
                    "required": true,
                    "description": "可选择官方地址、替代地址，或填写自定义地址。",
                    "options": api_base_url_options
                }
            ],
            "permissions": {
                "network": ["api.themoviedb.org", "api.tmdb.org", "image.tmdb.org"],
                "filesystem": ["plugin-cache"]
            },
            "files": [{"path": relative_binary, "sha256": file_hash}]
        }),
        PluginKind::MediaInfo => json!({
            "formatVersion": 1,
            "id": "org.lux.strm-media-info",
            "name": "strm媒体信息提取",
            "description": "使用 ffprobe 提取 STRM 外部媒体的技术信息。",
            "version": version,
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": relative_binary},
            "type": "media_probe",
            "category": "MEDIA",
            "supportedItemTypes": [],
            "capabilities": ["media.probe"],
            "configFields": [
                {
                    "key": "libraryIds",
                    "label": "媒体库",
                    "type": "select",
                    "required": true,
                    "multiple": true,
                    "optionsSource": "media-libraries",
                    "description": "选择需要提取媒体信息的媒体库。"
                },
                {
                    "key": "concurrency",
                    "label": "并发数",
                    "type": "number",
                    "required": true,
                    "defaultValue": 2,
                    "minimum": 1,
                    "maximum": 64,
                    "description": "远程媒体信息提取的最大并发数。"
                },
                {
                    "key": "existingInfoPolicy",
                    "label": "已有媒体信息处理方式",
                    "type": "select",
                    "required": true,
                    "defaultValue": "SKIP",
                    "options": [
                        {"value": "SKIP", "label": "跳过已有媒体信息"},
                        {"value": "OVERWRITE", "label": "覆盖已有媒体信息"}
                    ]
                },
                {
                    "key": "writeSidecars",
                    "label": "写入 mediainfo.json",
                    "type": "toggle",
                    "defaultValue": true
                },
                {
                    "key": "schedule",
                    "label": "执行间隔",
                    "type": "text",
                    "required": true,
                    "defaultValue": "24h",
                    "description": "后台扫描间隔，支持 1m 到 365d，例如 6h 或 24h。"
                }
            ],
            "permissions": {
                "network": ["media-source"],
                "filesystem": []
            },
            "files": [{"path": relative_binary, "sha256": file_hash}]
        }),
        PluginKind::IpHiofd => json!({
            "formatVersion": 1,
            "id": "org.lux.ip-hiofd",
            "name": "IP归属地查询增强",
            "description": "通过 Hiofd 查询公网 IP 的归属地信息。",
            "version": version,
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": relative_binary},
            "type": PLUGIN_TYPE_IP_LOCATION,
            "category": PLUGIN_CATEGORY_NETWORK,
            "supportedItemTypes": [],
            "capabilities": [IP_LOCATION_CAPABILITY],
            "configFields": [],
            "permissions": {
                "network": ["toola.hiofd.com"],
                "filesystem": []
            },
            "files": [{"path": relative_binary, "sha256": file_hash}]
        }),
        PluginKind::QooIp138 => json!({
            "formatVersion": 1,
            "id": "org.lux.qoo-ip138",
            "name": "ip138 IP归属地查询",
            "description": "通过 ipshudi.com 查询公网 IP 的归属地信息。",
            "version": version,
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": relative_binary},
            "type": PLUGIN_TYPE_IP_LOCATION,
            "category": PLUGIN_CATEGORY_NETWORK,
            "supportedItemTypes": [],
            "capabilities": [IP_LOCATION_CAPABILITY],
            "configFields": [],
            "permissions": {
                "network": ["www.ipshudi.com"],
                "filesystem": []
            },
            "files": [{"path": relative_binary, "sha256": file_hash}]
        }),
    }
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
    archive.start_file(relative_binary, options)?;
    archive.write_all(binary)?;
    archive.finish()?;
    Ok(())
}
