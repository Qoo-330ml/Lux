use std::{collections::BTreeSet, fmt, io::Write, path::Path};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio::fs;

pub const TMDB_TOKEN_FILE: &str = "tmdb_read_access_token";
pub const TMDB_API_KEY_FILE: &str = "tmdb_api_key";
pub const TMDB_SETTINGS_FILE: &str = "tmdb_settings.json";
pub const NETWORK_PROXY_URL_FILE: &str = "network_proxy_url";

pub const TMDB_DEFAULT_PREFERRED_LANGUAGE: &str = "zh-CN";
pub const TMDB_DEFAULT_API_BASE_URL: &str = "https://api.themoviedb.org";
pub const TMDB_ALTERNATE_API_BASE_URL: &str = "https://api.tmdb.org";
pub const TMDB_API_BASE_URL_OPTION_OFFICIAL: &str = "official";
pub const TMDB_API_BASE_URL_OPTION_ALTERNATE: &str = "alternate";
pub const TMDB_CUSTOM_API_BASE_URL: &str = "custom";
const TMDB_API_BASE_URL_MAX_LENGTH: usize = 2048;

const TMDB_DEFAULT_FALLBACK_LANGUAGES: [&str; 3] = ["zh-SG", "zh-HK", "zh-TW"];

// TMDb primary translations from /3/configuration/primary_translations.
const TMDB_PRIMARY_TRANSLATIONS: &[&str] = &[
    "af-ZA", "ar-AE", "ar-BH", "ar-EG", "ar-IQ", "ar-JO", "ar-LY", "ar-MA", "ar-QA", "ar-SA",
    "ar-TD", "ar-YE", "be-BY", "bg-BG", "bn-BD", "bn-IN", "br-FR", "ca-AD", "ca-ES", "ch-GU",
    "cs-CZ", "cy-GB", "da-DK", "de-AT", "de-CH", "de-DE", "el-CY", "el-GR", "en-AG", "en-AU",
    "en-BB", "en-BZ", "en-CA", "en-CM", "en-GB", "en-GG", "en-GH", "en-GI", "en-GY", "en-IE",
    "en-JM", "en-KE", "en-LC", "en-MW", "en-NZ", "en-PG", "en-TC", "en-US", "en-ZM", "en-ZW",
    "eo-EO", "es-AR", "es-CL", "es-DO", "es-EC", "es-ES", "es-GQ", "es-GT", "es-HN", "es-MX",
    "es-NI", "es-PA", "es-PE", "es-PY", "es-SV", "es-UY", "et-EE", "eu-ES", "fa-IR", "fi-FI",
    "fr-BF", "fr-CA", "fr-CD", "fr-CI", "fr-FR", "fr-GF", "fr-GP", "fr-MC", "fr-ML", "fr-MU",
    "fr-PF", "ga-IE", "gd-GB", "gl-ES", "he-IL", "hi-IN", "hr-HR", "hu-HU", "hy-AM", "id-ID",
    "it-IT", "it-VA", "ja-JP", "ka-GE", "kk-KZ", "kn-IN", "ko-KR", "ku-TR", "ky-KG", "lt-LT",
    "lv-LV", "ml-IN", "mr-IN", "ms-MY", "ms-SG", "nb-NO", "ne-NP", "nl-BE", "nl-NL", "no-NO",
    "oc-FR", "pa-IN", "pl-PL", "pt-AO", "pt-BR", "pt-MZ", "pt-PT", "ro-MD", "ro-RO", "ru-RU",
    "si-LK", "sk-SK", "sl-SI", "so-SO", "sq-AL", "sq-XK", "sr-ME", "sr-RS", "sv-SE", "sw-TZ",
    "ta-IN", "te-IN", "th-TH", "tl-PH", "tr-TR", "uk-UA", "ur-PK", "uz-UZ", "vi-VN", "zh-CN",
    "zh-HK", "zh-SG", "zh-TW", "zu-ZA",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TmdbLanguageOption {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TmdbApiBaseUrlOption {
    pub value: String,
    pub label: String,
}

pub fn tmdb_language_options() -> Vec<TmdbLanguageOption> {
    let mut values = Vec::with_capacity(TMDB_PRIMARY_TRANSLATIONS.len());
    values.extend(["zh-CN", "zh-SG", "zh-HK", "zh-TW"]);
    let additional = TMDB_PRIMARY_TRANSLATIONS
        .iter()
        .copied()
        .filter(|value| !values.contains(value))
        .collect::<Vec<_>>();
    values.extend(additional);
    values
        .into_iter()
        .map(|value| TmdbLanguageOption {
            label: if value == "zh-CN" {
                "简体中文".to_owned()
            } else {
                value.to_owned()
            },
            value: value.to_owned(),
        })
        .collect()
}

pub fn tmdb_api_base_url_options() -> Vec<TmdbApiBaseUrlOption> {
    vec![
        TmdbApiBaseUrlOption {
            value: TMDB_API_BASE_URL_OPTION_OFFICIAL.to_owned(),
            label: TMDB_DEFAULT_API_BASE_URL.to_owned(),
        },
        TmdbApiBaseUrlOption {
            value: TMDB_API_BASE_URL_OPTION_ALTERNATE.to_owned(),
            label: TMDB_ALTERNATE_API_BASE_URL.to_owned(),
        },
        TmdbApiBaseUrlOption {
            value: TMDB_CUSTOM_API_BASE_URL.to_owned(),
            label: "自定义".to_owned(),
        },
    ]
}

fn is_valid_tmdb_language(language: &str) -> bool {
    TMDB_PRIMARY_TRANSLATIONS.contains(&language)
}

fn default_preferred_language() -> String {
    TMDB_DEFAULT_PREFERRED_LANGUAGE.to_owned()
}

fn default_fallback_languages() -> Vec<String> {
    TMDB_DEFAULT_FALLBACK_LANGUAGES
        .iter()
        .map(|language| (*language).to_owned())
        .collect()
}

fn default_api_base_url() -> String {
    TMDB_DEFAULT_API_BASE_URL.to_owned()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TmdbSettings {
    #[serde(default = "default_preferred_language")]
    pub preferred_language: String,
    #[serde(default)]
    pub language_fallback_enabled: bool,
    #[serde(default)]
    pub title_alias_replacement_enabled: bool,
    #[serde(default = "default_fallback_languages")]
    pub fallback_languages: Vec<String>,
    #[serde(default)]
    pub alternate_api_enabled: bool,
    #[serde(default = "default_api_base_url")]
    pub api_base_url: String,
}

impl Default for TmdbSettings {
    fn default() -> Self {
        Self {
            preferred_language: default_preferred_language(),
            language_fallback_enabled: false,
            title_alias_replacement_enabled: false,
            fallback_languages: default_fallback_languages(),
            alternate_api_enabled: false,
            api_base_url: default_api_base_url(),
        }
    }
}

impl TmdbSettings {
    pub fn new(
        preferred_language: String,
        language_fallback_enabled: bool,
        fallback_languages: Vec<String>,
    ) -> Result<Self, TmdbSettingsError> {
        Self::new_with_api_config(
            preferred_language,
            language_fallback_enabled,
            fallback_languages,
            false,
            default_api_base_url(),
        )
    }

    pub fn new_with_api_config(
        preferred_language: String,
        language_fallback_enabled: bool,
        fallback_languages: Vec<String>,
        alternate_api_enabled: bool,
        api_base_url: String,
    ) -> Result<Self, TmdbSettingsError> {
        Self::new_with_api_and_title_alias_config(
            preferred_language,
            language_fallback_enabled,
            false,
            fallback_languages,
            alternate_api_enabled,
            api_base_url,
        )
    }

    pub fn new_with_api_and_title_alias_config(
        preferred_language: String,
        language_fallback_enabled: bool,
        title_alias_replacement_enabled: bool,
        fallback_languages: Vec<String>,
        alternate_api_enabled: bool,
        api_base_url: String,
    ) -> Result<Self, TmdbSettingsError> {
        let settings = Self {
            preferred_language: preferred_language.trim().to_owned(),
            language_fallback_enabled,
            title_alias_replacement_enabled,
            fallback_languages,
            alternate_api_enabled,
            api_base_url: api_base_url.trim().to_owned(),
        };
        settings.validate()?;
        Ok(settings.normalized())
    }

    pub fn validate(&self) -> Result<(), TmdbSettingsError> {
        if !is_valid_tmdb_language(self.preferred_language.trim()) {
            return Err(TmdbSettingsError::Invalid(
                "preferred language is not supported by TMDb".to_owned(),
            ));
        }
        if self.fallback_languages.len() > TMDB_PRIMARY_TRANSLATIONS.len() {
            return Err(TmdbSettingsError::Invalid(
                "too many TMDb fallback languages".to_owned(),
            ));
        }
        for language in &self.fallback_languages {
            if !is_valid_tmdb_language(language.trim()) {
                return Err(TmdbSettingsError::Invalid(format!(
                    "fallback language is not supported by TMDb: {language}"
                )));
            }
        }
        validate_tmdb_api_base_url(self.api_base_url.trim())?;
        Ok(())
    }

    pub fn normalized(mut self) -> Self {
        self.preferred_language = self.preferred_language.trim().to_owned();
        self.api_base_url = self.api_base_url.trim().to_owned();
        let mut seen = BTreeSet::new();
        self.fallback_languages = self
            .fallback_languages
            .into_iter()
            .map(|language| language.trim().to_owned())
            .filter(|language| seen.insert(language.clone()))
            .collect();
        self
    }
}

fn validate_tmdb_api_base_url(value: &str) -> Result<(), TmdbSettingsError> {
    if value.is_empty() || value.chars().count() > TMDB_API_BASE_URL_MAX_LENGTH {
        return Err(TmdbSettingsError::Invalid(
            "TMDb API base URL must be between 1 and 2048 characters".to_owned(),
        ));
    }
    let parsed = Url::parse(value).map_err(|error| {
        TmdbSettingsError::Invalid(format!("invalid TMDb API base URL: {error}"))
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(TmdbSettingsError::Invalid(
            "TMDb API base URL must use http or https and include a host".to_owned(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(TmdbSettingsError::Invalid(
            "TMDb API base URL must not include credentials".to_owned(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(TmdbSettingsError::Invalid(
            "TMDb API base URL must not include a query or fragment".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub enum TmdbSettingsError {
    Invalid(String),
    Io(std::io::Error),
    Serialization(String),
}

impl fmt::Display for TmdbSettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::Serialization(message) => formatter.write_str(message),
            Self::Io(error) => write!(formatter, "TMDb settings IO error: {error}"),
        }
    }
}

impl std::error::Error for TmdbSettingsError {}

pub async fn read_tmdb_settings(config_dir: &Path) -> TmdbSettings {
    let path = config_dir.join(TMDB_SETTINGS_FILE);
    let Ok(contents) = fs::read_to_string(path).await else {
        return TmdbSettings::default();
    };
    let Ok(settings) = serde_json::from_str::<TmdbSettings>(&contents) else {
        return TmdbSettings::default();
    };
    if settings.validate().is_err() {
        return TmdbSettings::default();
    }
    settings.normalized()
}

pub async fn write_tmdb_settings(
    config_dir: &Path,
    settings: &TmdbSettings,
) -> Result<(), TmdbSettingsError> {
    settings.validate()?;
    let serialized = serde_json::to_vec_pretty(&settings.clone().normalized())
        .map_err(|error| TmdbSettingsError::Serialization(error.to_string()))?;
    fs::create_dir_all(config_dir)
        .await
        .map_err(TmdbSettingsError::Io)?;
    let path = config_dir.join(TMDB_SETTINGS_FILE);
    let temporary_path = config_dir.join(format!(".{}.tmp", TMDB_SETTINGS_FILE));
    tokio::task::spawn_blocking(move || {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary_path)?;
        #[cfg(unix)]
        std::fs::set_permissions(
            &temporary_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )?;
        file.write_all(&serialized)?;
        file.sync_all()?;
        std::fs::rename(temporary_path, path)
    })
    .await
    .map_err(|error| TmdbSettingsError::Io(std::io::Error::other(error.to_string())))?
    .map_err(TmdbSettingsError::Io)
}

pub fn read_tmdb_api_key(config_dir: &Path) -> Option<String> {
    read_secret(config_dir.join(TMDB_API_KEY_FILE))
}

pub fn read_tmdb_token(config_dir: &Path) -> Option<String> {
    read_secret(config_dir.join(TMDB_TOKEN_FILE))
}

pub fn read_network_proxy_url(config_dir: &Path) -> Option<String> {
    read_secret(config_dir.join(NETWORK_PROXY_URL_FILE))
}

fn read_secret(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub async fn write_tmdb_api_key(config_dir: &Path, api_key: Option<&str>) -> std::io::Result<()> {
    write_secret_file(config_dir, TMDB_API_KEY_FILE, api_key).await
}

pub async fn write_tmdb_token(config_dir: &Path, token: &str) -> std::io::Result<()> {
    write_secret_file(config_dir, TMDB_TOKEN_FILE, Some(token)).await
}

pub async fn read_network_proxy_url_async(config_dir: &Path) -> Option<String> {
    fs::read_to_string(config_dir.join(NETWORK_PROXY_URL_FILE))
        .await
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub async fn write_network_proxy_url(
    config_dir: &Path,
    proxy_url: Option<&str>,
) -> std::io::Result<()> {
    write_secret_file(config_dir, NETWORK_PROXY_URL_FILE, proxy_url).await
}

async fn write_secret_file(
    config_dir: &Path,
    file_name: &str,
    value: Option<&str>,
) -> std::io::Result<()> {
    fs::create_dir_all(config_dir).await?;
    let path = config_dir.join(file_name);
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        if let Err(error) = fs::remove_file(path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error);
            }
        }
        return Ok(());
    };
    let value = format!("{value}\n");
    tokio::task::spawn_blocking(move || {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&path)?;
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        file.write_all(value.as_bytes())?;
        file.sync_all()
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?
}
