use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    storage::Database,
};
use reqwest::header::COOKIE;
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn admin_can_manage_users_and_last_manager_is_protected()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin_user = setup.complete("Admin", "Admin", "correct password").await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    sqlx::query("DROP TABLE audit_events")
        .execute(database.pool())
        .await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cookie = cookie_pair(login.headers());
    let csrf = cookie_value(login.headers(), "lux_csrf");
    let users = client
        .get(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(users.status(), reqwest::StatusCode::OK);
    assert_eq!(
        users.json::<Value>().await?["users"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    let created = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "managed",
            "displayName": "Managed",
            "password": "managed password"
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let created_id = created.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing user id")?
        .to_owned();

    let duplicate = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "MANAGED",
            "displayName": "Another Managed",
            "password": "another password"
        }))
        .send()
        .await?;
    assert_eq!(duplicate.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        duplicate.json::<Value>().await?["error"]["message"],
        "用户名或邮箱已存在"
    );

    let library = client
        .post(format!("{base_url}/api/v1/admin/libraries"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "name": "Movies", "kind": "MOVIE" }))
        .send()
        .await?;
    assert_eq!(library.status(), reqwest::StatusCode::CREATED);
    let library_id = library.json::<Value>().await?["library"]["id"]
        .as_str()
        .ok_or("missing library id")?
        .to_owned();
    let scan = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/{library_id}/scan"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(scan.status(), reqwest::StatusCode::ACCEPTED);
    let jobs = client
        .get(format!("{base_url}/api/v1/admin/jobs"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(jobs.status(), reqwest::StatusCode::OK);
    let jobs_body = jobs.json::<Value>().await?;
    assert_eq!(jobs_body["jobs"].as_array().map(Vec::len), Some(1));
    let job_id = jobs_body["jobs"][0]["id"]
        .as_str()
        .ok_or("missing job id")?;
    let job_detail = client
        .get(format!("{base_url}/api/v1/admin/jobs/{job_id}"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(job_detail.status(), reqwest::StatusCode::OK);
    assert_eq!(job_detail.json::<Value>().await?["job"]["id"], job_id);
    let access = client
        .patch(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries/{library_id}"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "canView": true }))
        .send()
        .await?;
    assert_eq!(access.status(), reqwest::StatusCode::OK);
    let access_list = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(access_list.status(), reqwest::StatusCode::OK);
    assert_eq!(
        access_list.json::<Value>().await?["libraryIds"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    let regular = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "regular",
            "password": "regular password"
        }))
        .send()
        .await?;
    assert_eq!(regular.status(), reqwest::StatusCode::CREATED);
    let regular_id = regular.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing regular user id")?
        .to_owned();
    let regular_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({
            "username": "regular",
            "password": "regular password"
        }))
        .send()
        .await?;
    assert_eq!(regular_login.status(), reqwest::StatusCode::OK);
    let regular_cookie = cookie_pair(regular_login.headers());
    let regular_users = client
        .get(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &regular_cookie)
        .send()
        .await?;
    assert_eq!(regular_users.status(), reqwest::StatusCode::FORBIDDEN);

    let updated = client
        .patch(format!("{base_url}/api/v1/admin/users/{created_id}"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "displayName": "Managed Updated",
            "password": "new managed password",
            "isAdmin": true,
            "canManageServer": true,
            "canRemoteAccess": true,
            "canDownload": true
        }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    let updated_user = updated.json::<Value>().await?["user"].clone();
    assert_eq!(updated_user["displayName"], "Managed Updated");
    assert_eq!(updated_user["canDownload"], true);
    assert_eq!(updated_user["canRemoteAccess"], true);

    let managed_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({
            "username": "managed",
            "password": "new managed password"
        }))
        .send()
        .await?;
    assert_eq!(managed_login.status(), reqwest::StatusCode::OK);
    let manager_cookie = cookie_pair(managed_login.headers());
    let manager_csrf = cookie_value(managed_login.headers(), "lux_csrf");

    let disabled_admin = client
        .patch(format!("{base_url}/api/v1/admin/users/{}", admin_user.id))
        .header(COOKIE, &manager_cookie)
        .header("x-csrf-token", &manager_csrf)
        .json(&json!({ "isDisabled": true }))
        .send()
        .await?;
    assert_eq!(disabled_admin.status(), reqwest::StatusCode::OK);

    let listed_after_disable = client
        .get(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &manager_cookie)
        .send()
        .await?;
    assert_eq!(listed_after_disable.status(), reqwest::StatusCode::OK);
    assert!(
        listed_after_disable.json::<Value>().await?["users"]
            .as_array()
            .is_some_and(|users| {
                users.iter().any(|user| {
                    user["id"] == admin_user.id.to_string() && user["isDisabled"] == true
                })
            })
    );

    let disabled_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({
            "username": "admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(disabled_login.status(), reqwest::StatusCode::UNAUTHORIZED);

    let admin_demotion = client
        .patch(format!("{base_url}/api/v1/admin/users/{created_id}"))
        .header(COOKIE, &manager_cookie)
        .header("x-csrf-token", &manager_csrf)
        .json(&json!({ "canManageServer": false }))
        .send()
        .await?;
    assert_eq!(admin_demotion.status(), reqwest::StatusCode::CONFLICT);

    let deleted_regular = client
        .delete(format!("{base_url}/api/v1/admin/users/{regular_id}"))
        .header(COOKIE, &manager_cookie)
        .header("x-csrf-token", &manager_csrf)
        .send()
        .await?;
    assert_eq!(deleted_regular.status(), reqwest::StatusCode::OK);
    let deleted_regular_user = deleted_regular.json::<Value>().await?["user"].clone();
    assert!(deleted_regular_user["deletedAt"].as_i64().is_some());
    assert!(deleted_regular_user["deletionPurgeAt"].as_i64().is_some());

    let reserved_username = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &manager_cookie)
        .header("x-csrf-token", &manager_csrf)
        .json(&json!({
            "username": "regular",
            "password": "regular password"
        }))
        .send()
        .await?;
    assert_eq!(reserved_username.status(), reqwest::StatusCode::CONFLICT);

    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit?pageSize=100"))
        .header(COOKIE, &manager_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let audit_events = audit.json::<Value>().await?["events"]
        .as_array()
        .cloned()
        .ok_or("missing audit events")?;
    assert!(
        audit_events
            .iter()
            .any(|event| event["eventType"] == "USER_CREATED")
    );
    assert!(
        audit_events
            .iter()
            .any(|event| event["eventType"] == "USER_UPDATED")
    );
    assert!(
        audit_events
            .iter()
            .any(|event| event["eventType"] == "USER_DISABLED")
    );
    assert!(
        audit_events
            .iter()
            .any(|event| event["eventType"] == "USER_DELETED")
    );

    let compatibility_logs = client
        .get(format!("{base_url}/api/v1/admin/logs?pageSize=100"))
        .header(COOKIE, &manager_cookie)
        .send()
        .await?;
    assert_eq!(compatibility_logs.status(), reqwest::StatusCode::OK);
    let compatibility_events = compatibility_logs.json::<Value>().await?["events"]
        .as_array()
        .cloned()
        .ok_or("missing compatibility audit events")?;
    assert!(
        compatibility_events
            .iter()
            .any(|event| event["eventType"] == "USER_CREATED")
    );
    assert!(compatibility_events.windows(2).all(|pair| {
        let previous_time = pair[0]["createdAt"].as_i64().unwrap_or_default();
        let next_time = pair[1]["createdAt"].as_i64().unwrap_or_default();
        previous_time > next_time
            || (previous_time == next_time
                && pair[0]["id"].as_str().unwrap_or_default()
                    >= pair[1]["id"].as_str().unwrap_or_default())
    }));
    let mut page_ids = Vec::new();
    for page in [1, 2] {
        let response = client
            .get(format!(
                "{base_url}/api/v1/admin/audit?page={page}&pageSize=1"
            ))
            .header(COOKIE, &manager_cookie)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let body: Value = response.json().await?;
        let events = body["events"].as_array().ok_or("missing audit page")?;
        assert_eq!(events.len(), 1);
        page_ids.push(
            events[0]["id"]
                .as_str()
                .ok_or("missing audit event ID")?
                .to_owned(),
        );
    }
    assert_ne!(page_ids[0], page_ids[1]);

    server.abort();
    Ok(())
}

fn cookie_pair(headers: &reqwest::header::HeaderMap) -> String {
    format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(headers, "lux_session"),
        cookie_value(headers, "lux_csrf")
    )
}

fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> String {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            value
                .strip_prefix(&format!("{name}="))
                .and_then(|value| value.split(';').next())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}
