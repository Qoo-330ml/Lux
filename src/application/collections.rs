use std::fmt;

use crate::{
    application::{
        scraper::{ScraperError, ScraperGetRequest, ScraperItemType, ScraperResolver},
        tmdb::TmdbError,
        tmdb_plugin::TmdbProvider,
    },
    storage::{Database, NewCollection, StorageError},
};

#[derive(Clone)]
pub struct CollectionService {
    database: Database,
    tmdb: TmdbProvider,
    resolver: Option<ScraperResolver>,
}

impl CollectionService {
    pub fn new<T>(database: Database, tmdb: T) -> Self
    where
        T: Into<TmdbProvider>,
    {
        Self {
            database,
            tmdb: tmdb.into(),
            resolver: None,
        }
    }

    pub fn with_resolver<T>(database: Database, tmdb: T, resolver: ScraperResolver) -> Self
    where
        T: Into<TmdbProvider>,
    {
        Self {
            database,
            tmdb: tmdb.into(),
            resolver: Some(resolver),
        }
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
        Ok(CollectionRefreshReport {
            source_item_id: item_id.to_owned(),
            collection_item_id: result.collection_item_id,
            member_count: result.member_count,
        })
    }

    async fn provider_for_item(&self, item_id: &str) -> Result<TmdbProvider, CollectionError> {
        let Some(resolver) = &self.resolver else {
            return Ok(self.tmdb.clone());
        };
        resolver
            .for_item(item_id)
            .await
            .map_err(CollectionError::Scraper)
            .map(|client| {
                client
                    .map(TmdbProvider::from_scraper)
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
    Tmdb(TmdbError),
    Scraper(ScraperError),
    Storage(StorageError),
}

impl fmt::Display for CollectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MovieProviderIdMissing => formatter.write_str("movie has no scraper provider ID"),
            Self::InvalidProviderId => formatter.write_str("scraper provider ID is invalid"),
            Self::NoCollection => formatter.write_str("movie does not belong to a collection"),
            Self::Tmdb(error) => error.fmt(formatter),
            Self::Scraper(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CollectionError {}

impl From<TmdbError> for CollectionError {
    fn from(error: TmdbError) -> Self {
        Self::Tmdb(error)
    }
}

impl From<StorageError> for CollectionError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
