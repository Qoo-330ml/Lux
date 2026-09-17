use serde::Deserialize;

pub(crate) const DEFAULT_THUMBNAIL_SCRAPING_MODE: &str = "SCRAPER_FIRST";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ThumbnailScrapingMode {
    None,
    ScreenshotFirst,
    #[default]
    ScraperFirst,
}

impl ThumbnailScrapingMode {
    pub(crate) fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("NONE") => Self::None,
            Some("SCREENSHOT_FIRST") => Self::ScreenshotFirst,
            Some("SCRAPER_FIRST") => Self::ScraperFirst,
            _ => Self::ScraperFirst,
        }
    }

    pub(crate) fn from_strategy_json(library: Option<&str>, global: Option<&str>) -> Self {
        library
            .and_then(parse_strategy_json)
            .or_else(|| global.and_then(parse_strategy_json))
            .unwrap_or_default()
    }

    pub(crate) fn allows_screenshots(self) -> bool {
        !matches!(self, Self::None)
    }

    pub(crate) fn prefers_screenshots(self) -> bool {
        matches!(self, Self::ScreenshotFirst)
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredMediaStrategy {
    #[serde(default)]
    images: StoredImageStrategy,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredImageStrategy {
    thumbnail_scraping_mode: Option<String>,
}

fn parse_strategy_json(value: &str) -> Option<ThumbnailScrapingMode> {
    serde_json::from_str::<StoredMediaStrategy>(value)
        .ok()
        .map(|strategy| {
            ThumbnailScrapingMode::parse(strategy.images.thumbnail_scraping_mode.as_deref())
        })
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_THUMBNAIL_SCRAPING_MODE, ThumbnailScrapingMode};

    #[test]
    fn missing_or_unknown_modes_preserve_scraper_first_compatibility() {
        assert_eq!(
            ThumbnailScrapingMode::parse(None),
            ThumbnailScrapingMode::ScraperFirst
        );
        assert_eq!(
            ThumbnailScrapingMode::parse(Some("future-mode")),
            ThumbnailScrapingMode::ScraperFirst
        );
        assert_eq!(DEFAULT_THUMBNAIL_SCRAPING_MODE, "SCRAPER_FIRST");
    }

    #[test]
    fn library_strategy_overrides_global_strategy() {
        let global = r#"{"images":{"thumbnailScrapingMode":"SCREENSHOT_FIRST"}}"#;
        let library = r#"{"images":{"thumbnailScrapingMode":"NONE"}}"#;
        assert_eq!(
            ThumbnailScrapingMode::from_strategy_json(Some(library), Some(global)),
            ThumbnailScrapingMode::None
        );
    }

    #[test]
    fn missing_library_field_is_compatible_with_old_library_json() {
        let global = r#"{"images":{"thumbnailScrapingMode":"SCREENSHOT_FIRST"}}"#;
        let library = r#"{"images":{"poster":true}}"#;
        assert_eq!(
            ThumbnailScrapingMode::from_strategy_json(Some(library), Some(global)),
            ThumbnailScrapingMode::ScraperFirst
        );
    }
}
