use std::fmt;

use crate::{
    application::access::{AccessError, AccessPrincipal, MediaAccessService},
    storage::{Database, StorageError, StoredCatalogRow},
};

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

    pub async fn find_item(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
    ) -> Result<Option<CatalogItem>, CatalogError> {
        if !self.access.can_view_item(principal, item_id).await? {
            return Ok(None);
        }
        let rows = self.database.find_catalog_rows(item_id).await?;
        Ok(assemble_items(rows).into_iter().next())
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
    Some(tokens.join(" OR "))
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogPage {
    pub items: Vec<CatalogItem>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
    pub production_year: Option<i64>,
    pub runtime_ticks: Option<i64>,
    pub poster_image_tag: Option<String>,
    pub fanart_image_tag: Option<String>,
    pub media_sources: Vec<CatalogSource>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogSource {
    pub id: String,
    pub source_kind: String,
    pub container: Option<String>,
    pub size: Option<i64>,
    pub external_url: Option<String>,
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
                    production_year: row.production_year,
                    runtime_ticks: row.runtime_ticks,
                    poster_image_tag: row.poster_image_tag.clone(),
                    fanart_image_tag: row.fanart_image_tag.clone(),
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
        });
        let _ = stream_id;
    }
    items
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
