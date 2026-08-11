use luxd::{
    application::{
        libraries::LibraryService,
        metadata::NfoMetadata,
        nfo::{
            MovieNfoCredit, MovieNfoMetadata, NfoWriteService, parse_movie_nfo_actors,
            rewrite_movie_nfo, rewrite_nfo, write_nfo_atomically,
        },
        people::ActorCredit,
        scanner::LibraryScanner,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[test]
fn nfo_rewrite_updates_common_fields_and_preserves_unknown_xml()
-> Result<(), Box<dyn std::error::Error>> {
    let original = r#"<?xml version="1.0" encoding="UTF-8"?>
<movie><title>旧标题</title><year>2020</year><custom><keep>保留</keep></custom></movie>"#;
    let rewritten = rewrite_nfo(
        original.as_bytes(),
        &NfoMetadata {
            title: Some("新标题".to_owned()),
            overview: Some("简介".to_owned()),
            production_year: Some(2024),
            ..NfoMetadata::default()
        },
    )?;
    let text = String::from_utf8(rewritten)?;
    assert!(text.contains("<title>新标题</title>"));
    assert!(text.contains("<year>2024</year>"));
    assert!(text.contains("<plot>简介</plot>"));
    assert!(text.contains("<custom><keep>保留</keep></custom>"));
    assert!(!text.contains("旧标题"));
    Ok(())
}

#[test]
fn nfo_rewrite_creates_a_movie_document_when_target_is_missing() {
    let rewritten = rewrite_nfo(
        &[],
        &NfoMetadata {
            title: Some("新电影".to_owned()),
            ..NfoMetadata::default()
        },
    )
    .expect("new movie nfo");
    let text = String::from_utf8(rewritten).expect("utf8 nfo");
    assert!(text.starts_with("<movie>"));
    assert!(text.contains("<title>新电影</title>"));
}

#[test]
fn movie_nfo_rewrite_writes_rich_fields_and_preserves_unknown_xml()
-> Result<(), Box<dyn std::error::Error>> {
    let original = r#"<movie><title>旧标题</title><rating>1</rating><genre>旧类型</genre><actor><name>旧演员</name></actor><custom><keep>保留</keep></custom></movie>"#;
    let rewritten = rewrite_movie_nfo(
        original.as_bytes(),
        &MovieNfoMetadata {
            base: NfoMetadata {
                title: Some("新标题".to_owned()),
                overview: Some("简介".to_owned()),
                production_year: Some(2026),
                ..NfoMetadata::default()
            },
            rating: Some(7.167),
            votes: Some(42),
            tagline: Some("速度与信念".to_owned()),
            premiered: Some("2026-02-17".to_owned()),
            releasedate: Some("2026-02-17".to_owned()),
            runtime: Some(126),
            status: Some("Released".to_owned()),
            original_language: Some("zh".to_owned()),
            website: Some("https://example.com/movie".to_owned()),
            set_name: Some("飞驰人生".to_owned()),
            set_id: Some("1281825".to_owned()),
            poster_url: Some("https://image.tmdb.org/t/p/original/poster.jpg".to_owned()),
            fanart_url: Some("https://image.tmdb.org/t/p/original/backdrop.jpg".to_owned()),
            certification: Some("PG-13".to_owned()),
            countries: vec!["中国".to_owned()],
            genres: vec!["剧情".to_owned(), "喜剧".to_owned()],
            studios: vec!["中国电影股份有限公司".to_owned()],
            provider_ids: [
                ("Tmdb".to_owned(), "1462229".to_owned()),
                ("Imdb".to_owned(), "tt38035835".to_owned()),
            ]
            .into_iter()
            .collect(),
            directors: vec![MovieNfoCredit {
                provider_id: "18899".to_owned(),
                name: "韩寒".to_owned(),
            }],
            writers: vec![MovieNfoCredit {
                provider_id: "18899".to_owned(),
                name: "韩寒".to_owned(),
            }],
            actors: vec![ActorCredit {
                id: "124".to_owned(),
                name: "沈腾".to_owned(),
                character: Some("张驰".to_owned()),
                order: Some(0),
                profile_url: None,
            }],
            trailers: vec!["https://www.youtube.com/watch?v=test".to_owned()],
        },
    )?;
    let text = String::from_utf8(rewritten)?;
    assert!(text.contains("<title>新标题</title>"));
    assert!(text.contains("<plot>简介</plot>"));
    assert!(text.contains("<rating>7.167</rating>"));
    assert!(text.contains("<votes>42</votes>"));
    assert!(text.contains("<tagline>速度与信念</tagline>"));
    assert!(text.contains("<premiered>2026-02-17</premiered>"));
    assert!(text.contains("<releasedate>2026-02-17</releasedate>"));
    assert!(text.contains("<runtime>126</runtime>"));
    assert!(text.contains("<status>Released</status>"));
    assert!(text.contains("<language>zh</language>"));
    assert!(text.contains("<website>https://example.com/movie</website>"));
    assert!(text.contains("<set>飞驰人生</set>"));
    assert!(text.contains("<setid>1281825</setid>"));
    assert!(text.contains(
        "<thumb aspect=\"poster\">https://image.tmdb.org/t/p/original/poster.jpg</thumb>"
    ));
    assert!(text.contains(
        "<fanart><thumb>https://image.tmdb.org/t/p/original/backdrop.jpg</thumb></fanart>"
    ));
    assert!(text.contains("<mpaa>PG-13</mpaa>"));
    assert!(text.contains("<country>中国</country>"));
    assert!(text.contains("<genre>剧情</genre>"));
    assert!(text.contains("<genre>喜剧</genre>"));
    assert!(text.contains("<studio>中国电影股份有限公司</studio>"));
    assert!(text.contains("<uniqueid type=\"tmdb\" default=\"true\">1462229</uniqueid>"));
    assert!(text.contains("<uniqueid type=\"imdb\">tt38035835</uniqueid>"));
    assert!(text.contains("<director tmdbid=\"18899\">韩寒</director>"));
    assert!(text.contains("<writer tmdbid=\"18899\">韩寒</writer>"));
    assert!(text.contains("<credits tmdbid=\"18899\">韩寒</credits>"));
    assert!(text.contains("<name>沈腾</name>"));
    assert!(text.contains("<role>张驰</role>"));
    assert!(text.contains("<tmdbid>124</tmdbid>"));
    assert!(text.contains("<trailer>https://www.youtube.com/watch?v=test</trailer>"));
    assert!(text.contains("<custom><keep>保留</keep></custom>"));
    assert!(!text.contains("<rating>1</rating>"));
    assert!(!text.contains("<genre>旧类型</genre>"));
    assert!(!text.contains("<name>旧演员</name>"));
    Ok(())
}

#[test]
fn movie_nfo_rewrite_keeps_existing_rich_fields_when_patch_is_partial()
-> Result<(), Box<dyn std::error::Error>> {
    let original = r#"<movie><title>旧标题</title><rating>8</rating><genre>旧类型</genre><actor><name>旧演员</name></actor></movie>"#;
    let rewritten = rewrite_movie_nfo(
        original.as_bytes(),
        &MovieNfoMetadata {
            base: NfoMetadata {
                title: Some("新标题".to_owned()),
                ..NfoMetadata::default()
            },
            ..MovieNfoMetadata::default()
        },
    )?;
    let text = String::from_utf8(rewritten)?;
    assert!(text.contains("<title>新标题</title>"));
    assert!(text.contains("<rating>8</rating>"));
    assert!(text.contains("<genre>旧类型</genre>"));
    assert!(text.contains("<name>旧演员</name>"));
    Ok(())
}

#[test]
fn movie_nfo_parser_reads_emby_actor_nodes_without_online_metadata() {
    let actors = parse_movie_nfo_actors(
        r#"<movie><title>本地电影</title><actor><name>演员甲</name><role>角色甲</role><type>Actor</type><tmdbid>9</tmdbid><order>0</order></actor><actor><name>演员乙</name><role>角色乙</role><type>Actor</type><tmdbid>10</tmdbid><order>1</order></actor></movie>"#
            .as_bytes(),
    )
    .expect("valid actor nodes");

    assert_eq!(actors.len(), 2);
    assert_eq!(actors[0].id, "9");
    assert_eq!(actors[0].name, "演员甲");
    assert_eq!(actors[0].character.as_deref(), Some("角色甲"));
    assert_eq!(actors[1].order, Some(1));
}

#[tokio::test]
async fn atomic_writer_replaces_target_without_leaving_a_temp_file()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let target = temp_dir.path().join("movie.nfo");
    tokio::fs::write(&target, b"<movie><title>old</title></movie>").await?;

    write_nfo_atomically(
        &target,
        &NfoMetadata {
            title: Some("new".to_owned()),
            ..NfoMetadata::default()
        },
    )
    .await?;

    assert!(String::from_utf8(tokio::fs::read(&target).await?)?.contains("<title>new</title>"));
    let mut entries = tokio::fs::read_dir(temp_dir.path()).await?;
    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        names.push(entry.file_name());
    }
    assert_eq!(names, vec![std::ffi::OsString::from("movie.nfo")]);
    Ok(())
}

