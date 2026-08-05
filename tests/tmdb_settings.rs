use luxd::application::settings::{
    TMDB_ALTERNATE_API_BASE_URL, TMDB_CUSTOM_API_BASE_URL, TMDB_DEFAULT_API_BASE_URL, TmdbSettings,
    read_tmdb_settings, tmdb_api_base_url_options, tmdb_language_options, write_tmdb_settings,
};

#[test]
fn tmdb_settings_default_to_simplified_chinese_and_disabled_fallback() {
    let settings = TmdbSettings::default();

    assert_eq!(settings.preferred_language, "zh-CN");
    assert!(!settings.language_fallback_enabled);
    assert_eq!(settings.fallback_languages, vec!["zh-SG", "zh-HK", "zh-TW"]);
    assert!(!settings.alternate_api_enabled);
    assert_eq!(settings.api_base_url, TMDB_DEFAULT_API_BASE_URL);
}

#[test]
fn tmdb_api_base_url_options_keep_official_default_and_alternative() {
    let options = tmdb_api_base_url_options();
    let values = options
        .iter()
        .map(|option| option.value.as_str())
        .collect::<Vec<_>>();

    assert_eq!(values, ["official", "alternate", TMDB_CUSTOM_API_BASE_URL]);
    assert_eq!(options[0].label, TMDB_DEFAULT_API_BASE_URL);
    assert_eq!(options[1].label, TMDB_ALTERNATE_API_BASE_URL);
}

#[test]
fn tmdb_settings_validate_custom_api_base_urls() {
    let settings = TmdbSettings::new_with_api_config(
        "zh-CN".to_owned(),
        false,
        vec![],
        true,
        "https://mirror.example/tmdb".to_owned(),
    )
    .expect("custom HTTPS API base URL should be valid");
    assert_eq!(settings.api_base_url, "https://mirror.example/tmdb");

    for invalid_url in [
        "",
        "ftp://mirror.example",
        "https://user:password@mirror.example",
        "https://mirror.example/path?token=secret",
        "https://mirror.example/path#fragment",
    ] {
        assert!(
            TmdbSettings::new_with_api_config(
                "zh-CN".to_owned(),
                false,
                vec![],
                true,
                invalid_url.to_owned(),
            )
            .is_err(),
            "invalid API base URL should be rejected: {invalid_url}"
        );
    }
}

#[test]
fn tmdb_settings_reject_unknown_languages() {
    let error = TmdbSettings::new(
        "not-a-tmdb-language".to_owned(),
        false,
        vec!["zh-SG".to_owned()],
    )
    .expect_err("unknown preferred language must be rejected");

    assert!(error.to_string().contains("preferred language"));
}

#[test]
fn tmdb_language_options_prioritize_chinese_regions() {
    let options = tmdb_language_options();
    let values = options
        .iter()
        .map(|option| option.value.as_str())
        .collect::<Vec<_>>();

    assert_eq!(&values[..4], ["zh-CN", "zh-SG", "zh-HK", "zh-TW"]);
    assert_eq!(options[0].label, "简体中文");
    assert!(values.contains(&"en-US"));
    assert_eq!(values.iter().filter(|value| **value == "zh-CN").count(), 1);
}

#[tokio::test]
async fn tmdb_settings_round_trip_without_exposing_credentials() {
    let directory = tempfile::tempdir().expect("temporary directory should exist");
    let settings = TmdbSettings::new(
        "zh-SG".to_owned(),
        true,
        vec!["zh-HK".to_owned(), "zh-TW".to_owned()],
    )
    .expect("settings should be valid");

    write_tmdb_settings(directory.path(), &settings)
        .await
        .expect("settings should be persisted");
    let restored = read_tmdb_settings(directory.path()).await;

    assert_eq!(restored, settings);
    let serialized = std::fs::read_to_string(directory.path().join("tmdb_settings.json"))
        .expect("settings file should exist");
    assert!(!serialized.contains("apiKey"));
    assert!(!serialized.contains("token"));
}
