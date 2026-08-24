use std::fmt;

use crate::{
    application::{
        metadata_objects::{MetadataObjectError, MetadataObjectSnapshot, MetadataObjectStore},
        scraper::{ScraperError, ScraperGetRequest, ScraperItemType, ScraperResolver},
        tmdb_plugin::ScraperProvider,
    },
    storage::{Database, NewCollection, StorageError},
};

#[derive(Clone)]
pub struct CollectionService {
    database: Database,
    tmdb: ScraperProvider,
    resolver: Option<ScraperResolver>,
    metadata_objects: Option<MetadataObjectStore>,
}

impl CollectionService {
    pub fn new<T>(database: Database, tmdb: T) -> Self
    where
        T: Into<ScraperProvider>,
    {
        Self {
            database,
            tmdb: tmdb.into(),
            resolver: None,
            metadata_objects: None,
        }
    }

    pub fn with_resolver<T>(database: Database, tmdb: T, resolver: ScraperResolver) -> Self
    where
        T: Into<ScraperProvider>,
    {
        Self {
            database,
            tmdb: tmdb.into(),
            resolver: Some(resolver),
            metadata_objects: None,
        }
    }

    pub fn with_config_dir(mut self, config_dir: std::path::PathBuf) -> Self {
        self.metadata_objects = Some(MetadataObjectStore::new(config_dir));
        self
    }

    pub async fn refresh_for_item(
        &self,
        item_id: &str,
    ) -> Result<CollectionRefreshReport, CollectionError> {
        let Some(identity) = self.database.find_movie_identity(item_id).await? else {
            return Err(CollectionError::MovieProviderIdMissing);
        };
        let tmdb = self.provider_for_item(item_id).await?;
        let movie = tmdb
            .get_generic(ScraperGetRequest::new(
                ScraperItemType::Movie,
                identity.provider_id.clone(),
                "zh-CN",
            ))
            .await
            .map_err(CollectionError::Scraper)?;
        let Some(collection) = movie.collection else {
            return Err(CollectionError::NoCollection);
        };
        let collection_id = collection
            .provider_id
            .filter(|value| !value.trim().is_empty())
            .ok_or(CollectionError::InvalidProviderId)?;
        let details = tmdb
            .get_generic(ScraperGetRequest::new(
                ScraperItemType::BoxSet,
                collection_id.clone(),
                "zh-CN",
            ))
            .await
            .map_err(CollectionError::Scraper)?;
        if details.first_provider_id().is_none() {
            return Err(CollectionError::InvalidProviderId);
        }
        let title = details
            .title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Scraper Collection");
        let provider_name = identity.provider_name.clone();
        let member_provider_ids = details
            .items
            .iter()
            .filter_map(|part| {
                let id = part
                    .provider_ids
                    .get(&provider_name)
                    .or_else(|| {
                        part.provider_ids.iter().find_map(|(name, id)| {
                            name.eq_ignore_ascii_case(&provider_name).then_some(id)
                        })
                    })
                    .or_else(|| part.provider_ids.values().next())?
                    .clone();
                Some((provider_name.clone(), id, 0))
            })
            .collect::<Vec<_>>();
        let result = self
            .database
            .upsert_collection(NewCollection {
                library_id: &identity.library_id,
                provider: &provider_name,
                provider_id: &collection_id,
                title,
                overview: details.overview.as_deref(),
                poster_path: None,
                backdrop_path: None,
                member_provider_ids: &member_provider_ids,
            })
            .await?;
        if let Some(metadata_objects) = &self.metadata_objects {
            let mut snapshot = MetadataObjectSnapshot::new(
                crate::application::metadata_paths::MetadataObjectKind::Collection,
                title,
                &provider_name,
                &collection_id,
            )?
            .with_member_count(result.member_count);
            if let Some(overview) = details.overview.clone() {
                snapshot = snapshot.with_overview(overview);
            }
            metadata_objects.write_snapshot(snapshot).await?;
        }
        Ok(CollectionRefreshReport {
            source_item_id: item_id.to_owned(),
            collection_item_id: result.collection_item_id,
            member_count: result.member_count,
        })
    }

    async fn provider_for_item(&self, item_id: &str) -> Result<ScraperProvider, CollectionError> {
        let Some(resolver) = &self.resolver else {
            return Ok(self.tmdb.clone());
        };
        resolver
            .for_item(item_id)
            .await
            .map_err(CollectionError::Scraper)
            .map(|client| {
                client
                    .map(ScraperProvider::from_scraper)
                    .unwrap_or_else(|| self.tmdb.clone())
            })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectionRefreshReport {
    pub source_item_id: String,
    pub collection_item_id: String,
    pub member_count: usize,
}

#[derive(Debug)]
pub enum CollectionError {
    MovieProviderIdMissing,
    InvalidProviderId,
    NoCollection,
    Scraper(ScraperError),
    Storage(StorageError),
    Metadata(MetadataObjectError),
}

impl fmt::Display for CollectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MovieProviderIdMissing => formatter.write_str("movie has no scraper provider ID"),
            Self::InvalidProviderId => formatter.write_str("scraper provider ID is invalid"),
            Self::NoCollection => formatter.write_str("movie does not belong to a collection"),
            Self::Scraper(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::Metadata(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CollectionError {}

impl From<StorageError> for CollectionError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<MetadataObjectError> for CollectionError {
    fn from(error: MetadataObjectError) -> Self {
        Self::Metadata(error)
    }
}
