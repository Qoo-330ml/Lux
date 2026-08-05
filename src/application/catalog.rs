use std::{collections::BTreeMap, fmt};

use crate::{
    application::access::{AccessError, AccessPrincipal, MediaAccessService},
    application::recommendations::{
        RECOMMENDATION_CANDIDATE_POOL, current_day_bucket, daily_recommendation_items,
    },
    storage::{
        CatalogFilterQuery, CatalogSort as StorageCatalogSort, Database, StorageError,
        StoredCatalogRow,
    },
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CatalogFilter {
    pub item_types: Vec<String>,
    pub years: Vec<i64>,
    pub is_played: Option<bool>,
    pub is_favorite: Option<bool>,
    pub sort_by: CatalogSort,
    pub descending: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CatalogSort {
    #[default]
    Name,
    DateCreated,
    PremiereDate,
    Rating,
}

#[derive(Clone)]
pub struct CatalogService {
    database: Database,
    access: MediaAccessService,
}

impl CatalogService {
    pub fn new(database: Database, access: MediaAccessService) -> Self {
        Self { database, access }
    }

    pub async fn list_library_items(
        &self,
        principal: AccessPrincipal,
        library_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let Some(library) = self.database.find_library(library_id).await? else {
            return Err(CatalogError::LibraryNotFound);
        };
        if !library.is_enabled {
            return Err(CatalogError::LibraryNotFound);
        }
        if !self.access.can_view_library(principal, library_id).await? {
            return Err(CatalogError::AccessDenied);
        }
        let total = self.database.count_catalog_items(Some(library_id)).await?;
        let rows = self
            .database
            .list_catalog_rows(Some(library_id), offset, limit)
            .await?;
        Ok(CatalogPage {
            items: assemble_items(rows),
            total,
            offset,
            limit,
        })
    }

    pub async fn list_library_items_filtered(
        &self,
        principal: AccessPrincipal,
        library_id: &str,
        filter: &CatalogFilter,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let Some(library) = self.database.find_library(library_id).await? else {
            return Err(CatalogError::LibraryNotFound);
        };
        if !library.is_enabled {
            return Err(CatalogError::LibraryNotFound);
        }
        if !self.access.can_view_library(principal, library_id).await? {
            return Err(CatalogError::AccessDenied);
        }
        let library_ids = vec![library_id.to_owned()];
        let user_id = principal.user_id.to_string();
        let query = CatalogFilterQuery {
            library_ids: &library_ids,
            user_id: &user_id,
            item_types: &filter.item_types,
            years: &filter.years,
            is_played: filter.is_played,
            is_favorite: filter.is_favorite,
            sort_by: match filter.sort_by {
                CatalogSort::Name => StorageCatalogSort::Name,
                CatalogSort::DateCreated => StorageCatalogSort::DateCreated,
                CatalogSort::PremiereDate => StorageCatalogSort::PremiereDate,
                CatalogSort::Rating => StorageCatalogSort::Rating,
            },
            descending: filter.descending,
            offset,
            limit,
        };
        let (rows, total) = self.database.list_filtered_catalog_rows(&query).await?;
        Ok(CatalogPage {
            items: assemble_items(rows),
            total,
            offset,
            limit,
        })
    }

    pub async fn list_all_items_filtered(
        &self,
        principal: AccessPrincipal,
        filter: &CatalogFilter,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let user_id = principal.user_id.to_string();
        let query = CatalogFilterQuery {
            library_ids: &library_ids,
            user_id: &user_id,
            item_types: &filter.item_types,
            years: &filter.years,
            is_played: filter.is_played,
            is_favorite: filter.is_favorite,
            sort_by: match filter.sort_by {
                CatalogSort::Name => StorageCatalogSort::Name,
                CatalogSort::DateCreated => StorageCatalogSort::DateCreated,
                CatalogSort::PremiereDate => StorageCatalogSort::PremiereDate,
                CatalogSort::Rating => StorageCatalogSort::Rating,
            },
            descending: filter.descending,
            offset,
            limit,
        };
        let (rows, total) = self.database.list_filtered_catalog_rows(&query).await?;
        Ok(CatalogPage {
            items: assemble_items(rows),
            total,
            offset,
            limit,
        })
    }

    pub async fn list_children(
        &self,
        principal: AccessPrincipal,
        parent_id: &str,
        item_type: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        if !self.access.can_view_item(principal, parent_id).await? {
            return Err(CatalogError::AccessDenied);
        }
        let total = self
            .database
            .count_catalog_children(parent_id, item_type)
            .await?;
        let rows = self
            .database
            .list_catalog_children(parent_id, item_type, offset, limit)
            .await?;
        Ok(CatalogPage {
            items: assemble_items(rows),
            total,
            offset,
            limit,
        })
    }

    pub async fn list_series_episodes(
        &self,
        principal: AccessPrincipal,
        series_id: &str,
        season_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        if !self.access.can_view_item(principal, series_id).await? {
            return Err(CatalogError::AccessDenied);
        }
        let season_ids = if let Some(season_id) = season_id {
            let season = self
                .database
                .find_catalog_rows(season_id)
                .await
                .map(assemble_items)?
                .into_iter()
                .next()
                .filter(|item| {
                    item.item_type == "SEASON" && item.parent_id.as_deref() == Some(series_id)
                });
            let Some(_) = season else {
                return Err(CatalogError::LibraryNotFound);
            };
            vec![season_id.to_owned()]
        } else {
            let rows = self
                .database
                .list_catalog_children(series_id, "SEASON", 0, i64::MAX)
                .await?;
            rows.into_iter().map(|row| row.item_id).collect::<Vec<_>>()
        };
        let mut items = Vec::new();
        for season_id in season_ids {
            let rows = self
                .database
                .list_catalog_children(&season_id, "EPISODE", 0, i64::MAX)
                .await?;
            items.extend(assemble_items(rows));
        }
        items.sort_by(|left, right| {
            left.season_number
                .cmp(&right.season_number)
                .then_with(|| left.episode_number.cmp(&right.episode_number))
                .then_with(|| left.sort_title.cmp(&right.sort_title))
                .then_with(|| left.id.cmp(&right.id))
        });
        let total = i64::try_from(items.len()).unwrap_or(i64::MAX);
        let items = items
            .into_iter()
            .skip(usize::try_from(offset).unwrap_or(usize::MAX))
            .take(usize::try_from(limit).unwrap_or(0))
            .collect();
        Ok(CatalogPage {
            items,
            total,
            offset,
            limit,
        })
    }

    pub async fn list_collection_items(
        &self,
        principal: AccessPrincipal,
        collection_item_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        if !self
            .access
            .can_view_item(principal, collection_item_id)
            .await?
        {
            return Err(CatalogError::AccessDenied);
        }
        let member_ids = self
            .database
            .list_collection_member_ids(collection_item_id)
            .await?;
        let mut items = Vec::new();
        for member_id in member_ids {
            if let Some(item) = self.find_item(principal, &member_id).await? {
                items.push(item);
            }
        }
        let total = i64::try_from(items.len()).unwrap_or(i64::MAX);
        let items = items
            .into_iter()
            .skip(usize::try_from(offset).unwrap_or(usize::MAX))
            .take(usize::try_from(limit).unwrap_or(0))
            .collect();
        Ok(CatalogPage {
            items,
            total,
            offset,
            limit,
        })
    }

    pub async fn list_next_up(
        &self,
        principal: AccessPrincipal,
        user_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let total = self
            .database
            .count_next_up_items(user_id, &library_ids)
            .await?;
        let rows = self
            .database
            .list_next_up_items(user_id, &library_ids, offset, limit)
            .await?;
        Ok(CatalogPage {
            items: assemble_items(rows),
            total,
            offset,
            limit,
        })
    }

    pub async fn list_recently_added(
        &self,
        principal: AccessPrincipal,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let (item_ids, total) = self
            .database
            .list_recent_catalog_item_ids(&library_ids, offset, limit)
            .await?;
        let mut items = Vec::with_capacity(item_ids.len());
        for item_id in item_ids {
            if let Some(item) = self.find_item(principal, &item_id).await? {
                items.push(item);
            }
        }
        Ok(CatalogPage {
            items,
            total,
            offset,
            limit,
        })
    }

    pub async fn list_recently_added_by_library(
        &self,
        principal: AccessPrincipal,
        limit: i64,
    ) -> Result<Vec<(String, Vec<CatalogItem>)>, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let rows = self
            .database
            .list_recent_catalog_rows_by_library(&library_ids, limit)
            .await?;
        let mut grouped = BTreeMap::<String, Vec<CatalogItem>>::new();
        for item in assemble_items(rows) {
            grouped
                .entry(item.library_id.clone())
                .or_default()
                .push(item);
        }
        Ok(grouped.into_iter().collect())
    }

    pub async fn list_recommended(
        &self,
        principal: AccessPrincipal,
        user_id: &str,
        limit: i64,
    ) -> Result<Vec<CatalogItem>, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let rows = self
            .database
            .list_recommended_catalog_rows(user_id, &library_ids, 0, RECOMMENDATION_CANDIDATE_POOL)
            .await?;
        let items = assemble_items(rows);
        Ok(daily_recommendation_items(
            items,
            user_id,
            current_day_bucket(),
            usize::try_from(limit).unwrap_or(0),
        ))
    }

    pub async fn find_item(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
    ) -> Result<Option<CatalogItem>, CatalogError> {
        if !self.access.can_view_item(principal, item_id).await? {
            return Ok(None);
        }
        let rows = self.database.find_catalog_rows(item_id).await?;
        let Some(mut item) = assemble_items(rows).into_iter().next() else {
            return Ok(None);
        };
        if let Some(detail) = self.database.find_catalog_detail(item_id).await? {
            item.premiere_date = detail.premiere_date;
            item.last_air_date = detail.last_air_date;
            item.status = detail.status;
            item.original_language = detail.original_language;
            item.provider_ids = provider_ids_from_json(detail.provider_ids_json.as_deref());
            if item.item_type == "SERIES" {
                item.season_count = Some(detail.season_count);
                item.episode_count = Some(detail.episode_count);
            }
        }
        Ok(Some(item))
    }

    pub async fn list_all_items(
        &self,
        principal: AccessPrincipal,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        if principal.is_admin {
            let total = self.database.count_catalog_items(None).await?;
            let rows = self.database.list_catalog_rows(None, offset, limit).await?;
            return Ok(CatalogPage {
                items: assemble_items(rows),
                total,
                offset,
                limit,
            });
        }
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let mut items = Vec::new();
        for library_id in library_ids {
            let rows = self
                .database
                .list_catalog_rows(Some(&library_id), 0, i64::MAX)
                .await?;
            items.extend(assemble_items(rows));
        }
        items.sort_by(|left, right| {
            left.sort_title
                .cmp(&right.sort_title)
                .then_with(|| left.id.cmp(&right.id))
        });
        let total = i64::try_from(items.len()).unwrap_or(i64::MAX);
        let items = items
            .into_iter()
            .skip(usize::try_from(offset).unwrap_or(usize::MAX))
            .take(usize::try_from(limit).unwrap_or(0))
            .collect();
        Ok(CatalogPage {
            items,
            total,
            offset,
            limit,
        })
    }

    pub async fn search_items(
        &self,
        principal: AccessPrincipal,
        query: &str,
        like_query: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = if principal.is_admin {
            None
        } else {
            Some(self.access.accessible_library_ids(principal).await?)
        };
        let library_filter = library_ids.as_deref();
        let (ids, total) = self
            .database
            .search_catalog_item_ids(query, like_query, library_filter, offset, limit)
            .await?;
        let mut items = Vec::with_capacity(ids.len());
        for item_id in ids {
            if let Some(item) = self.find_item(principal, &item_id).await? {
                items.push(item);
            }
        }
        Ok(CatalogPage {
            items,
            total,
            offset,
            limit,
        })
    }
}

