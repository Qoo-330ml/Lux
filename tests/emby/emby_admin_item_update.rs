//! Emby item metadata editing (`POST /Items/{Id}`) and session-management permissions.
//!
//! Emby/Jellyfin clients edit an item by posting the whole `BaseItemDto` back to
//! `POST /Items/{Id}`. Lux has to accept that wide body (it echoes `Genres`, `Studios`,
//! `ProviderIds`, `People`, `LockData`, ...) while the older person editor keeps working, and the
//! edit has to be gated on `can_manage_server` exactly like the Lux admin metadata endpoint.
//! `GET /Sessions` and `POST /Sessions/{Id}/Playing/Stop` follow the same capability, so a
//! manager-only user sees and stops every session while a plain user sees only their own.

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{
        admin_api_key::AdminApiKeyService,
        emby::EmbyAuthService,
        sessions::WebAuthService,
        users::{UserRecord, UserStore, UserUpdate},
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::net::TcpListener;

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Fixture {
    base_url: String,
    admin_key: String,
    database: Database,
    admin: UserRecord,
    manager: UserRecord,
    viewer: UserRecord,
    movie_id: String,
    movie_public_id: String,
    series_id: String,
    series_public_id: String,
    movie_directory: PathBuf,
    series_directory: PathBuf,
    _server: AbortOnDrop,
    _temp: tempfile::TempDir,
}

fn emby_public_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
}

fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client")
}

async fn emby_token(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
    device_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            format!(
                r#"Emby Client="LuxItemUpdateTest", Device="Mac", DeviceId="{device_id}", Version="1""#
            ),
        )
        .json(&json!({ "Username": username, "Pw": password }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK, "emby login");
    let body: Value = login.json().await?;
    body["AccessToken"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "missing Emby access token".into())
}

async fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup
        .complete("Admin", "Administrator", "correct password")
        .await?;
    let users = UserStore::new(database.clone())?;
    let viewer = users
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let manager = users
        .create_user("manager", "Manager", "manager password", false)
        .await?;
    // A manager is not an Emby administrator (`is_admin == false`); only the Lux
    // server-management capability is granted, which is what the session endpoints have to honour.
    let manager = users
        .update_user(
            &manager.id.to_string(),
            UserUpdate {
                can_manage_server: Some(true),
                ..UserUpdate::default()
            },
        )
        .await?
        .ok_or("manager disappeared after the permission update")?;

    let libraries = LibraryService::new(database.clone());
    let movie_library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let movie_root = temp_dir.path().join("Movies");
    let movie_directory = movie_root.join("Edited Movie (2020)");
    tokio::fs::create_dir_all(&movie_directory).await?;
    tokio::fs::write(
        movie_directory.join("Fixture.Movie.2020.mkv"),
        b"movie fixture",
    )
    .await?;
    libraries
        .add_root(
            movie_library.id,
            movie_root.to_str().ok_or("non-utf8 movie root")?,
        )
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(movie_library.id)
        .await?;

    let series_library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let series_root = temp_dir.path().join("Shows");
    let series_directory = series_root.join("Edited Show");
    let season_directory = series_directory.join("Season 01");
    tokio::fs::create_dir_all(&season_directory).await?;
    tokio::fs::write(
        season_directory.join("Edited.Show.S01E01.mkv"),
        b"episode fixture",
    )
    .await?;
    libraries
        .add_root(
            series_library.id,
            series_root.to_str().ok_or("non-utf8 series root")?,
        )
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(series_library.id)
        .await?;

    let movie_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE'")
            .fetch_one(database.pool())
            .await?;
    let series_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'SERIES'")
            .fetch_one(database.pool())
            .await?;
    let admin_key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = AbortOnDrop(tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    }));
    Ok(Fixture {
        base_url: format!("http://{address}"),
        admin_key,
        database,
        admin,
        manager,
        viewer,
        movie_public_id: emby_public_id(&movie_id),
        movie_id,
        series_public_id: emby_public_id(&series_id),
        series_id,
        movie_directory,
        series_directory,
        _server: server,
        _temp: temp_dir,
    })
}

struct StoredItem {
    title: String,
    original_title: Option<String>,
    overview: Option<String>,
    production_year: Option<i64>,
    locked_fields: Vec<String>,
}

