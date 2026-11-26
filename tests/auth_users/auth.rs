use luxd::{
    auth::{
        password::PasswordService,
        users::{UserStore, normalize_login_identity},
    },
    config::Config,
    storage::Database,
};

#[tokio::test]
async fn argon2id_hashes_and_verifies_without_plaintext_storage()
-> Result<(), Box<dyn std::error::Error>> {
    let passwords = PasswordService::new()?;
    let hash = passwords
        .hash_password("correct horse battery staple")
        .await?;

    assert!(hash.starts_with("$argon2id$"));
    assert!(
        passwords
            .verify_password(Some(&hash), "correct horse battery staple")
            .await?
    );
    assert!(
        !passwords
            .verify_password(Some(&hash), "wrong password")
            .await?
    );
    assert!(!passwords.verify_password(None, "wrong password").await?);
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

/// One normalization decides "which account is this", for storage, lookup and rate-limit keys.
///
/// The stored form has always been Unicode lowercasing after trimming. Exposing that exact rule
/// lets the login limiter key share it instead of re-implementing an ASCII-only copy, while
/// NFKC/NFC folding stays deliberately absent so existing usernames keep their meaning.
#[tokio::test]
async fn non_ascii_usernames_share_one_login_identity_normalization()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let store = UserStore::new(database.clone())?;
    let user = store
        .create_user(" Ädmin ", "Ädmin", "correct horse battery staple", false)
        .await?;

    assert_eq!(user.username_normalized, "ädmin");
    assert_eq!(normalize_login_identity("Ädmin"), "ädmin");
    assert_eq!(
        normalize_login_identity(" ÄDMIN "),
        user.username_normalized
    );
    assert_eq!(normalize_login_identity("ädmin"), user.username_normalized);
    // Compatibility forms are not folded; only trimming plus lowercasing applies.
    assert_ne!(
        normalize_login_identity("Ａdmin"),
        normalize_login_identity("admin")
    );

    assert!(
        store
            .authenticate("ädmin", "correct horse battery staple")
            .await?
            .is_some()
    );
    assert!(
        store
            .authenticate(" ÄDMIN ", "correct horse battery staple")
            .await?
            .is_some()
    );
    assert!(store.find_by_username("ÄDMIN").await?.is_some());
    let duplicate = store
        .create_user("ädmin", "Another", "another password", false)
        .await;
    assert!(duplicate.is_err());

    database.close().await;
    Ok(())
}
