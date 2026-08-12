use luxd::application::plugin_protocol::{
    CHAPTER_DETECT_CAPABILITY, CHAPTER_LOOKUP_CAPABILITY, ChapterDetectMarkerType,
    ChapterDetectRpcRequest, ChapterDetectRpcResult, ChapterFingerprintRpcEpisode,
    ChapterLookupRpcEpisode, ChapterLookupRpcRequest, IP_LOCATION_CAPABILITY, IpLocationRpcResult,
    MediaProbeRpcResult, PLUGIN_API_VERSION, PLUGIN_CATEGORY_MEDIA, PLUGIN_CATEGORY_NETWORK,
    PLUGIN_FORMAT_VERSION, PLUGIN_TYPE_CHAPTER_DETECTOR, PLUGIN_TYPE_IP_LOCATION,
    PLUGIN_TYPE_STRM_RESOLVER, PluginManifest, PluginRequest, STRM_RESOLVE_CAPABILITY,
    StrmResolveRpcRequest, StrmResolveRpcResult, StrmResolveStatus,
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
fn accepts_a_chapter_detector_manifest_and_bounded_rpc_contract() {
    let manifest = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.intro-outro-detector",
        "name": "Intro and outro detector",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": PLUGIN_TYPE_CHAPTER_DETECTOR,
        "category": PLUGIN_CATEGORY_MEDIA,
        "capabilities": [CHAPTER_DETECT_CAPABILITY],
        "permissions": {"network": [], "filesystem": []},
        "files": []
    }))
    .expect("chapter detector manifest should validate");
    let request = ChapterDetectRpcRequest {
        episodes: vec![
            ChapterFingerprintRpcEpisode {
                key: "episode-a".to_owned(),
                sample_rate: 11_025,
                fingerprint_point_duration_ticks: 1_238_095,
                intro_fingerprint_base64: "AQID".to_owned(),
                credits_fingerprint_base64: "BAUG".to_owned(),
                intro_window_start_ticks: 0,
                credits_window_start_ticks: 900_000_000,
                intro_window_duration_ticks: 1_800_000_000,
                credits_window_duration_ticks: 1_800_000_000,
            },
            ChapterFingerprintRpcEpisode {
                key: "episode-b".to_owned(),
                sample_rate: 11_025,
                fingerprint_point_duration_ticks: 1_238_095,
                intro_fingerprint_base64: "AQID".to_owned(),
                credits_fingerprint_base64: "BAUG".to_owned(),
                intro_window_start_ticks: 0,
                credits_window_start_ticks: 900_000_000,
                intro_window_duration_ticks: 1_800_000_000,
                credits_window_duration_ticks: 1_800_000_000,
            },
        ],
        intro_window_ticks: 1_800_000_000,
        credits_window_ticks: 1_800_000_000,
        minimum_match_duration_ticks: 100_000_000,
        match_threshold: 0.8,
    };
    let value = serde_json::to_value(&request).expect("request should serialize");
    assert!(value.get("mediaSourceId").is_none());
    assert!(value.get("path").is_none());
    let mut request_with_path = value.clone();
    request_with_path["path"] = json!("/media/episode.mkv");
    assert!(serde_json::from_value::<ChapterDetectRpcRequest>(request_with_path).is_err());
    let result: ChapterDetectRpcResult = serde_json::from_value(json!({
        "markers": [{
            "key": "episode-a",
            "markerType": "INTRO_START",
            "startPositionTicks": 10000000,
            "confidence": 0.93
        }]
    }))
    .expect("result should deserialize");
    assert_eq!(
        result.markers[0].marker_type,
        ChapterDetectMarkerType::IntroStart
    );
    assert_eq!(manifest.plugin_type, PLUGIN_TYPE_CHAPTER_DETECTOR);
}

