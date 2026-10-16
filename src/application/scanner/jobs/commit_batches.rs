use super::PendingManifestDirectoryChunk;
use std::collections::HashSet;

pub(super) const DEFAULT_SQLITE_MANIFEST_TRANSACTION_FILE_BUDGET: usize = 8_000;

#[derive(Clone, Copy, Debug)]
pub(super) struct ManifestDiscoveryTransactionBudget {
    pub(super) max_entries: usize,
    pub(super) max_files: usize,
    pub(super) max_child_directories: usize,
    pub(super) max_completed_directories: usize,
}

impl ManifestDiscoveryTransactionBudget {
    pub(super) fn sqlite(
        max_files: usize,
        directory_observation_slack: usize,
        max_child_directories: usize,
        max_completed_directories: usize,
    ) -> Self {
        let max_files = max_files.max(1);
        Self {
            max_entries: max_files.saturating_add(directory_observation_slack).max(1),
            max_files,
            max_child_directories: max_child_directories.max(1),
            max_completed_directories: max_completed_directories.max(1),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct TransactionCounts {
    entries: usize,
    files: usize,
    child_directories: usize,
    completed_directories: usize,
}

impl TransactionCounts {
    fn exceeds(self, delta: Self, budget: ManifestDiscoveryTransactionBudget) -> bool {
        self.entries.saturating_add(delta.entries) > budget.max_entries
            || self.files.saturating_add(delta.files) > budget.max_files
            || self
                .child_directories
                .saturating_add(delta.child_directories)
                > budget.max_child_directories
            || self
                .completed_directories
                .saturating_add(delta.completed_directories)
                > budget.max_completed_directories
    }

    fn add(&mut self, delta: Self) {
        self.entries = self.entries.saturating_add(delta.entries);
        self.files = self.files.saturating_add(delta.files);
        self.child_directories = self
            .child_directories
            .saturating_add(delta.child_directories);
        self.completed_directories = self
            .completed_directories
            .saturating_add(delta.completed_directories);
    }
}

pub(super) fn split_pending_manifest_directory_chunks(
    chunks: Vec<PendingManifestDirectoryChunk>,
    budget: ManifestDiscoveryTransactionBudget,
) -> Vec<Vec<PendingManifestDirectoryChunk>> {
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    let mut counts = TransactionCounts::default();

    for chunk in chunks {
        let child_paths = chunk
            .child_directories
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let mut matched_child_paths = HashSet::new();
        let mut piece = Some(empty_piece(&chunk));

        for entry in chunk.entries {
            let child_directory =
                if entry.entry_kind == "DIRECTORY" && child_paths.contains(&entry.relative_path) {
                    matched_child_paths.insert(entry.relative_path.clone());
                    Some(entry.relative_path.clone())
                } else {
                    None
                };
            let delta = TransactionCounts {
                entries: 1,
                files: usize::from(entry.entry_kind == "FILE"),
                child_directories: usize::from(child_directory.is_some()),
                completed_directories: 0,
            };
            if counts.exceeds(delta, budget) {
                flush_piece(&mut piece, &mut batch);
                flush_batch(&mut batch, &mut batches, &mut counts);
            }

            let current_piece = piece.get_or_insert_with(|| {
                empty_piece_from_parts(
                    &chunk.relative_directory,
                    &chunk.root_observation,
                    &chunk.directory_observation,
                )
            });
            current_piece.entries.push(entry);
            if let Some(child_directory) = child_directory {
                current_piece.child_directories.push(child_directory);
            }
            counts.add(delta);
        }

        for child_directory in chunk.child_directories {
            if matched_child_paths.contains(&child_directory) {
                continue;
            }
            let delta = TransactionCounts {
                child_directories: 1,
                ..TransactionCounts::default()
            };
            if counts.exceeds(delta, budget) {
                flush_piece(&mut piece, &mut batch);
                flush_batch(&mut batch, &mut batches, &mut counts);
            }
            let current_piece = piece.get_or_insert_with(|| {
                empty_piece_from_parts(
                    &chunk.relative_directory,
                    &chunk.root_observation,
                    &chunk.directory_observation,
                )
            });
            current_piece.child_directories.push(child_directory);
            counts.add(delta);
        }

        if let Some(completed_directory) = chunk.completed_directory {
            let delta = TransactionCounts {
                completed_directories: 1,
                ..TransactionCounts::default()
            };
            if counts.exceeds(delta, budget) {
                flush_piece(&mut piece, &mut batch);
                flush_batch(&mut batch, &mut batches, &mut counts);
            }
            let current_piece = piece.get_or_insert_with(|| {
                empty_piece_from_parts(
                    &chunk.relative_directory,
                    &chunk.root_observation,
                    &chunk.directory_observation,
                )
            });
            current_piece.completed_directory = Some(completed_directory);
            counts.add(delta);
        }

        flush_piece(&mut piece, &mut batch);
    }

    flush_batch(&mut batch, &mut batches, &mut counts);
    batches
}

fn empty_piece(source: &PendingManifestDirectoryChunk) -> PendingManifestDirectoryChunk {
    empty_piece_from_parts(
        &source.relative_directory,
        &source.root_observation,
        &source.directory_observation,
    )
}

fn empty_piece_from_parts(
    relative_directory: &str,
    root_observation: &crate::storage::NewScanManifestEntry,
    directory_observation: &crate::storage::NewScanManifestEntry,
) -> PendingManifestDirectoryChunk {
    PendingManifestDirectoryChunk {
        relative_directory: relative_directory.to_owned(),
        root_observation: root_observation.clone(),
        directory_observation: directory_observation.clone(),
        child_directories: Vec::new(),
        entries: Vec::new(),
        completed_directory: None,
    }
}

fn flush_piece(
    piece: &mut Option<PendingManifestDirectoryChunk>,
    batch: &mut Vec<PendingManifestDirectoryChunk>,
) {
    let Some(piece) = piece.take() else {
        return;
    };
    if !piece.entries.is_empty()
        || !piece.child_directories.is_empty()
        || piece.completed_directory.is_some()
    {
        batch.push(piece);
    }
}

fn flush_batch(
    batch: &mut Vec<PendingManifestDirectoryChunk>,
    batches: &mut Vec<Vec<PendingManifestDirectoryChunk>>,
    counts: &mut TransactionCounts,
) {
    if !batch.is_empty() {
        batches.push(std::mem::take(batch));
        *counts = TransactionCounts::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::NewScanManifestEntry;

    #[test]
    fn sqlite_transaction_budget_is_bounded_independently_from_reader() {
        const {
            assert!(DEFAULT_SQLITE_MANIFEST_TRANSACTION_FILE_BUDGET > 0);
            assert!(
                DEFAULT_SQLITE_MANIFEST_TRANSACTION_FILE_BUDGET
                    <= super::super::MANIFEST_STREAMED_INDEX_BATCH_SIZE
            );
            assert!(super::super::MANIFEST_STREAMED_INDEX_BATCH_SIZE == 8_000);
        }
    }

    fn observation(relative_path: &str, entry_kind: &str) -> NewScanManifestEntry {
        NewScanManifestEntry {
            relative_path: relative_path.to_owned(),
            entry_kind: entry_kind.to_owned(),
            size: 1,
            modified_at: 1,
            device: Some(1),
            inode: Some(1),
            fingerprint: vec![1],
        }
    }

    fn chunk(
        relative_directory: &str,
        entries: Vec<NewScanManifestEntry>,
        child_directories: Vec<String>,
        completed_directory: Option<&str>,
    ) -> PendingManifestDirectoryChunk {
        PendingManifestDirectoryChunk {
            relative_directory: relative_directory.to_owned(),
            root_observation: observation("", "DIRECTORY"),
            directory_observation: observation(relative_directory, "DIRECTORY"),
            child_directories,
            entries,
            completed_directory: completed_directory.map(str::to_owned),
        }
    }

    #[test]
    fn split_respects_all_budgets_and_keeps_child_observations_together() {
        let budget = ManifestDiscoveryTransactionBudget::sqlite(2, 1, 1, 1);
        let chunks = vec![chunk(
            "Films",
            vec![
                observation("Films/A.mkv", "FILE"),
                observation("Films/Child A", "DIRECTORY"),
                observation("Films/B.mkv", "FILE"),
                observation("Films/C.mkv", "FILE"),
                observation("Films/Child B", "DIRECTORY"),
            ],
            vec!["Films/Child A".to_owned(), "Films/Child B".to_owned()],
            Some("Films"),
        )];

        let batches = split_pending_manifest_directory_chunks(chunks, budget);
        assert!(batches.len() >= 2);
        let mut file_paths = Vec::new();
        let mut child_paths = Vec::new();
        let mut completed_directories = Vec::new();
        for batch in &batches {
            let entries = batch.iter().map(|chunk| chunk.entries.len()).sum::<usize>();
            let files = batch
                .iter()
                .flat_map(|chunk| &chunk.entries)
                .filter(|entry| entry.entry_kind == "FILE")
                .count();
            let children = batch
                .iter()
                .map(|chunk| chunk.child_directories.len())
                .sum::<usize>();
            let completed = batch
                .iter()
                .filter(|chunk| chunk.completed_directory.is_some())
                .count();
            assert!(entries <= budget.max_entries);
            assert!(files <= budget.max_files);
            assert!(children <= budget.max_child_directories);
            assert!(completed <= budget.max_completed_directories);

            for chunk in batch {
                for child in &chunk.child_directories {
                    assert!(chunk.entries.iter().any(|entry| {
                        entry.entry_kind == "DIRECTORY" && entry.relative_path == *child
                    }));
                    child_paths.push(child.clone());
                }
                file_paths.extend(
                    chunk
                        .entries
                        .iter()
                        .filter(|entry| entry.entry_kind == "FILE")
                        .map(|entry| entry.relative_path.clone()),
                );
                if let Some(completed) = &chunk.completed_directory {
                    completed_directories.push(completed.clone());
                }
            }
        }
        assert_eq!(
            file_paths,
            vec![
                "Films/A.mkv".to_owned(),
                "Films/B.mkv".to_owned(),
                "Films/C.mkv".to_owned()
            ]
        );
        assert_eq!(
            batches[0]
                .iter()
                .flat_map(|chunk| &chunk.entries)
                .filter(|entry| entry.entry_kind == "FILE")
                .count(),
            budget.max_files,
            "a transaction may use the exact file limit before the next item splits"
        );
        assert_eq!(
            child_paths,
            vec!["Films/Child A".to_owned(), "Films/Child B".to_owned()]
        );
        assert_eq!(completed_directories, vec!["Films"]);
        assert!(batches.last().is_some_and(|batch| {
            batch
                .iter()
                .any(|chunk| chunk.completed_directory.as_deref() == Some("Films"))
        }));
    }

    #[test]
    fn split_handles_empty_and_limit_plus_one_chunks_in_order() {
        let budget = ManifestDiscoveryTransactionBudget::sqlite(2, 0, 2, 2);
        assert!(split_pending_manifest_directory_chunks(Vec::new(), budget).is_empty());

        let chunks = vec![
            chunk(
                "",
                vec![observation("A.mkv", "FILE"), observation("B.mkv", "FILE")],
                Vec::new(),
                None,
            ),
            chunk("", vec![observation("C.mkv", "FILE")], Vec::new(), Some("")),
        ];
        let batches = split_pending_manifest_directory_chunks(chunks, budget);
        let paths = batches
            .iter()
            .flat_map(|batch| batch.iter())
            .flat_map(|chunk| &chunk.entries)
            .filter(|entry| entry.entry_kind == "FILE")
            .map(|entry| entry.relative_path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(paths, vec!["A.mkv", "B.mkv", "C.mkv"]);
        assert_eq!(
            batches
                .iter()
                .flatten()
                .filter(|chunk| chunk.completed_directory.is_some())
                .count(),
            1
        );
    }

    #[test]
    fn split_preserves_a_single_entry_and_empty_directory_completion() {
        let budget = ManifestDiscoveryTransactionBudget::sqlite(1, 0, 1, 1);
        let batches = split_pending_manifest_directory_chunks(
            vec![
                chunk(
                    "One",
                    vec![observation("One/A.mkv", "FILE")],
                    Vec::new(),
                    None,
                ),
                chunk("Empty", Vec::new(), Vec::new(), Some("Empty")),
            ],
            budget,
        );

        assert_eq!(batches.len(), 1);
        assert_eq!(
            batches[0]
                .iter()
                .flat_map(|chunk| &chunk.entries)
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["One/A.mkv"]
        );
        assert_eq!(
            batches[0]
                .iter()
                .filter_map(|chunk| chunk.completed_directory.as_deref())
                .collect::<Vec<_>>(),
            vec!["Empty"]
        );
    }
}
