use std::time::Duration;

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

#[tokio::test]
async fn registration_settings_require_admin_csrf_and_version_without_changing_mail_credentials()
-> Result<(), Box<dyn std::error::Error>> {
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
    users
        .create_user("viewer", "Viewer", "settings-test-password", false)
        .await?;
    let credentials_path = config.config_dir.join("smtp_credentials.json");
    let mail_sentinel = br#"{"username":"smtp-test-user","password":"smtp-test-password"}"#;
    tokio::fs::write(&credentials_path, mail_sentinel).await?;
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
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(12))
        .build()?;
    let endpoint = format!("{base}/api/v1/admin/settings/registration");
    assert_eq!(
        client.get(&endpoint).send().await?.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .patch(&endpoint)
            .json(&json!({}))
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let (viewer_cookie, viewer_csrf) = login(&client, &base, "viewer").await?;
    assert_eq!(
        client
            .get(&endpoint)
            .header(COOKIE, &viewer_cookie)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .patch(&endpoint)
            .header(COOKIE, &viewer_cookie)
            .header("X-CSRF-Token", viewer_csrf)
            .json(&json!({"registrationMode":"PUBLIC_EMAIL", "version":0}))
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    let (cookies, csrf) = login(&client, &base, "admin").await?;
    let initial = client
        .get(&endpoint)
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(initial.status(), StatusCode::OK);
    assert_eq!(
        initial.json::<Value>().await?,
        json!({"registrationMode":"INVITATION_ONLY", "version":0})
    );
    assert_eq!(
        client
            .patch(&endpoint)
            .header(COOKIE, &cookies)
            .json(&json!({"registrationMode":"PUBLIC_EMAIL", "version":0}))
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    for body in [
        json!({"registrationMode":"PUBLIC", "version":0}),
        json!({"registrationMode":"PUBLIC_EMAIL"}),
        json!({"registrationMode":"PUBLIC_EMAIL", "version":-1}),
        json!({"unexpected":true}),
        json!({"registrationMode":null, "version":0}),
        json!({"version":null}),
        json!([]),
    ] {
        let response = client
            .patch(&endpoint)
            .header(COOKIE, &cookies)
            .header("X-CSRF-Token", &csrf)
            .json(&body)
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
    }
    let saved = client
        .patch(&endpoint)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({"registrationMode":"PUBLIC_EMAIL", "version":0}))
        .send()
        .await?;
    assert_eq!(saved.status(), StatusCode::OK);
    assert_eq!(
        saved.json::<Value>().await?,
        json!({"registrationMode":"PUBLIC_EMAIL", "version":1})
    );
    let omitted = client
        .patch(&endpoint)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(omitted.status(), StatusCode::OK);
    assert_eq!(
        omitted.json::<Value>().await?,
        json!({"registrationMode":"PUBLIC_EMAIL", "version":1})
    );
    let stale = client
        .patch(&endpoint)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({"registrationMode":"INVITATION_ONLY", "version":0}))
        .send()
        .await?;
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert_eq!(
        stale.json::<Value>().await?["error"]["code"],
        "VERSION_CONFLICT"
    );
    let disabled = client
        .patch(&endpoint)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({"registrationMode":"INVITATION_ONLY", "version":1}))
        .send()
        .await?;
    assert_eq!(disabled.status(), StatusCode::OK);
    assert_eq!(
        disabled.json::<Value>().await?,
        json!({"registrationMode":"INVITATION_ONLY", "version":2})
    );
    let current = client
        .get(&endpoint)
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(
        current.json::<Value>().await?,
        json!({"registrationMode":"INVITATION_ONLY", "version":2})
    );
    assert_eq!(tokio::fs::read(credentials_path).await?, mail_sentinel);
    let audit = client
        .get(format!("{base}/api/v1/admin/audit"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json::<Value>()
        .await?;
    let events = audit["events"].as_array().ok_or("missing audit events")?;
    let changes: Vec<_> = events
        .iter()
        .filter(|event| event["metadata"].get("registrationMode").is_some())
        .collect();
    assert_eq!(changes.len(), 2);
    assert!(!audit.to_string().contains("smtp-test-user"));
    assert!(!audit.to_string().contains("smtp-test-password"));

    let too_large = client
        .patch(&endpoint)
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({"registrationMode":"x".repeat(4096), "version":2}))
        .send()
        .await?;
    assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn transcoding_settings_are_admin_only_validated_and_persisted()
-> Result<(), Box<dyn std::error::Error>> {
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
    users
        .create_user("viewer", "Viewer", "settings-test-password", false)
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
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(12))
        .build()?;
    let endpoint = format!("{base}/api/v1/admin/settings");
    let (admin_cookies, admin_csrf) = login(&client, &base, "admin").await?;
    let (viewer_cookies, viewer_csrf) = login(&client, &base, "viewer").await?;

    let initial = client
        .get(&endpoint)
        .header(COOKIE, &admin_cookies)
        .send()
        .await?;
    assert_eq!(initial.status(), StatusCode::OK);
    let initial = initial.json::<Value>().await?;
    let transcoding = initial["transcoding"].clone();
    assert_eq!(transcoding["enabled"], json!(true));
    assert_eq!(transcoding["hardwareEncoder"], json!("NONE"));
    assert_eq!(transcoding["remuxSessions"], json!(4));
    assert_eq!(transcoding["hardwareSessions"], json!(2));
    assert_eq!(transcoding["softwareSessions"], json!(1));
    assert_eq!(transcoding["softwarePreset"], json!("veryfast"));
    assert_eq!(transcoding["audioBitrateKbps"], json!(192));
    // Only encoders detected on the host are offered, so the list depends on the machine.
    let known = ["h264_nvenc", "h264_qsv", "h264_vaapi", "h264_videotoolbox"];
    assert!(
        initial["transcodingOptions"]["hardwareEncoders"]
            .as_array()
            .is_some_and(|encoders| encoders
                .iter()
                .all(|encoder| known.iter().any(|name| encoder == name)))
    );
    assert!(
        initial["transcodingOptions"]["softwarePresets"]
            .as_array()
            .is_some_and(|presets| presets.iter().any(|preset| preset == "veryfast"))
    );

    assert_eq!(initial["transcodingStatus"]["available"], json!(false));
    assert_eq!(initial["transcodingStatus"]["encoder"], Value::Null);
    let forbidden_probe = client
        .post(format!("{endpoint}/transcoding/probe"))
        .header(COOKIE, &viewer_cookies)
        .header("X-CSRF-Token", &viewer_csrf)
        .send()
        .await?;
    assert_eq!(forbidden_probe.status(), StatusCode::FORBIDDEN);

    let forbidden = client
        .patch(&endpoint)
        .header(COOKIE, &viewer_cookies)
        .header("X-CSRF-Token", &viewer_csrf)
        .json(&json!({ "transcoding": { "enabled": false } }))
        .send()
        .await?;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let invalid = client
        .patch(&endpoint)
        .header(COOKIE, &admin_cookies)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({ "transcoding": { "softwarePreset": "placebo" } }))
        .send()
        .await?;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid.json::<Value>().await?["error"]["message"],
        json!("服务端转码设置无效")
    );

    let saved = client
        .patch(&endpoint)
        .header(COOKIE, &admin_cookies)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({
            "transcoding": {
                "enabled": false,
                "hardwareEncoder": "h264_videotoolbox",
                "remuxSessions": 0,
                "hardwareSessions": 3,
                "softwareSessions": 2,
                "sessionQuotaMb": 1024,
                "globalBudgetMb": 4096,
                "minFreeSpaceMb": 256,
                "maxVideoBitrateKbps": 8000,
                "softwarePreset": "fast",
                "maxOutputHeight": 1080,
                "audioBitrateKbps": 256
            }
        }))
        .send()
        .await?;
    assert_eq!(saved.status(), StatusCode::OK);
    let saved = saved.json::<Value>().await?;
    assert_eq!(saved["transcoding"]["enabled"], json!(false));
    assert_eq!(
        saved["transcoding"]["hardwareEncoder"],
        json!("h264_videotoolbox")
    );
    assert_eq!(saved["transcoding"]["remuxSessions"], json!(0));
    assert_eq!(saved["transcoding"]["softwarePreset"], json!("fast"));
    assert_eq!(saved["transcoding"]["maxOutputHeight"], json!(1080));
    assert_eq!(saved["transcoding"]["audioBitrateKbps"], json!(256));
    assert_eq!(
        saved["transcoding"]["hardwareDevice"],
        json!("/dev/dri/renderD128")
    );
    assert_eq!(saved["transcoding"]["hardwareDecode"], json!(false));
    // Saving re-runs the self-test: this test host has no usable VideoToolbox/libva encoder.
    assert_eq!(
        saved["transcodingStatus"]["encoder"],
        json!("h264_videotoolbox")
    );
    assert_eq!(saved["transcodingStatus"]["available"], json!(false));

    let admin_probe = client
        .post(format!("{endpoint}/transcoding/probe"))
        .header(COOKIE, &admin_cookies)
        .header("X-CSRF-Token", &admin_csrf)
        .send()
        .await?;
    assert_eq!(admin_probe.status(), StatusCode::OK);
    let admin_probe = admin_probe.json::<Value>().await?;
    assert_eq!(
        admin_probe["transcodingStatus"]["encoder"],
        json!("h264_videotoolbox")
    );
    assert_eq!(admin_probe["transcodingStatus"]["available"], json!(false));

    // The stored row survives a fresh read, so a restart keeps the policy.
    let reloaded = client
        .get(&endpoint)
        .header(COOKIE, &admin_cookies)
        .send()
        .await?;
    assert_eq!(reloaded.status(), StatusCode::OK);
    let reloaded = reloaded.json::<Value>().await?;
    assert_eq!(reloaded["transcoding"], saved["transcoding"]);

    // Unrelated settings stay untouched by a transcoding-only patch.
    assert_eq!(reloaded["resumeMinTicks"], initial["resumeMinTicks"]);
    assert_eq!(reloaded["mediaStrategy"], initial["mediaStrategy"]);

    server.abort();
    Ok(())
}
