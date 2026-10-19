use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
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
    let applied_source: (String, String, i64, i64) = sqlx::query_as(
        "SELECT preset_id, preset_name, preset_version, applied_at
         FROM user_permission_preset_sources WHERE user_id = ?",
    )
    .bind(&target_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(applied_source.0, preset_id);
    assert_eq!(applied_source.1, "Applied snapshot");
    assert_eq!(applied_source.2, 1);

    let expected_source_details = json!({
        "id": preset_id,
        "name": "Applied snapshot",
        "version": 1,
        "appliedAt": applied_source.3,
    });
    let list = client
        .get(format!("{base_url}/api/v1/admin/users?page=1&pageSize=100"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(list.status(), reqwest::StatusCode::OK);
    let listed_users = list.json::<Value>().await?["users"]
        .as_array()
        .cloned()
        .ok_or("missing users list")?;
    let listed_target = listed_users
        .iter()
        .find(|user| user["id"] == target_id)
        .ok_or("missing target user in admin list")?;
    assert_eq!(listed_target["presetSource"], "Applied snapshot");
    assert_eq!(
        listed_target["presetSourceDetails"],
        expected_source_details
    );
    let listed_admin = listed_users
        .iter()
        .find(|user| user["id"] == admin_user.id.to_string())
        .ok_or("missing administrator in admin list")?;
    assert!(listed_admin["presetSource"].is_null());
    assert!(listed_admin["presetSourceDetails"].is_null());

    let profile = client
        .get(format!("{base_url}/api/v1/admin/users/{target_id}"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(profile.status(), reqwest::StatusCode::OK);
    let profile_user = profile.json::<Value>().await?["user"].clone();
    assert_eq!(profile_user["presetSource"], "Applied snapshot");
    assert_eq!(profile_user["presetSourceDetails"], expected_source_details);

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
    let profile_after_deleting_preset = client
        .get(format!("{base_url}/api/v1/admin/users/{target_id}"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(
        profile_after_deleting_preset.status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        profile_after_deleting_preset.json::<Value>().await?["user"]["presetSourceDetails"],
        expected_source_details
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

#[tokio::test]
async fn admin_can_batch_apply_permission_presets_with_per_user_results()
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
    let batch_url = format!("{base_url}/api/v1/admin/users/batch/permission-preset");

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let admin_cookie = cookie_pair(login.headers());
    let admin_csrf = cookie_value(login.headers(), "lux_csrf");

    let unauthenticated = client
        .post(&batch_url)
        .json(&json!({ "userIds": [], "presetId": "builtin-user" }))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);
    let missing_csrf = client
        .post(&batch_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({ "userIds": [], "presetId": "builtin-user" }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let create_user = |username: &'static str| {
        client
            .post(format!("{base_url}/api/v1/admin/users"))
            .header(COOKIE, &admin_cookie)
            .header("x-csrf-token", &admin_csrf)
            .json(&json!({ "username": username, "password": "target password" }))
    };
    let failed_target = create_user("batch-failed-target")
        .send()
        .await?
        .json::<Value>()
        .await?["user"]["id"]
        .as_str()
        .ok_or("missing failed target ID")?
        .to_owned();
    let successful_target = create_user("batch-success-target")
        .send()
        .await?
        .json::<Value>()
        .await?["user"]["id"]
        .as_str()
        .ok_or("missing successful target ID")?
        .to_owned();
    sqlx::query(
        "CREATE TRIGGER fail_batch_permission_preset_source
         BEFORE INSERT ON user_permission_preset_sources
         WHEN NEW.user_id = (
             SELECT id FROM users WHERE username_normalized = 'batch-failed-target'
         )
         BEGIN SELECT RAISE(ABORT, 'injected batch target failure'); END",
    )
    .execute(database.pool())
    .await?;

    let missing_user_id = "00000000-0000-4000-8000-000000000000";
    let body = json!({
        "userIds": [
            "not-a-user-id",
            missing_user_id,
            failed_target,
            successful_target,
            admin_user.id.to_string()
        ],
        "presetId": "builtin-user"
    });
    let applied = client
        .post(&batch_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&body)
        .send()
        .await?;
    let applied_status = applied.status();
    let applied_body = applied.text().await?;
    assert_eq!(applied_status, reqwest::StatusCode::OK, "{applied_body}");
    assert!(!applied_body.to_ascii_lowercase().contains("cookie"));
    assert!(!applied_body.to_ascii_lowercase().contains("token"));
    let response = serde_json::from_str::<Value>(&applied_body)?;
    let results = response["results"].as_array().ok_or("missing results")?;
    assert_eq!(results.len(), 5);
    assert_eq!(results[0]["userId"], "not-a-user-id");
    assert_eq!(results[0]["success"], false);
    assert_eq!(results[0]["errorCode"], "INVALID_USER_ID");
    assert_eq!(results[1]["userId"], missing_user_id);
    assert_eq!(results[1]["errorCode"], "USER_NOT_FOUND");
    assert_eq!(results[2]["userId"], failed_target);
    assert_eq!(results[2]["errorCode"], "STORAGE_UNAVAILABLE");
    assert_eq!(results[3]["userId"], successful_target);
    assert_eq!(results[3]["success"], true);
    assert!(results[3]["errorCode"].is_null());
    assert_eq!(results[4]["userId"], admin_user.id.to_string());
    assert_eq!(results[4]["errorCode"], "LAST_MANAGER");
    assert_eq!(response["preset"]["id"], "builtin-user");

    let applied_source_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_permission_preset_sources WHERE user_id = ?")
            .bind(&successful_target)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(applied_source_count, 1);
    let failed_source_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_permission_preset_sources WHERE user_id = ?")
            .bind(&failed_target)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(failed_source_count, 0);

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
    assert_eq!(
        events
            .iter()
            .filter(|event| {
                event["eventType"] == "USER_PERMISSION_PRESET_APPLIED"
                    && event["targetId"] == successful_target
            })
            .count(),
        1
    );
    assert!(!events.iter().any(|event| {
        event["eventType"] == "USER_PERMISSION_PRESET_APPLIED"
            && (event["targetId"] == failed_target
                || event["targetId"] == admin_user.id.to_string())
    }));

    for invalid_body in [
        json!({ "userIds": [], "presetId": "builtin-user" }),
        json!({ "userIds": [successful_target, successful_target], "presetId": "builtin-user" }),
        json!({
            "userIds": (0..101).map(|index| format!("invalid-{index}")).collect::<Vec<_>>(),
            "presetId": "builtin-user"
        }),
    ] {
        let invalid = client
            .post(&batch_url)
            .header(COOKIE, &admin_cookie)
            .header("x-csrf-token", &admin_csrf)
            .json(&invalid_body)
            .send()
            .await?;
        assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
    }
    let unknown_preset = client
        .post(&batch_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({ "userIds": [successful_target], "presetId": "missing-preset" }))
        .send()
        .await?;
    assert_eq!(unknown_preset.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_preview_and_copy_account_preferences() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
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
    let preview_url = format!("{base_url}/api/v1/admin/users/account-preferences/copy/preview");

    let preview_request = json!({
        "sourceUserId": "00000000-0000-4000-8000-000000000001",
        "targetUserIds": ["00000000-0000-4000-8000-000000000002"],
        "groups": { "appearance": true, "home": false, "playback": false }
    });
    let unauthenticated = client
        .post(&preview_url)
        .json(&preview_request)
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let admin_cookie = cookie_pair(login.headers());
    let admin_csrf = cookie_value(login.headers(), "lux_csrf");
    let create_user = |username: &'static str| {
        client
            .post(format!("{base_url}/api/v1/admin/users"))
            .header(COOKIE, &admin_cookie)
            .header("x-csrf-token", &admin_csrf)
            .json(&json!({ "username": username, "password": "target password" }))
    };
    let source = create_user("preferences-copy-source")
        .send()
        .await?
        .json::<Value>()
        .await?["user"]["id"]
        .as_str()
        .ok_or("missing source user ID")?
        .to_owned();
    let target = create_user("preferences-copy-target")
        .send()
        .await?
        .json::<Value>()
        .await?["user"]["id"]
        .as_str()
        .ok_or("missing target user ID")?
        .to_owned();
    let conflict_target = create_user("preferences-copy-conflict")
        .send()
        .await?
        .json::<Value>()
        .await?["user"]["id"]
        .as_str()
        .ok_or("missing conflict target ID")?
        .to_owned();
    create_user("preferences-copy-non-admin").send().await?;
    sqlx::query(
        "INSERT INTO user_account_preferences
         (user_id, theme, accent_color, show_media_libraries, show_continue_watching,
          audio_language, subtitle_language, auto_play_next_episode, version)
         VALUES (?, 'light', 'berry', 0, 1, 'Japanese', 'English', 0, 3),
                (?, 'dark', 'ocean', 1, 0, 'Chinese', 'Chinese', 1, 4),
                (?, 'dark', 'mint', 1, 1, 'French', 'French', 0, 2)",
    )
    .bind(&source)
    .bind(&target)
    .bind(&conflict_target)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind)
         VALUES ('copy-library-shared', 'Shared Library', 'MOVIE'),
                ('copy-library-private', 'Private Library', 'MOVIE')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE users SET library_access_mode = 'SELECTED' WHERE id = ?")
        .bind(&target)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE users SET library_access_mode = 'ALL' WHERE id = ?")
        .bind(&conflict_target)
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, 'copy-library-shared', 1)",
    )
    .bind(&target)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_library_order (user_id, library_id, position)
         VALUES (?, 'copy-library-shared', 0), (?, 'copy-library-private', 1),
                (?, 'copy-library-private', 0)",
    )
    .bind(&source)
    .bind(&source)
    .bind(&target)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_playback_settings (user_id, played_percent)
         VALUES (?, 67), (?, 42)",
    )
    .bind(&source)
    .bind(&target)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_library_order_preferences (user_id, use_admin_library_order)
         VALUES (?, 1)",
    )
    .bind(&target)
    .execute(database.pool())
    .await?;

    let unavailable_target = client
        .post(&preview_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({
            "sourceUserId": source,
            "targetUserIds": ["00000000-0000-4000-8000-000000000099"],
            "groups": { "appearance": true, "home": false, "playback": false }
        }))
        .send()
        .await?;
    assert_eq!(unavailable_target.status(), reqwest::StatusCode::NOT_FOUND);

    let non_admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({
            "username": "preferences-copy-non-admin",
            "password": "target password"
        }))
        .send()
        .await?;
    assert_eq!(non_admin_login.status(), reqwest::StatusCode::OK);
    let non_admin = client
        .post(&preview_url)
        .header(COOKIE, cookie_pair(non_admin_login.headers()))
        .json(&json!({
            "sourceUserId": source,
            "targetUserIds": [target],
            "groups": { "appearance": true, "home": false, "playback": false }
        }))
        .send()
        .await?;
    assert_eq!(non_admin.status(), reqwest::StatusCode::FORBIDDEN);

    let empty_groups = client
        .post(&preview_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({
            "sourceUserId": source,
            "targetUserIds": [target],
            "groups": { "appearance": false, "home": false, "playback": false }
        }))
        .send()
        .await?;
    assert_eq!(empty_groups.status(), reqwest::StatusCode::BAD_REQUEST);

    let duplicate_targets = client
        .post(&preview_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({
            "sourceUserId": source,
            "targetUserIds": [target, target],
            "groups": { "appearance": true, "home": false, "playback": false }
        }))
        .send()
        .await?;
    assert_eq!(duplicate_targets.status(), reqwest::StatusCode::BAD_REQUEST);

    let invalid_user_id = client
        .post(&preview_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({
            "sourceUserId": "not-a-user-id",
            "targetUserIds": [target],
            "groups": { "appearance": true, "home": false, "playback": false }
        }))
        .send()
        .await?;
    assert_eq!(invalid_user_id.status(), reqwest::StatusCode::BAD_REQUEST);

    let preview = client
        .post(&preview_url)
        .header(COOKIE, &admin_cookie)
        .json(&json!({
            "sourceUserId": source,
            "targetUserIds": [target, conflict_target],
            "groups": { "appearance": true, "home": false, "playback": true, "libraryOrder": true }
        }))
        .send()
        .await?;
    let preview_status = preview.status();
    let preview_body = preview.text().await?;
    assert_eq!(preview_status, reqwest::StatusCode::OK, "{preview_body}");
    let preview = serde_json::from_str::<Value>(&preview_body)?;
    assert_eq!(preview["sourceVersion"], 3);
    assert_eq!(preview["sourceSnapshot"]["appearance"]["theme"], "light");
    assert!(preview["sourceSnapshot"].get("home").is_none());
    assert_eq!(preview["sourceSnapshot"]["playback"]["playedPercent"], 67);
    assert_eq!(
        preview["sourceSnapshot"]["libraryOrder"],
        json!(["copy-library-shared", "copy-library-private"])
    );
    assert_eq!(preview["targets"][0]["expectedVersion"], 4);
    assert_eq!(preview["targets"][1]["expectedVersion"], 2);
    assert_eq!(
        preview["targets"][0]["libraryOrder"],
        json!(["copy-library-shared"])
    );
    assert_eq!(
        preview["targets"][1]["libraryOrder"],
        json!(["copy-library-shared", "copy-library-private"])
    );
    sqlx::query("UPDATE user_account_preferences SET version = version + 1 WHERE user_id = ?")
        .bind(&conflict_target)
        .execute(database.pool())
        .await?;

    let apply_url = format!("{base_url}/api/v1/admin/users/account-preferences/copy");
    let apply_targets = preview["targets"]
        .as_array()
        .ok_or("missing preview targets")?
        .iter()
        .map(|target| {
            json!({
                "userId": target["userId"],
                "expectedVersion": target["expectedVersion"]
            })
        })
        .collect::<Vec<_>>();
    let apply_request = json!({
        "sourceUserId": source,
        "sourceExpectedVersion": preview["sourceVersion"],
        "targets": apply_targets,
        "groups": { "appearance": true, "home": false, "playback": true, "libraryOrder": true }
    });
    let missing_csrf = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .json(&apply_request)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let applied = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&apply_request)
        .send()
        .await?;
    let applied_status = applied.status();
    let applied_body = applied.text().await?;
    assert_eq!(applied_status, reqwest::StatusCode::OK, "{applied_body}");
    let applied = serde_json::from_str::<Value>(&applied_body)?;
    assert_eq!(applied["results"][0]["success"], true);
    assert_eq!(applied["results"][1]["success"], false);
    assert_eq!(applied["results"][1]["errorCode"], "VERSION_CONFLICT");
    assert!(!applied_body.to_ascii_lowercase().contains("cookie"));
    assert!(!applied_body.to_ascii_lowercase().contains("token"));

    let stored: (String, String, i64, String) = sqlx::query_as(
        "SELECT theme, accent_color, show_media_libraries, audio_language
         FROM user_account_preferences WHERE user_id = ?",
    )
    .bind(&target)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored.0, "light");
    assert_eq!(stored.1, "berry");
    assert_eq!(stored.2, 1);
    assert_eq!(stored.3, "Japanese");
    let copied_library_order: Vec<String> = sqlx::query_scalar(
        "SELECT library_id FROM user_library_order WHERE user_id = ? ORDER BY position",
    )
    .bind(&target)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(copied_library_order, ["copy-library-shared"]);
    let copied_played_percent: i64 =
        sqlx::query_scalar("SELECT played_percent FROM user_playback_settings WHERE user_id = ?")
            .bind(&target)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(copied_played_percent, 67);
    let use_admin_library_order: i64 = sqlx::query_scalar(
        "SELECT use_admin_library_order FROM user_library_order_preferences WHERE user_id = ?",
    )
    .bind(&target)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(use_admin_library_order, 1);

    sqlx::query("UPDATE user_account_preferences SET version = version + 1 WHERE user_id = ?")
        .bind(&source)
        .execute(database.pool())
        .await?;
    let stale_source = client
        .post(&apply_url)
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&apply_request)
        .send()
        .await?;
    assert_eq!(stale_source.status(), reqwest::StatusCode::CONFLICT);

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
    let copy_event = events
        .iter()
        .find(|event| {
            event["eventType"] == "USER_ACCOUNT_PREFERENCES_COPIED" && event["targetId"] == target
        })
        .ok_or("missing account preferences copy audit event")?;
    assert_eq!(copy_event["metadata"]["sourceUserId"], source);
    assert_eq!(copy_event["metadata"]["groups"]["appearance"], true);
    assert_eq!(copy_event["metadata"]["groups"]["home"], false);
    assert_eq!(copy_event["metadata"]["groups"]["playback"], true);
    assert_eq!(copy_event["metadata"]["groups"]["libraryOrder"], true);
    assert!(!copy_event["metadata"].to_string().contains("light"));
    assert!(
        !copy_event["metadata"]
            .to_string()
            .to_ascii_lowercase()
            .contains("token")
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_user_profile_api_normalizes_email_and_preserves_verification_state()
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

    let legacy_profile = client
        .get(format!("{base_url}/api/v1/admin/users/{}", admin_user.id))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(legacy_profile.status(), reqwest::StatusCode::OK);
    let legacy_user = legacy_profile.json::<Value>().await?["user"].clone();
    assert_eq!(legacy_user["email"], Value::Null);
    assert_eq!(legacy_user["emailVerifiedAt"], Value::Null);
    assert_eq!(legacy_user["isDisabled"], false);

    let created = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "profile-managed",
            "displayName": "Profile Managed",
            "password": "managed password",
            "email": " Managed@Example.COM "
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let user_id = created.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing created user id")?
        .to_owned();
    let profile_url = format!("{base_url}/api/v1/admin/users/{user_id}/profile");

    let initial_profile = client
        .get(format!("{base_url}/api/v1/admin/users/{user_id}"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(initial_profile.status(), reqwest::StatusCode::OK);
    let initial_user = initial_profile.json::<Value>().await?["user"].clone();
    assert_eq!(initial_user["email"], "managed@example.com");
    assert_eq!(initial_user["emailVerifiedAt"], Value::Null);

    sqlx::query("UPDATE users SET email_verified_at = 123 WHERE id = ?")
        .bind(&user_id)
        .execute(database.pool())
        .await?;
    let missing_csrf = client
        .patch(&profile_url)
        .header(COOKIE, &cookie)
        .json(&json!({ "displayName": "Must Not Save" }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let display_update = client
        .patch(&profile_url)
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "displayName": "Managed Display" }))
        .send()
        .await?;
    assert_eq!(display_update.status(), reqwest::StatusCode::OK);
    let display_user = display_update.json::<Value>().await?["user"].clone();
    assert_eq!(display_user["email"], "managed@example.com");
    assert_eq!(display_user["emailVerifiedAt"], 123);

    let email_update = client
        .patch(&profile_url)
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "email": " New@Example.COM " }))
        .send()
        .await?;
    assert_eq!(email_update.status(), reqwest::StatusCode::OK);
    let updated_user = email_update.json::<Value>().await?["user"].clone();
    assert_eq!(updated_user["email"], "new@example.com");
    assert_eq!(updated_user["emailVerifiedAt"], Value::Null);

    sqlx::query("UPDATE users SET email_verified_at = 456 WHERE id = ?")
        .bind(&user_id)
        .execute(database.pool())
        .await?;
    let same_email_update = client
        .patch(&profile_url)
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "email": "NEW@example.com" }))
        .send()
        .await?;
    assert_eq!(same_email_update.status(), reqwest::StatusCode::OK);
    assert_eq!(
        same_email_update.json::<Value>().await?["user"]["emailVerifiedAt"],
        456
    );

    let duplicate_email = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({
            "username": "profile-duplicate",
            "displayName": "Duplicate Email",
            "password": "duplicate password",
            "email": "new@EXAMPLE.com"
        }))
        .send()
        .await?;
    assert_eq!(duplicate_email.status(), reqwest::StatusCode::CONFLICT);

    let invalid_email = client
        .patch(&profile_url)
        .header(COOKIE, &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "email": "not-an-email" }))
        .send()
        .await?;
    assert_eq!(invalid_email.status(), reqwest::StatusCode::BAD_REQUEST);
    let invalid_body = invalid_email.text().await?;
    assert!(!invalid_body.contains("sqlx"));
    assert!(!invalid_body.contains("database"));
    let unchanged_profile = client
        .get(format!("{base_url}/api/v1/admin/users/{user_id}"))
        .header(COOKIE, &cookie)
        .send()
        .await?;
    let unchanged_user = unchanged_profile.json::<Value>().await?["user"].clone();
    assert_eq!(unchanged_user["email"], "new@example.com");
    assert_eq!(unchanged_user["emailVerifiedAt"], 456);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_read_content_restriction_index_readiness()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "admin password").await?;
    let users = UserStore::new(database.clone())?;
    users
        .create_user("reader", "Reader", "reader password", false)
        .await?;
    let library = LibraryService::new(database.clone())
        .create_library("Restriction Readiness", LibraryKind::Movie, false)
        .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('restriction-readiness-item', ?, 'MOVIE', 'Readiness', 'Readiness', 'LOCAL_CONFIRMED')",
    )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config, database, setup, web_auth, emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let url = format!("{base_url}/api/v1/admin/content-restriction-index");

    let anonymous = client.get(&url).send().await?;
    assert_eq!(anonymous.status(), reqwest::StatusCode::UNAUTHORIZED);

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "admin password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_response = client.get(&url).header(COOKIE, admin_cookie).send().await?;
    assert_eq!(admin_response.status(), reqwest::StatusCode::OK);
    let readiness = admin_response.json::<Value>().await?;
    assert_eq!(readiness["totalItems"], 1);
    assert_eq!(readiness["indexedItems"], 0);
    assert_eq!(readiness["pendingItems"], 1);
    assert_eq!(readiness.as_object().map(|body| body.len()), Some(3));

    let reader_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "reader", "password": "reader password" }))
        .send()
        .await?;
    let reader_cookie = cookie_pair(reader_login.headers());
    let reader_response = client
        .get(&url)
        .header(COOKIE, reader_cookie)
        .send()
        .await?;
    assert_eq!(reader_response.status(), reqwest::StatusCode::FORBIDDEN);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_list_user_web_sessions_without_exposing_tokens()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "admin password").await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config, database, setup, web_auth, emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let unauthenticated = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{}/sessions",
            admin.id
        ))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "admin password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    let created = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "username": "session-target",
            "password": "target password"
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let target_id = created.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing target user id")?
        .to_owned();
    let first_target_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "session-target", "password": "target password" }))
        .send()
        .await?;
    let _second_target_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "session-target", "password": "target password" }))
        .send()
        .await?;
    let target_cookie = cookie_pair(first_target_login.headers());

    let denied = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions"
        ))
        .header(COOKIE, &target_cookie)
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);

    let first_page = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions?page=1&pageSize=1"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(first_page.status(), reqwest::StatusCode::OK);
    let first_body = first_page.json::<Value>().await?;
    assert_eq!(first_body["page"], 1);
    assert_eq!(first_body["pageSize"], 1);
    assert_eq!(first_body["totalCount"], 2);
    assert_eq!(first_body["sessions"].as_array().map(Vec::len), Some(1));
    let first_id = first_body["sessions"][0]["id"]
        .as_str()
        .ok_or("missing first id")?;
    assert!(first_body["sessions"][0]["createdAt"].as_i64().is_some());
    assert!(first_body["sessions"][0].get("sessionToken").is_none());
    assert!(first_body["sessions"][0].get("csrfToken").is_none());

    let second_page = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions?page=2&pageSize=1"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(second_page.status(), reqwest::StatusCode::OK);
    let second_body = second_page.json::<Value>().await?;
    assert_eq!(second_body["totalCount"], 2);
    assert_ne!(
        first_id,
        second_body["sessions"][0]["id"]
            .as_str()
            .unwrap_or_default()
    );

    let capped_page = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions?page=1&pageSize=500"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(capped_page.status(), reqwest::StatusCode::OK);
    let capped_body = capped_page.json::<Value>().await?;
    assert_eq!(capped_body["pageSize"], 100);
    assert_eq!(capped_body["sessions"].as_array().map(Vec::len), Some(2));

    let admin_sessions = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{}/sessions",
            admin.id
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(admin_sessions.status(), reqwest::StatusCode::OK);
    assert_eq!(
        admin_sessions.json::<Value>().await?["sessions"][0]["isCurrent"],
        true
    );

    let invalid_id = client
        .get(format!(
            "{base_url}/api/v1/admin/users/not-a-user-id/sessions"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(invalid_id.status(), reqwest::StatusCode::BAD_REQUEST);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_revoke_user_web_sessions_without_affecting_other_sessions()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "admin password").await?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let admin_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "admin password" }))
        .send()
        .await?;
    let admin_cookie = cookie_pair(admin_login.headers());
    let admin_csrf = cookie_value(admin_login.headers(), "lux_csrf");
    let admin_sessions = client
        .get(format!(
            "{base_url}/api/v1/admin/users/{}/sessions",
            admin.id
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?
        .json::<Value>()
        .await?;
    let admin_session_id = admin_sessions["sessions"]
        .as_array()
        .and_then(|sessions| sessions.iter().find(|session| session["isCurrent"] == true))
        .and_then(|session| session["id"].as_str())
        .ok_or("missing current admin session")?
        .to_owned();

    let created = client
        .post(format!("{base_url}/api/v1/admin/users"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .json(&json!({
            "username": "revoke-target",
            "password": "target password"
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let target_id = created.json::<Value>().await?["user"]["id"]
        .as_str()
        .ok_or("missing target user id")?
        .to_owned();

    let mut target_cookies = Vec::new();
    for _ in 0..3 {
        let login = client
            .post(format!("{base_url}/api/v1/auth/login"))
            .json(&json!({ "username": "revoke-target", "password": "target password" }))
            .send()
            .await?;
        assert_eq!(login.status(), reqwest::StatusCode::OK);
        target_cookies.push(cookie_pair(login.headers()));
    }

    let mut target_session_ids = Vec::new();
    for target_cookie in &target_cookies {
        let sessions = client
            .get(format!("{base_url}/api/v1/auth/sessions"))
            .header(COOKIE, target_cookie)
            .send()
            .await?;
        assert_eq!(sessions.status(), reqwest::StatusCode::OK);
        let sessions_body = sessions.json::<Value>().await?;
        let current_id = sessions_body["sessions"]
            .as_array()
            .and_then(|sessions| sessions.iter().find(|session| session["isCurrent"] == true))
            .and_then(|session| session["id"].as_str())
            .ok_or("missing current target session")?;
        target_session_ids.push(current_id.to_owned());
    }

    let list_url = format!("{base_url}/api/v1/admin/users/{target_id}/sessions?pageSize=100");
    let active = client
        .get(&list_url)
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(active.status(), reqwest::StatusCode::OK);
    let active_sessions = active.json::<Value>().await?["sessions"]
        .as_array()
        .cloned()
        .ok_or("missing active target sessions")?;
    assert_eq!(active_sessions.len(), 3);
    let expired_session_id = &target_session_ids[0];
    let revoke_session_id = &target_session_ids[1];

    let unauthenticated = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions/{revoke_session_id}"
        ))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let missing_csrf = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions/{revoke_session_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let wrong_owner = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions/{admin_session_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(wrong_owner.status(), reqwest::StatusCode::NOT_FOUND);

    sqlx::query("UPDATE web_sessions SET expires_at = unixepoch() - 1 WHERE id = ?")
        .bind(expired_session_id)
        .execute(database.pool())
        .await?;
    let expired = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions/{expired_session_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(expired.status(), reqwest::StatusCode::NOT_FOUND);

    let current_admin = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{}/sessions/{admin_session_id}",
            admin.id
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(current_admin.status(), reqwest::StatusCode::BAD_REQUEST);
    let admin_still_authenticated = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(admin_still_authenticated.status(), reqwest::StatusCode::OK);

    let revoked = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions/{revoke_session_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(revoked.status(), reqwest::StatusCode::NO_CONTENT);
    let revoked_login = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &target_cookies[1])
        .send()
        .await?;
    assert_eq!(revoked_login.status(), reqwest::StatusCode::UNAUTHORIZED);
    let other_login = client
        .get(format!("{base_url}/api/v1/auth/me"))
        .header(COOKIE, &target_cookies[2])
        .send()
        .await?;
    assert_eq!(other_login.status(), reqwest::StatusCode::OK);

    let audit = client
        .get(format!("{base_url}/api/v1/admin/audit?pageSize=100"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let audit_body = audit.json::<Value>().await?;
    let audit_events = audit_body["events"]
        .as_array()
        .ok_or("missing audit events")?;
    assert!(audit_events.iter().any(|event| {
        event["eventType"] == "USER_SESSION_REVOKED"
            && event["targetType"] == "web_session"
            && event["targetId"].as_str() == Some(revoke_session_id.as_str())
            && event["metadata"]["userId"].as_str() == Some(target_id.as_str())
    }));
    let audit_json = audit_body.to_string();
    assert!(!audit_json.contains(&target_cookies[1]));
    assert!(!audit_json.contains(&admin_cookie));

    let repeated = client
        .delete(format!(
            "{base_url}/api/v1/admin/users/{target_id}/sessions/{revoke_session_id}"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(repeated.status(), reqwest::StatusCode::NOT_FOUND);
    server.abort();
    Ok(())
}