#[tokio::test]
async fn nfo_service_checks_library_root_and_refreshes_metadata_fingerprint()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Example Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2020.mkv"), b"fixture").await?;
    tokio::fs::write(
        movie_dir.join("movie.nfo"),
        "<movie><custom>keep</custom><title>old</title></movie>",
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
    let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
        .fetch_one(database.pool())
        .await?;

    let report = NfoWriteService::new(database.clone())
        .write_item_nfo(
            &item_id,
            &NfoMetadata {
                title: Some("new".to_owned()),
                overview: Some("overview".to_owned()),
                ..NfoMetadata::default()
            },
        )
        .await?;
    let output = tokio::fs::read_to_string(&report.path).await?;
    assert!(output.contains("<custom>keep</custom>"));
    assert!(output.contains("<title>new</title>"));
    let fingerprint: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT metadata_fingerprint FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(fingerprint, Some(report.fingerprint));
    Ok(())
}

#[tokio::test]
async fn nfo_service_writes_next_to_strm_source() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Example STRM Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(
        movie_dir.join("Example.STRM.Movie.2020.strm"),
        "https://example.invalid/movie",
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
    let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
        .fetch_one(database.pool())
        .await?;

    let report = NfoWriteService::new(database)
        .write_item_nfo(
            &item_id,
            &NfoMetadata {
                title: Some("已识别 STRM 电影".to_owned()),
                ..NfoMetadata::default()
            },
        )
        .await?;

    let canonical_movie_dir = tokio::fs::canonicalize(&movie_dir).await?;
    assert_eq!(report.path, canonical_movie_dir.join("movie.nfo"));
    let output = tokio::fs::read_to_string(&report.path).await?;
    assert!(output.contains("<title>已识别 STRM 电影</title>"));
    Ok(())
}

