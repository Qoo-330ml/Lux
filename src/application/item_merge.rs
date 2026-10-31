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
        // Provider IDs come from sidecar NFOs that may point at another film. The folder year is
        // an independent veto: 银翼杀手 (1982) and 银翼杀手2049 (2017) can share a polluted TMDb
        // id but never the same release year.
        let candidate_item_ids = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
        let source_paths = self
            .database
            .list_identity_group_source_paths(&candidate_item_ids)
            .await?;
        if source_years_conflict(&source_paths) {
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

/// True when candidate items live in source folders that name different release years.
fn source_years_conflict(paths: &[(String, String)]) -> bool {
    let mut year: Option<u32> = None;
    for (_, path) in paths {
        let Some(found) = source_year(path) else {
            continue;
        };
        match year {
            None => year = Some(found),
            Some(existing) if existing != found => return true,
            Some(_) => {}
        }
    }
    false
}

/// Release year from the first path segment, e.g. `Movie (2017)/file.strm`.
///
/// Only parenthesized 4-digit years count so titles such as `银翼杀手2049` and quality labels
/// such as `2160p` never look like years.
fn source_year(path: &str) -> Option<u32> {
    let segment = path.split(['/', '\\']).next().unwrap_or(path);
    let characters = segment.chars().collect::<Vec<_>>();
    for (index, character) in characters.iter().enumerate() {
        if *character != '(' && *character != '（' {
            continue;
        }
        let mut cursor = index + 1;
        let mut digits = String::new();
        while cursor < characters.len() && characters[cursor].is_ascii_digit() && digits.len() < 4 {
            digits.push(characters[cursor]);
            cursor += 1;
        }
        if digits.len() != 4 || cursor >= characters.len() {
            continue;
        }
        if characters[cursor] != ')' && characters[cursor] != '）' {
            continue;
        }
        if let Ok(year) = digits.parse::<u32>()
            && (1900..=2099).contains(&year)
        {
            return Some(year);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{application::libraries::LibraryService, config::Config, library::LibraryKind};

    async fn test_database() -> Result<(tempfile::TempDir, Database), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        Ok((temp_dir, database))
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_item(
        database: &Database,
        library_id: &str,
        id: &str,
        item_type: &str,
        parent_id: Option<&str>,
        series_id: Option<&str>,
        season_number: Option<i64>,
        episode_number: Option<i64>,
        title: &str,
        production_year: Option<i64>,
        provider_ids_json: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, parent_id, series_id, season_number, episode_number,
                 title, sort_title, production_year, provider_ids_json, identification_status
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(id)
        .bind(library_id)
        .bind(item_type)
        .bind(parent_id)
        .bind(series_id)
        .bind(season_number)
        .bind(episode_number)
        .bind(title)
        .bind(title.to_lowercase())
        .bind(production_year)
        .bind(provider_ids_json)
        .execute(database.pool())
        .await
        .map(|_| ())
    }

    async fn insert_source(
        database: &Database,
        id: &str,
        item_id: &str,
        edition_name: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO media_sources (
                 id, item_id, source_kind, edition_name, is_default, probe_status
             ) VALUES (?, ?, 'LOCAL_FILE', ?, 1, 'PENDING')",
        )
        .bind(id)
        .bind(item_id)
        .bind(edition_name)
        .execute(database.pool())
        .await
        .map(|_| ())
    }

    async fn insert_library_root(
        database: &Database,
        id: &str,
        library_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES (?, ?, ?, ?, 1, 0)",
        )
        .bind(id)
        .bind(library_id)
        .bind(format!("/media/{library_id}"))
        .bind(format!("/media/{library_id}"))
        .execute(database.pool())
        .await
        .map(|_| ())
    }

    async fn attach_source_path(
        database: &Database,
        source_id: &str,
        root_id: &str,
        relative_path: &str,
    ) -> Result<(), sqlx::Error> {
        let entry_id = format!("{source_id}-entry");
        sqlx::query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 last_seen_generation
             ) VALUES (?, ?, ?, 'FILE', 1, 0, 'test')",
        )
        .bind(&entry_id)
        .bind(root_id)
        .bind(relative_path)
        .execute(database.pool())
        .await?;
        sqlx::query("UPDATE media_sources SET filesystem_entry_id = ? WHERE id = ?")
            .bind(entry_id)
            .bind(source_id)
            .execute(database.pool())
            .await
            .map(|_| ())
    }

    async fn merged_into(database: &Database, id: &str) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar("SELECT merged_into_item_id FROM media_items WHERE id = ?")
            .bind(id)
            .fetch_one(database.pool())
            .await
    }

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

    #[tokio::test]
    async fn provider_identity_merges_series_and_reports_zero_on_the_next_pass()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Shows", LibraryKind::Series, false)
            .await?;
        let library_id = library.id.to_string();
        let provider = Some(r#"{"tmdb":"77"}"#);
        for (series_id, title) in [("series-a", "Show A"), ("series-b", "Show B")] {
            insert_item(
                &database,
                &library_id,
                series_id,
                "SERIES",
                None,
                None,
                None,
                None,
                title,
                None,
                provider,
            )
            .await?;
            let season_id = format!("{series_id}-season-1");
            insert_item(
                &database,
                &library_id,
                &season_id,
                "SEASON",
                Some(series_id),
                Some(series_id),
                Some(1),
                None,
                "Season 01",
                None,
                None,
            )
            .await?;
            let episode_id = format!("{series_id}-s01e01");
            insert_item(
                &database,
                &library_id,
                &episode_id,
                "EPISODE",
                Some(&season_id),
                Some(series_id),
                Some(1),
                Some(1),
                "Pilot",
                None,
                None,
            )
            .await?;
            insert_source(
                &database,
                &format!("{episode_id}-source"),
                &episode_id,
                None,
            )
            .await?;
        }

        let service = ItemMergeService::new(database.clone());
        let report = service.merge_identity_duplicates(None, 10).await?;
        assert_eq!(report.groups_merged, 1);
        assert_eq!(report.items_merged, 1);
        assert_eq!(
            merged_into(&database, "series-b").await?.as_deref(),
            Some("series-a")
        );
        let episode_sources: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM media_sources WHERE item_id = 'series-a-s01e01'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(episode_sources, 2);
        let auto_records: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM media_merge_records WHERE source = 'AUTO'")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(auto_records, 1);

        let second = service.merge_identity_duplicates(None, 10).await?;
        assert_eq!(second.groups_merged, 0);
        Ok(())
    }

    #[tokio::test]
    async fn title_and_year_fallback_merges_only_items_without_provider_ids()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        insert_item(
            &database,
            &library_id,
            "movie-a",
            "MOVIE",
            None,
            None,
            None,
            None,
            "Same Movie",
            Some(2001),
            None,
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "movie-b",
            "MOVIE",
            None,
            None,
            None,
            None,
            "Same Movie",
            Some(2001),
            None,
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "movie-other-year",
            "MOVIE",
            None,
            None,
            None,
            None,
            "Same Movie",
            Some(2002),
            None,
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "movie-identified",
            "MOVIE",
            None,
            None,
            None,
            None,
            "Same Movie",
            Some(2001),
            Some(r#"{"tmdb":"9"}"#),
        )
        .await?;
        insert_source(&database, "source-a", "movie-a", None).await?;
        insert_source(&database, "source-b", "movie-b", None).await?;
        insert_source(&database, "source-other", "movie-other-year", None).await?;
        insert_source(&database, "source-identified", "movie-identified", None).await?;

        let service = ItemMergeService::new(database.clone());
        let report = service.merge_identity_duplicates(None, 10).await?;
        assert_eq!(report.groups_merged, 1);
        assert_eq!(
            merged_into(&database, "movie-b").await?.as_deref(),
            Some("movie-a")
        );
        assert_eq!(merged_into(&database, "movie-other-year").await?, None);
        assert_eq!(merged_into(&database, "movie-identified").await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn explicit_editions_are_never_auto_merged() -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        let provider = Some(r#"{"tmdb":"12"}"#);
        insert_item(
            &database,
            &library_id,
            "theatrical",
            "MOVIE",
            None,
            None,
            None,
            None,
            "Cut Movie",
            Some(1999),
            provider,
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "extended",
            "MOVIE",
            None,
            None,
            None,
            None,
            "Cut Movie",
            Some(1999),
            provider,
        )
        .await?;
        insert_source(&database, "source-theatrical", "theatrical", None).await?;
        insert_source(
            &database,
            "source-extended",
            "extended",
            Some("Extended Cut"),
        )
        .await?;

        let service = ItemMergeService::new(database.clone());
        let report = service.merge_identity_duplicates(None, 10).await?;
        assert_eq!(report.groups_merged, 0);
        assert!(report.groups_skipped >= 1);
        assert_eq!(merged_into(&database, "extended").await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn provider_identity_requires_a_shared_tmdb_key() -> Result<(), Box<dyn std::error::Error>>
    {
        let (_temp_dir, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        // The 2018 film carries a stale 1987 IMDb id in its sidecar NFO. A shared IMDb id must
        // not merge items whose TMDb identities differ.
        insert_item(
            &database,
            &library_id,
            "predator-1987",
            "MOVIE",
            None,
            None,
            None,
            None,
            "Predator",
            Some(1987),
            Some(r#"{"imdb":"tt0093773","tmdb":"48193"}"#),
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "predator-2018",
            "MOVIE",
            None,
            None,
            None,
            None,
            "Predator",
            Some(2018),
            Some(r#"{"imdb":"tt0093773","tmdb":"346910"}"#),
        )
        .await?;
        insert_source(&database, "source-1987", "predator-1987", None).await?;
        insert_source(&database, "source-2018", "predator-2018", None).await?;

        let service = ItemMergeService::new(database.clone());
        let report = service.merge_identity_duplicates(None, 10).await?;
        assert_eq!(report.groups_merged, 0);
        assert_eq!(merged_into(&database, "predator-2018").await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn source_path_year_conflict_vetoes_merge() -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        insert_library_root(&database, "root-movies", &library_id).await?;
        // Both items were identified as TMDb 78 by a polluted movie.nfo, but their folders
        // belong to different films.
        let provider = Some(r#"{"tmdb":"78"}"#);
        insert_item(
            &database,
            &library_id,
            "blade-runner",
            "MOVIE",
            None,
            None,
            None,
            None,
            "Blade Runner",
            Some(1982),
            provider,
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "blade-runner-2049",
            "MOVIE",
            None,
            None,
            None,
            None,
            "Blade Runner",
            Some(1982),
            provider,
        )
        .await?;
        insert_source(&database, "source-1982", "blade-runner", None).await?;
        insert_source(&database, "source-2049", "blade-runner-2049", None).await?;
        attach_source_path(
            &database,
            "source-1982",
            "root-movies",
            "银翼杀手 (1982)/银翼杀手 (1982) - 2160p.strm",
        )
        .await?;
        attach_source_path(
            &database,
            "source-2049",
            "root-movies",
            "银翼杀手2049 (2017)/银翼杀手2049 (2017) - 2160p.strm",
        )
        .await?;

        let service = ItemMergeService::new(database.clone());
        let report = service.merge_identity_duplicates(None, 10).await?;
        assert_eq!(report.groups_merged, 0);
        assert_eq!(merged_into(&database, "blade-runner-2049").await?, None);
        Ok(())
    }

    #[test]
    fn parenthesized_year_ignores_unparenthesized_numbers() {
        assert_eq!(
            source_year("银翼杀手2049 (2017)/银翼杀手2049 (2017) - 2160p.strm"),
            Some(2017)
        );
        assert_eq!(
            source_year("铁血战士（1987）/铁血战士 (1987) - 2160p.strm"),
            Some(1987)
        );
        assert_eq!(source_year("Movie 2160p (2010)/file.strm"), Some(2010));
        assert_eq!(
            source_year("Show (2017)/Season 1/S01E01 (2019).mkv"),
            Some(2017)
        );
        assert_eq!(source_year("Movie (1888)/file.strm"), None);
        assert_eq!(source_year("Movie (2099)/file.strm"), Some(2099));
        assert_eq!(source_year("Movie/file.strm"), None);
        assert_eq!(source_year("Movie (1800)/file.strm"), None);
        assert_eq!(source_year("Movie (2100)/file.strm"), None);
    }

    #[tokio::test]
    async fn manual_unmerge_suppresses_provider_group_from_auto_merge()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        let provider = Some(r#"{"tmdb":"77"}"#);
        for (id, title) in [("movie-a", "Same Movie"), ("movie-b", "Same Movie")] {
            insert_item(
                &database,
                &library_id,
                id,
                "MOVIE",
                None,
                None,
                None,
                None,
                title,
                Some(2001),
                provider,
            )
            .await?;
            insert_source(&database, &format!("source-{id}"), id, None).await?;
        }

        let service = ItemMergeService::new(database.clone());
        let report = service.merge_identity_duplicates(None, 10).await?;
        assert_eq!(report.groups_merged, 1);
        assert_eq!(
            merged_into(&database, "movie-b").await?.as_deref(),
            Some("movie-a")
        );

        service.unmerge("movie-a", "movie-b").await?;
        assert_eq!(merged_into(&database, "movie-b").await?, None);

        let second = service.merge_identity_duplicates(None, 10).await?;
        assert_eq!(second.groups_merged, 0);
        assert_eq!(merged_into(&database, "movie-b").await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn manual_unmerge_suppresses_title_fallback_group()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        for (id, title) in [("movie-a", "Same Movie"), ("movie-b", "Same Movie")] {
            insert_item(
                &database,
                &library_id,
                id,
                "MOVIE",
                None,
                None,
                None,
                None,
                title,
                Some(2001),
                None,
            )
            .await?;
            insert_source(&database, &format!("source-{id}"), id, None).await?;
        }

        let service = ItemMergeService::new(database.clone());
        let report = service.merge_identity_duplicates(None, 10).await?;
        assert_eq!(report.groups_merged, 1);

        service.unmerge("movie-a", "movie-b").await?;
        let second = service.merge_identity_duplicates(None, 10).await?;
        assert_eq!(second.groups_merged, 0);
        assert_eq!(merged_into(&database, "movie-b").await?, None);
        Ok(())
    }
}