async fn stored_item(
    database: &Database,
    item_id: &str,
) -> Result<StoredItem, Box<dyn std::error::Error>> {
    let (title, original_title, overview, production_year, locked_fields_json): (
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT title, original_title, overview, production_year, locked_fields_json
         FROM media_items WHERE id = ?",
    )
    .bind(item_id)
    .fetch_one(database.pool())
    .await?;
    let locked_fields = locked_fields_json
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default();
    Ok(StoredItem {
        title,
        original_title,
        overview,
        production_year,
        locked_fields,
    })
}

#[tokio::test]
async fn admin_edits_movie_metadata_through_both_emby_item_update_routes()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture().await?;
    let client = test_client();
    let admin_token = emby_token(
        &client,
        &fixture.base_url,
        "admin",
        "correct password",
        "admin-device",
    )
    .await?;

    // Emby `POST /Items/{Id}` with the administrator's Emby session token. The body is the whole
    // `BaseItemDto`, including fields Lux does not model.
    let updated = client
        .post(format!(
            "{}/emby/Items/{}",
            fixture.base_url, fixture.movie_public_id
        ))
        .header("X-Emby-Token", &admin_token)
        .json(&json!({
            "Name": "Edited Movie",
            "ServerId": "ignored-by-lux",
            "Id": fixture.movie_public_id,
            "Type": "Movie",
            "OriginalTitle": "Edited Original",
            "Overview": "Edited overview",
            "ProductionYear": 2021,
            "LockedFields": ["Name", "Overview", "Genres", "Tags", "Studios"],
            "Genres": ["Drama"],
            "Studios": [{ "Name": "A Studio", "Id": "1" }],
            "ProviderIds": { "Tmdb": "123456" },
            "People": [{ "Name": "An Actor", "Id": "actor-1", "Type": "Actor" }],
            "Taglines": ["A tagline"],
            "LockData": false
        }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::NO_CONTENT);

    let stored = stored_item(&fixture.database, &fixture.movie_id).await?;
    assert_eq!(stored.title, "Edited Movie");
    assert_eq!(stored.original_title.as_deref(), Some("Edited Original"));
    assert_eq!(stored.overview.as_deref(), Some("Edited overview"));
    assert_eq!(stored.production_year, Some(2021));
    assert_eq!(stored.locked_fields, vec!["title", "overview"]);

    let detail = client
        .get(format!(
            "{}/Items/{}",
            fixture.base_url, fixture.movie_public_id
        ))
        .query(&[("api_key", fixture.admin_key.as_str())])
        .send()
        .await?;
    assert_eq!(detail.status(), reqwest::StatusCode::OK);
    let detail: Value = detail.json().await?;
    assert_eq!(detail["Name"], "Edited Movie");
    assert_eq!(detail["OriginalTitle"], "Edited Original");
    assert_eq!(detail["Overview"], "Edited overview");
    assert_eq!(detail["ProductionYear"], 2021);

    let movie_nfo = tokio::fs::read_to_string(fixture.movie_directory.join("movie.nfo")).await?;
    assert!(movie_nfo.contains("<title>Edited Movie</title>"));
    assert!(movie_nfo.contains("<originaltitle>Edited Original</originaltitle>"));
    assert!(movie_nfo.contains("<plot>Edited overview</plot>"));
    assert!(movie_nfo.contains("<year>2021</year>"));

    // The same endpoint on the unprefixed mount with the shared admin API key, and the Emby
    // "clear this field" shape: an explicit `null`. Omitted keys keep the stored value and the
    // locks.
    let cleared = client
        .post(format!(
            "{}/Items/{}",
            fixture.base_url, fixture.movie_public_id
        ))
        .query(&[("api_key", fixture.admin_key.as_str())])
        .json(&json!({
            "Name": "Renamed Movie",
            "OriginalTitle": null,
            "ProductionYear": null
        }))
        .send()
        .await?;
    assert_eq!(cleared.status(), reqwest::StatusCode::NO_CONTENT);

    let stored = stored_item(&fixture.database, &fixture.movie_id).await?;
    assert_eq!(stored.title, "Renamed Movie");
    assert_eq!(stored.original_title, None);
    assert_eq!(stored.production_year, None);
    assert_eq!(stored.overview.as_deref(), Some("Edited overview"));
    assert_eq!(stored.locked_fields, vec!["title", "overview"]);

    let detail = client
        .get(format!(
            "{}/emby/Items/{}",
            fixture.base_url, fixture.movie_public_id
        ))
        .query(&[("api_key", fixture.admin_key.as_str())])
        .send()
        .await?;
    assert_eq!(detail.status(), reqwest::StatusCode::OK);
    let detail: Value = detail.json().await?;
    assert_eq!(detail["Name"], "Renamed Movie");
    assert_eq!(detail["OriginalTitle"], "");
    assert_eq!(detail["Overview"], "Edited overview");
    assert!(detail.get("ProductionYear").is_none());

    let movie_nfo = tokio::fs::read_to_string(fixture.movie_directory.join("movie.nfo")).await?;
    assert!(movie_nfo.contains("<title>Renamed Movie</title>"));

    Ok(())
}

