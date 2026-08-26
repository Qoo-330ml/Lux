use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Body,
    extract::State as AxumState,
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
        metadata_paths::{library_item_directory, lux_person_directory, people_directory},
        people::PeopleService,
        reidentify::{MetadataRefreshMode, MetadataReidentifyService},
        scanner::LibraryScanner,
        scraper::ScraperProvider,
        setup::SetupService,
        tmdb::{TmdbClient, TmdbClientConfig},
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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
            "actors": [{
                "id": "9",
                "name": "演员甲",
                "character": "角色甲",
                "order": 0,
                "person": {
                    "biography": "演员甲的生平介绍",
                    "birthday": "1970-01-01",
                    "knownForDepartment": "Acting",
                    "placeOfBirth": "测试城市"
                }
            }],
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
    assert!(nfo.contains("<tmdbid>9</tmdbid>"));
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
        sqlx::query_scalar("SELECT poster_fallback_required FROM media_items WHERE id = ?")
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
        sqlx::query_scalar("SELECT poster_fallback_required FROM media_items WHERE id = ?")
            .bind(&fixture.item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(fallback_required, 0);

    image_server.abort();
    Ok(())
}

#[tokio::test]
async fn full_refresh_keeps_nfo_when_image_download_fails() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = prepare_fixture(true).await?;
    let (image_url, image_server) = start_image_stub().await?;
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "Online Title",
            "overview": "Online Overview",
            "posterUrl": format!("{image_url}/missing")
        }),
    )
    .await?;
    let selection = MetadataSelectionService::with_config_dir(
        fixture.database.clone(),
        ImageWriteService::new(fixture.database.clone())?,
        fixture.config.config_dir.clone(),
    );

    let report = selection
        .select(
            &fixture.item_id,
            &candidate_id,
            MetadataSelectionMode::RefreshUnlocked,
        )
        .await?;

    assert_eq!(report.status, "ONLINE_CONFIRMED");
    let nfo = tokio::fs::read_to_string(fixture.movie_dir.join("movie.nfo")).await?;
    assert!(nfo.contains("<plot>Online Overview</plot>"));
    let candidate_status: String =
        sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
            .bind(&candidate_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(candidate_status, "SELECTED");

    image_server.abort();
    Ok(())
}

#[tokio::test]
async fn one_media_image_selection_is_bounded_to_four_concurrent_downloads()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    configure_image_strategy(&fixture.database, &fixture.item_id).await?;
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let handler_active = Arc::clone(&active);
    let handler_maximum = Arc::clone(&maximum);
    let handler_requests = Arc::clone(&requests);
    let app = Router::new().route(
        "/{image_type}",
        get(move || {
            let active = Arc::clone(&handler_active);
            let maximum = Arc::clone(&handler_maximum);
            let requests = Arc::clone(&handler_requests);
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(current, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(40)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                Response::builder()
                    .header(CONTENT_TYPE, "image/png")
                    .body(Body::from(PNG_1X1.to_vec()))
                    .expect("test image response should be valid")
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let image_url = format!("http://{address}");
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "Online Movie",
            "overview": "Online overview",
            "productionYear": 2020,
            "providerIds": {"Tmdb": "603"},
            "images": {
                "POSTER": [format!("{image_url}/poster")],
                "FANART": [format!("{image_url}/fanart")],
                "LOGO": [format!("{image_url}/logo")],
                "THUMB": [format!("{image_url}/thumb")],
                "DISC": [format!("{image_url}/disc")]
            }
        }),
    )
    .await?;
    let selection = MetadataSelectionService::new(
        fixture.database.clone(),
        ImageWriteService::new(fixture.database.clone())?,
    );

    let report = selection
        .select(
            &fixture.item_id,
            &candidate_id,
            MetadataSelectionMode::FillMissing,
        )
        .await?;

    assert_eq!(report.image_types.len(), 5);
    assert_eq!(requests.load(Ordering::SeqCst), 5);
    assert!(maximum.load(Ordering::SeqCst) <= 4);
    assert!(maximum.load(Ordering::SeqCst) > 1);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn fill_missing_selection_preserves_existing_actor_relation_without_credits()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(true).await?;
    let people = PeopleService::new(fixture.config.config_dir.clone())
        .with_database(fixture.database.clone());
    let selection = MetadataSelectionService::with_config_dir(
        fixture.database.clone(),
        ImageWriteService::new(fixture.database.clone())?,
        fixture.config.config_dir.clone(),
    );
    let first_candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "Initial Title",
            "actors": [{
                "id": "9",
                "name": "Existing Actor",
                "character": "Existing Role",
                "order": 0
            }]
        }),
    )
    .await?;
    selection
        .select(
            &fixture.item_id,
            &first_candidate_id,
            MetadataSelectionMode::FillMissing,
        )
        .await?;
    let library_id: String = sqlx::query_scalar("SELECT library_id FROM media_items WHERE id = ?")
        .bind(&fixture.item_id)
        .fetch_one(fixture.database.pool())
        .await?;
    MetadataEnricher::new(fixture.database.clone())
        .enrich_movie_library(library_id.parse()?)
        .await?;
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({ "title": "Online Title", "overview": "Online Overview" }),
    )
    .await?;

    selection
        .select(
            &fixture.item_id,
            &candidate_id,
            MetadataSelectionMode::FillMissing,
        )
        .await?;

    let actors = people.list_item_actors(&fixture.item_id).await?;
    assert_eq!(actors.len(), 1);
    assert_eq!(actors[0].name, "Existing Actor");
    assert_eq!(actors[0].character.as_deref(), Some("Existing Role"));
    Ok(())
}

