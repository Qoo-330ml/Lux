use std::net::SocketAddr;

use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    security::LoginRateLimiter,
    storage::Database,
};
use serde_json::json;
use tokio::net::TcpListener;

#[tokio::test]
async fn login_rate_limiter_blocks_after_repeated_failures_and_resets_on_success() {
    let limiter = LoginRateLimiter::default();
    for _ in 0..5 {
        assert!(limiter.try_acquire("viewer@local").await);
    }
    assert!(!limiter.try_acquire("viewer@local").await);
    limiter.record_success("viewer@local").await;
    assert!(limiter.try_acquire("viewer@local").await);
}

async fn test_server(
    config: Config,
) -> Result<
    (
        String,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
        Database,
    ),
    Box<dyn std::error::Error>,
> {
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    // The peer address becomes the rate-limit key's first half, so every request made by this
    // test shares one peer exactly like a real browser behind one address.
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    Ok((format!("http://{address}"), server, database))
}

async fn login_status(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<reqwest::StatusCode, Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": username, "password": password }))
        .send()
        .await?;
    Ok(response.status())
}

/// `Ädmin` and `ädmin` authenticate the same account, so they must also share one failure budget.
///
/// Before the login key reused the stored-username normalization, `to_ascii_lowercase` left `Ä`
/// as `Ä` while the lookup lowered it to `ä`: an attacker got a fresh `MAX_FAILURES` window per
/// spelling of the same account.
#[tokio::test]
async fn web_login_rate_limits_every_non_ascii_username_spelling_of_one_account()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, database) = test_server(config).await?;
    let users = UserStore::new(database.clone())?;
    users
        .create_user("Ädmin", "Ädmin", "correct password", false)
        .await?;
    users
        .create_user("Control", "Control", "control password", false)
        .await?;
    let client = reqwest::Client::builder().no_proxy().build()?;

    // The canonical spelling works before the window is used, and doing so clears the counter.
    assert_eq!(
        login_status(&client, &base_url, "Ädmin", "correct password").await?,
        reqwest::StatusCode::OK
    );

    let spellings = ["Ädmin", "ädmin", " ÄDMIN ", "ädmin", "ÄDmin"];
    for spelling in spellings {
        assert_eq!(
            login_status(&client, &base_url, spelling, "wrong password").await?,
            reqwest::StatusCode::UNAUTHORIZED,
            "{spelling}"
        );
    }

    // Six failures in total for one identity: the correct password is now refused too.
    assert_eq!(
        login_status(&client, &base_url, "ädmin", "correct password").await?,
        reqwest::StatusCode::UNAUTHORIZED
    );
    // Another account from the same peer keeps its own budget, so the block is per identity.
    assert_eq!(
        login_status(&client, &base_url, "Control", "control password").await?,
        reqwest::StatusCode::OK
    );

    server.abort();
    Ok(())
}

/// A successful login inside the window clears what the failures before it accumulated.
#[tokio::test]
async fn web_login_success_clears_the_failure_window() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server, database) = test_server(config).await?;
    let users = UserStore::new(database.clone())?;
    users
        .create_user("Cycler", "Cycler", "correct password", false)
        .await?;
    let client = reqwest::Client::builder().no_proxy().build()?;

    for _ in 0..4 {
        assert_eq!(
            login_status(&client, &base_url, "cycler", "wrong password").await?,
            reqwest::StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        login_status(&client, &base_url, "cycler", "correct password").await?,
        reqwest::StatusCode::OK
    );
    for _ in 0..4 {
        assert_eq!(
            login_status(&client, &base_url, " cycler ", "wrong password").await?,
            reqwest::StatusCode::UNAUTHORIZED
        );
    }
    // Four fresh failures plus the earlier four would be eight; the success above reset them.
    assert_eq!(
        login_status(&client, &base_url, "CYCLER", "correct password").await?,
        reqwest::StatusCode::OK
    );

    server.abort();
    Ok(())
}