#[tokio::test]
async fn admin_edits_series_metadata_with_camel_case_body_fields()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture().await?;
    let client = test_client();

    // Emby's ASP.NET binder is case-insensitive, so clients send either spelling. Lux accepts
    // both for every editable field.
    let updated = client
        .post(format!(
            "{}/emby/Items/{}",
            fixture.base_url, fixture.series_public_id
        ))
        .query(&[("api_key", fixture.admin_key.as_str())])
        .json(&json!({
            "Id": fixture.series_public_id,
            "Type": "Series",
            "Name": "Edited Show",
            "originalTitle": "Edited Show Original",
            "overview": "Edited show overview",
            "productionYear": 2022,
            "lockedFields": ["Title", "OriginalTitle", "ProductionYear", "Genres", "SortName"],
            "Genres": ["Drama"],
            "LockData": false
        }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::NO_CONTENT);

    let stored = stored_item(&fixture.database, &fixture.series_id).await?;
    assert_eq!(stored.title, "Edited Show");
    assert_eq!(
        stored.original_title.as_deref(),
        Some("Edited Show Original")
    );
    assert_eq!(stored.overview.as_deref(), Some("Edited show overview"));
    assert_eq!(stored.production_year, Some(2022));
    assert_eq!(
        stored.locked_fields,
        vec!["title", "originalTitle", "productionYear"]
    );

    let detail = client
        .get(format!(
            "{}/Items/{}",
            fixture.base_url, fixture.series_public_id
        ))
        .query(&[("api_key", fixture.admin_key.as_str())])
        .send()
        .await?;
    assert_eq!(detail.status(), reqwest::StatusCode::OK);
    let detail: Value = detail.json().await?;
    assert_eq!(detail["Type"], "Series");
    assert_eq!(detail["Name"], "Edited Show");
    assert_eq!(detail["OriginalTitle"], "Edited Show Original");
    assert_eq!(detail["Overview"], "Edited show overview");
    assert_eq!(detail["ProductionYear"], 2022);

    let series_nfo = tokio::fs::read_to_string(fixture.series_directory.join("tvshow.nfo")).await?;
    assert!(series_nfo.contains("<title>Edited Show</title>"));
    assert!(series_nfo.contains("<originaltitle>Edited Show Original</originaltitle>"));
    assert!(series_nfo.contains("<plot>Edited show overview</plot>"));
    assert!(series_nfo.contains("<year>2022</year>"));

    Ok(())
}