pub fn normalize_search_query(value: &str) -> Option<String> {
    let tokens = value
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return None;
    }
    // A multi-word search should narrow results to items containing every
    // token. The LIKE fallback already treats the full input as a phrase;
    // keeping FTS on the same AND semantics prevents broad matches such as
    // "Reference Movie 40" from returning every title containing "Movie".
    Some(tokens.join(" AND "))
}

pub fn normalize_search_like_query(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let escaped = value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    Some(format!("%{escaped}%"))
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogPage {
    pub items: Vec<CatalogItem>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogItem {
    pub id: String,
    pub library_id: String,
    pub item_type: String,
    pub parent_id: Option<String>,
    pub series_id: Option<String>,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
    pub title: String,
    pub sort_title: String,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub premiere_date: Option<String>,
    pub last_air_date: Option<String>,
    pub status: Option<String>,
    pub original_language: Option<String>,
    pub provider_ids: BTreeMap<String, String>,
    pub season_count: Option<i64>,
    pub episode_count: Option<i64>,
    pub production_year: Option<i64>,
    pub rating: Option<f64>,
    pub rating_source: Option<String>,
    pub runtime_ticks: Option<i64>,
    pub poster_image_tag: Option<String>,
    pub fanart_image_tag: Option<String>,
    pub logo_image_tag: Option<String>,
    pub media_sources: Vec<CatalogSource>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogSource {
    pub id: String,
    pub source_kind: String,
    pub container: Option<String>,
    pub size: Option<i64>,
    pub external_url: Option<String>,
    pub edition_name: Option<String>,
    pub quality_label: Option<String>,
    pub bitrate: Option<i64>,
    pub duration_ticks: Option<i64>,
    pub is_default: bool,
    pub probe_status: String,
    pub streams: Vec<CatalogStream>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogStream {
    pub index: i64,
    pub stream_type: String,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_external: bool,
    pub is_default: bool,
    pub is_forced: bool,
    pub details: std::collections::BTreeMap<String, serde_json::Value>,
}

fn assemble_items(rows: Vec<StoredCatalogRow>) -> Vec<CatalogItem> {
    let mut items = Vec::new();
    for row in rows {
        let item_index = match items
            .iter()
            .position(|item: &CatalogItem| item.id == row.item_id)
        {
            Some(index) => index,
            None => {
                items.push(CatalogItem {
                    id: row.item_id.clone(),
                    library_id: row.library_id.clone(),
                    item_type: row.item_type.clone(),
                    parent_id: row.parent_id.clone(),
                    series_id: row.series_id.clone(),
                    season_number: row.season_number,
                    episode_number: row.episode_number,
                    title: row.title.clone(),
                    sort_title: row.sort_title.clone(),
                    original_title: row.original_title.clone(),
                    overview: row.overview.clone(),
                    premiere_date: None,
                    last_air_date: None,
                    status: None,
                    original_language: None,
                    provider_ids: BTreeMap::new(),
                    season_count: None,
                    episode_count: None,
                    production_year: row.production_year,
                    rating: row.rating,
                    rating_source: row.rating_source.clone(),
                    runtime_ticks: row.runtime_ticks,
                    poster_image_tag: row.poster_image_tag.clone(),
                    fanart_image_tag: row.fanart_image_tag.clone(),
                    logo_image_tag: row.logo_image_tag.clone(),
                    media_sources: Vec::new(),
                });
                items.len() - 1
            }
        };
        let Some(source_id) = row.source_id else {
            continue;
        };
        let item = &mut items[item_index];
        let source_index = match item
            .media_sources
            .iter()
            .position(|source| source.id == source_id)
        {
            Some(index) => index,
            None => {
                item.media_sources.push(CatalogSource {
                    id: source_id,
                    source_kind: row.source_kind.unwrap_or_else(|| "LOCAL_FILE".to_owned()),
                    container: row.container.clone(),
                    size: row.size,
                    external_url: row.external_url.clone(),
                    edition_name: row.edition_name.clone(),
                    quality_label: row.quality_label.clone(),
                    bitrate: row.bitrate,
                    duration_ticks: row.duration_ticks,
                    is_default: row.is_default.unwrap_or(false),
                    probe_status: row.probe_status.unwrap_or_else(|| "PENDING".to_owned()),
                    streams: Vec::new(),
                });
                item.media_sources.len() - 1
            }
        };
        let Some(stream_id) = row.stream_id else {
            continue;
        };
        let source = &mut item.media_sources[source_index];
        if source
            .streams
            .iter()
            .any(|stream| stream.index == row.stream_index.unwrap_or(-1))
        {
            continue;
        }
        source.streams.push(CatalogStream {
            index: row.stream_index.unwrap_or(source.streams.len() as i64),
            stream_type: row.stream_type.unwrap_or_else(|| "UNKNOWN".to_owned()),
            codec: row.codec,
            language: row.language,
            title: row.stream_title,
            is_external: row.stream_is_external.unwrap_or(false),
            is_default: row.stream_is_default.unwrap_or(false),
            is_forced: row.stream_is_forced.unwrap_or(false),
            details: row
                .stream_details_json
                .as_deref()
                .and_then(|value| serde_json::from_str(value).ok())
                .unwrap_or_default(),
        });
        let _ = stream_id;
    }
    items
}

fn provider_ids_from_json(raw: Option<&str>) -> BTreeMap<String, String> {
    raw.and_then(|value| {
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(value).ok()
    })
    .map(|object| {
        object
            .into_iter()
            .filter_map(|(name, value)| {
                let id = value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| value.as_i64().map(|value| value.to_string()))?;
                (!name.trim().is_empty() && !id.trim().is_empty()).then_some((name, id))
            })
            .collect()
    })
    .unwrap_or_default()
}

#[derive(Debug)]
pub enum CatalogError {
    LibraryNotFound,
    AccessDenied,
    Storage(StorageError),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LibraryNotFound => formatter.write_str("library not found"),
            Self::AccessDenied => formatter.write_str("library access denied"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LibraryNotFound | Self::AccessDenied => None,
            Self::Storage(error) => Some(error),
        }
    }
}

impl From<StorageError> for CatalogError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<AccessError> for CatalogError {
    fn from(error: AccessError) -> Self {
        match error {
            AccessError::Storage(error) => Self::Storage(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::application::recommendations::daily_recommendation_items;

    #[test]
    fn daily_recommendations_are_stable_for_one_day_and_change_next_day() {
        let items = || (1..=6).collect::<Vec<_>>();

        let day_one = daily_recommendation_items(items(), "user-1", 20, 3);
        let same_day = daily_recommendation_items(items(), "user-1", 20, 3);
        let next_day = daily_recommendation_items(items(), "user-1", 21, 3);

        assert_eq!(day_one, same_day);
        assert_ne!(day_one, next_day);
    }
}
