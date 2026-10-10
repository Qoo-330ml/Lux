use std::time::Duration;

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// Source ids sort as `01-a-cd1`, `02-b`, `03-a-cd2`, so an id-ordered list would put the
/// single-file version between the two parts of the other version.
async fn insert_sources(
    database: &Database,
    root_id: &str,
    default_source: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    for (source_id, file_name) in [
        ("01-a-cd1", "Movie A cd1.mkv"),
        ("02-b", "Movie B.mkv"),
        ("03-a-cd2", "Movie A cd2.mkv"),
    ] {
        sqlx::query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 last_seen_generation
             ) VALUES (?, ?, ?, 'FILE', 10, 1, 'generation')",
        )
        .bind(format!("entry-{source_id}"))
        .bind(root_id)
        .bind(format!("Movie/{file_name}"))
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_sources (
                 id, item_id, source_kind, filesystem_entry_id, container, size, is_default,
                 probe_status
             ) VALUES (?, 'movie', 'LOCAL_FILE', ?, 'mkv', 10, ?, 'READY')",
        )
        .bind(source_id)
        .bind(format!("entry-{source_id}"))
        .bind(i64::from(source_id == default_source))
        .execute(database.pool())
        .await?;
    }
    Ok(())
}

async fn playback_info_order(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base_url}/emby/Items/movie/PlaybackInfo"))
        .header("X-Emby-Token", token)
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await?;
    Ok(body["MediaSources"]
        .as_array()
        .ok_or("missing media sources")?
        .iter()
        .filter_map(|source| source["Id"].as_str().map(str::to_owned))
        .collect())
}

#[tokio::test]
async fn playback_info_keeps_the_parts_of_a_version_together()
-> Result<(), Box<dyn std::error::Error>> {
    for (default_source, expected) in [
        ("01-a-cd1", ["01-a-cd1", "03-a-cd2", "02-b"]),
        ("02-b", ["02-b", "01-a-cd1", "03-a-cd2"]),
    ] {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let root_path = temp_dir.path().join("Movies");
        tokio::fs::create_dir_all(&root_path).await?;
        let database = Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        setup.complete("Admin", "Admin", "correct password").await?;
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let root = libraries
            .add_root(library.id, root_path.to_str().ok_or("non-utf8 root")?)
            .await?
            .root;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status,
                 has_available_source
             ) VALUES ('movie', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        insert_sources(&database, &root.id.to_string(), default_source).await?;

        let web_auth = WebAuthService::new(database.clone())?;
        let emby_auth = EmbyAuthService::new(database.clone())?;
        let app = app_with_state(AppState::ready(
            config, database, setup, web_auth, emby_auth,
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let base_url = format!("http://{address}");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;
        let login = client
            .post(format!("{base_url}/emby/Users/AuthenticateByName"))
            .header(
                AUTHORIZATION,
                r#"Emby Client="PartsTest", Device="Mac", DeviceId="parts-device", Version="1""#,
            )
            .json(&json!({ "Username": "admin", "Pw": "correct password" }))
            .send()
            .await?;
        assert_eq!(login.status(), reqwest::StatusCode::OK);
        let token = login.json::<Value>().await?["AccessToken"]
            .as_str()
            .ok_or("missing access token")?
            .to_owned();

        assert_eq!(
            playback_info_order(&client, &base_url, &token).await?,
            expected,
            "default {default_source}"
        );
        server.abort();
    }
    Ok(())
}
