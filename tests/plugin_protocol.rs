use luxd::application::plugin_protocol::{
    CHAPTER_DETECT_CAPABILITY, CHAPTER_LOOKUP_CAPABILITY, ChapterDetectMarkerType,
    ChapterDetectRpcRequest, ChapterDetectRpcResult, ChapterFingerprintRpcEpisode,
    ChapterLookupRpcEpisode, ChapterLookupRpcRequest, DANMAKU_MATCH_CAPABILITY,
    DANMAKU_MATCH_METHOD, DanmakuMatchRpcRequest, DanmakuMatchRpcResult, DanmakuMatchStatus,
    IP_LOCATION_CAPABILITY, IpLocationRpcResult, LoginBackgroundContentKind,
    LoginBackgroundRpcResult, LoginBackgroundRpcValidationError, MediaProbeRpcResult,
    PLUGIN_API_VERSION, PLUGIN_CATEGORY_MEDIA, PLUGIN_CATEGORY_NETWORK,
    PLUGIN_CATEGORY_NOTIFICATION, PLUGIN_FORMAT_VERSION, PLUGIN_TYPE_CHAPTER_DETECTOR,
    PLUGIN_TYPE_DANMAKU, PLUGIN_TYPE_IP_LOCATION, PLUGIN_TYPE_STRM_RESOLVER, PluginManifest,
    PluginRequest, STRM_RESOLVE_CAPABILITY, StrmResolveRpcRequest, StrmResolveRpcResult,
    StrmResolveStatus,
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
        "providerKey": "imdb",
        "aliases": ["legacy-imdb"],
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
    assert_eq!(manifest.provider_key.as_deref(), Some("imdb"));
    assert_eq!(manifest.aliases, vec!["legacy-imdb"]);
    assert_eq!(manifest.runtime.kind, "process");
}

#[test]
fn accepts_a_multiline_notification_config_field() {
    let manifest = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.webhook",
        "name": "Webhook notifier",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "notification",
        "category": PLUGIN_CATEGORY_NOTIFICATION,
        "capabilities": ["notification.send"],
        "configFields": [{
            "key": "bodyTemplate",
            "label": "Body template",
            "type": "textarea",
            "required": false
        }],
        "permissions": {"network": [], "filesystem": []},
        "files": []
    }))
    .expect("textarea config field should validate");

    assert_eq!(manifest.config_fields[0].input_type, "textarea");
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
fn accepts_a_login_background_manifest_with_declared_image_hosts() {
    let manifest = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.login-background-example",
        "name": "Login background example",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "login_background",
        "category": "UTILITY",
        "capabilities": ["login_background.get"],
        "permissions": {
            "network": ["api.example.com"],
            "imageHosts": ["images.example.com"],
            "filesystem": []
        },
        "files": []
    }))
    .expect("login background manifest should validate");

    assert_eq!(manifest.plugin_type, "login_background");
    assert_eq!(manifest.category, "UTILITY");
    assert_eq!(manifest.capabilities, ["login_background.get"]);
    assert_eq!(manifest.permissions.image_hosts, ["images.example.com"]);
}

#[test]
fn rejects_login_background_capability_on_metadata_plugins() {
    let error = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.invalid-login-background",
        "name": "Invalid metadata background plugin",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "metadata",
        "capabilities": ["metadata.search", "login_background.get"],
        "permissions": {"network": [], "filesystem": []},
        "files": []
    }))
    .expect_err("metadata plugin must not claim login background capability");

    assert!(error.to_string().contains("login_background.get"));
}

#[test]
fn rejects_invalid_login_background_manifest_category_capabilities_and_hosts() {
    let valid = json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.login-background-test",
        "name": "Login background test",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "login_background",
        "category": "UTILITY",
        "capabilities": ["login_background.get"],
        "permissions": {"imageHosts": ["images.example.com"]},
        "files": []
    });

    for (key, value) in [
        ("category", json!("MEDIA")),
        (
            "capabilities",
            json!(["login_background.get", "metadata.search"]),
        ),
        ("permissions", json!({"imageHosts": ["localhost"]})),
        ("permissions", json!({"imageHosts": ["10.0.0.1"]})),
        ("permissions", json!({"imageHosts": ["*.example.com"]})),
        ("permissions", json!({"imageHosts": ["single-label"]})),
        (
            "permissions",
            json!({"imageHosts": ["image_host.example.com"]}),
        ),
        (
            "permissions",
            json!({"imageHosts": ["-images.example.com"]}),
        ),
        (
            "permissions",
            json!({"imageHosts": ["images-.example.com"]}),
        ),
        (
            "permissions",
            json!({"imageHosts": ["images.example.com:8443"]}),
        ),
        ("permissions", json!({})),
    ] {
        let mut invalid = valid.clone();
        invalid[key] = value;
        assert!(
            PluginManifest::from_value(invalid).is_err(),
            "invalid login background manifest field {key} should be rejected"
        );
    }

    let invalid_other_type = json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.invalid-media-probe-background",
        "name": "Invalid media probe background",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "media_probe",
        "category": "MEDIA",
        "capabilities": ["media.probe", "login_background.get"],
        "permissions": {"imageHosts": ["images.example.com"]},
        "files": []
    });
    assert!(PluginManifest::from_value(invalid_other_type).is_err());
}

