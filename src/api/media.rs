use super::*;
use crate::application::playback::session::EMBY_DIRECT_STREAM_TICKET_VERSION;

const LUX_SEARCH_ITEM_TYPES: [&str; 3] = ["MOVIE", "SERIES", "VIDEO"];
const MAX_HOME_LATEST_LIBRARY_IDS: usize = 100;
const HOME_LATEST_ITEMS_PER_LIBRARY: i64 = 12;

#[derive(Deserialize, Default)]
pub(super) struct LuxImageQuery {
    #[serde(default)]
    #[allow(dead_code)]
    tag: Option<String>,
    #[serde(default)]
    w: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct LuxPageQuery {
    #[serde(default)]
    page: Option<i64>,
    #[serde(rename = "pageSize", default)]
    page_size: Option<i64>,
    #[serde(rename = "itemType", default)]
    item_type: Option<String>,
    #[serde(rename = "parentId", default)]
    parent_id: Option<String>,
    #[serde(default)]
    year: Option<i64>,
    #[serde(default)]
    is_played: Option<bool>,
    #[serde(default)]
    is_favorite: Option<bool>,
    #[serde(rename = "sort_by", alias = "sortBy", default)]
    sort_by: Option<String>,
    #[serde(rename = "sort_order", alias = "sortOrder", default)]
    sort_order: Option<String>,
    #[serde(rename = "metadataStatus", default)]
    metadata_status: Option<String>,
    #[serde(default)]
    fields: Option<String>,
    #[serde(default)]
    count: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct LuxSearchQuery {
    #[serde(alias = "query")]
    q: Option<String>,
    page: Option<i64>,
    page_size: Option<i64>,
}

pub(super) async fn lux_search(
    headers: HeaderMap,
    Query(query): Query<LuxSearchQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(raw_query) = query.q.as_deref() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(search_query) = normalize_search_query(raw_query) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(like_query) = normalize_search_like_query(raw_query) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog
        .search_items_with_types(
            AccessPrincipal::new(user.id, user.is_admin),
            &search_query,
            &like_query,
            LUX_SEARCH_ITEM_TYPES
                .iter()
                .map(|item_type| (*item_type).to_owned())
                .collect(),
            offset,
            limit,
        )
        .await
    {
        Ok(page) => {
            match lux_catalog_page_json_for_user(database, &user.id.to_string(), &page).await {
                Ok(body) => Json(body).into_response(),
                Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            StatusCode::FORBIDDEN.into_response()
        }
    }
}

pub(super) async fn lux_home(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(home) = state.home.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let user_id = user.id.to_string();
    let accessible_library_ids = match access.accessible_library_ids(principal).await {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let snapshot = match home
        .snapshot(principal, accessible_library_ids.clone())
        .await
    {
        Ok(value) => value,
        Err(HomeError::Catalog(_) | HomeError::Libraries(_)) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let accessible_library_ids = accessible_library_ids.into_iter().collect::<HashSet<_>>();
    let latest_groups = snapshot
        .latest_groups
        .iter()
        .filter(|(library_id, _)| accessible_library_ids.contains(library_id))
        .cloned()
        .collect::<Vec<_>>();
    let latest_items = latest_groups
        .iter()
        .flat_map(|(_, items)| items.iter().cloned())
        .collect::<Vec<_>>();
    let all_items = snapshot
        .continue_watching
        .items
        .iter()
        .chain(snapshot.recently_added.items.iter())
        .chain(snapshot.recommended.iter())
        .chain(latest_items.iter())
        .cloned()
        .collect::<Vec<_>>();
    let user_values = match lux_catalog_item_values_by_id(database, &user_id, &all_items).await {
        Ok(values) => values,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let continue_watching_items =
        lux_catalog_items_from_values(&snapshot.continue_watching.items, &user_values);
    let recently_added_items =
        lux_catalog_items_from_values(&snapshot.recently_added.items, &user_values);
    let recommended_items = lux_catalog_items_from_values(&snapshot.recommended, &user_values);
    let latest_values = lux_catalog_items_from_values(&latest_items, &user_values);
    let mut latest_by_library = BTreeMap::<String, Vec<Value>>::new();
    for (item, value) in latest_items.iter().zip(latest_values) {
        latest_by_library
            .entry(item.library_id.clone())
            .or_default()
            .push(value);
    }
    let mut visible = Vec::new();
    for view in &snapshot.views {
        let library_id = view.library.id.to_string();
        if !accessible_library_ids.contains(&library_id) {
            continue;
        }
        visible.push(json!({
            "id": view.library.id,
            "name": view.library.name,
            "kind": view.library.kind.as_str(),
            "coverImageUrl": library_cover_url(&view.library),
            "latest": latest_by_library
                .get(&library_id)
                .cloned()
                .unwrap_or_default(),
        }));
    }
    Json(json!({
        "continueWatching": continue_watching_items,
        "continueWatchingTotal": snapshot.continue_watching.total,
        "recentlyAdded": recently_added_items,
        "recentlyAddedTotal": snapshot.recently_added.total,
        "recommended": recommended_items,
        "libraries": visible,
    }))
    .into_response()
}

pub(super) async fn lux_home_carousel(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(home) = state.home.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let user_id = user.id.to_string();
    let library_ids = match access.accessible_library_ids(principal).await {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let recommended = match home.carousel(principal, library_ids).await {
        Ok(items) => items,
        Err(HomeError::Catalog(_) | HomeError::Libraries(_)) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    match lux_catalog_items_json_for_user(database, &user_id, &recommended).await {
        Ok(items) => Json(json!({ "recommended": items })).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn lux_list_home_library_latest(
    headers: HeaderMap,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let mut requested_library_ids = Vec::new();
    if let Some(query) = raw_query.0 {
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            if name == "libraryId" {
                if requested_library_ids.len() == MAX_HOME_LATEST_LIBRARY_IDS {
                    return api_error(
                        &headers,
                        StatusCode::BAD_REQUEST,
                        lux::ApiErrorCode::InvalidRequest,
                        "媒体库 ID 数量必须为 1 到 100",
                    )
                    .into_response();
                }
                requested_library_ids.push(value.into_owned());
            }
        }
    }
    if requested_library_ids.is_empty() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体库 ID 数量必须为 1 到 100",
        )
        .into_response();
    }

    let mut library_ids = Vec::with_capacity(requested_library_ids.len());
    let mut unique_library_ids = HashSet::with_capacity(requested_library_ids.len());
    for raw_id in requested_library_ids {
        let library_id = match raw_id.parse::<crate::domain::ids::LibraryId>() {
            Ok(id) => id.to_string(),
            Err(_) => {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "媒体库 ID 无效",
                )
                .into_response();
            }
        };
        if unique_library_ids.insert(library_id.clone()) {
            library_ids.push(library_id);
        }
    }

    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let accessible_library_ids = match access.accessible_library_ids(principal).await {
        Ok(ids) => ids.into_iter().collect::<HashSet<_>>(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let regular_library_ids = library_ids
        .iter()
        .filter(|library_id| *library_id != crate::storage::GLOBAL_COLLECTION_LIBRARY_ID)
        .cloned()
        .collect::<Vec<_>>();
    if regular_library_ids
        .iter()
        .any(|library_id| !accessible_library_ids.contains(library_id))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let content_restrictions = match catalog.content_restrictions_for_principal(principal).await {
        Ok(restrictions) => restrictions,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    let mut latest_items = Vec::new();
    if !regular_library_ids.is_empty() {
        let latest_groups = match catalog
            .list_recently_added_by_library_ids(
                &regular_library_ids,
                content_restrictions.as_ref(),
                HOME_LATEST_ITEMS_PER_LIBRARY,
            )
            .await
        {
            Ok(groups) => groups,
            Err(CatalogError::Storage(_)) => {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
                return StatusCode::FORBIDDEN.into_response();
            }
        };
        latest_items.extend(latest_groups.into_iter().flat_map(|(library_id, items)| {
            items
                .into_iter()
                .map(move |item| (library_id.clone(), item))
        }));
    }

    let mut collection_card_fields = HashMap::new();
    if library_ids
        .iter()
        .any(|library_id| library_id == crate::storage::GLOBAL_COLLECTION_LIBRARY_ID)
    {
        let collection_page = match catalog
            .list_recent_global_collection_cards(principal, 0, HOME_LATEST_ITEMS_PER_LIBRARY)
            .await
        {
            Ok(page) => page,
            Err(CatalogError::Storage(_)) => {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
                return StatusCode::FORBIDDEN.into_response();
            }
        };
        if collection_page.total == 0 {
            return StatusCode::FORBIDDEN.into_response();
        }
        for card in collection_page.cards {
            collection_card_fields.insert(
                card.item.id.clone(),
                (card.member_count, card.poster_item_id.clone()),
            );
            latest_items.push((
                crate::storage::GLOBAL_COLLECTION_LIBRARY_ID.to_owned(),
                card.item,
            ));
        }
    }

    let catalog_items = latest_items
        .iter()
        .map(|(_, item)| item.clone())
        .collect::<Vec<_>>();
    let item_values =
        match lux_catalog_items_json_for_user(database, &user.id.to_string(), &catalog_items).await
        {
            Ok(values) => values,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    let mut values_by_library = HashMap::<String, Vec<Value>>::new();
    for ((library_id, item), mut value) in latest_items.into_iter().zip(item_values) {
        if library_id == crate::storage::GLOBAL_COLLECTION_LIBRARY_ID {
            if let Some((member_count, poster_item_id)) = collection_card_fields.get(&item.id) {
                add_global_collection_card_fields(
                    &mut value,
                    *member_count,
                    poster_item_id.as_deref(),
                );
            }
        }
        values_by_library.entry(library_id).or_default().push(value);
    }
    let libraries = library_ids
        .into_iter()
        .map(|library_id| {
            json!({
                "libraryId": library_id,
                "items": values_by_library.remove(&library_id).unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    Json(json!({ "libraries": libraries })).into_response()
}

pub(super) async fn lux_list_continue_watching(
    headers: HeaderMap,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let (offset, limit) = match page_params(
        Some(query.page.unwrap_or(1)),
        Some(query.page_size.unwrap_or(10)),
    ) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog
        .list_home_continue_watching(
            AccessPrincipal::new(user.id, user.is_admin),
            &user.id.to_string(),
            offset,
            limit,
        )
        .await
    {
        Ok(page) => {
            match lux_catalog_page_json_for_user(database, &user.id.to_string(), &page).await {
                Ok(body) => Json(body).into_response(),
                Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            StatusCode::FORBIDDEN.into_response()
        }
    }
}

pub(super) async fn lux_list_library_latest(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id.to_string(),
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库 ID 无效",
            )
            .into_response();
        }
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let accessible_library_ids = match access.accessible_library_ids(principal).await {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if !accessible_library_ids.iter().any(|id| id == &library_id) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let content_restrictions = match catalog.content_restrictions_for_principal(principal).await {
        Ok(restrictions) => restrictions,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match catalog
        .list_recently_added_by_library_ids(
            std::slice::from_ref(&library_id),
            content_restrictions.as_ref(),
            12,
        )
        .await
    {
        Ok(groups) => {
            let items = groups
                .into_iter()
                .flat_map(|(_, items)| items)
                .collect::<Vec<_>>();
            match lux_catalog_items_json_for_user(database, &user.id.to_string(), &items).await {
                Ok(items) => Json(json!({ "items": items })).into_response(),
                Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            StatusCode::FORBIDDEN.into_response()
        }
    }
}

#[derive(Deserialize, Default)]
pub(super) struct EmbySearchQuery {
    #[serde(
        rename = "api_key",
        alias = "apiKey",
        alias = "ApiKey",
        alias = "X-Emby-Token",
        alias = "x-emby-token",
        alias = "X-MediaBrowser-Token",
        alias = "x-media-browser-token"
    )]
    api_key: Option<String>,
    #[serde(rename = "SearchTerm", alias = "searchTerm")]
    search_term: Option<String>,
    #[serde(rename = "StartIndex", alias = "startIndex")]
    start_index: Option<i64>,
    #[serde(rename = "Limit", alias = "limit")]
    limit: Option<i64>,
}

pub(super) async fn emby_search_hints(
    headers: HeaderMap,
    Query(query): Query<EmbySearchQuery>,
    State(state): State<AppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let principal = match emby_access_principal(&auth_principal, None) {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    let Some(raw_query) = query.search_term.as_deref() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(search_query) = normalize_search_query(raw_query) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(like_query) = normalize_search_like_query(raw_query) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let page_query = EmbyItemsQuery {
        start_index: query.start_index,
        limit: query.limit,
        ..EmbyItemsQuery::default()
    };
    let (offset, limit) = match emby_page_params(&page_query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let page = match catalog
        .search_items(principal, &search_query, &like_query, offset, limit)
        .await
    {
        Ok(page) => page,
        Err(CatalogError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            return StatusCode::FORBIDDEN.into_response();
        }
    };
    let hints = page
        .items
        .iter()
        .map(|item| {
            json!({
                "Id": emby_public_id(&item.id),
                "Name": item.title,
                "Type": emby_item_type(&item.item_type),
                "MediaType": "Video",
                "ProductionYear": item.production_year,
                "RunTimeTicks": item.runtime_ticks,
            })
        })
        .collect::<Vec<_>>();
    Json(json!({
        "SearchHints": hints,
        "TotalRecordCount": page.total,
    }))
    .into_response()
}

pub(super) async fn lux_list_libraries(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let show_metadata_pending = match read_media_strategy_settings(database).await {
        Ok(settings) => settings.show_metadata_pending,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let accessible_library_ids = match access.accessible_library_ids(principal).await {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match libraries
        .list_libraries_for_user(&user.id.to_string(), user.is_admin, &accessible_library_ids)
        .await
    {
        Ok(views) => {
            let mut visible = Vec::new();
            for view in views {
                visible.push(json!({
                    "id": view.library.id.to_string(),
                    "name": view.library.name,
                    "kind": view.library.kind.as_str(),
                    "coverImageUrl": library_cover_url(&view.library),
                }));
            }
            let Some(catalog) = state.catalog.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            let collections = match catalog.list_global_collection_cards(principal, 0, 1).await {
                Ok(collections) => collections,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            let mut order_scope = accessible_library_ids;
            if collections.total > 0 {
                order_scope.push(crate::storage::GLOBAL_COLLECTION_LIBRARY_ID.to_owned());
                visible.push(json!({
                    "id": crate::storage::GLOBAL_COLLECTION_LIBRARY_ID,
                    "name": "合集",
                    "kind": "MIXED",
                    "coverImageUrl": null,
                }));
            }
            let order = match libraries
                .saved_library_order_for_user(&user.id.to_string(), user.is_admin, &order_scope)
                .await
            {
                Ok(order) => order,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            let order_index = order
                .into_iter()
                .enumerate()
                .map(|(index, library_id)| (library_id, index))
                .collect::<HashMap<_, _>>();
            visible.sort_by_key(|library| {
                library["id"]
                    .as_str()
                    .and_then(|library_id| order_index.get(library_id))
                    .copied()
                    .unwrap_or(usize::MAX)
            });
            Json(json!({
                "libraries": visible,
                "showMetadataPending": show_metadata_pending,
            }))
            .into_response()
        }
        Err(error) => library_error(&headers, error),
    }
}

pub(super) async fn lux_library_cover(
    headers: HeaderMap,
    method: Method,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    match access
        .can_view_library(principal, &library_id.to_string())
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(covers) = state.library_covers.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let cover = match covers.resolve(library_id).await {
        Ok(Some(cover)) => cover,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(LibraryCoverError::LibraryNotFound | LibraryCoverError::InvalidPath) => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(LibraryCoverError::Storage(_)) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(
            LibraryCoverError::Io { .. }
            | LibraryCoverError::ImageWrite(_)
            | LibraryCoverError::FontNotFound
            | LibraryCoverError::Render(_)
            | LibraryCoverError::RenderPanicked
            | LibraryCoverError::GeneratedCoverRace
            | LibraryCoverError::GenerationUnavailable
            | LibraryCoverError::TaskNotRegistered
            | LibraryCoverError::JobNotFound
            | LibraryCoverError::AlreadyActive,
        ) => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(
            LibraryCoverError::UnsupportedContentType(_)
            | LibraryCoverError::InvalidContent { .. }
            | LibraryCoverError::TooLarge { .. },
        ) => return StatusCode::NOT_FOUND.into_response(),
    };
    if if_none_match_matches(&headers, &cover.etag) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header("ETag", &cover.etag)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let Ok(file) = tokio::fs::File::open(&cover.path).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        Body::from_stream(tokio_util::io::ReaderStream::new(file))
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", &cover.content_type)
        .header("Content-Length", cover.content_length)
        .header("ETag", &cover.etag)
        .header("Cache-Control", "private, max-age=3600")
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub(super) async fn lux_list_favorites(
    headers: HeaderMap,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let (offset, limit) = match lux_page_params(&query) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let filter = CatalogFilter {
        is_favorite: Some(true),
        sort_by: CatalogSort::DateCreated,
        descending: true,
        ..CatalogFilter::default()
    };
    match catalog
        .list_all_items_filtered(
            AccessPrincipal::new(user.id, user.is_admin),
            &filter,
            offset,
            limit,
        )
        .await
    {
        Ok(page) => {
            match lux_catalog_page_json_for_user(database, &user.id.to_string(), &page).await {
                Ok(body) => Json(body).into_response(),
                Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            StatusCode::FORBIDDEN.into_response()
        }
    }
}

pub(super) async fn lux_list_library_items(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id.to_string(),
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库 ID 无效",
            )
            .into_response();
        }
    };
    let (offset, limit) = match lux_page_params(&query) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let projection = match query.fields.as_deref() {
        None => crate::application::catalog::CatalogItemProjection::Full,
        Some(value) if value.eq_ignore_ascii_case("card") => {
            crate::application::catalog::CatalogItemProjection::Card
        }
        Some(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "fields 参数无效",
            )
            .into_response();
        }
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let metadata_pending = match query.metadata_status.as_deref() {
        None => false,
        Some(value) if value.eq_ignore_ascii_case("PENDING") => true,
        Some(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "元数据状态无效",
            )
            .into_response();
        }
    };
    if library_id == crate::storage::GLOBAL_COLLECTION_LIBRARY_ID {
        let item_type_matches = query.item_type.as_deref().is_none_or(|item_types| {
            item_types
                .split(',')
                .any(|item_type| item_type.trim().eq_ignore_ascii_case("BOX_SET"))
        });
        if query.parent_id.is_some() || !item_type_matches {
            return Json(json!({
                "items": [],
                "total": 0,
                "page": offset / limit + 1,
                "pageSize": limit,
            }))
            .into_response();
        }
        return match catalog
            .list_global_collection_cards(principal, offset, limit)
            .await
        {
            Ok(page) => {
                let items = page
                    .cards
                    .iter()
                    .map(|card| card.item.clone())
                    .collect::<Vec<_>>();
                let mut values = match lux_catalog_items_json_for_user_with_projection(
                    database,
                    &user.id.to_string(),
                    &items,
                    projection,
                )
                .await
                {
                    Ok(values) => values,
                    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                };
                for (value, card) in values.iter_mut().zip(&page.cards) {
                    add_global_collection_card_fields(
                        value,
                        card.member_count,
                        card.poster_item_id.as_deref(),
                    );
                }
                Json(json!({
                    "items": values,
                    "total": page.total,
                    "page": offset / limit + 1,
                    "pageSize": limit,
                }))
                .into_response()
            }
            Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
                StatusCode::NOT_FOUND.into_response()
            }
        };
    }
    let mut filter = catalog_filter_from_values(
        query.item_type.as_deref(),
        query.year.map(|year| year.to_string()).as_deref(),
        query.is_played,
        query.is_favorite,
        query.sort_by.as_deref(),
        query.sort_order.as_deref(),
        metadata_pending,
    );
    let parent_scope = match query.parent_id.as_deref() {
        None => None,
        Some(parent_id) if parent_id.eq_ignore_ascii_case("root") => {
            Some(crate::application::catalog::CatalogParentScope::Root)
        }
        Some(parent_id) => Some(crate::application::catalog::CatalogParentScope::Item(
            parent_id.to_owned(),
        )),
    };
    if query.count == Some(false) {
        filter.include_total = false;
    }
    if parent_scope.is_some() {
        if filter.item_types.is_empty() {
            filter.item_types = vec!["FOLDER".to_owned(), "VIDEO".to_owned()];
        } else {
            filter
                .item_types
                .retain(|item_type| matches!(item_type.as_str(), "FOLDER" | "VIDEO"));
            if filter.item_types.is_empty() {
                filter.item_types.push("__NO_MATCH__".to_owned());
            }
        }
    }
    if query.item_type.as_deref().is_some_and(|item_types| {
        item_types
            .split(',')
            .any(|item_type| item_type.trim().eq_ignore_ascii_case("VIDEO"))
    }) {
        filter
            .item_types
            .retain(|item_type| item_type != "__NO_MATCH__");
        if !filter
            .item_types
            .iter()
            .any(|item_type| item_type == "VIDEO")
        {
            filter.item_types.push("VIDEO".to_owned());
        }
    }
    match catalog
        .list_library_items_in_scope(principal, &library_id, &filter, parent_scope, offset, limit)
        .await
    {
        Ok(page) => {
            match lux_catalog_page_json_for_user_with_projection(
                database,
                &user.id.to_string(),
                &page,
                projection,
            )
            .await
            {
                Ok(body) => Json(body).into_response(),
                Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
        Err(CatalogError::LibraryNotFound) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体库不存在",
        )
        .into_response(),
        Err(CatalogError::AccessDenied) => api_error(
            &headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "没有媒体库访问权限",
        )
        .into_response(),
        Err(CatalogError::Storage(_)) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        )
        .into_response(),
    }
}

pub(super) async fn lux_get_item(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let Some(catalog) = state.catalog.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog.find_item(principal, &item_id).await {
        Ok(Some(item)) => {
            match load_lux_item_detail(&state, database, &item, &user.id.to_string()).await {
                Ok(detail) => Json(detail.body).into_response(),
                Err(()) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体条目不存在",
        )
        .into_response(),
        Err(CatalogError::Storage(_)) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        )
        .into_response(),
        // The catalog already maps inaccessible items to `Ok(None)`; treat any leak the same
        // way rather than aborting the request.
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体条目不存在",
        )
        .into_response(),
    }
}

pub(super) struct LuxItemDetail {
    pub(super) body: Value,
    pub(super) user_state: Option<crate::storage::StoredUserItemState>,
}

pub(super) async fn load_lux_item_detail(
    state: &AppState,
    database: &Database,
    item: &CatalogItem,
    user_id: &str,
) -> Result<LuxItemDetail, ()> {
    let (metadata_pending, local_metadata_pending, user_state, actors, nfo) = tokio::try_join!(
        async {
            database
                .list_pending_metadata_item_ids(std::slice::from_ref(&item.id))
                .await
                .map(|item_ids| item_ids.contains(&item.id))
                .map_err(|_| ())
        },
        async {
            database
                .list_pending_local_metadata_item_ids(std::slice::from_ref(&item.id))
                .await
                .map(|item_ids| item_ids.contains(&item.id))
                .map_err(|_| ())
        },
        async {
            database
                .find_user_item_state(user_id, &item.id)
                .await
                .map_err(|_| ())
        },
        async {
            let actors = match state.people.as_ref() {
                Some(people) => match people.list_item_actors(&item.id).await {
                    Ok(actors) => actors,
                    Err(error) => {
                        tracing::warn!(
                            item_id = %item.id,
                            %error,
                            "derived actor relation is unavailable; returning an empty cast"
                        );
                        Vec::new()
                    }
                },
                None => Vec::new(),
            };
            Ok::<_, ()>(actors)
        },
        async {
            let nfo = match state.local_nfo.as_ref() {
                Some(local_nfo) => match local_nfo.read_item(&item.id).await {
                    Ok(nfo) => nfo,
                    Err(error) => {
                        tracing::warn!(
                            item_id = %item.id,
                            %error,
                            "derived local NFO cache is unavailable; returning partial item detail"
                        );
                        None
                    }
                },
                None => None,
            };
            Ok::<_, ()>(nfo)
        },
    )
    .map_err(|_| ())?;
    let mut body = lux_catalog_item_json_with_user_state(item, user_state.as_ref());
    if let Value::Object(object) = &mut body {
        object.insert("addedAt".to_owned(), json!(item.added_at));
        object.insert("actors".to_owned(), json!(actors));
        object.insert("nfo".to_owned(), json!(nfo));
        object.insert("metadataPending".to_owned(), json!(metadata_pending));
        object.insert(
            "localMetadataPending".to_owned(),
            json!(local_metadata_pending),
        );
        if let Some(nfo) = nfo.as_ref() {
            apply_local_nfo_details(object, nfo);
        }
    }
    Ok(LuxItemDetail { body, user_state })
}

pub(super) async fn lux_get_metadata(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_media_item_metadata(&item_id).await {
        Ok(Some(metadata)) => Json(metadata_json(
            &metadata.title,
            metadata.original_title.as_deref(),
            metadata.overview.as_deref(),
            metadata.production_year,
            metadata.locked_fields_json.as_deref(),
        ))
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn lux_update_metadata(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<UpdateItemMetadataRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(writes) = state.metadata_writes.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match writes
        .write_item_metadata(
            &item_id,
            MetadataWriteRequest {
                title: request.title,
                original_title: request.original_title,
                overview: request.overview,
                production_year: request.production_year,
                locked_fields: request.locked_fields.into_iter().collect(),
            },
        )
        .await
    {
        Ok(result) => {
            record_audit_event(
                &state,
                &headers,
                "METADATA_EDITED",
                Some("item"),
                Some(&item_id),
                "{}",
            )
            .await;
            let locked_fields_json =
                serde_json::to_string(&result.locked_fields).unwrap_or_else(|_| "[]".to_owned());
            Json(metadata_json(
                &result.title,
                result.original_title.as_deref(),
                result.overview.as_deref(),
                result.production_year.map(i64::from),
                Some(&locked_fields_json),
            ))
            .into_response()
        }
        Err(error) => metadata_write_error(&headers, error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UpdateItemMetadataRequest {
    title: String,
    original_title: Option<String>,
    overview: Option<String>,
    production_year: Option<i32>,
    #[serde(default)]
    locked_fields: Vec<MetadataField>,
}

pub(super) fn metadata_json(
    title: &str,
    original_title: Option<&str>,
    overview: Option<&str>,
    production_year: Option<i64>,
    locked_fields_json: Option<&str>,
) -> Value {
    let locked_fields = locked_fields_json
        .and_then(|value| serde_json::from_str::<Vec<MetadataField>>(value).ok())
        .unwrap_or_default();
    json!({
        "title": title,
        "originalTitle": original_title,
        "overview": overview,
        "productionYear": production_year,
        "lockedFields": locked_fields,
    })
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct LuxChildrenQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    item_type: Option<String>,
    season_id: Option<String>,
}

pub(super) async fn lux_get_children(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<LuxChildrenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let Some(parent) = (match catalog.find_item(principal, &item_id).await {
        Ok(parent) => parent,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let result = match parent.item_type.as_str() {
        "BOX_SET" => {
            catalog
                .list_collection_items(principal, &item_id, offset, limit)
                .await
        }
        "SERIES"
            if query
                .item_type
                .as_deref()
                .is_some_and(|item_type| item_type.eq_ignore_ascii_case("EPISODE"))
                || query.season_id.is_some() =>
        {
            catalog
                .list_series_episodes(
                    principal,
                    &item_id,
                    query.season_id.as_deref(),
                    offset,
                    limit,
                )
                .await
        }
        "SERIES" => {
            catalog
                .list_children(principal, &item_id, "SEASON", offset, limit)
                .await
        }
        _ => Ok(CatalogPage::new(Vec::new(), 0, offset, limit)),
    };
    match result {
        Ok(page) => {
            match lux_catalog_page_json_for_user(database, &user.id.to_string(), &page).await {
                Ok(body) => Json(body).into_response(),
                Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn lux_get_collection(
    headers: HeaderMap,
    Path(collection_id): Path<String>,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let (offset, limit) = match lux_page_params(&query) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    match catalog
        .find_global_collection_card(principal, &collection_id)
        .await
    {
        Ok(Some(card)) => {
            let collection_state = match database
                .find_user_item_state(&user.id.to_string(), &card.item.id)
                .await
            {
                Ok(state) => state,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            let page = match catalog
                .list_global_collection_members(principal, &collection_id, offset, limit)
                .await
            {
                Ok(page) => page,
                Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
                    return StatusCode::NOT_FOUND.into_response();
                }
                Err(CatalogError::Storage(_)) => {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
            };
            let member_items = page
                .members
                .iter()
                .map(|member| member.item.clone())
                .collect::<Vec<_>>();
            let values = match lux_catalog_items_json_for_user(
                database,
                &user.id.to_string(),
                &member_items,
            )
            .await
            {
                Ok(values) => values,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            let items = values
                .into_iter()
                .zip(&page.members)
                .map(|(mut value, member)| {
                    if let Value::Object(object) = &mut value {
                        object.insert(
                            "sources".to_owned(),
                            Value::Array(
                                member
                                    .sources
                                    .iter()
                                    .map(lux_global_collection_source_copy_json)
                                    .collect(),
                            ),
                        );
                        object.insert(
                            "preferredSourceItemId".to_owned(),
                            json!(member.preferred_source_item_id),
                        );
                        object.insert(
                            "preferredMediaSourceId".to_owned(),
                            json!(member.preferred_media_source_id),
                        );
                    }
                    value
                })
                .collect::<Vec<_>>();
            let mut collection_value =
                lux_catalog_item_json_with_user_state(&card.item, collection_state.as_ref());
            add_global_collection_card_fields(
                &mut collection_value,
                card.member_count,
                card.poster_item_id.as_deref(),
            );
            return Json(json!({
                "collection": collection_value,
                "items": items,
                "total": page.total,
                "page": page.offset / page.limit + 1,
                "pageSize": page.limit,
            }))
            .into_response();
        }
        Ok(None) => {}
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(collection) = (match catalog.find_item(principal, &collection_id).await {
        Ok(collection) => collection,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if collection.item_type != "BOX_SET" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let collection_state = match database
        .find_user_item_state(&user.id.to_string(), &collection.id)
        .await
    {
        Ok(state) => state,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match catalog
        .list_collection_items(principal, &collection_id, offset, limit)
        .await
    {
        Ok(page) => {
            let items =
                match lux_catalog_items_json_for_user(database, &user.id.to_string(), &page.items)
                    .await
                {
                    Ok(items) => items,
                    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                };
            Json(json!({
                "collection": lux_catalog_item_json_with_user_state(&collection, collection_state.as_ref()),
                "items": items,
                "total": page.total,
                "page": page.offset / page.limit + 1,
                "pageSize": page.limit,
            }))
            .into_response()
        }
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LuxGlobalCollectionSourceRequest {
    source_item_id: String,
    #[serde(default)]
    media_source_id: Option<String>,
}

pub(super) async fn lux_set_collection_source(
    headers: HeaderMap,
    Path((collection_id, work_item_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<LuxGlobalCollectionSourceRequest>,
) -> Response {
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog
        .set_global_collection_source_preference(
            AccessPrincipal::new(user.id, user.is_admin),
            &collection_id,
            &work_item_id,
            &request.source_item_id,
            request.media_source_id.as_deref(),
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn lux_image(
    headers: HeaderMap,
    method: Method,
    Path((item_id, image_type)): Path<(String, String)>,
    Query(query): Query<LuxImageQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let width = match lux_image_variant_width(&state, &headers, query.w.as_deref()) {
        Ok(width) => width,
        Err(response) => return *response,
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let Some(images) = state.images.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    serve_image(
        images,
        state.image_variants.as_ref(),
        principal,
        &headers,
        &method,
        ServeImageTarget {
            item_id: &item_id,
            image_type: &image_type,
            image_index: 0,
        },
        width,
    )
    .await
}

pub(super) async fn lux_image_at_index(
    headers: HeaderMap,
    method: Method,
    Path((item_id, image_type, image_index)): Path<(String, String, String)>,
    Query(query): Query<LuxImageQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Ok(image_index) = image_index.parse::<i64>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let width = match lux_image_variant_width(&state, &headers, query.w.as_deref()) {
        Ok(width) => width,
        Err(response) => return *response,
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let Some(images) = state.images.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    serve_image(
        images,
        state.image_variants.as_ref(),
        principal,
        &headers,
        &method,
        ServeImageTarget {
            item_id: &item_id,
            image_type: &image_type,
            image_index,
        },
        width,
    )
    .await
}

fn lux_image_variant_width(
    state: &AppState,
    headers: &HeaderMap,
    requested: Option<&str>,
) -> Result<Option<u32>, Box<Response>> {
    let Some(variants) = state.image_variants.as_ref() else {
        return Ok(None);
    };
    if !variants.enabled() {
        return Ok(None);
    }
    let Some(requested) = requested else {
        return Ok(None);
    };
    let Ok(width) = requested.parse::<u32>() else {
        return Err(Box::new(
            api_error(
                headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "图片宽度无效",
            )
            .into_response(),
        ));
    };
    if !crate::application::image_variants::IMAGE_VARIANT_WIDTHS.contains(&width) {
        return Err(Box::new(
            api_error(
                headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "图片宽度无效",
            )
            .into_response(),
        ));
    }
    Ok(Some(width))
}

pub(super) async fn lux_list_item_images(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(images) = state.image_writes.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match images.list_item_images(&item_id).await {
        Ok(images) => Json(json!({
            "images": images.iter().map(|image| item_image_json(&item_id, image)).collect::<Vec<_>>()
        })).into_response(),
        Err(error) => image_write_error(&headers, error),
    }
}

pub(super) async fn lux_search_item_images(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<ItemImageSearchRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    // Image search fans out to the external scraper, so it is a management action: any other
    // caller could otherwise drive arbitrary outbound metadata requests from a user session.
    if !user.can_manage_server {
        return api_error(
            &headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "需要管理员权限",
        )
        .into_response();
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(candidates) = state.image_candidates.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match candidates
        .search(
            &item_id,
            &request.image_type,
            request.language.as_deref(),
            request.source.as_deref(),
        )
        .await
    {
        Ok(images) => Json(json!({
            "images": images.iter().map(image_candidate_json).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(error) => image_candidate_error(&headers, error),
    }
}

pub(super) async fn lux_select_item_image(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<ItemImageSelectRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(images) = state.image_writes.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let report = match images
        .download_item_image_from_scraper_candidate(&item_id, &request.image_type, &request.url)
        .await
    {
        Ok(report) => report,
        Err(error) => return image_write_error(&headers, error),
    };
    let image = match images.list_item_images(&item_id).await {
        Ok(images) => images.into_iter().find(|image| image.id == report.id),
        Err(error) => return image_write_error(&headers, error),
    };
    let Some(image) = image else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    record_audit_event(
        &state,
        &headers,
        "IMAGE_SELECTED",
        Some("item_image"),
        Some(&image.id),
        "{}",
    )
    .await;
    Json(json!({ "image": item_image_json(&item_id, &image) })).into_response()
}

pub(super) async fn lux_subtitle(
    headers: HeaderMap,
    method: Method,
    Path((item_id, stream_index)): Path<(String, String)>,
    Query(query): Query<LuxStreamQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Ok(stream_index) = stream_index.parse::<i64>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    serve_subtitle(
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
        &method,
        &item_id,
        query.source_id.as_deref(),
        stream_index,
    )
    .await
}

pub(super) async fn lux_danmaku_info(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<LuxStreamQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = ensure_lux_item_visible(&state, &headers, &user, &item_id).await {
        return response;
    }
    let Some(service) = state.danmaku.as_ref() else {
        return lux_danmaku_unavailable(&headers);
    };
    match service
        .read_registered_sidecar_for_source(&item_id, query.source_id.as_deref())
        .await
    {
        Ok(Some(_)) => Json(json!({
            "available": true,
            "format": "BILIBILI_XML",
            "sourceId": query.source_id,
            "rawUrl": lux_danmaku_raw_url(&item_id, query.source_id.as_deref()),
        }))
        .into_response(),
        Ok(None) => lux_danmaku_not_found(&headers),
        Err(_) => lux_danmaku_unavailable(&headers),
    }
}

pub(super) async fn lux_danmaku_raw(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<LuxStreamQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = ensure_lux_item_visible(&state, &headers, &user, &item_id).await {
        return response;
    }
    let Some(service) = state.danmaku.as_ref() else {
        return lux_danmaku_unavailable(&headers);
    };
    match service
        .read_registered_sidecar_for_source(&item_id, query.source_id.as_deref())
        .await
    {
        Ok(Some(bytes)) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/xml; charset=utf-8")
            .header("Cache-Control", "private, no-cache")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Ok(None) => lux_danmaku_not_found(&headers),
        Err(_) => lux_danmaku_unavailable(&headers),
    }
}

pub(super) fn lux_danmaku_not_found(headers: &HeaderMap) -> Response {
    api_error(
        headers,
        StatusCode::NOT_FOUND,
        lux::ApiErrorCode::NotFound,
        "当前媒体源没有可用弹幕",
    )
    .into_response()
}

pub(super) fn lux_danmaku_unavailable(headers: &HeaderMap) -> Response {
    api_error(
        headers,
        StatusCode::SERVICE_UNAVAILABLE,
        lux::ApiErrorCode::DatabaseUnavailable,
        "弹幕读取服务暂时不可用",
    )
    .into_response()
}

pub(super) async fn ensure_lux_item_visible(
    state: &AppState,
    headers: &HeaderMap,
    user: &UserRecord,
    item_id: &str,
) -> Result<(), Response> {
    let Some(access) = state.access.as_ref() else {
        return Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "媒体访问服务暂时不可用",
        )
        .into_response());
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), item_id)
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体条目不存在",
        )
        .into_response()),
        Err(_) => Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "媒体访问服务暂时不可用",
        )
        .into_response()),
    }
}

pub(super) fn lux_danmaku_raw_url(item_id: &str, source_id: Option<&str>) -> String {
    let mut url = format!(
        "/api/v1/items/{}/danmaku/raw",
        percent_encode_filename(item_id)
    );
    if let Some(source_id) = source_id {
        url.push_str("?sourceId=");
        url.push_str(&percent_encode_filename(source_id));
    }
    url
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct LuxStreamQuery {
    #[serde(alias = "MediaSourceId")]
    pub(super) source_id: Option<String>,
}

pub(super) async fn lux_stream(
    headers: HeaderMap,
    method: Method,
    Path(item_id): Path<String>,
    Query(query): Query<LuxStreamQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    serve_media_file(
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
        &headers,
        &method,
        &item_id,
        query.source_id.as_deref(),
        None,
    )
    .await
}

pub(super) async fn lux_download(
    headers: HeaderMap,
    method: Method,
    Path(item_id): Path<String>,
    Query(query): Query<LuxStreamQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let source = match access
        .authorized_playback_source(
            AccessPrincipal::new(user.id, user.is_admin),
            &item_id,
            query.source_id.as_deref(),
        )
        .await
    {
        Ok(Some(source)) => source,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if !user.can_download {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(downloads) = state.downloads.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let artifact = match downloads.prepare_authorized_source(&source).await {
        Ok(artifact) => artifact,
        Err(error) => return download_error_response(error),
    };
    let mut response = serve_download_artifact(downloads, &artifact, &method, &headers).await;
    add_download_header_with_name(&mut response, artifact.file_name());
    response
}

pub(super) async fn emby_image(
    headers: HeaderMap,
    method: Method,
    Path((item_id, image_type)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let internal_item_id = emby_internal_id(&item_id);
    let filmly_compat = state.filmly_image_compat_mode == FilmlyImageCompatMode::Compat
        && is_filmly_image_request(&headers)
        && query.tag.is_none();
    let principal = match optional_emby_access_principal(&headers, &state, &query).await {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    if normalize_image_type(&image_type) == Some("POSTER")
        && let Some(response) = serve_emby_library_cover(
            &state,
            principal,
            query.tag.as_deref(),
            &headers,
            &method,
            &item_id,
            0,
        )
        .await
    {
        return response;
    }
    if (principal.is_some() || query.tag.is_some())
        && let Some(response) = serve_emby_person_item_image(
            &state,
            &headers,
            &method,
            &item_id,
            &image_type,
            0,
            query.tag.as_deref(),
        )
        .await
    {
        return response;
    }
    let Some(images) = state.images.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    // Filmly's native image loader drops Emby auth headers and image tags. Its Windows
    // WebView can also issue the backdrop request with a browser UA, so keep the exception
    // limited to untagged backdrop artwork; media streams and tagged images remain gated.
    let untagged_backdrop_compat = state.filmly_image_compat_mode == FilmlyImageCompatMode::Compat
        && principal.is_none()
        && query.tag.is_none()
        && normalize_image_type(&image_type) == Some("FANART");
    if (filmly_compat || untagged_backdrop_compat) && principal.is_none() {
        return serve_filmly_compat_image(
            images,
            &headers,
            &method,
            &internal_item_id,
            &image_type,
            0,
        )
        .await;
    }
    match principal {
        Some(principal) => {
            serve_image(
                images,
                None,
                principal,
                &headers,
                &method,
                ServeImageTarget {
                    item_id: &internal_item_id,
                    image_type: &image_type,
                    image_index: 0,
                },
                None,
            )
            .await
        }
        None => {
            serve_tagged_image(
                images,
                &headers,
                &method,
                &internal_item_id,
                &image_type,
                0,
                query.tag.as_deref(),
            )
            .await
        }
    }
}

pub(super) fn is_filmly_image_request(headers: &HeaderMap) -> bool {
    header_str(headers, "user-agent").is_some_and(is_filmly_user_agent)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum FilmlyImageCompatMode {
    #[default]
    Generic,
    Compat,
}

pub(super) fn filmly_image_compat_mode_from_env_value(
    value: Option<&str>,
) -> FilmlyImageCompatMode {
    // The compat path serves untagged images without authentication, so it is opt-in only.
    if value.is_some_and(|value| value.trim().eq_ignore_ascii_case("compat")) {
        FilmlyImageCompatMode::Compat
    } else {
        FilmlyImageCompatMode::Generic
    }
}

pub(super) fn is_filmly_user_agent(value: &str) -> bool {
    value.split_ascii_whitespace().next().is_some_and(|client| {
        client.starts_with("网易爆米花")
            || client.starts_with("%E7%BD%91%E6%98%93%E7%88%86%E7%B1%B3%E8%8A%B1")
            || client
                .get(.."Filmly/".len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Filmly/"))
    })
}

pub(super) async fn emby_image_at_index(
    headers: HeaderMap,
    method: Method,
    Path((item_id, image_type, image_index)): Path<(String, String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let Ok(image_index) = image_index.parse::<i64>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let internal_item_id = emby_internal_id(&item_id);
    let filmly_compat = state.filmly_image_compat_mode == FilmlyImageCompatMode::Compat
        && is_filmly_image_request(&headers)
        && query.tag.is_none();
    let principal = match optional_emby_access_principal(&headers, &state, &query).await {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    if normalize_image_type(&image_type) == Some("POSTER")
        && let Some(response) = serve_emby_library_cover(
            &state,
            principal,
            query.tag.as_deref(),
            &headers,
            &method,
            &item_id,
            image_index,
        )
        .await
    {
        return response;
    }
    if (principal.is_some() || query.tag.is_some())
        && let Some(response) = serve_emby_person_item_image(
            &state,
            &headers,
            &method,
            &item_id,
            &image_type,
            image_index,
            query.tag.as_deref(),
        )
        .await
    {
        return response;
    }
    let Some(images) = state.images.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if filmly_compat && principal.is_none() {
        return serve_filmly_compat_image(
            images,
            &headers,
            &method,
            &internal_item_id,
            &image_type,
            image_index,
        )
        .await;
    }
    match principal {
        Some(principal) => {
            serve_image(
                images,
                None,
                principal,
                &headers,
                &method,
                ServeImageTarget {
                    item_id: &internal_item_id,
                    image_type: &image_type,
                    image_index,
                },
                None,
            )
            .await
        }
        None => {
            serve_tagged_image(
                images,
                &headers,
                &method,
                &internal_item_id,
                &image_type,
                image_index,
                query.tag.as_deref(),
            )
            .await
        }
    }
}

async fn optional_emby_access_principal(
    headers: &HeaderMap,
    state: &AppState,
    query: &EmbyTokenQuery,
) -> Result<Option<AccessPrincipal>, StatusCode> {
    let has_credentials = emby_token_from_headers(headers).is_some() || query.api_key.is_some();
    match require_emby_principal_with_query(headers, state, query).await {
        Ok(auth_principal) => emby_access_principal(&auth_principal, None).map(Some),
        Err(StatusCode::UNAUTHORIZED) if !has_credentials => Ok(None),
        Err(status) => Err(status),
    }
}

pub(super) async fn serve_emby_person_item_image(
    state: &AppState,
    headers: &HeaderMap,
    method: &Method,
    item_id: &str,
    image_type: &str,
    image_index: i64,
    tag: Option<&str>,
) -> Option<Response> {
    if image_index != 0 || normalize_image_type(image_type) != Some("POSTER") {
        return None;
    }
    let expected_tag = emby_person_image_tag(item_id);
    match tag.filter(|tag| !tag.is_empty()) {
        Some(tag) if tag != expected_tag => return None,
        _ => {}
    }
    let people = state.people.as_ref()?;
    let image = match people.profile_image_for_emby_name_or_id(item_id).await {
        Ok(Some(image)) => image,
        Ok(None) | Err(PeopleError::InvalidComponent(_)) => return None,
        Err(_) => return Some(StatusCode::SERVICE_UNAVAILABLE.into_response()),
    };
    Some(
        serve_image_file(
            &image.path,
            image.content_type,
            image.content_length,
            &format!("\"{expected_tag}\""),
            headers,
            method,
        )
        .await,
    )
}

pub(super) async fn emby_update_person_or_library_image(
    headers: HeaderMap,
    Path((item_id, image_type)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let principal = match emby_access_principal(&auth_principal, None) {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    if normalize_image_type(&image_type) != Some("POSTER") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let internal_item_id = emby_internal_id(&item_id);
    if let Ok(library_id) = internal_item_id.parse::<crate::domain::ids::LibraryId>()
        && let Some(database) = state.database.as_ref()
        && database
            .find_library(&library_id.to_string())
            .await
            .ok()
            .flatten()
            .is_some()
    {
        if !auth_principal.can_manage_server() {
            return StatusCode::FORBIDDEN.into_response();
        }
        let Some(covers) = state.library_covers.as_ref() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let content_type = headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        return match covers.store(library_id, content_type, &body).await {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(
                LibraryCoverError::UnsupportedContentType(_)
                | LibraryCoverError::InvalidContent { .. }
                | LibraryCoverError::TooLarge { .. },
            ) => StatusCode::BAD_REQUEST.into_response(),
            Err(LibraryCoverError::LibraryNotFound) => StatusCode::NOT_FOUND.into_response(),
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let library_ids = match access.accessible_library_ids(principal).await {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(people) = state.people.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let person = match people.find_person(&library_ids, "Actor", &item_id).await {
        Ok(Some(person)) => person,
        Ok(None) | Err(PeopleError::InvalidComponent(_)) => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    match people
        .update_person_image(
            &item_id,
            &person.name,
            person.provider.as_deref(),
            content_type,
            &body,
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(PeopleError::InvalidComponent(_) | PeopleError::InvalidImage(_)) => {
            StatusCode::BAD_REQUEST.into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_subtitle_with_source(
    headers: HeaderMap,
    method: Method,
    Path((item_id, media_source_id, stream_index)): Path<(String, String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let principal = match emby_access_principal(&auth_principal, None) {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    let Ok(stream_index) = stream_index.parse::<i64>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let item_id = emby_internal_id(&item_id);
    serve_subtitle(
        &state,
        principal,
        &method,
        &item_id,
        Some(&media_source_id),
        stream_index,
    )
    .await
}

pub(super) async fn emby_subtitle_without_source(
    headers: HeaderMap,
    method: Method,
    Path((item_id, stream_index)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let principal = match emby_access_principal(&auth_principal, None) {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    let Ok(stream_index) = stream_index.parse::<i64>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let item_id = emby_internal_id(&item_id);
    serve_subtitle(&state, principal, &method, &item_id, None, stream_index).await
}

#[derive(Default)]
pub(super) struct EmbyStreamQuery {
    pub(super) api_key: Option<String>,
    pub(super) client: Option<String>,
    pub(super) media_source_id: Option<String>,
    pub(super) play_session_id: Option<String>,
    pub(super) playback_user_id: Option<String>,
    pub(super) playback_is_admin: Option<bool>,
    pub(super) playback_ticket_version: Option<String>,
    pub(super) playback_expires: Option<i64>,
    pub(super) playback_signature: Option<String>,
}

pub(super) fn emby_stream_query_from_raw(raw_query: RawQuery) -> EmbyStreamQuery {
    let mut query = EmbyStreamQuery::default();
    let Some(raw_query) = raw_query.0 else {
        return query;
    };

    for (name, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
        if query.api_key.is_none()
            && (name.eq_ignore_ascii_case("api_key")
                || name.eq_ignore_ascii_case("apiKey")
                || name.eq_ignore_ascii_case("ApiKey")
                || name.eq_ignore_ascii_case("X-Emby-Token")
                || name.eq_ignore_ascii_case("X-MediaBrowser-Token")
                || name.eq_ignore_ascii_case("x-media-browser-token"))
        {
            query.api_key = Some(value.into_owned());
        } else if query.media_source_id.is_none()
            && (name.eq_ignore_ascii_case("mediaSourceId")
                || name.eq_ignore_ascii_case("MediaSourceId")
                || name.eq_ignore_ascii_case("media_source_id"))
        {
            query.media_source_id = Some(value.into_owned());
        } else if query.play_session_id.is_none()
            && (name.eq_ignore_ascii_case("PlaySessionId")
                || name.eq_ignore_ascii_case("playSessionId"))
        {
            query.play_session_id = Some(value.into_owned());
        } else if query.client.is_none()
            && (name.eq_ignore_ascii_case("X-Emby-Client") || name.eq_ignore_ascii_case("client"))
        {
            query.client = Some(value.into_owned());
        } else if query.playback_user_id.is_none() && name.eq_ignore_ascii_case("luxPlaybackUserId")
        {
            query.playback_user_id = Some(value.into_owned());
        } else if query.playback_is_admin.is_none() && name.eq_ignore_ascii_case("luxPlaybackAdmin")
        {
            query.playback_is_admin = match value.as_ref() {
                "1" => Some(true),
                "0" => Some(false),
                _ => None,
            };
        } else if query.playback_ticket_version.is_none()
            && name.eq_ignore_ascii_case("luxPlaybackTicketVersion")
        {
            query.playback_ticket_version = Some(value.into_owned());
        } else if query.playback_expires.is_none()
            && name.eq_ignore_ascii_case("luxPlaybackExpires")
        {
            query.playback_expires = value.parse().ok();
        } else if query.playback_signature.is_none()
            && name.eq_ignore_ascii_case("luxPlaybackSignature")
        {
            query.playback_signature = Some(value.into_owned());
        }
    }

    query
}

pub(super) fn emby_stream_query_has_playback_ticket(query: &EmbyStreamQuery) -> bool {
    query.playback_user_id.is_some()
        || query.playback_is_admin.is_some()
        || query.playback_ticket_version.is_some()
        || query.playback_expires.is_some()
        || query.playback_signature.is_some()
}

fn emby_direct_admission_session_id(
    headers: &HeaderMap,
    query: &EmbyStreamQuery,
    item_id: &str,
) -> String {
    if emby_stream_query_has_playback_ticket(query)
        && let Some(play_session_id) = query.play_session_id.as_deref()
    {
        return play_session_id.to_owned();
    }
    let device_id = super::emby_handlers::emby_device_info_from_headers(headers).device_id;
    let device_id = if device_id.trim().is_empty() {
        "unknown"
    } else {
        device_id.as_str()
    };
    format!("{item_id}:{device_id}")
}

pub(super) fn emby_stream_query_from_path(
    mut query: EmbyStreamQuery,
    container: &str,
) -> (String, EmbyStreamQuery) {
    let Some((container, embedded_query)) = container.split_once('?') else {
        return (container.to_owned(), query);
    };
    let embedded = emby_stream_query_from_raw(RawQuery(Some(embedded_query.to_owned())));
    if query.api_key.is_none() {
        query.api_key = embedded.api_key;
    }
    if query.media_source_id.is_none() {
        query.media_source_id = embedded.media_source_id;
    }
    if query.play_session_id.is_none() {
        query.play_session_id = embedded.play_session_id;
    }
    if query.client.is_none() {
        query.client = embedded.client;
    }
    if query.playback_user_id.is_none() {
        query.playback_user_id = embedded.playback_user_id;
    }
    if query.playback_is_admin.is_none() {
        query.playback_is_admin = embedded.playback_is_admin;
    }
    if query.playback_ticket_version.is_none() {
        query.playback_ticket_version = embedded.playback_ticket_version;
    }
    if query.playback_expires.is_none() {
        query.playback_expires = embedded.playback_expires;
    }
    if query.playback_signature.is_none() {
        query.playback_signature = embedded.playback_signature;
    }
    (container.to_owned(), query)
}

pub(super) async fn emby_stream_principal(
    headers: &HeaderMap,
    state: &AppState,
    query: &EmbyStreamQuery,
    item_id: &str,
    media_source_id: Option<&str>,
) -> Result<AccessPrincipal, StatusCode> {
    if emby_stream_query_has_playback_ticket(query) {
        let (
            Some(user_id),
            Some(is_admin),
            Some(expires_at),
            Some(signature),
            Some(source_id),
            Some(play_session_id),
        ) = (
            query.playback_user_id.as_deref(),
            query.playback_is_admin,
            query.playback_expires,
            query.playback_signature.as_deref(),
            media_source_id,
            query.play_session_id.as_deref(),
        )
        else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        let Some(service) = state.web_playback.as_ref() else {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        };
        let ticket_user_id = user_id.to_string();
        let legacy_ticket = query.playback_ticket_version.is_none();
        let is_valid = match query.playback_ticket_version.as_deref() {
            None => service.verify_emby_direct_stream(
                &ticket_user_id,
                is_admin,
                item_id,
                source_id,
                play_session_id,
                expires_at,
                signature,
            ),
            Some(EMBY_DIRECT_STREAM_TICKET_VERSION) => service.verify_emby_direct_stream_v2(
                &ticket_user_id,
                is_admin,
                item_id,
                source_id,
                play_session_id,
                expires_at,
                signature,
            ),
            Some(_) => false,
        };
        if !is_valid {
            return Err(StatusCode::UNAUTHORIZED);
        }
        let client_ip = super::request_access_ip(headers, state)
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        match service
            .authorize_emby_direct_playback_ticket_with_ip(
                &ticket_user_id,
                play_session_id,
                expires_at,
                legacy_ticket,
                client_ip,
            )
            .await
        {
            Ok(()) => {}
            Err(crate::application::playback::session::WebPlaybackSessionError::NotFound) => {
                return Err(StatusCode::UNAUTHORIZED);
            }
            Err(
                crate::application::playback::session::WebPlaybackSessionError::Expired
                | crate::application::playback::session::WebPlaybackSessionError::NotActive,
            ) => return Err(StatusCode::GONE),
            Err(
                crate::application::playback::session::WebPlaybackSessionError::AccessRulesUnavailable
                | crate::application::playback::session::WebPlaybackSessionError::Storage(_),
            ) => return Err(StatusCode::SERVICE_UNAVAILABLE),
            Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
        }
        return Ok(AccessPrincipal::new(user_id, is_admin));
    }

    let auth_principal = require_emby_principal(headers, state, query.api_key.as_deref()).await?;
    emby_access_principal(&auth_principal, None)
}

pub(super) async fn emby_stream(
    headers: HeaderMap,
    method: Method,
    Path(item_id): Path<String>,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let query = emby_stream_query_from_raw(raw_query);
    let item_id = emby_internal_id(&item_id);
    let principal = match emby_stream_principal(
        &headers,
        &state,
        &query,
        &item_id,
        query.media_source_id.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    serve_emby_media_file(
        &state,
        principal,
        &headers,
        &method,
        &item_id,
        query.media_source_id.as_deref(),
        &emby_direct_admission_session_id(&headers, &query, &item_id),
        None,
    )
    .await
}

pub(super) async fn emby_stream_with_container(
    headers: HeaderMap,
    method: Method,
    Path((item_id, container)): Path<(String, String)>,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let query = emby_stream_query_from_raw(raw_query);
    let (container, query) = emby_stream_query_from_path(query, &container);
    let item_id = emby_internal_id(&item_id);
    let principal = match emby_stream_principal(
        &headers,
        &state,
        &query,
        &item_id,
        query.media_source_id.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    serve_emby_media_file(
        &state,
        principal,
        &headers,
        &method,
        &item_id,
        query.media_source_id.as_deref(),
        &emby_direct_admission_session_id(&headers, &query, &item_id),
        Some(&container),
    )
    .await
}

pub(super) async fn emby_stream_with_source(
    headers: HeaderMap,
    method: Method,
    Path((item_id, media_source_id)): Path<(String, String)>,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let query = emby_stream_query_from_raw(raw_query);
    let item_id = emby_internal_id(&item_id);
    let principal =
        match emby_stream_principal(&headers, &state, &query, &item_id, Some(&media_source_id))
            .await
        {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    serve_emby_media_file(
        &state,
        principal,
        &headers,
        &method,
        &item_id,
        Some(&media_source_id),
        &emby_direct_admission_session_id(&headers, &query, &item_id),
        None,
    )
    .await
}

pub(super) async fn emby_stream_with_source_and_container(
    headers: HeaderMap,
    method: Method,
    Path((item_id, media_source_id, container)): Path<(String, String, String)>,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let query = emby_stream_query_from_raw(raw_query);
    let (container, query) = emby_stream_query_from_path(query, &container);
    let item_id = emby_internal_id(&item_id);
    let principal =
        match emby_stream_principal(&headers, &state, &query, &item_id, Some(&media_source_id))
            .await
        {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    serve_emby_media_file(
        &state,
        principal,
        &headers,
        &method,
        &item_id,
        Some(&media_source_id),
        &emby_direct_admission_session_id(&headers, &query, &item_id),
        Some(&container),
    )
    .await
}

pub(super) async fn emby_download(
    headers: HeaderMap,
    method: Method,
    Path(item_id): Path<String>,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let query = emby_stream_query_from_raw(raw_query);
    let item_id = emby_internal_id(&item_id);
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let principal = match emby_access_principal(&auth_principal, None) {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let source = match access
        .authorized_playback_source(principal, &item_id, query.media_source_id.as_deref())
        .await
    {
        Ok(Some(source)) => source,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if !auth_principal.can_download() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(downloads) = state.downloads.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let artifact = match downloads.prepare_authorized_source(&source).await {
        Ok(artifact) => artifact,
        Err(error) => return download_error_response(error),
    };
    let mut response = serve_download_artifact(downloads, &artifact, &method, &headers).await;
    add_download_header_with_name(&mut response, artifact.file_name());
    response
}

pub(super) fn add_download_header_with_name(response: &mut Response, file_name: &str) {
    if !response.status().is_success() {
        return;
    }
    let encoded = percent_encode_filename(file_name);
    let fallback = ascii_download_filename(file_name);
    let value = format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}");
    if let Ok(value) = HeaderValue::from_str(&value) {
        response.headers_mut().insert("Content-Disposition", value);
    }
}

pub(super) fn ascii_download_filename(value: &str) -> String {
    let fallback = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if fallback.is_empty() {
        "download".to_owned()
    } else {
        fallback
    }
}

pub(super) fn percent_encode_filename(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .flat_map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                vec![char::from(*byte)]
            } else {
                format!("%{byte:02X}").chars().collect()
            }
        })
        .collect()
}

struct MediaFileRequest<'a> {
    item_id: &'a str,
    media_source_id: Option<&'a str>,
    play_session_id: Option<&'a str>,
    pass_through_external_url: bool,
}

pub(super) async fn serve_media_file(
    state: &AppState,
    principal: AccessPrincipal,
    headers: &HeaderMap,
    method: &Method,
    item_id: &str,
    media_source_id: Option<&str>,
    _requested_container: Option<&str>,
) -> Response {
    serve_media_file_internal(
        state,
        principal,
        headers,
        method,
        MediaFileRequest {
            item_id,
            media_source_id,
            play_session_id: None,
            pass_through_external_url: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn serve_emby_media_file(
    state: &AppState,
    principal: AccessPrincipal,
    headers: &HeaderMap,
    method: &Method,
    item_id: &str,
    media_source_id: Option<&str>,
    play_session_id: &str,
    _requested_container: Option<&str>,
) -> Response {
    // Emby is also the handoff boundary for external proxies. Once Lux has
    // authenticated the request, let the player issue the original URL
    // request so its User-Agent, Range, cookies, and redirect state reach the
    // 302 service unchanged.
    serve_media_file_internal(
        state,
        principal,
        headers,
        method,
        MediaFileRequest {
            item_id,
            media_source_id,
            play_session_id: Some(play_session_id),
            pass_through_external_url: true,
        },
    )
    .await
}

async fn serve_media_file_internal(
    state: &AppState,
    principal: AccessPrincipal,
    headers: &HeaderMap,
    method: &Method,
    request: MediaFileRequest<'_>,
) -> Response {
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let source = match access
        .authorized_playback_source(principal, request.item_id, request.media_source_id)
        .await
    {
        Ok(Some(source)) => source,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let playback_probe_source =
        (source.source_kind == "STRM_URL").then_some((request.item_id, source.source_id.as_str()));
    if source.source_kind == "STRM_URL" {
        let Some(external_url) = source.external_url else {
            return StatusCode::NOT_FOUND.into_response();
        };
        match classify_strm_target(&external_url).kind {
            StrmTargetKind::Url => {
                if request.pass_through_external_url {
                    // Do not consume a load-balanced redirect with Lux's
                    // server-side Range: bytes=0-0 probe.
                    let response = redirect_emby_strm_playback(&external_url);
                    return admit_emby_media_response(
                        headers,
                        state,
                        principal,
                        method,
                        request.play_session_id,
                        response,
                        playback_probe_source,
                    )
                    .await;
                }
                let Some(resolver) = state.strm_playback.as_ref() else {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                };
                let user_agent = headers
                    .get("user-agent")
                    .and_then(|value| value.to_str().ok());
                let location = match resolver.resolve(&external_url, user_agent).await {
                    Ok(url) => url,
                    Err(StrmPlaybackError::ClientBuild(_)) => {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
                };
                let response = redirect_strm_playback(location.as_str());
                return admit_emby_media_response(
                    headers,
                    state,
                    principal,
                    method,
                    request.play_session_id,
                    response,
                    playback_probe_source,
                )
                .await;
            }
            StrmTargetKind::Path => {
                let path = match canonical_local_strm_target(
                    &source.root_path,
                    &source.relative_path,
                    &external_url,
                )
                .await
                {
                    Ok(path) => path,
                    Err(StrmLocalPathError::Missing) => {
                        return StatusCode::NOT_FOUND.into_response();
                    }
                    Err(StrmLocalPathError::Forbidden) => {
                        return StatusCode::FORBIDDEN.into_response();
                    }
                };
                if path
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"))
                {
                    return StatusCode::NOT_IMPLEMENTED.into_response();
                }
                let response = serve_media_path(headers, method, &path).await;
                return admit_emby_media_response(
                    headers,
                    state,
                    principal,
                    method,
                    request.play_session_id,
                    response,
                    playback_probe_source,
                )
                .await;
            }
            StrmTargetKind::Smb | StrmTargetKind::Ftp => {}
            StrmTargetKind::Empty | StrmTargetKind::Unsupported => {
                return StatusCode::NOT_IMPLEMENTED.into_response();
            }
        }
        let Some(plugins) = state.plugins.as_ref() else {
            return StatusCode::NOT_IMPLEMENTED.into_response();
        };
        let location = match plugins.resolve_strm_target(&external_url).await {
            Ok(Some(url)) => url,
            Ok(None) => return StatusCode::NOT_IMPLEMENTED.into_response(),
            Err(PluginServiceError::InvalidResponse) => {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        let response = redirect_strm_playback(&location);
        return admit_emby_media_response(
            headers,
            state,
            principal,
            method,
            request.play_session_id,
            response,
            playback_probe_source,
        )
        .await;
    }
    if source.source_kind != "LOCAL_FILE" {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    let path = match canonical_local_media_path(&source.root_path, &source.relative_path).await {
        Ok(path) => path,
        Err(LocalPathError::Missing) => return StatusCode::NOT_FOUND.into_response(),
        Err(LocalPathError::Forbidden) => return StatusCode::FORBIDDEN.into_response(),
    };
    let response = serve_media_path(headers, method, &path).await;
    admit_emby_media_response(
        headers,
        state,
        principal,
        method,
        request.play_session_id,
        response,
        playback_probe_source,
    )
    .await
}

async fn admit_emby_media_response(
    headers: &HeaderMap,
    state: &AppState,
    principal: AccessPrincipal,
    method: &Method,
    play_session_id: Option<&str>,
    response: Response,
    playback_probe_source: Option<(&str, &str)>,
) -> Response {
    if *method != Method::GET
        || !(response.status().is_success() || response.status().is_redirection())
    {
        return response;
    }
    if let Some(play_session_id) = play_session_id {
        let user_id = principal.user_id_string();
        // The shared server-admin key has no user identity, and playback admission is per-user
        // (concurrent stream limits and progress). It must not turn a trusted admin credential
        // into a 401 for read-only media requests.
        if !user_id.is_empty()
            && let Err(response) = super::playback::admit_emby_direct_playback(
                headers,
                state,
                &user_id,
                play_session_id,
            )
            .await
        {
            return response;
        }
    }
    if let Some((item_id, source_id)) = playback_probe_source
        && let Some(strm_probe) = state.strm_probe.as_ref()
    {
        strm_probe.schedule_playback_probe(item_id, source_id);
    }
    response
}

pub(super) fn redirect_strm_playback(location: &str) -> Response {
    redirect_strm_playback_with_status(location, StatusCode::TEMPORARY_REDIRECT)
}

fn redirect_emby_strm_playback(location: &str) -> Response {
    redirect_strm_playback_with_status(location, StatusCode::FOUND)
}

fn redirect_strm_playback_with_status(location: &str, status: StatusCode) -> Response {
    let Some(location) = normalize_strm_http_location(location) else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    match Response::builder()
        .status(status)
        .header("Location", location)
        .body(Body::empty())
    {
        Ok(response) => response,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub(super) fn download_error_response(error: DownloadError) -> Response {
    let status = match error {
        DownloadError::ItemNotFound => StatusCode::NOT_FOUND,
        DownloadError::PathOutsideRoot(_) => StatusCode::FORBIDDEN,
        DownloadError::InvalidFileName(_)
        | DownloadError::UnsupportedStrmTarget
        | DownloadError::RemoteUrl(
            crate::application::remote_url_policy::RemoteMediaUrlError::Invalid
            | crate::application::remote_url_policy::RemoteMediaUrlError::BlockedHost,
        ) => StatusCode::BAD_REQUEST,
        DownloadError::RemoteUrl(
            crate::application::remote_url_policy::RemoteMediaUrlError::ResolutionFailed,
        )
        | DownloadError::RemoteRequest => StatusCode::BAD_GATEWAY,
        DownloadError::Io(_)
        | DownloadError::Storage(_)
        | DownloadError::ProxyConfiguration(_)
        | DownloadError::ClientBuild(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    status.into_response()
}

pub(super) async fn serve_download_artifact(
    downloads: &DownloadService,
    artifact: &DownloadArtifact,
    method: &Method,
    headers: &HeaderMap,
) -> Response {
    if let Some(path) = artifact.local_path() {
        return serve_media_path(headers, method, path).await;
    }
    let range = headers.get("range").and_then(|value| value.to_str().ok());
    let upstream = match downloads.fetch_remote(artifact, method, range).await {
        Ok(response) => response,
        Err(error) => return download_error_response(error),
    };
    let status = match StatusCode::from_u16(upstream.status().as_u16()) {
        Ok(status) => status,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let upstream_headers = upstream.headers().clone();
    let is_success = status.is_success();
    let body = if is_success && method != Method::HEAD {
        Body::from_stream(upstream.bytes_stream())
    } else {
        Body::empty()
    };
    let mut response = Response::builder().status(status);
    for header_name in [
        "accept-ranges",
        "content-length",
        "content-range",
        "content-type",
        "etag",
        "last-modified",
    ] {
        if let Some(value) = upstream_headers.get(header_name) {
            response = response.header(header_name, value.clone());
        }
    }
    if is_success && upstream_headers.get("content-type").is_none() {
        response = response.header("content-type", "application/octet-stream");
    }
    match response.body(body) {
        Ok(response) => response,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub(super) async fn serve_media_path(
    headers: &HeaderMap,
    method: &Method,
    path: &FsPath,
) -> Response {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    let metadata = match fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let size = metadata.len();
    let modified = metadata.modified().ok();
    let etag = media_etag(size, modified);
    let last_modified = modified.and_then(|value| {
        value
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|_| httpdate::fmt_http_date(value))
    });
    let range = match parse_single_range(
        headers
            .get("range")
            .map(|value| value.to_str().unwrap_or("")),
        size,
    ) {
        Ok(range) => range,
        // An unparseable Range header must be ignored (RFC 9113 §14.2 sends the full body);
        // only a syntactically valid but unsatisfiable range earns a 416.
        Err(RangeError::Invalid) => ByteRange::Full,
        Err(RangeError::Unsatisfiable) => {
            let mut response = Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header("Accept-Ranges", "bytes")
                .header("Content-Range", format!("bytes */{size}"))
                .header("Content-Length", 0)
                .header("ETag", &etag)
                .header("Content-Type", media_content_type(extension.as_deref()));
            if let Some(last_modified) = &last_modified {
                response = response.header("Last-Modified", last_modified);
            }
            return response
                .body(Body::empty())
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    };
    let (status, start, length, content_range) = match range {
        ByteRange::Full => (StatusCode::OK, 0, size, None),
        ByteRange::Partial { start, end } => (
            StatusCode::PARTIAL_CONTENT,
            start,
            end - start + 1,
            Some(format!("bytes {start}-{end}/{size}")),
        ),
    };
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let Ok(mut file) = fs::File::open(&path).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if file.seek(SeekFrom::Start(start)).await.is_err() {
            return StatusCode::NOT_FOUND.into_response();
        }
        Body::from_stream(tokio_util::io::ReaderStream::new(file.take(length)))
    };
    let mut response = Response::builder()
        .status(status)
        .header("Accept-Ranges", "bytes")
        .header("Content-Length", length)
        .header("Content-Type", media_content_type(extension.as_deref()))
        .header("ETag", &etag);
    if let Some(content_range) = content_range {
        response = response.header("Content-Range", content_range);
    }
    if let Some(last_modified) = &last_modified {
        response = response.header("Last-Modified", last_modified);
    }
    response
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub(super) enum LocalPathError {
    Missing,
    Forbidden,
}

pub(super) async fn canonical_local_media_path(
    root_path: &str,
    relative_path: &str,
) -> Result<PathBuf, LocalPathError> {
    let relative = FsPath::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(LocalPathError::Forbidden);
    }
    let root = fs::canonicalize(root_path)
        .await
        .map_err(|_| LocalPathError::Missing)?;
    let path = fs::canonicalize(root.join(relative))
        .await
        .map_err(|_| LocalPathError::Missing)?;
    if !path.starts_with(&root) || path == root {
        return Err(LocalPathError::Forbidden);
    }
    Ok(path)
}

pub(super) fn media_etag(size: u64, modified: Option<std::time::SystemTime>) -> String {
    let modified = modified
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| format!("{}-{}", value.as_secs(), value.subsec_nanos()))
        .unwrap_or_else(|| "unknown".to_owned());
    format!("\"{size:x}-{modified}\"")
}

pub(super) fn media_content_type(extension: Option<&str>) -> &'static str {
    match extension {
        Some("mkv") => "video/x-matroska",
        Some("mp4" | "m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("avi") => "video/x-msvideo",
        Some("ts" | "m2ts") => "video/mp2t",
        Some("flv") => "video/x-flv",
        _ => "application/octet-stream",
    }
}

pub(super) async fn serve_subtitle(
    state: &AppState,
    principal: AccessPrincipal,
    method: &Method,
    item_id: &str,
    media_source_id: Option<&str>,
    stream_index: i64,
) -> Response {
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access.can_view_item(principal, item_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let subtitle = match database
        .find_external_subtitle(item_id, media_source_id, stream_index)
        .await
    {
        Ok(Some(subtitle)) => subtitle,
        Ok(None) => {
            let streams = match database
                .list_subtitle_streams(item_id, media_source_id, 0, 500)
                .await
            {
                Ok(streams) => streams,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            let Some(stream) = streams
                .into_iter()
                .find(|stream| stream.stream_index == stream_index)
            else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if stream.is_external {
                return StatusCode::NOT_FOUND.into_response();
            }
            let Some(service) = state.embedded_subtitle.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            if method == Method::HEAD {
                let Some(content_type) = embedded_subtitle_content_type(stream.codec.as_deref())
                else {
                    return StatusCode::NOT_FOUND.into_response();
                };
                return Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", content_type)
                    .body(Body::empty())
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
            let language = stream.language.clone();
            let result = match service.extract(&stream).await {
                Ok(result) => result,
                Err(error) => return embedded_subtitle_error(error),
            };
            let mut response = Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", result.content_type)
                .header("Content-Length", result.bytes.len());
            if let Some(language) = language {
                response = response.header("Content-Language", language);
            }
            return response
                .body(Body::from(result.bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let relative = std::path::Path::new(&subtitle.external_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let root = match tokio::fs::canonicalize(&subtitle.root_path).await {
        Ok(root) => root,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let path = root.join(relative);
    let path = match tokio::fs::canonicalize(&path).await {
        Ok(path) => path,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    if !path.starts_with(&root) || path == root {
        return StatusCode::FORBIDDEN.into_response();
    }
    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    if metadata.len() > 10 * 1024 * 1024 {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    let content_type = match extension.as_deref() {
        Some("vtt") => "text/vtt; charset=utf-8",
        Some("srt" | "ass" | "ssa" | "sub") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    };
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let Ok(file) = tokio::fs::File::open(&path).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        Body::from_stream(tokio_util::io::ReaderStream::new(file))
    };
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .header("Content-Length", metadata.len());
    if let Some(language) = subtitle.language {
        builder = builder.header("Content-Language", language);
    }
    builder
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn embedded_subtitle_content_type(codec: Option<&str>) -> Option<&'static str> {
    match codec.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("srt" | "subrip") => Some("text/plain; charset=utf-8"),
        Some("ass" | "ssa") => Some("text/x-ass; charset=utf-8"),
        _ => None,
    }
}

fn embedded_subtitle_error(
    error: crate::application::embedded_subtitle::EmbeddedSubtitleError,
) -> Response {
    use crate::application::embedded_subtitle::EmbeddedSubtitleError;

    let status = match error {
        EmbeddedSubtitleError::InvalidSource
        | EmbeddedSubtitleError::UnsupportedFormat
        | EmbeddedSubtitleError::Missing => StatusCode::NOT_FOUND,
        EmbeddedSubtitleError::Forbidden => StatusCode::FORBIDDEN,
        EmbeddedSubtitleError::Limit => StatusCode::PAYLOAD_TOO_LARGE,
        EmbeddedSubtitleError::Timeout
        | EmbeddedSubtitleError::Spawn
        | EmbeddedSubtitleError::Io
        | EmbeddedSubtitleError::ProcessFailed => StatusCode::SERVICE_UNAVAILABLE,
    };
    status.into_response()
}

pub(super) struct ServeImageTarget<'a> {
    pub item_id: &'a str,
    pub image_type: &'a str,
    pub image_index: i64,
}

pub(super) async fn serve_image(
    images: &ImageService,
    variants: Option<&Arc<crate::application::image_variants::ImageVariantService>>,
    principal: AccessPrincipal,
    headers: &HeaderMap,
    method: &Method,
    target: ServeImageTarget<'_>,
    width: Option<u32>,
) -> Response {
    let ServeImageTarget {
        item_id,
        image_type,
        image_index,
    } = target;
    let Some(image_type) = normalize_image_type(image_type) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let image = match images
        .resolve(principal, item_id, image_type, image_index)
        .await
    {
        Ok(Some(image)) => image,
        Ok(None) if image_type == "POSTER" => match images
            .resolve(principal, item_id, "THUMB", image_index)
            .await
        {
            Ok(Some(image)) => image,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(ImageError::Forbidden | ImageError::TooLarge { .. }) => {
                return StatusCode::FORBIDDEN.into_response();
            }
            Err(ImageError::Io { .. }) => return StatusCode::NOT_FOUND.into_response(),
            Err(ImageError::Storage(_)) => {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        },
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(ImageError::Forbidden | ImageError::TooLarge { .. }) => {
            return StatusCode::FORBIDDEN.into_response();
        }
        Err(ImageError::Io { .. }) => return StatusCode::NOT_FOUND.into_response(),
        Err(ImageError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if let (Some(width), Some(variants)) = (width, variants)
        && let Some(variant) = variants.resolve_variant(item_id, &image, width).await
        && let Some(response) = serve_variant_file(&variant, headers, method).await
    {
        return response;
    }
    serve_image_file(
        &image.path,
        image.content_type,
        image.content_length,
        &image.etag,
        headers,
        method,
    )
    .await
}

pub(super) async fn serve_tagged_image(
    images: &ImageService,
    headers: &HeaderMap,
    method: &Method,
    item_id: &str,
    image_type: &str,
    image_index: i64,
    tag: Option<&str>,
) -> Response {
    let Some(tag) = tag.filter(|tag| !tag.is_empty()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(image_type) = normalize_image_type(image_type) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let image = match images
        .resolve_tagged(item_id, image_type, image_index, tag)
        .await
    {
        Ok(Some(image)) => image,
        Ok(None) if image_type == "POSTER" => match images
            .resolve_tagged(item_id, "THUMB", image_index, tag)
            .await
        {
            Ok(Some(image)) => image,
            Ok(None) => return StatusCode::UNAUTHORIZED.into_response(),
            Err(ImageError::Forbidden | ImageError::TooLarge { .. }) => {
                return StatusCode::FORBIDDEN.into_response();
            }
            Err(ImageError::Io { .. }) => return StatusCode::NOT_FOUND.into_response(),
            Err(ImageError::Storage(_)) => {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        },
        Ok(None) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(ImageError::Forbidden | ImageError::TooLarge { .. }) => {
            return StatusCode::FORBIDDEN.into_response();
        }
        Err(ImageError::Io { .. }) => return StatusCode::NOT_FOUND.into_response(),
        Err(ImageError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    serve_image_file(
        &image.path,
        image.content_type,
        image.content_length,
        &image.etag,
        headers,
        method,
    )
    .await
}

pub(super) async fn serve_filmly_compat_image(
    images: &ImageService,
    headers: &HeaderMap,
    method: &Method,
    item_id: &str,
    image_type: &str,
    image_index: i64,
) -> Response {
    let Some(image_type) = normalize_image_type(image_type) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let image = match images
        .resolve_filmly_compat(item_id, image_type, image_index)
        .await
    {
        Ok(Some(image)) => image,
        Ok(None) if image_type == "POSTER" => match images
            .resolve_filmly_compat(item_id, "THUMB", image_index)
            .await
        {
            Ok(Some(image)) => image,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(ImageError::Forbidden | ImageError::TooLarge { .. }) => {
                return StatusCode::FORBIDDEN.into_response();
            }
            Err(ImageError::Io { .. }) => return StatusCode::NOT_FOUND.into_response(),
            Err(ImageError::Storage(_)) => {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        },
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(ImageError::Forbidden | ImageError::TooLarge { .. }) => {
            return StatusCode::FORBIDDEN.into_response();
        }
        Err(ImageError::Io { .. }) => return StatusCode::NOT_FOUND.into_response(),
        Err(ImageError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    serve_image_file(
        &image.path,
        image.content_type,
        image.content_length,
        &image.etag,
        headers,
        method,
    )
    .await
}

pub(super) async fn serve_emby_library_cover(
    state: &AppState,
    principal: Option<AccessPrincipal>,
    capability_tag: Option<&str>,
    headers: &HeaderMap,
    method: &Method,
    library_id: &str,
    image_index: i64,
) -> Option<Response> {
    let library_id = emby_internal_id(library_id)
        .parse::<crate::domain::ids::LibraryId>()
        .ok()?;
    let covers = state.library_covers.as_ref()?;
    let cover = match covers.resolve(library_id).await {
        Ok(Some(cover)) => cover,
        Ok(None) => return None,
        Err(LibraryCoverError::Storage(_)) => {
            return Some(StatusCode::SERVICE_UNAVAILABLE.into_response());
        }
        Err(_) => return Some(StatusCode::NOT_FOUND.into_response()),
    };
    if image_index != 0 {
        return Some(StatusCode::NOT_FOUND.into_response());
    }
    if let Some(capability_tag) = capability_tag {
        if cover.etag.trim_matches('"') != capability_tag {
            return None;
        }
    } else {
        let principal = principal?;
        let Some(access) = state.access.as_ref() else {
            return Some(StatusCode::SERVICE_UNAVAILABLE.into_response());
        };
        match access
            .can_view_library(principal, &library_id.to_string())
            .await
        {
            Ok(true) => {}
            Ok(false) => return Some(StatusCode::NOT_FOUND.into_response()),
            Err(_) => return Some(StatusCode::SERVICE_UNAVAILABLE.into_response()),
        }
    }
    Some(
        serve_image_file(
            &cover.path,
            &cover.content_type,
            cover.content_length,
            &cover.etag,
            headers,
            method,
        )
        .await,
    )
}

/// ETag comparison per RFC 9110 §13.1.2: the `If-None-Match` list is weak-comparison based, so
/// `W/"x"` matches `"x"`, and `*` matches any current representation.
pub(super) fn if_none_match_matches(headers: &HeaderMap, etag: &str) -> bool {
    let Some(value) = headers
        .get("if-none-match")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let expected = strip_weak_prefix(etag.trim());
    value
        .split(',')
        .map(str::trim)
        .any(|candidate| candidate == "*" || strip_weak_prefix(candidate) == expected)
}

fn strip_weak_prefix(tag: &str) -> &str {
    tag.strip_prefix("W/").unwrap_or(tag)
}

pub(super) async fn serve_image_file(
    path: &FsPath,
    content_type: &str,
    content_length: u64,
    etag: &str,
    headers: &HeaderMap,
    method: &Method,
) -> Response {
    if if_none_match_matches(headers, etag) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header("ETag", etag)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let Ok(file) = tokio::fs::File::open(path).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        Body::from_stream(tokio_util::io::ReaderStream::new(file))
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", content_type)
        .header("Content-Length", content_length)
        .header("ETag", etag)
        .header("Cache-Control", "private, max-age=3600")
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn serve_variant_file(
    variant: &crate::application::image_variants::VariantFile,
    headers: &HeaderMap,
    method: &Method,
) -> Option<Response> {
    if if_none_match_matches(headers, &variant.etag) {
        return Some(
            Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .header("ETag", &variant.etag)
                .header("Cache-Control", "private, max-age=31536000, immutable")
                .body(Body::empty())
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        );
    }
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        // A concurrent startup scan or LRU eviction can delete the cache file
        // between resolution and opening it; fall back to the original image.
        let Ok(file) = tokio::fs::File::open(&variant.path).await else {
            return None;
        };
        Body::from_stream(tokio_util::io::ReaderStream::new(file))
    };
    Some(
        Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "image/jpeg")
            .header("Content-Length", variant.content_length)
            .header("ETag", &variant.etag)
            .header("Cache-Control", "private, max-age=31536000, immutable")
            .body(body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    )
}

pub(super) fn lux_page_params(query: &LuxPageQuery) -> Result<(i64, i64), &'static str> {
    page_params(query.page, query.page_size)
}

pub(super) fn metadata_page_params(
    query: &MetadataCandidateQuery,
) -> Result<(i64, i64), &'static str> {
    page_params(query.page, query.page_size)
}

pub(super) fn page_params(
    page: Option<i64>,
    page_size: Option<i64>,
) -> Result<(i64, i64), &'static str> {
    let page = page.unwrap_or(1);
    let page_size = page_size.unwrap_or(50);
    if page < 1 || !(1..=100).contains(&page_size) {
        return Err("分页参数无效");
    }
    let offset = (page - 1)
        .checked_mul(page_size)
        .ok_or("分页参数超出范围")?;
    Ok((offset, page_size))
}

pub(super) async fn lux_catalog_items_json_for_user(
    database: &Database,
    user_id: &str,
    items: &[CatalogItem],
) -> Result<Vec<Value>, StorageError> {
    lux_catalog_items_json_for_user_with_projection(
        database,
        user_id,
        items,
        crate::application::catalog::CatalogItemProjection::Full,
    )
    .await
}

pub(super) async fn lux_catalog_items_json_for_user_with_projection(
    database: &Database,
    user_id: &str,
    items: &[CatalogItem],
    projection: crate::application::catalog::CatalogItemProjection,
) -> Result<Vec<Value>, StorageError> {
    let values =
        lux_catalog_item_values_by_id_with_projection(database, user_id, items, projection).await?;
    Ok(lux_catalog_items_from_values(items, &values))
}

pub(super) async fn lux_catalog_item_values_by_id(
    database: &Database,
    user_id: &str,
    items: &[CatalogItem],
) -> Result<HashMap<String, Value>, StorageError> {
    lux_catalog_item_values_by_id_with_projection(
        database,
        user_id,
        items,
        crate::application::catalog::CatalogItemProjection::Full,
    )
    .await
}

pub(super) async fn lux_catalog_item_values_by_id_with_projection(
    database: &Database,
    user_id: &str,
    items: &[CatalogItem],
    projection: crate::application::catalog::CatalogItemProjection,
) -> Result<HashMap<String, Value>, StorageError> {
    let mut item_ids = Vec::with_capacity(items.len());
    let mut seen = HashSet::with_capacity(items.len());
    for item in items {
        if seen.insert(item.id.clone()) {
            item_ids.push(item.id.clone());
        }
    }
    let (states, pending_item_ids, local_metadata_pending_item_ids) = tokio::try_join!(
        database.list_user_item_states(user_id, &item_ids),
        database.list_pending_metadata_item_ids(&item_ids),
        database.list_pending_local_metadata_item_ids(&item_ids),
    )?;
    Ok(items
        .iter()
        .map(|item| {
            let mut value = match projection {
                crate::application::catalog::CatalogItemProjection::Full => {
                    lux_catalog_item_json_with_user_state(item, states.get(&item.id))
                }
                crate::application::catalog::CatalogItemProjection::Card => {
                    lux_catalog_card_json_with_user_state(item, states.get(&item.id))
                }
            };
            if let Value::Object(object) = &mut value {
                object.insert(
                    "metadataPending".to_owned(),
                    Value::Bool(pending_item_ids.contains(&item.id)),
                );
                object.insert(
                    "localMetadataPending".to_owned(),
                    Value::Bool(local_metadata_pending_item_ids.contains(&item.id)),
                );
            }
            (item.id.clone(), value)
        })
        .collect())
}

pub(super) fn lux_catalog_items_from_values(
    items: &[CatalogItem],
    values: &HashMap<String, Value>,
) -> Vec<Value> {
    items
        .iter()
        .filter_map(|item| values.get(&item.id).cloned())
        .collect()
}

pub(super) async fn lux_catalog_page_json_for_user(
    database: &Database,
    user_id: &str,
    page: &CatalogPage,
) -> Result<Value, StorageError> {
    lux_catalog_page_json_for_user_with_projection(
        database,
        user_id,
        page,
        crate::application::catalog::CatalogItemProjection::Full,
    )
    .await
}

pub(super) async fn lux_catalog_page_json_for_user_with_projection(
    database: &Database,
    user_id: &str,
    page: &CatalogPage,
    projection: crate::application::catalog::CatalogItemProjection,
) -> Result<Value, StorageError> {
    let items =
        lux_catalog_items_json_for_user_with_projection(database, user_id, &page.items, projection)
            .await?;
    let mut body = json!({
        "items": items,
        "page": page.offset / page.limit + 1,
        "pageSize": page.limit,
        "hasNext": page.has_next,
    });
    if page.total_included {
        body["total"] = json!(page.total);
    }
    Ok(body)
}

pub(super) fn lux_catalog_item_json(item: &CatalogItem) -> Value {
    json!({
        "id": item.id,
        "libraryId": item.library_id,
        "itemType": item.item_type,
        "title": item.title,
        "sortTitle": item.sort_title,
        "originalTitle": item.original_title,
        "overview": item.overview,
        "premiereDate": item.premiere_date,
        "lastAirDate": item.last_air_date,
        "status": item.status,
        "originalLanguage": item.original_language,
        "providerIds": item.provider_ids,
        "parentId": item.parent_id,
        "seriesId": item.series_id,
        "seriesName": item.series_name,
        "parentIndexNumber": item.season_number,
        "indexNumber": item.episode_number,
        "seasonCount": item.season_count,
        "episodeCount": item.episode_count,
        "productionYear": item.production_year,
        "rating": item.rating,
        "ratingSource": item.rating_source,
        "runtimeTicks": item.runtime_ticks,
        "imageTags": {
            "poster": item.poster_image_tag,
            "fanart": item.fanart_image_tag,
            "thumb": item.thumb_image_tag,
            "logo": item.logo_image_tag,
        },
        "mediaSources": item.media_sources.iter().map(lux_catalog_source_json).collect::<Vec<_>>(),
    })
}

fn add_global_collection_card_fields(
    value: &mut Value,
    member_count: i64,
    poster_item_id: Option<&str>,
) {
    if let Value::Object(object) = value {
        object.insert("memberCount".to_owned(), json!(member_count));
        object.insert("posterItemId".to_owned(), json!(poster_item_id));
    }
}

fn lux_global_collection_source_copy_json(
    source: &crate::application::catalog::GlobalCollectionSourceCopy,
) -> Value {
    json!({
        "itemId": source.item_id,
        "libraryId": source.library_id,
        "libraryName": source.library_name,
        "title": source.title,
        "posterImageTag": source.poster_image_tag,
        "versions": source.versions.iter().map(|version| json!({
            "id": version.id,
            "editionName": version.edition_name,
            "qualityLabel": version.quality_label,
            "container": version.container,
        })).collect::<Vec<_>>(),
    })
}

pub(super) fn lux_catalog_item_json_with_user_state(
    item: &CatalogItem,
    user_state: Option<&crate::storage::StoredUserItemState>,
) -> Value {
    let mut value = lux_catalog_item_json(item);
    if let Value::Object(object) = &mut value {
        object.insert("userData".to_owned(), lux_user_data_json(user_state));
    }
    value
}

pub(super) fn lux_catalog_card_json(item: &CatalogItem) -> Value {
    let default_duration_ticks = item
        .media_sources
        .iter()
        .find(|source| source.is_default)
        .or_else(|| item.media_sources.first())
        .and_then(|source| source.duration_ticks);
    let mut object = serde_json::Map::new();
    object.insert("id".to_owned(), json!(item.id));
    object.insert("libraryId".to_owned(), json!(item.library_id));
    object.insert("itemType".to_owned(), json!(item.item_type));
    object.insert("title".to_owned(), json!(item.title));
    object.insert("addedAt".to_owned(), json!(item.added_at));
    for (key, value) in [
        ("productionYear", item.production_year.map(Value::from)),
        ("rating", item.rating.map(Value::from)),
        ("runtimeTicks", item.runtime_ticks.map(Value::from)),
        ("episodeCount", item.episode_count.map(Value::from)),
        ("seriesId", item.series_id.as_deref().map(Value::from)),
        ("parentId", item.parent_id.as_deref().map(Value::from)),
        (
            "defaultDurationTicks",
            default_duration_ticks.map(Value::from),
        ),
    ] {
        if let Some(value) = value {
            object.insert(key.to_owned(), value);
        }
    }
    let mut image_tags = serde_json::Map::new();
    for (key, value) in [
        ("poster", item.poster_image_tag.as_ref()),
        ("fanart", item.fanart_image_tag.as_ref()),
        ("thumb", item.thumb_image_tag.as_ref()),
        ("logo", item.logo_image_tag.as_ref()),
    ] {
        if let Some(value) = value {
            image_tags.insert(key.to_owned(), json!(value));
        }
    }
    if !image_tags.is_empty() {
        object.insert("imageTags".to_owned(), Value::Object(image_tags));
    }
    Value::Object(object)
}

pub(super) fn lux_catalog_card_json_with_user_state(
    item: &CatalogItem,
    user_state: Option<&crate::storage::StoredUserItemState>,
) -> Value {
    let mut value = lux_catalog_card_json(item);
    if let Value::Object(object) = &mut value {
        object.insert(
            "userData".to_owned(),
            lux_catalog_card_user_data_json(user_state),
        );
    }
    value
}

fn lux_catalog_card_user_data_json(state: Option<&crate::storage::StoredUserItemState>) -> Value {
    let mut object = serde_json::Map::new();
    if let Some(state) = state {
        if state.position_ticks != 0 {
            object.insert("positionTicks".to_owned(), json!(state.position_ticks));
        }
        if state.play_count != 0 {
            object.insert("playCount".to_owned(), json!(state.play_count));
        }
        if state.is_favorite {
            object.insert("isFavorite".to_owned(), json!(true));
        }
        if state.is_played {
            object.insert("isPlayed".to_owned(), json!(true));
        }
        if let Some(last_played_at) = state.last_played_at {
            object.insert("lastPlayedAt".to_owned(), json!(last_played_at));
        }
    }
    Value::Object(object)
}

pub(super) fn apply_local_nfo_details(
    object: &mut serde_json::Map<String, Value>,
    nfo: &LocalNfoDetails,
) {
    if let Some(rating) = nfo.rating {
        object.insert("rating".to_owned(), json!(rating));
        object.insert("ratingSource".to_owned(), json!("NFO"));
    }
    if let Some(premiered) = nfo
        .premiered
        .as_deref()
        .or(nfo.release_date.as_deref())
        .or(nfo.aired.as_deref())
    {
        object.insert("premiereDate".to_owned(), json!(premiered));
    }
    if let Some(status) = nfo.status.as_deref() {
        object.insert("status".to_owned(), json!(status));
    }
    if let Some(language) = nfo.original_language.as_deref() {
        object.insert("originalLanguage".to_owned(), json!(language));
    }
    if let Some(last_air_date) = nfo.last_air_date.as_deref() {
        object.insert("lastAirDate".to_owned(), json!(last_air_date));
    }
    if let Some(runtime) = nfo.runtime {
        if let Some(runtime_ticks) = i64::from(runtime)
            .checked_mul(60)
            .and_then(|value| value.checked_mul(10_000_000))
        {
            object.insert("runtimeTicks".to_owned(), json!(runtime_ticks));
        }
    }
    if !nfo.provider_ids.is_empty() {
        let mut provider_ids = object
            .get("providerIds")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (provider, id) in &nfo.provider_ids {
            provider_ids.insert(provider.clone(), json!(id));
        }
        object.insert("providerIds".to_owned(), Value::Object(provider_ids));
    }
}

pub(super) fn lux_user_data_json(state: Option<&crate::storage::StoredUserItemState>) -> Value {
    json!({
        "positionTicks": state.map(|value| value.position_ticks).unwrap_or_default(),
        "playCount": state.map(|value| value.play_count).unwrap_or_default(),
        "isFavorite": state.map(|value| value.is_favorite).unwrap_or(false),
        "isPlayed": state.map(|value| value.is_played).unwrap_or(false),
        "lastPlayedAt": state.and_then(|value| value.last_played_at),
    })
}

pub(super) const MAX_LUX_CHAPTERS_PER_SOURCE: usize = 1_000;

pub(super) fn lux_catalog_source_json(
    source: &crate::application::catalog::CatalogSource,
) -> Value {
    let mut chapters = source
        .chapters
        .iter()
        .filter(|chapter| chapter.start_position_ticks >= 0 && chapter.chapter_index >= 0)
        .collect::<Vec<_>>();
    chapters.sort_by(|left, right| {
        left.start_position_ticks
            .cmp(&right.start_position_ticks)
            .then_with(|| {
                lux_chapter_marker_rank(&left.marker_type)
                    .cmp(&lux_chapter_marker_rank(&right.marker_type))
            })
            .then(left.chapter_index.cmp(&right.chapter_index))
    });
    json!({
        "id": source.id,
        "sourceKind": source.source_kind,
        "container": source.container,
        "size": source.size,
        "bitrate": source.bitrate,
        "durationTicks": source.duration_ticks,
        "externalUrl": source.external_url,
        "editionName": source.edition_name,
        "qualityLabel": source.quality_label,
        "isDefault": source.is_default,
        "versionKey": source.version_key(),
        "partIndex": source.part_index(),
        "probeStatus": source.probe_status,
        "streams": source.streams.iter().map(|stream| json!({
            "index": stream.index,
            "type": stream.stream_type,
            "codec": stream.codec,
            "language": stream.language,
            "title": stream.title,
            "isExternal": stream.is_external,
            "isDefault": stream.is_default,
            "isForced": stream.is_forced,
            "details": &stream.details,
        })).collect::<Vec<_>>(),
        "chapters": chapters
            .into_iter()
            .take(MAX_LUX_CHAPTERS_PER_SOURCE)
            .map(lux_catalog_chapter_json)
            .collect::<Vec<_>>(),
    })
}

pub(super) fn lux_chapter_marker_rank(marker_type: &str) -> u8 {
    match marker_type {
        "INTRO_START" => 0,
        "INTRO_END" => 1,
        "CREDITS_START" => 2,
        _ => 99,
    }
}

pub(super) fn lux_catalog_chapter_json(
    chapter: &crate::application::catalog::CatalogChapter,
) -> Value {
    json!({
        "startPositionTicks": chapter.start_position_ticks,
        "name": chapter.name,
        "markerType": chapter.marker_type,
        "chapterIndex": chapter.chapter_index,
    })
}

#[cfg(test)]
mod direct_ticket_tests {
    use super::*;
    use crate::{
        api::AppState,
        application::setup::SetupService,
        auth::{emby::EmbyAuthService, sessions::WebAuthService},
        config::Config,
        storage::Database,
    };

    #[tokio::test]
    async fn legacy_direct_ticket_initializes_empty_context_then_fails_closed_on_client_rule()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        let user = setup.complete("Admin", "Admin", "correct password").await?;
        let state = AppState::ready(
            config,
            database.clone(),
            setup,
            WebAuthService::new(database.clone())?,
            EmbyAuthService::new(database.clone())?,
        );
        let service = state
            .web_playback
            .as_ref()
            .ok_or("missing Web playback service")?;
        let user_id = user.id.to_string();
        let item_id = uuid::Uuid::now_v7().to_string();
        let source_id = uuid::Uuid::now_v7().to_string();
        let play_session_id = "legacy-direct-ticket-session";
        let expires_at = crate::application::playback::session::unix_timestamp() + 60;
        let signature = service
            .sign_emby_direct_stream_legacy(
                &user_id,
                user.is_admin,
                &item_id,
                &source_id,
                play_session_id,
                expires_at,
            )
            .ok_or("could not sign legacy direct ticket")?;
        let query = emby_stream_query_from_raw(RawQuery(Some(format!(
            "luxPlaybackUserId={user_id}&luxPlaybackAdmin=1&PlaySessionId={play_session_id}&luxPlaybackExpires={expires_at}&luxPlaybackSignature={signature}"
        ))));

        assert!(query.playback_ticket_version.is_none());
        assert!(
            emby_stream_principal(
                &HeaderMap::new(),
                &state,
                &query,
                &item_id,
                Some(&source_id),
            )
            .await
            .is_ok()
        );
        let stored_ticket = database
            .find_emby_direct_playback_ticket(&user_id, play_session_id)
            .await?
            .ok_or("legacy direct ticket context was not initialized")?;
        assert_eq!(stored_ticket.authorization_device_id, None);
        assert_eq!(stored_ticket.authorization_client_name, None);

        sqlx::query(
            "INSERT INTO user_access_blacklist_rules (
                 id, user_id, rule_type, match_value, is_enabled, created_by
             ) VALUES ('legacy-direct-ticket-client-rule', NULL, 'CLIENT', 'playbacktest', 1, ?)",
        )
        .bind(&user_id)
        .execute(database.pool())
        .await?;
        let blocked = emby_stream_principal(
            &HeaderMap::new(),
            &state,
            &query,
            &item_id,
            Some(&source_id),
        )
        .await;
        assert!(matches!(blocked, Err(StatusCode::GONE)));
        let stored_ticket = database
            .find_emby_direct_playback_ticket(&user_id, play_session_id)
            .await?
            .ok_or("legacy direct ticket row disappeared")?;
        assert!(stored_ticket.revoked_at.is_some());

        state.shutdown_background_workers().await;
        Ok(())
    }

    #[tokio::test]
    async fn direct_ticket_with_ip_rule_fails_closed_without_peer_address()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        let user = setup.complete("Admin", "Admin", "correct password").await?;
        let state = AppState::ready(
            config,
            database.clone(),
            setup,
            WebAuthService::new(database.clone())?,
            EmbyAuthService::new(database.clone())?,
        );
        let service = state
            .web_playback
            .as_ref()
            .ok_or("missing Web playback service")?;
        let user_id = user.id.to_string();
        let item_id = uuid::Uuid::now_v7().to_string();
        let source_id = uuid::Uuid::now_v7().to_string();
        let play_session_id = "direct-ticket-missing-peer-session";
        let expires_at = crate::application::playback::session::unix_timestamp() + 60;
        let signature = service
            .sign_emby_direct_stream_v2(
                &user_id,
                user.is_admin,
                &item_id,
                &source_id,
                play_session_id,
                expires_at,
            )
            .ok_or("could not sign v2 direct ticket")?;
        service
            .register_emby_direct_playback_ticket(
                &user_id,
                play_session_id,
                Some("device-id"),
                Some("PlaybackTest"),
                expires_at,
                crate::application::playback::session::unix_timestamp(),
            )
            .await?;
        sqlx::query(
            "INSERT INTO user_access_blacklist_rules (
                 id, user_id, rule_type, match_value, is_enabled, created_by
             ) VALUES ('direct-ticket-missing-peer-rule', NULL, 'IP_CIDR', '192.0.2.0/24', 1, ?)",
        )
        .bind(&user_id)
        .execute(database.pool())
        .await?;
        let query = emby_stream_query_from_raw(RawQuery(Some(format!(
            "luxPlaybackUserId={user_id}&luxPlaybackAdmin=1&PlaySessionId={play_session_id}&luxPlaybackTicketVersion=2&luxPlaybackExpires={expires_at}&luxPlaybackSignature={signature}"
        ))));

        let denied = emby_stream_principal(
            &HeaderMap::new(),
            &state,
            &query,
            &item_id,
            Some(&source_id),
        )
        .await;
        assert!(matches!(denied, Err(StatusCode::SERVICE_UNAVAILABLE)));
        let stored_ticket = database
            .find_emby_direct_playback_ticket(&user_id, play_session_id)
            .await?
            .ok_or("direct ticket context was not persisted")?;
        assert!(stored_ticket.revoked_at.is_some());
        sqlx::query("DELETE FROM user_access_blacklist_rules WHERE id = ?")
            .bind("direct-ticket-missing-peer-rule")
            .execute(database.pool())
            .await?;

        let invalid_rule_session_id = "direct-ticket-invalid-ip-rule-session";
        let invalid_rule_expires_at = crate::application::playback::session::unix_timestamp() + 60;
        service
            .register_emby_direct_playback_ticket(
                &user_id,
                invalid_rule_session_id,
                Some("device-id"),
                Some("PlaybackTest"),
                invalid_rule_expires_at,
                crate::application::playback::session::unix_timestamp(),
            )
            .await?;
        sqlx::query(
            "INSERT INTO user_access_blacklist_rules (
                 id, user_id, rule_type, match_value, is_enabled, created_by
             ) VALUES ('direct-ticket-invalid-ip-rule', NULL, 'IP_CIDR', 'not-a-cidr', 1, ?)",
        )
        .bind(&user_id)
        .execute(database.pool())
        .await?;
        let invalid_rule_result = service
            .authorize_emby_direct_playback_ticket_with_ip(
                &user_id,
                invalid_rule_session_id,
                invalid_rule_expires_at,
                false,
                Some("192.0.2.1".parse()?),
            )
            .await;
        assert!(matches!(
            invalid_rule_result,
            Err(crate::application::playback::session::WebPlaybackSessionError::AccessRulesUnavailable)
        ));
        let invalid_rule_ticket = database
            .find_emby_direct_playback_ticket(&user_id, invalid_rule_session_id)
            .await?
            .ok_or("invalid-rule ticket context was not persisted")?;
        assert!(invalid_rule_ticket.revoked_at.is_some());

        state.shutdown_background_workers().await;
        Ok(())
    }
}

#[cfg(test)]
mod etag_tests {
    use super::if_none_match_matches;
    use axum::http::{HeaderMap, HeaderValue};

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("if-none-match", HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn weak_and_strong_validators_compare_equal() {
        assert!(if_none_match_matches(&headers(r#"W/"abc""#), r#""abc""#));
        assert!(if_none_match_matches(&headers(r#""abc""#), r#"W/"abc""#));
        assert!(if_none_match_matches(
            &headers(r#""other", W/"abc""#),
            r#""abc""#
        ));
        assert!(if_none_match_matches(&headers("*"), r#""abc""#));
        assert!(!if_none_match_matches(&headers(r#""other""#), r#""abc""#));
        assert!(!if_none_match_matches(&HeaderMap::new(), r#""abc""#));
    }
}
