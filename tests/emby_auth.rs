use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat};
use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{
        admin_api_key::AdminApiKeyService,
        emby::{EmbyAuthService, EmbyDeviceInfo},
        sessions::WebAuthService,
        users::{UserStore, UserUpdate},
    },
    config::Config,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{io::Cursor, net::SocketAddr};
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
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

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
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(viewer_response.status(), reqwest::StatusCode::FORBIDDEN);

    // A non-administrator must not reach the media-info import route even with an empty body.
    let viewer_sync = client
        .post(format!("http://{address}/Items/SyncMediaInfo"))
        .header("X-Emby-Token", &viewer_token)
        .body("")
        .send()
        .await?;
    assert_eq!(viewer_sync.status(), reqwest::StatusCode::FORBIDDEN);

    let missing_key = client.get(format!("http://{address}/Users")).send().await?;
    assert_eq!(missing_key.status(), reqwest::StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn emby_users_query_returns_paginated_admin_user_result()
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
    let disabled = users
        .create_user("Disabled", "Disabled", "disabled password", false)
        .await?;
    users
        .update_user(
            &disabled.id.to_string(),
            UserUpdate {
                is_disabled: Some(true),
                ..UserUpdate::default()
            },
        )
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
    let _server = AbortOnDrop(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

    for path in ["/Users/Query", "/emby/Users/Query"] {
        let response = client
            .get(format!("http://{address}{path}"))
            .query(&[
                ("StartIndex", "0"),
                ("Limit", "1"),
                ("IsDisabled", "false"),
                ("api_key", admin_key.as_str()),
            ])
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
        let body = response.json::<serde_json::Value>().await?;
        assert_eq!(body["TotalRecordCount"], 2, "{path}");
        let items = body["Items"].as_array().ok_or("missing Items")?;
        assert_eq!(items.len(), 1, "{path}");
        assert_eq!(items[0]["Id"], admin.id.to_string(), "{path}");
        assert!(body["Items"].is_array(), "{path}");
    }

    let second_page = client
        .get(format!("http://{address}/Users/Query"))
        .query(&[
            ("StartIndex", "1"),
            ("Limit", "1"),
            ("IsDisabled", "false"),
            ("api_key", admin_key.as_str()),
        ])
        .send()
        .await?;
    assert_eq!(second_page.status(), reqwest::StatusCode::OK);
    let second_page_body = second_page.json::<serde_json::Value>().await?;
    assert_eq!(second_page_body["TotalRecordCount"], 2);
    assert_eq!(second_page_body["Items"][0]["Id"], viewer.id.to_string());

    let disabled_response = client
        .get(format!("http://{address}/emby/Users/Query"))
        .query(&[
            ("StartIndex", "0"),
            ("Limit", "1"),
            ("IsDisabled", "true"),
            ("api_key", admin_key.as_str()),
        ])
        .send()
        .await?;
    assert_eq!(disabled_response.status(), reqwest::StatusCode::OK);
    let disabled_body = disabled_response.json::<serde_json::Value>().await?;
    assert_eq!(disabled_body["TotalRecordCount"], 1);
    assert_eq!(disabled_body["Items"][0]["Id"], disabled.id.to_string());
    assert_eq!(disabled_body["Items"][0]["Policy"]["IsDisabled"], true);

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
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

    let public = client
        .get(format!("http://{address}/emby/Users/Public"))
        .send()
        .await?;
    assert_eq!(public.status(), reqwest::StatusCode::OK);
    let public_body: serde_json::Value = public.json().await?;
    assert_eq!(public_body.as_array().map(Vec::len), Some(1));
    assert_eq!(public_body[0]["Id"], admin.id.to_string());
    assert_eq!(public_body[0]["HasPassword"], true);
    assert_eq!(public_body[0]["Name"], "Administrator");
    let public_user_name = public_body[0]["Name"]
        .as_str()
        .ok_or("missing public user name")?;

    let afuse_login = client
        .post(format!(
            "http://{address}/emby/Users/authenticatebyname"
        ))
        .header(
            AUTHORIZATION,
            r#"Emby Client="AfuseKt", Device="iPhone", DeviceId="afuse-device", Version="2.9.8.6-fix""#,
        )
        .form(&[
            ("Username", public_user_name),
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

    let users = UserStore::new(database.clone())?;
    users
        .create_user(
            "another-admin-label",
            "Administrator",
            "another password",
            false,
        )
        .await?;
    users
        .create_user(
            "display-name-collision",
            "Admin",
            "collision password",
            false,
        )
        .await?;
    let canonical_username_wrong_password = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .json(&json!({
            "Username": "admin",
            "Pw": "collision password"
        }))
        .send()
        .await?;
    assert_eq!(
        canonical_username_wrong_password.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let ambiguous_display_name_login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .json(&json!({
            "Username": public_user_name,
            "Pw": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(
        ambiguous_display_name_login.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    Ok(())
}

#[tokio::test]
async fn emby_user_id_authentication_matches_avdb_request() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup
        .complete("login-name", "Display Name", "correct password")
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
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

    for prefix in ["", "/emby"] {
        let path = format!("{prefix}/Users/{}/Authenticate", admin.id);
        let response = client
            .post(format!("http://{address}{path}"))
            .header("Accept", "application/json")
            .header("X-Emby-Token", &admin_key)
            .header("X-MediaBrowser-Token", &admin_key)
            .header(
                AUTHORIZATION,
                r#"Emby Client="Avdb", Device="Avdb Server", DeviceId="avdb-server", Version="1.0""#,
            )
            .json(&json!({ "Pw": "correct password" }))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
        let body: serde_json::Value = response.json().await?;
        assert_eq!(body["User"]["Id"], admin.id.to_string());
        assert!(
            body["AccessToken"]
                .as_str()
                .is_some_and(|token| !token.is_empty())
        );
    }

    let wrong_password = client
        .post(format!("http://{address}/Users/{}/Authenticate", admin.id))
        .header("X-Emby-Token", &admin_key)
        .json(&json!({ "Pw": "wrong password" }))
        .send()
        .await?;
    assert_eq!(wrong_password.status(), reqwest::StatusCode::UNAUTHORIZED);

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
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

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

    let avatar_upload_as_base64 = client
        .post(format!(
            "http://{address}/emby/Users/{}/Images/Primary",
            viewer.id
        ))
        .header(&admin_auth.0, &admin_auth.1)
        .header("Content-Type", "image/png")
        .body(STANDARD.encode(&avatar_bytes))
        .send()
        .await?;
    assert_eq!(avatar_upload_as_base64.status(), reqwest::StatusCode::OK);
    assert!(avatar_upload_as_base64.bytes().await?.is_empty());

    let avatar_read_anonymous = client
        .get(format!(
            "http://{address}/emby/Users/{}/Images/Primary",
            viewer.id
        ))
        .send()
        .await?;
    assert_eq!(
        avatar_read_anonymous.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let avatar_read = client
        .get(format!(
            "http://{address}/emby/Users/{}/Images/Primary",
            viewer.id
        ))
        .header(&admin_auth.0, &admin_auth.1)
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
        .header(&admin_auth.0, &admin_auth.1)
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
        .header(&admin_auth.0, &admin_auth.1)
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
        .header(&admin_auth.0, &admin_auth.1)
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
async fn emby_create_user_accepts_query_name_without_a_request_body()
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
    let viewer = UserStore::new(database.clone())?
        .create_user("Viewer", "Viewer", "viewer password", false)
        .await?;
    let defaults = json!({
        "libraryAccessMode": "NONE",
        "libraryIds": [],
        "canDownload": true,
        "canPlay": false,
        "maxSimultaneousPlaybacks": 3,
        "allowRemux": false,
        "allowAudioTranscode": true,
        "allowVideoTranscode": false,
    });
    sqlx::query("INSERT INTO server_settings (key, value) VALUES ('new_user_permissions', ?)")
        .bind(defaults.to_string())
        .execute(database.pool())
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
    let _server = AbortOnDrop(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

    let created = client
        .post(format!("http://{address}/emby/Users/New"))
        .query(&[("Name", "QueryUser")])
        .header("X-Emby-Token", &admin_key)
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::OK);
    let created_body = created.json::<serde_json::Value>().await?;
    assert_eq!(created_body["Name"], "QueryUser");
    let created_id = created_body["Id"]
        .as_str()
        .ok_or("missing created user id")?;
    assert_eq!(created_body["HasPassword"], false);
    assert_eq!(created_body["Policy"]["EnableContentDownloading"], true);
    assert!(uuid::Uuid::parse_str(created_id).is_ok());
    let stored_policy: (i64, i64, i64, i64, i64, i64, String) = sqlx::query_as(
        "SELECT can_download, can_play, max_simultaneous_playbacks,
                allow_remux, allow_audio_transcode, allow_video_transcode,
                library_access_mode
         FROM users WHERE id = ?",
    )
    .bind(created_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored_policy, (1, 0, 3, 0, 1, 0, "NONE".to_owned()));

    let duplicate = client
        .post(format!("http://{address}/Users/New"))
        .query(&[("Name", "QueryUser")])
        .header("X-Emby-Token", &admin_key)
        .send()
        .await?;
    assert_eq!(duplicate.status(), reqwest::StatusCode::CONFLICT);

    let missing_name = client
        .post(format!("http://{address}/Users/New"))
        .header("X-Emby-Token", &admin_key)
        .send()
        .await?;
    assert_eq!(missing_name.status(), reqwest::StatusCode::BAD_REQUEST);

    let viewer_login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .json(&json!({
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
    let forbidden = client
        .post(format!("http://{address}/Users/New"))
        .query(&[("Name", "ForbiddenUser")])
        .header("X-Emby-Token", viewer_token)
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    sqlx::query("DROP TABLE server_settings")
        .execute(database.pool())
        .await?;
    let settings_unavailable = client
        .post(format!("http://{address}/Users/New"))
        .query(&[("Name", "SettingsUnavailable")])
        .header("X-Emby-Token", &admin_key)
        .send()
        .await?;
    assert_eq!(
        settings_unavailable.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    let missing_user_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE username_normalized = 'settingsunavailable'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(missing_user_count, 0);
    assert_eq!(viewer.id.to_string().len(), 36);
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
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

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

#[tokio::test]
async fn emby_authenticated_access_refreshes_device_activity()
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
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let _server = AbortOnDrop(tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    }));
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let device_id = "emby-activity-device";
    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES ('trusted_proxy_cidrs', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind("[\"127.0.0.1/32\"]")
    .execute(database.pool())
    .await?;
    let login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            format!(
                r#"Emby Client="Infuse", Device="Apple TV", DeviceId="{device_id}", Version="1.0""#
            ),
        )
        .header("x-forwarded-for", "203.0.113.10")
        .json(&json!({ "Username": "Admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let token = login.json::<serde_json::Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing access token")?
        .to_owned();
    let first_seen_at: i64 = sqlx::query_scalar(
        "SELECT first_seen_at FROM user_devices WHERE user_id = ? AND device_id = ?",
    )
    .bind(admin.id.to_string())
    .bind(device_id)
    .fetch_one(database.pool())
    .await?;
    sqlx::query(
        "UPDATE user_devices SET last_seen_at = 1, last_remote_ip = '203.0.113.10'
         WHERE user_id = ? AND device_id = ?",
    )
    .bind(admin.id.to_string())
    .bind(device_id)
    .execute(database.pool())
    .await?;

    let response = client
        .get(format!("http://{address}/Users/{}", admin.id))
        .header("X-Emby-Token", &token)
        .header("x-forwarded-for", "203.0.113.11")
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let refreshed_device: (i64, i64, Option<String>) = sqlx::query_as(
        "SELECT first_seen_at, last_seen_at, last_remote_ip
         FROM user_devices WHERE user_id = ? AND device_id = ?",
    )
    .bind(admin.id.to_string())
    .bind(device_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(refreshed_device.0, first_seen_at);
    assert!(refreshed_device.1 > 1);
    assert_eq!(refreshed_device.2.as_deref(), Some("203.0.113.11"));

    // Once the proxy allowlist is gone the forwarded header must not be believed: the recorded
    // address has to fall back to the connection peer.
    sqlx::query("DELETE FROM server_settings WHERE key = 'trusted_proxy_cidrs'")
        .execute(database.pool())
        .await?;
    let untrusted_forward = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="Infuse", Device="Apple TV", DeviceId="emby-untrusted-forward", Version="1.0""#,
        )
        .header("x-forwarded-for", "203.0.113.12")
        .json(&json!({ "Username": "Admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(untrusted_forward.status(), reqwest::StatusCode::OK);
    let untrusted_device: (Option<String>,) = sqlx::query_as(
        "SELECT last_remote_ip FROM user_devices WHERE user_id = ? AND device_id = ?",
    )
    .bind(admin.id.to_string())
    .bind("emby-untrusted-forward")
    .fetch_one(database.pool())
    .await?;
    assert_eq!(untrusted_device.0.as_deref(), Some("127.0.0.1"));
    sqlx::query("DELETE FROM user_devices WHERE user_id = ? AND device_id = ?")
        .bind(admin.id.to_string())
        .bind("emby-untrusted-forward")
        .execute(database.pool())
        .await?;

    let login_without_device = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .json(&json!({ "Username": "Admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(login_without_device.status(), reqwest::StatusCode::OK);
    let token_without_device =
        login_without_device.json::<serde_json::Value>().await?["AccessToken"]
            .as_str()
            .ok_or("missing access token")?
            .to_owned();
    let no_device_access = client
        .get(format!("http://{address}/Users/{}", admin.id))
        .header("X-Emby-Token", token_without_device)
        .send()
        .await?;
    assert_eq!(no_device_access.status(), reqwest::StatusCode::OK);
    let inventory_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_devices WHERE user_id = ?")
            .bind(admin.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(inventory_count, 1);

    let failing_device_login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="Infuse", Device="Apple TV", DeviceId="failing-write-device", Version="1.0""#,
        )
        .json(&json!({ "Username": "Admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(failing_device_login.status(), reqwest::StatusCode::OK);
    let failing_device_token =
        failing_device_login.json::<serde_json::Value>().await?["AccessToken"]
            .as_str()
            .ok_or("missing access token")?
            .to_owned();
    sqlx::query(
        "CREATE TRIGGER fail_emby_user_device_activity
         BEFORE UPDATE ON user_devices
         BEGIN SELECT RAISE(ABORT, 'device inventory unavailable'); END",
    )
    .execute(database.pool())
    .await?;
    let storage_failure_access = client
        .get(format!("http://{address}/Users/{}", admin.id))
        .header("X-Emby-Token", failing_device_token)
        .send()
        .await?;
    assert_eq!(storage_failure_access.status(), reqwest::StatusCode::OK);

    Ok(())
}

#[tokio::test]
async fn emby_login_and_existing_tokens_enforce_device_client_blacklists()
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
    let target = UserStore::new(database.clone())?
        .create_user(
            "emby-blacklist-target",
            "Emby Blacklist Target",
            "target password",
            false,
        )
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
    let _server = AbortOnDrop(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));
    let client = reqwest::Client::builder().no_proxy().build()?;
    let base_url = format!("http://{address}");
    let login = |client_name: &str, device_id: &str| {
        client
            .post(format!("{base_url}/Users/AuthenticateByName"))
            .header(
                AUTHORIZATION,
                format!(
                    r#"Emby Client="{client_name}", Device="Test Device", DeviceId="{device_id}", Version="1.0""#
                ),
            )
            .json(&json!({
                "Username": "emby-blacklist-target",
                "Pw": "target password"
            }))
    };

    let device_a = "emby-blacklist-device-a";
    let first_login = login("Infuse", device_a).send().await?;
    assert_eq!(first_login.status(), reqwest::StatusCode::OK);
    let first_token = first_login.json::<serde_json::Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing access token")?
        .to_owned();

    sqlx::query(
        "INSERT INTO user_access_blacklist_rules (
             id, user_id, rule_type, match_value, is_enabled, created_by, created_at, updated_at
         ) VALUES (?, ?, 'DEVICE', ?, 1, ?, unixepoch(), unixepoch())",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(target.id.to_string())
    .bind(device_a)
    .bind(admin.id.to_string())
    .execute(database.pool())
    .await?;

    let blocked_existing_token = client
        .get(format!("{base_url}/Users/{}", target.id))
        .header("X-Emby-Token", &first_token)
        .send()
        .await?;
    assert_eq!(
        blocked_existing_token.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let first_token_hash = Sha256::digest(first_token.as_bytes()).to_vec();
    let revoked_at: Option<i64> =
        sqlx::query_scalar("SELECT revoked_at FROM access_tokens WHERE token_hash = ?")
            .bind(first_token_hash)
            .fetch_one(database.pool())
            .await?;
    assert!(revoked_at.is_some());

    let denied_device_login = login("Infuse", device_a).send().await?;
    assert_eq!(
        denied_device_login.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let device_b = "emby-blacklist-device-b";
    let other_device_login = login("Infuse", device_b).send().await?;
    assert_eq!(other_device_login.status(), reqwest::StatusCode::OK);
    let other_device_token = other_device_login.json::<serde_json::Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing other-device token")?
        .to_owned();

    sqlx::query(
        "INSERT INTO user_access_blacklist_rules (
             id, user_id, rule_type, match_value, is_enabled, created_by, created_at, updated_at
         ) VALUES (?, NULL, 'CLIENT', 'infuse', 1, ?, unixepoch(), unixepoch())",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(admin.id.to_string())
    .execute(database.pool())
    .await?;
    let blocked_by_client = client
        .get(format!("{base_url}/Users/{}", target.id))
        .header("X-Emby-Token", &other_device_token)
        .send()
        .await?;
    assert_eq!(
        blocked_by_client.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let denied_normalized_client = login("  INFUSE   ", "emby-blacklist-device-c")
        .send()
        .await?;
    assert_eq!(
        denied_normalized_client.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let other_client_login = login("Plex", "emby-blacklist-device-c").send().await?;
    assert_eq!(other_client_login.status(), reqwest::StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn emby_password_logins_update_device_inventory_without_blocking_auth()
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
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let _server = AbortOnDrop(tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    }));
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let device_id = "emby-shared-device";
    let remote_ip = "203.0.113.10";
    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES ('trusted_proxy_cidrs', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind("[\"127.0.0.1/32\"]")
    .execute(database.pool())
    .await?;

    let first_login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="Infuse", Device="iPhone", DeviceId="emby-shared-device", Version="1.2.3""#,
        )
        .header("x-forwarded-for", remote_ip)
        .json(&json!({ "Username": "Admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(first_login.status(), reqwest::StatusCode::OK);

    let first_row: (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT device_id, device_name, device_type, client_name, client_version,
                first_seen_at, last_seen_at
         FROM user_devices WHERE user_id = ?",
    )
    .bind(admin.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(first_row.0, device_id);
    assert_eq!(first_row.1.as_deref(), Some("iPhone"));
    assert_eq!(first_row.2.as_deref(), Some("EMBY"));
    assert_eq!(first_row.3.as_deref(), Some("Infuse"));
    assert_eq!(first_row.4.as_deref(), Some("1.2.3"));
    assert!(first_row.5 > 0);
    assert!(first_row.6 >= first_row.5);
    let (stored_ip,): (Option<String>,) = sqlx::query_as(
        "SELECT last_remote_ip FROM user_devices WHERE user_id = ? AND device_id = ?",
    )
    .bind(admin.id.to_string())
    .bind(device_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored_ip.as_deref(), Some(remote_ip));

    let second_login = client
        .post(format!("http://{address}/Users/{}/Authenticate", admin.id))
        .header(
            AUTHORIZATION,
            r#"Emby Client="SenPlayer", Device="iPad", DeviceId="emby-shared-device", Version="2.0""#,
        )
        .json(&json!({ "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(second_login.status(), reqwest::StatusCode::OK);
    let updated_row: (i64, String, String, String) = sqlx::query_as(
        "SELECT COUNT(*), MAX(client_name), MAX(device_name), MAX(client_version)
         FROM user_devices WHERE user_id = ? AND device_id = ?",
    )
    .bind(admin.id.to_string())
    .bind(device_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(updated_row.0, 1);
    assert_eq!(updated_row.1, "SenPlayer");
    assert_eq!(updated_row.2, "iPad");
    assert_eq!(updated_row.3, "2.0");

    for device_header in [
        r#"Emby Client="MissingId""#,
        r#"Emby Client="BlankId", DeviceId="   ""#,
        &format!("Emby Client=\"LongId\", DeviceId=\"{}\"", "d".repeat(257)),
    ] {
        let response = client
            .post(format!("http://{address}/Users/AuthenticateByName"))
            .header(AUTHORIZATION, device_header)
            .json(&json!({ "Username": "Admin", "Pw": "correct password" }))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }
    let count_before_storage_failure: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_devices WHERE user_id = ?")
            .bind(admin.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(count_before_storage_failure, 1);

    sqlx::query(
        "CREATE TRIGGER fail_emby_user_device_insert
         BEFORE INSERT ON user_devices
         BEGIN SELECT RAISE(ABORT, 'device inventory unavailable'); END",
    )
    .execute(database.pool())
    .await?;
    let storage_failure_login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="Infuse", Device="Apple TV", DeviceId="another-device", Version="1.0""#,
        )
        .json(&json!({ "Username": "Admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(storage_failure_login.status(), reqwest::StatusCode::OK);

    Ok(())
}

#[tokio::test]
async fn emby_credentials_follow_server_precedence_without_query_alias_conflicts()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "admin password").await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("Viewer", "Viewer", "viewer password", false)
        .await?;
    let key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let auth = EmbyAuthService::new(database.clone())?;
    let token = auth
        .authenticate("Viewer", "viewer password", &EmbyDeviceInfo::default())
        .await?
        .ok_or("viewer login failed")?
        .token;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database)?,
        auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let _server = AbortOnDrop(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let invalid = "invalid-test-credential";
    for prefix in ["", "/emby"] {
        // Public discovery and password login do not authenticate with query tokens.
        let public = client
            .get(format!("http://{address}{prefix}/System/Info/Public"))
            .query(&[("api_key", invalid), ("api_key", invalid)])
            .send()
            .await?;
        assert_eq!(public.status(), reqwest::StatusCode::OK);
        let password_login = client
            .post(format!("http://{address}{prefix}/Users/AuthenticateByName"))
            .query(&[("api_key", invalid), ("api_key", invalid)])
            .json(&json!({"Username": "Viewer", "Pw": "viewer password"}))
            .send()
            .await?;
        assert_eq!(password_login.status(), reqwest::StatusCode::OK);
        for (label, query, expected) in [
            (
                "same values",
                vec![
                    ("X-Emby-Token", token.as_str()),
                    ("api_key", token.as_str()),
                ],
                200,
            ),
            (
                "viewer wins",
                vec![("X-Emby-Token", token.as_str()), ("api_key", key.as_str())],
                200,
            ),
            (
                "reverse order",
                vec![("api_key", key.as_str()), ("X-Emby-Token", token.as_str())],
                200,
            ),
            (
                "invalid higher",
                vec![("X-Emby-Token", invalid), ("api_key", key.as_str())],
                401,
            ),
            (
                "empty higher",
                vec![("X-Emby-Token", ""), ("api_key", key.as_str())],
                200,
            ),
            (
                "same name twice",
                vec![("api_key", token.as_str()), ("api_key", token.as_str())],
                401,
            ),
            (
                "repeated higher",
                vec![
                    ("X-Emby-Token", token.as_str()),
                    ("x-emby-token", token.as_str()),
                    ("api_key", key.as_str()),
                ],
                401,
            ),
            (
                "query higher ignores repeated lower",
                vec![
                    ("api_key", key.as_str()),
                    ("api_key", invalid),
                    ("X-Emby-Token", token.as_str()),
                ],
                200,
            ),
            (
                "different repeated values",
                vec![("api_key", token.as_str()), ("api_key", invalid)],
                401,
            ),
            (
                "legacy media browser fallback",
                vec![("X-MediaBrowser-Token", token.as_str())],
                200,
            ),
            ("legacy fallback", vec![("apiKey", token.as_str())], 200),
            (
                "legacy lower",
                vec![("X-MediaBrowser-Token", invalid), ("api_key", key.as_str())],
                200,
            ),
        ] {
            let response = client
                .get(format!("http://{address}{prefix}/System/Info"))
                .query(&query)
                .send()
                .await?;
            assert_eq!(response.status().as_u16(), expected, "{prefix}: {label}");
        }
        // A proxy's administrator key must not promote the selected user token.
        let response = client
            .get(format!("http://{address}{prefix}/Users"))
            .query(&[("X-Emby-Token", token.as_str()), ("api_key", key.as_str())])
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        let reverse_response = client
            .get(format!("http://{address}{prefix}/Users"))
            .query(&[("api_key", key.as_str()), ("X-Emby-Token", token.as_str())])
            .send()
            .await?;
        assert_eq!(reverse_response.status(), reqwest::StatusCode::FORBIDDEN);
        let administrator_response = client
            .get(format!("http://{address}{prefix}/Users"))
            .query(&[("api_key", token.as_str()), ("X-Emby-Token", key.as_str())])
            .send()
            .await?;
        assert_eq!(administrator_response.status(), reqwest::StatusCode::OK);
        let bearer_query = format!("Bearer {token}");
        let opaque_response = client
            .get(format!("http://{address}{prefix}/System/Info"))
            .query(&[("api_key", bearer_query.as_str())])
            .send()
            .await?;
        assert_eq!(opaque_response.status(), reqwest::StatusCode::UNAUTHORIZED);
        let authorization = format!("MediaBrowser Token=\"{token}\"");
        for (headers, query, expected) in [
            (
                vec![("X-Emby-Token", key.as_str())],
                vec![
                    ("X-Emby-Token", invalid),
                    ("api_key", token.as_str()),
                    ("api_key", invalid),
                ],
                200,
            ),
            (
                vec![("X-Emby-Token", invalid)],
                vec![("api_key", key.as_str())],
                401,
            ),
            (
                vec![("X-MediaBrowser-Token", key.as_str())],
                vec![("X-Emby-Token", invalid)],
                401,
            ),
            (
                vec![
                    ("Authorization", authorization.as_str()),
                    ("X-MediaBrowser-Token", invalid),
                ],
                vec![],
                200,
            ),
            (
                vec![("X-Lux-Api-Key", key.as_str())],
                vec![("X-Emby-Token", invalid)],
                401,
            ),
        ] {
            let mut request = client
                .get(format!("http://{address}{prefix}/System/Info"))
                .query(&query);
            for (name, value) in headers {
                request = request.header(name, value);
            }
            assert_eq!(request.send().await?.status().as_u16(), expected);
        }
        let invalid_authorization = "MediaBrowser Token=\"invalid-test-credential\"";
        for (headers, query, expected) in [
            (
                vec![("Authorization", invalid_authorization)],
                vec![("X-Emby-Token", token.as_str())],
                200,
            ),
            (
                vec![("Authorization", invalid_authorization)],
                vec![("api_key", token.as_str())],
                401,
            ),
            (
                vec![("X-MediaBrowser-Token", invalid)],
                vec![("api_key", token.as_str())],
                401,
            ),
            (
                vec![("X-Emby-Token", ""), ("X-MediaBrowser-Token", invalid)],
                vec![("X-Emby-Token", token.as_str())],
                200,
            ),
            (
                vec![
                    ("X-Emby-Authorization", invalid_authorization),
                    ("Authorization", authorization.as_str()),
                ],
                vec![],
                401,
            ),
            (
                vec![
                    ("X-Emby-Token", key.as_str()),
                    ("X-Emby-Authorization", invalid_authorization),
                ],
                vec![],
                200,
            ),
        ] {
            let mut request = client
                .get(format!("http://{address}{prefix}/System/Info"))
                .query(&query);
            for (name, value) in headers {
                request = request.header(name, value);
            }
            assert_eq!(request.send().await?.status().as_u16(), expected);
        }
        // External callers cannot set the internal selected-credential header.
        let forged = client
            .get(format!("http://{address}{prefix}/System/Info"))
            .header("x-lux-internal-emby-credential", key.as_str())
            .send()
            .await?;
        assert_eq!(forged.status(), reqwest::StatusCode::UNAUTHORIZED);
        let head = client
            .head(format!("http://{address}{prefix}/Items/{}", viewer.id))
            .query(&[("api_key", key.as_str()), ("X-Emby-Token", token.as_str())])
            .send()
            .await?;
        assert_eq!(head.status(), reqwest::StatusCode::OK);
        let post = client
            .post(format!(
                "http://{address}{prefix}/Items/{}/PlaybackInfo",
                viewer.id
            ))
            .query(&[("api_key", key.as_str()), ("X-Emby-Token", token.as_str())])
            .json(&json!({}))
            .send()
            .await?;
        assert_eq!(post.status(), reqwest::StatusCode::NOT_FOUND);
        // All credential-bearing route shapes must match their single-token baseline.
        for path in [
            format!("/Users/{}/Items/Root", viewer.id),
            "/Items?Limit=1".to_owned(),
            "/Items/Counts".to_owned(),
            "/Search/Hints?SearchTerm=example".to_owned(),
            format!("/Items/{}/Images/Primary", viewer.id),
            format!("/Items/{}/PlaybackInfo", viewer.id),
            format!("/Videos/{}/original.strm", viewer.id),
            format!("/Users/{}/Items/{}", viewer.id, viewer.id),
        ] {
            let url = format!("http://{address}{prefix}{path}");
            let baseline = client
                .get(&url)
                .query(&[("api_key", token.as_str())])
                .send()
                .await?;
            assert_ne!(
                baseline.status(),
                reqwest::StatusCode::BAD_REQUEST,
                "{path}"
            );
            assert_ne!(
                baseline.status(),
                reqwest::StatusCode::UNAUTHORIZED,
                "{path}"
            );
            let mixed = client
                .get(&url)
                .query(&[("api_key", key.as_str()), ("X-Emby-Token", token.as_str())])
                .send()
                .await?;
            assert_eq!(mixed.status(), baseline.status(), "{prefix}{path}");
            let invalid_response = client
                .get(&url)
                .query(&[("api_key", key.as_str()), ("X-Emby-Token", invalid)])
                .send()
                .await?;
            assert_eq!(
                invalid_response.status(),
                reqwest::StatusCode::UNAUTHORIZED,
                "{prefix}{path}"
            );
        }
    }
    // The native API retains its own key selection contract.
    let response = client
        .get(format!("http://{address}/api/v1/admin/api-key"))
        .query(&[("api_key", key.as_str()), ("X-Emby-Token", invalid)])
        .send()
        .await?;
    let native_baseline = client
        .get(format!("http://{address}/api/v1/admin/api-key"))
        .query(&[("api_key", key.as_str())])
        .send()
        .await?;
    // The query-string key no longer authenticates admin routes at all, so the request is
    // simply unauthenticated (401) instead of being rejected as a client token (403).
    assert!(
        matches!(
            native_baseline.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ),
        "admin routes must not accept a query-string key"
    );
    assert_eq!(response.status(), native_baseline.status());
    Ok(())
}

#[tokio::test]
async fn emby_login_and_existing_tokens_enforce_ip_blacklists_through_trusted_proxies()
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
    let target = UserStore::new(database.clone())?
        .create_user("emby-ip-target", "Emby IP Target", "target password", false)
        .await?;
    let admin_key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        emby_auth.clone(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let _server = AbortOnDrop(tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    }));
    let client = reqwest::Client::builder().no_proxy().build()?;
    let base_url = format!("http://{address}");
    let login = |client_ip: &str| {
        client
            .post(format!("{base_url}/Users/AuthenticateByName"))
            .header(
                AUTHORIZATION,
                r#"Emby Client="Infuse", Device="iPhone", DeviceId="emby-ip-device", Version="1.0""#,
            )
            .header("X-Forwarded-For", client_ip)
            .json(&json!({
                "Username": "emby-ip-target",
                "Pw": "target password"
            }))
    };

    sqlx::query(
        "INSERT INTO user_access_blacklist_rules (
             id, user_id, rule_type, match_value, is_enabled, created_by, created_at, updated_at
         ) VALUES ('emby-ip-rule', ?, 'IP_CIDR', '198.51.100.0/24', 1, ?, unixepoch(), unixepoch())",
    )
    .bind(target.id.to_string())
    .bind(admin.id.to_string())
    .execute(database.pool())
    .await?;

    let untrusted_xff = login("198.51.100.7").send().await?;
    assert_eq!(untrusted_xff.status(), reqwest::StatusCode::OK);
    let untrusted_token = untrusted_xff.json::<serde_json::Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing access token")?
        .to_owned();

    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES ('trusted_proxy_cidrs', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind("[\"127.0.0.1/32\"]")
    .execute(database.pool())
    .await?;
    let no_peer_auth = emby_auth
        .authenticate(
            "emby-ip-target",
            "target password",
            &EmbyDeviceInfo {
                client: "Infuse".to_owned(),
                device: "iPhone".to_owned(),
                device_id: "emby-ip-no-peer".to_owned(),
                version: "1.0".to_owned(),
                user_id: None,
            },
        )
        .await?;
    assert!(no_peer_auth.is_none());

    let trusted_xff = login("198.51.100.8").send().await?;
    assert_eq!(trusted_xff.status(), reqwest::StatusCode::UNAUTHORIZED);
    let trusted_user_id_login = client
        .post(format!("{base_url}/Users/{}/Authenticate", target.id))
        .header(
            AUTHORIZATION,
            r#"Emby Client="Infuse", Device="iPhone", DeviceId="emby-ip-device", Version="1.0""#,
        )
        .header("X-Forwarded-For", "198.51.100.10")
        .json(&json!({ "Pw": "target password" }))
        .send()
        .await?;
    assert_eq!(
        trusted_user_id_login.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let existing_token_on_blocked_ip = client
        .get(format!("{base_url}/Users/{}", target.id))
        .header("X-Emby-Token", &untrusted_token)
        .header("X-Forwarded-For", "198.51.100.9")
        .send()
        .await?;
    assert_eq!(
        existing_token_on_blocked_ip.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let token_hash = Sha256::digest(untrusted_token.as_bytes()).to_vec();
    let revoked_at: Option<i64> =
        sqlx::query_scalar("SELECT revoked_at FROM access_tokens WHERE token_hash = ?")
            .bind(token_hash)
            .fetch_one(database.pool())
            .await?;
    assert!(revoked_at.is_some());

    let allowed_login = login("192.0.2.17").send().await?;
    assert_eq!(allowed_login.status(), reqwest::StatusCode::OK);
    let allowed_token = allowed_login.json::<serde_json::Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing allowed access token")?
        .to_owned();
    let allowed_existing_token = client
        .get(format!("{base_url}/Users/{}", target.id))
        .header("X-Emby-Token", &allowed_token)
        .header("X-Forwarded-For", "192.0.2.18")
        .send()
        .await?;
    assert_eq!(allowed_existing_token.status(), reqwest::StatusCode::OK);

    sqlx::query(
        "UPDATE user_access_blacklist_rules SET match_value = 'not-a-cidr'
         WHERE id = 'emby-ip-rule'",
    )
    .execute(database.pool())
    .await?;
    let corrupt_rule_login = login("192.0.2.19").send().await?;
    assert_eq!(
        corrupt_rule_login.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    sqlx::query(
        "UPDATE user_access_blacklist_rules SET match_value = '198.51.100.0/24'
         WHERE id = 'emby-ip-rule'",
    )
    .execute(database.pool())
    .await?;

    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES ('trusted_proxy_cidrs', 'invalid-json')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .execute(database.pool())
    .await?;
    let invalid_proxy_settings_login = login("192.0.2.21").send().await?;
    assert_eq!(
        invalid_proxy_settings_login.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    sqlx::query("UPDATE server_settings SET value = ? WHERE key = 'trusted_proxy_cidrs'")
        .bind("[\"127.0.0.1/32\"]")
        .execute(database.pool())
        .await?;

    sqlx::query(
        "INSERT INTO user_access_blacklist_rules (
             id, user_id, rule_type, match_value, is_enabled, created_by, created_at, updated_at
         ) VALUES ('emby-ip-global-rule', NULL, 'IP_CIDR', '203.0.113.0/24', 1, ?, unixepoch(), unixepoch())",
    )
    .bind(admin.id.to_string())
    .execute(database.pool())
    .await?;
    let shared_admin_key_access = client
        .get(format!("{base_url}/Users"))
        .query(&[("api_key", admin_key.as_str())])
        .header("X-Forwarded-For", "203.0.113.5")
        .send()
        .await?;
    assert_eq!(shared_admin_key_access.status(), reqwest::StatusCode::OK);

    for index in 0..200 {
        let id = format!("emby-ip-overflow-{index}");
        let match_value = format!("10.0.{}/32", index % 256);
        sqlx::query(
            "INSERT INTO user_access_blacklist_rules (
                 id, user_id, rule_type, match_value, is_enabled, created_by,
                 created_at, updated_at
             ) VALUES (?, ?, 'IP_CIDR', ?, 1, ?, unixepoch(), unixepoch())",
        )
        .bind(id)
        .bind(target.id.to_string())
        .bind(match_value)
        .bind(admin.id.to_string())
        .execute(database.pool())
        .await?;
    }
    let overflow_login = login("192.0.2.20").send().await?;
    assert_eq!(
        overflow_login.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    Ok(())
}