#[test]
fn accepts_versioned_login_background_sdk_manifest_and_result_fixtures() {
    let manifest_value = serde_json::from_str(include_str!(
        "fixtures/plugin-sdk/login-background/manifest-v1.json"
    ))
    .expect("manifest fixture should be JSON");
    let manifest = PluginManifest::from_value(manifest_value)
        .expect("login background manifest fixture should validate");
    let result_value = serde_json::from_str(include_str!(
        "fixtures/plugin-sdk/login-background/poster-feed-v1.json"
    ))
    .expect("RPC result fixture should be JSON");
    let result = LoginBackgroundRpcResult::validate(result_value, &manifest)
        .expect("login background RPC fixture should validate");

    assert_eq!(result.content_kind, LoginBackgroundContentKind::PosterFeed);
    assert_eq!(result.source_name, "Example catalog");
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].title.as_deref(), Some("Example film"));

    let single_image_value = serde_json::from_str(include_str!(
        "fixtures/plugin-sdk/login-background/single-image-v1.json"
    ))
    .expect("single image RPC fixture should be JSON");
    let single_image = LoginBackgroundRpcResult::validate(single_image_value, &manifest)
        .expect("single image RPC fixture should validate");
    assert_eq!(single_image.content_kind, LoginBackgroundContentKind::SingleImage);
    assert_eq!(single_image.items.len(), 1);
    assert!(single_image.items[0].attribution_url.is_some());
    assert!(single_image.items[0].license_url.is_some());
}

#[test]
fn preserves_legacy_manifest_serialization_and_reserves_image_hosts() {
    let legacy_manifest = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.legacy",
        "name": "Legacy plugin",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "metadata",
        "permissions": {"network": [], "filesystem": []},
        "files": []
    }))
    .expect("legacy plugin manifest should validate");
    let serialized =
        serde_json::to_value(legacy_manifest).expect("legacy manifest should serialize");
    assert!(serialized["permissions"].get("imageHosts").is_none());

    let metadata_with_image_hosts = json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.invalid-metadata-images",
        "name": "Invalid metadata image host declaration",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "metadata",
        "permissions": {"imageHosts": ["images.example.com"]},
        "files": []
    });
    assert!(PluginManifest::from_value(metadata_with_image_hosts).is_err());
}

#[test]
fn accepts_exactly_one_item_for_a_hero_image_result() {
    let manifest = login_background_test_manifest();
    let result = LoginBackgroundRpcResult::validate(
        json!({
            "contentKind": "HERO_IMAGE",
            "sourceName": "Bing",
            "items": [{"imageUrl": "https://images.example.com/today.jpg"}]
        }),
        &manifest,
    )
    .expect("one hero image should validate");

    assert_eq!(result.content_kind, LoginBackgroundContentKind::HeroImage);
    assert_eq!(result.items.len(), 1);
}

#[test]
fn accepts_exactly_one_item_for_a_single_poster_result() {
    let manifest = login_background_test_manifest();
    let result = LoginBackgroundRpcResult::validate(
        json!({
            "contentKind": "SINGLE_POSTER",
            "sourceName": "TMDb Daily Trending",
            "items": [{"imageUrl": "https://images.example.com/poster.jpg"}]
        }),
        &manifest,
    )
    .expect("a single poster result should validate");

    assert_eq!(result.items.len(), 1);
    assert_eq!(
        serde_json::to_value(result).expect("single poster result should serialize")["contentKind"],
        "SINGLE_POSTER"
    );

    for items in [
        json!([]),
        json!([
            {"imageUrl": "https://images.example.com/first.jpg"},
            {"imageUrl": "https://images.example.com/second.jpg"}
        ]),
    ] {
        let error = LoginBackgroundRpcResult::validate(
            json!({
                "contentKind": "SINGLE_POSTER",
                "sourceName": "TMDb Daily Trending",
                "items": items
            }),
            &manifest,
        )
        .expect_err("single poster results must contain exactly one item");
        assert_eq!(error, LoginBackgroundRpcValidationError::InvalidItemCount);
    }
}

