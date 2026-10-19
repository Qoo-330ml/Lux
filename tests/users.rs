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
    let users_status = users.status();
    let users_body = users.text().await?;
    assert_eq!(users_status, reqwest::StatusCode::OK, "{users_body}");
    assert_eq!(
        serde_json::from_str::<Value>(&users_body)?["users"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    let preset_list = client
        .get(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(preset_list.status(), reqwest::StatusCode::OK);
    let presets = preset_list.json::<Value>().await?["presets"].clone();
    assert_eq!(presets.as_array().map(Vec::len), Some(2));
    assert!(
        presets
            .as_array()
            .is_some_and(|presets| presets.iter().any(|preset| preset["id"] == "builtin-admin"))
    );
    let preset_input = json!({
        "name": "Family",
        "description": "Family media access",
        "isAdmin": false,
        "canManageServer": false,
        "permissions": {
            "libraryAccessMode": "NONE",
            "libraryIds": [],
            "canDownload": false,
            "canPlay": true,
            "maxSimultaneousPlaybacks": 2,
            "allowRemux": true,
            "allowAudioTranscode": true,
            "allowVideoTranscode": true
        }
    });
    let missing_preset_csrf = client
        .post(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &cookie)
        .json(&preset_input)
        .send()
        .await?;
    assert_eq!(missing_preset_csrf.status(), reqwest::StatusCode::FORBIDDEN);
    let invalid_preset = client
        .post(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "name": "Invalid",
            "isAdmin": false,
            "canManageServer": false,
            "permissions": {
                "libraryAccessMode": "NONE",
                "libraryIds": [],
                "canDownload": false,
                "canPlay": true,
                "maxSimultaneousPlaybacks": -1,
                "allowRemux": true,
                "allowAudioTranscode": true,
                "allowVideoTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(invalid_preset.status(), reqwest::StatusCode::BAD_REQUEST);
    let created_preset = client
        .post(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&preset_input)
        .send()
        .await?;
    assert_eq!(created_preset.status(), reqwest::StatusCode::CREATED);
    let created_preset = created_preset.json::<Value>().await?["preset"].clone();
    assert_eq!(created_preset["version"], 1);
    assert_eq!(created_preset["isBuiltin"], false);
    let preset_id = created_preset["id"]
        .as_str()
        .ok_or("missing permission preset id")?
        .to_owned();
    let duplicate_preset = client
        .post(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "name": " family ",
            "isAdmin": false,
            "canManageServer": false,
            "permissions": preset_input["permissions"].clone()
        }))
        .send()
        .await?;
    assert_eq!(duplicate_preset.status(), reqwest::StatusCode::CONFLICT);

    let updated_preset = client
        .put(format!(
            "{base_url}/api/v1/admin/user-permission-presets/{preset_id}"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "version": 1,
            "name": "Family media",
            "description": "Updated family access",
            "isAdmin": false,
            "canManageServer": false,
            "permissions": preset_input["permissions"].clone()
        }))
        .send()
        .await?;
    assert_eq!(updated_preset.status(), reqwest::StatusCode::OK);
    assert_eq!(
        updated_preset.json::<Value>().await?["preset"]["version"],
        2
    );
    let stale_preset_update = client
        .put(format!(
            "{base_url}/api/v1/admin/user-permission-presets/{preset_id}"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "version": 1,
            "name": "Stale family",
            "isAdmin": false,
            "canManageServer": false,
            "permissions": preset_input["permissions"].clone()
        }))
        .send()
        .await?;
    assert_eq!(stale_preset_update.status(), reqwest::StatusCode::CONFLICT);

    let builtin_update = client
        .put(format!(
            "{base_url}/api/v1/admin/user-permission-presets/builtin-admin"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "version": 1,
            "name": "Admin override",
            "isAdmin": true,
            "canManageServer": true,
            "permissions": preset_input["permissions"].clone()
        }))
        .send()
        .await?;
    assert_eq!(builtin_update.status(), reqwest::StatusCode::CONFLICT);

    let builtin_delete = client
        .delete(format!(
            "{base_url}/api/v1/admin/user-permission-presets/builtin-admin?version=1"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(builtin_delete.status(), reqwest::StatusCode::CONFLICT);
    let missing_delete_version = client
        .delete(format!(
            "{base_url}/api/v1/admin/user-permission-presets/{preset_id}"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(
        missing_delete_version.status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let missing_preset = client
        .delete(format!(
            "{base_url}/api/v1/admin/user-permission-presets/missing?version=1"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(missing_preset.status(), reqwest::StatusCode::NOT_FOUND);

    let deleted_preset = client
        .delete(format!(
            "{base_url}/api/v1/admin/user-permission-presets/{preset_id}?version=2"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(deleted_preset.status(), reqwest::StatusCode::NO_CONTENT);

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

    let managed_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "managed", "password": "managed password" }))
        .send()
        .await?;
    let managed_cookie = cookie_pair(managed_login.headers());
    let managed_csrf = cookie_value(managed_login.headers(), "lux_csrf");
    let managed_presets = client
        .get(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &managed_cookie)
        .send()
        .await?;
    assert_eq!(managed_presets.status(), reqwest::StatusCode::FORBIDDEN);
    let managed_preset_create = client
        .post(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &managed_cookie)
        .header("x-csrf-token", &managed_csrf)
        .json(&preset_input)
        .send()
        .await?;
    assert_eq!(
        managed_preset_create.status(),
        reqwest::StatusCode::FORBIDDEN
    );

    let default_playback_policy = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{created_id}/playback-policy"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(default_playback_policy.status(), reqwest::StatusCode::OK);
    let default_policy = default_playback_policy.json::<Value>().await?["policy"].clone();
    assert_eq!(default_policy["canPlay"], true);
    assert_eq!(default_policy["maxSimultaneousPlaybacks"], 2);
    assert_eq!(default_policy["allowRemux"], true);
    assert_eq!(default_policy["allowAudioTranscode"], true);
    assert_eq!(default_policy["allowVideoTranscode"], true);

    let playback_policy = json!({
        "canPlay": false,
        "maxSimultaneousPlaybacks": 1,
        "allowRemux": false,
        "allowAudioTranscode": true,
        "allowVideoTranscode": false
    });
    let updated_playback_policy = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/playback-policy"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&playback_policy)
        .send()
        .await?;
    assert_eq!(updated_playback_policy.status(), reqwest::StatusCode::OK);
    assert_eq!(
        updated_playback_policy.json::<Value>().await?["policy"],
        playback_policy
    );
    let missing_csrf_update = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/playback-policy"
        ))
        .header(COOKIE, &cookie)
        .json(&playback_policy)
        .send()
        .await?;
    assert_eq!(missing_csrf_update.status(), reqwest::StatusCode::FORBIDDEN);
    let invalid_playback_policy = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/playback-policy"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "canPlay": true,
            "maxSimultaneousPlaybacks": -1,
            "allowRemux": true,
            "allowAudioTranscode": true,
            "allowVideoTranscode": true
        }))
        .send()
        .await?;
    assert_eq!(
        invalid_playback_policy.status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let unlimited_playback_policy = json!({
        "canPlay": true,
        "maxSimultaneousPlaybacks": 0,
        "allowRemux": true,
        "allowAudioTranscode": true,
        "allowVideoTranscode": true
    });
    let unlimited_update = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/playback-policy"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&unlimited_playback_policy)
        .send()
        .await?;
    assert_eq!(unlimited_update.status(), reqwest::StatusCode::OK);
    assert_eq!(
        unlimited_update.json::<Value>().await?["policy"],
        unlimited_playback_policy
    );

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
    let access_list_body = access_list.json::<Value>().await?;
    assert_eq!(
        access_list_body["libraryIds"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(access_list_body["libraryAccessMode"], "SELECTED");

    let set_none = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "libraryAccessMode": "NONE" }))
        .send()
        .await?;
    assert_eq!(set_none.status(), reqwest::StatusCode::OK);
    let set_none_body = set_none.json::<Value>().await?;
    assert_eq!(set_none_body["libraryAccessMode"], "NONE");
    assert_eq!(
        set_none_body["libraryIds"].as_array().map(Vec::len),
        Some(0)
    );

    let empty_selected = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "libraryAccessMode": "SELECTED",
            "libraryIds": []
        }))
        .send()
        .await?;
    assert_eq!(empty_selected.status(), reqwest::StatusCode::OK);
    assert_eq!(
        empty_selected.json::<Value>().await?["libraryAccessMode"],
        "NONE"
    );

    let invalid_mode = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "libraryAccessMode": "INVALID" }))
        .send()
        .await?;
    assert_eq!(invalid_mode.status(), reqwest::StatusCode::BAD_REQUEST);
    let duplicate_selection = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "libraryAccessMode": "SELECTED",
            "libraryIds": [library_id, library_id]
        }))
        .send()
        .await?;
    assert_eq!(
        duplicate_selection.status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let missing_csrf = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .json(&json!({ "libraryAccessMode": "ALL" }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let invalid_selection = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "libraryAccessMode": "SELECTED",
            "libraryIds": ["00000000-0000-0000-0000-000000000000"]
        }))
        .send()
        .await?;
    assert_eq!(invalid_selection.status(), reqwest::StatusCode::NOT_FOUND);
    let unchanged_access = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(
        unchanged_access.json::<Value>().await?["libraryAccessMode"],
        "NONE"
    );

    let selected = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "libraryAccessMode": "SELECTED",
            "libraryIds": [library_id]
        }))
        .send()
        .await?;
    assert_eq!(selected.status(), reqwest::StatusCode::OK);
    let selected_body = selected.json::<Value>().await?;
    assert_eq!(selected_body["libraryAccessMode"], "SELECTED");
    assert_eq!(
        selected_body["libraryIds"].as_array().map(Vec::len),
        Some(1)
    );

    let set_all = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{created_id}/libraries"
        ))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "libraryAccessMode": "ALL" }))
        .send()
        .await?;
    assert_eq!(set_all.status(), reqwest::StatusCode::OK);
    assert_eq!(set_all.json::<Value>().await?["libraryAccessMode"], "ALL");
    let missing_access = client
        .get(format!(
            "{base_url}/api/v1/admin/users/00000000-0000-0000-0000-000000000000/libraries"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(missing_access.status(), reqwest::StatusCode::NOT_FOUND);

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
    let regular_playback_policy = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{created_id}/playback-policy"
        ))
        .header(COOKIE, &regular_cookie)
        .send()
        .await?;
    assert_eq!(
        regular_playback_policy.status(),
        reqwest::StatusCode::FORBIDDEN
    );

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
    let deleted_regular_status = deleted_regular.status();
    let deleted_regular_body = deleted_regular.text().await?;
    assert_eq!(
        deleted_regular_status,
        reqwest::StatusCode::OK,
        "{deleted_regular_body}"
    );
    let deleted_regular_user =
        serde_json::from_str::<Value>(&deleted_regular_body)?["user"].clone();
    assert!(deleted_regular_user["deletedAt"].as_i64().is_some());
    assert!(deleted_regular_user["deletionPurgeAt"].as_i64().is_some());
    let deleted_policy_update = client
        .put(format!(
            "{base_url}/api/v1/admin/users/{regular_id}/playback-policy"
        ))
        .header(COOKIE, &manager_cookie)
        .header("x-csrf-token", &manager_csrf)
        .json(&playback_policy)
        .send()
        .await?;
    assert_eq!(
        deleted_policy_update.status(),
        reqwest::StatusCode::NOT_FOUND
    );
    let deleted_policy_read = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{regular_id}/playback-policy"
        ))
        .header(COOKIE, &manager_cookie)
        .send()
        .await?;
    assert_eq!(deleted_policy_read.status(), reqwest::StatusCode::NOT_FOUND);

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
            .any(|event| { event["eventType"] == "USER_PLAYBACK_POLICY_UPDATED" })
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

