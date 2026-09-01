use image::{DynamicImage, ImageFormat};
use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{
        admin_api_key::AdminApiKeyService,
        emby::{EmbyAuthService, EmbyDeviceInfo},
        sessions::WebAuthService,
        users::UserStore,
    },
    config::Config,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Cursor;
use tokio::net::TcpListener;

struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[tokio::test]
async fn emby_users_requires_server_manager_and_supports_both_prefixes()
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
    let users = UserStore::new(database.clone())?;
    let viewer = users
        .create_user("Viewer", "Viewer", "viewer password", false)
        .await?;
    let admin_key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let _server = AbortOnDrop(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));
    let client = reqwest::Client::new();

    for path in ["/Users", "/emby/Users"] {
        let response = client
            .get(format!("http://{address}{path}"))
            .query(&[("api_key", admin_key.as_str())])
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
        assert_eq!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next()),
            Some("application/json"),
            "{path}"
        );
        let body = response.json::<serde_json::Value>().await?;
        let listed_users = body.as_array().ok_or("users response is not an array")?;
        assert_eq!(listed_users.len(), 2, "{path}");
        assert!(
            listed_users
                .iter()
                .any(|user| user["Id"] == admin.id.to_string())
        );
        assert!(
            listed_users
                .iter()
                .any(|user| user["Id"] == viewer.id.to_string())
        );
        assert!(body.to_string().find("password").is_none());
    }

    let viewer_login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .json(&serde_json::json!({
            "Username": "viewer",
            "Pw": "viewer password"
        }))
        .send()
        .await?;
    assert_eq!(viewer_login.status(), reqwest::StatusCode::OK);
    let viewer_token = viewer_login.json::<serde_json::Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let viewer_response = client
        .get(format!("http://{address}/Users"))
        .header("X-Emby-Token", viewer_token)
        .send()
        .await?;
    assert_eq!(viewer_response.status(), reqwest::StatusCode::FORBIDDEN);

    let missing_key = client.get(format!("http://{address}/Users")).send().await?;
    assert_eq!(missing_key.status(), reqwest::StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn emby_public_users_login_and_logout_use_hashed_device_tokens()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let admin = setup
        .complete("Admin", "Administrator", "correct password")
        .await?;
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let _server = AbortOnDrop(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));
    let client = reqwest::Client::new();

    let public = client
        .get(format!("http://{address}/emby/Users/Public"))
        .send()
        .await?;
    assert_eq!(public.status(), reqwest::StatusCode::OK);
    let public_body: serde_json::Value = public.json().await?;
    assert_eq!(public_body.as_array().map(Vec::len), Some(1));
    assert_eq!(public_body[0]["Id"], admin.id.to_string());
    assert_eq!(public_body[0]["HasPassword"], true);

    let afuse_login = client
        .post(format!(
            "http://{address}/emby/Users/authenticatebyname"
        ))
        .header(
            AUTHORIZATION,
            r#"Emby Client="AfuseKt", Device="iPhone", DeviceId="afuse-device", Version="2.9.8.6-fix""#,
        )
        .form(&[
            ("Username", "ADMIN"),
            ("Pw", "correct password"),
            ("appName", "AfuseKt"),
        ])
        .send()
        .await?;
    assert_eq!(afuse_login.status(), reqwest::StatusCode::OK);
    assert!(afuse_login.json::<serde_json::Value>().await?["AccessToken"].is_string());

    let login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="Infuse", Device="iPhone", DeviceId="device-1", Version="1.2.3""#,
        )
        .json(&json!({ "Username": "ADMIN", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let login_body: serde_json::Value = login.json().await?;
    let token = login_body["AccessToken"]
        .as_str()
        .ok_or("missing access token")?
        .to_owned();
    assert_eq!(login_body["User"]["Id"], admin.id.to_string());
    assert_eq!(login_body["User"]["ServerId"], database.server_id());
    assert_eq!(login_body["User"]["HasConfiguredPassword"], true);
    assert_eq!(login_body["User"]["HasConfiguredEasyPassword"], false);
    assert_eq!(login_body["User"]["EnableAutoLogin"], false);
    assert_eq!(
        login_body["User"]["Configuration"]["PlayDefaultAudioTrack"],
        true
    );
    assert_eq!(login_body["User"]["Policy"]["IsAdministrator"], true);
    assert_eq!(login_body["User"]["Policy"]["IsDisabled"], false);
    assert_eq!(
        login_body["User"]["Policy"]["EnableRemoteAccess"],
        admin.can_remote_access
    );
    assert_eq!(login_body["User"]["Policy"]["EnableMediaPlayback"], true);
    assert_eq!(login_body["ServerId"], database.server_id());
    assert_eq!(login_body["SessionInfo"]["Client"], "Infuse");
    assert_eq!(login_body["SessionInfo"]["DeviceId"], "device-1");
    assert_eq!(login_body["SessionInfo"]["ServerId"], database.server_id());
    assert_eq!(login_body["SessionInfo"]["UserId"], admin.id.to_string());
    assert_eq!(login_body["SessionInfo"]["UserName"], "Administrator");
    assert!(
        login_body["SessionInfo"]["Id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );

    let current_user = client
        .get(format!("http://{address}/emby/Users/{}", admin.id))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(current_user.status(), reqwest::StatusCode::OK);
    let current_user_body: serde_json::Value = current_user.json().await?;
    assert_eq!(current_user_body["Id"], admin.id.to_string());
    assert_eq!(current_user_body["Name"], "Administrator");
    assert_eq!(current_user_body["ServerId"], database.server_id());
    assert_eq!(current_user_body["Policy"]["IsAdministrator"], true);

    let raw_token_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM access_tokens WHERE token_hash = ?")
            .bind(token.as_bytes())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(raw_token_count, 0);
    let token_hash = Sha256::digest(token.as_bytes()).to_vec();
    let hashed_token_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM access_tokens WHERE token_hash = ?")
            .bind(token_hash)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(hashed_token_count, 1);

    let logout = client
        .post(format!(
            "http://{address}/emby/Sessions/Logout?api_key={token}"
        ))
        .send()
        .await?;
    assert_eq!(logout.status(), reqwest::StatusCode::NO_CONTENT);
    let second_logout = client
        .post(format!("http://{address}/Sessions/Logout"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(second_logout.status(), reqwest::StatusCode::NO_CONTENT);

    let after_logout = client
        .get(format!("http://{address}/System/Info"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(after_logout.status(), reqwest::StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn emby_user_routes_match_official_request_and_response_contracts()
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
    let users = UserStore::new(database.clone())?;
    let viewer = users
        .create_user("Viewer", "Viewer", "viewer password", false)
        .await?;
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let _server = AbortOnDrop(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));
    let client = reqwest::Client::new();

    let admin_login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .json(&json!({
            "Username": "ADMIN",
            "Pw": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_token = admin_login.json::<serde_json::Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing admin token")?
        .to_owned();
    let admin_auth = (AUTHORIZATION, format!("Emby Token=\"{admin_token}\""));

    let target_user = client
        .get(format!("http://{address}/emby/Users/{}", viewer.id))
        .header(&admin_auth.0, &admin_auth.1)
        .send()
        .await?;
    assert_eq!(target_user.status(), reqwest::StatusCode::OK);
    let target_user_body: serde_json::Value = target_user.json().await?;
    assert_eq!(target_user_body["Id"], viewer.id.to_string());
    assert_eq!(target_user_body["Name"], "Viewer");
    assert!(target_user_body["HasPassword"].is_boolean());
    assert!(target_user_body["Configuration"].is_object());
    assert!(target_user_body["Policy"].is_object());

    let rename = client
        .post(format!("http://{address}/Users/{}", viewer.id))
        .header(&admin_auth.0, &admin_auth.1)
        .json(&json!({
            "Name": "Viewer Renamed",
            "Id": viewer.id.to_string(),
            "ServerId": database.server_id()
        }))
        .send()
        .await?;
    assert_eq!(rename.status(), reqwest::StatusCode::OK);
    assert!(rename.bytes().await?.is_empty());

    let unsupported_put = client
        .put(format!("http://{address}/Users/{}", viewer.id))
        .header(&admin_auth.0, &admin_auth.1)
        .json(&json!({ "Name": "Must Not Apply" }))
        .send()
        .await?;
    assert_eq!(
        unsupported_put.status(),
        reqwest::StatusCode::METHOD_NOT_ALLOWED
    );

    let policy = client
        .post(format!("http://{address}/emby/Users/{}/Policy", viewer.id))
        .header(&admin_auth.0, &admin_auth.1)
        .json(&json!({
            "IsAdministrator": false,
            "IsDisabled": false,
            "EnableRemoteAccess": true,
            "EnableContentDownloading": true,
            "EnableRemoteControlOfOtherUsers": true,
            "EnableSubtitleManagement": true
        }))
        .send()
        .await?;
    assert_eq!(policy.status(), reqwest::StatusCode::OK);
    assert!(policy.bytes().await?.is_empty());

    let policy_xml = client
        .post(format!("http://{address}/emby/Users/{}/Policy", viewer.id))
        .header(&admin_auth.0, &admin_auth.1)
        .header("Content-Type", "application/xml")
        .body(
            "<UserPolicy><EnableRemoteAccess>false</EnableRemoteAccess><EnableContentDownloading>false</EnableContentDownloading></UserPolicy>",
        )
        .send()
        .await?;
    assert_eq!(policy_xml.status(), reqwest::StatusCode::OK);

    let policy_after = client
        .get(format!("http://{address}/Users/{}", viewer.id))
        .header(&admin_auth.0, &admin_auth.1)
        .send()
        .await?;
    let policy_after_body: serde_json::Value = policy_after.json().await?;
    assert_eq!(policy_after_body["Policy"]["EnableRemoteAccess"], false);
    assert_eq!(
        policy_after_body["Policy"]["EnableContentDownloading"],
        false
    );
    assert_eq!(
        policy_after_body["Policy"]["EnableRemoteControlOfOtherUsers"],
        false
    );
    assert_eq!(
        policy_after_body["Policy"]["EnableSubtitleManagement"],
        false
    );

    let password = client
        .post(format!(
            "http://{address}/emby/Users/{}/Password",
            viewer.id
        ))
        .header(&admin_auth.0, &admin_auth.1)
        .json(&json!({
            "Id": viewer.id.to_string(),
            "NewPw": "new viewer password",
            "ResetPassword": true
        }))
        .send()
        .await?;
    assert_eq!(password.status(), reqwest::StatusCode::OK);
    assert!(password.bytes().await?.is_empty());

    let relogin = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .json(&json!({
            "Username": "viewer",
            "Pw": "new viewer password"
        }))
        .send()
        .await?;
    assert_eq!(relogin.status(), reqwest::StatusCode::OK);

    let mut avatar = Cursor::new(Vec::new());
    DynamicImage::new_rgba8(1, 1).write_to(&mut avatar, ImageFormat::Png)?;
    let avatar_bytes = avatar.into_inner();
    let avatar_upload = client
        .post(format!(
            "http://{address}/emby/Users/{}/Images/Primary",
            viewer.id
        ))
        .header(&admin_auth.0, &admin_auth.1)
        .header("Content-Type", "application/octet-stream")
        .body(avatar_bytes.clone())
        .send()
        .await?;
    assert_eq!(avatar_upload.status(), reqwest::StatusCode::OK);
    assert!(avatar_upload.bytes().await?.is_empty());

    let avatar_read = client
        .get(format!(
            "http://{address}/emby/Users/{}/Images/Primary",
            viewer.id
        ))
        .send()
        .await?;
    assert_eq!(avatar_read.status(), reqwest::StatusCode::OK);
    assert_eq!(
        avatar_read
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("image/png")
    );
    assert_eq!(avatar_read.bytes().await?.as_ref(), avatar_bytes.as_slice());

    let avatar_read_at_index = client
        .get(format!(
            "http://{address}/emby/Users/{}/Images/Primary/0",
            viewer.id
        ))
        .send()
        .await?;
    assert_eq!(avatar_read_at_index.status(), reqwest::StatusCode::OK);
    assert_eq!(
        avatar_read_at_index.bytes().await?.as_ref(),
        avatar_bytes.as_slice()
    );

    let avatar_head = client
        .head(format!(
            "http://{address}/emby/Users/{}/Images/Primary",
            viewer.id
        ))
        .send()
        .await?;
    assert_eq!(avatar_head.status(), reqwest::StatusCode::OK);
    assert_eq!(avatar_head.bytes().await?.len(), 0);

    let avatar_delete = client
        .delete(format!(
            "http://{address}/emby/Users/{}/Images/Primary",
            viewer.id
        ))
        .header(&admin_auth.0, &admin_auth.1)
        .send()
        .await?;
    assert_eq!(avatar_delete.status(), reqwest::StatusCode::OK);
    assert!(avatar_delete.bytes().await?.is_empty());

    let avatar_after_delete = client
        .get(format!(
            "http://{address}/emby/Users/{}/Images/Primary",
            viewer.id
        ))
        .send()
        .await?;
    assert_eq!(avatar_after_delete.status(), reqwest::StatusCode::NOT_FOUND);

    let viewer_login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .json(&json!({
            "Username": "viewer",
            "Pw": "new viewer password"
        }))
        .send()
        .await?;
    assert_eq!(viewer_login.status(), reqwest::StatusCode::OK);
    let viewer_token = viewer_login.json::<serde_json::Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let viewer_policy = client
        .post(format!("http://{address}/Users/{}/Policy", viewer.id))
        .header("X-Emby-Token", &viewer_token)
        .json(&json!({ "IsAdministrator": true }))
        .send()
        .await?;
    assert_eq!(viewer_policy.status(), reqwest::StatusCode::FORBIDDEN);

    let viewer_other_user_update = client
        .post(format!("http://{address}/Users/{}", admin.id))
        .header("X-Emby-Token", &viewer_token)
        .json(&json!({ "Name": "Must Not Apply" }))
        .send()
        .await?;
    assert_eq!(
        viewer_other_user_update.status(),
        reqwest::StatusCode::FORBIDDEN
    );

    let missing_user_update = client
        .post(format!(
            "http://{address}/Users/00000000-0000-0000-0000-000000000000"
        ))
        .header(&admin_auth.0, &admin_auth.1)
        .json(&json!({}))
        .send()
        .await;
    assert_eq!(
        missing_user_update?.status(),
        reqwest::StatusCode::NOT_FOUND
    );

    let oversized_for_handler = client
        .post(format!(
            "http://{address}/emby/Users/{}/Images/Primary",
            viewer.id
        ))
        .header(&admin_auth.0, &admin_auth.1)
        .header("Content-Type", "application/octet-stream")
        .body(vec![0_u8; 3 * 1024 * 1024])
        .send()
        .await?;
    assert_eq!(
        oversized_for_handler.status(),
        reqwest::StatusCode::BAD_REQUEST
    );

    let oversized_for_router = client
        .post(format!(
            "http://{address}/emby/Users/{}/Images/Primary",
            viewer.id
        ))
        .header(&admin_auth.0, &admin_auth.1)
        .header("Content-Type", "application/octet-stream")
        .body(vec![0_u8; 5 * 1024 * 1024 + 1])
        .send()
        .await?;
    assert_eq!(
        oversized_for_router.status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE
    );

    let _ = admin;
    Ok(())
}

#[tokio::test]
async fn emby_user_creation_deletion_and_configuration_are_persistent()
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
    let users = UserStore::new(database.clone())?;
    let template = users
        .create_user("Template", "Template", "template password", false)
        .await?;
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let _server = AbortOnDrop(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));
    let client = reqwest::Client::new();

    let admin_login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .json(&json!({
            "Username": "ADMIN",
            "Pw": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_token = admin_login.json::<serde_json::Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing admin token")?
        .to_owned();

    let template_policy = client
        .post(format!(
            "http://{address}/emby/Users/{}/Policy",
            template.id
        ))
        .header("X-Emby-Token", &admin_token)
        .json(&json!({
            "IsDisabled": false,
            "EnableRemoteAccess": true,
            "EnableContentDownloading": true
        }))
        .send()
        .await?;
    assert_eq!(template_policy.status(), reqwest::StatusCode::OK);

    let created = client
        .post(format!("http://{address}/emby/Users/New"))
        .header("X-Emby-Token", &admin_token)
        .json(&json!({
            "Name": "Managed",
            "CopyFromUserId": template.id,
            "UserCopyOptions": ["UserPolicy", "UserConfiguration"]
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::OK);
    let created_body = created.json::<serde_json::Value>().await?;
    let created_id = created_body["Id"]
        .as_str()
        .ok_or("missing created user id")?
        .to_owned();
    assert_eq!(created_body["Name"], "Managed");
    assert_eq!(created_body["HasPassword"], false);
    assert_eq!(created_body["Policy"]["EnableRemoteAccess"], true);
    assert_eq!(created_body["Policy"]["EnableContentDownloading"], true);

    let updated = client
        .post(format!("http://{address}/Users/{created_id}"))
        .header("X-Emby-Token", &admin_token)
        .json(&json!({
            "Id": created_id,
            "Name": "Managed Renamed",
            "Configuration": {
                "PlayDefaultAudioTrack": false,
                "SubtitleMode": "Always",
                "HidePlayedInLatest": true,
                "EnableNextEpisodeAutoPlay": false
            }
        }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);

    let detail = client
        .get(format!("http://{address}/Users/{created_id}"))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(detail.status(), reqwest::StatusCode::OK);
    let detail_body = detail.json::<serde_json::Value>().await?;
    assert_eq!(detail_body["Name"], "Managed Renamed");
    assert_eq!(detail_body["HasPassword"], false);
    assert_eq!(detail_body["Configuration"]["PlayDefaultAudioTrack"], false);
    assert_eq!(detail_body["Configuration"]["SubtitleMode"], "Always");
    assert_eq!(detail_body["Configuration"]["HidePlayedInLatest"], true);
    assert_eq!(
        detail_body["Configuration"]["EnableNextEpisodeAutoPlay"],
        false
    );

    let password = client
        .post(format!("http://{address}/Users/{created_id}/Password"))
        .header("X-Emby-Token", &admin_token)
        .json(&json!({
            "Id": created_id,
            "NewPw": "managed password"
        }))
        .send()
        .await?;
    assert_eq!(password.status(), reqwest::StatusCode::OK);

    let managed_login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .json(&json!({
            "Username": "managed",
            "Pw": "managed password"
        }))
        .send()
        .await?;
    assert_eq!(managed_login.status(), reqwest::StatusCode::OK);
    let managed_token = managed_login.json::<serde_json::Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing managed token")?
        .to_owned();

    let deleted = client
        .delete(format!("http://{address}/emby/Users/{created_id}"))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(deleted.status(), reqwest::StatusCode::OK);
    assert!(deleted.bytes().await?.is_empty());

    let missing = client
        .get(format!("http://{address}/Users/{created_id}"))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let revoked = client
        .get(format!("http://{address}/System/Info"))
        .header("X-Emby-Token", &managed_token)
        .send()
        .await?;
    assert_eq!(revoked.status(), reqwest::StatusCode::UNAUTHORIZED);

    let last_manager = client
        .delete(format!("http://{address}/Users/{}", admin.id))
        .header("X-Emby-Token", &admin_token)
        .send()
        .await?;
    assert_eq!(last_manager.status(), reqwest::StatusCode::CONFLICT);

    Ok(())
}

#[test]
fn emby_device_info_parser_keeps_only_expected_fields() {
    let info = EmbyDeviceInfo::parse(
        r#"Emby Client="Infuse", Device="iPhone", DeviceId="device-1", Version="1.2.3", UserId="user-1""#,
    );
    assert_eq!(info.client, "Infuse");
    assert_eq!(info.device, "iPhone");
    assert_eq!(info.device_id, "device-1");
    assert_eq!(info.version, "1.2.3");
    assert_eq!(info.user_id.as_deref(), Some("user-1"));
    assert!(!format!("{info:?}").contains("token"));
}
