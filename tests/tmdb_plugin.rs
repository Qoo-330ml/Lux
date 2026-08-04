use std::{env, process::Stdio, time::Duration};

use axum::{Router, body::Body, http::Request, response::Response, routing::any};
use luxd::application::plugin_protocol::PluginResponse;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, Lines},
    net::TcpListener,
    process::{ChildStdout, Command},
};

async fn tmdb_stub(request: Request<Body>) -> Response<Body> {
    let path = request.uri().path();
    if path == "/3/search/movie" {
        json_response(json!({
            "page": 1,
            "total_pages": 1,
            "total_results": 1,
            "results": [{
                "id": 157336,
                "title": "Interstellar",
                "original_title": "Interstellar",
                "overview": "A test overview",
                "release_date": "2014-11-07",
                "original_language": "en"
            }]
        }))
    } else if path == "/3/search/tv" {
        json_response(json!({
            "page": 1,
            "total_pages": 1,
            "total_results": 1,
            "results": [{
                "id": 8,
                "name": "Test Series",
                "original_name": "Original Series",
                "overview": "Series overview",
                "first_air_date": "2021-01-02",
                "original_language": "en",
                "poster_path": "/series-poster.jpg",
                "backdrop_path": "/series-backdrop.jpg"
            }]
        }))
    } else if path == "/3/search/person" {
        json_response(json!({
            "page": 1,
            "total_pages": 1,
            "total_results": 1,
            "results": [{
                "id": 9,
                "name": "Test Person",
                "known_for_department": "Acting",
                "profile_path": "/profile.jpg"
            }]
        }))
    } else if path == "/3/search/collection" {
        json_response(json!({
            "page": 1,
            "total_pages": 1,
            "total_results": 1,
            "results": [{
                "id": 10,
                "name": "Test Collection",
                "overview": "Collection overview",
                "poster_path": "/collection-poster.jpg",
                "backdrop_path": "/collection-backdrop.jpg"
            }]
        }))
    } else if path == "/3/movie/157336" {
        json_response(json!({
            "id": 157336,
            "title": "Interstellar",
            "original_title": "Interstellar",
            "overview": "A test overview",
            "release_date": "2014-11-07",
            "original_language": "en",
            "poster_path": "/movie-poster.jpg",
            "backdrop_path": "/movie-backdrop.jpg"
        }))
    } else if path == "/3/tv/8" {
        json_response(json!({
            "id": 8,
            "name": "Test Series",
            "original_name": "Original Series",
            "overview": "Series overview",
            "first_air_date": "2021-01-02",
            "last_air_date": "2021-02-03",
            "original_language": "en",
            "number_of_seasons": 1,
            "number_of_episodes": 2,
            "poster_path": "/series-poster.jpg",
            "backdrop_path": "/series-backdrop.jpg",
            "seasons": [{
                "id": 801,
                "name": "Season 1",
                "overview": "Season overview",
                "air_date": "2021-01-02",
                "season_number": 1,
                "episode_count": 2,
                "poster_path": "/season-poster.jpg"
            }]
        }))
    } else if path == "/3/tv/8/season/1" {
        json_response(json!({
            "id": 801,
            "name": "Season 1",
            "overview": "Season overview",
            "air_date": "2021-01-02",
            "season_number": 1,
            "poster_path": "/season-poster.jpg",
            "episodes": [{
                "id": 802,
                "name": "Test Episode",
                "overview": "Episode overview",
                "air_date": "2021-01-03",
                "episode_number": 2,
                "season_number": 1,
                "still_path": "/still.jpg",
                "runtime": 45
            }]
        }))
    } else if path == "/3/tv/8/season/1/episode/2" {
        json_response(json!({
            "id": 802,
            "name": "Test Episode",
            "overview": "Episode overview",
            "air_date": "2021-01-03",
            "episode_number": 2,
            "season_number": 1,
            "still_path": "/still.jpg",
            "runtime": 45
        }))
    } else if path == "/3/person/9" {
        json_response(json!({
            "id": 9,
            "name": "Test Person",
            "biography": "Biography",
            "birthday": "1970-01-01",
            "known_for_department": "Acting",
            "place_of_birth": "Test City",
            "profile_path": "/profile.jpg"
        }))
    } else if path == "/3/collection/10" {
        json_response(json!({
            "id": 10,
            "name": "Test Collection",
            "overview": "Collection overview",
            "poster_path": "/collection-poster.jpg",
            "backdrop_path": "/collection-backdrop.jpg",
            "parts": [{
                "id": 157336,
                "title": "Interstellar",
                "release_date": "2014-11-07",
                "poster_path": "/movie-poster.jpg"
            }]
        }))
    } else if path == "/3/movie/157336/external_ids" {
        json_response(json!({
            "imdb_id": "tt0816692",
            "tvdb_id": 123,
            "wikidata_id": "Q123"
        }))
    } else if path == "/3/movie/157336/images" {
        json_response(json!({
            "posters": [{"file_path": "/movie-poster.jpg", "iso_639_1": "en", "width": 100, "height": 150}],
            "backdrops": [{"file_path": "/movie-backdrop.jpg", "iso_639_1": null, "width": 1920, "height": 1080}],
            "profiles": []
        }))
    } else if path == "/3/movie/157336/videos" {
        json_response(json!({
            "results": [{
                "id": "trailer-1",
                "key": "abc123",
                "name": "Official Trailer",
                "site": "YouTube",
                "type": "Trailer",
                "official": true,
                "published_at": "2020-01-01T00:00:00.000Z"
            }]
        }))
    } else if path == "/3/person/9/external_ids" {
        json_response(json!({
            "imdb_id": "nm0000009",
            "tvdb_id": 9009,
            "wikidata_id": "Q9"
        }))
    } else {
        Response::builder()
            .status(404)
            .body(Body::from("not found"))
            .expect("stub response should build")
    }
}

