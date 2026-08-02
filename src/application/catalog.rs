use std::fmt;

use crate::storage::{Database, StorageError, StoredCatalogRow};

#[derive(Clone)]
pub struct CatalogService {
    database: Database,
}

impl CatalogService {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn list_library_items(
        &self,
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

    pub async fn find_item(&self, item_id: &str) -> Result<Option<CatalogItem>, CatalogError> {
        let rows = self.database.find_catalog_rows(item_id).await?;
        Ok(assemble_items(rows).into_iter().next())
    }

    pub async fn list_all_items(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let total = self.database.count_catalog_items(None).await?;
        let rows = self.database.list_catalog_rows(None, offset, limit).await?;
        Ok(CatalogPage {
            items: assemble_items(rows),
            total,
            offset,
            limit,
        })
    }
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
        });
        let _ = stream_id;
    }
    items
}

#[derive(Debug)]
pub enum CatalogError {
    LibraryNotFound,
    Storage(StorageError),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LibraryNotFound => formatter.write_str("library not found"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LibraryNotFound => None,
            Self::Storage(error) => Some(error),
        }
    }
}

impl From<StorageError> for CatalogError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