#[tokio::test]
async fn item_edit_requires_server_management_permission_and_valid_identifiers()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture().await?;
    let client = test_client();
    let viewer_token = emby_token(
        &client,
        &fixture.base_url,
        "viewer",
        "viewer password",
        "viewer-device",
    )
    .await?;

    let forbidden = client
        .post(format!(
            "{}/emby/Items/{}",
            fixture.base_url, fixture.movie_public_id
        ))
        .header("X-Emby-Token", &viewer_token)
        .json(&json!({ "Name": "Viewer Edit", "Type": "Movie" }))
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        stored_item(&fixture.database, &fixture.movie_id)
            .await?
            .title,
        "Fixture Movie"
    );

    let missing_public_id = uuid::Uuid::new_v4().as_u128().to_string();
    let missing = client
        .post(format!(
            "{}/emby/Items/{missing_public_id}",
            fixture.base_url
        ))
        .query(&[("api_key", fixture.admin_key.as_str())])
        .json(&json!({ "Name": "Missing Movie", "Type": "Movie" }))
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let mismatched = client
        .post(format!(
            "{}/emby/Items/{}",
            fixture.base_url, fixture.movie_public_id
        ))
        .query(&[("api_key", fixture.admin_key.as_str())])
        .json(&json!({
            "Id": fixture.series_public_id,
            "Name": "Wrong Item",
            "Type": "Movie"
        }))
        .send()
        .await?;
    assert_eq!(mismatched.status(), reqwest::StatusCode::BAD_REQUEST);

    let blank_title = client
        .post(format!(
            "{}/emby/Items/{}",
            fixture.base_url, fixture.movie_public_id
        ))
        .query(&[("api_key", fixture.admin_key.as_str())])
        .json(&json!({ "Name": "   ", "Type": "Movie" }))
        .send()
        .await?;
    assert_eq!(blank_title.status(), reqwest::StatusCode::BAD_REQUEST);

    let invalid_year = client
        .post(format!(
            "{}/emby/Items/{}",
            fixture.base_url, fixture.movie_public_id
        ))
        .query(&[("api_key", fixture.admin_key.as_str())])
        .json(&json!({ "ProductionYear": "not a year", "Type": "Movie" }))
        .send()
        .await?;
    assert_eq!(invalid_year.status(), reqwest::StatusCode::BAD_REQUEST);

    // The person editor is unchanged: a `Person` body with a catalogue id still updates people
    // metadata instead of being treated as a media item.
    let person_public_id = "104";
    let person = client
        .post(format!(
            "{}/emby/Items/{person_public_id}",
            fixture.base_url
        ))
        .query(&[("api_key", fixture.admin_key.as_str())])
        .json(&json!({
            "Name": "Person Editor Still Works",
            "Id": person_public_id,
            "Type": "Person"
        }))
        .send()
        .await?;
    // No such person exists in this fixture, so the person editor answers 404 rather than the
    // media editor's 400/403; the important part is that it never wrote media metadata.
    assert_eq!(person.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(
        stored_item(&fixture.database, &fixture.movie_id)
            .await?
            .title,
        "Fixture Movie"
    );

    Ok(())
}

async fn start_session(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    item_id: &str,
    play_session_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let playing = client
        .post(format!("{base_url}/emby/Sessions/Playing"))
        .query(&[("api_key", token)])
        .json(&json!({
            "ItemId": item_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": 0
        }))
        .send()
        .await?;
    assert_eq!(playing.status(), reqwest::StatusCode::NO_CONTENT);
    Ok(())
}

async fn session_listing(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let sessions = client
        .get(format!("{base_url}/emby/Sessions"))
        .query(&[("api_key", token)])
        .send()
        .await?;
    assert_eq!(sessions.status(), reqwest::StatusCode::OK);
    let body: Value = sessions.json().await?;
    body.as_array()
        .cloned()
        .ok_or_else(|| "session listing is not an array".into())
}

fn session_id_for<'a>(sessions: &'a [Value], play_session_id: &str) -> Option<&'a str> {
    sessions
        .iter()
        .find(|session| session["PlaySessionId"] == play_session_id)
        .and_then(|session| session["Id"].as_str())
}