#[test]
fn accepts_a_single_original_image_with_allowlisted_attribution_links() {
    let manifest = login_background_test_manifest();
    let result = LoginBackgroundRpcResult::validate(
        json!({
            "contentKind": "SINGLE_IMAGE",
            "sourceName": "Wikimedia Commons · Picture of the Day",
            "items": [{
                "imageUrl": "https://images.example.com/today.jpg",
                "title": "Example wildlife photograph",
                "copyrightNotice": "By Example Photographer · CC BY-SA 4.0",
                "attributionUrl": "https://commons.wikimedia.org/wiki/File:Example.jpg",
                "licenseUrl": "https://creativecommons.org/licenses/by-sa/4.0/"
            }]
        }),
        &manifest,
    )
    .expect("a single image with declared attribution links should validate");

    assert_eq!(result.content_kind, LoginBackgroundContentKind::SingleImage);
    assert_eq!(result.items.len(), 1);
    assert_eq!(
        result.items[0].attribution_url.as_deref(),
        Some("https://commons.wikimedia.org/wiki/File:Example.jpg")
    );
    assert_eq!(
        result.items[0].license_url.as_deref(),
        Some("https://creativecommons.org/licenses/by-sa/4.0/")
    );
}

#[test]
fn rejects_unlisted_or_insecure_attribution_links() {
    let manifest = login_background_test_manifest();
    for (field, url) in [
        ("attributionUrl", "http://commons.wikimedia.org/wiki/File:Example.jpg"),
        ("attributionUrl", "https://attacker.invalid/File:Example.jpg"),
        ("licenseUrl", "https://attacker.invalid/license"),
        ("licenseUrl", "https://user:pass@creativecommons.org/licenses/by/4.0/"),
    ] {
        let mut item = json!({"imageUrl": "https://images.example.com/today.jpg"});
        item[field] = json!(url);
        let error = LoginBackgroundRpcResult::validate(
            json!({
                "contentKind": "SINGLE_IMAGE",
                "sourceName": "Wikimedia Commons",
                "items": [item]
            }),
            &manifest,
        )
        .expect_err("untrusted attribution links must be rejected");
        assert_eq!(error, LoginBackgroundRpcValidationError::InvalidAttributionUrl);
    }
}

#[test]
fn rejects_invalid_login_background_rpc_shapes_and_urls() {
    let manifest = login_background_test_manifest();
    let base = json!({
        "contentKind": "POSTER_FEED",
        "sourceName": "Example catalog",
        "items": [{"imageUrl": "https://images.example.com/poster.jpg"}]
    });
    let invalid_values = [
        json!({
            "contentKind": "UNKNOWN",
            "sourceName": "Example catalog",
            "items": []
        }),
        json!({
            "contentKind": "POSTER_FEED",
            "sourceName": "Example catalog",
            "items": [],
            "html": "<script>alert(1)</script>"
        }),
        json!({
            "contentKind": "HERO_IMAGE",
            "sourceName": "Example catalog",
            "items": []
        }),
        json!({
            "contentKind": "HERO_IMAGE",
            "sourceName": "Example catalog",
            "items": [
                {"imageUrl": "https://images.example.com/a.jpg"},
                {"imageUrl": "https://images.example.com/b.jpg"}
            ]
        }),
        json!({
            "contentKind": "POSTER_FEED",
            "sourceName": "Example catalog",
            "items": [
                {"imageUrl": "http://images.example.com/poster.jpg"}
            ]
        }),
        json!({
            "contentKind": "POSTER_FEED",
            "sourceName": "Example catalog",
            "items": [
                {"imageUrl": "https://user:password@images.example.com/poster.jpg"}
            ]
        }),
        json!({
            "contentKind": "POSTER_FEED",
            "sourceName": "Example catalog",
            "items": [{"imageUrl": "https://127.0.0.1/poster.jpg"}]
        }),
        json!({
            "contentKind": "POSTER_FEED",
            "sourceName": "Example catalog",
            "items": [{"imageUrl": "https://localhost/poster.jpg"}]
        }),
        json!({
            "contentKind": "POSTER_FEED",
            "sourceName": "Example catalog",
            "items": [{"imageUrl": "https://images.example.com.evil.invalid/poster.jpg"}]
        }),
        json!({
            "contentKind": "POSTER_FEED",
            "sourceName": "Example catalog",
            "items": [{"imageUrl": "https://images.example.com/poster.jpg#fragment"}]
        }),
    ];

    for value in invalid_values {
        assert!(
            LoginBackgroundRpcResult::validate(value, &manifest).is_err(),
            "invalid login background result should be rejected"
        );
    }

    let mut over_limit = base;
    over_limit["items"] = json!(
        (0..=40)
            .map(|index| json!({"imageUrl": format!("https://images.example.com/{index}.jpg")}))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        LoginBackgroundRpcResult::validate(over_limit, &manifest).unwrap_err(),
        LoginBackgroundRpcValidationError::TooManyItems
    );
}

