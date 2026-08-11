use std::time::Duration;

use axum::{
    Json, Router,
    body::Body,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use luxd::{
    api::{AppState, app_with_state},
    application::{
        candidates::{MetadataCandidateService, MetadataSelectionMode, MetadataSelectionService},
        images::ImageWriteService,
        libraries::LibraryService,
        metadata::MetadataEnricher,
        metadata_paths::{library_item_directory, people_directory},
        people::PeopleService,
        reidentify::{MetadataRefreshMode, MetadataReidentifyService},
        scanner::LibraryScanner,
        setup::SetupService,
        tmdb::{TmdbClient, TmdbClientConfig},
        tmdb_plugin::TmdbProvider,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use uuid::Uuid;

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

#[tokio::test]
async fn admin_selection_fills_missing_fields_and_writes_nfo_and_images()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(true).await?;
    let (image_url, image_server) = start_image_stub().await?;
    let fallback_path = fixture.movie_dir.join("Example.Movie.2020-thumb.jpg");
    tokio::fs::write(&fallback_path, b"ffmpeg-fallback").await?;
    sqlx::query(
        "INSERT INTO item_images (
            id, item_id, image_type, image_index, local_path, file_size, content_tag, source
         ) VALUES (?, ?, 'THUMB', 0, ?, ?, 'fallback', 'STRM_FFMPEG')",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&fixture.item_id)
    .bind(fallback_path.to_string_lossy().as_ref())
    .bind(14_i64)
    .execute(fixture.database.pool())
    .await?;
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "Online Title",
            "overview": "Online Overview",
            "tagline": "速度与信念",
            "website": "https://example.invalid/movie",
            "productionYear": 2025,
            "status": "Released",
            "originalLanguage": "zh",
            "setName": "飞驰人生",
            "setId": "1281825",
            "posterUrl": "https://image.tmdb.org/t/p/original/poster.jpg",
            "backdropUrl": "https://image.tmdb.org/t/p/original/backdrop.jpg",
            "rating": 8.6,
            "votes": 123,
            "runtime": 126,
            "certification": "PG-13",
            "premiereDate": "2025-02-17",
            "countries": ["China"],
            "genres": ["剧情", "喜剧"],
            "studios": ["Stub Films"],
            "providerIds": {"Tmdb": "7", "Imdb": "tt0000007"},
            "directors": [{"providerId": "11", "name": "导演甲"}],
            "writers": [{"providerId": "12", "name": "编剧甲"}],
            "actors": [{"id": "9", "name": "演员甲", "character": "角色甲", "order": 0}],
            "trailers": ["https://www.youtube.com/watch?v=abc123"],
            "images": {
                "POSTER": [format!("{image_url}/poster")],
                "FANART": [format!("{image_url}/fanart")],
                "THUMB": [format!("{image_url}/thumb")]
            }
        }),
    )
    .await?;
    let item_id = fixture.item_id.clone();
    let (base_url, lux_server) = start_lux(&fixture).await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = login(&client, &base_url).await?;

    let response = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{item_id}/identify/candidates/{candidate_id}/select"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "mode": "fillMissing" }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await?;
    assert_eq!(body["status"], "ONLINE_CONFIRMED");
    assert_eq!(body["mode"], "fillMissing");
    assert_eq!(body["imageTypes"], json!(["POSTER", "FANART", "THUMB"]));

    let nfo = tokio::fs::read_to_string(&fixture.movie_dir.join("movie.nfo")).await?;
    assert!(nfo.contains("<title>本地标题</title>"));
    assert!(nfo.contains("<plot>Online Overview</plot>"));
    assert!(nfo.contains("<tagline>速度与信念</tagline>"));
    assert!(nfo.contains("<website>https://example.invalid/movie</website>"));
    assert!(nfo.contains("<status>Released</status>"));
    assert!(nfo.contains("<language>zh</language>"));
    assert!(nfo.contains("<set>飞驰人生</set>"));
    assert!(nfo.contains("<setid>1281825</setid>"));
    assert!(nfo.contains("<rating>8.6</rating>"));
    assert!(nfo.contains("<runtime>126</runtime>"));
    assert!(nfo.contains("<mpaa>PG-13</mpaa>"));
    assert!(nfo.contains("<genre>剧情</genre>"));
    assert!(nfo.contains("<country>China</country>"));
    assert!(nfo.contains("<studio>Stub Films</studio>"));
    assert!(nfo.contains("<director tmdbid=\"11\">导演甲</director>"));
    assert!(nfo.contains("<actor>"));
    assert!(nfo.contains("<name>演员甲</name>"));
    assert!(nfo.contains("<trailer>https://www.youtube.com/watch?v=abc123</trailer>"));
    let metadata_item_dir = tokio::fs::canonicalize(library_item_directory(
        &fixture.config.config_dir,
        &item_id,
    )?)
    .await?;
    assert_eq!(
        tokio::fs::read(metadata_item_dir.join("poster.png")).await?,
        PNG_1X1
    );
    assert_eq!(
        tokio::fs::read(metadata_item_dir.join("fanart.webp")).await?,
        b"RIFF\x04\x00\x00\x00WEBP"
    );
    assert_eq!(
        tokio::fs::read(metadata_item_dir.join("thumb.png")).await?,
        PNG_1X1
    );
    assert_eq!(tokio::fs::read(&fallback_path).await?, b"ffmpeg-fallback");
    let status: String =
        sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(status, "ONLINE_CONFIRMED");
    let rating: Option<f64> = sqlx::query_scalar("SELECT rating FROM media_items WHERE id = ?")
        .bind(&item_id)
        .fetch_one(fixture.database.pool())
        .await?;
    assert_eq!(rating, Some(8.6));
    let rating_source: Option<String> =
        sqlx::query_scalar("SELECT rating_source FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(rating_source.as_deref(), Some("TMDB"));
    let detail = client
        .get(format!("{base_url}/api/v1/items/{item_id}"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(detail.status(), StatusCode::OK);
    let detail_body: Value = detail.json().await?;
    assert_eq!(detail_body["rating"], 8.6);
    assert_eq!(detail_body["ratingSource"], "TMDB");
    let candidate_status: String =
        sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
            .bind(&candidate_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(candidate_status, "SELECTED");
    let image_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM item_images WHERE item_id = ? AND source = 'TMDB'",
    )
    .bind(&item_id)
    .fetch_one(fixture.database.pool())
    .await?;
    assert_eq!(image_count, 3);
    let fallback_required: i64 =
        sqlx::query_scalar("SELECT thumbnail_fallback_required FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(fallback_required, 0);

    lux_server.abort();
    image_server.abort();
    Ok(())
}

#[tokio::test]
async fn full_refresh_preserves_locked_nfo_fields_and_replaces_existing_images()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(true).await?;
    tokio::fs::write(fixture.movie_dir.join("poster.png"), b"old-poster").await?;
    sqlx::query("UPDATE media_items SET locked_fields_json = ? WHERE id = ?")
        .bind(json!(["title"]).to_string())
        .bind(&fixture.item_id)
        .execute(fixture.database.pool())
        .await?;
    let (image_url, image_server) = start_image_stub().await?;
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "Online Title",
            "overview": "Online Overview",
            "posterUrl": format!("{image_url}/poster")
        }),
    )
    .await?;
    let service = ImageWriteService::new(fixture.database.clone())?;
    let selection = MetadataSelectionService::new(fixture.database.clone(), service);

    let report = selection
        .select(
            &fixture.item_id,
            &candidate_id,
            MetadataSelectionMode::RefreshUnlocked,
        )
        .await?;

    assert_eq!(report.mode, MetadataSelectionMode::RefreshUnlocked);
    let nfo = tokio::fs::read_to_string(fixture.movie_dir.join("movie.nfo")).await?;
    assert!(nfo.contains("<title>本地标题</title>"));
    assert!(nfo.contains("<plot>Online Overview</plot>"));
    assert_eq!(
        tokio::fs::read(fixture.movie_dir.join("poster.png")).await?,
        PNG_1X1
    );
    let fallback_required: i64 =
        sqlx::query_scalar("SELECT thumbnail_fallback_required FROM media_items WHERE id = ?")
            .bind(&fixture.item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(fallback_required, 1);

    image_server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_selection_persists_cast_in_config_and_detail_api()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    let profile_dir = fixture.config.config_dir.join("people/profiles");
    tokio::fs::create_dir_all(&profile_dir).await?;
    tokio::fs::write(profile_dir.join("9.png"), PNG_1X1).await?;
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "演员电影",
            "actors": [{
                "id": 9,
                "name": "演员甲",
                "character": "角色甲",
                "order": 0,
                "profileUrl": "https://image.tmdb.org/t/p/w185/profile.jpg"
            }]
        }),
    )
    .await?;
    let (base_url, lux_server) = start_lux(&fixture).await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = login(&client, &base_url).await?;

    let selected = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{}/identify/candidates/{candidate_id}/select",
            fixture.item_id
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "mode": "fillMissing" }))
        .send()
        .await?;
    assert_eq!(selected.status(), StatusCode::OK);
    let selected_body: Value = selected.json().await?;
    assert_eq!(selected_body["actorCount"], 1);

    let people_file =
        library_item_directory(&fixture.config.config_dir, &fixture.item_id)?.join("people.json");
    let people: Value = serde_json::from_slice(&tokio::fs::read(people_file).await?)?;
    assert_eq!(people[0]["name"], "演员甲");
    assert_eq!(people[0]["provider"], "tmdb");
    let person_dir = people_directory(&fixture.config.config_dir, "演员甲", "tmdb", "9")?;
    assert!(person_dir.join("person.nfo").exists());

    let detail = client
        .get(format!("{base_url}/api/v1/items/{}", fixture.item_id))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(detail.status(), StatusCode::OK);
    let detail_body: Value = detail.json().await?;
    assert_eq!(detail_body["actors"][0]["name"], "演员甲");
    assert_eq!(detail_body["actors"][0]["character"], "角色甲");
    assert_eq!(
        detail_body["actors"][0]["imageUrl"],
        "/api/v1/people/9/image"
    );

    let profile = client
        .get(format!("{base_url}/api/v1/people/9/image"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(profile.status(), StatusCode::OK);
    assert_eq!(
        profile
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("image/png")
    );

    lux_server.abort();
    Ok(())
}

#[tokio::test]
async fn local_nfo_cast_is_exposed_by_detail_api_without_selection()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    tokio::fs::write(
        fixture.movie_dir.join("movie.nfo"),
        r#"<movie><title>本地演员电影</title><actor><name>演员甲</name><role>角色甲</role><type>Actor</type><tmdbid>9</tmdbid><order>0</order></actor><actor><name>演员乙</name><role>角色乙</role><type>Actor</type><tmdbid>10</tmdbid><order>1</order></actor></movie>"#,
    )
    .await?;
    let person_dir = people_directory(&fixture.config.config_dir, "演员甲", "tmdb", "9")?;
    tokio::fs::create_dir_all(&person_dir).await?;
    tokio::fs::write(person_dir.join("folder.png"), PNG_1X1).await?;

    let library_id: String = sqlx::query_scalar("SELECT library_id FROM media_items WHERE id = ?")
        .bind(&fixture.item_id)
        .fetch_one(fixture.database.pool())
        .await?;
    let people = PeopleService::new(fixture.config.config_dir.clone());
    MetadataEnricher::new(fixture.database.clone())
        .with_people(people)
        .enrich_movie_library(library_id.parse()?)
        .await?;

    let (base_url, lux_server) = start_lux(&fixture).await?;
    let client = reqwest::Client::new();
    let (cookies, _) = login(&client, &base_url).await?;
    let detail = client
        .get(format!("{base_url}/api/v1/items/{}", fixture.item_id))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(detail.status(), StatusCode::OK);
    let body: Value = detail.json().await?;
    assert_eq!(body["actors"][0]["name"], "演员甲");
    assert_eq!(body["actors"][0]["imageUrl"], "/api/v1/people/9/image");
    assert_eq!(body["actors"][1]["name"], "演员乙");
    assert!(body["actors"][1]["imageUrl"].is_null());

    lux_server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_selection_writes_only_configured_candidate_image_types()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    configure_image_strategy(&fixture.database, &fixture.item_id).await?;
    let (image_url, image_server) = start_image_stub().await?;
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "Configured Images",
            "images": {
                "POSTER": [
                    format!("{image_url}/poster-first"),
                    format!("{image_url}/poster-second")
                ],
                "LOGO": [format!("{image_url}/logo")],
                "THUMB": [format!("{image_url}/thumb")],
                "BANNER": [format!("{image_url}/banner")],
                "DISC": [],
                "ART": [format!("{image_url}/art")],
                "WALLPAPER": [format!("{image_url}/wallpaper")]
            }
        }),
    )
    .await?;
    let service = ImageWriteService::new(fixture.database.clone())?;
    let selection = MetadataSelectionService::new(fixture.database.clone(), service);

    let report = selection
        .select(
            &fixture.item_id,
            &candidate_id,
            MetadataSelectionMode::RefreshUnlocked,
        )
        .await?;

    assert_eq!(report.image_types, vec!["POSTER", "LOGO", "THUMB", "ART"]);
    let image_rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT image_type, local_path FROM item_images WHERE item_id = ? ORDER BY image_type",
    )
    .bind(&fixture.item_id)
    .fetch_all(fixture.database.pool())
    .await?;
    let image_names = image_rows
        .iter()
        .map(|(image_type, path)| {
            (
                image_type.clone(),
                std::path::Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        image_names,
        vec![
            ("ART".to_owned(), "art.png".to_owned()),
            ("LOGO".to_owned(), "logo.png".to_owned()),
            ("POSTER".to_owned(), "poster.png".to_owned()),
            ("THUMB".to_owned(), "thumb.png".to_owned()),
        ]
    );
    assert!(fixture.movie_dir.join("poster.png").exists());
    assert!(!fixture.movie_dir.join("poster-second.png").exists());
    assert!(!fixture.movie_dir.join("banner.png").exists());
    assert!(!fixture.movie_dir.join("wallpaper.png").exists());

    image_server.abort();
    Ok(())
}

#[tokio::test]
async fn failed_selection_stays_pending_and_can_be_retried()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    let (image_url, image_server) = start_image_stub().await?;
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "Retry Title",
            "posterUrl": format!("{image_url}/bad")
        }),
    )
    .await?;
    let item_id = fixture.item_id.clone();
    let (base_url, lux_server) = start_lux(&fixture).await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = login(&client, &base_url).await?;
    let path = format!(
        "{base_url}/api/v1/admin/items/{item_id}/identify/candidates/{candidate_id}/select"
    );

    let failed = client
        .post(&path)
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "mode": "refreshUnlocked" }))
        .send()
        .await?;
    assert!(failed.status().is_server_error());
    let status: String =
        sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(status, "LOCAL_CONFIRMED");
    let candidate_status: String =
        sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
            .bind(&candidate_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(candidate_status, "PENDING");
    assert!(!fixture.movie_dir.join("movie.nfo").exists());

    sqlx::query("UPDATE metadata_candidates SET candidate_json = ? WHERE id = ?")
        .bind(
            json!({
                "title": "Retry Title",
                "posterUrl": format!("{image_url}/poster")
            })
            .to_string(),
        )
        .bind(&candidate_id)
        .execute(fixture.database.pool())
        .await?;
    let retried = client
        .post(&path)
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "mode": "refreshUnlocked" }))
        .send()
        .await?;
    assert_eq!(retried.status(), StatusCode::OK);
    let status: String =
        sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(status, "ONLINE_CONFIRMED");
    let candidate_status: String =
        sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
            .bind(&candidate_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(candidate_status, "SELECTED");

    lux_server.abort();
    image_server.abort();
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn selection_failure_after_writeback_checks_does_not_confirm_candidate()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let fixture = prepare_fixture(false).await?;
    symlink(
        fixture.movie_dir.join("missing-thumb.jpg"),
        fixture.movie_dir.join("thumb.jpg"),
    )?;
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({ "title": "Retry Without Confirmation" }),
    )
    .await?;
    let (base_url, lux_server) = start_lux(&fixture).await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = login(&client, &base_url).await?;

    let response = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{}/identify/candidates/{candidate_id}/select",
            fixture.item_id
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "mode": "refreshUnlocked" }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let item_status: String =
        sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
            .bind(&fixture.item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(item_status, "LOCAL_CONFIRMED");
    let candidate_status: String =
        sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
            .bind(&candidate_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(candidate_status, "PENDING");

    lux_server.abort();
    Ok(())
}

#[tokio::test]
async fn series_and_season_selection_writes_to_their_media_directories()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_series_fixture().await?;
    let (image_url, image_server) = start_image_stub().await?;
    let series_candidate_id = insert_candidate(
        &fixture.database,
        &fixture.series_id,
        json!({
            "title": "Online Series",
            "overview": "Online Series Overview",
            "premiereDate": "2020-01-01",
            "endDate": "2020-02-01",
            "status": "Ended",
            "originalLanguage": "en",
            "posterUrl": format!("{image_url}/poster")
        }),
    )
    .await?;
    let season_candidate_id = insert_candidate(
        &fixture.database,
        &fixture.season_id,
        json!({
            "title": "Online Season",
            "overview": "Online Season Overview",
            "posterUrl": format!("{image_url}/poster")
        }),
    )
    .await?;
    let selection = MetadataSelectionService::new(
        fixture.database.clone(),
        ImageWriteService::new(fixture.database.clone())?,
    );

    selection
        .select(
            &fixture.series_id,
            &series_candidate_id,
            MetadataSelectionMode::FillMissing,
        )
        .await
        .map_err(|error| format!("series selection failed: {error:?}"))?;
    selection
        .select(
            &fixture.season_id,
            &season_candidate_id,
            MetadataSelectionMode::FillMissing,
        )
        .await
        .map_err(|error| format!("season selection failed: {error:?}"))?;
    let lifecycle: (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT premiere_date, last_air_date, status, original_language
             FROM media_items WHERE id = ?",
    )
    .bind(&fixture.series_id)
    .fetch_one(fixture.database.pool())
    .await?;
    assert_eq!(lifecycle.0.as_deref(), Some("2020-01-01"));
    assert_eq!(lifecycle.1.as_deref(), Some("2020-02-01"));
    assert_eq!(lifecycle.2.as_deref(), Some("Ended"));
    assert_eq!(lifecycle.3.as_deref(), Some("en"));
    assert!(
        tokio::fs::read_to_string(fixture.series_dir.join("tvshow.nfo"))
            .await?
            .contains("<plot>Online Series Overview</plot>")
    );
    assert!(fixture.series_dir.join("poster.png").exists());
    assert!(!fixture.root.join("Drama").join("tvshow.nfo").exists());
    assert!(
        tokio::fs::read_to_string(fixture.season_dir.join("season01.nfo"))
            .await?
            .contains("<plot>Online Season Overview</plot>")
    );
    assert!(fixture.season_dir.join("poster.png").exists());

    image_server.abort();
    Ok(())
}