#[tokio::test]
async fn malformed_original_is_not_replaced() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let target = temp_dir.path().join("movie.nfo");
    let original = b"<movie><title>broken";
    tokio::fs::write(&target, original).await?;

    let result = write_nfo_atomically(
        &target,
        &NfoMetadata {
            title: Some("new".to_owned()),
            ..NfoMetadata::default()
        },
    )
    .await;
    assert!(result.is_err());
    assert_eq!(tokio::fs::read(&target).await?, original);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn read_only_directory_rejects_nfo_write() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::tempdir()?;
    let directory = temp_dir.path().join("ReadOnly");
    tokio::fs::create_dir(&directory).await?;
    let mut permissions = tokio::fs::metadata(&directory).await?.permissions();
    permissions.set_mode(0o555);
    tokio::fs::set_permissions(&directory, permissions).await?;
    let result = write_nfo_atomically(
        &directory.join("movie.nfo"),
        &NfoMetadata {
            title: Some("new".to_owned()),
            ..NfoMetadata::default()
        },
    )
    .await;
    let mut restore = tokio::fs::metadata(&directory).await?.permissions();
    restore.set_mode(0o755);
    tokio::fs::set_permissions(&directory, restore).await?;
    assert!(matches!(
        result,
        Err(luxd::application::nfo::NfoWriteError::Io { .. })
    ));
    Ok(())
}