fn json_response(value: Value) -> Response<Body> {
    Response::new(Body::from(value.to_string()))
}

async fn rpc_call<W>(
    stdin: &mut W,
    stdout: &mut Lines<BufReader<ChildStdout>>,
    id: &str,
    method: &str,
    params: Value,
) -> Result<Value, Box<dyn std::error::Error>>
where
    W: AsyncWrite + Unpin,
{
    stdin
        .write_all(
            serde_json::to_string(&json!({
                "id": id,
                "method": method,
                "params": params
            }))?
            .as_bytes(),
        )
        .await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    let line = tokio::time::timeout(Duration::from_secs(2), stdout.next_line()).await??;
    let response: PluginResponse = serde_json::from_str(&line.ok_or("plugin closed stdout")?)?;
    if let Some(error) = response.error {
        return Err(format!("{}: {}", error.code, error.message).into());
    }
    response
        .result
        .ok_or_else(|| "plugin result is missing".into())
}

#[tokio::test]
async fn standalone_tmdb_plugin_uses_the_lux_rpc_contract() -> Result<(), Box<dyn std::error::Error>>
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(any(tmdb_stub)))
            .await
            .expect("TMDb stub should run");
    });

    let binary = env::var("CARGO_BIN_EXE_lux-plugin-tmdb")
        .or_else(|_| env::var("CARGO_BIN_EXE_lux_plugin_tmdb"))?;
    let mut child = Command::new(binary)
        .env("LUX_TMDB_BASE_URL", format!("http://{address}/"))
        .env("LUX_TMDB_API_KEY", "test-only-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("plugin stdin should exist");
    let stdout = child.stdout.take().expect("plugin stdout should exist");
    let mut stdout = BufReader::new(stdout).lines();
    let result = rpc_call(
        &mut stdin,
        &mut stdout,
        "request-1",
        "metadata.search",
        json!({"itemType": "Movie", "name": "Interstellar", "year": 2014}),
    )
    .await?;
    assert_eq!(result["items"][0]["Name"], "Interstellar");
    assert_eq!(result["items"][0]["ProviderIds"]["Tmdb"], "157336");

    child.kill().await?;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn standalone_tmdb_plugin_maps_emby_media_types_and_provider_data()
-> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(any(tmdb_stub)))
            .await
            .expect("TMDb stub should run");
    });
    let binary = env::var("CARGO_BIN_EXE_lux-plugin-tmdb")
        .or_else(|_| env::var("CARGO_BIN_EXE_lux_plugin_tmdb"))?;
    let mut child = Command::new(binary)
        .env("LUX_TMDB_BASE_URL", format!("http://{address}/"))
        .env("LUX_TMDB_API_KEY", "test-only-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("plugin stdin is missing")?;
    let stdout = child.stdout.take().ok_or("plugin stdout is missing")?;
    let mut stdout = BufReader::new(stdout).lines();

    let series_search = rpc_call(
        &mut stdin,
        &mut stdout,
        "series-search",
        "metadata.search",
        json!({"itemType": "Series", "name": "Test Series", "year": 2021}),
    )
    .await?;
    assert_eq!(series_search["items"][0]["Type"], "Series");
    assert_eq!(series_search["items"][0]["ProviderIds"]["Tmdb"], "8");

    let series = rpc_call(
        &mut stdin,
        &mut stdout,
        "series-get",
        "metadata.get",
        json!({"itemType": "Series", "tmdbId": 8}),
    )
    .await?;
    assert_eq!(series["metadata"]["Type"], "Series");
    assert_eq!(series["metadata"]["ProductionYear"], 2021);

    let season = rpc_call(
        &mut stdin,
        &mut stdout,
        "season-get",
        "metadata.get",
        json!({"itemType": "Season", "tmdbId": 8, "seasonNumber": 1}),
    )
    .await?;
    assert_eq!(season["metadata"]["Type"], "Season");
    assert_eq!(season["metadata"]["IndexNumber"], 1);

    let episode = rpc_call(
        &mut stdin,
        &mut stdout,
        "episode-get",
        "metadata.get",
        json!({"itemType": "Episode", "tmdbId": 8, "seasonNumber": 1, "episodeNumber": 2}),
    )
    .await?;
    assert_eq!(episode["metadata"]["Type"], "Episode");
    assert_eq!(episode["metadata"]["ParentIndexNumber"], 1);
    assert_eq!(episode["metadata"]["IndexNumber"], 2);

    let person = rpc_call(
        &mut stdin,
        &mut stdout,
        "person-get",
        "metadata.get",
        json!({"itemType": "Person", "tmdbId": 9}),
    )
    .await?;
    assert_eq!(person["metadata"]["Type"], "Person");
    assert_eq!(person["metadata"]["ProviderIds"]["Tmdb"], "9");

    let collection = rpc_call(
        &mut stdin,
        &mut stdout,
        "collection-get",
        "metadata.get",
        json!({"itemType": "BoxSet", "collectionId": 10}),
    )
    .await?;
    assert_eq!(collection["metadata"]["Type"], "BoxSet");

    let images = rpc_call(
        &mut stdin,
        &mut stdout,
        "movie-images",
        "metadata.images",
        json!({"itemType": "Movie", "tmdbId": 157336}),
    )
    .await?;
    assert_eq!(images["images"][0]["Type"], "Primary");
    assert_eq!(images["images"][0]["ProviderName"], "Tmdb");

    let ids = rpc_call(
        &mut stdin,
        &mut stdout,
        "movie-ids",
        "metadata.externalIds",
        json!({"itemType": "Movie", "tmdbId": 157336}),
    )
    .await?;
    assert_eq!(ids["providerIds"]["Imdb"], "tt0816692");
    assert_eq!(ids["providerIds"]["Tmdb"], "157336");

    let trailers = rpc_call(
        &mut stdin,
        &mut stdout,
        "movie-trailers",
        "metadata.trailers",
        json!({"itemType": "Movie", "tmdbId": 157336}),
    )
    .await?;
    assert_eq!(trailers["trailers"][0]["Type"], "Trailer");
    assert_eq!(trailers["trailers"][0]["VideoId"], "abc123");

    child.kill().await?;
    server.abort();
    Ok(())
}
