use std::{
    collections::HashMap,
    env,
    io::{BufRead, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

use luxd::application::{
    plugin_protocol::{PluginRequest, PluginResponse, PluginRpcError},
    settings::{read_tmdb_api_key, read_tmdb_token},
    tmdb::{
        TmdbClient, TmdbCollectionDetails, TmdbCollectionSearchResponse, TmdbEpisodeDetails,
        TmdbExternalIds, TmdbImagesResponse, TmdbMovieDetails, TmdbMovieSearchResponse,
        TmdbPersonDetails, TmdbPersonSearchResponse, TmdbSeasonDetails, TmdbSeriesDetails,
        TmdbTvSearchResponse, TmdbVideosResponse,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::OnceCell;

static CLIENT: OnceCell<Result<TmdbClient, String>> = OnceCell::const_new();
static RESPONSE_CACHE: OnceCell<tokio::sync::Mutex<HashMap<String, CachedResponse>>> =
    OnceCell::const_new();
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const CACHE_CAPACITY: usize = 256;

#[derive(Clone)]
struct CachedResponse {
    created_at: Instant,
    value: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataRequest {
    #[serde(default)]
    item_type: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    year: Option<i32>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    tmdb_id: Option<i64>,
    #[serde(default)]
    collection_id: Option<i64>,
    #[serde(default)]
    season_number: Option<i32>,
    #[serde(default)]
    episode_number: Option<i32>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let lines = stdin.lock().lines();
    let mut output = stdout.lock();

    for line in lines {
        let line = line?;
        let response = match serde_json::from_str::<PluginRequest>(&line) {
            Ok(request) => handle_request(request).await,
            Err(error) => PluginResponse {
                id: "invalid-request".to_owned(),
                result: None,
                error: Some(PluginRpcError {
                    code: "PLUGIN_INVALID_REQUEST".to_owned(),
                    message: error.to_string(),
                }),
            },
        };
        let mut serialized = serde_json::to_vec(&response)?;
        serialized.push(b'\n');
        output.write_all(&serialized)?;
        output.flush()?;
    }
    Ok(())
}

async fn handle_request(request: PluginRequest) -> PluginResponse {
    let id = request.id.clone();
    match handle_method(&request.method, request.params).await {
        Ok(result) => PluginResponse {
            id,
            result: Some(result),
            error: None,
        },
        Err(error) => PluginResponse {
            id,
            result: None,
            error: Some(error),
        },
    }
}

async fn handle_method(method: &str, params: Value) -> Result<Value, PluginRpcError> {
    match method {
        "plugin.hello" => Ok(json!({
            "id": "org.lux.tmdb",
            "name": "TMDb 元数据插件",
            "apiVersion": 1,
            "capabilities": [
                "metadata.search",
                "metadata.details",
                "metadata.images",
                "metadata.externalIds",
                "metadata.trailers"
            ],
            "supportedItemTypes": ["Movie", "BoxSet"]
        })),
        "plugin.health" => {
            let _ = client().await?;
            Ok(json!({"available": true, "configured": true}))
        }
        "metadata.search"
        | "metadata.get"
        | "metadata.externalIds"
        | "metadata.images"
        | "metadata.trailers" => cached_metadata_call(method, params).await,
        "plugin.shutdown" => Ok(json!({"accepted": true})),
        _ => Err(PluginRpcError {
            code: "PLUGIN_INVALID_REQUEST".to_owned(),
            message: format!("unsupported plugin method: {method}"),
        }),
    }
}

async fn cached_metadata_call(method: &str, params: Value) -> Result<Value, PluginRpcError> {
    let key = serde_json::to_string(&(method, &params)).map_err(|error| PluginRpcError {
        code: "PLUGIN_INVALID_REQUEST".to_owned(),
        message: error.to_string(),
    })?;
    let cache = RESPONSE_CACHE
        .get_or_init(|| async { tokio::sync::Mutex::new(HashMap::new()) })
        .await;
    {
        let mut entries = cache.lock().await;
        if let Some(entry) = entries.get(&key) {
            if entry.created_at.elapsed() < CACHE_TTL {
                return Ok(entry.value.clone());
            }
            entries.remove(&key);
        }
    }
    let value = match method {
        "metadata.search" => search(params).await?,
        "metadata.get" => metadata(params).await?,
        "metadata.externalIds" => external_ids(params).await?,
        "metadata.images" => images(params).await?,
        "metadata.trailers" => trailers(params).await?,
        _ => {
            return Err(PluginRpcError {
                code: "PLUGIN_INVALID_REQUEST".to_owned(),
                message: format!("unsupported metadata method: {method}"),
            });
        }
    };
    let mut entries = cache.lock().await;
    if entries.len() >= CACHE_CAPACITY {
        if let Some(oldest_key) = entries
            .iter()
            .min_by_key(|(_, entry)| entry.created_at)
            .map(|(key, _)| key.clone())
        {
            entries.remove(&oldest_key);
        }
    }
    entries.insert(
        key,
        CachedResponse {
            created_at: Instant::now(),
            value: value.clone(),
        },
    );
    Ok(value)
}

async fn search(params: Value) -> Result<Value, PluginRpcError> {
    let request = parse_request(params)?;
    match request.item_type.as_deref().unwrap_or("Movie") {
        "Movie" => {
            let query = request.name.as_deref().unwrap_or_default();
            let response = client()
                .await?
                .search_movies_with_english_fallback(query, request.year)
                .await
                .map_err(tmdb_error)?;
            Ok(json!({"items": movie_search_results(response)}))
        }
        "Series" | "TvSeries" => {
            let query = request.name.as_deref().unwrap_or_default();
            let response = client()
                .await?
                .search_tv(query, request.year, language(&request))
                .await
                .map_err(tmdb_error)?;
            Ok(json!({"items": tv_search_results(response)}))
        }
        "Person" => {
            let query = request.name.as_deref().unwrap_or_default();
            let response = client()
                .await?
                .search_people(query, language(&request))
                .await
                .map_err(tmdb_error)?;
            Ok(json!({"items": person_search_results(response)}))
        }
        "BoxSet" | "Collection" => {
            let query = request.name.as_deref().unwrap_or_default();
            let response = client()
                .await?
                .search_collections(query, language(&request))
                .await
                .map_err(tmdb_error)?;
            Ok(json!({"items": collection_search_results(response)}))
        }
        item_type => Err(PluginRpcError {
            code: "PLUGIN_PROVIDER_NOT_FOUND".to_owned(),
            message: format!("unsupported TMDb item type: {item_type}"),
        }),
    }
}

async fn metadata(params: Value) -> Result<Value, PluginRpcError> {
    let request = parse_request(params)?;
    let language = request.language.as_deref().unwrap_or("zh-CN");
    match request.item_type.as_deref().unwrap_or("Movie") {
        "Movie" => {
            let movie_id = request
                .tmdb_id
                .ok_or_else(|| invalid("tmdbId is required"))?;
            let details = client()
                .await?
                .movie_details(movie_id, language)
                .await
                .map_err(tmdb_error)?;
            Ok(json!({"metadata": movie_details(details)}))
        }
        "Series" | "TvSeries" => {
            let series_id = request
                .tmdb_id
                .ok_or_else(|| invalid("tmdbId is required"))?;
            let details = client()
                .await?
                .series_details(series_id, language)
                .await
                .map_err(tmdb_error)?;
            Ok(json!({"metadata": series_details(details)}))
        }
        "Season" => {
            let series_id = request
                .tmdb_id
                .ok_or_else(|| invalid("tmdbId is required"))?;
            let season_number = request
                .season_number
                .ok_or_else(|| invalid("seasonNumber is required"))?;
            let details = client()
                .await?
                .season_details(series_id, season_number, language)
                .await
                .map_err(tmdb_error)?;
            Ok(json!({"metadata": season_details(details)}))
        }
        "Episode" => {
            let series_id = request
                .tmdb_id
                .ok_or_else(|| invalid("tmdbId is required"))?;
            let season_number = request
                .season_number
                .ok_or_else(|| invalid("seasonNumber is required"))?;
            let episode_number = request
                .episode_number
                .ok_or_else(|| invalid("episodeNumber is required"))?;
            let details = client()
                .await?
                .episode_details(series_id, season_number, episode_number, language)
                .await
                .map_err(tmdb_error)?;
            Ok(json!({"metadata": episode_details(details)}))
        }
        "Person" => {
            let person_id = request
                .tmdb_id
                .ok_or_else(|| invalid("tmdbId is required"))?;
            let details = client()
                .await?
                .person_details(person_id, language)
                .await
                .map_err(tmdb_error)?;
            Ok(json!({"metadata": person_details(details)}))
        }
        "BoxSet" | "Collection" => {
            let collection_id = request
                .collection_id
                .or(request.tmdb_id)
                .ok_or_else(|| invalid("collectionId is required"))?;
            let details = client()
                .await?
                .collection_details(collection_id, language)
                .await
                .map_err(tmdb_error)?;
            Ok(json!({"metadata": collection_details(details)}))
        }
        item_type => Err(PluginRpcError {
            code: "PLUGIN_PROVIDER_NOT_FOUND".to_owned(),
            message: format!("unsupported TMDb item type: {item_type}"),
        }),
    }
}

async fn external_ids(params: Value) -> Result<Value, PluginRpcError> {
    let request = parse_request(params)?;
    let id = request.tmdb_id.or(request.collection_id);
    let id = id.ok_or_else(|| invalid("tmdbId or collectionId is required"))?;
    let mut provider_ids = serde_json::Map::new();
    provider_ids.insert("Tmdb".to_owned(), Value::String(id.to_string()));
    let external = match request.item_type.as_deref().unwrap_or("Movie") {
        "Movie" => client()
            .await?
            .movie_external_ids(id)
            .await
            .map_err(tmdb_error)?,
        "Series" | "TvSeries" => client()
            .await?
            .tv_external_ids(id)
            .await
            .map_err(tmdb_error)?,
        "Person" => client()
            .await?
            .person_external_ids(id)
            .await
            .map_err(tmdb_error)?,
        "Season" | "Episode" => client()
            .await?
            .tv_external_ids(id)
            .await
            .map_err(tmdb_error)?,
        "BoxSet" | "Collection" => TmdbExternalIds::default(),
        item_type => {
            return Err(PluginRpcError {
                code: "PLUGIN_PROVIDER_NOT_FOUND".to_owned(),
                message: format!("unsupported TMDb item type: {item_type}"),
            });
        }
    };
    add_external_ids(&mut provider_ids, external);
    Ok(json!({"providerIds": provider_ids}))
}

async fn images(params: Value) -> Result<Value, PluginRpcError> {
    let request = parse_request(params)?;
    let id = request
        .tmdb_id
        .ok_or_else(|| invalid("tmdbId is required"))?;
    let language = language(&request);
    let images = match request.item_type.as_deref().unwrap_or("Movie") {
        "Movie" => client()
            .await?
            .movie_images(id, language)
            .await
            .map_err(tmdb_error)?,
        "Series" | "TvSeries" => client()
            .await?
            .tv_images(id, language)
            .await
            .map_err(tmdb_error)?,
        "Person" => client()
            .await?
            .person_images(id, language)
            .await
            .map_err(tmdb_error)?,
        item_type => {
            return Err(PluginRpcError {
                code: "PLUGIN_PROVIDER_NOT_FOUND".to_owned(),
                message: format!("image provider is not available for {item_type}"),
            });
        }
    };
    Ok(json!({"images": image_results(images)}))
}

async fn trailers(params: Value) -> Result<Value, PluginRpcError> {
    let request = parse_request(params)?;
    let id = request
        .tmdb_id
        .ok_or_else(|| invalid("tmdbId is required"))?;
    let language = language(&request);
    let videos = match request.item_type.as_deref().unwrap_or("Movie") {
        "Movie" => client()
            .await?
            .movie_videos(id, language)
            .await
            .map_err(tmdb_error)?,
        "Series" | "TvSeries" => client()
            .await?
            .tv_videos(id, language)
            .await
            .map_err(tmdb_error)?,
        item_type => {
            return Err(PluginRpcError {
                code: "PLUGIN_PROVIDER_NOT_FOUND".to_owned(),
                message: format!("trailer provider is not available for {item_type}"),
            });
        }
    };
    Ok(json!({"trailers": trailer_results(videos)}))
}

fn parse_request(params: Value) -> Result<MetadataRequest, PluginRpcError> {
    serde_json::from_value(params).map_err(|error| invalid(&error.to_string()))
}

async fn client() -> Result<&'static TmdbClient, PluginRpcError> {
    let value = CLIENT
        .get_or_init(|| async {
            let config_dir = env::var_os("LUX_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("./config"));
            TmdbClient::from_env_or_config(
                read_tmdb_api_key(&config_dir),
                read_tmdb_token(&config_dir),
            )
            .map_err(|error| error.to_string())
        })
        .await;
    value.as_ref().map_err(|error| PluginRpcError {
        code: "PLUGIN_AUTH_FAILED".to_owned(),
        message: error.clone(),
    })
}

fn movie_search_results(response: TmdbMovieSearchResponse) -> Vec<Value> {
    response
        .results
        .into_iter()
        .map(|result| {
            json!({
                "Type": "Movie",
                "Name": result.title,
                "OriginalTitle": result.original_title,
                "Overview": result.overview,
                "ProductionYear": result.release_date.as_deref().and_then(parse_year),
                "ProviderIds": {"Tmdb": result.id.to_string()},
                "SearchProviderName": "Tmdb"
            })
        })
        .collect()
}

fn movie_details(details: TmdbMovieDetails) -> Value {
    json!({
        "Type": "Movie",
        "Name": details.title,
        "OriginalTitle": details.original_title,
        "Overview": details.overview,
        "ProductionYear": details.release_date.as_deref().and_then(parse_year),
        "ProviderIds": {"Tmdb": details.id.to_string()},
        "OriginalLanguage": details.original_language
    })
}

fn tv_search_results(response: TmdbTvSearchResponse) -> Vec<Value> {
    response
        .results
        .into_iter()
        .map(|result| {
            json!({
                "Type": "Series",
                "Name": result.name,
                "OriginalTitle": result.original_name,
                "Overview": result.overview,
                "ProductionYear": result.first_air_date.as_deref().and_then(parse_year),
                "ProviderIds": {"Tmdb": result.id.to_string()},
                "SearchProviderName": "Tmdb"
            })
        })
        .collect()
}

fn person_search_results(response: TmdbPersonSearchResponse) -> Vec<Value> {
    response
        .results
        .into_iter()
        .map(|result| {
            json!({
                "Type": "Person",
                "Name": result.name,
                "ProviderIds": {"Tmdb": result.id.to_string()},
                "SearchProviderName": "Tmdb"
            })
        })
        .collect()
}

fn collection_search_results(response: TmdbCollectionSearchResponse) -> Vec<Value> {
    response
        .results
        .into_iter()
        .map(|result| {
            json!({
                "Type": "BoxSet",
                "Name": result.name,
                "Overview": result.overview,
                "ProviderIds": {"Tmdb": result.id.to_string()},
                "SearchProviderName": "Tmdb"
            })
        })
        .collect()
}

fn series_details(details: TmdbSeriesDetails) -> Value {
    json!({
        "Type": "Series",
        "Name": details.name,
        "OriginalTitle": details.original_name,
        "Overview": details.overview,
        "ProductionYear": details.first_air_date.as_deref().and_then(parse_year),
        "PremiereDate": details.first_air_date,
        "EndDate": details.last_air_date,
        "OriginalLanguage": details.original_language,
        "ChildCount": details.number_of_episodes,
        "ProviderIds": {"Tmdb": details.id.to_string()}
    })
}

fn season_details(details: TmdbSeasonDetails) -> Value {
    json!({
        "Type": "Season",
        "Name": details.name,
        "Overview": details.overview,
        "PremiereDate": details.air_date,
        "IndexNumber": details.season_number,
        "ProviderIds": {"Tmdb": details.id.to_string()}
    })
}

fn episode_details(details: TmdbEpisodeDetails) -> Value {
    json!({
        "Type": "Episode",
        "Name": details.name,
        "Overview": details.overview,
        "PremiereDate": details.air_date,
        "ParentIndexNumber": details.season_number,
        "IndexNumber": details.episode_number,
        "RunTimeTicks": details.runtime.map(runtime_ticks),
        "ProviderIds": {"Tmdb": details.id.to_string()}
    })
}

fn person_details(details: TmdbPersonDetails) -> Value {
    json!({
        "Type": "Person",
        "Name": details.name,
        "Overview": details.biography,
        "BirthDate": details.birthday,
        "DeathDate": details.deathday,
        "BirthLocation": details.place_of_birth,
        "ProviderIds": {"Tmdb": details.id.to_string()}
    })
}

fn collection_details(details: TmdbCollectionDetails) -> Value {
    json!({
        "Type": "BoxSet",
        "Name": details.name,
        "Overview": details.overview,
        "ProviderIds": {"Tmdb": details.id.to_string()},
        "Items": details.parts.into_iter().map(|part| json!({
            "Type": "Movie",
            "Name": part.title,
            "ProductionYear": part.release_date.as_deref().and_then(parse_year),
            "ProviderIds": {"Tmdb": part.id.to_string()}
        })).collect::<Vec<_>>()
    })
}

fn image_results(response: TmdbImagesResponse) -> Vec<Value> {
    let mut images = Vec::new();
    images.extend(response.posters.into_iter().filter_map(|image| {
        image.file_path.map(|path| {
            json!({
                "Type": "Primary",
                "Url": image_url(&path),
                "ThumbnailUrl": image_url(&path),
                "Language": image.iso_639_1,
                "Width": image.width,
                "Height": image.height,
                "ProviderName": "Tmdb"
            })
        })
    }));
    images.extend(response.backdrops.into_iter().filter_map(|image| {
        image.file_path.map(|path| {
            json!({
                "Type": "Backdrop",
                "Url": image_url(&path),
                "ThumbnailUrl": image_url(&path),
                "Language": image.iso_639_1,
                "Width": image.width,
                "Height": image.height,
                "ProviderName": "Tmdb"
            })
        })
    }));
    images.extend(response.profiles.into_iter().filter_map(|image| {
        image.file_path.map(|path| {
            json!({
                "Type": "Primary",
                "Url": image_url(&path),
                "ThumbnailUrl": image_url(&path),
                "Language": image.iso_639_1,
                "Width": image.width,
                "Height": image.height,
                "ProviderName": "Tmdb"
            })
        })
    }));
    images
}

fn trailer_results(response: TmdbVideosResponse) -> Vec<Value> {
    response
        .results
        .into_iter()
        .filter_map(|video| {
            let key = video.key?;
            let site = video.site.as_deref()?;
            let url = match site {
                "YouTube" => format!("https://www.youtube.com/watch?v={key}"),
                "Vimeo" => format!("https://vimeo.com/{key}"),
                _ => return None,
            };
            Some(json!({
                "Name": video.name,
                "Url": url,
                "Type": video.video_type.unwrap_or_else(|| "Trailer".to_owned()),
                "VideoId": key,
                "ProviderName": "Tmdb",
                "Official": video.official,
                "PublishedAt": video.published_at
            }))
        })
        .collect()
}

fn add_external_ids(provider_ids: &mut serde_json::Map<String, Value>, ids: TmdbExternalIds) {
    if let Some(value) = ids.imdb_id {
        provider_ids.insert("Imdb".to_owned(), Value::String(value));
    }
    if let Some(value) = ids.tvdb_id {
        provider_ids.insert("Tvdb".to_owned(), Value::String(value.to_string()));
    }
    if let Some(value) = ids.wikidata_id {
        provider_ids.insert("Wikidata".to_owned(), Value::String(value));
    }
}

fn language(request: &MetadataRequest) -> &str {
    request.language.as_deref().unwrap_or("zh-CN")
}

fn runtime_ticks(minutes: i32) -> i64 {
    i64::from(minutes.max(0)) * 60 * 10_000_000
}

fn image_url(path: &str) -> String {
    format!("https://image.tmdb.org/t/p/original{path}")
}

fn parse_year(value: &str) -> Option<i32> {
    value.get(0..4)?.parse().ok()
}

fn invalid(message: &str) -> PluginRpcError {
    PluginRpcError {
        code: "PLUGIN_INVALID_REQUEST".to_owned(),
        message: message.to_owned(),
    }
}

fn tmdb_error(error: luxd::application::tmdb::TmdbError) -> PluginRpcError {
    PluginRpcError {
        code: "PLUGIN_PROVIDER_ERROR".to_owned(),
        message: error.to_string(),
    }
}