#[tokio::test]
async fn sessions_listing_and_stop_honour_server_management_permission()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture().await?;
    let client = test_client();
    let manager_token = emby_token(
        &client,
        &fixture.base_url,
        "manager",
        "manager password",
        "manager-device",
    )
    .await?;
    let viewer_token = emby_token(
        &client,
        &fixture.base_url,
        "viewer",
        "viewer password",
        "viewer-device",
    )
    .await?;

    start_session(
        &client,
        &fixture.base_url,
        &viewer_token,
        &fixture.movie_public_id,
        "viewer-play-session",
    )
    .await?;
    start_session(
        &client,
        &fixture.base_url,
        &manager_token,
        &fixture.movie_public_id,
        "manager-play-session",
    )
    .await?;

    // A `can_manage_server` user that is not an Emby administrator sees every session, exactly
    // like `/api/v1/admin/playback-sessions`.
    let manager_sessions = session_listing(&client, &fixture.base_url, &manager_token).await?;
    assert!(
        session_id_for(&manager_sessions, "viewer-play-session").is_some(),
        "a server manager must see the viewer session"
    );
    assert!(
        session_id_for(&manager_sessions, "manager-play-session").is_some(),
        "a server manager must see their own session"
    );
    let viewer_session_id = session_id_for(&manager_sessions, "viewer-play-session")
        .ok_or("missing viewer session")?
        .to_owned();
    let manager_session_id = session_id_for(&manager_sessions, "manager-play-session")
        .ok_or("missing manager session")?
        .to_owned();
    let manager_session = manager_sessions
        .iter()
        .find(|session| session["PlaySessionId"] == "manager-play-session")
        .ok_or("missing manager session")?;
    assert_eq!(manager_session["UserId"], fixture.manager.id.to_string());
    assert_eq!(manager_session["UserName"], "Manager");

    // A plain user still only sees their own session.
    let viewer_sessions = session_listing(&client, &fixture.base_url, &viewer_token).await?;
    assert!(session_id_for(&viewer_sessions, "viewer-play-session").is_some());
    assert!(session_id_for(&viewer_sessions, "manager-play-session").is_none());
    for session in &viewer_sessions {
        assert_eq!(session["UserId"], fixture.viewer.id.to_string());
    }

    // A plain user cannot stop another user's playback.
    let foreign_stop = client
        .post(format!(
            "{}/emby/Sessions/{manager_session_id}/Playing/Stop",
            fixture.base_url
        ))
        .query(&[("api_key", viewer_token.as_str())])
        .send()
        .await?;
    assert_eq!(foreign_stop.status(), reqwest::StatusCode::NOT_FOUND);
    assert!(
        session_id_for(
            &session_listing(&client, &fixture.base_url, &manager_token).await?,
            "manager-play-session"
        )
        .is_some()
    );

    // The server manager stops the viewer's session, which is the same capability the Lux admin
    // playback-session endpoint grants.
    let manager_stop = client
        .post(format!(
            "{}/emby/Sessions/{viewer_session_id}/Playing/Stop",
            fixture.base_url
        ))
        .query(&[("api_key", manager_token.as_str())])
        .send()
        .await?;
    assert_eq!(manager_stop.status(), reqwest::StatusCode::NO_CONTENT);
    let remaining = session_listing(&client, &fixture.base_url, &manager_token).await?;
    assert!(session_id_for(&remaining, "viewer-play-session").is_none());

    // A plain user still stops their own playback.
    start_session(
        &client,
        &fixture.base_url,
        &viewer_token,
        &fixture.movie_public_id,
        "viewer-second-session",
    )
    .await?;
    let second_session_id = session_id_for(
        &session_listing(&client, &fixture.base_url, &viewer_token).await?,
        "viewer-second-session",
    )
    .ok_or("missing second viewer session")?
    .to_owned();
    let own_stop = client
        .post(format!(
            "{}/emby/Sessions/{second_session_id}/Playing/Stop",
            fixture.base_url
        ))
        .query(&[("api_key", viewer_token.as_str())])
        .send()
        .await?;
    assert_eq!(own_stop.status(), reqwest::StatusCode::NO_CONTENT);

    Ok(())
}

#[tokio::test]
async fn echoed_lock_fields_keep_stored_locks_and_lock_data_locks_every_field()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture().await?;
    let client = test_client();
    let admin_token = emby_token(
        &client,
        &fixture.base_url,
        "admin",
        "correct password",
        "admin-device",
    )
    .await?;

    let locked = client
        .post(format!(
            "{}/emby/Items/{}",
            fixture.base_url, fixture.movie_public_id
        ))
        .header("X-Emby-Token", &admin_token)
        .json(&json!({
            "Id": fixture.movie_public_id,
            "Name": "Locked Movie",
            "Overview": "Locked overview",
            "LockedFields": ["Name", "Overview"]
        }))
        .send()
        .await?;
    assert_eq!(locked.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        stored_item(&fixture.database, &fixture.movie_id)
            .await?
            .locked_fields,
        vec!["title", "overview"]
    );

    // A stock Emby client echoes the DTO it read back, and Lux's own DTO serializes
    // `LockData: false` with an empty `LockedFields`. Editing only the year through that echo must
    // not unlock the fields an administrator locked in Lux.
    let echoed = client
        .post(format!(
            "{}/emby/Items/{}",
            fixture.base_url, fixture.movie_public_id
        ))
        .header("X-Emby-Token", &admin_token)
        .json(&json!({
            "Id": fixture.movie_public_id,
            "Type": "Movie",
            "Name": "Locked Movie",
            "Overview": "Locked overview",
            "ProductionYear": 2019,
            "LockData": false,
            "LockedFields": [],
            "Genres": ["Drama"]
        }))
        .send()
        .await?;
    assert_eq!(echoed.status(), reqwest::StatusCode::NO_CONTENT);
    let stored = stored_item(&fixture.database, &fixture.movie_id).await?;
    assert_eq!(stored.production_year, Some(2019));
    assert_eq!(stored.locked_fields, vec!["title", "overview"]);

    // `LockData: true` is Emby's "lock every editable field".
    let fully_locked = client
        .post(format!(
            "{}/emby/Items/{}",
            fixture.base_url, fixture.movie_public_id
        ))
        .header("X-Emby-Token", &admin_token)
        .json(&json!({
            "Name": "Fully Locked Movie",
            "LockData": true,
            "LockedFields": []
        }))
        .send()
        .await?;
    assert_eq!(fully_locked.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        stored_item(&fixture.database, &fixture.movie_id)
            .await?
            .locked_fields,
        vec!["title", "originalTitle", "overview", "productionYear"]
    );

    // Malformed lock payloads are rejected instead of silently ignored.
    for body in [
        json!({ "Name": "Bad Lock", "LockData": "yes" }),
        json!({ "Name": "Bad Lock", "LockedFields": "Name" }),
    ] {
        let rejected = client
            .post(format!(
                "{}/emby/Items/{}",
                fixture.base_url, fixture.movie_public_id
            ))
            .query(&[("api_key", fixture.admin_key.as_str())])
            .json(&body)
            .send()
            .await?;
        assert_eq!(
            rejected.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{body}"
        );
    }
    assert_eq!(
        stored_item(&fixture.database, &fixture.movie_id)
            .await?
            .title,
        "Fully Locked Movie"
    );

    Ok(())
}

