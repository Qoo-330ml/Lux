use luxd::application::{
    metadata_objects::{MetadataObjectError, MetadataObjectSnapshot, MetadataObjectStore},
    metadata_paths::{MetadataObjectKind, metadata_object_directory},
};
use serde_json::Value;

#[tokio::test]
async fn metadata_object_store_writes_a_rebuildable_snapshot()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let store = MetadataObjectStore::new(config_dir.clone());
    let report = store
        .write_snapshot(
            MetadataObjectSnapshot::new(MetadataObjectKind::Genre, "科幻 / 冒险", "TMDb", "878")?
                .with_overview("A genre snapshot")
                .with_member_count(3),
        )
        .await?;

    let expected_directory = metadata_object_directory(
        &config_dir,
        MetadataObjectKind::Genre,
        "科幻 / 冒险",
        "TMDb",
        "878",
    )?;
    assert_eq!(
        report.path,
        tokio::fs::canonicalize(&expected_directory)
            .await?
            .join("genre.json")
    );
    let bytes = tokio::fs::read(&report.path).await?;
    let snapshot: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(snapshot["kind"], "genres");
    assert_eq!(snapshot["displayName"], "科幻 / 冒险");
    assert_eq!(snapshot["provider"], "tmdb");
    assert_eq!(snapshot["objectId"], "878");
    assert_eq!(snapshot["overview"], "A genre snapshot");
    assert_eq!(snapshot["memberCount"], 3);
    let mut entries = tokio::fs::read_dir(expected_directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        assert!(!entry.file_name().to_string_lossy().starts_with(".lux-"));
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn metadata_object_store_rejects_a_symlinked_metadata_parent()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    tokio::fs::create_dir_all(&config_dir).await?;
    let external = temp_dir.path().join("external");
    tokio::fs::create_dir_all(&external).await?;
    symlink(&external, config_dir.join("metadata"))?;

    let store = MetadataObjectStore::new(config_dir);
    let error = store
        .write_snapshot(MetadataObjectSnapshot::new(
            MetadataObjectKind::Tag,
            "Drama",
            "local",
            "drama",
        )?)
        .await
        .expect_err("metadata parent symlink should be rejected");
    assert!(matches!(error, MetadataObjectError::SymlinkTarget(_)));
    assert!(!external.join("tags").exists());
    Ok(())
}

#[test]
fn metadata_object_snapshot_rejects_empty_identity() {
    let result = MetadataObjectSnapshot::new(MetadataObjectKind::Tag, "Drama", "local", "");
    assert!(result.is_err());
}

#[tokio::test]
async fn metadata_object_store_rejects_an_oversized_snapshot()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let store = MetadataObjectStore::new(temp_dir.path().join("config"));
    let error = store
        .write_snapshot(
            MetadataObjectSnapshot::new(MetadataObjectKind::Studio, "Studio", "local", "studio")?
                .with_overview("x".repeat(300_000)),
        )
        .await
        .expect_err("oversized metadata snapshot should be rejected");
    assert!(matches!(error, MetadataObjectError::TooLarge));
    Ok(())
}