#[tokio::test]
async fn fill_missing_skips_a_backoff_image_and_uses_the_next_candidate()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    let (image_url, image_server) = start_image_stub().await?;
    let failed_url = format!("{image_url}/poster-second");
    let usable_url = format!("{image_url}/poster");
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "Online Title",
            "images": { "POSTER": [failed_url, usable_url] }
        }),
    )
    .await?;
    let candidate_key =
        Sha256::digest(format!("TMDB\0POSTER\0{image_url}/poster-second").as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
    sqlx::query(
        "INSERT INTO metadata_image_attempts
         (item_id, image_type, candidate_key, status, attempt_count, next_retry_at)
         VALUES (?, 'POSTER', ?, 'FAILED', 1, 9223372036854775807)",
    )
    .bind(&fixture.item_id)
    .bind(candidate_key)
    .execute(fixture.database.pool())
    .await?;

    let selection = MetadataSelectionService::with_config_dir(
        fixture.database.clone(),
        ImageWriteService::new_with_config_dir(
            fixture.database.clone(),
            fixture.config.config_dir.clone(),
        )?,
        fixture.config.config_dir.clone(),
    );
    let report = selection
        .select(
            &fixture.item_id,
            &candidate_id,
            MetadataSelectionMode::FillMissing,
        )
        .await?;

    assert_eq!(report.image_types, vec!["POSTER"]);
    let poster =
        library_item_directory(&fixture.config.config_dir, &fixture.item_id)?.join("poster.png");
    assert_eq!(tokio::fs::read(poster).await?, PNG_1X1);

    image_server.abort();
    Ok(())
}

#[tokio::test]
async fn selection_records_the_scraper_that_confirmed_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({ "title": "Backup Match", "productionYear": 2020 }),
    )
    .await?;
    let selection = MetadataSelectionService::new(
        fixture.database.clone(),
        ImageWriteService::new(fixture.database.clone())?,
    );

    selection
        .select_with_scraper(
            &fixture.item_id,
            &candidate_id,
            MetadataSelectionMode::FillMissing,
            Some("org.lux.backup"),
            false,
        )
        .await?;

    let source: Option<String> =
        sqlx::query_scalar("SELECT metadata_scraper_id FROM media_items WHERE id = ?")
            .bind(&fixture.item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(source.as_deref(), Some("org.lux.backup"));
    Ok(())
}

#[tokio::test]
async fn supplemental_selection_preserves_existing_rich_nfo_and_fills_missing_lists()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(true).await?;
    tokio::fs::write(
        fixture.movie_dir.join("movie.nfo"),
        "<movie><title>本地标题</title><genre>本地类型</genre></movie>",
    )
    .await?;
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "Supplement Title",
            "genres": ["在线类型"],
            "studios": ["补充制作公司"]
        }),
    )
    .await?;
    let selection = MetadataSelectionService::new(
        fixture.database.clone(),
        ImageWriteService::new(fixture.database.clone())?,
    );

    selection
        .select_with_scraper(
            &fixture.item_id,
            &candidate_id,
            MetadataSelectionMode::FillMissing,
            Some("org.lux.supplement"),
            true,
        )
        .await?;

    let nfo = tokio::fs::read_to_string(fixture.movie_dir.join("movie.nfo")).await?;
    assert!(nfo.contains("<genre>本地类型</genre>"));
    assert!(!nfo.contains("<genre>在线类型</genre>"));
    assert!(nfo.contains("<studio>补充制作公司</studio>"));
    let source: Option<String> =
        sqlx::query_scalar("SELECT metadata_scraper_id FROM media_items WHERE id = ?")
            .bind(&fixture.item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(source, None);
    Ok(())
}

