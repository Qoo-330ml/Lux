use super::*;

#[path = "discovery/frontier.rs"]
mod frontier;
#[path = "discovery/input.rs"]
mod input;

#[path = "discovery/checkpoint.rs"]
mod checkpoint;
#[path = "discovery/counters.rs"]
mod counters;
#[path = "discovery/indexing.rs"]
mod indexing;

#[path = "discovery/paths.rs"]
mod paths;

#[path = "discovery/presence.rs"]
mod presence;

#[path = "discovery/coordinator.rs"]
mod coordinator;

#[path = "discovery/state.rs"]
mod state;
fn manifest_positive_file_path(file: &NewScanManifestIndexedFile) -> &str {
    match file {
        NewScanManifestIndexedFile::Movie(file) => &file.relative_path,
        NewScanManifestIndexedFile::Episode(file) => &file.relative_path,
        NewScanManifestIndexedFile::Unresolved(file) => &file.relative_path,
        NewScanManifestIndexedFile::Sidecar(file) => &file.relative_path,
    }
}
