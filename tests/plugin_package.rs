use std::{fs, process::Command};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::SigningKey;
use luxd::application::plugin_runtime::PluginCatalog;
use serde_json::json;
use tempfile::tempdir;

#[test]
fn packages_a_signed_tmdb_zip_that_the_catalog_can_verify() -> Result<(), Box<dyn std::error::Error>>
{
    let root = tempdir()?;
    let binary = root.path().join("lux-plugin-tmdb");
    let archive = root.path().join("org.lux.tmdb-1.0.0.zip");
    let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
    fs::write(&binary, b"standalone plugin binary")?;
    fs::write(
        root.path().join("trusted_keys.json"),
        serde_json::to_vec(&json!({
            "test": BASE64.encode(signing_key.verifying_key().to_bytes())
        }))?,
    )?;

    let packer = std::env::var("CARGO_BIN_EXE_lux-plugin-pack")
        .or_else(|_| std::env::var("CARGO_BIN_EXE_lux_plugin_pack"))?;
    let status = Command::new(packer)
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
            "--key-id",
            "test",
            "--signing-key-hex",
            &"07".repeat(32),
        ])
        .status()?;
    assert!(status.success());

    let catalog = PluginCatalog::discover(root.path());

    assert!(catalog.failures.is_empty());
    assert_eq!(catalog.plugins.len(), 1);
    assert_eq!(catalog.plugins[0].manifest.id, "org.lux.tmdb");
    assert_eq!(catalog.plugins[0].manifest.version, "1.0.0");
    assert!(catalog.plugins[0].entrypoint.is_file());
    Ok(())
}
