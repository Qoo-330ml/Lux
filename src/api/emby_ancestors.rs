use super::*;
use crate::storage::GLOBAL_COLLECTION_LIBRARY_ID;

/// Query parameters of `GET /Items/{item_id}/Ancestors` (and `/emby/Items/{item_id}/Ancestors`).
#[derive(Deserialize, Default)]
pub(super) struct EmbyAncestorsQuery {
    #[serde(
        rename = "api_key",
        alias = "apiKey",
        alias = "ApiKey",
        alias = "X-Emby-Token",
        alias = "x-emby-token",
        alias = "X-MediaBrowser-Token",
        alias = "x-media-browser-token",
        default
    )]
    pub(super) api_key: Option<String>,
    #[serde(rename = "UserId", alias = "userId", alias = "userid", default)]
    pub(super) user_id: Option<String>,
    #[serde(rename = "Fields", alias = "fields", default)]
    pub(super) fields: Option<String>,
}

/// `GET /Items/{item_id}/Ancestors`, also served under `/emby`.
///
/// Returns the standard Emby/Jellyfin `BaseItemDto` array ordered from the item's direct parent
/// upward to its owning virtual library `CollectionFolder` node:
/// - `Episode` -> `[Season, Series, CollectionFolder]`
/// - `Season` -> `[Series, CollectionFolder]`
/// - `Movie` / `Series` -> `[CollectionFolder]` (plus intermediate `Folder` nodes if present)
///
/// Items outside the caller's accessible libraries and unknown IDs both answer `404`.
pub(super) async fn emby_item_ancestors(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<EmbyAncestorsQuery>,
    State(state): State<ReadyAppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
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

    let internal_item_id = emby_internal_id(&item_id);
    if internal_item_id == GLOBAL_COLLECTION_LIBRARY_ID {
        let catalog = state.catalog();
        return match catalog.list_global_collection_cards(principal, 0, 1).await {
            Ok(page) if page.total > 0 => Json(Vec::<Value>::new()).into_response(),
            Ok(_) => StatusCode::NOT_FOUND.into_response(),
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    }

    if let Ok(library_id) = internal_item_id.parse::<crate::domain::ids::LibraryId>() {
        let libraries = state.libraries();
        match libraries.get_library(library_id).await {
            Ok(library) => {
                if !library.is_enabled {
                    return StatusCode::NOT_FOUND.into_response();
                }
                let access = state.access();
                return match access.can_view_library(principal, &internal_item_id).await {
                    Ok(true) => Json(Vec::<Value>::new()).into_response(),
                    Ok(false) => StatusCode::NOT_FOUND.into_response(),
                    Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                };
            }
            Err(LibraryServiceError::LibraryNotFound) => {}
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }

    let catalog = state.catalog();
    let (item, ancestor_items) = match catalog
        .list_item_ancestors(principal, &internal_item_id)
        .await
    {
        Ok(Some(result)) => result,
        Ok(None) | Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(CatalogError::Storage(_)) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };

    let mut ancestors = if ancestor_items.is_empty() {
        Vec::with_capacity(1)
    } else {
        let page_len = ancestor_items.len() as i64;
        let ancestor_page = CatalogPage::new(ancestor_items, page_len, 0, page_len);
        match emby_catalog_items_for_user(
            &state,
            &principal.user_id_string(),
            &ancestor_page,
            query.fields.as_deref(),
            auth_principal.can_download(),
            auth_principal.can_manage_server(),
        )
        .await
        {
            Ok(items) => items,
            Err(status) => return status.into_response(),
        }
    };

    if item.library_id == GLOBAL_COLLECTION_LIBRARY_ID {
        let content_restrictions = match catalog.content_restrictions_for_principal(principal).await
        {
            Ok(restrictions) => restrictions,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        let collection_count = match catalog
            .count_global_collection_cards_with_restrictions(
                principal,
                content_restrictions.as_ref(),
            )
            .await
        {
            Ok(count) => count,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        ancestors.push(emby_global_collection_library_json(
            &state.server_id,
            collection_count,
        ));
    } else if let Ok(library_id) = item.library_id.parse::<crate::domain::ids::LibraryId>() {
        let libraries = state.libraries();
        let library = match libraries.get_library(library_id).await {
            Ok(library) if library.is_enabled => library,
            Ok(_) | Err(LibraryServiceError::LibraryNotFound) => {
                return StatusCode::NOT_FOUND.into_response();
            }
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        let counts = match catalog
            .count_library_root_items(std::slice::from_ref(&item.library_id))
            .await
        {
            Ok(counts) => counts,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        let child_count = library_root_count(counts.get(&item.library_id), library.kind);
        ancestors.push(emby_library_view_json(
            &library,
            &state.server_id,
            child_count,
        ));
    }

    Json(ancestors).into_response()
}
