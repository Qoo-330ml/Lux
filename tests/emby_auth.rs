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
