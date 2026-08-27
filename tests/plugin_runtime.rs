use std::{fs, io::Write, path::Path};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signer, SigningKey};
use luxd::application::{plugin_protocol::PluginManifest, plugin_runtime::PluginCatalog};
use serde_json::{Value, json};
use tempfile::{Builder, tempdir};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

fn test_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7_u8; 32])
}

fn signed_manifest_value_with_version(entrypoint: &str, version: &str) -> Value {
    let mut value = json!({
        "formatVersion": 1,
        "id": "org.lux.example",
        "name": "Example plugin",
        "version": version,
        "apiVersion": 1,
        "runtime": {"kind": "process", "entrypoint": entrypoint},
        "type": "metadata",
        "providerKey": "example-provider",
        "aliases": ["legacy-example"],
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

fn signed_manifest_value(entrypoint: &str) -> Value {
    signed_manifest_value_with_version(entrypoint, "1.0.0")
}

fn manifest_bytes(entrypoint: &str) -> Vec<u8> {
    serde_json::to_vec_pretty(&signed_manifest_value(entrypoint))
        .expect("manifest should serialize")
}

fn manifest_bytes_with_version(entrypoint: &str, version: &str) -> Vec<u8> {
    serde_json::to_vec_pretty(&signed_manifest_value_with_version(entrypoint, version))
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
fn resolves_a_manifest_alias_to_the_installed_plugin() {
    let root = tempdir().expect("temp dir should be created");
    let plugin = root.path().join("example");
    fs::create_dir_all(plugin.join("binaries")).expect("plugin directory should be created");
    fs::write(plugin.join("binaries/plugin"), b"plugin").expect("entrypoint should be written");
    write_manifest(&plugin, "binaries/plugin");

    let catalog = PluginCatalog::discover(root.path());

    assert_eq!(
        catalog
            .get_by_alias("legacy-example")
            .map(|plugin| plugin.manifest.id.as_str()),
        Some("org.lux.example")
    );
}

#[test]
fn resolves_a_manifest_provider_key_to_the_installed_plugin() {
    let root = tempdir().expect("temp dir should be created");
    let plugin = root.path().join("example");
    fs::create_dir_all(plugin.join("binaries")).expect("plugin directory should be created");
    fs::write(plugin.join("binaries/plugin"), b"plugin").expect("entrypoint should be written");
    write_manifest(&plugin, "binaries/plugin");

    let catalog = PluginCatalog::discover(root.path());

    assert_eq!(
        catalog
            .get_by_provider_key("example-provider")
            .map(|plugin| plugin.manifest.id.as_str()),
        Some("org.lux.example")
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
fn prefers_the_requested_new_version_when_old_and_new_packages_coexist() {
    let root = tempdir().expect("temp dir should be created");
    let old = root.path().join("old");
    fs::create_dir_all(old.join("binaries")).expect("old plugin directory should be created");
    fs::write(old.join("binaries/plugin"), b"old").expect("old entrypoint should be written");
    fs::write(
        old.join("manifest.json"),
        manifest_bytes_with_version("binaries/plugin", "1.0.0"),
    )
    .expect("old manifest should be written");

    let archive_path = root.path().join("org.lux.example-1.1.0.zip");
    let file = fs::File::create(&archive_path).expect("archive should be created");
    let mut archive = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    archive
        .start_file("manifest.json", options)
        .expect("manifest entry should be created");
    archive
        .write_all(&manifest_bytes_with_version("binaries/plugin", "1.1.0"))
        .expect("manifest should be written");
    archive
        .start_file("binaries/plugin", options)
        .expect("entrypoint entry should be created");
    archive
        .write_all(b"new")
        .expect("entrypoint should be written");
    archive.finish().expect("archive should finish");

    let catalog = PluginCatalog::discover_prefer(root.path(), "org.lux.example", "1.1.0");

    assert_eq!(catalog.plugins.len(), 1);
    assert_eq!(catalog.plugins[0].manifest.version, "1.1.0");
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

#[cfg(unix)]
#[tokio::test]
async fn removes_a_plugin_process_after_natural_exit() {
    use std::os::unix::fs::PermissionsExt;

    use luxd::application::plugin_runtime::PluginSupervisor;

    let root = tempdir().expect("temp dir should be created");
    let plugin = root.path().join("example");
    fs::create_dir_all(plugin.join("binaries")).expect("plugin directory should be created");
    let entrypoint = plugin.join("binaries/plugin");
    fs::write(
        &entrypoint,
        b"#!/bin/sh
IFS= read -r line || exit 1
id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p')
printf '{\"id\":\"%s\",\"result\":{\"ok\":true}}\\n' \"$id\"
",
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
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!supervisor.status("org.lux.example").await.running);
}

#[cfg(unix)]
#[tokio::test]
async fn multiplexes_concurrent_plugin_calls_and_matches_out_of_order_responses() {
    use std::os::unix::fs::PermissionsExt;

    use luxd::application::plugin_runtime::PluginSupervisor;

    let root = tempdir().expect("temp dir should be created");
    let plugin = root.path().join("example");
    fs::create_dir_all(plugin.join("binaries")).expect("plugin directory should be created");
    let entrypoint = plugin.join("binaries/plugin");
    fs::write(
        &entrypoint,
        b"#!/bin/sh
first=
while IFS= read -r line; do
  id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p')
  marker=$(printf '%s' \"$line\" | sed -n 's/.*\"n\":\\([0-9]*\\).*/\\1/p')
  if [ -z \"$first\" ]; then
    first=$id
    first_marker=$marker
  else
    printf '{\"id\":\"%s\",\"result\":{\"marker\":\"%s\"}}\\n' \"$id\" \"$marker\"
    printf '{\"id\":\"%s\",\"result\":{\"marker\":\"%s\"}}\\n' \"$first\" \"$first_marker\"
    first=
    first_marker=
  fi
done
",
    )
    .expect("plugin process should be written");
    fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o700))
        .expect("plugin process should be executable");
    write_manifest(&plugin, "binaries/plugin");
    write_trusted_keys(root.path());

    let catalog = PluginCatalog::discover(root.path());
    let supervisor = PluginSupervisor::new(catalog);
    let first = supervisor.call("org.lux.example", "plugin.health", json!({"n": 1}));
    let second = supervisor.call("org.lux.example", "plugin.health", json!({"n": 2}));
    let (first, second) = tokio::join!(first, second);

    let first = first.expect("first concurrent call should succeed");
    let second = second.expect("second concurrent call should succeed");
    assert_eq!(first["marker"], "1");
    assert_eq!(second["marker"], "2");
    supervisor.stop_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn keeps_plugin_process_alive_for_request_level_errors() {
    use std::os::unix::fs::PermissionsExt;

    use luxd::application::plugin_runtime::PluginSupervisor;

    let root = tempdir().expect("temp dir should be created");
    let plugin = root.path().join("example");
    fs::create_dir_all(plugin.join("binaries")).expect("plugin directory should be created");
    let entrypoint = plugin.join("binaries/plugin");
    fs::write(
        &entrypoint,
        b"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p')
  case \"$line\" in
    *fail*true*) printf '{\"id\":\"%s\",\"error\":{\"code\":\"PLUGIN_PROVIDER_ERROR\",\"message\":\"temporary\"}}\\n' \"$id\" ;;
    *) printf '{\"id\":\"%s\",\"result\":{\"ok\":true}}\\n' \"$id\" ;;
  esac
done
",
    )
    .expect("plugin process should be written");
    fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o700))
        .expect("plugin process should be executable");
    write_manifest(&plugin, "binaries/plugin");
    write_trusted_keys(root.path());

    let catalog = PluginCatalog::discover(root.path());
    let supervisor = PluginSupervisor::new(catalog);
    let failure = supervisor.call("org.lux.example", "metadata.get", json!({"fail": true}));
    let success = supervisor.call("org.lux.example", "plugin.health", json!({}));
    let (failure, success) = tokio::join!(failure, success);

    assert!(failure.is_err());
    assert_eq!(
        success.expect("success request should complete")["ok"],
        true
    );
    assert!(supervisor.status("org.lux.example").await.running);
    supervisor.stop_all().await;
}

#[cfg(unix)]
#[tokio::test]
async fn supervises_a_plugin_process_when_catalog_paths_are_relative()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    use luxd::application::plugin_runtime::PluginSupervisor;

    let temporary_root = Builder::new()
        .prefix("lux-plugin-runtime-")
        .tempdir_in(".")?;
    let current_dir = std::env::current_dir()?;
    let relative_root = temporary_root
        .path()
        .strip_prefix(&current_dir)
        .map(Path::to_owned)?;
    let plugin = relative_root.join("example");
    fs::create_dir_all(plugin.join("binaries"))?;
    let entrypoint = plugin.join("binaries/plugin");
    fs::write(
        &entrypoint,
        b"#!/bin/sh\ncase \"$LUX_PLUGIN_CONFIG_PATH\" in /*) config_absolute=true;; *) config_absolute=false;; esac\nwhile IFS= read -r line; do id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p'); printf '{\"id\":\"%s\",\"result\":{\"ok\":true,\"configAbsolute\":%s}}\\n' \"$id\" \"$config_absolute\"; done\n",
    )?;
    fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o700))?;
    write_manifest(&plugin, "binaries/plugin");
    write_trusted_keys(&relative_root);

    let catalog = PluginCatalog::discover(&relative_root);
    let supervisor = PluginSupervisor::new(catalog).with_config_dir(relative_root.join("config"));
    let result = supervisor
        .call("org.lux.example", "plugin.health", json!({}))
        .await?;

    assert_eq!(result["ok"], true);
    assert_eq!(result["configAbsolute"], true);
    supervisor.stop_all().await;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn metadata_plugins_receive_only_their_dedicated_config_path()
-> Result<(), Box<dyn std::error::Error>> {
    use luxd::application::plugin_runtime::PluginSupervisor;
    use std::os::unix::fs::PermissionsExt;

    let root = tempdir()?;
    let plugin = root.path().join("example");
    fs::create_dir_all(plugin.join("binaries"))?;
    let entrypoint = plugin.join("binaries/plugin");
    fs::write(
        &entrypoint,
        b"#!/bin/sh\ncase \"$LUX_PLUGIN_CONFIG_PATH\" in\n  */org.lux.example.json) plugin_path=true ;;\n  *) plugin_path=false ;;\nesac\nif printenv LUX_CONFIG_DIR >/dev/null 2>&1; then shared_root=true; else shared_root=false; fi\nwhile IFS= read -r line; do id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p'); printf '{\"id\":\"%s\",\"result\":{\"pluginPath\":%s,\"sharedRoot\":%s}}\\n' \"$id\" \"$plugin_path\" \"$shared_root\"; done\n",
    )?;
    fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o700))?;
    write_manifest(&plugin, "binaries/plugin");
    write_trusted_keys(root.path());

    let catalog = PluginCatalog::discover(root.path());
    let supervisor = PluginSupervisor::new(catalog).with_config_dir(root.path().join("config"));
    let result = supervisor
        .call("org.lux.example", "plugin.health", json!({}))
        .await?;

    assert_eq!(result["pluginPath"], true);
    assert_eq!(result["sharedRoot"], false);
    supervisor.stop_all().await;
    Ok(())
}
