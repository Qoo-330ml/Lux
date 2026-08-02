use luxd::{
    auth::{password::PasswordService, users::UserStore},
    config::Config,
    storage::Database,
};

#[test]
fn argon2id_hashes_and_verifies_without_plaintext_storage() -> Result<(), Box<dyn std::error::Error>>
{
    let passwords = PasswordService::new()?;
    let hash = passwords.hash_password("correct horse battery staple")?;

    assert!(hash.starts_with("$argon2id$"));
    assert!(passwords.verify_password(Some(&hash), "correct horse battery staple")?);
    assert!(!passwords.verify_password(Some(&hash), "wrong password")?);
    assert!(!passwords.verify_password(None, "wrong password")?);
    assert!(!hash.contains("correct horse battery staple"));
    Ok(())
}

#[tokio::test]
async fn usernames_are_normalized_unique_and_authentication_isolated()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let store = UserStore::new(database.clone())?;
    let user = store
        .create_user("  Alice ", "Alice", "correct horse battery staple", true)
        .await?;

    assert_eq!(user.username_normalized, "alice");
    assert!(user.is_admin);
    assert!(
        store
            .authenticate("ALICE", "correct horse battery staple")
            .await?
            .is_some()
    );
    assert!(
        store
            .authenticate("alice", "wrong password")
            .await?
            .is_none()
    );
    assert!(
        store
            .authenticate("missing", "wrong password")
            .await?
            .is_none()
    );

    let duplicate = store
        .create_user("alice", "Another Alice", "another password", false)
        .await;
    assert!(duplicate.is_err());

    database.close().await;
    Ok(())
}
