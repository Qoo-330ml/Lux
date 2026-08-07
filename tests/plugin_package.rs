use std::{fs, process::Command};

use luxd::application::plugin_runtime::PluginCatalog;
use serde_json::json;
use tempfile::tempdir;
use zip::ZipArchive;

#[test]
fn packages_an_unsigned_tmdb_zip_even_when_a_signing_key_is_configured()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let binary = root.path().join("lux-plugin-tmdb");
    let archive = root.path().join("org.lux.tmdb-1.0.0.zip");
    fs::write(&binary, b"standalone plugin binary")?;

    let packer = std::env::var("CARGO_BIN_EXE_lux-plugin-pack")
        .or_else(|_| std::env::var("CARGO_BIN_EXE_lux_plugin_pack"))?;
    let signing_key_hex = "07".repeat(32);
    let status = Command::new(packer)
        .env("LUX_PLUGIN_SIGNING_KEY_HEX", &signing_key_hex)
        .args([
            "--binary",
            binary.to_str().ok_or("binary path is not UTF-8")?,
            "--output",
            archive.to_str().ok_or("archive path is not UTF-8")?,
            "--version",
            "1.0.0",
            "--platform",
            "linux",
            "--arch",
            "x86_64",
        ])
        .status()?;
    assert!(status.success());

    let mut zip = ZipArchive::new(fs::File::open(&archive)?)?;
    assert!(zip.by_name("signature.json").is_err());

    let catalog = PluginCatalog::discover(root.path());

    assert!(catalog.failures.is_empty());
    assert_eq!(catalog.plugins.len(), 1);
    assert_eq!(catalog.plugins[0].manifest.id, "org.lux.tmdb");
    assert_eq!(catalog.plugins[0].manifest.version, "1.0.0");
    assert!(catalog.plugins[0].entrypoint.is_file());
    assert_eq!(
        catalog.plugins[0].manifest.config_fields[1].key,
        "preferredLanguage"
    );
    assert_eq!(
        catalog.plugins[0].manifest.config_fields[1].options[0].value,
        "zh-CN"
    );
    assert!(catalog.plugins[0].manifest.config_fields[3].multiple);
    assert_eq!(
        catalog.plugins[0].manifest.config_fields[4].key,
        "alternateApiEnabled"
    );
    assert_eq!(
        catalog.plugins[0].manifest.config_fields[5].options[1].label,
        "https://api.tmdb.org"
    );
    Ok(())
}

#[test]
fn packages_an_unsigned_tmdb_zip_without_a_signing_key() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let binary = root.path().join("lux-plugin-tmdb");
    let archive = root.path().join("org.lux.tmdb-1.0.0.zip");
    fs::write(&binary, b"standalone plugin binary")?;

    let packer = std::env::var("CARGO_BIN_EXE_lux-plugin-pack")
        .or_else(|_| std::env::var("CARGO_BIN_EXE_lux_plugin_pack"))?;
    let status = Command::new(packer)
        .env_remove("LUX_PLUGIN_SIGNING_KEY_HEX")
        .args([
            "--binary",
            binary.to_str().ok_or("binary path is not UTF-8")?,
            "--output",
            archive.to_str().ok_or("archive path is not UTF-8")?,
            "--version",
            "1.0.0",
            "--platform",
            "linux",
            "--arch",
            "x86_64",
        ])
        .status()?;
    assert!(status.success());

    let mut zip = ZipArchive::new(fs::File::open(&archive)?)?;
    assert!(zip.by_name("signature.json").is_err());

    let catalog = PluginCatalog::discover(root.path());

    assert!(catalog.failures.is_empty());
    assert_eq!(catalog.plugins.len(), 1);
    assert_eq!(catalog.plugins[0].manifest.id, "org.lux.tmdb");
    Ok(())
}

