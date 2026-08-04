use std::{fs, io::Write, path::Path};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signer, SigningKey};
use luxd::application::{plugin_protocol::PluginManifest, plugin_runtime::PluginCatalog};
use serde_json::{Value, json};
use tempfile::tempdir;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

fn test_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7_u8; 32])
}

fn signed_manifest_value(entrypoint: &str) -> Value {
    let mut value = json!({
        "formatVersion": 1,
        "id": "org.lux.example",
        "name": "Example plugin",
        "version": "1.0.0",
        "apiVersion": 1,
        "runtime": {"kind": "process", "entrypoint": entrypoint},
        "type": "metadata",
        "supportedItemTypes": ["Movie"],
        "capabilities": ["metadata.search"],
        "configFields": [],
        "permissions": {"network": [], "filesystem": ["plugin-cache"]},
        "files": [],
        "signature": {"algorithm": "ed25519", "keyId": "test", "value": "placeholder"}
    });
    let manifest = PluginManifest::from_value(value.clone()).expect("manifest should validate");
    let payload = manifest.signing_payload().expect("manifest payload");
    let signature = test_signing_key().sign(&payload);
    value["signature"]["value"] = Value::String(BASE64.encode(signature.to_bytes()));
    value
}

fn manifest_bytes(entrypoint: &str) -> Vec<u8> {
    serde_json::to_vec_pretty(&signed_manifest_value(entrypoint))
        .expect("manifest should serialize")
}

fn write_manifest(path: &Path, entrypoint: &str) {
    fs::write(path.join("manifest.json"), manifest_bytes(entrypoint))
        .expect("manifest should be written");
}

fn unsigned_manifest_bytes(entrypoint: &str) -> Vec<u8> {
    let mut value = signed_manifest_value(entrypoint);
    value
        .as_object_mut()
        .expect("manifest should be an object")
        .remove("signature");
    serde_json::to_vec_pretty(&value).expect("manifest should serialize")
}

fn write_unsigned_manifest(path: &Path, entrypoint: &str) {
    fs::write(
        path.join("manifest.json"),
        unsigned_manifest_bytes(entrypoint),
    )
    .expect("manifest should be written");
}

fn write_trusted_keys(path: &Path) {
    fs::write(
        path.join("trusted_keys.json"),
        serde_json::to_vec(&json!({
            "test": BASE64.encode(test_signing_key().verifying_key().to_bytes())
        }))
        .expect("trusted keys should serialize"),
    )
    .expect("trusted keys should be written");
}

#[test]
fn discovers_an_exploded_plugin_directory() {
    let root = tempdir().expect("temp dir should be created");
    let plugin = root.path().join("example");
    fs::create_dir_all(plugin.join("binaries")).expect("plugin directory should be created");
    fs::write(plugin.join("binaries/plugin"), b"plugin").expect("entrypoint should be written");
    write_manifest(&plugin, "binaries/plugin");
    write_trusted_keys(root.path());

    let catalog = PluginCatalog::discover(root.path());

    assert_eq!(catalog.plugins.len(), 1);
    assert!(catalog.failures.is_empty());
    assert_eq!(catalog.plugins[0].manifest.id, "org.lux.example");
    assert_eq!(
        catalog.plugins[0].entrypoint,
        plugin.join("binaries/plugin")
    );
}

#[test]
fn discovers_a_zip_plugin_package_and_extracts_its_entrypoint() {
    let root = tempdir().expect("temp dir should be created");
    let archive_path = root.path().join("org.lux.example-1.0.0.zip");
    let file = fs::File::create(&archive_path).expect("archive should be created");
    let mut archive = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    archive
        .start_file("manifest.json", options)
        .expect("manifest entry should be created");
    archive
        .write_all(&manifest_bytes("binaries/plugin"))
        .expect("manifest should be written");
    archive
        .start_file("binaries/plugin", options)
        .expect("entrypoint entry should be created");
    archive
        .write_all(b"plugin")
        .expect("entrypoint should be written");
    archive.finish().expect("archive should finish");
    write_trusted_keys(root.path());

    let catalog = PluginCatalog::discover(root.path());

    assert_eq!(catalog.plugins.len(), 1);
    assert!(catalog.plugins[0].is_archive);
    assert!(catalog.plugins[0].entrypoint.is_file());
    assert!(
        catalog.plugins[0]
            .root_path
            .ends_with(".extracted/org.lux.example-1.0.0")
    );
}

#[test]
fn ignores_unknown_files_and_reports_invalid_plugin_directories() {
    let root = tempdir().expect("temp dir should be created");
    fs::write(root.path().join("notes.txt"), b"not a plugin").expect("file should be written");
    let invalid = root.path().join("invalid");
    fs::create_dir_all(&invalid).expect("directory should be created");

    let catalog = PluginCatalog::discover(root.path());

    assert!(catalog.plugins.is_empty());
    assert_eq!(catalog.failures.len(), 1);
    assert!(catalog.failures[0].message.contains("manifest"));
}

#[test]
fn discovers_a_signed_plugin_without_a_trusted_signature_key() {
    let root = tempdir().expect("temp dir should be created");
    let plugin = root.path().join("example");
    fs::create_dir_all(plugin.join("binaries")).expect("plugin directory should be created");
    fs::write(plugin.join("binaries/plugin"), b"plugin").expect("entrypoint should be written");
    write_manifest(&plugin, "binaries/plugin");

    let catalog = PluginCatalog::discover(root.path());

    assert_eq!(catalog.plugins.len(), 1);
    assert!(catalog.failures.is_empty());
    assert_eq!(catalog.plugins[0].manifest.id, "org.lux.example");
}

#[test]
fn discovers_an_unsigned_plugin_without_trusted_signature_key() {
    let root = tempdir().expect("temp dir should be created");
    let plugin = root.path().join("unsigned-example");
    fs::create_dir_all(plugin.join("binaries")).expect("plugin directory should be created");
    fs::write(plugin.join("binaries/plugin"), b"plugin").expect("entrypoint should be written");
    write_unsigned_manifest(&plugin, "binaries/plugin");

    let catalog = PluginCatalog::discover(root.path());

    assert_eq!(catalog.plugins.len(), 1);
    assert!(catalog.failures.is_empty());
    assert_eq!(catalog.plugins[0].manifest.id, "org.lux.example");
}

#[cfg(unix)]
#[tokio::test]
async fn supervises_a_plugin_process_over_json_lines() {
    use std::os::unix::fs::PermissionsExt;

    use luxd::application::plugin_runtime::PluginSupervisor;

    let root = tempdir().expect("temp dir should be created");
    let plugin = root.path().join("example");
    fs::create_dir_all(plugin.join("binaries")).expect("plugin directory should be created");
    let entrypoint = plugin.join("binaries/plugin");
    fs::write(
        &entrypoint,
        b"#!/bin/sh\nwhile IFS= read -r line; do id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p'); printf '{\"id\":\"%s\",\"result\":{\"ok\":true}}\\n' \"$id\"; done\n",
    )
    .expect("plugin process should be written");
    fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o700))
        .expect("plugin process should be executable");
    write_manifest(&plugin, "binaries/plugin");
    write_trusted_keys(root.path());

    let catalog = PluginCatalog::discover(root.path());
    let supervisor = PluginSupervisor::new(catalog);
    let result = supervisor
        .call("org.lux.example", "plugin.health", json!({}))
        .await
        .expect("plugin call should succeed");

    assert_eq!(result["ok"], true);
    assert!(supervisor.status("org.lux.example").await.running);
    supervisor.stop_all().await;
    assert!(!supervisor.status("org.lux.example").await.running);
}
