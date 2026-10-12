use super::*;

#[derive(Default)]
struct ManifestDiscoveryPositiveIndexResult<'a> {
    created_items: usize,
    metadata_targets_changed: bool,
    add_count: i64,
    change_count: i64,
    reappeared_count: i64,
    applied_count: i64,
    indexed_paths: Vec<&'a str>,
    local_metadata_refs: Vec<(&'a str, &'a str)>,
}

struct ManifestDiscoveryPositiveIndexCommit<'a, 'b> {
    job_id: &'b str,
    library_id: &'b str,
    library_root_id: &'b str,
    generation: &'b str,
    positives: &'a [&'a NewScanManifestPositiveIndex],
    observations: &'a [&'a NewScanManifestEntry],
}

fn record_manifest_positive_applied<'a>(
    result: &mut ManifestDiscoveryPositiveIndexResult<'a>,
    positive: &'a NewScanManifestPositiveIndex,
) {
    result.indexed_paths.push(positive.relative_path.as_str());
    result.local_metadata_refs.push((
        positive.relative_path.as_str(),
        manifest_positive_file_filesystem_entry_id(&positive.file),
    ));
    result.applied_count = result.applied_count.saturating_add(1);
    match positive.delta_kind.as_str() {
        "ADD" => result.add_count = result.add_count.saturating_add(1),
        "CHANGE" => result.change_count = result.change_count.saturating_add(1),
        "REAPPEARED" => result.reappeared_count = result.reappeared_count.saturating_add(1),
        _ => {}
    }
}

fn manifest_positive_file_filesystem_entry_id(file: &NewScanManifestIndexedFile) -> &str {
    match file {
        NewScanManifestIndexedFile::Movie(file) => &file.filesystem_entry_id,
        NewScanManifestIndexedFile::Episode(file) => &file.filesystem_entry_id,
        NewScanManifestIndexedFile::Unresolved(file) => &file.filesystem_entry_id,
        NewScanManifestIndexedFile::Sidecar(file) => &file.filesystem_entry_id,
    }
}

#[path = "manifest/indexes.rs"]
mod indexes;

#[path = "manifest/discovery.rs"]
mod discovery;

#[path = "manifest/targets.rs"]
mod targets;

#[path = "manifest/state.rs"]
mod state;