#[test]
fn packages_a_media_info_zip_with_the_media_probe_manifest()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let binary = root.path().join("lux-plugin-strm-media-info");
    let archive = root.path().join("org.lux.strm-media-info-1.0.0.zip");
    fs::write(&binary, b"standalone media info plugin binary")?;

    let packer = std::env::var("CARGO_BIN_EXE_lux-plugin-pack")
        .or_else(|_| std::env::var("CARGO_BIN_EXE_lux_plugin_pack"))?;
    let status = Command::new(packer)
        .env_remove("LUX_PLUGIN_SIGNING_KEY_HEX")
        .args([
            "--plugin",
            "strm-media-info",
            "--binary",
            binary.to_str().ok_or("binary path is not UTF-8")?,
            "--output",
            archive.to_str().ok_or("archive path is not UTF-8")?,
            "--version",
            "1.0.0",
            "--platform",
            "linux",
            "--arch",
            "x86_64",
        ])
        .status()?;
    assert!(status.success());

    let catalog = PluginCatalog::discover(root.path());
    assert!(catalog.failures.is_empty());
    assert_eq!(catalog.plugins.len(), 1);
    let manifest = &catalog.plugins[0].manifest;
    assert_eq!(manifest.id, "org.lux.strm-media-info");
    assert_eq!(manifest.name, "strm媒体信息提取");
    assert_eq!(manifest.plugin_type, "media_probe");
    assert_eq!(manifest.category, "MEDIA");
    assert_eq!(manifest.capabilities, vec!["media.probe"]);
    assert_eq!(
        manifest
            .config_fields
            .iter()
            .map(|field| field.key.as_str())
            .collect::<Vec<_>>(),
        vec![
            "libraryIds",
            "concurrency",
            "existingInfoPolicy",
            "writeSidecars"
        ]
    );
    assert_eq!(
        manifest.config_fields[0].options_source.as_deref(),
        Some("media-libraries")
    );
    assert_eq!(manifest.config_fields[1].minimum, Some(1));
    assert_eq!(manifest.config_fields[1].maximum, Some(64));
    assert_eq!(manifest.config_fields[2].input_type, "select");
    assert_eq!(manifest.config_fields[2].default_value, Some(json!("SKIP")));
    assert_eq!(
        manifest.config_fields[2]
            .options
            .iter()
            .map(|option| option.value.as_str())
            .collect::<Vec<_>>(),
        vec!["SKIP", "OVERWRITE"]
    );
    Ok(())
}

#[test]
fn packages_both_ip_location_plugin_zips_with_network_manifests()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempdir()?;
    let packer = std::env::var("CARGO_BIN_EXE_lux-plugin-pack")
        .or_else(|_| std::env::var("CARGO_BIN_EXE_lux_plugin_pack"))?;
    for (plugin, expected_id, expected_name, expected_host) in [
        (
            "ip-hiofd",
            "org.lux.ip-hiofd",
            "IP归属地查询增强",
            "toola.hiofd.com",
        ),
        (
            "qoo-ip138",
            "org.lux.qoo-ip138",
            "qoo-ip138 IP归属地查询",
            "www.ipshudi.com",
        ),
    ] {
        let binary = root.path().join(format!("lux-plugin-{plugin}"));
        let archive = root.path().join(format!("{expected_id}-1.0.0.zip"));
        fs::write(&binary, b"standalone IP location plugin binary")?;
        let status = Command::new(&packer)
            .env_remove("LUX_PLUGIN_SIGNING_KEY_HEX")
            .args([
                "--plugin",
                plugin,
                "--binary",
                binary.to_str().ok_or("binary path is not UTF-8")?,
                "--output",
                archive.to_str().ok_or("archive path is not UTF-8")?,
                "--version",
                "1.0.0",
                "--platform",
                "linux",
                "--arch",
                "x86_64",
            ])
            .status()?;
        assert!(status.success());

        let catalog = PluginCatalog::discover(root.path());
        let manifest = catalog
            .plugins
            .iter()
            .find(|plugin| plugin.manifest.id == expected_id)
            .ok_or("IP location plugin was not discovered")?;
        assert_eq!(manifest.manifest.name, expected_name);
        assert_eq!(manifest.manifest.plugin_type, "ip_location");
        assert_eq!(manifest.manifest.category, "NETWORK");
        assert_eq!(manifest.manifest.capabilities, vec!["ip.location"]);
        assert_eq!(manifest.manifest.permissions.network, vec![expected_host]);
    }
    Ok(())
}
