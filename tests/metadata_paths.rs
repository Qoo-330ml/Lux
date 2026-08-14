use std::path::Path;

use luxd::application::metadata_paths::{
    MetadataObjectKind, canonical_person_directory, library_item_directory,
    metadata_object_directory, metadata_root, people_directory, people_index_path,
    people_index_path_for_provider,
};

#[test]
fn metadata_paths_use_the_unified_root_and_stable_shards() {
    let config = Path::new("/config");
    assert_eq!(metadata_root(config), Path::new("/config/metadata"));

    let path = library_item_directory(config, "item-123").expect("valid item ID");
    assert_eq!(
        path.file_name().and_then(|value| value.to_str()),
        Some("item-123")
    );
    assert_eq!(
        path.parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|value| value.to_str()),
        Some("library")
    );
    assert_eq!(path.components().count(), 6);
}

#[test]
fn people_paths_are_readable_but_identity_uses_provider_and_id() {
    let config = Path::new("/config");
    let path = people_directory(config, "阿·米切尔", "TMDb", "1391125").expect("valid person");
    assert_eq!(
        path,
        Path::new("/config/metadata/people/阿/阿·米切尔-tmdb-1391125")
    );
    assert_eq!(
        people_index_path(config, "1391125").expect("valid person ID"),
        Path::new("/config/metadata/people/index/1391125.json")
    );
    assert_eq!(
        people_index_path_for_provider(config, "TMDb", "1391125").expect("valid provider ID"),
        Path::new("/config/metadata/people/index/tmdb-1391125.json")
    );
}

#[test]
fn canonical_person_paths_are_provider_independent() {
    let config = Path::new("/config");
    let path = canonical_person_directory(config, "person-abc123").expect("valid person key");
    assert_eq!(
        path.file_name().and_then(|value| value.to_str()),
        Some("person-abc123")
    );
    assert_eq!(
        path.parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|value| value.to_str()),
        Some("person")
    );
}

#[test]
fn metadata_paths_reject_traversal_components() {
    let config = Path::new("/config");
    assert!(library_item_directory(config, "../item").is_err());
    assert!(people_directory(config, "person", "tmdb", "../person").is_err());
    assert!(people_index_path(config, "person/id").is_err());
    assert!(people_index_path_for_provider(config, "tmdb/provider", "person").is_err());
}

#[test]
fn metadata_object_paths_keep_the_kind_and_human_readable_identity() {
    let config = Path::new("/config");
    let path = metadata_object_directory(
        config,
        MetadataObjectKind::Genre,
        "科幻 / 冒险",
        "TMDb",
        "878",
    )
    .expect("valid metadata object");
    assert_eq!(
        path,
        Path::new("/config/metadata/genres/科/科幻-_-冒险-tmdb-878")
    );
}

#[test]
fn metadata_object_kinds_use_independent_directories() {
    let config = Path::new("/config");
    let kinds = [
        (MetadataObjectKind::Collection, "collections"),
        (MetadataObjectKind::Genre, "genres"),
        (MetadataObjectKind::Studio, "studios"),
        (MetadataObjectKind::Tag, "tags"),
    ];
    for (kind, directory) in kinds {
        let path = metadata_object_directory(config, kind, "Drama", "local", "drama")
            .expect("valid metadata object");
        assert_eq!(
            path.components()
                .nth(3)
                .and_then(|value| value.as_os_str().to_str()),
            Some(directory)
        );
    }
}

#[test]
fn metadata_object_paths_reject_unsafe_identity_components() {
    let config = Path::new("/config");
    assert!(
        metadata_object_directory(config, MetadataObjectKind::Tag, "tag", "local", "../tag")
            .is_err()
    );
    assert!(
        metadata_object_directory(
            config,
            MetadataObjectKind::Tag,
            "tag",
            "local/provider",
            "tag"
        )
        .is_err()
    );
    assert!(
        metadata_object_directory(config, MetadataObjectKind::Tag, "", "local", "tag").is_err()
    );
}