#[test]
fn accepts_a_metadata_lookup_chapter_contract_without_media_paths() {
    let manifest = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.theintrodb-chapter-source",
        "name": "TheIntroDB chapter source",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": PLUGIN_TYPE_CHAPTER_DETECTOR,
        "category": PLUGIN_CATEGORY_MEDIA,
        "capabilities": [CHAPTER_LOOKUP_CAPABILITY],
        "permissions": {"network": ["api.theintrodb.org"], "filesystem": []},
        "files": []
    }))
    .expect("metadata lookup manifest should validate");
    let request = ChapterLookupRpcRequest {
        episodes: vec![ChapterLookupRpcEpisode {
            key: "episode-a".to_owned(),
            tmdb_id: Some(123),
            tvdb_id: Some(456),
            imdb_id: Some("tt1234567".to_owned()),
            season_number: 1,
            episode_number: 2,
            duration_ticks: Some(1_800_000_000),
        }],
    };
    let value = serde_json::to_value(request).expect("request should serialize");
    assert!(value.get("path").is_none());
    assert!(value.get("url").is_none());
    assert!(value.get("mediaSourceId").is_none());
    assert_eq!(value["episodes"][0]["tmdbId"], 123);
    assert_eq!(value["episodes"][0]["seasonNumber"], 1);
    let mut request_with_path = value;
    request_with_path["episodes"][0]["path"] = json!("/media/episode.mkv");
    assert!(serde_json::from_value::<ChapterLookupRpcRequest>(request_with_path).is_err());
    assert_eq!(manifest.capabilities, vec![CHAPTER_LOOKUP_CAPABILITY]);
}

#[test]
fn media_probe_result_can_carry_a_thumbnail() {
    let result = MediaProbeRpcResult {
        container: Some("matroska".to_owned()),
        source_size: None,
        duration_ticks: Some(10_000_000),
        bitrate: None,
        streams: Vec::new(),
        thumbnail_jpeg_base64: Some("/9j/test".to_owned()),
    };
    let value = serde_json::to_value(result).expect("media probe result should serialize");
    assert_eq!(value["thumbnailJpegBase64"], "/9j/test");
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
fn accepts_a_generic_strm_resolver_manifest_and_contract() {
    let manifest = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.example-resolver",
        "name": "Generic STRM resolver",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": PLUGIN_TYPE_STRM_RESOLVER,
        "category": PLUGIN_CATEGORY_MEDIA,
        "capabilities": [STRM_RESOLVE_CAPABILITY],
        "permissions": {"network": ["resolver.example"]},
        "files": []
    }))
    .expect("STRM resolver manifest should validate");

    let request = StrmResolveRpcRequest {
        target: "/opaque/or/path target.mp4".to_owned(),
    };
    let request_value = serde_json::to_value(request).expect("resolver request should serialize");
    assert_eq!(
        request_value,
        json!({"target": "/opaque/or/path target.mp4"})
    );

    let result: StrmResolveRpcResult = serde_json::from_value(json!({
        "status": "RESOLVED",
        "url": "https://media.example/direct.mp4"
    }))
    .expect("resolved result should deserialize");
    assert_eq!(result.status, StrmResolveStatus::Resolved);
    assert_eq!(
        result.url.as_deref(),
        Some("https://media.example/direct.mp4")
    );
    assert_eq!(manifest.plugin_type, PLUGIN_TYPE_STRM_RESOLVER);
}

#[test]
fn accepts_an_unsupported_strm_resolver_result_without_a_url() {
    let result: StrmResolveRpcResult = serde_json::from_value(json!({
        "status": "UNSUPPORTED"
    }))
    .expect("unsupported result should deserialize");

    assert_eq!(result.status, StrmResolveStatus::Unsupported);
    assert!(result.url.is_none());
}

#[test]
fn rejects_a_strm_resolver_manifest_without_its_capability() {
    let error = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.invalid-resolver",
        "name": "Invalid STRM resolver",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": PLUGIN_TYPE_STRM_RESOLVER,
        "category": PLUGIN_CATEGORY_MEDIA,
        "capabilities": [],
        "files": []
    }))
    .expect_err("STRM resolver capability must be declared");

    assert!(error.to_string().contains("strm.resolve"));
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