#[tokio::test]
async fn batch_confirmation_selects_the_highest_scored_pending_candidate()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    let low_candidate_id = Uuid::now_v7().to_string();
    let high_candidate_id = Uuid::now_v7().to_string();
    for (candidate_id, provider_id, score, title) in [
        (&low_candidate_id, "601", 72.0_f64, "低分候选"),
        (&high_candidate_id, "602", 88.0_f64, "高分候选"),
    ] {
        sqlx::query(
            "INSERT INTO metadata_candidates
             (id, item_id, provider, provider_id, candidate_json, score, status)
             VALUES (?, ?, 'TMDB', ?, ?, ?, 'PENDING')",
        )
        .bind(candidate_id)
        .bind(&fixture.item_id)
        .bind(provider_id)
        .bind(json!({ "title": title, "productionYear": 2020 }).to_string())
        .bind(score)
        .execute(fixture.database.pool())
        .await?;
    }
    let service = ImageWriteService::new(fixture.database.clone())?;
    let selection = MetadataSelectionService::new(fixture.database.clone(), service);

    let report = selection.confirm_best_pending(&fixture.item_id).await?;

    assert_eq!(report.candidate_id, high_candidate_id);
    assert_eq!(report.status, "ONLINE_CONFIRMED");
    let high_status: String =
        sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
            .bind(&high_candidate_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(high_status, "SELECTED");
    let low_status: String =
        sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
            .bind(&low_candidate_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(low_status, "REJECTED");

    Ok(())
}

#[tokio::test]
async fn admin_can_batch_confirm_pending_metadata_items() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = prepare_fixture(false).await?;
    let candidate_id = insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({ "title": "待确认候选", "productionYear": 2020 }),
    )
    .await?;
    let (base_url, lux_server) = start_lux(&fixture).await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = login(&client, &base_url).await?;

    let response = client
        .post(format!("{base_url}/api/v1/admin/metadata/confirm"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "itemIds": [fixture.item_id] }))
        .send()
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await?;
    assert_eq!(body["confirmedCount"], 1);
    assert_eq!(body["failedCount"], 0);
    assert_eq!(body["failedItemIds"], json!([]));
    let status: String = sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
        .bind(candidate_id)
        .fetch_one(fixture.database.pool())
        .await?;
    assert_eq!(status, "SELECTED");

    lux_server.abort();
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
                "profileUrl": "https://image.tmdb.org/t/p/w185/profile.jpg",
                "person": {
                    "biography": "演员甲的生平介绍",
                    "birthday": "1970-01-01",
                    "knownForDepartment": "Acting",
                    "placeOfBirth": "测试城市"
                }
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
    assert_eq!(people["schemaVersion"], 4);
    assert_eq!(people["actors"][0]["name"], "演员甲");
    assert_eq!(people["actors"][0]["provider"], "tmdb");
    let person_key = people["actors"][0]["personKey"]
        .as_str()
        .ok_or("missing canonical person key")?;
    let person_dir = lux_person_directory(
        &fixture.config.config_dir,
        people["actors"][0]["name"]
            .as_str()
            .ok_or("missing actor name")?,
        person_key,
    )?;
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
    assert_eq!(detail_body["actors"][0]["biography"], "演员甲的生平介绍");
    assert_eq!(detail_body["actors"][0]["birthday"], "1970-01-01");
    assert_eq!(detail_body["actors"][0]["knownForDepartment"], "Acting");
    assert_eq!(detail_body["actors"][0]["placeOfBirth"], "测试城市");
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
async fn image_failure_does_not_block_selection_or_nfo_writeback()
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
    assert_eq!(failed.status(), StatusCode::OK);
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
    let image_attempt: (String, Option<i64>) = sqlx::query_as(
        "SELECT status, next_retry_at
         FROM metadata_image_attempts
         WHERE item_id = ?",
    )
    .bind(&fixture.item_id)
    .fetch_one(fixture.database.pool())
    .await?;
    assert_eq!(image_attempt.0, "UNAVAILABLE");
    assert!(image_attempt.1.is_none());
    let nfo = tokio::fs::read_to_string(fixture.movie_dir.join("movie.nfo")).await?;
    assert!(nfo.contains("<title>Retry Title</title>"));
    let image_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item_images WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(fixture.database.pool())
        .await?;
    assert_eq!(image_count, 0);

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
            &ScraperProvider::from(tmdb),
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
async fn automatic_candidate_search_expands_only_the_best_result()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    let non_best_details_requests = Arc::new(AtomicUsize::new(0));
    let tmdb_app = Router::new()
        .fallback(any(
            |AxumState(non_best_details_requests): AxumState<Arc<AtomicUsize>>,
             request: Request<Body>| async move {
                let path = request.uri().path();
                if path == "/3/search/movie" {
                    return Json(json!({
                        "page": 1,
                        "total_pages": 1,
                        "total_results": 2,
                        "results": [
                            {
                                "id": 1,
                                "title": "Example Movie",
                                "original_title": "Example Movie",
                                "release_date": "2020-01-01"
                            },
                            {
                                "id": 2,
                                "title": "Unrelated Movie",
                                "original_title": "Unrelated Movie",
                                "release_date": "2020-01-01"
                            }
                        ]
                    }))
                    .into_response();
                }
                if path == "/3/movie/1" {
                    return Json(json!({
                        "id": 1,
                        "title": "Example Movie",
                        "original_title": "Example Movie",
                        "overview": "Best result",
                        "release_date": "2020-01-01",
                        "original_language": "en"
                    }))
                    .into_response();
                }
                if path == "/3/movie/2" {
                    non_best_details_requests.fetch_add(1, Ordering::SeqCst);
                }
                StatusCode::NOT_FOUND.into_response()
            },
        ))
        .with_state(non_best_details_requests.clone());
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
    let scraper = ScraperProvider::from(tmdb);

    let page = candidates
        .search_and_store_for_automatic_match(
            &fixture.item_id,
            "Example Movie",
            Some(2020),
            &scraper,
        )
        .await?;

    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].provider_id, "1");
    assert_eq!(page.items[1].provider_id, "2");
    assert_eq!(non_best_details_requests.load(Ordering::SeqCst), 0);

    let repeated_page = candidates
        .search_and_store_for_automatic_match(
            &fixture.item_id,
            "Example Movie",
            Some(2020),
            &scraper,
        )
        .await?;
    assert_eq!(repeated_page.items.len(), 2);
    let stored_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM metadata_candidates WHERE item_id = ? AND status = 'PENDING'",
    )
    .bind(&fixture.item_id)
    .fetch_one(fixture.database.pool())
    .await?;
    assert_eq!(stored_count, 2);

    tmdb_server.abort();
    Ok(())
}