#[test]
fn rejects_oversized_and_control_character_login_background_data() {
    let manifest = login_background_test_manifest();
    let oversized = json!({
        "contentKind": "POSTER_FEED",
        "sourceName": "Example catalog",
        "items": [{
            "imageUrl": format!("https://images.example.com/{}", "x".repeat(260 * 1024))
        }]
    });
    assert_eq!(
        LoginBackgroundRpcResult::validate(oversized, &manifest).unwrap_err(),
        LoginBackgroundRpcValidationError::ResultTooLarge
    );

    let control_character = json!({
        "contentKind": "POSTER_FEED",
        "sourceName": "Example\nCatalog",
        "items": []
    });
    assert_eq!(
        LoginBackgroundRpcResult::validate(control_character, &manifest).unwrap_err(),
        LoginBackgroundRpcValidationError::InvalidText
    );
}

fn login_background_test_manifest() -> PluginManifest {
    PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.login-background-test",
        "name": "Login background test",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "login_background",
        "category": "UTILITY",
        "capabilities": ["login_background.get"],
        "permissions": {
            "network": ["commons.wikimedia.org", "creativecommons.org"],
            "imageHosts": ["images.example.com"]
        },
        "files": []
    }))
    .expect("test login background manifest should validate")
}

#[test]
fn accepts_a_danmaku_plugin_manifest_and_path_free_match_contract() {
    let manifest = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.danmaku",
        "name": "Danmaku provider",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": PLUGIN_TYPE_DANMAKU,
        "category": PLUGIN_CATEGORY_MEDIA,
        "capabilities": [DANMAKU_MATCH_CAPABILITY],
        "permissions": {"network": ["danmaku.example"]},
        "files": []
    }))
    .expect("danmaku manifest should validate");

    let request = DanmakuMatchRpcRequest {
        file_name: "Show.S01E02.1080p.mkv".to_owned(),
        alternate_file_names: Vec::new(),
    };
    let request_value = serde_json::to_value(request).expect("danmaku request should serialize");
    assert_eq!(request_value, json!({"fileName": "Show.S01E02.1080p.mkv"}));
    assert!(request_value.get("path").is_none());
    assert!(request_value.get("url").is_none());
    let mut request_with_path = request_value;
    request_with_path["path"] = json!("/media/Show.S01E02.1080p.mkv");
    assert!(serde_json::from_value::<DanmakuMatchRpcRequest>(request_with_path).is_err());

    let result: DanmakuMatchRpcResult = serde_json::from_value(json!({
        "status": "MATCHED",
        "provider": "dandanplay",
        "animeId": "anime-1",
        "episodeId": "episode-2",
        "xmlBase64": "PD94bWwgdmVyc2lvbj0iMS4wIj8+"
    }))
    .expect("danmaku result should deserialize");
    assert_eq!(manifest.plugin_type, PLUGIN_TYPE_DANMAKU);
    assert_eq!(manifest.capabilities, vec![DANMAKU_MATCH_CAPABILITY]);
    assert_eq!(DANMAKU_MATCH_METHOD, "danmaku.match");
    assert_eq!(result.status, DanmakuMatchStatus::Matched);
    assert_eq!(result.episode_id.as_deref(), Some("episode-2"));
}