#[tokio::test]
async fn admin_user_creation_applies_configured_permission_snapshot()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup
        .complete("Admin", "Administrator", "correct password")
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
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookie = cookie_pair(login.headers());
    let csrf = cookie_value(login.headers(), "lux_csrf");
    let defaults = json!({
        "libraryAccessMode": "NONE",
        "libraryIds": [],
        "canDownload": true,
        "canPlay": false,
        "maxSimultaneousPlaybacks": 5,
        "allowRemux": false,
        "allowAudioTranscode": true,
        "allowVideoTranscode": false,
    });
    let settings = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "newUserPermissions": defaults }))
        .send()
        .await?;
    assert_eq!(settings.status(), reqwest::StatusCode::OK);

    let created = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "snapshot-user",
            "password": "snapshot password"
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let user = created.json::<Value>().await?["user"].clone();
    assert_eq!(user["canDownload"], true);
    let user_id = user["id"].as_str().ok_or("missing user id")?;

    let playback = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{user_id}/playback-policy"
        ))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(playback.status(), reqwest::StatusCode::OK);
    assert_eq!(
        playback.json::<Value>().await?["policy"],
        json!({
            "canPlay": false,
            "maxSimultaneousPlaybacks": 5,
            "allowRemux": false,
            "allowAudioTranscode": true,
            "allowVideoTranscode": false,
        })
    );

    let libraries = client
        .get(format!("{base_url}/api/v1/admin/users/{user_id}/libraries"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(libraries.status(), reqwest::StatusCode::OK);
    assert_eq!(
        libraries.json::<Value>().await?["libraryAccessMode"],
        "NONE"
    );

    sqlx::query("DROP TABLE server_settings")
        .execute(database.pool())
        .await?;
    let settings_unavailable = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "settings-unavailable",
            "password": "snapshot password"
        }))
        .send()
        .await?;
    assert_eq!(
        settings_unavailable.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    let missing_user_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE username_normalized = 'settings-unavailable'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(missing_user_count, 0);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_apply_permission_preset_to_user_with_audit_and_guards()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin_user = setup.complete("Admin", "Admin", "correct password").await?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
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
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let admin_cookie = cookie_pair(login.headers());
    let admin_csrf = cookie_value(login.headers(), "lux_csrf");
    let created_user = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "username": "preset-target", "password": "target password" }))
        .send()
        .await?;
    assert_eq!(created_user.status(), reqwest::StatusCode::CREATED);
    let target_id = created_user.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing target user ID")?
        .to_owned();

    let created_preset = client
        .post(format!("{base_url}/api/v1/admin/user-permission-presets"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "name": "Applied snapshot",
            "isAdmin": false,
            "canManageServer": false,
            "permissions": {
                "libraryAccessMode": "NONE",
                "libraryIds": [],
                "canDownload": true,
                "canPlay": true,
                "maxSimultaneousPlaybacks": 3,
                "allowRemux": false,
                "allowAudioTranscode": true,
                "allowVideoTranscode": false
            }
        }))
        .send()
        .await?;
    assert_eq!(created_preset.status(), reqwest::StatusCode::CREATED);
    let preset = created_preset.json::<Value>().await?["preset"].clone();
    let preset_id = preset["id"].as_str().ok_or("missing preset ID")?;
    let apply_url = format!(
        "{base_url}/api/v1/admin/users/{}/permission-preset",
        target_id.to_uppercase()
    );
    let request = json!({ "presetId": preset_id });

    let missing_csrf = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .json(&request)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let applied = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&request)
        .send()
        .await?;
    assert_eq!(applied.status(), reqwest::StatusCode::NO_CONTENT);

    let playback = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/playback-policy"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(playback.status(), reqwest::StatusCode::OK);
    assert_eq!(
        playback.json::<Value>().await?["policy"],
        json!({
            "canPlay": true,
            "maxSimultaneousPlaybacks": 3,
            "allowRemux": false,
            "allowAudioTranscode": true,
            "allowVideoTranscode": false
        })
    );
    let library_access = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/libraries"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(library_access.status(), reqwest::StatusCode::OK);
    assert_eq!(
        library_access.json::<Value>().await?["libraryAccessMode"],
        "NONE"
    );
    let applied_source: (String, String, i64) = sqlx::query_as(
        "SELECT preset_id, preset_name, preset_version
         FROM user_permission_preset_sources WHERE user_id = ?",
    )
    .bind(&target_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        applied_source,
        (preset_id.to_owned(), "Applied snapshot".to_owned(), 1)
    );

    let deleted_preset = client
        .delete(format!(
            "{base_url}/api/v1/admin/user-permission-presets/{preset_id}?version=1"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(deleted_preset.status(), reqwest::StatusCode::NO_CONTENT);
    let preserved_policy = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/playback-policy"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(preserved_policy.status(), reqwest::StatusCode::OK);
    assert_eq!(
        preserved_policy.json::<Value>().await?["policy"]["canPlay"],
        true
    );

    let target_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "preset-target", "password": "target password" }))
        .send()
        .await?;
    assert_eq!(target_login.status(), reqwest::StatusCode::OK);
    let forbidden = client
        .post(&apply_url)
        .header(COOKIE, cookie_pair(target_login.headers()))
        .header(
            "x-csrf-token",
            cookie_value(target_login.headers(), "lux_csrf"),
        )
        .json(&request)
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    let invalid_user = client
        .post(format!(
            "{base_url}/api/v1/admin/users/not-a-user-id/permission-preset"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&request)
        .send()
        .await?;
    assert_eq!(invalid_user.status(), reqwest::StatusCode::BAD_REQUEST);
    let missing_user = client
        .post(format!(
            "{base_url}/api/v1/admin/users/00000000-0000-4000-8000-000000000000/permission-preset"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&request)
        .send()
        .await?;
    assert_eq!(missing_user.status(), reqwest::StatusCode::NOT_FOUND);
    let missing_preset = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "presetId": "missing-preset" }))
        .send()
        .await?;
    assert_eq!(missing_preset.status(), reqwest::StatusCode::NOT_FOUND);
    let last_manager = client
        .post(format!(
            "{base_url}/api/v1/admin/users/{}/permission-preset",
            admin_user.id
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "presetId": "builtin-user" }))
        .send()
        .await?;
    assert_eq!(last_manager.status(), reqwest::StatusCode::CONFLICT);

    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit?pageSize=100"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let events = audit.json::<Value>().await?["events"]
        .as_array()
        .cloned()
        .ok_or("missing audit events")?;
    assert!(events.iter().any(|event| {
        event["eventType"] == "USER_PERMISSION_PRESET_APPLIED"
            && event["targetId"] == target_id
            && event["metadata"]["presetId"] == preset_id
            && event["metadata"]["version"] == 1
    }));

    server.abort();
    Ok(())
}
