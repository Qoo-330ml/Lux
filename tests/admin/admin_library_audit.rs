use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    storage::Database,
};
use reqwest::{
    StatusCode,
    header::{COOKIE, SET_COOKIE},
};
use serde_json::{Value, json};
use tokio::net::TcpListener;

async fn login(
    client: &reqwest::Client,
    base: &str,
    username: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({"username": username, "password": "settings-test-password"}))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = |name: &str| {
        response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find_map(|value| {
                let (pair, _) = value.split_once(';')?;
                let (key, value) = pair.split_once('=')?;
                (key == name).then(|| value.to_owned())
            })
    };
    let session = cookie("lux_session").ok_or("missing session")?;
    let csrf = cookie("lux_csrf").ok_or("missing csrf")?;
    Ok((format!("lux_session={session}; lux_csrf={csrf}"), csrf))
}

async fn audit_events(
    client: &reqwest::Client,
    base: &str,
    cookies: &str,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let audit = client
        .get(format!("{base}/api/v1/admin/audit"))
        .header(COOKIE, cookies)
        .send()
        .await?
        .json::<Value>()
        .await?;
    Ok(audit["events"]
        .as_array()
        .ok_or("missing audit events")?
        .clone())
}

#[tokio::test]
async fn library_audit_events_record_what_changed() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let users = UserStore::new(database.clone())?;
    users
        .create_initial_admin("admin", "Admin", "settings-test-password")
        .await?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        SetupService::new(database.clone())?,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database)?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder().no_proxy().build()?;
    let (cookies, csrf) = login(&client, &base, "admin").await?;

    let created = client
        .post(format!("{base}/api/v1/admin/libraries"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({"name": "Movies", "kind": "MOVIE"}))
        .send()
        .await?;
    assert_eq!(created.status(), StatusCode::CREATED);
    let library_id = created.json::<Value>().await?["library"]["id"]
        .as_str()
        .ok_or("missing library id")?
        .to_owned();

    let updated = client
        .patch(format!("{base}/api/v1/admin/libraries/{library_id}"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({"name": "Films", "isEnabled": false}))
        .send()
        .await?;
    assert_eq!(updated.status(), StatusCode::OK);

    let events = audit_events(&client, &base, &cookies).await?;
    let created_event = events
        .iter()
        .find(|event| event["eventType"] == "LIBRARY_CREATED")
        .ok_or("missing LIBRARY_CREATED")?;
    assert_eq!(created_event["metadata"]["name"], "Movies");
    assert_eq!(created_event["metadata"]["kind"], "MOVIE");
    let updated_event = events
        .iter()
        .find(|event| event["eventType"] == "LIBRARY_UPDATED")
        .ok_or("missing LIBRARY_UPDATED")?;
    assert_eq!(
        updated_event["metadata"]["fields"],
        json!(["name", "isEnabled"])
    );

    server.abort();
    Ok(())
}
