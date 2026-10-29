use luxd::{
    application::setup::SetupService,
    auth::emby::{EmbyAuthService, EmbyDeviceInfo},
    config::Config,
    storage::Database,
};
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

fn device() -> EmbyDeviceInfo {
    EmbyDeviceInfo {
        client: "Lux Web".to_owned(),
        device: "Browser".to_owned(),
        device_id: "0d9f6f0e-0b7e-4f3e-9d6b-0f6f2d2a8c11".to_owned(),
        version: "1".to_owned(),
        user_id: None,
    }
}

async fn auth_with_user() -> Result<
    (
        tempfile::TempDir,
        EmbyAuthService,
        luxd::auth::users::UserRecord,
    ),
    Box<dyn std::error::Error>,
> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    SetupService::new(database.clone())?
        .complete("Admin", "Administrator", "correct password")
        .await?;
    let auth = EmbyAuthService::new(database)?;
    let login = auth
        .authenticate("Admin", "correct password", &device())
        .await?
        .ok_or("login failed")?;
    Ok((temp_dir, auth, login.user))
}

#[tokio::test]
async fn playback_token_is_rejected_by_the_default_resolver_and_accepted_on_emby_routes()
-> Result<(), Box<dyn std::error::Error>> {
    let (_dir, auth, user) = auth_with_user().await?;
    let token = auth
        .issue_playback_token(&user, "play-session-1", &device(), now() + 600, None)
        .await?
        .ok_or("token was not issued")?;

    assert!(token.starts_with("luxpb_"));
    // Everything that is not an Emby route (Lux API, web playback, ...) keeps using the default
    // resolver, so a playback token must never authenticate there.
    assert!(auth.resolve_token_with_ip(&token, None).await?.is_none());
    let resolved = auth
        .resolve_emby_route_token_with_ip(&token, None)
        .await?
        .ok_or("route resolver rejected the token")?;
    assert_eq!(resolved.id, user.id);
    Ok(())
}

#[tokio::test]
async fn ordinary_tokens_resolve_through_both_resolvers() -> Result<(), Box<dyn std::error::Error>>
{
    let (_dir, auth, user) = auth_with_user().await?;
    let login = auth
        .authenticate("Admin", "correct password", &device())
        .await?
        .ok_or("login failed")?;
    assert!(!login.token.starts_with("luxpb_"));
    assert_eq!(
        auth.resolve_token_with_ip(&login.token, None)
            .await?
            .map(|found| found.id),
        Some(user.id)
    );
    assert_eq!(
        auth.resolve_emby_route_token_with_ip(&login.token, None)
            .await?
            .map(|found| found.id),
        Some(user.id)
    );
    Ok(())
}

#[tokio::test]
async fn playback_token_stops_working_when_expired_or_revoked()
-> Result<(), Box<dyn std::error::Error>> {
    let (_dir, auth, user) = auth_with_user().await?;

    let expired = auth
        .issue_playback_token(&user, "play-session-expired", &device(), now() - 10, None)
        .await?;
    if let Some(expired) = expired {
        assert!(
            auth.resolve_emby_route_token_with_ip(&expired, None)
                .await?
                .is_none()
        );
    }

    let first = auth
        .issue_playback_token(&user, "play-session-2", &device(), now() + 600, None)
        .await?
        .ok_or("token was not issued")?;
    let other = auth
        .issue_playback_token(&user, "play-session-3", &device(), now() + 600, None)
        .await?
        .ok_or("token was not issued")?;
    assert_ne!(first, other);

    auth.revoke_playback_tokens("play-session-2").await?;
    assert!(
        auth.resolve_emby_route_token_with_ip(&first, None)
            .await?
            .is_none()
    );
    // Revoking one playback session leaves the others alone.
    assert!(
        auth.resolve_emby_route_token_with_ip(&other, None)
            .await?
            .is_some()
    );
    Ok(())
}

#[tokio::test]
async fn playback_token_is_not_listed_as_a_login_session() -> Result<(), Box<dyn std::error::Error>>
{
    let (_dir, auth, user) = auth_with_user().await?;
    let user_id = user.id.to_string();
    let before = auth.list_admin_login_sessions(&user_id, 50, 0).await?.1;
    auth.issue_playback_token(&user, "play-session-4", &device(), now() + 600, None)
        .await?
        .ok_or("token was not issued")?;
    let after = auth.list_admin_login_sessions(&user_id, 50, 0).await?.1;
    assert_eq!(before, after);
    Ok(())
}

#[tokio::test]
async fn playback_token_resolves_through_the_route_resolver_with_an_admin_user()
-> Result<(), Box<dyn std::error::Error>> {
    let (_dir, auth, user) = auth_with_user().await?;
    let token = auth
        .issue_playback_token(&user, "play-session-9", &device(), now() + 600, None)
        .await?
        .ok_or("token was not issued")?;
    assert!(
        auth.resolve_emby_route_token_with_ip(&token, None)
            .await?
            .is_some(),
        "route resolver rejected a freshly issued playback token"
    );
    Ok(())
}
