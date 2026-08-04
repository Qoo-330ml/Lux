use std::{fs, process::Command};

use luxd::application::plugin_runtime::PluginCatalog;
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
