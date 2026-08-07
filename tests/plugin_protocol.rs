use luxd::application::plugin_protocol::{
    IP_LOCATION_CAPABILITY, PLUGIN_API_VERSION, PLUGIN_CATEGORY_NETWORK,
    PLUGIN_FORMAT_VERSION, PLUGIN_TYPE_IP_LOCATION, IpLocationRpcResult, PluginManifest,
    PluginRequest,
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
fn accepts_a_media_probe_plugin_manifest() {
    let manifest = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.strm-media-info",
        "name": "Media information probe",
        "description": "Probes STRM media sources with ffprobe",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {
            "kind": "process",
            "entrypoint": "binaries/${platform}-${arch}/lux-plugin-strm-media-info"
        },
        "type": "media_probe",
        "category": "MEDIA",
        "supportedItemTypes": [],
        "capabilities": ["media.probe"],
        "configFields": [
            {
                "key": "libraryIds",
                "label": "媒体库",
                "type": "select",
                "multiple": true,
                "required": true,
                "optionsSource": "media-libraries"
            },
            {
                "key": "concurrency",
                "label": "并发数",
                "type": "number",
                "defaultValue": 2,
                "minimum": 1,
                "maximum": 64
            }
        ],
        "permissions": {
            "network": ["media-source"],
            "filesystem": []
        },
        "files": []
    }))
    .expect("media probe manifest should validate");

    assert_eq!(manifest.plugin_type, "media_probe");
    assert_eq!(manifest.category, "MEDIA");
    assert_eq!(manifest.capabilities, vec!["media.probe"]);
    assert_eq!(
        manifest.config_fields[0].options_source.as_deref(),
        Some("media-libraries")
    );
    assert_eq!(manifest.config_fields[1].input_type, "number");
    assert_eq!(manifest.config_fields[1].default_value, Some(json!(2)));
}

#[test]
fn accepts_an_ip_location_plugin_manifest_and_result() {
    let manifest = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.ip-hiofd",
        "name": "IP归属地查询增强",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": PLUGIN_TYPE_IP_LOCATION,
        "category": PLUGIN_CATEGORY_NETWORK,
        "capabilities": [IP_LOCATION_CAPABILITY],
        "permissions": {"network": ["toola.hiofd.com"]},
        "files": []
    }))
    .expect("ip location manifest should validate");

    let result: IpLocationRpcResult = serde_json::from_value(json!({
        "ip": "8.8.8.8",
        "country": "美国",
        "province": "加利福尼亚州",
        "city": "山景城",
        "isp": "Google",
        "latitude": null,
        "longitude": null
    }))
    .expect("ip location result should deserialize");

    assert_eq!(manifest.plugin_type, PLUGIN_TYPE_IP_LOCATION);
    assert_eq!(manifest.category, PLUGIN_CATEGORY_NETWORK);
    assert_eq!(manifest.capabilities, vec![IP_LOCATION_CAPABILITY]);
    assert_eq!(result.ip, "8.8.8.8");
    assert_eq!(result.city.as_deref(), Some("山景城"));
}

#[test]
fn rejects_an_ip_location_manifest_without_the_network_capability() {
    let error = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.ip-invalid",
        "name": "Invalid IP plugin",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": PLUGIN_TYPE_IP_LOCATION,
        "category": PLUGIN_CATEGORY_NETWORK,
        "capabilities": [],
        "files": []
    }))
    .expect_err("ip location plugin capability must be declared");

    assert!(error.to_string().contains("ip.location"));
}

#[test]
fn accepts_select_config_fields_with_options_and_multiple_selection() {
    let manifest = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.languages",
        "name": "Language plugin",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "metadata",
        "configFields": [{
            "key": "languages",
            "label": "Languages",
            "type": "select",
            "multiple": true,
            "options": [
                {"value": "zh-CN", "label": "简体中文"},
                {"value": "en-US", "label": "English"}
            ]
        }]
    }))
    .expect("select config field should validate");

    let field = &manifest.config_fields[0];
    assert_eq!(field.input_type, "select");
    assert!(field.multiple);
    assert_eq!(field.options[0].value, "zh-CN");
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
