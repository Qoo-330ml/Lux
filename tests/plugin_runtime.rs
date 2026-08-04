use std::{fs, io::Write, path::Path};

use luxd::application::plugin_runtime::PluginCatalog;
use serde_json::json;
use tempfile::tempdir;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

fn manifest_bytes(entrypoint: &str) -> Vec<u8> {
    serde_json::to_vec_pretty(&json!({
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
        "signature": {"algorithm": "ed25519", "keyId": "test", "value": "signature"}
    }))
    .expect("manifest should serialize")
}

fn write_manifest(path: &Path, entrypoint: &str) {
    fs::write(path.join("manifest.json"), manifest_bytes(entrypoint))
        .expect("manifest should be written");
}

#[test]
fn discovers_an_exploded_plugin_directory() {
    let root = tempdir().expect("temp dir should be created");
    let plugin = root.path().join("example");
    fs::create_dir_all(plugin.join("binaries")).expect("plugin directory should be created");
    fs::write(plugin.join("binaries/plugin"), b"plugin").expect("entrypoint should be written");
    write_manifest(&plugin, "binaries/plugin");

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
