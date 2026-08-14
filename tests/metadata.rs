use luxd::{
    application::{
        libraries::LibraryService,
        metadata::{
            ImageType, LocalImage, MetadataCandidate, MetadataEnricher, MetadataField,
            MetadataSource, MetadataState, NfoMetadata, find_local_images, parse_nfo,
        },
        metadata_paths::people_directory,
        nfo::LocalNfoMetadataStore,
        people::PeopleService,
        scanner::LibraryScanner,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[test]
fn metadata_merge_table_preserves_local_and_locked_values() {
    struct Case {
        name: &'static str,
        initial: NfoMetadata,
        initial_source: Option<(MetadataField, MetadataSource)>,
        locked: Option<MetadataField>,
        candidate: MetadataCandidate,
        expected: NfoMetadata,
        expected_source_field: MetadataField,
        expected_source: MetadataSource,
    }

    let cases = [
        Case {
            name: "local nfo wins over tmdb",
            initial: NfoMetadata {
                title: Some("本地标题".to_owned()),
                ..NfoMetadata::default()
            },
            initial_source: Some((MetadataField::Title, MetadataSource::LocalNfo)),
            locked: None,
            candidate: MetadataCandidate {
                source: MetadataSource::TmdbLocalized,
                metadata: NfoMetadata {
                    title: Some("在线标题".to_owned()),
                    production_year: Some(2020),
                    ..NfoMetadata::default()
                },
            },
            expected: NfoMetadata {
                title: Some("本地标题".to_owned()),
                production_year: Some(2020),
                ..NfoMetadata::default()
            },
            expected_source_field: MetadataField::ProductionYear,
            expected_source: MetadataSource::TmdbLocalized,
        },
        Case {
            name: "locked local value cannot be refreshed",
            initial: NfoMetadata {
                overview: Some("手工简介".to_owned()),
                ..NfoMetadata::default()
            },
            initial_source: Some((MetadataField::Overview, MetadataSource::LocalNfo)),
            locked: Some(MetadataField::Overview),
            candidate: MetadataCandidate {
                source: MetadataSource::TmdbLocalized,
                metadata: NfoMetadata {
                    overview: Some("在线简介".to_owned()),
                    ..NfoMetadata::default()
                },
            },
            expected: NfoMetadata {
                overview: Some("手工简介".to_owned()),
                ..NfoMetadata::default()
            },
            expected_source_field: MetadataField::Overview,
            expected_source: MetadataSource::LockedLocal,
        },
        Case {
            name: "tmdb wins over fallback",
            initial: NfoMetadata {
                title: Some("文件名标题".to_owned()),
                ..NfoMetadata::default()
            },
            initial_source: Some((MetadataField::Title, MetadataSource::Fallback)),
            locked: None,
            candidate: MetadataCandidate {
                source: MetadataSource::TmdbLocalized,
                metadata: NfoMetadata {
                    title: Some("在线标题".to_owned()),
                    ..NfoMetadata::default()
                },
            },
            expected: NfoMetadata {
                title: Some("在线标题".to_owned()),
                ..NfoMetadata::default()
            },
            expected_source_field: MetadataField::Title,
            expected_source: MetadataSource::TmdbLocalized,
        },
        Case {
            name: "blank online value does not erase local value",
            initial: NfoMetadata {
                overview: Some("有效简介".to_owned()),
                ..NfoMetadata::default()
            },
            initial_source: Some((MetadataField::Overview, MetadataSource::LocalNfo)),
            locked: None,
            candidate: MetadataCandidate {
                source: MetadataSource::TmdbLocalized,
                metadata: NfoMetadata {
                    overview: Some("   ".to_owned()),
                    ..NfoMetadata::default()
                },
            },
            expected: NfoMetadata {
                overview: Some("有效简介".to_owned()),
                ..NfoMetadata::default()
            },
            expected_source_field: MetadataField::Overview,
            expected_source: MetadataSource::LocalNfo,
        },
    ];

    for case in cases {
        let mut state = MetadataState::from_metadata(case.initial);
        if let Some((field, source)) = case.initial_source {
            state.provenance.insert(field, source);
        }
        if let Some(field) = case.locked {
            state.lock(field);
        }
        state.apply_automatic(&case.candidate);
        assert_eq!(state.metadata, case.expected, "{}", case.name);
        assert_eq!(
            state.provenance.get(&case.expected_source_field).copied(),
            Some(case.expected_source),
            "{}",
            case.name
        );
    }
}

#[test]
fn metadata_state_round_trip_keeps_provenance_and_locks() {
    let mut state = MetadataState::from_metadata(NfoMetadata {
        title: Some("锁定标题".to_owned()),
        ..NfoMetadata::default()
    });
    state
        .provenance
        .insert(MetadataField::Title, MetadataSource::LocalNfo);
    state.lock(MetadataField::Title);

    let provenance_json = state.provenance_json();
    let locked_fields_json = state.locked_fields_json();
    let restored = MetadataState::from_persisted(
        state.metadata.clone(),
        Some(&provenance_json),
        Some(&locked_fields_json),
    );
    let mut refreshed = restored;
    refreshed.apply_automatic(&MetadataCandidate {
        source: MetadataSource::TmdbLocalized,
        metadata: NfoMetadata {
            title: Some("在线标题".to_owned()),
            ..NfoMetadata::default()
        },
    });
    assert_eq!(refreshed.metadata.title.as_deref(), Some("锁定标题"));
    assert_eq!(
        refreshed.provenance.get(&MetadataField::Title),
        Some(&MetadataSource::LockedLocal)
    );
}

#[test]
fn explicit_selection_modes_fill_or_refresh_only_unlocked_fields() {
    let candidate = MetadataCandidate {
        source: MetadataSource::TmdbLocalized,
        metadata: NfoMetadata {
            title: Some("在线标题".to_owned()),
            overview: Some("在线简介".to_owned()),
            production_year: Some(2025),
            ..NfoMetadata::default()
        },
    };
    let mut fill = MetadataState::from_metadata(NfoMetadata {
        title: Some("本地标题".to_owned()),
        production_year: Some(2020),
        ..NfoMetadata::default()
    });
    fill.provenance
        .insert(MetadataField::Title, MetadataSource::LocalNfo);
    fill.provenance
        .insert(MetadataField::ProductionYear, MetadataSource::LocalNfo);
    fill.apply_fill_missing(&candidate);
    assert_eq!(fill.metadata.title.as_deref(), Some("本地标题"));
    assert_eq!(fill.metadata.overview.as_deref(), Some("在线简介"));
    assert_eq!(fill.metadata.production_year, Some(2020));

    let mut refresh = fill.clone();
    refresh.apply_refresh_unlocked(&candidate);
    assert_eq!(refresh.metadata.title.as_deref(), Some("在线标题"));
    assert_eq!(refresh.metadata.overview.as_deref(), Some("在线简介"));
    assert_eq!(refresh.metadata.production_year, Some(2025));
    refresh.lock(MetadataField::Title);
    refresh.apply_refresh_unlocked(&MetadataCandidate {
        source: MetadataSource::TmdbLocalized,
        metadata: NfoMetadata {
            title: Some("再次在线标题".to_owned()),
            ..NfoMetadata::default()
        },
    });
    assert_eq!(refresh.metadata.title.as_deref(), Some("在线标题"));
}

#[test]
fn fill_missing_completeness_rejects_fallback_values() {
    let fields = [
        MetadataField::Title,
        MetadataField::OriginalTitle,
        MetadataField::Overview,
        MetadataField::ProductionYear,
    ];
    let mut state = MetadataState::from_metadata(NfoMetadata {
        title: Some("标题".to_owned()),
        original_title: Some("Original".to_owned()),
        overview: Some("简介".to_owned()),
        production_year: Some(2024),
    });
    assert!(!state.has_complete_fill_values(&fields));

    for field in fields {
        state.provenance.insert(field, MetadataSource::LocalNfo);
    }
    assert!(state.has_complete_fill_values(&fields));
}

#[test]
fn fill_missing_replaces_scanner_fallback_with_online_episode_title() {
    let mut state = MetadataState::from_metadata(NfoMetadata {
        title: Some("暗夜与黎明".to_owned()),
        ..NfoMetadata::default()
    });

    state.apply_fill_missing(&MetadataCandidate {
        source: MetadataSource::ScraperLocalized,
        metadata: NfoMetadata {
            title: Some("第一集：暗夜与黎明".to_owned()),
            ..NfoMetadata::default()
        },
    });

    assert_eq!(state.metadata.title.as_deref(), Some("第一集：暗夜与黎明"));
    assert_eq!(
        state.provenance.get(&MetadataField::Title),
        Some(&MetadataSource::ScraperLocalized)
    );
}

#[test]
fn nfo_parser_reads_local_fields_and_ignores_unknown_fields() {
    let metadata = parse_nfo(
        r#"<movie><title>本地标题</title><originaltitle>Original</originaltitle><year>2021</year><plot>简介</plot><unknown>忽略</unknown></movie>"#.as_bytes(),
    )
    .expect("valid nfo");

    assert_eq!(
        metadata,
        NfoMetadata {
            title: Some("本地标题".to_owned()),
            original_title: Some("Original".to_owned()),
            production_year: Some(2021),
            overview: Some("简介".to_owned()),
        }
    );
}

#[test]
fn malformed_nfo_is_rejected() {
    assert!(parse_nfo(b"<movie><title>broken").is_err());
}

#[test]
fn partial_and_empty_nfo_are_accepted() {
    let partial = parse_nfo(b"<movie><title>Only Title</title></movie>").expect("partial nfo");
    assert_eq!(partial.title.as_deref(), Some("Only Title"));
    assert_eq!(partial.production_year, None);
    assert_eq!(
        parse_nfo(b"<movie/>").expect("empty nfo"),
        NfoMetadata::default()
    );
}

#[test]
fn image_discovery_returns_supported_local_image_types() {
    let paths = [
        "/media/poster.jpg",
        "/media/fanart.png",
        "/media/clearlogo.png",
        "/media/poster.txt",
        "/media/thumb.jpg",
    ];

    let images = find_local_images(paths.iter().map(std::path::Path::new));

    assert_eq!(
        images,
        vec![
            LocalImage {
                image_type: ImageType::Poster,
                path: std::path::PathBuf::from("/media/poster.jpg"),
            },
            LocalImage {
                image_type: ImageType::Fanart,
                path: std::path::PathBuf::from("/media/fanart.png"),
            },
            LocalImage {
                image_type: ImageType::Logo,
                path: std::path::PathBuf::from("/media/clearlogo.png"),
            },
            LocalImage {
                image_type: ImageType::Thumb,
                path: std::path::PathBuf::from("/media/thumb.jpg"),
            },
        ]
    );
}

#[tokio::test]
async fn metadata_enrichment_updates_items_and_keeps_bad_nfo_non_blocking()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let good_dir = root.join("Good Movie (2020)");
    let bad_dir = root.join("Broken Movie (2021)");
    tokio::fs::create_dir_all(&good_dir).await?;
    tokio::fs::create_dir_all(&bad_dir).await?;
    tokio::fs::write(good_dir.join("Good.Movie.2020.mkv"), b"movie").await?;
    tokio::fs::write(
        good_dir.join("movie.nfo"),
        r#"<movie><title>本地电影</title><originaltitle>Local Movie</originaltitle><year>2021</year><plot>本地简介</plot></movie>"#,
    )
    .await?;
    tokio::fs::write(good_dir.join("poster.jpg"), b"poster").await?;
    tokio::fs::write(good_dir.join("fanart.png"), b"fanart").await?;
    tokio::fs::write(bad_dir.join("Broken.Movie.2021.mkv"), b"movie").await?;
    tokio::fs::write(bad_dir.join("Broken.Movie.2021.nfo"), b"<movie><title>").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let report = MetadataEnricher::new(database.clone())
        .enrich_movie_library(library.id)
        .await?;
    assert_eq!(report.nfo_loaded, 1);
    assert_eq!(report.nfo_failed, 1);
    assert_eq!(report.images_found, 2);

    let item: (String, String, String, i64, String) = sqlx::query_as(
        "SELECT title, sort_title, original_title, production_year, overview
         FROM media_items WHERE title = '本地电影'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        item,
        (
            "本地电影".to_owned(),
            "本地电影".to_owned(),
            "Local Movie".to_owned(),
            2021,
            "本地简介".to_owned()
        )
    );
    let provenance: String = sqlx::query_scalar(
        "SELECT metadata_provenance_json FROM media_items WHERE title = '本地电影'",
    )
    .fetch_one(database.pool())
    .await?;
    let provenance: serde_json::Value = serde_json::from_str(&provenance)?;
    assert_eq!(provenance["title"], "LOCAL_NFO");
    assert_eq!(provenance["originalTitle"], "LOCAL_NFO");
    assert_eq!(provenance["overview"], "LOCAL_NFO");
    assert_eq!(provenance["productionYear"], "LOCAL_NFO");
    let locked_fields: String =
        sqlx::query_scalar("SELECT locked_fields_json FROM media_items WHERE title = '本地电影'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&locked_fields)?,
        serde_json::json!([])
    );
    let image_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item_images")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(image_count, 2);
    let image_metadata: (i64, String) =
        sqlx::query_as("SELECT file_size, source FROM item_images ORDER BY image_type LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(image_metadata, (6, "LOCAL".to_owned()));

    let second_report = MetadataEnricher::new(database.clone())
        .enrich_movie_library(library.id)
        .await?;
    assert_eq!(second_report.nfo_loaded, 0);
    assert_eq!(second_report.nfo_failed, 0);
    assert_eq!(second_report.nfo_skipped, 2);
    assert_eq!(second_report.images_found, 0);
    let image_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item_images")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(image_count, 2);
    Ok(())
}

#[tokio::test]
async fn local_movie_nfo_actors_are_available_without_online_matching_and_reuse_people_image()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Local Movie (2026)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Local.Movie.2026.mkv"), b"movie").await?;
    tokio::fs::write(
        movie_dir.join("movie.nfo"),
        r#"<movie><title>本地电影</title><actor><name>演员甲</name><role>角色甲</role><type>Actor</type><tmdbid>9</tmdbid><order>0</order></actor><actor><name>演员乙</name><role>角色乙</role><type>Actor</type><tmdbid>10</tmdbid><order>1</order></actor><actor><name>本地演员</name><role>本地角色</role><order>2</order></actor></movie>"#,
    )
    .await?;

    let people_dir = people_directory(&config.config_dir, "演员甲", "tmdb", "9")?;
    tokio::fs::create_dir_all(&people_dir).await?;
    tokio::fs::write(people_dir.join("folder.png"), b"person-image").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let people = PeopleService::new(config.config_dir.clone());
    MetadataEnricher::new(database.clone())
        .with_people(people.clone())
        .enrich_movie_library(library.id)
        .await?;

    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let actors = people.list_item_actors(&item_id).await?;
    assert_eq!(actors.len(), 3);
    assert_eq!(actors[0].name, "演员甲");
    assert_eq!(
        actors[0].image_url.as_deref(),
        Some("/api/v1/people/9/image")
    );
    assert_eq!(actors[1].name, "演员乙");
    assert_eq!(actors[1].image_url, None);
    assert_eq!(actors[2].name, "本地演员");
    assert_eq!(actors[2].character.as_deref(), Some("本地角色"));
    assert_eq!(actors[2].image_url, None);
    Ok(())
}

#[tokio::test]
async fn local_movie_nfo_rich_details_are_cached_during_background_enrichment()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Rich Movie (2026)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Rich.Movie.2026.mkv"), b"movie").await?;
    tokio::fs::write(
        movie_dir.join("movie.nfo"),
        r#"<movie><title>本地丰富电影</title><rating>8.1</rating><votes>123</votes><tagline>大漠路远</tagline><premiered>2026-02-17</premiered><releasedate>2026-02-20</releasedate><runtime>126</runtime><status>Released</status><language>zh</language><mpaa>PG-13</mpaa><country>中国</country><genre>动作</genre><studio>示例影业</studio><tmdbid>1462229</tmdbid><director tmdbid="18899">导演甲</director><writer tmdbid="19999">编剧甲</writer><trailer>https://example.com/trailer</trailer></movie>"#,
    )
    .await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let store = LocalNfoMetadataStore::new(database.clone());
    let report = MetadataEnricher::new(database.clone())
        .with_nfo_store(store.clone())
        .enrich_movie_library(library.id)
        .await?;
    assert_eq!(report.nfo_loaded, 1);

    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let details = store
        .read_item(&item_id)
        .await?
        .ok_or("rich NFO cache missing")?;
    assert_eq!(details.rating, Some(8.1));
    assert_eq!(details.genres, vec!["动作"]);
    assert_eq!(details.directors[0].name, "导演甲");
    assert_eq!(details.trailers, vec!["https://example.com/trailer"]);
    let stored_json: Option<String> = sqlx::query_scalar(
        "SELECT nfo_metadata_json FROM media_items WHERE item_type = 'MOVIE' LIMIT 1",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(stored_json.is_some());
    assert!(
        stored_json
            .as_deref()
            .is_some_and(|value| value.contains("示例影业"))
    );
    assert!(!config.config_dir.join("metadata").join("library").exists());
    Ok(())
}

#[tokio::test]
async fn unchanged_nfo_content_keeps_the_rich_snapshot_after_file_revision_changes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Stable Movie (2026)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Stable.Movie.2026.mkv"), b"movie").await?;
    let nfo = r#"<movie><title>稳定电影</title><rating>8.2</rating><genre>剧情</genre></movie>"#;
    let nfo_path = movie_dir.join("movie.nfo");
    tokio::fs::write(&nfo_path, nfo.as_bytes()).await?;

    let database = Database::connect(&config).await?;
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
    let store = LocalNfoMetadataStore::new(database.clone());
    let enricher = MetadataEnricher::new(database.clone()).with_nfo_store(store);
    enricher.enrich_movie_library(library.id).await?;

    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let before: (Option<String>, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT nfo_metadata_json, nfo_metadata_fingerprint
         FROM media_items WHERE id = ?",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    tokio::fs::write(&nfo_path, nfo.as_bytes()).await?;
    let second = enricher.enrich_movie_library(library.id).await?;
    assert_eq!(second.nfo_loaded, 1);

    let after: (Option<String>, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT nfo_metadata_json, nfo_metadata_fingerprint
         FROM media_items WHERE id = ?",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(after, before);
    Ok(())
}

#[tokio::test]
async fn actor_relation_failure_does_not_discard_nfo_and_is_retried_separately()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Actor Retry Movie (2026)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Actor.Retry.Movie.2026.mkv"), b"movie").await?;
    tokio::fs::write(
        movie_dir.join("movie.nfo"),
        r#"<movie><title>演员重试电影</title><rating>7.5</rating><actor><name>演员甲</name><role>角色甲</role><tmdbid>9</tmdbid></actor></movie>"#,
    )
    .await?;

    let database = Database::connect(&config).await?;
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

    let metadata_root = config.config_dir.join("metadata");
    tokio::fs::create_dir_all(&metadata_root).await?;
    let blocked_library_dir = metadata_root.join("library");
    tokio::fs::write(&blocked_library_dir, b"temporarily blocked").await?;

    let people = PeopleService::new(config.config_dir.clone());
    let enricher = MetadataEnricher::new(database.clone())
        .with_people(people.clone())
        .with_nfo_store(LocalNfoMetadataStore::new(database.clone()));
    let first = enricher.enrich_movie_library(library.id).await?;
    assert_eq!(first.nfo_loaded, 1);
    assert_eq!(first.nfo_failed, 0);
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let rich_json: Option<String> =
        sqlx::query_scalar("SELECT nfo_metadata_json FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert!(rich_json.is_some());
    let metadata_fingerprint: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT metadata_fingerprint FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert!(metadata_fingerprint.is_some());

    tokio::fs::remove_file(&blocked_library_dir).await?;
    let second = enricher.enrich_movie_library(library.id).await?;
    assert_eq!(second.nfo_loaded, 1);
    let actors = people.list_item_actors(&item_id).await?;
    assert_eq!(actors.len(), 1);
    assert_eq!(actors[0].name, "演员甲");
    Ok(())
}

#[tokio::test]
async fn metadata_enrichment_skips_conflicting_nfo_and_indexes_following_images()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let existing_dir = root.join("Target Movie (1987)");
    let conflicting_dir = root.join("Target Movie (2018)");
    let following_dir = root.join("Z Later Movie (2020)");
    for directory in [&existing_dir, &conflicting_dir, &following_dir] {
        tokio::fs::create_dir_all(directory).await?;
    }
    tokio::fs::write(
        existing_dir.join("Target Movie (1987).mkv"),
        b"existing movie",
    )
    .await?;
    tokio::fs::write(
        conflicting_dir.join("Target Movie (2018).mkv"),
        b"conflicting movie",
    )
    .await?;
    tokio::fs::write(
        conflicting_dir.join("Target Movie (2018).nfo"),
        "<movie><title>Target Movie</title><year>1987</year></movie>",
    )
    .await?;
    tokio::fs::write(conflicting_dir.join("poster.jpg"), b"conflicting poster").await?;
    tokio::fs::write(
        following_dir.join("Z Later Movie (2020).mkv"),
        b"following movie",
    )
    .await?;
    tokio::fs::write(following_dir.join("poster.jpg"), b"following poster").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let report = MetadataEnricher::new(database.clone())
        .enrich_movie_library(library.id)
        .await?;

    assert_eq!(report.nfo_loaded, 0);
    assert_eq!(report.nfo_failed, 1);
    assert_eq!(report.images_found, 2);
    let image_paths: Vec<String> =
        sqlx::query_scalar("SELECT local_path FROM item_images ORDER BY local_path")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(image_paths.len(), 2);
    assert!(
        image_paths
            .iter()
            .any(|path| path.ends_with("Target Movie (2018)/poster.jpg"))
    );
    assert!(
        image_paths
            .iter()
            .any(|path| path.ends_with("Z Later Movie (2020)/poster.jpg"))
    );
    Ok(())
}
