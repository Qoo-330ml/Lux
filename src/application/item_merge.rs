use crate::storage::{
    Database, MediaMergeOrigin, StorageError, StoredIdentityMergeCandidateItem,
    StoredIdentityMergeCandidateKey, StoredMediaMerge, StoredMediaMergeRecord,
    StoredMediaMergeUndo,
};

/// How many duplicate groups one automatic pass is allowed to merge.
pub(crate) const IDENTITY_MERGE_GROUP_BUDGET: usize = 25;
/// Keys are cheap to read; the page size keeps a pass from loading an unbounded key list.
const IDENTITY_MERGE_KEY_PAGE_SIZE: i64 = 50;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct IdentityMergeReport {
    pub(crate) groups_merged: usize,
    pub(crate) items_merged: usize,
    pub(crate) groups_skipped: usize,
}

impl IdentityMergeReport {
    pub(crate) fn changed(&self) -> bool {
        self.groups_merged > 0
    }
}

#[derive(Clone)]
pub struct ItemMergeService {
    database: Database,
}

impl ItemMergeService {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub(crate) async fn merge(
        &self,
        primary_item_id: &str,
        item_ids: &[String],
    ) -> Result<StoredMediaMerge, StorageError> {
        self.database
            .merge_media_items(primary_item_id, item_ids)
            .await
    }

    pub(crate) async fn unmerge(
        &self,
        primary_item_id: &str,
        merged_item_id: &str,
    ) -> Result<StoredMediaMergeUndo, StorageError> {
        self.database
            .unmerge_media_items(primary_item_id, merged_item_id)
            .await
    }

    pub(crate) async fn list_records(
        &self,
        primary_item_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredMediaMergeRecord>, StorageError> {
        self.database
            .list_media_merge_records_for_item(primary_item_id, offset, limit)
            .await
    }

    /// Merges same-library duplicates whose identity is confirmed, optionally limited to one
    /// library. Provider-ID groups run first; the title+year fallback only covers items without
    /// any provider ID. One pass merges at most `budget` groups so a background sweep stays
    /// bounded.
    pub(crate) async fn merge_identity_duplicates(
        &self,
        library_id: Option<&str>,
        budget: usize,
    ) -> Result<IdentityMergeReport, StorageError> {
        let mut report = IdentityMergeReport::default();
        let budget = budget.max(1);

        let mut cursor: Option<(String, String, String, String)> = None;
        loop {
            if report.groups_merged >= budget {
                return Ok(report);
            }
            let page = self
                .database
                .list_provider_identity_merge_keys(
                    cursor
                        .as_ref()
                        .map(|(library, item_type, provider, provider_id)| {
                            (
                                library.as_str(),
                                item_type.as_str(),
                                provider.as_str(),
                                provider_id.as_str(),
                            )
                        }),
                    IDENTITY_MERGE_KEY_PAGE_SIZE,
                )
                .await?;
            let Some(last) = page.last() else {
                break;
            };
            cursor = Some((
                last.library_id.clone(),
                last.item_type.clone(),
                last.provider.clone().unwrap_or_default(),
                last.provider_id.clone().unwrap_or_default(),
            ));
            for key in &page {
                if library_id.is_some_and(|library| key.library_id != library) {
                    continue;
                }
                if report.groups_merged >= budget {
                    return Ok(report);
                }
                self.merge_identity_group(key, &mut report).await?;
            }
        }

        let mut cursor: Option<(String, String, String, i64)> = None;
        loop {
            if report.groups_merged >= budget {
                return Ok(report);
            }
            let page = self
                .database
                .list_title_identity_merge_keys(
                    cursor
                        .as_ref()
                        .map(|(library, item_type, sort_title, year)| {
                            (
                                library.as_str(),
                                item_type.as_str(),
                                sort_title.as_str(),
                                *year,
                            )
                        }),
                    IDENTITY_MERGE_KEY_PAGE_SIZE,
                )
                .await?;
            let Some(last) = page.last() else {
                break;
            };
            cursor = Some((
                last.library_id.clone(),
                last.item_type.clone(),
                last.sort_title.clone().unwrap_or_default(),
                last.production_year.unwrap_or_default(),
            ));
            for key in &page {
                if library_id.is_some_and(|library| key.library_id != library) {
                    continue;
                }
                if report.groups_merged >= budget {
                    return Ok(report);
                }
                self.merge_identity_group(key, &mut report).await?;
            }
        }
        Ok(report)
    }

    async fn merge_identity_group(
        &self,
        key: &StoredIdentityMergeCandidateKey,
        report: &mut IdentityMergeReport,
    ) -> Result<(), StorageError> {
        let items = if key.provider.is_some() {
            self.database
                .list_provider_identity_group_items(key)
                .await?
        } else {
            self.database.list_title_identity_group_items(key).await?
        };
        if items.len() < 2 {
            return Ok(());
        }
        // Explicit editions (Director's Cut, Extended, ...) stay independent logic items.
        if items.iter().any(|item| item.edition_source_count > 0) {
            report.groups_skipped += 1;
            return Ok(());
        }
        let ordered = order_identity_merge_items(items);
        let primary_item_id = ordered
            .first()
            .map(|item| item.id.clone())
            .ok_or_else(|| StorageError::Conflict("合并候选为空".to_owned()))?;
        let item_ids = ordered.into_iter().map(|item| item.id).collect::<Vec<_>>();
        match self
            .database
            .merge_media_items_with_origin(&primary_item_id, &item_ids, MediaMergeOrigin::Automatic)
            .await
        {
            Ok(result) => {
                report.groups_merged += 1;
                report.items_merged += result.merged_item_ids.len();
                Ok(())
            }
            // A concurrent merge or removal already resolved this group; the next pass sees the
            // remaining duplicates.
            Err(StorageError::Conflict(_)) => {
                report.groups_skipped += 1;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

/// Deterministic primary selection: most media sources, then an existing default source, then the
/// oldest item, then ID order.
fn order_identity_merge_items(
    mut items: Vec<StoredIdentityMergeCandidateItem>,
) -> Vec<StoredIdentityMergeCandidateItem> {
    items.sort_by(|left, right| {
        right
            .source_count
            .cmp(&left.source_count)
            .then_with(|| right.default_source_count.cmp(&left.default_source_count))
            .then_with(|| left.added_at.cmp(&right.added_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        id: &str,
        source_count: i64,
        default_source_count: i64,
        created_at: i64,
    ) -> StoredIdentityMergeCandidateItem {
        StoredIdentityMergeCandidateItem {
            id: id.to_owned(),
            added_at: created_at,
            source_count,
            default_source_count,
            edition_source_count: 0,
        }
    }

    #[test]
    fn primary_prefers_most_sources_then_default_then_oldest_then_id() {
        let ordered = order_identity_merge_items(vec![
            candidate("b", 1, 1, 10),
            candidate("a", 2, 0, 20),
            candidate("c", 2, 1, 30),
            candidate("d", 1, 1, 5),
        ]);
        let ids = ordered.into_iter().map(|item| item.id).collect::<Vec<_>>();
        assert_eq!(ids, vec!["c", "a", "d", "b"]);
    }
}