#[tokio::test]
async fn automatic_matching_reuses_an_unexpired_pending_candidate_without_search()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "Pending Movie",
            "productionYear": 2020,
            "providerIds": {"Tmdb": "603"},
            "metadataFetched": true
        }),
    )
    .await?;
    let requests = Arc::new(AtomicUsize::new(0));
    let handler_requests = Arc::clone(&requests);
    let tmdb_app = Router::new().fallback(any(move |_request: Request<Body>| {
        let requests = Arc::clone(&handler_requests);
        async move {
            requests.fetch_add(1, Ordering::SeqCst);
            StatusCode::NOT_FOUND.into_response()
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let tmdb_server = tokio::spawn(async move { axum::serve(listener, tmdb_app).await });
    let scraper = ScraperProvider::from(TmdbClient::new(TmdbClientConfig {
        base_url: format!("http://{address}"),
        proxy_url: None,
        api_key: None,
        read_access_token: Some("stub-token".to_owned()),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?);

    let page = MetadataCandidateService::new(fixture.database.clone())
        .search_and_store_for_automatic_match(
            &fixture.item_id,
            "Example Movie",
            Some(2020),
            &scraper,
        )
        .await?;

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].provider_id, "603");
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    tmdb_server.abort();
    Ok(())
}

#[tokio::test]
async fn automatic_matching_expands_only_the_best_pending_candidate_once()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "Pending Movie",
            "productionYear": 2020,
            "providerIds": {"Tmdb": "603"}
        }),
    )
    .await?;
    let detail_requests = Arc::new(AtomicUsize::new(0));
    let handler_detail_requests = Arc::clone(&detail_requests);
    let tmdb_app = Router::new().fallback(any(move |request: Request<Body>| {
        let detail_requests = Arc::clone(&handler_detail_requests);
        async move {
            if request.uri().path() == "/3/movie/603" {
                detail_requests.fetch_add(1, Ordering::SeqCst);
                return Json(json!({
                    "id": 603,
                    "title": "Hydrated Movie",
                    "overview": "Hydrated overview",
                    "release_date": "2020-01-01",
                    "original_language": "en"
                }))
                .into_response();
            }
            StatusCode::NOT_FOUND.into_response()
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let tmdb_server = tokio::spawn(async move { axum::serve(listener, tmdb_app).await });
    let config = TmdbClientConfig {
        base_url: format!("http://{address}"),
        proxy_url: None,
        api_key: None,
        read_access_token: Some("stub-token".to_owned()),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    };
    let first_scraper = ScraperProvider::from(TmdbClient::new(config.clone())?);
    let candidates = MetadataCandidateService::new(fixture.database.clone());
    let first_page = candidates
        .search_and_store_for_automatic_match(
            &fixture.item_id,
            "Example Movie",
            Some(2020),
            &first_scraper,
        )
        .await?;
    assert_eq!(
        first_page.items[0].candidate["overview"],
        "Hydrated overview"
    );

    let second_scraper = ScraperProvider::from(TmdbClient::new(config)?);
    let second_page = candidates
        .search_and_store_for_automatic_match(
            &fixture.item_id,
            "Example Movie",
            Some(2020),
            &second_scraper,
        )
        .await?;
    assert_eq!(
        second_page.items[0].candidate["overview"],
        "Hydrated overview"
    );
    assert_eq!(detail_requests.load(Ordering::SeqCst), 1);
    tmdb_server.abort();
    Ok(())
}

#[tokio::test]
async fn expired_pending_candidates_are_not_reused_by_automatic_matching()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    insert_candidate(
        &fixture.database,
        &fixture.item_id,
        json!({
            "title": "Expired Movie",
            "productionYear": 2020,
            "providerIds": {"Tmdb": "603"},
            "metadataFetched": true
        }),
    )
    .await?;
    sqlx::query(
        "UPDATE metadata_candidates SET expires_at = unixepoch() - 1
         WHERE item_id = ?",
    )
    .bind(&fixture.item_id)
    .execute(fixture.database.pool())
    .await?;

    let searches = Arc::new(AtomicUsize::new(0));
    let handler_searches = Arc::clone(&searches);
    let tmdb_app = Router::new().fallback(any(move |request: Request<Body>| {
        let searches = Arc::clone(&handler_searches);
        async move {
            let path = request.uri().path();
            if path == "/3/search/movie" {
                searches.fetch_add(1, Ordering::SeqCst);
                return Json(json!({
                    "page": 1,
                    "total_pages": 1,
                    "total_results": 1,
                    "results": [{
                        "id": 604,
                        "title": "Fresh Movie",
                        "release_date": "2020-01-01",
                        "original_language": "en"
                    }]
                }))
                .into_response();
            }
            if path.ends_with("/images") {
                return Json(json!({"posters": [], "backdrops": []})).into_response();
            }
            if path.ends_with("/credits") {
                return Json(json!({"cast": [], "crew": []})).into_response();
            }
            if path.ends_with("/external_ids") {
                return Json(json!({})).into_response();
            }
            if path.ends_with("/videos") || path.ends_with("/release_dates") {
                return Json(json!({"results": []})).into_response();
            }
            Json(json!({
                "id": 604,
                "title": "Fresh Movie",
                "release_date": "2020-01-01",
                "original_language": "en"
            }))
            .into_response()
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let tmdb_server = tokio::spawn(async move { axum::serve(listener, tmdb_app).await });
    let scraper = ScraperProvider::from(TmdbClient::new(TmdbClientConfig {
        base_url: format!("http://{address}"),
        proxy_url: None,
        api_key: None,
        read_access_token: Some("stub-token".to_owned()),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?);

    let page = MetadataCandidateService::new(fixture.database.clone())
        .search_and_store_for_automatic_match(
            &fixture.item_id,
            "Example Movie",
            Some(2020),
            &scraper,
        )
        .await?;

    assert!(searches.load(Ordering::SeqCst) >= 1);
    assert!(page.items.iter().any(|item| item.provider_id == "604"));
    tmdb_server.abort();
    Ok(())
}

#[tokio::test]
async fn fill_missing_requests_the_missing_credits_capability_without_images()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    tokio::fs::write(
        fixture.movie_dir.join("movie.nfo"),
        "<movie><title>Example Movie</title><originaltitle>Example Movie</originaltitle><plot>Overview</plot><year>2020</year><rating>8.0</rating><premiered>2020-01-01</premiered><language>en</language><director tmdbid=\"director-1\">Director</director><writer tmdbid=\"writer-1\">Writer</writer><trailer>https://example.invalid/trailer</trailer></movie>",
    )
    .await?;
    let library_id: String = sqlx::query_scalar("SELECT library_id FROM media_items WHERE id = ?")
        .bind(&fixture.item_id)
        .fetch_one(fixture.database.pool())
        .await?;
    MetadataEnricher::new(fixture.database.clone())
        .enrich_movie_library(library_id.parse()?)
        .await?;
    let metadata_dir = library_item_directory(&fixture.config.config_dir, &fixture.item_id)?;
    tokio::fs::create_dir_all(&metadata_dir).await?;
    for image_type in ["poster", "fanart", "logo", "thumb"] {
        tokio::fs::write(metadata_dir.join(format!("{image_type}.png")), PNG_1X1).await?;
    }
    sqlx::query(
        "UPDATE libraries
         SET scraper_id = 'tmdb'
         WHERE id = (SELECT library_id FROM media_items WHERE id = ?)",
    )
    .bind(&fixture.item_id)
    .execute(fixture.database.pool())
    .await?;
    sqlx::query(
        "UPDATE media_items SET
            original_title = 'Example Movie', overview = 'Overview', production_year = 2020,
            premiere_date = '2020-01-01', original_language = 'en', rating = 8.0,
            provider_ids_json = ?, metadata_scraper_id = 'tmdb',
            metadata_provenance_json = ?, nfo_metadata_json = ?
         WHERE id = ?",
    )
    .bind(json!({"Tmdb": "1", "Imdb": "tt1"}).to_string())
    .bind(
        json!({
            "title": "LOCAL_NFO",
            "originalTitle": "LOCAL_NFO",
            "overview": "LOCAL_NFO",
            "productionYear": "LOCAL_NFO"
        })
        .to_string(),
    )
    .bind(
        json!({
            "rating": 8.0,
            "releaseDate": "2020-01-01",
            "originalLanguage": "en",
            "directors": [{"provider_id": "director-1", "name": "Director"}],
            "writers": [{"provider_id": "writer-1", "name": "Writer"}],
            "trailers": ["https://example.invalid/trailer"]
        })
        .to_string(),
    )
    .bind(&fixture.item_id)
    .execute(fixture.database.pool())
    .await?;

    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let handler_calls = Arc::clone(&calls);
    let tmdb_app = Router::new().fallback(any(move |request: Request<Body>| {
        let calls = Arc::clone(&handler_calls);
        async move {
            let path = request.uri().path().to_owned();
            calls
                .lock()
                .expect("request call list should not be poisoned")
                .push(path.clone());
            if path == "/3/movie/1/credits" {
                Json(json!({"cast": [], "crew": []})).into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }
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
    let images = ImageWriteService::new_with_config_dir(
        fixture.database.clone(),
        fixture.config.config_dir.clone(),
    )?;
    let selection = MetadataSelectionService::with_config_dir(
        fixture.database.clone(),
        images,
        fixture.config.config_dir.clone(),
    );
    let service = MetadataReidentifyService::with_selection(
        fixture.database.clone(),
        ScraperProvider::from(tmdb),
        Some(selection.clone()),
    );
    let job = service
        .create_item_refresh_job(&fixture.item_id, MetadataRefreshMode::FillMissing)
        .await?;
    service.run(&job.id).await;

    let first_calls = calls
        .lock()
        .expect("request call list should not be poisoned")
        .clone();
    assert_eq!(first_calls.len(), 1);
    assert!(first_calls.iter().any(|path| path == "/3/movie/1/credits"));
    let finished = service.get_job(&job.id).await?;
    assert_eq!(finished.status, "COMPLETED");
    tokio::fs::write(metadata_dir.join("poster.png"), PNG_1X1).await?;
    let second_job = service
        .create_item_refresh_job(&fixture.item_id, MetadataRefreshMode::FillMissing)
        .await?;
    service.run(&second_job.id).await;
    let calls_after_complete = calls
        .lock()
        .expect("request call list should not be poisoned")
        .clone();
    assert_eq!(calls_after_complete, vec!["/3/movie/1/credits"]);
    assert_eq!(service.get_job(&second_job.id).await?.status, "COMPLETED");

    let capability_status: String = sqlx::query_scalar(
        "SELECT status FROM metadata_capability_attempts
         WHERE item_id = ? AND capability = 'CREDITS'",
    )
    .bind(&fixture.item_id)
    .fetch_one(fixture.database.pool())
    .await?;
    assert_eq!(capability_status, "UNAVAILABLE");

    let fresh_tmdb = TmdbClient::new(TmdbClientConfig {
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
    let fresh_service = MetadataReidentifyService::with_selection(
        fixture.database.clone(),
        ScraperProvider::from(fresh_tmdb),
        Some(selection),
    );
    let third_job = fresh_service
        .create_item_refresh_job(&fixture.item_id, MetadataRefreshMode::FillMissing)
        .await?;
    fresh_service.run(&third_job.id).await;
    assert_eq!(
        calls
            .lock()
            .expect("request call list should not be poisoned")
            .as_slice(),
        ["/3/movie/1/credits"]
    );

    tmdb_server.abort();
    Ok(())
}

#[tokio::test]
async fn metadata_candidate_refresh_counts_all_capabilities_and_reuses_cache()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let handler_calls = Arc::clone(&calls);
    let tmdb_app = Router::new().fallback(any(move |request: Request<Body>| {
        let calls = Arc::clone(&handler_calls);
        async move {
            let path = request.uri().path().to_owned();
            calls
                .lock()
                .expect("request call list should not be poisoned")
                .push(path.clone());
            let body = match path.as_str() {
                "/3/search/movie" => json!({
                    "page": 1,
                    "total_pages": 1,
                    "total_results": 1,
                    "results": [{
                        "id": 7,
                        "title": "Example Movie",
                        "original_title": "Example Movie",
                        "overview": "Search overview",
                        "release_date": "2020-01-01",
                        "original_language": "en"
                    }]
                }),
                "/3/movie/7" => json!({
                    "id": 7,
                    "title": "Example Movie",
                    "original_title": "Example Movie",
                    "overview": "Movie overview",
                    "release_date": "2020-01-01",
                    "original_language": "en"
                }),
                "/3/movie/7/release_dates" => json!({"results": []}),
                "/3/movie/7/images" => json!({"posters": [], "backdrops": []}),
                "/3/movie/7/credits" => json!({"cast": [], "crew": []}),
                "/3/movie/7/external_ids" => json!({"imdb_id": "tt7"}),
                "/3/movie/7/videos" => json!({"results": []}),
                _ => return StatusCode::NOT_FOUND.into_response(),
            };
            Json(body).into_response()
        }
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, tmdb_app).await });
    let tmdb = TmdbClient::new(TmdbClientConfig {
        base_url: format!("http://{address}"),
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
    let scraper = ScraperProvider::from(tmdb);

    candidates
        .search_and_store(&fixture.item_id, "Example Movie", Some(2020), &scraper)
        .await?;
    let mut first_calls = calls
        .lock()
        .expect("request call list should not be poisoned")
        .clone();
    first_calls.sort();
    assert_eq!(
        first_calls,
        vec![
            "/3/movie/7".to_owned(),
            "/3/movie/7/credits".to_owned(),
            "/3/movie/7/external_ids".to_owned(),
            "/3/movie/7/images".to_owned(),
            "/3/movie/7/release_dates".to_owned(),
            "/3/movie/7/videos".to_owned(),
            "/3/search/movie".to_owned(),
        ]
    );

    candidates
        .search_and_store(&fixture.item_id, "Example Movie", Some(2020), &scraper)
        .await?;
    let second_calls = calls
        .lock()
        .expect("request call list should not be poisoned")
        .clone();
    assert_eq!(second_calls.len(), first_calls.len());

    server.abort();
    Ok(())
}

#[tokio::test]
async fn fill_missing_does_not_repeat_an_explicitly_empty_image_result()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = prepare_fixture(false).await?;
    let nfo_details = json!({
        "rating": 8.0,
        "releaseDate": "2020-01-01",
        "originalLanguage": "en",
        "directors": [{"provider_id": "director-1", "name": "Director"}],
        "writers": [{"provider_id": "writer-1", "name": "Writer"}],
        "trailers": ["https://example.invalid/trailer"]
    });
    PeopleService::new(fixture.config.config_dir.clone())
        .with_database(fixture.database.clone())
        .persist_item_actors(&fixture.item_id, "tmdb", &[])
        .await?;
    sqlx::query(
        "UPDATE libraries
         SET scraper_id = 'tmdb'
         WHERE id = (SELECT library_id FROM media_items WHERE id = ?)",
    )
    .bind(&fixture.item_id)
    .execute(fixture.database.pool())
    .await?;
    sqlx::query(
        "UPDATE media_items SET
            original_title = 'Example Movie', overview = 'Overview', production_year = 2020,
            premiere_date = '2020-01-01', original_language = 'en', rating = 8.0,
            provider_ids_json = ?, metadata_scraper_id = 'tmdb',
            metadata_provenance_json = ?, nfo_metadata_json = ?
         WHERE id = ?",
    )
    .bind(json!({"Tmdb": "1", "Imdb": "tt1"}).to_string())
    .bind(
        json!({
            "title": "LOCAL_NFO",
            "originalTitle": "LOCAL_NFO",
            "overview": "LOCAL_NFO",
            "productionYear": "LOCAL_NFO"
        })
        .to_string(),
    )
    .bind(nfo_details.to_string())
    .bind(&fixture.item_id)
    .execute(fixture.database.pool())
    .await?;

    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let handler_calls = Arc::clone(&calls);
    let tmdb_app = Router::new().fallback(any(move |request: Request<Body>| {
        let calls = Arc::clone(&handler_calls);
        async move {
            let path = request.uri().path().to_owned();
            calls
                .lock()
                .expect("request call list should not be poisoned")
                .push(path.clone());
            if path == "/3/movie/1/images" {
                Json(json!({"id": 1, "backdrops": [], "posters": []})).into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }
    }));
    let tmdb_listener = TcpListener::bind("127.0.0.1:0").await?;
    let tmdb_address = tmdb_listener.local_addr()?;
    let tmdb_server = tokio::spawn(async move { axum::serve(tmdb_listener, tmdb_app).await });
    let tmdb_config = TmdbClientConfig {
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
    };
    let selection = MetadataSelectionService::with_config_dir(
        fixture.database.clone(),
        ImageWriteService::new_with_config_dir(
            fixture.database.clone(),
            fixture.config.config_dir.clone(),
        )?,
        fixture.config.config_dir.clone(),
    );
    let selection_for_second_run = selection.clone();
    let service = MetadataReidentifyService::with_selection(
        fixture.database.clone(),
        ScraperProvider::from(TmdbClient::new(tmdb_config.clone())?),
        Some(selection),
    );
    let first_job = service
        .create_item_refresh_job(&fixture.item_id, MetadataRefreshMode::FillMissing)
        .await?;
    service.run(&first_job.id).await;
    assert_eq!(
        calls
            .lock()
            .expect("request call list should not be poisoned")
            .as_slice(),
        ["/3/movie/1/images"]
    );
    PeopleService::new(fixture.config.config_dir.clone())
        .with_database(fixture.database.clone())
        .persist_item_actors(&fixture.item_id, "tmdb", &[])
        .await?;
    sqlx::query("UPDATE media_items SET nfo_metadata_json = ? WHERE id = ?")
        .bind(nfo_details.to_string())
        .bind(&fixture.item_id)
        .execute(fixture.database.pool())
        .await?;

    let second_service = MetadataReidentifyService::with_selection(
        fixture.database.clone(),
        ScraperProvider::from(TmdbClient::new(tmdb_config)?),
        Some(selection_for_second_run),
    );
    let second_job = second_service
        .create_item_refresh_job(&fixture.item_id, MetadataRefreshMode::FillMissing)
        .await?;
    second_service.run(&second_job.id).await;
    assert_eq!(
        calls
            .lock()
            .expect("request call list should not be poisoned")
            .as_slice(),
        ["/3/movie/1/images"]
    );
    let unavailable_images: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM metadata_image_attempts
         WHERE item_id = ? AND status = 'UNAVAILABLE' AND error_code = 'NO_IMAGE'",
    )
    .bind(&fixture.item_id)
    .fetch_one(fixture.database.pool())
    .await?;
    assert!(unavailable_images >= 8);

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
    sqlx::query("UPDATE media_items SET provider_ids_json = ? WHERE id = ?")
        .bind(r#"{"Tmdb":"999"}"#)
        .bind(&fixture.item_id)
        .execute(fixture.database.pool())
        .await?;

    let tmdb_app = Router::new()
        .route(
            "/3/movie/999",
            get(|| async {
                Json(json!({
                    "id": 999,
                    "title": "Example Movie",
                    "original_title": "Example Movie",
                    "overview": "Automatically matched overview.",
                    "release_date": "2020-04-01",
                    "original_language": "en"
                }))
            }),
        )
        .route(
            "/3/movie/999/credits",
            get(|| async {
                Json(json!({
                    "cast": [{
                        "id": 10,
                        "name": "后台演员",
                        "character": "后台角色",
                        "order": 0
                    }]
                }))
            }),
        )
        .route(
            "/3/person/10",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                Json(json!({
                    "id": 10,
                    "name": "后台演员",
                    "biography": "后台补全的人物简介",
                    "birthday": "1970-01-01"
                }))
            }),
        )
        .fallback(any(|| async { (StatusCode::NOT_FOUND, Json(json!({}))) }));
    let tmdb_listener = TcpListener::bind("127.0.0.1:0").await?;
    let tmdb_address = tmdb_listener.local_addr()?;
    let tmdb_server = tokio::spawn(async move { axum::serve(tmdb_listener, tmdb_app).await });
    let tmdb = TmdbClient::new(TmdbClientConfig {
        base_url: format!("http://{tmdb_address}"),
        proxy_url: None,
        api_key: None,
        read_access_token: Some("stub-token".to_owned()),
        timeout: Duration::from_secs(3),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?;
    let selection = MetadataSelectionService::with_config_dir(
        fixture.database.clone(),
        ImageWriteService::new(fixture.database.clone())?,
        fixture.config.config_dir.clone(),
    );
    let metadata = MetadataReidentifyService::with_selection(
        fixture.database.clone(),
        ScraperProvider::from(tmdb),
        Some(selection),
    );
    let scan_jobs = luxd::application::scanner::ScanJobService::new(fixture.database.clone());
    let scan_job = scan_jobs
        .create_movie_scan_job_with_metadata(library_id.parse()?, true)
        .await?;
    let metadata_started = Instant::now();
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
    assert!(
        metadata_started.elapsed() < Duration::from_secs(1),
        "metadata job waited for optional actor enrichment"
    );
    let status: String =
        sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
            .bind(&fixture.item_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(status, "ONLINE_CONFIRMED");
    let nfo = tokio::fs::read_to_string(fixture.movie_dir.join("movie.nfo")).await?;
    assert!(nfo.contains("<plot>Automatically matched overview.</plot>"));
    let score: f64 = sqlx::query_scalar(
        "SELECT score FROM metadata_candidates WHERE item_id = ? AND status = 'SELECTED' LIMIT 1",
    )
    .bind(&fixture.item_id)
    .fetch_one(fixture.database.pool())
    .await?;
    assert_eq!(score, 100.0);
    let mode: String = sqlx::query_scalar(
        "SELECT mode FROM metadata_reidentify_jobs ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(fixture.database.pool())
    .await?;
    assert_eq!(mode, MetadataRefreshMode::FillMissing.as_str());

    let people_file =
        library_item_directory(&fixture.config.config_dir, &fixture.item_id)?.join("people.json");
    let people: Value = serde_json::from_slice(&tokio::fs::read(&people_file).await?)?;
    let person_key = people["actors"][0]["personKey"]
        .as_str()
        .ok_or("background person key missing")?;
    let person_nfo = lux_person_directory(
        &fixture.config.config_dir,
        people["actors"][0]["name"]
            .as_str()
            .ok_or("background actor name missing")?,
        person_key,
    )?
    .join("person.nfo");
    let enrichment_started = Instant::now();
    let mut enriched = false;
    while enrichment_started.elapsed() < Duration::from_secs(3) {
        if tokio::fs::read_to_string(&person_nfo)
            .await
            .is_ok_and(|contents| contents.contains("后台补全的人物简介"))
        {
            enriched = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(enriched, "background actor metadata was not persisted");

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
    let item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' LIMIT 1",
    )
    .bind(library.id.to_string())
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
