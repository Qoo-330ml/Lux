use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        metadata_paths::library_item_directory,
        people::{ActorCredit, PeopleService, PersonMetadata},
        scanner::LibraryScanner,
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use serde_json::json;
use tokio::net::TcpListener;

#[tokio::test]
async fn emby_persons_lists_library_actors_with_shared_admin_key()
-> Result<(), Box<dyn std::error::Error>> {
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
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Example Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2024.mkv"), b"movie").await?;
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

    let second_movie_dir = root.join("Second Movie (2023)");
    tokio::fs::create_dir_all(&second_movie_dir).await?;
    tokio::fs::write(
        second_movie_dir.join("Second.Movie.2023.mkv"),
        b"second movie",
    )
    .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let second_item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items
         WHERE library_id = ? AND item_type = 'MOVIE' AND id <> ?
         LIMIT 1",
    )
    .bind(library.id.to_string())
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    sqlx::query("UPDATE media_items SET added_at = ? WHERE id = ?")
        .bind(100_i64)
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_items SET added_at = ? WHERE id = ?")
        .bind(200_i64)
        .bind(&second_item_id)
        .execute(database.pool())
        .await?;

    PeopleService::new(config.config_dir.clone())
        .with_database(database.clone())
        .persist_item_actors(
            &item_id,
            "tmdb",
            &[
                ActorCredit {
                    id: "101".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员甲".to_owned(),
                    character: Some("角色甲".to_owned()),
                    order: Some(0),
                    profile_url: None,
                    person: None,
                },
                ActorCredit {
                    id: "102".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员乙".to_owned(),
                    character: None,
                    order: Some(1),
                    profile_url: None,
                    person: None,
                },
            ],
        )
        .await?;
    PeopleService::new(config.config_dir.clone())
        .with_database(database.clone())
        .persist_item_actors(
            &second_item_id,
            "tmdb",
            &[
                ActorCredit {
                    id: "101".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员甲".to_owned(),
                    character: Some("角色乙".to_owned()),
                    order: Some(0),
                    profile_url: None,
                    person: None,
                },
                ActorCredit {
                    id: "104".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员丁".to_owned(),
                    character: None,
                    order: Some(1),
                    profile_url: None,
                    person: Some(PersonMetadata {
                        biography: Some("演员丁简介".to_owned()),
                        birthday: None,
                        deathday: None,
                        known_for_department: None,
                        place_of_birth: None,
                    }),
                },
            ],
        )
        .await?;
    let relation_path = library_item_directory(&config.config_dir, &item_id)?.join("people.json");
    tokio::fs::write(
        &relation_path,
        serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "actors": [
                {
                    "id": "101",
                    "name": "演员甲",
                    "provider": "tmdb",
                    "character": "角色甲",
                    "order": 0
                },
                {
                    "id": null,
                    "name": "演员乙",
                    "provider": "",
                    "identities": [{"provider": "tmdb", "id": "102"}],
                    "order": 1
                },
                {
                    "id": null,
                    "name": "演员丙",
                    "provider": "",
                    "identities": [{"provider": "imdb", "id": "nm103"}],
                    "order": 2
                }
            ]
        }))?,
    )
    .await?;
    sqlx::query("DELETE FROM person_credits")
        .execute(database.pool())
        .await?;
    PeopleService::new(config.config_dir.clone())
        .with_database(database.clone())
        .rebuild_person_credit_index()
        .await?;

    let key = luxd::auth::admin_api_key::AdminApiKeyService::new(
        config.config_dir.clone(),
        database.clone(),
    )
    .rotate()
    .await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config, database, setup, web_auth, emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::new();
    let query = format!(
        "ParentId={}&PersonTypes=Actor&StartIndex=0&Limit=10&api_key={key}",
        library.id
    );

    let response = client
        .get(format!("http://{address}/Persons?{query}"))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await?;
    assert_eq!(body["TotalRecordCount"], 4);
    assert_eq!(body["StartIndex"], 0);
    let actors = body["Items"].as_array().ok_or("missing actor items")?;
    let actor_a = actors
        .iter()
        .find(|actor| actor["Id"] == "101")
        .ok_or("missing actor 101")?;
    assert_eq!(
        actors.iter().filter(|actor| actor["Id"] == "101").count(),
        1
    );
    assert_eq!(actor_a["Name"], "演员甲");
    assert!(matches!(
        actor_a["Role"].as_str(),
        Some("角色甲") | Some("角色乙")
    ));
    assert_eq!(actor_a["Type"], "Person");
    assert!(
        actor_a["ServerId"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert!(actor_a["ImageTags"].is_object());
    assert_eq!(actor_a["BackdropImageTags"], json!([]));
    let actor_b = actors
        .iter()
        .find(|actor| actor["Id"] == "102")
        .ok_or("missing actor 102")?;
    assert_eq!(actor_b["Name"], "演员乙");
    assert!(actor_b.get("Role").is_none());
    let actor_c = actors
        .iter()
        .find(|actor| actor["Id"] == "nm103")
        .ok_or("missing actor nm103")?;
    assert_eq!(actor_c["Name"], "演员丙");
    assert!(actors.iter().any(|actor| actor["Id"] == "104"));

    let full_query = format!(
        "ParentId={}&Recursive=true&PersonTypes=Actor&StartIndex=0&Limit=999999&Fields=DateCreated,Overview&SortBy=DateCreated&SortOrder=Descending&userid={}&api_key={key}",
        library.id, admin.id
    );
    let full_response = client
        .get(format!("http://{address}/emby/Persons?{full_query}&&"))
        .send()
        .await?;
    assert_eq!(full_response.status(), reqwest::StatusCode::OK);
    let full_body: serde_json::Value = full_response.json().await?;
    assert_eq!(full_body["TotalRecordCount"], 4);
    assert_eq!(full_body["Items"].as_array().map(Vec::len), Some(4));
    assert_eq!(full_body["Items"][0]["Id"], "104");
    assert!(full_body["Items"][0]["DateCreated"].is_string());
    assert!(full_body["Items"][0].get("Overview").is_some());
    assert!(full_body["Items"][0].get("Role").is_none());

    let ascending = client
        .get(format!(
            "http://{address}/Persons?ParentId={}&Recursive=true&PersonTypes=Actor&Limit=999999&SortBy=DateCreated&SortOrder=Ascending&api_key={key}",
            library.id
        ))
        .send()
        .await?;
    assert_eq!(ascending.status(), reqwest::StatusCode::OK);
    let ascending_body: serde_json::Value = ascending.json().await?;
    let ascending_items = ascending_body["Items"]
        .as_array()
        .ok_or("missing ascending items")?;
    assert_eq!(
        ascending_items.last().ok_or("empty ascending items")?["Id"],
        "104"
    );

    let prefixed = client
        .get(format!("http://{address}/emby/Persons?{query}"))
        .send()
        .await?;
    assert_eq!(prefixed.status(), reqwest::StatusCode::OK);
    assert_eq!(
        prefixed.json::<serde_json::Value>().await?["TotalRecordCount"],
        4
    );

    let non_recursive = client
        .get(format!(
            "http://{address}/Persons?ParentId={}&Recursive=false&PersonTypes=Actor&Limit=999999&api_key={key}",
            library.id
        ))
        .send()
        .await?;
    assert_eq!(non_recursive.status(), reqwest::StatusCode::OK);
    assert_eq!(
        non_recursive.json::<serde_json::Value>().await?["TotalRecordCount"],
        0
    );

    let directors = client
        .get(format!(
            "http://{address}/Persons?ParentId={}&PersonTypes=Director&api_key={key}",
            library.id
        ))
        .send()
        .await?;
    assert_eq!(directors.status(), reqwest::StatusCode::OK);
    assert_eq!(
        directors.json::<serde_json::Value>().await?["Items"],
        json!([])
    );

    server.abort();
    Ok(())
}
