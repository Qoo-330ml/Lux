//! Emby filter-panel metadata: `GET /OfficialRatings` and `GET /Tags`.
//!
//! Hills requests both endpoints when it opens a library filter panel, with the same query shape
//! as `/Genres` (`UserId`, `ParentId`, `Recursive`, `IncludeItemTypes`, `StartIndex`, `Limit`) and
//! the same `{"Items": [...], "TotalRecordCount": n}` envelope.
//!
//! `OfficialRatings` aggregates the normalized certification codes that the Emby
//! `OfficialRating` field already renders, so it reads the existing content-restriction index and
//! never parses NFO JSON in a request. `Tags` has no stored data source yet: item tags are not
//! parsed from NFO and there is no `TAG` facet kind, so the endpoint answers an empty envelope
//! instead of `404` (an empty filter list rather than a failed request). It stays empty until a
//! later task adds a tag projection.

use super::*;

/// Query of the Emby filter-panel metadata endpoints.
///
/// Unlike the `/Genres` query, an empty `StartIndex=`/`Limit=` counts as absent: Hills sends
/// `Limit=` on filter-panel requests and an empty value is not a malformed number.
#[derive(Deserialize, Default)]
pub(super) struct EmbyFilterMetadataQuery {
    #[serde(flatten)]
    pub(super) auth: EmbyTokenQuery,
    #[serde(rename = "UserId", alias = "userId", alias = "userid", default)]
    pub(super) user_id: Option<String>,
    #[serde(rename = "ParentId", alias = "parentId", default)]
    pub(super) parent_id: Option<String>,
    #[serde(rename = "IncludeItemTypes", alias = "includeItemTypes", default)]
    pub(super) include_item_types: Option<String>,
    #[serde(
        rename = "StartIndex",
        alias = "startIndex",
        default,
        deserialize_with = "empty_query_number"
    )]
    pub(super) start_index: Option<i64>,
    #[serde(
        rename = "Limit",
        alias = "limit",
        default,
        deserialize_with = "empty_query_number"
    )]
    pub(super) limit: Option<i64>,
}

/// Reads an optional integer query value where an empty string means "not provided".
fn empty_query_number<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    match value.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(raw) => raw
            .parse::<i64>()
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// The page window both endpoints share; `EMBY_MAX_PAGE_LIMIT` is the server-side upper bound.
fn emby_filter_metadata_page(query: &EmbyFilterMetadataQuery) -> Result<(i64, i64), StatusCode> {
    let offset = query.start_index.unwrap_or(0);
    let limit = match query.limit {
        Some(value) if value > 0 => value,
        _ => 50,
    };
    if offset < 0 || !(1..=EMBY_MAX_PAGE_LIMIT).contains(&limit) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok((offset, limit))
}

fn emby_filter_metadata_item_types(value: Option<&str>) -> Vec<String> {
    value
        .map(|values| {
            values
                .split(',')
                .filter_map(|item_type| normalize_emby_item_type(item_type.trim()))
                .collect()
        })
        .unwrap_or_default()
}

/// `ParentId=` (Hills sends an empty value on some filter-panel requests) means "no library
/// filter", not "the empty id".
fn emby_filter_metadata_parent_id(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(emby_internal_id)
}

pub(super) async fn emby_official_ratings(
    headers: HeaderMap,
    Query(query): Query<EmbyFilterMetadataQuery>,
    State(state): State<ReadyAppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal_with_query(&headers, &state, &query.auth).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let principal =
        match emby_access_principal_for_target(&state, &auth_principal, query.user_id.as_deref())
            .await
        {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let (offset, limit) = match emby_filter_metadata_page(&query) {
        Ok(page) => page,
        Err(status) => return status.into_response(),
    };
    let item_types = emby_filter_metadata_item_types(query.include_item_types.as_deref());
    let parent_id = emby_filter_metadata_parent_id(query.parent_id.as_deref());
    let result = state
        .catalog()
        .list_official_ratings(principal, parent_id.as_deref(), &item_types, offset, limit)
        .await;
    let (ratings, total) = match result {
        Ok(result) => result,
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(CatalogError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    Json(json!({
        "Items": ratings
            .into_iter()
            .map(|rating| {
                json!({
                    "Name": rating.certification_code,
                    "Id": rating.certification_code,
                    "ServerId": state.server_id,
                })
            })
            .collect::<Vec<_>>(),
        "TotalRecordCount": total,
        "StartIndex": offset,
    }))
    .into_response()
}

pub(super) async fn emby_tags(
    headers: HeaderMap,
    Query(query): Query<EmbyFilterMetadataQuery>,
    State(state): State<ReadyAppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal_with_query(&headers, &state, &query.auth).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    // Resolve the target user (and reject unknown ones) even though the answer is empty, so the
    // endpoint authenticates and authorizes like every other library-scoped list.
    if let Err(status) =
        emby_access_principal_for_target(&state, &auth_principal, query.user_id.as_deref()).await
    {
        return status.into_response();
    }
    let (offset, _limit) = match emby_filter_metadata_page(&query) {
        Ok(page) => page,
        Err(status) => return status.into_response(),
    };
    Json(json!({
        "Items": Vec::<Value>::new(),
        "TotalRecordCount": 0,
        "StartIndex": offset,
    }))
    .into_response()
}
