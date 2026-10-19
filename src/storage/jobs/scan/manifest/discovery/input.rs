use super::*;

pub(super) struct ManifestDiscoveryInput<'a, 'b> {
    pub(super) chunk: &'a NewScanManifestDiscoveryChunk<'b>,
    pub(super) child_directories: Vec<&'a str>,
    pub(super) entries: Vec<&'a NewScanManifestEntry>,
    pub(super) completed_directories: Vec<&'a str>,
    pub(super) positive_indexes: Vec<&'a NewScanManifestPositiveIndex>,
    pub(super) unchanged_paths: Vec<&'a str>,
    pub(super) seen_filesystem_entries: Vec<&'a NewScanManifestSeenFilesystemEntry>,
}

impl<'a, 'b> ManifestDiscoveryInput<'a, 'b>
where
    'b: 'a,
{
    pub(super) fn collect(
        chunks: &'a [NewScanManifestDiscoveryChunk<'b>],
    ) -> Result<Option<Self>, StorageError> {
        let Some(chunk) = chunks.first() else {
            return Ok(None);
        };
        let mut child_directories = Vec::new();
        let mut entries = Vec::new();
        let mut completed_directories = Vec::new();
        let mut positive_indexes = Vec::new();
        let mut unchanged_paths = Vec::new();
        let mut seen_filesystem_entries = Vec::new();
        for candidate in chunks {
            if candidate.manifest_id != chunk.manifest_id
                || candidate.job_id != chunk.job_id
                || candidate.library_root_id != chunk.library_root_id
            {
                return Err(StorageError::Conflict(
                    "manifest discovery transaction cannot span jobs or roots".to_owned(),
                ));
            }
            child_directories.extend(candidate.child_directories.iter().map(String::as_str));
            entries.extend(candidate.entries.iter());
            positive_indexes.extend(candidate.positive_indexes.iter());
            unchanged_paths.extend(candidate.unchanged_paths.iter().map(String::as_str));
            seen_filesystem_entries.extend(candidate.seen_filesystem_entries.iter());
            if let Some(directory) = candidate.completed_directory {
                completed_directories.push(directory);
            }
        }
        if child_directories.iter().any(|path| {
            let path = std::path::Path::new(path);
            path.is_absolute()
                || path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
        }) || entries.iter().any(|entry| {
            let path = std::path::Path::new(&entry.relative_path);
            !matches!(entry.entry_kind.as_str(), "FILE" | "DIRECTORY")
                || entry.size < 0
                || entry.fingerprint_pending
                || path.is_absolute()
                || path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
        }) {
            return Err(StorageError::Conflict(
                "manifest discovery chunk contains an invalid relative path or observation"
                    .to_owned(),
            ));
        }
        let mut observed_paths = std::collections::HashSet::with_capacity(entries.len());
        if entries
            .iter()
            .any(|entry| !observed_paths.insert(entry.relative_path.as_str()))
        {
            return Err(StorageError::Conflict(
                "manifest discovery chunk contains duplicate observation paths".to_owned(),
            ));
        }
        let observations_by_path = entries
            .iter()
            .map(|entry| (entry.relative_path.as_str(), *entry))
            .collect::<HashMap<_, _>>();
        let mut indexed_paths = std::collections::HashSet::with_capacity(positive_indexes.len());
        for positive in &positive_indexes {
            let observed_path = manifest_positive_file_path(&positive.file);
            let Some(observation) = observations_by_path.get(positive.relative_path.as_str())
            else {
                return Err(StorageError::Conflict(
                    "manifest positive index is missing its observation".to_owned(),
                ));
            };
            if observed_path != positive.relative_path
                || observation.entry_kind != "FILE"
                || !indexed_paths.insert(positive.relative_path.as_str())
            {
                return Err(StorageError::Conflict(
                    "manifest positive index does not match a unique file observation".to_owned(),
                ));
            }
            let valid_baseline = match positive.delta_kind.as_str() {
                "ADD" => {
                    positive.base_filesystem_entry_id.is_none()
                        && positive.base_fingerprint.is_none()
                }
                "CHANGE" | "REAPPEARED" => positive.base_filesystem_entry_id.is_some(),
                _ => false,
            };
            if !valid_baseline {
                return Err(StorageError::Conflict(
                    "manifest positive index has an invalid baseline".to_owned(),
                ));
            }
        }
        let mut unchanged_path_set =
            std::collections::HashSet::with_capacity(unchanged_paths.len());
        for path in &unchanged_paths {
            if observations_by_path
                .get(path)
                .is_none_or(|observation| observation.entry_kind != "FILE")
                || indexed_paths.contains(path)
                || !unchanged_path_set.insert(*path)
            {
                return Err(StorageError::Conflict(
                    "manifest unchanged path does not match a unique unindexed file observation"
                        .to_owned(),
                ));
            }
        }
        let mut seen_filesystem_entry_paths =
            std::collections::HashSet::with_capacity(seen_filesystem_entries.len());
        for seen in &seen_filesystem_entries {
            let Some(observation) = observations_by_path.get(seen.relative_path.as_str()) else {
                return Err(StorageError::Conflict(
                    "manifest seen filesystem entry does not match a unique unindexed file observation"
                        .to_owned(),
                ));
            };
            if observation.entry_kind != "FILE"
                || indexed_paths.contains(seen.relative_path.as_str())
                || !seen_filesystem_entry_paths.insert(seen.relative_path.as_str())
                || seen.filesystem_entry_id.is_empty()
                || seen.fingerprint != observation.fingerprint
            {
                return Err(StorageError::Conflict(
                    "manifest seen filesystem entry does not match a unique unindexed file observation"
                        .to_owned(),
                ));
            }
        }
        Ok(Some(Self {
            chunk,
            child_directories,
            entries,
            completed_directories,
            positive_indexes,
            unchanged_paths,
            seen_filesystem_entries,
        }))
    }
}
