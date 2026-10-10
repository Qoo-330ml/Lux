use std::collections::BTreeMap;

use luxd::{
    application::{
        libraries::LibraryService,
        metadata::{MetadataEnricher, MetadataField, NfoMetadata},
        metadata_paths::library_item_directory,
        nfo::{
            LocalNfoMetadataStore, MetadataWriteRequest, MetadataWriteService, MovieNfoCredit,
            MovieNfoMetadata, NfoWriteService, parse_local_nfo_actors, parse_local_nfo_details,
            parse_local_nfo_projection, parse_movie_nfo_actors, parse_movie_nfo_details,
            rewrite_movie_nfo, rewrite_nfo, rewrite_nfo_probe_details, rewrite_series_nfo,
            write_nfo_atomically,
        },
        people::ActorCredit,
        probe::{MediaProbeResult, MediaStreamResult, StreamType},
        scanner::{LibraryScanner, ScanJobService},
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use serde_json::Value;

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
    let original = r#"<movie><title>旧标题</title><rating>1</rating><genre>旧类型</genre><actor><name>旧演员</name></actor><id>tt-old</id><uniqueid type="official website">https://old.example/movie</uniqueid><uniqueid type="custom">custom-id</uniqueid><custom><keep>保留</keep></custom></movie>"#;
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
            last_air_date: None,
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
                provider: None,
                identities: Vec::new(),
                name: "沈腾".to_owned(),
                character: Some("张驰".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
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
    assert!(text.contains("<id>tt38035835</id>"), "{text}");
    assert!(
        text.contains("<uniqueid type=\"official website\">https://example.com/movie</uniqueid>")
    );
    assert!(text.contains("<uniqueid type=\"custom\">custom-id</uniqueid>"));
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
    assert!(!text.contains("<id>tt-old</id>"));
    assert!(!text.contains("https://old.example/movie"));
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
fn movie_nfo_rewrite_keeps_original_bytes_when_rich_patch_is_semantically_equal()
-> Result<(), Box<dyn std::error::Error>> {
    let original = r#"<movie><title>电影</title><thumb aspect="poster">https://example.com/poster.jpg</thumb><actor><name>演员甲</name><role>角色甲</role><tmdbid>42</tmdbid><order>0</order></actor><genre>剧情</genre><rating>8</rating></movie>"#;
    let patch = MovieNfoMetadata {
        rating: Some(8.0),
        genres: vec!["剧情".to_owned()],
        poster_url: Some("https://example.com/poster.jpg".to_owned()),
        actors: vec![ActorCredit {
            id: "42".to_owned(),
            provider: Some("tmdb".to_owned()),
            identities: Vec::new(),
            name: "演员甲".to_owned(),
            character: Some("角色甲".to_owned()),
            order: Some(0),
            profile_url: None,
            person: None,
        }],
        ..MovieNfoMetadata::default()
    };

    assert_eq!(
        rewrite_movie_nfo(original.as_bytes(), &patch)?,
        original.as_bytes()
    );
    Ok(())
}

#[test]
fn series_nfo_rewrite_writes_rich_fields_with_tvshow_root() -> Result<(), Box<dyn std::error::Error>>
{
    let rewritten = rewrite_series_nfo(
        &[],
        &MovieNfoMetadata {
            base: NfoMetadata {
                title: Some("藏锋".to_owned()),
                original_title: Some("藏锋".to_owned()),
                overview: Some("剧集简介".to_owned()),
                production_year: Some(2026),
            },
            rating: Some(8.0),
            votes: Some(456),
            premiered: Some("2026-08-17".to_owned()),
            releasedate: Some("2026-08-17".to_owned()),
            last_air_date: Some("2026-09-17".to_owned()),
            runtime: Some(45),
            status: Some("Continuing".to_owned()),
            original_language: Some("zh".to_owned()),
            countries: vec!["中国".to_owned()],
            genres: vec!["犯罪".to_owned(), "悬疑".to_owned()],
            studios: vec!["Tencent Video".to_owned()],
            provider_ids: [
                ("tmdb".to_owned(), "280133".to_owned()),
                ("imdb".to_owned(), "tt36277135".to_owned()),
                ("tvdb".to_owned(), "480823".to_owned()),
            ]
            .into_iter()
            .collect(),
            directors: vec![MovieNfoCredit {
                provider_id: "11".to_owned(),
                name: "导演甲".to_owned(),
            }],
            writers: vec![MovieNfoCredit {
                provider_id: "12".to_owned(),
                name: "编剧甲".to_owned(),
            }],
            actors: vec![ActorCredit {
                id: "126736".to_owned(),
                provider: Some("tmdb".to_owned()),
                identities: Vec::new(),
                name: "段奕宏".to_owned(),
                character: Some("Tan Yan".to_owned()),
                order: Some(0),
                profile_url: None,
                person: None,
            }],
            trailers: vec!["https://example.com/trailer".to_owned()],
            ..MovieNfoMetadata::default()
        },
    )?;
    let text = String::from_utf8(rewritten)?;
    assert!(text.starts_with("<tvshow>"));
    assert!(text.contains("<title>藏锋</title>"));
    assert!(text.contains("<rating>8</rating>"));
    assert!(text.contains("<votes>456</votes>"));
    assert!(text.contains("<premiered>2026-08-17</premiered>"));
    assert!(text.contains("<releasedate>2026-08-17</releasedate>"));
    assert!(text.contains("<lastaired>2026-09-17</lastaired>"));
    assert!(text.contains("<runtime>45</runtime>"));
    assert!(text.contains("<status>Continuing</status>"));
    assert!(text.contains("<language>zh</language>"));
    assert!(text.contains("<country>中国</country>"));
    assert!(text.contains("<genre>犯罪</genre>"));
    assert!(text.contains("<genre>悬疑</genre>"));
    assert!(text.contains("<studio>Tencent Video</studio>"));
    assert!(text.contains("<uniqueid type=\"tmdb\" default=\"true\">280133</uniqueid>"));
    assert!(text.contains("<uniqueid type=\"imdb\">tt36277135</uniqueid>"));
    assert!(text.contains("<uniqueid type=\"tvdb\">480823</uniqueid>"));
    assert!(text.contains("<director tmdbid=\"11\">导演甲</director>"));
    assert!(text.contains("<writer tmdbid=\"12\">编剧甲</writer>"));
    assert!(text.contains("<name>段奕宏</name>"));
    assert!(text.contains("<role>Tan Yan</role>"));
    assert!(text.contains("<tmdbid>126736</tmdbid>"));
    assert!(text.contains("<trailer>https://example.com/trailer</trailer>"));
    assert!(text.ends_with("</tvshow>"));
    Ok(())
}

#[test]
fn series_nfo_rewrite_normalizes_an_existing_movie_root() -> Result<(), Box<dyn std::error::Error>>
{
    let rewritten = rewrite_series_nfo(
        "<movie><title>旧剧名</title><rating>1</rating></movie>".as_bytes(),
        &MovieNfoMetadata {
            rating: Some(8.0),
            last_air_date: Some("2026-09-17".to_owned()),
            ..MovieNfoMetadata::default()
        },
    )?;
    let text = String::from_utf8(rewritten)?;
    assert!(text.starts_with("<tvshow>"));
    assert!(text.ends_with("</tvshow>"));
    assert!(text.contains("<rating>8</rating>"));
    assert!(text.contains("<lastaired>2026-09-17</lastaired>"));
    assert!(!text.contains("<movie>"));
    Ok(())
}

#[test]
fn movie_nfo_rewrite_keeps_actor_without_provider_id() -> Result<(), Box<dyn std::error::Error>> {
    let result = rewrite_movie_nfo(
        b"<movie><title>Movie</title></movie>",
        &MovieNfoMetadata {
            actors: vec![ActorCredit {
                id: String::new(),
                provider: None,
                identities: Vec::new(),
                name: "演员甲".to_owned(),
                character: None,
                order: None,
                profile_url: None,
                person: None,
            }],
            ..MovieNfoMetadata::default()
        },
    );

    let text = String::from_utf8(result?)?;
    assert!(text.contains("<name>演员甲</name>"));
    assert!(!text.contains("<tmdbid>"));
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

#[test]
fn movie_nfo_parser_keeps_actor_without_provider_id() {
    let actors = parse_movie_nfo_actors(
        r#"<movie><actor><name>本地演员</name><role>本地角色</role><order>2</order></actor></movie>"#
            .as_bytes(),
    )
    .expect("valid actor without provider id");

    assert_eq!(actors.len(), 1);
    assert!(actors[0].id.is_empty());
    assert_eq!(actors[0].name, "本地演员");
    assert_eq!(actors[0].character.as_deref(), Some("本地角色"));
}

#[test]
fn movie_nfo_actor_parser_and_writer_support_up_to_100_cast_members()
-> Result<(), Box<dyn std::error::Error>> {
    let actor_nodes = (0..105)
        .map(|order| {
            format!(
                "<actor><name>Actor {order}</name><role>Character {order}</role><order>{order}</order></actor>"
            )
        })
        .collect::<String>();
    let original = format!("<movie>{actor_nodes}</movie>");
    let parsed = parse_movie_nfo_actors(original.as_bytes())?;
    assert_eq!(parsed.len(), 100);
    assert_eq!(parsed[0].name, "Actor 0");
    assert_eq!(parsed[99].name, "Actor 99");
    assert_eq!(parsed[99].order, Some(99));

    let actors = (0..105)
        .map(|order| ActorCredit {
            id: order.to_string(),
            provider: Some("tmdb".to_owned()),
            identities: Vec::new(),
            name: format!("Actor {order}"),
            character: Some(format!("Character {order}")),
            order: Some(order),
            profile_url: None,
            person: None,
        })
        .collect();
    let rewritten = rewrite_movie_nfo(
        b"<movie/>",
        &MovieNfoMetadata {
            actors,
            ..MovieNfoMetadata::default()
        },
    )?;
    let text = String::from_utf8(rewritten)?;
    assert_eq!(text.matches("<actor>").count(), 100);
    assert!(text.contains("<name>Actor 99</name>"));
    assert!(!text.contains("<name>Actor 100</name>"));
    Ok(())
}

#[test]
fn movie_nfo_rewrite_serializes_probe_fileinfo_and_preserves_other_fileinfo_xml()
-> Result<(), Box<dyn std::error::Error>> {
    let video_details = BTreeMap::from([
        ("Width".to_owned(), Value::from(1920)),
        ("Height".to_owned(), Value::from(798)),
        ("AspectRatio".to_owned(), Value::from("320:133")),
        ("RealFrameRate".to_owned(), Value::from("24000/1001")),
        ("BitRate".to_owned(), Value::from(15_000_000)),
        ("ScanType".to_owned(), Value::from("progressive")),
    ]);
    let audio_details = BTreeMap::from([
        ("Channels".to_owned(), Value::from(6)),
        ("SampleRate".to_owned(), Value::from(48_000)),
    ]);
    let probe_details = MediaProbeResult {
        container: Some("matroska".to_owned()),
        source_size: Some(12_345),
        duration_ticks: Some(63_740_000_000),
        bitrate: Some(15_975_933),
        streams: vec![
            MediaStreamResult {
                stream_index: 0,
                stream_type: StreamType::Video,
                codec: Some("hevc".to_owned()),
                language: Some("eng".to_owned()),
                title: None,
                is_default: true,
                is_forced: false,
                details: video_details,
            },
            MediaStreamResult {
                stream_index: 1,
                stream_type: StreamType::Audio,
                codec: Some("dts".to_owned()),
                language: Some("eng".to_owned()),
                title: None,
                is_default: true,
                is_forced: false,
                details: audio_details,
            },
            MediaStreamResult {
                stream_index: 2,
                stream_type: StreamType::Subtitle,
                codec: Some("hdmv_pgs_subtitle".to_owned()),
                language: Some("chi".to_owned()),
                title: None,
                is_default: false,
                is_forced: false,
                details: BTreeMap::new(),
            },
        ],
    };
    let original = r#"<movie><title>Keep title</title><fileinfo><legacy>keep</legacy><streamdetails><video><codec>old</codec></video></streamdetails></fileinfo><custom>keep</custom></movie>"#;

    let rewritten = rewrite_nfo_probe_details(original.as_bytes(), &probe_details)?;
    let text = String::from_utf8(rewritten)?;

    assert!(text.contains("<legacy>keep</legacy>"));
    assert!(text.contains("<custom>keep</custom>"));
    assert!(text.contains("<codec>hevc</codec><micodec>hevc</micodec>"));
    assert!(text.contains("<bitrate>15000000</bitrate>"));
    assert!(text.contains("<width>1920</width><height>798</height>"));
    assert!(text.contains("<aspect>320:133</aspect><aspectratio>320:133</aspectratio>"));
    assert!(text.contains("<framerate>23.976024</framerate>"));
    assert!(text.contains("<scantype>progressive</scantype>"));
    assert!(text.contains("<duration>106</duration><durationinseconds>6374</durationinseconds>"));
    assert!(text.contains("<channels>6</channels><samplingrate>48000</samplingrate>"));
    assert!(text.contains("<codec>PGSSUB</codec><micodec>PGSSUB</micodec>"));
    assert!(!text.contains("<codec>old</codec>"));
    Ok(())
}

#[test]
fn movie_nfo_uses_actor_provider_specific_identity_tags() -> Result<(), Box<dyn std::error::Error>>
{
    let rewritten = rewrite_movie_nfo(
        b"<movie><title>Movie</title></movie>",
        &MovieNfoMetadata {
            actors: vec![ActorCredit {
                id: "nm123".to_owned(),
                provider: Some("imdb".to_owned()),
                identities: Vec::new(),
                name: "演员甲".to_owned(),
                character: None,
                order: None,
                profile_url: None,
                person: None,
            }],
            ..MovieNfoMetadata::default()
        },
    )?;
    let text = String::from_utf8(rewritten)?;
    assert!(text.contains("<imdbid>nm123</imdbid>"));
    assert!(!text.contains("<tmdbid>nm123</tmdbid>"));

    let actors = parse_movie_nfo_actors(
        b"<movie><actor><name>Actor</name><imdbid>nm123</imdbid></actor></movie>",
    )?;
    assert_eq!(actors[0].provider.as_deref(), Some("imdb"));
    assert_eq!(actors[0].id, "nm123");
    Ok(())
}

#[test]
fn local_nfo_parser_supports_series_season_episode_fields() {
    let details = parse_local_nfo_details(
        r#"<episodedetails>
            <aired>2020-01-05</aired><lastaired>2020-04-01</lastaired>
            <runtime>45</runtime><seasonnumber>1</seasonnumber><episodenumber>2</episodenumber>
            <writer tmdbid="99">编剧甲</writer>
        </episodedetails>"#
            .as_bytes(),
    )
    .expect("valid episode nfo");
    assert_eq!(details.aired.as_deref(), Some("2020-01-05"));
    assert_eq!(details.last_air_date.as_deref(), Some("2020-04-01"));
    assert_eq!(details.runtime, Some(45));
    assert_eq!(details.season_number, Some(1));
    assert_eq!(details.episode_number, Some(2));
    assert_eq!(details.writers[0].name, "编剧甲");

    let actors = parse_local_nfo_actors(
        r#"<tvshow><actor><name>演员甲</name><role>角色甲</role><tmdbid>9</tmdbid></actor></tvshow>"#
            .as_bytes(),
    )
    .expect("valid series actor node");
    assert_eq!(actors[0].id, "9");
}

#[test]
fn local_nfo_projection_parses_base_rich_and_actor_fields_together() {
    let projection = parse_local_nfo_projection(
        r#"<tvshow>
            <title>本地剧集</title><originaltitle>Original Show</originaltitle>
            <year>2020</year><plot>剧集简介</plot><rating>8.7</rating>
            <genre>剧情</genre><tmdbid>60625</tmdbid>
            <actor><name>演员甲</name><role>角色甲</role><tmdbid>9</tmdbid><order>0</order></actor>
        </tvshow>"#
            .as_bytes(),
    )
    .expect("valid local NFO projection");

    assert_eq!(projection.metadata.title.as_deref(), Some("本地剧集"));
    assert_eq!(
        projection.metadata.original_title.as_deref(),
        Some("Original Show")
    );
    assert_eq!(projection.metadata.production_year, Some(2020));
    assert_eq!(projection.metadata.overview.as_deref(), Some("剧集简介"));
    assert_eq!(projection.details.rating, Some(8.7));
    assert_eq!(projection.details.genres, vec!["剧情"]);
    assert_eq!(projection.details.provider_ids["tmdb"], "60625");
    assert_eq!(projection.actors.len(), 1);
    assert_eq!(projection.actors[0].id, "9");
    assert_eq!(projection.actors[0].character.as_deref(), Some("角色甲"));
}

#[test]
fn movie_nfo_parser_reads_rich_local_details_without_online_metadata() {
    let details = parse_movie_nfo_details(
        r#"<movie>
            <rating>8.1</rating><votes>123</votes><tagline>大漠路远</tagline>
            <premiered>2026-02-17</premiered><releasedate>2026-02-20</releasedate>
            <runtime>126</runtime><status>Released</status><language>zh</language>
            <website>https://example.com/movie</website><set>镖人</set><setid>77</setid>
            <mpaa>PG-13</mpaa><country>中国</country><country>香港</country>
            <genre>动作</genre><genre>剧情</genre><studio>示例影业</studio>
            <tmdbid>1462229</tmdbid><imdbid>tt1234567</imdbid>
            <uniqueid type="wikidata">Q123</uniqueid>
            <director tmdbid="18899">导演甲</director>
            <writer tmdbid="19999">编剧甲</writer>
            <credits tmdbid="20000">编剧乙</credits>
            <trailer>https://www.youtube.com/watch?v=test</trailer>
        </movie>"#
            .as_bytes(),
    )
    .expect("valid rich movie nfo");

    assert_eq!(details.rating, Some(8.1));
    assert_eq!(details.votes, Some(123));
    assert_eq!(details.tagline.as_deref(), Some("大漠路远"));
    assert_eq!(details.premiered.as_deref(), Some("2026-02-17"));
    assert_eq!(details.release_date.as_deref(), Some("2026-02-20"));
    assert_eq!(details.runtime, Some(126));
    assert_eq!(details.original_language.as_deref(), Some("zh"));
    assert_eq!(details.countries, vec!["中国", "香港"]);
    assert_eq!(details.genres, vec!["动作", "剧情"]);
    assert_eq!(details.studios, vec!["示例影业"]);
    assert_eq!(details.provider_ids["tmdb"], "1462229");
    assert_eq!(details.provider_ids["imdb"], "tt1234567");
    assert_eq!(details.provider_ids["wikidata"], "Q123");
    assert_eq!(details.directors[0].name, "导演甲");
    assert_eq!(details.writers.len(), 2);
    assert_eq!(
        details.trailers,
        vec!["https://www.youtube.com/watch?v=test"]
    );
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
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;

    let report = NfoWriteService::new_with_config_dir(database.clone(), config.config_dir.clone())
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
    let canonical_movie_dir = tokio::fs::canonicalize(&movie_dir).await?;
    assert_eq!(report.path, canonical_movie_dir.join("movie.nfo"));
    assert!(
        !library_item_directory(&config.config_dir, &item_id)?
            .join("movie.nfo")
            .exists()
    );
    let fingerprint: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT metadata_fingerprint FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(fingerprint, Some(report.fingerprint));
    Ok(())
}

#[tokio::test]
async fn nfo_service_can_write_an_additional_metadata_copy()
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
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    sqlx::query("UPDATE libraries SET media_strategy_json = ? WHERE id = ?")
        .bind(
            serde_json::json!({
                "images": { "writeToMetadata": true }
            })
            .to_string(),
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;

    let report = NfoWriteService::new_with_config_dir(database, config.config_dir.clone())
        .write_item_nfo(
            &item_id,
            &NfoMetadata {
                title: Some("镜像电影".to_owned()),
                ..NfoMetadata::default()
            },
        )
        .await?;
    let media_copy = tokio::fs::read(&report.path).await?;
    let metadata_copy = library_item_directory(&config.config_dir, &item_id)?.join("movie.nfo");
    assert_eq!(tokio::fs::read(metadata_copy).await?, media_copy);
    Ok(())
}

#[tokio::test]
async fn movie_nfo_write_adds_database_sort_title_and_date_without_overwriting_local_values()
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
    let item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE' LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    sqlx::query("UPDATE media_items SET sort_title = ?, added_at = 0 WHERE id = ?")
        .bind("007: quantum of solace")
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE libraries SET media_strategy_json = ? WHERE id = ?")
        .bind(serde_json::json!({ "images": { "writeToMetadata": true } }).to_string())
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;

    let service = NfoWriteService::new_with_config_dir(database.clone(), config.config_dir.clone());
    service
        .write_item_movie_nfo(
            &item_id,
            &MovieNfoMetadata {
                base: NfoMetadata {
                    title: Some("Quantum of Solace".to_owned()),
                    ..NfoMetadata::default()
                },
                ..MovieNfoMetadata::default()
            },
        )
        .await?;
    let nfo_path = movie_dir.join("movie.nfo");
    let generated = tokio::fs::read_to_string(&nfo_path).await?;
    assert!(generated.contains("<sorttitle>007: quantum of solace</sorttitle>"));
    assert!(generated.contains("<dateadded>1970-01-01 00:00:00</dateadded>"));
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    let probe = MediaProbeResult {
        container: Some("mkv".to_owned()),
        source_size: Some(100),
        duration_ticks: Some(60 * 10_000_000),
        bitrate: Some(500_000),
        streams: vec![MediaStreamResult {
            stream_index: 0,
            stream_type: StreamType::Video,
            codec: Some("h264".to_owned()),
            language: None,
            title: None,
            is_default: true,
            is_forced: false,
            details: BTreeMap::from([("Width".to_owned(), Value::from(1920))]),
        }],
    };
    assert!(
        service
            .write_item_probe_details(&item_id, &source_id, &probe)
            .await?
    );
    let with_probe = tokio::fs::read_to_string(&nfo_path).await?;
    assert!(with_probe.contains("<width>1920</width>"));
    assert!(with_probe.contains("<sorttitle>007: quantum of solace</sorttitle>"));
    let metadata_copy = library_item_directory(&config.config_dir, &item_id)?.join("movie.nfo");
    assert_eq!(tokio::fs::read_to_string(metadata_copy).await?, with_probe);

    tokio::fs::write(
        &nfo_path,
        "<movie><sorttitle>Curated sort</sorttitle><dateadded>2001-02-03 04:05:06</dateadded><custom>keep</custom></movie>",
    )
    .await?;
    service
        .write_item_movie_nfo(
            &item_id,
            &MovieNfoMetadata {
                base: NfoMetadata {
                    title: Some("Quantum of Solace".to_owned()),
                    ..NfoMetadata::default()
                },
                ..MovieNfoMetadata::default()
            },
        )
        .await?;
    let preserved = tokio::fs::read_to_string(nfo_path).await?;
    assert!(preserved.contains("<sorttitle>Curated sort</sorttitle>"));
    assert!(preserved.contains("<dateadded>2001-02-03 04:05:06</dateadded>"));
    assert!(preserved.contains("<custom>keep</custom>"));
    Ok(())
}

#[tokio::test]
async fn metadata_editor_writes_home_video_same_name_nfo_and_metadata_mirror()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Other videos");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Holiday.S01E01.mkv"), b"home video").await?;
    tokio::fs::write(
        root.join("movie.nfo"),
        "<movie><title>unrelated directory nfo</title></movie>",
    )
    .await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Other videos", LibraryKind::HomeVideos, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;
    let item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'VIDEO' LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    sqlx::query("UPDATE libraries SET media_strategy_json = ? WHERE id = ?")
        .bind(
            serde_json::json!({
                "images": { "writeToMetadata": true }
            })
            .to_string(),
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;

    let editor =
        MetadataWriteService::new_with_config_dir(database.clone(), config.config_dir.clone());
    editor
        .write_item_metadata(
            &item_id,
            MetadataWriteRequest {
                title: "手动视频标题".to_owned(),
                original_title: Some("Original clip".to_owned()),
                overview: Some("手动维护的简介".to_owned()),
                production_year: Some(2024),
                locked_fields: [MetadataField::Title].into_iter().collect(),
            },
        )
        .await?;

    let same_name_nfo = root.join("Holiday.S01E01.nfo");
    let output = tokio::fs::read_to_string(&same_name_nfo).await?;
    assert!(output.contains("<title>手动视频标题</title>"));
    assert!(output.contains("<year>2024</year>"));
    assert_eq!(
        tokio::fs::read_to_string(root.join("movie.nfo")).await?,
        "<movie><title>unrelated directory nfo</title></movie>"
    );
    let metadata_copy =
        library_item_directory(&config.config_dir, &item_id)?.join("Holiday.S01E01.nfo");
    assert_eq!(
        tokio::fs::read(metadata_copy).await?,
        tokio::fs::read(same_name_nfo).await?
    );
    let item_type: String = sqlx::query_scalar("SELECT item_type FROM media_items WHERE id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(item_type, "VIDEO");
    Ok(())
}

#[tokio::test]
async fn nfo_writeback_only_invalidates_rich_snapshot_when_content_changes()
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
        b"<movie><title>old</title><tagline>cached</tagline></movie>",
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
    MetadataEnricher::new(database.clone())
        .with_nfo_store(LocalNfoMetadataStore::new(database.clone()))
        .enrich_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let initial: (Option<String>, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT nfo_metadata_json, nfo_metadata_fingerprint
         FROM media_items WHERE id = ?",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert!(
        initial
            .0
            .as_deref()
            .is_some_and(|json| json.contains("cached"))
    );
    assert!(initial.1.is_some());

    NfoWriteService::new(database.clone())
        .write_item_nfo(
            &item_id,
            &NfoMetadata {
                title: Some("old".to_owned()),
                ..NfoMetadata::default()
            },
        )
        .await?;
    let unchanged: (Option<String>, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT nfo_metadata_json, nfo_metadata_fingerprint
         FROM media_items WHERE id = ?",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(unchanged, initial);

    NfoWriteService::new(database.clone())
        .write_item_nfo(
            &item_id,
            &NfoMetadata {
                title: Some("new".to_owned()),
                ..NfoMetadata::default()
            },
        )
        .await?;
    let changed: (Option<String>, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT nfo_metadata_json, nfo_metadata_fingerprint
         FROM media_items WHERE id = ?",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(changed, (None, None));
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
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
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
async fn nfo_service_reuses_an_existing_nonstandard_movie_nfo()
-> Result<(), Box<dyn std::error::Error>> {
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
    let existing_nfo = movie_dir.join("metadata-export.nfo");
    tokio::fs::write(&existing_nfo, "<movie><custom>keep</custom></movie>").await?;

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
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;

    let report = NfoWriteService::new(database)
        .write_item_nfo(
            &item_id,
            &NfoMetadata {
                title: Some("已更新标题".to_owned()),
                ..NfoMetadata::default()
            },
        )
        .await?;

    assert_eq!(report.path, tokio::fs::canonicalize(&existing_nfo).await?);
    assert!(!movie_dir.join("movie.nfo").exists());
    let output = tokio::fs::read_to_string(existing_nfo).await?;
    assert!(output.contains("<custom>keep</custom>"));
    assert!(output.contains("<title>已更新标题</title>"));
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
