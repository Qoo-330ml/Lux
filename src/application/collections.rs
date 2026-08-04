use std::fmt;

use crate::{
    application::{tmdb::TmdbError, tmdb_plugin::TmdbProvider},
    storage::{Database, NewTmdbCollection, StorageError},
};

#[derive(Clone)]
pub struct CollectionService {
    database: Database,
    tmdb: TmdbProvider,
}

impl CollectionService {
    pub fn new<T>(database: Database, tmdb: T) -> Self
    where
        T: Into<TmdbProvider>,
    {
        Self {
            database,
            tmdb: tmdb.into(),
        }
    }

    pub async fn refresh_for_item(
        &self,
        item_id: &str,
    ) -> Result<CollectionRefreshReport, CollectionError> {
        let Some(identity) = self.database.find_tmdb_movie_identity(item_id).await? else {
            return Err(CollectionError::MovieProviderIdMissing);
        };
        let provider_id = identity
            .provider_id
            .parse::<i64>()
            .map_err(|_| CollectionError::InvalidProviderId)?;
        let movie = self.tmdb.movie_details(provider_id, "zh-CN").await?;
        let Some(collection) = movie.belongs_to_collection else {
            return Err(CollectionError::NoCollection);
        };
        if collection.id <= 0 {
            return Err(CollectionError::InvalidProviderId);
        }
        let details = self.tmdb.collection_details(collection.id, "zh-CN").await?;
        let title = details
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("TMDb Collection");
        let member_provider_ids = details
            .parts
            .iter()
            .enumerate()
            .map(|(index, part)| (part.id, i64::try_from(index).unwrap_or(i64::MAX)))
            .collect::<Vec<_>>();
        let result = self
            .database
            .upsert_tmdb_collection(NewTmdbCollection {
                library_id: &identity.library_id,
                provider_id: &collection.id.to_string(),
                title,
                overview: details.overview.as_deref(),
                poster_path: details.poster_path.as_deref(),
                backdrop_path: details.backdrop_path.as_deref(),
                member_provider_ids: &member_provider_ids,
            })
            .await?;
        Ok(CollectionRefreshReport {
            source_item_id: item_id.to_owned(),
            collection_item_id: result.collection_item_id,
            member_count: result.member_count,
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
    Storage(StorageError),
}

impl fmt::Display for CollectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MovieProviderIdMissing => formatter.write_str("movie has no TMDb provider ID"),
            Self::InvalidProviderId => formatter.write_str("TMDb provider ID is invalid"),
            Self::NoCollection => formatter.write_str("movie does not belong to a collection"),
            Self::Tmdb(error) => error.fmt(formatter),
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
