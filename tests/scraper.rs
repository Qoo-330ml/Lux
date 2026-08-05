use luxd::application::identification::{
    IdentificationStatus, ScraperMovieIdentity, identify_scraper_movie,
};
use luxd::application::scraper::{
    ScraperGetRequest, ScraperImageRequest, ScraperItemType, ScraperSearchRequest,
    decode_credits_response, decode_images_response, decode_metadata_response,
    decode_search_response,
};
use serde_json::json;

#[test]
fn generic_scraper_requests_use_provider_neutral_fields() {
    let search = ScraperSearchRequest::new(ScraperItemType::Movie, "二毛", Some(2019), "zh-CN");
    assert_eq!(
        serde_json::to_value(search).expect("search request should serialize"),
        json!({
            "itemType": "Movie",
            "name": "二毛",
            "year": 2019,
            "language": "zh-CN"
        })
    );

    let get = ScraperGetRequest::new(ScraperItemType::Movie, "douban-123", "zh-CN");
    assert_eq!(
        serde_json::to_value(get).expect("get request should serialize"),
        json!({
            "itemType": "Movie",
            "providerId": "douban-123",
            "language": "zh-CN"
        })
    );

    let season = ScraperGetRequest::for_season("tmdb-series-8", 1, "zh-CN");
    assert_eq!(
        serde_json::to_value(season).expect("season request should serialize"),
        json!({
            "itemType": "Season",
            "providerId": "tmdb-series-8",
            "language": "zh-CN",
            "seasonNumber": 1
        })
    );

    let episode = ScraperGetRequest::for_episode("tmdb-series-8", 1, 2, "zh-CN");
    assert_eq!(
        serde_json::to_value(episode).expect("episode request should serialize"),
        json!({
            "itemType": "Episode",
            "providerId": "tmdb-series-8",
            "language": "zh-CN",
            "seasonNumber": 1,
            "episodeNumber": 2
        })
    );

    let images = ScraperImageRequest::new(ScraperItemType::Movie, "douban-123", "zh-CN");
    assert_eq!(
        serde_json::to_value(images).expect("image request should serialize"),
        json!({
            "itemType": "Movie",
            "providerId": "douban-123",
            "language": "zh-CN"
        })
    );
}

#[test]
fn generic_scraper_decodes_provider_neutral_responses() {
    let search = decode_search_response(json!({
        "items": [{
            "Type": "Movie",
            "Name": "二毛",
            "OriginalTitle": "Er Mao",
            "ProductionYear": 2019,
            "Rating": 8.6,
            "ProviderIds": {"Douban": "douban-123"},
            "SearchProviderName": "Douban"
        }]
    }))
    .expect("search response should decode");
    assert_eq!(search.items[0].provider_id("Douban"), Some("douban-123"));
    assert_eq!(search.items[0].title.as_deref(), Some("二毛"));
    assert_eq!(search.items[0].rating, Some(8.6));

    let metadata = decode_metadata_response(json!({
        "metadata": {
            "Type": "Movie",
            "Name": "二毛",
            "Rating": 8.6,
            "ProviderIds": {"Douban": "douban-123"}
        }
    }))
    .expect("metadata response should decode");
    assert_eq!(metadata.provider_id("Douban"), Some("douban-123"));
    assert_eq!(metadata.rating, Some(8.6));

    let images = decode_images_response(json!({
        "images": [{
            "Type": "Primary",
            "Url": "https://img.example/poster.jpg",
            "Language": "zh"
        }]
    }))
    .expect("image response should decode");
    assert_eq!(images.images[0].url, "https://img.example/poster.jpg");

    let credits = decode_credits_response(json!({
        "cast": [{"Id": "douban-person-1", "Name": "演员"}]
    }))
    .expect("credits response should decode");
    assert_eq!(credits.cast[0].provider_id, "douban-person-1");
}

#[test]
fn generic_scraper_identification_keeps_opaque_provider_ids() {
    let identity = ScraperMovieIdentity {
        title: "二毛".to_owned(),
        year: Some(2019),
        provider_id: Some("douban-123".to_owned()),
    };
    let candidates = vec![luxd::application::scraper::ScraperSearchResult {
        title: Some("二毛".to_owned()),
        production_year: Some(2019),
        provider_ids: [("Douban".to_owned(), "douban-123".to_owned())]
            .into_iter()
            .collect(),
        ..Default::default()
    }];
    let decision = identify_scraper_movie(&identity, &candidates);
    assert_eq!(decision.status, IdentificationStatus::Confirmed);
    assert_eq!(
        decision
            .candidate
            .and_then(|value| value.first_provider_id().map(str::to_owned)),
        Some("douban-123".to_owned())
    );
}
