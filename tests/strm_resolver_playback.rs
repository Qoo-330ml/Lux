#[cfg(unix)]
#[tokio::test]
async fn path_strm_playback_uses_a_generic_resolver_plugin() -> Result<(), Box<dyn std::error::Error>> {
    use std::{fs, os::unix::fs::PermissionsExt};

    use luxd::{
        api::{AppState, app_with_state},
        application::{
            libraries::LibraryService,
            plugins::PluginService,
            scanner::LibraryScanner,
            setup::SetupService,
        },
        auth::{emby::EmbyAuthService, sessions::WebAuthService},
        config::Config,
        library::LibraryKind,
        storage::Database,
    };
    use reqwest::header::AUTHORIZATION;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;

    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let plugin_dir = config_dir.join("plugins/org.lux.test-strm-resolver");
    let binaries_dir = plugin_dir.join("binaries");
    tokio::fs::create_dir_all(&binaries_dir).await?;
    tokio::fs::write(
        plugin_dir.join("manifest.json"),
        serde_json::to_vec(&json!({
            "formatVersion": 1,
            "id": "org.lux.test-strm-resolver",
            "name": "Test STRM resolver",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "strm_resolver",
            "category": "MEDIA",
            "capabilities": ["strm.resolve"],
            "permissions": {"network": []},
            "files": []
        }))?,
    )
    .await?;
    let entrypoint = binaries_dir.join("plugin");
    fs::write(
        &entrypoint,
        br#"#!/bin/sh
IFS= read -r line
id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
if [ "$method" = "strm.resolve" ]; then
  printf '{"id":"%s","result":{"status":"RESOLVED","url":"https://media.example.test/resolved.mkv"}}\n' "$id"
else
  printf '{"id":"%s","result":{}}\n' "$id"
fi
"#,
    )?;
    fs::set_permissions(&entrypoint, fs::Permissions::from_mode(0o700))?;

    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(
        root.join("Path.Movie.2026.strm"),
        "/cloud/library/movie.mp4\n",
    )
    .await?;
    LibraryService::new(database.clone())
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE title = 'Path Movie'")
            .fetch_one(database.pool())
            .await?;
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    PluginService::new(database.clone(), config_dir)
        .install("org.lux.test-strm-resolver")
        .await?;

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database,
        setup,
        auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::new();
    let login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="StrmResolverTest", Device="Mac", DeviceId="strm-resolver", Version="1""#,
        )
        .json(&json!({"Username": "admin", "Pw": "correct password"}))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();

    let playback = client
        .get(format!("http://{address}/Items/{item_id}/PlaybackInfo"))
        .query(&[("api_key", token.as_str())])
        .send()
        .await?;
    assert_eq!(playback.status(), reqwest::StatusCode::OK);
    let playback_body = playback.json::<Value>().await?;
    assert_eq!(
        playback_body["MediaSources"][0]["DirectStreamUrl"],
        format!("/Videos/{item_id}/{source_id}/stream")
    );
    assert_eq!(playback_body["MediaSources"][0]["Protocol"], "Http");

    let no_redirect_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let stream = no_redirect_client
        .get(format!("http://{address}/Videos/{item_id}/{source_id}/stream"))
        .query(&[("api_key", token.as_str())])
        .send()
        .await?;
    assert_eq!(
        stream.status(),
        reqwest::StatusCode::TEMPORARY_REDIRECT
    );
    assert_eq!(
        stream.headers()[reqwest::header::LOCATION],
        "https://media.example.test/resolved.mkv"
    );

    server.abort();
    Ok(())
}
