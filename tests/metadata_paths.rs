use std::path::Path;

use luxd::application::metadata_paths::{
    library_item_directory, metadata_root, people_directory, people_index_path,
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
}

#[test]
fn metadata_paths_reject_traversal_components() {
    let config = Path::new("/config");
    assert!(library_item_directory(config, "../item").is_err());
    assert!(people_directory(config, "person", "tmdb", "../person").is_err());
    assert!(people_index_path(config, "person/id").is_err());
}