#[tokio::test]
async fn person_editor_still_handles_ids_without_a_declared_type()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture().await?;
    let client = test_client();
    let viewer_token = emby_token(
        &client,
        &fixture.base_url,
        "viewer",
        "viewer password",
        "viewer-device",
    )
    .await?;

    // Non-administrators always get the media editor's 403, so a 404 proves the request stayed on
    // the person editor (where the person simply does not exist). Both the legacy numeric
    // catalogue id and a local slug must keep resolving that way without a `Type`.
    for person_id in ["104", "local-actor"] {
        let response = client
            .post(format!("{}/emby/Items/{person_id}", fixture.base_url))
            .header("X-Emby-Token", &viewer_token)
            .json(&json!({ "Name": "Legacy Person", "Id": person_id }))
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND,
            "{person_id}"
        );
    }

    // A hyphenated uuid, a 128-bit public id and a stored media item still route to the media
    // editor without a `Type`, so a plain user gets the media 403.
    let hyphenated_uuid = uuid::Uuid::new_v4().to_string();
    let public_128_bit_id = (u128::from(u64::MAX) + 7).to_string();
    for item_id in [
        hyphenated_uuid.as_str(),
        public_128_bit_id.as_str(),
        fixture.movie_public_id.as_str(),
    ] {
        let response = client
            .post(format!("{}/emby/Items/{item_id}", fixture.base_url))
            .header("X-Emby-Token", &viewer_token)
            .json(&json!({ "Name": "Media Edit" }))
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::FORBIDDEN,
            "{item_id}"
        );
    }

    // Neither the person attempts nor the rejected media attempts changed the stored movie.
    assert_eq!(
        stored_item(&fixture.database, &fixture.movie_id)
            .await?
            .title,
        "Fixture Movie"
    );

    Ok(())
}

#[tokio::test]
async fn successful_media_edit_records_the_metadata_edited_audit_event()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture().await?;
    let client = test_client();
    let admin_token = emby_token(
        &client,
        &fixture.base_url,
        "admin",
        "correct password",
        "admin-device",
    )
    .await?;

    let updated = client
        .post(format!(
            "{}/emby/Items/{}",
            fixture.base_url, fixture.movie_public_id
        ))
        .header("X-Emby-Token", &admin_token)
        .json(&json!({
            "Name": "Audited Movie",
            "Overview": "Audited overview"
        }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::NO_CONTENT);

    let audit = client
        .get(format!("{}/api/v1/admin/audit", fixture.base_url))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let audit: Value = audit.json().await?;
    let event = audit["events"]
        .as_array()
        .and_then(|events| {
            events.iter().find(|event| {
                event["eventType"] == "METADATA_EDITED" && event["targetId"] == fixture.movie_id
            })
        })
        .ok_or("missing METADATA_EDITED audit event for the edited item")?;
    assert_eq!(event["targetType"], "item");
    assert_eq!(event["actorUserId"], fixture.admin.id.to_string());
    assert_eq!(event["metadata"]["source"], "emby_item_update");

    Ok(())
}