#[tokio::test]
async fn series_candidate_search_persists_cast_data() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_series_fixture().await?;
    let tmdb_app = Router::new().fallback(any(|request: Request<Body>| async move {
        let body = match request.uri().path() {
            "/3/search/tv" => json!({
                "page": 1,
                "total_pages": 1,
                "total_results": 1,
                "results": [{
                    "id": 8,
                    "name": "Example Show",
                    "original_name": "Example Show",
                    "overview": "Series overview",
                    "first_air_date": "2020-01-01",
                    "original_language": "en",
                    "vote_average": 8.0
                }]
            }),
            "/3/tv/8" => json!({
                "id": 8,
                "name": "Example Show",
                "original_name": "Example Show Original",
                "overview": "Series overview",
                "first_air_date": "2020-01-01",
                "last_air_date": "2020-02-01",
                "status": "Ended",
                "original_language": "en",
                "vote_average": 8.0
            }),
            "/3/tv/8/credits" => json!({
                "cast": [{
                    "id": 10,
                    "name": "剧集演员",
                    "character": "剧集角色",
                    "profile_path": null,
                    "order": 0
                }]
            }),
            _ => return StatusCode::NOT_FOUND.into_response(),
        };
        Json(body).into_response()
    }));
    let tmdb_listener = TcpListener::bind("127.0.0.1:0").await?;
    let tmdb_address = tmdb_listener.local_addr()?;
    let tmdb_server = tokio::spawn(async move { axum::serve(tmdb_listener, tmdb_app).await });
    let tmdb = TmdbClient::new(TmdbClientConfig {
        base_url: format!("http://{tmdb_address}"),
        proxy_url: None,
        api_key: None,
        read_access_token: Some("stub-token".to_owned()),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?;
    let candidates = MetadataCandidateService::new(fixture.database.clone());

    let page = candidates
        .search_and_store(
            &fixture.series_id,
            "Example Show",
            Some(2020),
            &TmdbProvider::from(tmdb),
        )
        .await?;

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].candidate["actors"][0]["name"], "剧集演员");
    assert_eq!(
        page.items[0].candidate["actors"][0]["character"],
        "剧集角色"
    );
    assert_eq!(page.items[0].candidate["premiereDate"], "2020-01-01");
    assert_eq!(page.items[0].candidate["endDate"], "2020-02-01");
    assert_eq!(page.items[0].candidate["status"], "Ended");
    assert_eq!(page.items[0].candidate["originalLanguage"], "en");

    tmdb_server.abort();
    Ok(())
}

