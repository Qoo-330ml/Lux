//! Provider configuration and match-title candidate construction.
//!
//! The host never sends paths or upstream URLs to a plugin; it only builds the
//! title candidates the plugin is asked to match, and maps the plugin error
//! taxonomy to the stable `danmaku_match_job_items.error_code` values.

use std::fmt;

use reqwest::Url;

use crate::{
    application::{
        media_matching::{MediaKind, parse_media_name},
        plugins::{DanmakuSettings, PluginServiceError},
    },
    storage::StoredDanmakuSource,
};

const MAX_PROVIDER_BASE_URL_CHARS: usize = 4096;

#[derive(Clone, Eq, PartialEq)]
pub struct ProviderBaseUrl {
    normalized: String,
    redacted: String,
}

impl ProviderBaseUrl {
    pub fn normalized(&self) -> &str {
        &self.normalized
    }

    pub fn redacted(&self) -> &str {
        &self.redacted
    }
}

impl fmt::Debug for ProviderBaseUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderBaseUrl")
            .field("redacted", &self.redacted)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderUrlError {
    Invalid,
    TooLong,
}

impl fmt::Display for ProviderUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => formatter.write_str("danmaku provider URL is invalid"),
            Self::TooLong => formatter.write_str("danmaku provider URL is too long"),
        }
    }
}

impl std::error::Error for ProviderUrlError {}

pub fn validate_provider_base_url(value: &str) -> Result<ProviderBaseUrl, ProviderUrlError> {
    let value = value.trim();
    if value.chars().count() > MAX_PROVIDER_BASE_URL_CHARS {
        return Err(ProviderUrlError::TooLong);
    }
    let mut url = Url::parse(value).map_err(|_| ProviderUrlError::Invalid)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ProviderUrlError::Invalid);
    }
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(if path.is_empty() { "/" } else { &path });
    let normalized = url.to_string().trim_end_matches('/').to_owned();
    let authority = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or("configured")),
        None => url.host_str().unwrap_or("configured").to_owned(),
    };
    let redacted = format!("{}://{}/[redacted]", url.scheme(), authority);
    Ok(ProviderBaseUrl {
        normalized,
        redacted,
    })
}

pub(super) fn match_file_name_candidates(
    file_name: &str,
    source: &StoredDanmakuSource,
    settings: &DanmakuSettings,
) -> Vec<String> {
    let mut candidates = Vec::new();
    if settings.match_original_filename {
        candidates.push(file_name.to_owned());
    }
    let title = if source.item_type.as_deref() == Some("EPISODE") {
        source.series_title.as_deref().or(source.title.as_deref())
    } else {
        source.title.as_deref()
    };
    if settings.match_simplified_traditional_titles {
        if let Some(title) = title {
            candidates.push(title_with_episode_suffix(title, file_name, source));
        }
    }
    if settings.match_english_title {
        let title = if source.item_type.as_deref() == Some("EPISODE") {
            source
                .series_original_title
                .as_deref()
                .or(source.original_title.as_deref())
        } else {
            source.original_title.as_deref()
        };
        if let Some(title) = title {
            candidates.push(title_with_episode_suffix(title, file_name, source));
        }
    }
    candidates.retain(|candidate| !candidate.trim().is_empty());
    candidates.dedup();
    candidates
}

fn title_with_episode_suffix(title: &str, file_name: &str, source: &StoredDanmakuSource) -> String {
    let season = source
        .season_number
        .and_then(|value| u32::try_from(value).ok());
    let episode = source
        .episode_number
        .and_then(|value| u32::try_from(value).ok());
    let parsed = (season.is_none() || episode.is_none())
        .then(|| parse_media_name(file_name, MediaKind::Episode))
        .flatten();
    let season = season.or_else(|| parsed.as_ref().and_then(|value| value.season));
    let episode = episode.or_else(|| parsed.as_ref().and_then(|value| value.episode));
    let mut result = title.trim().to_owned();
    if let Some(season) = season {
        result.push_str(&format!(" S{season:02}"));
    }
    if let Some(episode) = episode {
        result.push_str(&format!("E{episode:02}"));
    }
    result
}

pub(super) fn danmaku_plugin_error_code(error: &PluginServiceError) -> &'static str {
    match error {
        PluginServiceError::UnknownPlugin(_) | PluginServiceError::Unavailable(_) => {
            "PLUGIN_UNAVAILABLE"
        }
        PluginServiceError::InvalidConfig => "PLUGIN_NOT_CONFIGURED",
        PluginServiceError::InvalidResponse => "PLUGIN_INVALID_RESPONSE",
        PluginServiceError::Runtime(_) => "PLUGIN_RUNTIME_ERROR",
        PluginServiceError::ConfigIo(_) | PluginServiceError::ImageAsset(_) => {
            "PLUGIN_CONFIG_ERROR"
        }
        PluginServiceError::NoUpdate
        | PluginServiceError::Store(_)
        | PluginServiceError::Storage(_) => "PLUGIN_ERROR",
    }
}

#[cfg(test)]
mod tests {
    use super::match_file_name_candidates;
    use crate::application::plugins::DanmakuSettings;
    use crate::application::schedule::DEFAULT_DANMAKU_MATCH_SCHEDULE;
    use crate::storage::StoredDanmakuSource;

    #[test]
    fn title_matching_candidates_follow_configured_order() {
        let source = StoredDanmakuSource {
            source_id: "source-1".to_owned(),
            root_path: "/media".to_owned(),
            relative_path: "Show/Episode 01.mkv".to_owned(),
            item_type: Some("EPISODE".to_owned()),
            season_number: None,
            episode_number: None,
            title: Some("第 1 集".to_owned()),
            original_title: Some("Episode 1".to_owned()),
            series_title: Some("简体剧名".to_owned()),
            series_original_title: Some("English Show".to_owned()),
        };
        let settings = DanmakuSettings {
            library_ids: vec!["library-1".to_owned()],
            match_original_filename: true,
            match_simplified_traditional_titles: true,
            match_english_title: true,
            concurrency: 2,
            overwrite: false,
            schedule: DEFAULT_DANMAKU_MATCH_SCHEDULE.to_owned(),
        };

        assert_eq!(
            match_file_name_candidates("Original.S01E01.mkv", &source, &settings),
            vec![
                "Original.S01E01.mkv",
                "简体剧名 S01E01",
                "English Show S01E01"
            ]
        );
    }

    #[test]
    fn structured_episode_numbers_take_precedence_over_filename_numbers() {
        let source = StoredDanmakuSource {
            source_id: "source-1".to_owned(),
            root_path: "/media".to_owned(),
            relative_path: "Show/Episode 01.mkv".to_owned(),
            item_type: Some("EPISODE".to_owned()),
            season_number: Some(2),
            episode_number: Some(3),
            title: Some("第 3 集".to_owned()),
            original_title: Some("Episode 3".to_owned()),
            series_title: Some("简体剧名".to_owned()),
            series_original_title: Some("English Show".to_owned()),
        };
        let settings = DanmakuSettings {
            library_ids: vec!["library-1".to_owned()],
            match_original_filename: false,
            match_simplified_traditional_titles: true,
            match_english_title: true,
            concurrency: 2,
            overwrite: false,
            schedule: DEFAULT_DANMAKU_MATCH_SCHEDULE.to_owned(),
        };

        assert_eq!(
            match_file_name_candidates("Original.S01E01.mkv", &source, &settings),
            vec!["简体剧名 S02E03", "English Show S02E03"]
        );
    }
}
