use luxd::{
    auth::{admin_api_key::AdminApiKeyService, users::UserStore},
    config::Config,
    storage::Database,
};

#[tokio::test]
async fn shared_admin_key_survives_restart_and_can_be_revoked()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let users = UserStore::new(database.clone())?;
    let admin = users
        .create_initial_admin("Admin", "Administrator", "correct horse battery staple")
        .await?;
    let service = AdminApiKeyService::new(config.config_dir.clone(), database.clone());

    assert!(service.current().await?.is_none());

    let key = service.rotate().await?;
    assert!(key.starts_with("lux_"));
    assert_eq!(service.current().await?.as_deref(), Some(key.as_str()));
    assert_eq!(
        service.resolve(&key).await?.map(|user| user.id),
        Some(admin.id)
    );

    let restarted = AdminApiKeyService::new(config.config_dir.clone(), database.clone());
    assert_eq!(
        restarted.resolve(&key).await?.map(|user| user.id),
        Some(admin.id)
    );

    service.revoke().await?;
    assert!(service.current().await?.is_none());
    assert!(service.resolve(&key).await?.is_none());

    database.close().await;
    Ok(())
}
