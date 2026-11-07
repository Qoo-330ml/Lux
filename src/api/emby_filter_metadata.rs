//! Emby filter-panel metadata: `GET /OfficialRatings` and `GET /Tags`.
//!
//! Hills requests both endpoints when it opens a library filter panel, with the same query shape
//! as `/Genres` (`UserId`, `ParentId`, `Recursive`, `IncludeItemTypes`, `StartIndex`, `Limit`) and
//! the same `{"Items": [...], "TotalRecordCount": n}` envelope.
//!
//! `OfficialRatings` lists the subset of rating codes the content-restriction index recognized
//! (normalized MPA/TVPG/BBFC codes). The index is filled from NFO metadata in the background, so a
//! request never parses NFO JSON. These values are **not** the raw NFO certification the Emby
//! `OfficialRating` field renders: an unrecognized rating does not appear at all, and a recognized
//! one may be spelled differently (an NFO `Rated R` is listed as `R`).
//!
//! `Tags` reads the `TAG` projection of `media_item_facets` through `CatalogService::list_tags`,
//! the same service that answers the native `GET /api/v1/tags` route, so both endpoints name the
//! same tags for one library and one user. The projection is written by the same background
//! worker, so a request never parses NFO JSON; tags are de-duplicated on their case-insensitive
//! name key (`Star Wars` and `star wars` are one entry) and ordered by it. The library ACL, the
//! content restrictions and the page bounds follow `/OfficialRatings` exactly. Items projected
//! before migration 0249 carry no tag rows until that backfill has run, so an older library keeps
//! answering an empty list until then.

use super::*;

/// Query of the Emby filter-panel metadata endpoints.
///
/// `StartIndex=`/`Limit=` reuse the shared `deserialize_optional_i64`, so an empty or
/// whitespace-only value counts as absent (Hills sends `Limit=` on filter-panel requests) while a
/// malformed non-empty value still answers `400`.
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
        deserialize_with = "deserialize_optional_i64"
    )]
    pub(super) start_index: Option<i64>,
    #[serde(
        rename = "Limit",
        alias = "limit",
        default,
        deserialize_with = "deserialize_optional_i64"
    )]
    pub(super) limit: Option<i64>,
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
    // Reuses the `/Items` rule: an entirely unrecognized list matches nothing instead of
    // silently dropping the restriction.
    let item_types = emby_item_types_from_value(query.include_item_types.as_deref());
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
    // Same rules as `/OfficialRatings`: an entirely unrecognized type list matches nothing, and a
    // non-empty `ParentId` naming an unknown or inaccessible library answers 404 (the service
    // enforces the ACL, exactly like the rating list).
    let item_types = emby_item_types_from_value(query.include_item_types.as_deref());
    let parent_id = emby_filter_metadata_parent_id(query.parent_id.as_deref());
    let result = state
        .catalog()
        .list_tags(principal, parent_id.as_deref(), &item_types, offset, limit)
        .await;
    let (tags, total) = match result {
        Ok(result) => result,
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(CatalogError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    Json(json!({
        "Items": tags
            .into_iter()
            .map(|tag| {
                json!({
                    "Name": tag.name,
                    "Id": tag.name,
                    "ServerId": state.server_id,
                })
            })
            .collect::<Vec<_>>(),
        "TotalRecordCount": total,
        "StartIndex": offset,
    }))
    .into_response()
}
