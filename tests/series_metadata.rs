use std::collections::BTreeMap;

use luxd::{
    application::{
        libraries::LibraryService,
        metadata::{MetadataEnricher, NfoMetadata},
        nfo::{LocalNfoMetadataStore, NfoWriteService},
        people::PeopleService,
        scanner::LibraryScanner,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[tokio::test]
async fn series_metadata_reads_tvshow_season_episode_nfo_and_images()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Shows");
    let series_dir = root.join("Example Show");
    let season_dir = series_dir.join("Season 01");
    tokio::fs::create_dir_all(&season_dir).await?;
    tokio::fs::write(
        series_dir.join("tvshow.nfo"),
        "<tvshow><title>本地剧集</title><plot>剧集简介</plot><rating>8.7</rating><votes>456</votes><tagline>剧集标语</tagline><premiered>2020-01-01</premiered><status>Ended</status><language>zh</language><genre>剧情</genre><studio>示例剧厂</studio><tmdbid>60625</tmdbid><imdbid>tt1234567</imdbid><director tmdbid=\"88\">导演甲</director><actor><name>演员甲</name><role>角色甲</role><tmdbid>9</tmdbid><order>0</order></actor><custom>keep</custom></tvshow>",
    )
    .await?;
    tokio::fs::write(series_dir.join("poster.jpg"), b"series-poster").await?;
    tokio::fs::write(series_dir.join("fanart.png"), b"series-fanart").await?;
    tokio::fs::write(
        season_dir.join("season.nfo"),
        "<season><title>本地第一季</title><plot>季简介</plot><aired>2020-01-05</aired><seasonnumber>1</seasonnumber><genre>剧情</genre></season>",
    )
    .await?;
    tokio::fs::write(season_dir.join("poster.jpg"), b"season-poster").await?;
    tokio::fs::write(season_dir.join("fanart.jpg"), b"season-fanart").await?;
    tokio::fs::write(
        season_dir.join("Example.Show.S01E01.nfo"),
        "<episodedetails><title>本地第一集</title><plot>集简介</plot><aired>2020-01-05</aired><runtime>45</runtime><episodenumber>1</episodenumber><writer tmdbid=\"99\">编剧甲</writer></episodedetails>",
    )
    .await?;
    tokio::fs::write(season_dir.join("Example.Show.S01E01.mkv"), b"episode").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_series_library(library.id).await?;

    let series_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'SERIES'")
            .fetch_one(database.pool())
            .await?;
    sqlx::query("UPDATE media_items SET provider_ids_json = ? WHERE id = ?")
        .bind(r#"{"tmdb":"preexisting-tmdb","tvdb":"existing-tvdb"}"#)
        .bind(&series_id)
        .execute(database.pool())
        .await?;

    let report = MetadataEnricher::new(database.clone())
        .with_people(PeopleService::new(config.config_dir.clone()))
        .with_nfo_store(LocalNfoMetadataStore::new(database.clone()))
        .enrich_series_library(library.id)
        .await?;
    assert_eq!(report.nfo_loaded, 3);
    assert_eq!(report.nfo_failed, 0);
    assert_eq!(report.images_found, 4);

    let series_title: String =
        sqlx::query_scalar("SELECT title FROM media_items WHERE item_type = 'SERIES'")
            .fetch_one(database.pool())
            .await?;
    let season_title: String =
        sqlx::query_scalar("SELECT title FROM media_items WHERE item_type = 'SEASON'")
            .fetch_one(database.pool())
            .await?;
    let episode_title: String =
        sqlx::query_scalar("SELECT title FROM media_items WHERE item_type = 'EPISODE'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(series_title, "本地剧集");
    assert_eq!(season_title, "本地第一季");
    assert_eq!(episode_title, "本地第一集");
    let series_provider_ids: String =
        sqlx::query_scalar("SELECT provider_ids_json FROM media_items WHERE id = ?")
            .bind(&series_id)
            .fetch_one(database.pool())
            .await?;
    let series_provider_ids =
        serde_json::from_str::<BTreeMap<String, String>>(&series_provider_ids)?;
    assert_eq!(
        series_provider_ids.get("tmdb"),
        Some(&"preexisting-tmdb".to_owned())
    );
    assert_eq!(
        series_provider_ids.get("tvdb"),
        Some(&"existing-tvdb".to_owned())
    );
    assert_eq!(
        series_provider_ids.get("imdb"),
        Some(&"tt1234567".to_owned())
    );
    let rich_nfo: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT item_type, nfo_metadata_json
         FROM media_items
         WHERE item_type IN ('SERIES', 'SEASON', 'EPISODE')
         ORDER BY item_type",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(rich_nfo.len(), 3);
    assert!(rich_nfo.iter().all(|(_, value)| value.is_some()));
    assert!(
        rich_nfo
            .iter()
            .filter_map(|(_, value)| value.as_deref())
            .any(|value| value.contains("2020-01-05"))
    );
    assert!(
        rich_nfo
            .iter()
            .filter_map(|(_, value)| value.as_deref())
            .any(|value| value.contains("剧集标语"))
    );
    assert!(
        rich_nfo
            .iter()
            .filter_map(|(_, value)| value.as_deref())
            .any(|value| value.contains("编剧甲"))
    );
    let image_rows: Vec<(String, String)> =
        sqlx::query_as("SELECT image_type, source FROM item_images ORDER BY item_id, image_type")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(image_rows.len(), 4);
    assert!(image_rows.iter().all(|row| row.1 == "LOCAL"));

    let second = MetadataEnricher::new(database.clone())
        .enrich_series_library(library.id)
        .await?;
    assert_eq!(second.nfo_loaded, 0);
    assert_eq!(second.nfo_skipped, 3);
    assert_eq!(second.images_found, 0);

    sqlx::query("UPDATE media_items SET provider_ids_json = NULL WHERE id = ?")
        .bind(&series_id)
        .execute(database.pool())
        .await?;
    let third = MetadataEnricher::new(database.clone())
        .with_nfo_store(LocalNfoMetadataStore::new(database.clone()))
        .enrich_series_library(library.id)
        .await?;
    assert_eq!(third.nfo_loaded, 0);
    assert_eq!(third.nfo_skipped, 3);
    let restored_provider_ids: Option<String> =
        sqlx::query_scalar("SELECT provider_ids_json FROM media_items WHERE id = ?")
            .bind(&series_id)
            .fetch_one(database.pool())
            .await?;
    let restored_provider_ids = serde_json::from_str::<BTreeMap<String, String>>(
        restored_provider_ids
            .as_deref()
            .ok_or("provider IDs were not restored")?,
    )?;
    assert_eq!(restored_provider_ids.get("tmdb"), Some(&"60625".to_owned()));
    assert_eq!(
        restored_provider_ids.get("imdb"),
        Some(&"tt1234567".to_owned())
    );
    let actors = PeopleService::new(config.config_dir.clone())
        .list_item_actors(&series_id)
        .await?;
    assert_eq!(actors[0].name, "演员甲");
    let season_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'SEASON'")
            .fetch_one(database.pool())
            .await?;
    let episode_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'EPISODE'")
            .fetch_one(database.pool())
            .await?;
    let writer = NfoWriteService::new(database.clone());
    writer
        .write_item_nfo(
            &series_id,
            &NfoMetadata {
                title: Some("改写剧集".to_owned()),
                ..NfoMetadata::default()
            },
        )
        .await?;
    writer
        .write_item_nfo(
            &season_id,
            &NfoMetadata {
                title: Some("改写季度".to_owned()),
                ..NfoMetadata::default()
            },
        )
        .await?;
    writer
        .write_item_nfo(
            &episode_id,
            &NfoMetadata {
                title: Some("改写单集".to_owned()),
                ..NfoMetadata::default()
            },
        )
        .await?;
    assert!(
        tokio::fs::read_to_string(series_dir.join("tvshow.nfo"))
            .await?
            .contains("<custom>keep</custom>")
    );
    assert!(
        tokio::fs::read_to_string(season_dir.join("season.nfo"))
            .await?
            .contains("<title>改写季度</title>")
    );
    assert!(
        tokio::fs::read_to_string(season_dir.join("Example.Show.S01E01.nfo"))
            .await?
            .contains("<title>改写单集</title>")
    );
    Ok(())
}
