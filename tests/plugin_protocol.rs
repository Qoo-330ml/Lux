use luxd::application::plugin_protocol::{
    PLUGIN_API_VERSION, PLUGIN_FORMAT_VERSION, PluginManifest, PluginRequest,
};
use serde_json::json;

#[test]
fn accepts_a_versioned_process_plugin_manifest() {
    let manifest = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.example",
        "name": "Example plugin",
        "description": "A test plugin",
        "version": "1.2.3",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {
            "kind": "process",
            "entrypoint": "binaries/${platform}-${arch}/plugin"
        },
        "type": "metadata",
        "supportedItemTypes": ["Movie"],
        "capabilities": ["metadata.search"],
        "configFields": [],
        "permissions": {
            "network": [],
            "filesystem": ["plugin-cache"]
        },
        "files": [],
        "signature": {
            "algorithm": "ed25519",
            "keyId": "test",
            "value": "test-signature"
        }
    }))
    .expect("manifest should validate");

    assert_eq!(manifest.id, "org.lux.example");
    assert_eq!(manifest.version, "1.2.3");
    assert_eq!(manifest.category, "SCRAPER");
    assert_eq!(manifest.runtime.kind, "process");
}

#[test]
fn accepts_a_versioned_process_plugin_manifest_without_a_signature() {
    let manifest = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.unsigned",
        "name": "Unsigned plugin",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "metadata",
        "supportedItemTypes": [],
        "capabilities": [],
        "configFields": [],
        "permissions": {"network": [], "filesystem": []},
        "files": []
    }))
    .expect("manifest without a signature should validate");

    assert!(manifest.signature.is_none());
}

#[test]
fn rejects_manifest_entrypoints_that_escape_the_package() {
    let error = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.example",
        "name": "Example plugin",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "../run.sh"},
        "type": "metadata",
        "supportedItemTypes": [],
        "capabilities": [],
        "configFields": [],
        "permissions": {"network": [], "filesystem": []},
        "files": [],
        "signature": {"algorithm": "ed25519", "keyId": "test", "value": "sig"}
    }))
    .expect_err("path traversal must be rejected");

    assert!(error.to_string().contains("entrypoint"));
}

#[test]
fn encodes_plugin_rpc_requests_without_secrets_in_the_envelope() {
    let request = PluginRequest::new("request-1", "plugin.health", json!({}));
    let value = serde_json::to_value(request).expect("request should serialize");

    assert_eq!(value["id"], "request-1");
    assert_eq!(value["method"], "plugin.health");
    assert!(value.get("apiKey").is_none());
}