#[tokio::test]
async fn completed_scan_automatically_matches_and_writes_metadata()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    let library_id: String = sqlx::query_scalar("SELECT library_id FROM media_items WHERE id = ?")
        .bind(&fixture.item_id)
        .fetch_one(fixture.database.pool())
        .await?;
    sqlx::query("UPDATE libraries SET scraper_id = 'tmdb' WHERE id = ?")
        .bind(&library_id)
        .execute(fixture.database.pool())
        .await?;

    let tmdb_app = Router::new().fallback(any(|| async {
        Json(json!({
            "page": 1,
            "total_pages": 1,
            "total_results": 1,
            "results": [{
                "id": 999,
                "title": "Example Movie",
                "original_title": "Example Movie",
                "overview": "Automatically matched overview.",
                "release_date": "2020-04-01",
                "original_language": "en"
            }]
        }))
    }));
    let tmdb_listener = TcpListener::bind("127.0.0.1:0").await?;
    let tmdb_address = tmdb_listener.local_addr()?;
    let tmdb_server = tokio::spawn(async move { axum::serve(tmdb_listener, tmdb_app).await });
    let tmdb = TmdbClient::new(TmdbClientConfig {
        base_url: format!("http://{tmdb_address}"),
        proxy_url: None,
        api_key: None,
        read_access_token: Some("stub-token".to_owned()),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?;
    let selection = MetadataSelectionService::new(
        fixture.database.clone(),
        ImageWriteService::new(fixture.database.clone())?,
    );
    let metadata = MetadataReidentifyService::with_selection(
        fixture.database.clone(),
        TmdbProvider::from(tmdb),
        Some(selection),
    );
    let scan_jobs = luxd::application::scanner::ScanJobService::new(fixture.database.clone());
    let scan_job = scan_jobs
        .create_movie_scan_job_with_metadata(library_id.parse()?, true)
        .await?;
    scan_jobs
        .run_to_completion_with_metadata(&scan_job.id, 100, None, Some(metadata))
        .await?;

    for _ in 0..80 {
        let status: String = sqlx::query_scalar(
            "SELECT status FROM metadata_reidentify_jobs ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_one(fixture.database.pool())
        .await?;
        if status == "COMPLETED" || status == "FAILED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let status: String =
        sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
            .bind(&fixture.item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(status, "ONLINE_CONFIRMED");
    let nfo = tokio::fs::read_to_string(fixture.movie_dir.join("movie.nfo")).await?;
    assert!(nfo.contains("<plot>Automatically matched overview.</plot>"));
    let mode: String = sqlx::query_scalar(
        "SELECT mode FROM metadata_reidentify_jobs ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(fixture.database.pool())
    .await?;
    assert_eq!(mode, MetadataRefreshMode::FillMissing.as_str());

    tmdb_server.abort();
    Ok(())
}

struct Fixture {
    _temp_dir: TempDir,
    config: Config,
    database: Database,
    setup: SetupService,
    item_id: String,
    movie_dir: std::path::PathBuf,
}

struct SeriesFixture {
    _temp_dir: TempDir,
    database: Database,
    root: std::path::PathBuf,
    series_dir: std::path::PathBuf,
    season_dir: std::path::PathBuf,
    series_id: String,
    season_id: String,
}

async fn prepare_fixture(with_local_nfo: bool) -> Result<Fixture, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Example Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2020.mkv"), b"fixture").await?;
    if with_local_nfo {
        tokio::fs::write(
            movie_dir.join("movie.nfo"),
            "<movie><title>本地标题</title><custom>keep</custom></movie>",
        )
        .await?;
    }
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    if with_local_nfo {
        MetadataEnricher::new(database.clone())
            .enrich_movie_library(library.id)
            .await?;
    }
    let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
        .fetch_one(database.pool())
        .await?;
    Ok(Fixture {
        _temp_dir: temp_dir,
        config,
        database,
        setup,
        item_id,
        movie_dir,
    })
}

async fn prepare_series_fixture() -> Result<SeriesFixture, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let root = temp_dir.path().join("Shows");
    let series_dir = root.join("Drama").join("Example Show (2020)");
    let season_dir = series_dir.join("Season 01");
    tokio::fs::create_dir_all(&season_dir).await?;
    tokio::fs::write(season_dir.join("Example.Show.S01E01.mkv"), b"fixture").await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;
    let series_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'SERIES' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let season_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'SEASON' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    Ok(SeriesFixture {
        _temp_dir: temp_dir,
        database,
        root,
        series_dir,
        season_dir,
        series_id,
        season_id,
    })
}

async fn insert_candidate(
    database: &Database,
    item_id: &str,
    candidate: Value,
) -> Result<String, Box<dyn std::error::Error>> {
    let candidate_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO metadata_candidates
         (id, item_id, provider, provider_id, candidate_json, score, status)
         VALUES (?, ?, 'TMDB', '603', ?, 100, 'PENDING')",
    )
    .bind(&candidate_id)
    .bind(item_id)
    .bind(candidate.to_string())
    .execute(database.pool())
    .await?;
    Ok(candidate_id)
}

async fn configure_image_strategy(
    database: &Database,
    item_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let library_id: String = sqlx::query_scalar("SELECT library_id FROM media_items WHERE id = ?")
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
    sqlx::query("UPDATE libraries SET media_strategy_json = ? WHERE id = ?")
        .bind(
            json!({
                "images": {
                    "poster": true,
                    "artwork": true,
                    "banner": false,
                    "logo": true,
                    "thumbnail": true,
                    "disc": true,
                    "wallpaper": false
                }
            })
            .to_string(),
        )
        .bind(library_id)
        .execute(database.pool())
        .await?;
    Ok(())
}

async fn start_image_stub()
-> Result<(String, tokio::task::JoinHandle<Result<(), std::io::Error>>), Box<dyn std::error::Error>>
{
    let app = Router::new().route(
        "/{name}",
        get(|path: axum::extract::Path<String>| async move {
            match path.0.as_str() {
                "poster" => Response::builder()
                    .header(CONTENT_TYPE, "image/png")
                    .body(Body::from(PNG_1X1.to_vec()))
                    .unwrap(),
                "fanart" => Response::builder()
                    .header(CONTENT_TYPE, "image/webp")
                    .body(Body::from(b"RIFF\x04\x00\x00\x00WEBP".to_vec()))
                    .unwrap(),
                "poster-second" => Response::builder()
                    .header(CONTENT_TYPE, "image/png")
                    .body(Body::from(b"broken".to_vec()))
                    .unwrap(),
                "poster-first" | "logo" | "thumb" | "banner" | "art" | "wallpaper" => {
                    Response::builder()
                        .header(CONTENT_TYPE, "image/png")
                        .body(Body::from(PNG_1X1.to_vec()))
                        .unwrap()
                }
                "bad" => Response::builder()
                    .header(CONTENT_TYPE, "image/png")
                    .body(Body::from(b"broken".to_vec()))
                    .unwrap(),
                _ => Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::empty())
                    .unwrap(),
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    Ok((
        format!("http://{address}"),
        tokio::spawn(async move { axum::serve(listener, app).await }),
    ))
}

async fn start_lux(
    fixture: &Fixture,
) -> Result<(String, tokio::task::JoinHandle<Result<(), std::io::Error>>), Box<dyn std::error::Error>>
{
    let auth = WebAuthService::new(fixture.database.clone())?;
    let emby_auth = EmbyAuthService::new(fixture.database.clone())?;
    let app = app_with_state(AppState::ready(
        fixture.config.clone(),
        fixture.database.clone(),
        fixture.setup.clone(),
        auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    Ok((
        format!("http://{address}"),
        tokio::spawn(async move { axum::serve(listener, app).await }),
    ))
}

async fn login(
    client: &reqwest::Client,
    base_url: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let session = cookie_value(response.headers(), "lux_session");
    let csrf = cookie_value(response.headers(), "lux_csrf");
    Ok((format!("lux_session={session}; lux_csrf={csrf}"), csrf))
}

fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> String {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _) = value.split_once(';')?;
            let (cookie_name, cookie_value) = pair.split_once('=')?;
            (cookie_name == name).then(|| cookie_value.to_owned())
        })
        .expect("expected cookie")
}