#[test]
fn accepts_manifest_scheduled_task_declaration() {
    let manifest = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.scheduled",
        "name": "Scheduled plugin",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "metadata",
        "configFields": [
            {"key": "schedule", "label": "Schedule", "type": "text", "defaultValue": "0 6 * * *"},
            {"key": "libraryIds", "label": "Libraries", "type": "select", "multiple": true, "optionsSource": "media-libraries"}
        ],
        "scheduledTasks": [{
            "taskType": "EXAMPLE_TASK",
            "ownerType": "GLOBAL",
            "name": "Example task",
            "description": "Runs the example task.",
            "scheduleConfigKey": "schedule",
            "defaultSchedule": "0 6 * * *",
            "requiredConfigKeys": ["libraryIds"],
            "resourceLimit": {"concurrency": 2, "overwrite": false}
        }],
        "permissions": {"network": [], "filesystem": []},
        "files": []
    }))
    .expect("manifest scheduled task should validate");

    assert_eq!(manifest.scheduled_tasks.len(), 1);
    assert_eq!(manifest.scheduled_tasks[0].task_type, "EXAMPLE_TASK");
    assert_eq!(manifest.scheduled_tasks[0].owner_type, "GLOBAL");
    assert_eq!(manifest.scheduled_tasks[0].schedule_config_key, "schedule");
    assert_eq!(
        manifest.scheduled_tasks[0].required_config_keys,
        vec!["libraryIds".to_owned()]
    );
    assert_eq!(manifest.scheduled_tasks[0].resource_limit["concurrency"], 2);
}

#[test]
fn rejects_manifest_scheduled_task_with_invalid_owner_or_schedule() {
    let base = json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.scheduled",
        "name": "Scheduled plugin",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": "metadata",
        "configFields": [{"key": "schedule", "label": "Schedule", "type": "text"}],
        "permissions": {"network": [], "filesystem": []},
        "files": []
    });

    let mut invalid_owner = base.clone();
    invalid_owner["scheduledTasks"] = json!([{
        "taskType": "EXAMPLE_TASK",
        "ownerType": "USER",
        "name": "Example task",
        "description": "Runs the example task.",
        "scheduleConfigKey": "schedule",
        "defaultSchedule": "0 6 * * *"
    }]);
    assert!(PluginManifest::from_value(invalid_owner).is_err());

    let mut invalid_schedule = base.clone();
    invalid_schedule["scheduledTasks"] = json!([{
        "taskType": "EXAMPLE_TASK",
        "ownerType": "GLOBAL",
        "name": "Example task",
        "description": "Runs the example task.",
        "scheduleConfigKey": "schedule",
        "defaultSchedule": "0 6 * *"
    }]);
    assert!(PluginManifest::from_value(invalid_schedule).is_err());

    let mut invalid_resource_limit = base;
    invalid_resource_limit["scheduledTasks"] = json!([{
        "taskType": "EXAMPLE_TASK",
        "ownerType": "GLOBAL",
        "name": "Example task",
        "description": "Runs the example task.",
        "scheduleConfigKey": "schedule",
        "defaultSchedule": "0 6 * * *",
        "resourceLimit": {"concurrency": "2"}
    }]);
    assert!(PluginManifest::from_value(invalid_resource_limit).is_err());
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
        "supportedMediaSourceKinds": ["LOCAL_FILE"],
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
    assert_eq!(manifest.supported_media_source_kinds, ["LOCAL_FILE"]);
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
        "supportedMediaSourceKinds": ["LOCAL_FILE", "STRM_URL"],
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
    assert_eq!(
        manifest.supported_media_source_kinds,
        ["LOCAL_FILE", "STRM_URL"]
    );
}

#[test]
fn rejects_chapter_manifest_with_an_unknown_media_source_kind() {
    let error = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.invalid-chapter-source-kind",
        "name": "Invalid chapter source kind",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": PLUGIN_TYPE_CHAPTER_DETECTOR,
        "category": PLUGIN_CATEGORY_MEDIA,
        "supportedMediaSourceKinds": ["REMOTE_STREAM"],
        "capabilities": [CHAPTER_LOOKUP_CAPABILITY],
        "permissions": {"network": [], "filesystem": []},
        "files": []
    }))
    .expect_err("unknown chapter source kind must be rejected");

    assert!(error.to_string().contains("media source kind"));
}

#[test]
fn rejects_a_fingerprint_detector_that_declares_strm_support() {
    let error = PluginManifest::from_value(json!({
        "formatVersion": PLUGIN_FORMAT_VERSION,
        "id": "org.lux.invalid-strm-detector",
        "name": "Invalid STRM detector",
        "version": "1.0.0",
        "apiVersion": PLUGIN_API_VERSION,
        "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
        "type": PLUGIN_TYPE_CHAPTER_DETECTOR,
        "category": PLUGIN_CATEGORY_MEDIA,
        "supportedMediaSourceKinds": ["STRM_URL"],
        "capabilities": [CHAPTER_DETECT_CAPABILITY],
        "permissions": {"network": [], "filesystem": []},
        "files": []
    }))
    .expect_err("fingerprint detector must not claim unsupported STRM input");

    assert!(error.to_string().contains("only LOCAL_FILE"));
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
