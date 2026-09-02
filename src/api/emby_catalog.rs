use super::*;
use tokio::{sync::Semaphore, task::JoinSet};

const EMBY_ITEM_EXTRA_CONCURRENCY: usize = 8;

use crate::application::catalog::CatalogItemCounts;

#[derive(Deserialize, Default)]
pub(super) struct EmbyItemsQuery {
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
    #[serde(rename = "SeriesId", alias = "seriesId", default)]
    pub(super) series_id: Option<String>,
    #[serde(rename = "ParentId", default)]
    pub(super) parent_id: Option<String>,
    #[serde(rename = "Ids", default)]
    pub(super) ids: Option<String>,
    #[serde(rename = "IncludeItemTypes", default)]
    pub(super) include_item_types: Option<String>,
    #[serde(rename = "ExcludeItemTypes", default)]
    pub(super) exclude_item_types: Option<String>,
    #[serde(rename = "SeasonId", default)]
    pub(super) season_id: Option<String>,
    #[serde(rename = "SearchTerm", alias = "searchTerm", default)]
    pub(super) search_term: Option<String>,
    #[serde(rename = "StartIndex", default)]
    pub(super) start_index: Option<i64>,
    #[serde(rename = "Limit", default)]
    pub(super) limit: Option<i64>,
    #[serde(
        rename = "IsPlayed",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    pub(super) is_played: Option<bool>,
    #[serde(
        rename = "IsFavorite",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    pub(super) is_favorite: Option<bool>,
    #[serde(rename = "Years", default)]
    pub(super) years: Option<String>,
    #[serde(rename = "SortBy", default)]
    pub(super) sort_by: Option<String>,
    #[serde(rename = "SortOrder", default)]
    pub(super) sort_order: Option<String>,
    #[serde(rename = "Fields", default)]
    pub(super) fields: Option<String>,
    #[serde(
        rename = "GroupItems",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    pub(super) group_items: Option<bool>,
    #[serde(
        rename = "EnableTotalRecordCount",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    pub(super) enable_total_record_count: Option<bool>,
    #[serde(
        rename = "Recursive",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    pub(super) recursive: Option<bool>,
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyItemCountsQuery {
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
    #[serde(
        rename = "IsFavorite",
        alias = "isFavorite",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    pub(super) is_favorite: Option<bool>,
}

pub(super) fn deserialize_optional_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    match value.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => value.parse().map(Some).map_err(serde::de::Error::custom),
    }
}

pub(super) fn emby_fields_include(fields: Option<&str>, field: &str) -> bool {
    fields.is_none_or(|fields| {
        fields
            .split(',')
            .map(str::trim)
            .any(|value| value.eq_ignore_ascii_case(field))
    })
}

/// Filmly sends `ShareLevel` as a capability hint on item detail requests. It
/// is not a field selector, so discard it before applying the Emby field
/// projection; if it is the only value, keep the normal full-detail response.
pub(super) fn emby_detail_fields(fields: Option<&str>) -> Option<String> {
    let fields = fields?;
    let filtered = fields
        .split(',')
        .map(str::trim)
        .filter(|field| !field.is_empty() && !field.eq_ignore_ascii_case("ShareLevel"))
        .collect::<Vec<_>>();
    (!filtered.is_empty()).then(|| filtered.join(","))
}

pub(super) fn normalize_emby_item_type(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "movie" => Some("MOVIE".to_owned()),
        "series" | "show" => Some("SERIES".to_owned()),
        "season" => Some("SEASON".to_owned()),
        "episode" => Some("EPISODE".to_owned()),
        "boxset" | "box_set" => Some("BOX_SET".to_owned()),
        "folder" => Some("FOLDER".to_owned()),
        _ => None,
    }
}

pub(super) fn catalog_filter_from_values(
    item_types: Option<&str>,
    years: Option<&str>,
    is_played: Option<bool>,
    is_favorite: Option<bool>,
    sort_by: Option<&str>,
    sort_order: Option<&str>,
    metadata_pending: bool,
) -> CatalogFilter {
    let item_types = item_types
        .map(|values| {
            let raw_values = values
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            let normalized = raw_values
                .iter()
                .filter_map(|value| normalize_emby_item_type(value))
                .collect::<Vec<_>>();
            if raw_values.is_empty() || !normalized.is_empty() {
                normalized
            } else {
                vec!["__NO_MATCH__".to_owned()]
            }
        })
        .unwrap_or_default();
    let years = years
        .map(|values| {
            values
                .split(',')
                .filter_map(|value| value.trim().parse::<i64>().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    CatalogFilter {
        item_types,
        excluded_item_types: Vec::new(),
        item_ids: None,
        person_id: None,
        media_source_ids: None,
        years,
        is_played,
        is_favorite,
        metadata_pending,
        sort_by: match sort_by {
            Some(value)
                if value
                    .split(',')
                    .any(|field| field.trim().eq_ignore_ascii_case("DateCreated")) =>
            {
                CatalogSort::DateCreated
            }
            Some(value)
                if value
                    .split(',')
                    .any(|field| field.trim().eq_ignore_ascii_case("PremiereDate")) =>
            {
                CatalogSort::PremiereDate
            }
            Some(value)
                if value.split(',').any(|field| {
                    field.trim().eq_ignore_ascii_case("CommunityRating")
                        || field.trim().eq_ignore_ascii_case("Rating")
                }) =>
            {
                CatalogSort::Rating
            }
            _ => CatalogSort::Name,
        },
        descending: sort_order.is_some_and(|value| value.eq_ignore_ascii_case("Descending")),
    }
}

pub(super) fn catalog_filter_from_emby(query: &EmbyItemsQuery) -> CatalogFilter {
    let mut filter = catalog_filter_from_values(
        query.include_item_types.as_deref(),
        query.years.as_deref(),
        query.is_played,
        query.is_favorite,
        query.sort_by.as_deref(),
        query.sort_order.as_deref(),
        false,
    );
    let ids = query.ids.as_deref().map(|values| {
        values
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect()
    });
    filter.item_ids = ids.clone();
    filter.media_source_ids = ids;
    filter.excluded_item_types = query
        .exclude_item_types
        .as_deref()
        .map(|values| {
            values
                .split(',')
                .filter_map(normalize_emby_item_type)
                .collect()
        })
        .unwrap_or_default();
    filter
}

pub(super) fn emby_compat_media_source_id<'a>(
    ids: Option<&'a str>,
    page: &CatalogPage,
) -> Option<&'a str> {
    ids?.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .find(|id| {
            page.items.iter().any(|item| {
                item.id != *id && item.media_sources.iter().any(|source| source.id == *id)
            })
        })
}

pub(super) async fn emby_user_views(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    match emby_visible_library_items(&state, principal).await {
        Ok(items) => {
            let total = items.len();
            Json(json!({
                "Items": items,
                "TotalRecordCount": total,
                "StartIndex": 0,
            }))
            .into_response()
        }
        Err(status) => status.into_response(),
    }
}

pub(super) async fn emby_library_virtual_folders(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if !user.can_manage_server {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(libraries) = state.libraries.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let media_strategy = match read_media_strategy_settings(database).await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let (resume_played_percent, resume_min_ticks) = match database.resume_settings().await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let accessible_library_ids = match access.accessible_library_ids(principal).await {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match libraries
        .list_libraries_for_user(&user.id.to_string(), &accessible_library_ids)
        .await
    {
        Ok(views) => Json(
            views
                .iter()
                .map(|view| {
                    emby_virtual_folder_json(
                        view,
                        &media_strategy,
                        resume_played_percent,
                        resume_min_ticks,
                    )
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_persons(
    headers: HeaderMap,
    Query(query): Query<EmbyPersonsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.auth.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Some(user_id) = query.user_id.as_deref()
        && let Err(status) = ensure_emby_user_scope(&user, user_id)
    {
        return status.into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let accessible_library_ids = match access.accessible_library_ids(principal).await {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let library_ids = match query.parent_id.as_deref() {
        Some(parent_id) if accessible_library_ids.iter().any(|id| id == parent_id) => {
            vec![parent_id.to_owned()]
        }
        Some(_) => return StatusCode::NOT_FOUND.into_response(),
        None => accessible_library_ids,
    };
    let (offset, limit) = match emby_person_page_params(&query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    // Keep the historical Lux behavior for clients that omit Recursive. An
    // explicit false still requests only direct children.
    let recursive = query.recursive.unwrap_or(true);
    let sort_by = match emby_person_sort(query.sort_by.as_deref()) {
        Ok(sort_by) => sort_by,
        Err(status) => return status.into_response(),
    };
    let descending = match emby_person_sort_order(query.sort_order.as_deref()) {
        Ok(descending) => descending,
        Err(status) => return status.into_response(),
    };
    let options = PersonListOptions {
        recursive,
        sort_by,
        descending,
        offset,
        limit,
    };
    let person_type = match emby_person_type_filter(query.person_types.as_deref()) {
        Some(person_type) => person_type,
        None => {
            return Json(json!({
                "Items": [],
                "TotalRecordCount": 0,
            }))
            .into_response();
        }
    };
    let Some(people) = state.people.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = match query.parent_id.as_deref() {
        Some(parent_id) => {
            people
                .list_library_actors(parent_id, person_type, options)
                .await
        }
        None => {
            people
                .list_libraries_actors(&library_ids, person_type, options)
                .await
        }
    };
    let (actors, total) = match result {
        Ok(result) => result,
        Err(PeopleError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    Json(json!({
        "Items": actors
            .into_iter()
            .map(|actor| {
                emby_person_json_with_fields(actor, &state.server_id, query.auth.fields.as_deref())
            })
            .collect::<Vec<_>>(),
        "TotalRecordCount": total,
    }))
    .into_response()
}

pub(super) async fn emby_person(
    headers: HeaderMap,
    Path(person_id_or_name): Path<String>,
    Query(query): Query<EmbyPersonQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.auth.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Some(user_id) = query.user_id.as_deref()
        && let Err(status) = ensure_emby_user_scope(&user, user_id)
    {
        return status.into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let library_ids = match access
        .accessible_library_ids(AccessPrincipal::new(user.id, user.is_admin))
        .await
    {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(people) = state.people.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match people
        .find_person(&library_ids, "Actor", &person_id_or_name)
        .await
    {
        Ok(Some(person)) => Json(emby_person_json_with_fields(
            person,
            &state.server_id,
            query.auth.fields.as_deref(),
        ))
        .into_response(),
        Ok(None) | Err(PeopleError::InvalidComponent(_)) => StatusCode::NOT_FOUND.into_response(),
        Err(PeopleError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_user_root(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    emby_user_root_response(&state, AccessPrincipal::new(user.id, user.is_admin)).await
}

pub(super) async fn emby_items_root(
    headers: HeaderMap,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let requested_user_id = query.user_id.unwrap_or_else(|| user.id.to_string());
    if let Err(status) = ensure_emby_user_scope(&user, &requested_user_id) {
        return status.into_response();
    }
    emby_user_root_response(&state, AccessPrincipal::new(user.id, user.is_admin)).await
}

pub(super) async fn emby_user_root_response(
    state: &AppState,
    principal: AccessPrincipal,
) -> Response {
    let items = match emby_visible_library_items(state, principal).await {
        Ok(items) => items,
        Err(status) => return status.into_response(),
    };
    Json(json!({
        "Name": "Media Folders",
        "SortName": "Media Folders",
        "Id": principal.user_id.to_string(),
        "ServerId": state.server_id,
        "Type": "Folder",
        "IsFolder": true,
        "MediaType": "Video",
        "ChildCount": items.len(),
        "RecursiveItemCount": items.len(),
        "ImageTags": {},
        "BackdropImageTags": [],
        "UserData": {
            "PlaybackPositionTicks": 0,
            "PlayCount": 0,
            "IsFavorite": false,
            "Played": false,
        },
    }))
    .into_response()
}

pub(super) async fn emby_visible_library_items(
    state: &AppState,
    principal: AccessPrincipal,
) -> Result<Vec<Value>, StatusCode> {
    let Some(access) = state.access.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let accessible_library_ids = access
        .accessible_library_ids(principal)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let views = libraries
        .list_libraries_for_user(&principal.user_id.to_string(), &accessible_library_ids)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let child_counts = state
        .catalog
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?
        .count_library_root_items(&accessible_library_ids)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let mut items = Vec::new();
    for view in views {
        let library_id = view.library.id.to_string();
        let child_count = library_root_count(child_counts.get(&library_id), view.library.kind);
        items.push(emby_library_view_json(
            &view.library,
            &state.server_id,
            child_count,
        ));
    }
    Ok(items)
}

pub(super) async fn emby_library_root_count(
    state: &AppState,
    principal: AccessPrincipal,
    library_id: &str,
    kind: LibraryKind,
) -> Result<i64, StatusCode> {
    let Some(catalog) = state.catalog.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let item_types = match kind {
        LibraryKind::Movie => vec!["MOVIE".to_owned()],
        LibraryKind::Series => vec!["SERIES".to_owned()],
        LibraryKind::Mixed => vec!["MOVIE".to_owned(), "SERIES".to_owned()],
    };
    catalog
        .list_library_items_filtered(
            principal,
            library_id,
            &CatalogFilter {
                item_types,
                ..CatalogFilter::default()
            },
            0,
            1,
        )
        .await
        .map(|page| page.total)
        .map_err(|error| match error {
            CatalogError::Storage(_) => StatusCode::SERVICE_UNAVAILABLE,
            CatalogError::LibraryNotFound | CatalogError::AccessDenied => StatusCode::NOT_FOUND,
        })
}

fn library_root_count(counts: Option<&CatalogItemCounts>, kind: LibraryKind) -> i64 {
    let Some(counts) = counts else {
        return 0;
    };
    match kind {
        LibraryKind::Movie => counts.movie_count,
        LibraryKind::Series => counts.series_count,
        LibraryKind::Mixed => counts.movie_count + counts.series_count,
    }
}

pub(super) async fn emby_user_resume(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let (offset, limit) = match emby_page_params(&query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let page = match catalog
        .list_continue_watching(principal, &user_id, offset, limit)
        .await
    {
        Ok(page) => page,
        Err(CatalogError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            return StatusCode::FORBIDDEN.into_response();
        }
    };
    emby_catalog_page_for_user_with_fields(
        &state,
        &user_id,
        &page,
        query.fields.as_deref(),
        user.can_download,
    )
    .await
}

pub(super) async fn emby_user_latest(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(mut query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let group_items = query.group_items.unwrap_or(true);
    let parent_is_library = match query.parent_id.as_deref() {
        Some(parent_id) => emby_parent_is_library(&state, parent_id).await,
        None => false,
    };
    if group_items
        && query.include_item_types.is_none()
        && (query.parent_id.is_none() || parent_is_library)
    {
        query.include_item_types = Some("Movie,Series".to_owned());
    }
    query.sort_by = Some("DateCreated".to_owned());
    query.sort_order = Some("Descending".to_owned());
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let page = match emby_catalog_page_from_query(&state, principal, &query).await {
        Ok(page) => page,
        Err(status) => return status.into_response(),
    };
    if group_items && emby_latest_groups_children(&query) {
        let (grouped_page, group_counts) =
            match emby_group_latest_page(&state, principal, page).await {
                Ok(result) => result,
                Err(status) => return status.into_response(),
            };
        let mut items = match emby_catalog_items_for_user(
            &state,
            &user_id,
            &grouped_page,
            query.fields.as_deref(),
            user.can_download,
        )
        .await
        {
            Ok(items) => items,
            Err(status) => return status.into_response(),
        };
        for item in &mut items {
            let Some(item_id) = item.get("Id").and_then(Value::as_str) else {
                continue;
            };
            let Some(child_count) = group_counts.get(item_id) else {
                continue;
            };
            if let Value::Object(object) = item {
                object.insert("ChildCount".to_owned(), json!(child_count));
                object.insert("RecursiveItemCount".to_owned(), json!(child_count));
            }
        }
        return Json(items).into_response();
    }
    match emby_catalog_items_for_user(
        &state,
        &user_id,
        &page,
        query.fields.as_deref(),
        user.can_download,
    )
    .await
    {
        Ok(items) => Json(items).into_response(),
        Err(status) => status.into_response(),
    }
}

pub(super) async fn emby_user_favorites(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(mut query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    query.is_favorite = Some(true);
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    emby_list_items(&headers, &state, principal, user.can_download, &query).await
}

pub(super) async fn emby_parent_is_library(state: &AppState, parent_id: &str) -> bool {
    let Ok(library_id) = parent_id.parse::<crate::domain::ids::LibraryId>() else {
        return false;
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return false;
    };
    matches!(
        libraries.get_library(library_id).await,
        Ok(library) if library.is_enabled
    )
}

pub(super) fn emby_latest_groups_children(query: &EmbyItemsQuery) -> bool {
    query.include_item_types.as_deref().is_some_and(|types| {
        types.split(',').any(|item_type| {
            matches!(
                item_type.trim().to_ascii_lowercase().as_str(),
                "episode" | "season"
            )
        })
    })
}

pub(super) async fn emby_group_latest_page(
    state: &AppState,
    principal: AccessPrincipal,
    page: CatalogPage,
) -> Result<(CatalogPage, HashMap<String, i64>), StatusCode> {
    enum LatestGroup {
        Series(String),
        Item(Box<CatalogItem>),
    }

    let mut groups = Vec::new();
    let mut group_counts = HashMap::new();
    let mut series_ids = Vec::new();
    for item in page.items {
        let Some(series_id) = item
            .series_id
            .as_deref()
            .filter(|_| matches!(item.item_type.as_str(), "EPISODE" | "SEASON"))
        else {
            groups.push(LatestGroup::Item(Box::new(item)));
            continue;
        };
        if !group_counts.contains_key(series_id) {
            series_ids.push(series_id.to_owned());
            groups.push(LatestGroup::Series(series_id.to_owned()));
        }
        *group_counts.entry(series_id.to_owned()).or_insert(0) += 1;
    }

    let Some(catalog) = state.catalog.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let mut series_by_id = HashMap::new();
    if !series_ids.is_empty() {
        let filter = CatalogFilter {
            item_types: vec!["SERIES".to_owned()],
            excluded_item_types: Vec::new(),
            item_ids: Some(series_ids.clone()),
            person_id: None,
            media_source_ids: None,
            years: Vec::new(),
            is_played: None,
            is_favorite: None,
            metadata_pending: false,
            sort_by: CatalogSort::Name,
            descending: false,
        };
        let series_page = catalog
            .list_all_items_filtered(principal, &filter, 0, series_ids.len() as i64)
            .await
            .map_err(emby_catalog_error_status)?;
        series_by_id.extend(
            series_page
                .items
                .into_iter()
                .map(|item| (item.id.clone(), item)),
        );
    }

    let mut items = Vec::with_capacity(groups.len());
    let mut resolved_group_counts = HashMap::new();
    for group in groups {
        match group {
            LatestGroup::Series(series_id) => {
                if let Some(item) = series_by_id.remove(&series_id) {
                    if let Some(count) = group_counts.get(&series_id) {
                        resolved_group_counts.insert(series_id, *count);
                    }
                    items.push(item);
                }
            }
            LatestGroup::Item(item) => items.push(*item),
        }
    }
    let total = i64::try_from(items.len()).unwrap_or(i64::MAX);
    Ok((
        CatalogPage {
            items,
            total,
            offset: page.offset,
            limit: page.limit,
        },
        resolved_group_counts,
    ))
}

pub(super) async fn emby_user_next_up(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    emby_next_up_response(&state, &user, &user_id, &query).await
}

pub(super) async fn emby_shows_next_up(
    headers: HeaderMap,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let user_id = query.user_id.clone().unwrap_or_else(|| user.id.to_string());
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    emby_next_up_response(&state, &user, &user_id, &query).await
}

pub(super) async fn emby_next_up_response(
    state: &AppState,
    user: &UserRecord,
    user_id: &str,
    query: &EmbyItemsQuery,
) -> Response {
    let (offset, limit) = match emby_page_params(query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog
        .list_next_up(
            AccessPrincipal::new(user.id, user.is_admin),
            user_id,
            query.series_id.as_deref(),
            offset,
            limit,
        )
        .await
    {
        Ok(page) => {
            emby_catalog_page_for_user_with_preferred_source(
                state,
                user_id,
                &page,
                query.fields.as_deref(),
                user.can_download,
                None,
                query.enable_total_record_count != Some(false),
            )
            .await
        }
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            StatusCode::FORBIDDEN.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_show_seasons(
    headers: HeaderMap,
    Path(series_id): Path<String>,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let (offset, limit) = match emby_page_params(&query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog
        .list_children(
            AccessPrincipal::new(user.id, user.is_admin),
            &series_id,
            "SEASON",
            offset,
            limit,
        )
        .await
    {
        Ok(page) => {
            emby_catalog_page_for_user_with_fields(
                &state,
                &user.id.to_string(),
                &page,
                query.fields.as_deref(),
                user.can_download,
            )
            .await
        }
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_show_episodes(
    headers: HeaderMap,
    Path(series_id): Path<String>,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let (offset, limit) = match emby_page_params(&query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    // Emby clients commonly serialize an unset season selector as `SeasonId=`.
    // Treat it the same as an omitted selector instead of looking up an empty ID.
    let season_id = query.season_id.as_deref().and_then(|value| {
        let value = value.trim();
        (!value.is_empty()
            && !value.eq_ignore_ascii_case("null")
            && !value.eq_ignore_ascii_case("undefined"))
        .then_some(value)
    });
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let episodes = catalog
        .list_series_episodes(principal, &series_id, season_id, offset, limit)
        .await;
    match episodes {
        Ok(page) => {
            emby_catalog_page_for_user_with_preferred_source_and_options(
                &state,
                &user.id.to_string(),
                &page,
                query.fields.as_deref(),
                user.can_download,
                EmbyCatalogPageOptions {
                    preferred_source_id: None,
                    include_start_index: true,
                },
            )
            .await
        }
        // VidHub can retain a stale season identifier after a library refresh.
        // Emby still serves the show's episode list in that case; retry without
        // the optional season filter so one stale selector cannot blank the page.
        Err(CatalogError::LibraryNotFound) if season_id.is_some() => match catalog
            .list_series_episodes(principal, &series_id, None, offset, limit)
            .await
        {
            Ok(page) => {
                emby_catalog_page_for_user_with_preferred_source_and_options(
                    &state,
                    &user.id.to_string(),
                    &page,
                    query.fields.as_deref(),
                    user.can_download,
                    EmbyCatalogPageOptions {
                        preferred_source_id: None,
                        include_start_index: true,
                    },
                )
                .await
            }
            Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
                StatusCode::NOT_FOUND.into_response()
            }
            Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_collection_children(
    headers: HeaderMap,
    Path(collection_id): Path<String>,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let (offset, limit) = match emby_page_params(&query) {
        Ok(params) => params,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match catalog
        .list_collection_items(
            AccessPrincipal::new(user.id, user.is_admin),
            &collection_id,
            offset,
            limit,
        )
        .await
    {
        Ok(page) => {
            emby_catalog_page_for_user_with_fields(
                &state,
                &user.id.to_string(),
                &page,
                query.fields.as_deref(),
                user.can_download,
            )
            .await
        }
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_catalog_page_for_user_with_fields(
    state: &AppState,
    user_id: &str,
    page: &CatalogPage,
    fields: Option<&str>,
    can_download: bool,
) -> Response {
    emby_catalog_page_for_user_with_preferred_source(
        state,
        user_id,
        page,
        fields,
        can_download,
        None,
        true,
    )
    .await
}

pub(super) async fn emby_catalog_page_for_user_with_preferred_source(
    state: &AppState,
    user_id: &str,
    page: &CatalogPage,
    fields: Option<&str>,
    can_download: bool,
    preferred_source_id: Option<&str>,
    include_start_index: bool,
) -> Response {
    emby_catalog_page_for_user_with_preferred_source_and_options(
        state,
        user_id,
        page,
        fields,
        can_download,
        EmbyCatalogPageOptions {
            preferred_source_id,
            include_start_index,
        },
    )
    .await
}

pub(super) struct EmbyCatalogPageOptions<'a> {
    pub(super) preferred_source_id: Option<&'a str>,
    pub(super) include_start_index: bool,
}

pub(super) async fn emby_catalog_page_for_user_with_preferred_source_and_options(
    state: &AppState,
    user_id: &str,
    page: &CatalogPage,
    fields: Option<&str>,
    can_download: bool,
    options: EmbyCatalogPageOptions<'_>,
) -> Response {
    match emby_catalog_items_for_user_with_preferred_source(
        state,
        user_id,
        page,
        fields,
        can_download,
        options.preferred_source_id,
    )
    .await
    {
        Ok(items) => {
            let mut body = json!({
                "Items": items,
                "TotalRecordCount": page.total,
            });
            if options.include_start_index
                && let Value::Object(object) = &mut body
            {
                object.insert("StartIndex".to_owned(), json!(page.offset));
            }
            Json(body).into_response()
        }
        Err(status) => status.into_response(),
    }
}

pub(super) async fn emby_catalog_items_for_user(
    state: &AppState,
    user_id: &str,
    page: &CatalogPage,
    fields: Option<&str>,
    can_download: bool,
) -> Result<Vec<Value>, StatusCode> {
    emby_catalog_items_for_user_with_preferred_source(
        state,
        user_id,
        page,
        fields,
        can_download,
        None,
    )
    .await
}

pub(super) async fn emby_catalog_items_for_user_with_preferred_source(
    state: &AppState,
    user_id: &str,
    page: &CatalogPage,
    fields: Option<&str>,
    can_download: bool,
    preferred_source_id: Option<&str>,
) -> Result<Vec<Value>, StatusCode> {
    let Some(database) = state.database.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let item_ids = page
        .items
        .iter()
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    let user_states = match database.list_user_item_states(user_id, &item_ids).await {
        Ok(states) => states,
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let mut catalog_items = page.items.clone();
    if catalog
        .populate_image_tags(&mut catalog_items)
        .await
        .is_err()
    {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let unplayed_item_counts =
        match emby_unplayed_episode_counts(catalog, user_id, &catalog_items).await {
            Ok(counts) => counts,
            Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
        };
    if emby_fields_include(fields, "Chapters")
        && catalog.populate_chapters(&mut catalog_items).await.is_err()
    {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let include_nfo = emby_nfo_fields_requested(fields);
    let include_people = fields.is_some_and(|fields| emby_fields_include(Some(fields), "People"));
    let extras = load_emby_item_extras(
        state.local_nfo.clone(),
        state.people.clone(),
        &catalog_items,
        include_nfo,
        include_people,
    )
    .await;
    let mut items = Vec::with_capacity(catalog_items.len());
    for (item, (nfo, actors)) in catalog_items.iter().zip(extras) {
        let mut value = emby_catalog_item_json_with_state(
            item,
            &state.server_id,
            user_states.get(&item.id),
            nfo.as_ref(),
            can_download,
            fields,
            unplayed_item_counts.get(&item.id).copied(),
        );
        if let Some(source_id) = preferred_source_id
            && let Some(Value::Array(sources)) = value.get_mut("MediaSources")
            && let Some(index) = sources
                .iter()
                .position(|source| source.get("Id").and_then(Value::as_str) == Some(source_id))
        {
            let source = sources.remove(index);
            sources.insert(0, source);
        }
        if include_people {
            if let Value::Object(object) = &mut value {
                let mut people = actors
                    .into_iter()
                    .map(|actor| emby_person_json(actor, &state.server_id))
                    .collect::<Vec<_>>();
                if let Some(nfo) = nfo.as_ref() {
                    people.extend(emby_nfo_crew_json(nfo));
                }
                object.insert("People".to_owned(), Value::Array(people));
            }
        }
        items.push(value);
    }
    Ok(items)
}

async fn emby_unplayed_episode_counts(
    catalog: &CatalogService,
    user_id: &str,
    items: &[CatalogItem],
) -> Result<HashMap<String, i64>, CatalogError> {
    let item_ids = items
        .iter()
        .filter(|item| matches!(item.item_type.as_str(), "SERIES" | "SEASON"))
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    catalog
        .list_unplayed_episode_counts(user_id, &item_ids)
        .await
}

async fn emby_unplayed_episode_count(
    catalog: &CatalogService,
    user_id: &str,
    item: &CatalogItem,
) -> Result<Option<i64>, CatalogError> {
    if !matches!(item.item_type.as_str(), "SERIES" | "SEASON") {
        return Ok(None);
    }
    let counts = catalog
        .list_unplayed_episode_counts(user_id, std::slice::from_ref(&item.id))
        .await?;
    Ok(Some(counts.get(&item.id).copied().unwrap_or_default()))
}

async fn load_emby_item_extras(
    local_nfo: Option<LocalNfoMetadataStore>,
    people: Option<PeopleService>,
    items: &[CatalogItem],
    include_nfo: bool,
    include_people: bool,
) -> Vec<(
    Option<LocalNfoDetails>,
    Vec<crate::application::people::ActorView>,
)> {
    let mut extras = vec![(None, Vec::new()); items.len()];
    if items.is_empty() || (!include_nfo && !include_people) {
        return extras;
    }
    let semaphore = Arc::new(Semaphore::new(EMBY_ITEM_EXTRA_CONCURRENCY));
    let mut tasks = JoinSet::new();
    for (index, item) in items.iter().enumerate() {
        let permit = match Arc::clone(&semaphore).acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => break,
        };
        let item_id = item.id.clone();
        let local_nfo = local_nfo.clone();
        let people = people.clone();
        tasks.spawn(async move {
            let _permit = permit;
            let nfo = if include_nfo {
                match local_nfo {
                    Some(store) => match store.read_item(&item_id).await {
                        Ok(details) => details,
                        Err(error) => {
                            tracing::warn!(
                                item_id = %item_id,
                                %error,
                                "derived local NFO cache is unavailable for Emby list response"
                            );
                            None
                        }
                    },
                    None => None,
                }
            } else {
                None
            };
            let actors = if include_people {
                match people {
                    Some(people) => match people.list_item_actors(&item_id).await {
                        Ok(actors) => actors,
                        Err(error) => {
                            tracing::warn!(
                                item_id = %item_id,
                                %error,
                                "derived actor relation is unavailable for Emby list response"
                            );
                            Vec::new()
                        }
                    },
                    None => Vec::new(),
                }
            } else {
                Vec::new()
            };
            (index, nfo, actors)
        });
    }
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok((index, nfo, actors)) if index < extras.len() => {
                extras[index] = (nfo, actors);
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(%error, "Emby item extras task failed");
            }
        }
    }
    extras
}

pub(super) async fn emby_user_items(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    emby_list_items(&headers, &state, principal, user.can_download, &query).await
}

pub(super) async fn emby_items(
    headers: HeaderMap,
    Query(query): Query<EmbyItemsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Some(user_id) = query.user_id.as_deref()
        && let Err(status) = ensure_emby_user_scope(&user, user_id)
    {
        return status.into_response();
    }
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    emby_list_items(&headers, &state, principal, user.can_download, &query).await
}

pub(super) async fn emby_items_counts(
    headers: HeaderMap,
    Query(query): Query<EmbyItemCountsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Some(auth) = state.emby_auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    let (principal, target_user_id) = match query.user_id.as_deref() {
        Some(requested_id) => {
            if let Err(status) = ensure_emby_user_scope(&user, requested_id) {
                return status.into_response();
            }
            let target_user = match auth.user_by_id(requested_id).await {
                Ok(Some(target_user)) => target_user,
                Ok(None) => return StatusCode::NOT_FOUND.into_response(),
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
            if target_user.is_disabled {
                return StatusCode::NOT_FOUND.into_response();
            }
            let target_user_id = match requested_id.parse::<crate::domain::ids::UserId>() {
                Ok(target_user_id) => target_user_id,
                Err(_) => return StatusCode::BAD_REQUEST.into_response(),
            };
            (
                AccessPrincipal::new(target_user_id, target_user.is_admin),
                target_user.id.to_string(),
            )
        }
        None => (
            AccessPrincipal::new(user.id, user.is_admin),
            user.id.to_string(),
        ),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let counts = match catalog
        .count_item_types(principal, &target_user_id, query.is_favorite)
        .await
    {
        Ok(counts) => counts,
        Err(CatalogError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            return StatusCode::FORBIDDEN.into_response();
        }
    };

    Json(json!({
        "MovieCount": counts.movie_count,
        "SeriesCount": counts.series_count,
        "EpisodeCount": counts.episode_count,
        "GameCount": 0,
        "ArtistCount": 0,
        "ProgramCount": 0,
        "GameSystemCount": 0,
        "TrailerCount": 0,
        "SongCount": 0,
        "AlbumCount": 0,
        "MusicVideoCount": 0,
        "BoxSetCount": counts.box_set_count,
        "BookCount": 0,
        "ItemCount": counts.item_count,
    }))
    .into_response()
}

pub(super) async fn emby_list_items(
    _headers: &HeaderMap,
    state: &AppState,
    principal: AccessPrincipal,
    can_download: bool,
    query: &EmbyItemsQuery,
) -> Response {
    let root_id = principal.user_id.to_string();
    if emby_query_targets_user_root_views(query, &root_id) {
        return match emby_visible_library_items(state, principal).await {
            Ok(items) => Json(json!({
                "Items": items,
                "TotalRecordCount": items.len(),
                "StartIndex": 0,
            }))
            .into_response(),
            Err(status) => status.into_response(),
        };
    }
    if let Some(response) =
        emby_single_id_lookup_response(state, principal, can_download, query).await
    {
        return response;
    }
    match emby_catalog_page_from_query(state, principal, query).await {
        Ok(page) => {
            let preferred_source_id = emby_compat_media_source_id(query.ids.as_deref(), &page);
            emby_catalog_page_for_user_with_preferred_source(
                state,
                &principal.user_id.to_string(),
                &page,
                query.fields.as_deref(),
                can_download,
                preferred_source_id,
                emby_query_requests_series_children(state, principal, query).await,
            )
            .await
        }
        Err(status) => status.into_response(),
    }
}

pub(super) fn emby_single_id_lookup(query: &EmbyItemsQuery) -> Option<&str> {
    if query.start_index.unwrap_or(0) != 0
        || query.limit.is_some_and(|limit| !(1..=100).contains(&limit))
        || query.user_id.is_some()
        || query.series_id.is_some()
        || query.parent_id.is_some()
        || query.include_item_types.is_some()
        || query.exclude_item_types.is_some()
        || query.season_id.is_some()
        || query.search_term.is_some()
        || query.is_played.is_some()
        || query.is_favorite.is_some()
        || query.years.is_some()
        || query.sort_by.is_some()
        || query.sort_order.is_some()
        || query.group_items.is_some()
        || query.recursive.is_some()
    {
        return None;
    }
    let mut ids = query
        .ids
        .as_deref()?
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let id = ids.next()?;
    ids.next().is_none().then_some(id)
}

pub(super) async fn emby_single_id_lookup_response(
    state: &AppState,
    principal: AccessPrincipal,
    can_download: bool,
    query: &EmbyItemsQuery,
) -> Option<Response> {
    let requested_id = emby_single_id_lookup(query)?;
    let catalog = state.catalog.as_ref()?;
    let (item, preferred_source_id) = match catalog.find_item(principal, requested_id).await {
        Ok(Some(item)) => (Some(item), None),
        Ok(None) => match catalog
            .find_item_by_media_source_id(principal, requested_id)
            .await
        {
            Ok(item) => (item, Some(requested_id)),
            Err(CatalogError::Storage(_)) => {
                return Some(StatusCode::SERVICE_UNAVAILABLE.into_response());
            }
            Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => (None, None),
        },
        Err(CatalogError::Storage(_)) => {
            return Some(StatusCode::SERVICE_UNAVAILABLE.into_response());
        }
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => (None, None),
    };
    let mut item = item?;
    if preferred_source_id.is_some()
        && emby_fields_include(query.fields.as_deref(), "Chapters")
        && catalog
            .populate_chapters(std::slice::from_mut(&mut item))
            .await
            .is_err()
    {
        return Some(StatusCode::SERVICE_UNAVAILABLE.into_response());
    }
    let work_plan = emby_item_detail_work_plan(query.fields.as_deref());
    if work_plan.populate_image_tags
        && catalog
            .populate_image_tags(std::slice::from_mut(&mut item))
            .await
            .is_err()
    {
        return Some(StatusCode::SERVICE_UNAVAILABLE.into_response());
    }
    let database = state.database.as_ref()?;
    let nfo = if emby_nfo_fields_requested(query.fields.as_deref()) {
        read_local_nfo_details(state, &item.id).await
    } else {
        None
    };
    let user_state = match database
        .find_user_item_state(&principal.user_id.to_string(), &item.id)
        .await
    {
        Ok(state) => state,
        Err(_) => return Some(StatusCode::SERVICE_UNAVAILABLE.into_response()),
    };
    let unplayed_item_count =
        match emby_unplayed_episode_count(catalog, &principal.user_id.to_string(), &item).await {
            Ok(count) => count,
            Err(_) => return Some(StatusCode::SERVICE_UNAVAILABLE.into_response()),
        };
    let mut item_json = emby_catalog_item_json_with_state(
        &item,
        &state.server_id,
        user_state.as_ref(),
        nfo.as_ref(),
        can_download,
        query.fields.as_deref(),
        unplayed_item_count,
    );
    if let Some(source_id) = preferred_source_id
        && let Some(Value::Array(sources)) = item_json.get_mut("MediaSources")
        && let Some(index) = sources
            .iter()
            .position(|source| source.get("Id").and_then(Value::as_str) == Some(source_id))
    {
        let source = sources.remove(index);
        sources.insert(0, source);
    }
    if emby_fields_include(query.fields.as_deref(), "People") {
        let actors = match state.people.as_ref() {
            Some(people) => match people.list_item_actors(&item.id).await {
                Ok(actors) => actors,
                Err(error) => {
                    tracing::warn!(
                        item_id = %item.id,
                        %error,
                        "derived actor relation is unavailable for Emby ID lookup"
                    );
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        if let Value::Object(object) = &mut item_json {
            let mut people = actors
                .into_iter()
                .map(|actor| emby_person_json(actor, &state.server_id))
                .collect::<Vec<_>>();
            if let Some(nfo) = nfo.as_ref() {
                people.extend(emby_nfo_crew_json(nfo));
            }
            object.insert("People".to_owned(), Value::Array(people));
        }
    }
    Some(
        Json(json!({
            "Items": [item_json],
            "TotalRecordCount": 1,
        }))
        .into_response(),
    )
}

pub(super) async fn emby_query_requests_series_children(
    state: &AppState,
    principal: AccessPrincipal,
    query: &EmbyItemsQuery,
) -> bool {
    if query.include_item_types.as_deref().is_some_and(|types| {
        types.split(',').any(|item_type| {
            matches!(
                item_type.trim().to_ascii_lowercase().as_str(),
                "season" | "episode"
            )
        })
    }) {
        return true;
    }
    // Emby infers the child type from ParentId when IncludeItemTypes is omitted.
    // VidHub uses this compact form on the series detail screen.
    let Some(parent_id) = query.parent_id.as_deref() else {
        return false;
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return false;
    };
    matches!(
        catalog.find_item(principal, parent_id).await,
        Ok(Some(item)) if matches!(item.item_type.as_str(), "SERIES" | "SEASON")
    )
}

pub(super) fn emby_query_targets_user_root_views(query: &EmbyItemsQuery, root_id: &str) -> bool {
    let parent_is_root = query.parent_id.as_deref() == Some(root_id);
    let requests_folder_views = query.include_item_types.as_deref().is_some_and(|types| {
        types.split(',').all(|item_type| {
            matches!(
                item_type.trim().to_ascii_lowercase().as_str(),
                "folder" | "collectionfolder"
            )
        })
    });
    let requests_filmly_home_views = query.parent_id.is_none()
        && query.include_item_types.is_none()
        && query.recursive != Some(true)
        && query.exclude_item_types.is_some();
    (parent_is_root && (query.include_item_types.is_none() || requests_folder_views))
        || (query.parent_id.is_none() && requests_folder_views)
        || requests_filmly_home_views
}

pub(super) async fn emby_catalog_page_from_query(
    state: &AppState,
    principal: AccessPrincipal,
    query: &EmbyItemsQuery,
) -> Result<CatalogPage, StatusCode> {
    let (offset, limit) = match emby_page_params(query) {
        Ok(params) => params,
        Err(status) => return Err(status),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    if let Some(raw_query) = query.search_term.as_deref().map(str::trim)
        && !raw_query.is_empty()
    {
        let (Some(search_query), Some(like_query)) = (
            normalize_search_query(raw_query),
            normalize_search_like_query(raw_query),
        ) else {
            return Ok(CatalogPage {
                items: Vec::new(),
                total: 0,
                offset,
                limit,
            });
        };
        return catalog
            .search_items(principal, &search_query, &like_query, offset, limit)
            .await
            .map_err(emby_catalog_error_status);
    }
    let mut filter = catalog_filter_from_emby(query);
    let root_scope = match query.parent_id.as_deref() {
        Some(parent_id) => emby_parent_is_library(state, parent_id).await,
        None => true,
    };
    if root_scope && !query.recursive.unwrap_or(false) && query.include_item_types.is_none() {
        filter.item_types = vec!["MOVIE".to_owned(), "SERIES".to_owned()];
    }
    let page = match query.parent_id.as_deref() {
        Some(parent_id) => {
            if let Ok(library_id) = parent_id.parse::<crate::domain::ids::LibraryId>() {
                match catalog
                    .list_library_items_filtered(
                        principal,
                        &library_id.to_string(),
                        &filter,
                        offset,
                        limit,
                    )
                    .await
                {
                    Ok(page) => Ok(page),
                    Err(CatalogError::LibraryNotFound) => {
                        emby_catalog_page_for_item_parent(
                            catalog, principal, parent_id, query, offset, limit,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                }
            } else {
                emby_catalog_page_for_item_parent(
                    catalog, principal, parent_id, query, offset, limit,
                )
                .await
            }
        }
        None => {
            catalog
                .list_all_items_filtered(principal, &filter, offset, limit)
                .await
        }
    };
    match page {
        Ok(page) => Ok(page),
        Err(error) => Err(emby_catalog_error_status(error)),
    }
}

pub(super) async fn emby_catalog_page_for_item_parent(
    catalog: &CatalogService,
    principal: AccessPrincipal,
    parent_id: &str,
    query: &EmbyItemsQuery,
    offset: i64,
    limit: i64,
) -> Result<CatalogPage, CatalogError> {
    let Some(parent) = catalog.find_item(principal, parent_id).await? else {
        return Err(CatalogError::LibraryNotFound);
    };
    let requested_types = catalog_filter_from_emby(query).item_types;
    let requested_type = requested_types.first().map(String::as_str);
    if parent.item_type == "SERIES"
        && requested_type == Some("EPISODE")
        && (query.recursive.unwrap_or(false) || query.group_items == Some(false))
    {
        return catalog
            .list_series_episodes(
                principal,
                parent_id,
                query.season_id.as_deref(),
                offset,
                limit,
            )
            .await;
    }
    let child_type = match (parent.item_type.as_str(), requested_type) {
        (_, Some(item_type)) => item_type,
        ("SERIES", _) => "SEASON",
        ("SEASON", _) => "EPISODE",
        _ => {
            return Ok(CatalogPage {
                items: Vec::new(),
                total: 0,
                offset,
                limit,
            });
        }
    };
    catalog
        .list_children(principal, parent_id, child_type, offset, limit)
        .await
}

pub(super) fn emby_catalog_error_status(error: CatalogError) -> StatusCode {
    match error {
        CatalogError::LibraryNotFound => StatusCode::NOT_FOUND,
        CatalogError::AccessDenied => StatusCode::FORBIDDEN,
        CatalogError::Storage(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

pub(super) async fn emby_item(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let fields = emby_detail_fields(query.fields.as_deref());
    emby_item_response(
        &state,
        principal,
        &item_id,
        user.can_download,
        fields.as_deref(),
    )
    .await
}

#[derive(Deserialize)]
pub(super) struct EmbyPersonUpdateRequest {
    #[serde(rename = "Name")]
    pub(super) name: String,
    #[serde(rename = "Id")]
    pub(super) id: String,
    #[serde(rename = "Type")]
    pub(super) item_type: Option<String>,
    #[serde(rename = "Overview")]
    pub(super) overview: Option<String>,
    #[serde(rename = "BirthDate")]
    pub(super) birth_date: Option<String>,
    #[serde(rename = "DeathDate")]
    pub(super) death_date: Option<String>,
    #[serde(rename = "KnownForDepartment")]
    pub(super) known_for_department: Option<String>,
    #[serde(rename = "PlaceOfBirth")]
    pub(super) place_of_birth: Option<String>,
    #[serde(rename = "ProviderIds", default)]
    pub(super) provider_ids: BTreeMap<String, String>,
    #[serde(rename = "Genres", default)]
    pub(super) genres: Vec<String>,
    #[serde(rename = "Tags", default)]
    pub(super) tags: Vec<String>,
    #[serde(rename = "ProductionLocations", default)]
    pub(super) production_locations: Vec<String>,
    #[serde(rename = "PremiereDate")]
    pub(super) premiere_date: Option<String>,
    #[serde(rename = "ProductionYear")]
    pub(super) production_year: Option<i32>,
    #[serde(rename = "Taglines", default)]
    pub(super) taglines: Vec<String>,
}

pub(super) async fn emby_update_item(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    Json(request): Json<EmbyPersonUpdateRequest>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if request.id != item_id {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if request
        .item_type
        .as_deref()
        .is_some_and(|item_type| !item_type.eq_ignore_ascii_case("Person"))
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let library_ids = match access
        .accessible_library_ids(AccessPrincipal::new(user.id, user.is_admin))
        .await
    {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(people) = state.people.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let update = PersonMetadataUpdate {
        name: request.name.trim().to_owned(),
        biography: request.overview,
        birthday: request.birth_date,
        deathday: request.death_date,
        known_for_department: request.known_for_department,
        place_of_birth: request.place_of_birth,
        provider_ids: request.provider_ids,
        genres: request.genres,
        tags: request.tags,
        production_locations: request.production_locations,
        premiere_date: request.premiere_date,
        production_year: request.production_year,
        taglines: request.taglines,
    };
    match people
        .update_person_metadata(&library_ids, &item_id, update)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(PeopleError::InvalidComponent(_)) => return StatusCode::BAD_REQUEST.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    match people.find_person(&library_ids, "Actor", &item_id).await {
        Ok(Some(person)) => Json(emby_person_json_with_fields(
            person,
            &state.server_id,
            emby_detail_fields(query.fields.as_deref()).as_deref(),
        ))
        .into_response(),
        Ok(None) | Err(PeopleError::InvalidComponent(_)) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_user_item(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let fields = emby_detail_fields(query.fields.as_deref());
    emby_item_response(
        &state,
        principal,
        &item_id,
        user.can_download,
        fields.as_deref(),
    )
    .await
}

pub(super) async fn emby_item_response(
    state: &AppState,
    principal: AccessPrincipal,
    item_id: &str,
    can_download: bool,
    fields: Option<&str>,
) -> Response {
    if item_id == principal.user_id.to_string() {
        return emby_user_root_response(state, principal).await;
    }
    if let Ok(library_id) = item_id.parse::<crate::domain::ids::LibraryId>()
        && let Some(libraries) = state.libraries.as_ref()
    {
        match libraries.get_library(library_id).await {
            Ok(library) => {
                if !library.is_enabled {
                    return StatusCode::NOT_FOUND.into_response();
                }
                let Some(access) = state.access.as_ref() else {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                };
                match access.can_view_library(principal, item_id).await {
                    Ok(true) => {}
                    Ok(false) => return StatusCode::NOT_FOUND.into_response(),
                    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                }
                let child_count =
                    match emby_library_root_count(state, principal, item_id, library.kind).await {
                        Ok(count) => count,
                        Err(status) => return status.into_response(),
                    };
                return Json(emby_library_view_json(
                    &library,
                    &state.server_id,
                    child_count,
                ))
                .into_response();
            }
            Err(LibraryServiceError::LibraryNotFound) => {}
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let (catalog_item, resolved_from_media_source_id) =
        match catalog.find_item(principal, item_id).await {
            Ok(Some(item)) => (Some(item), false),
            Ok(None) => match catalog
                .find_item_by_media_source_id(principal, item_id)
                .await
            {
                Ok(item) => (item, true),
                Err(CatalogError::Storage(_)) => {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => (None, true),
            },
            Err(CatalogError::Storage(_)) => {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => (None, false),
        };
    match catalog_item {
        Some(item) if resolved_from_media_source_id => {
            let unplayed_item_count =
                match emby_unplayed_episode_count(catalog, &principal.user_id.to_string(), &item)
                    .await
                {
                    Ok(count) => count,
                    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                };
            let item_json = emby_catalog_item_json_with_state_and_aspect_ratio(
                &item,
                &state.server_id,
                None,
                EmbyItemJsonOptions {
                    nfo: None,
                    can_download,
                    fields,
                    primary_image_aspect_ratio: None,
                    include_top_level_media_streams: true,
                    unplayed_item_count,
                },
            );
            Json(item_json).into_response()
        }
        Some(mut item) => {
            let Some(database) = state.database.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            let work_plan = emby_item_detail_work_plan(fields);
            if work_plan.populate_image_tags
                && catalog
                    .populate_image_tags(std::slice::from_mut(&mut item))
                    .await
                    .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            let user_id = principal.user_id.to_string();
            let (nfo, user_state, aspect_ratio, actors) = tokio::join!(
                async {
                    if work_plan.read_nfo {
                        read_local_nfo_details(state, &item.id).await
                    } else {
                        None
                    }
                },
                database.find_user_item_state(&user_id, &item.id),
                async {
                    if work_plan.read_primary_image_aspect_ratio {
                        emby_primary_image_aspect_ratio(state, principal, &item.id).await
                    } else {
                        None
                    }
                },
                async {
                    if !work_plan.read_people {
                        return Vec::new();
                    }
                    match state.people.as_ref() {
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
                    }
                },
            );
            let user_state = match user_state {
                Ok(state) => state,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            let unplayed_item_count =
                match emby_unplayed_episode_count(catalog, &principal.user_id.to_string(), &item)
                    .await
                {
                    Ok(count) => count,
                    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                };
            let mut item_json = emby_catalog_item_json_with_state_and_aspect_ratio(
                &item,
                &state.server_id,
                user_state.as_ref(),
                EmbyItemJsonOptions {
                    nfo: nfo.as_ref(),
                    can_download,
                    fields,
                    primary_image_aspect_ratio: aspect_ratio,
                    include_top_level_media_streams: true,
                    unplayed_item_count,
                },
            );
            if work_plan.read_people
                && let Value::Object(object) = &mut item_json
            {
                let mut people = actors
                    .into_iter()
                    .map(|actor| emby_person_json(actor, &state.server_id))
                    .collect::<Vec<_>>();
                if let Some(nfo) = nfo.as_ref() {
                    people.extend(emby_nfo_crew_json(nfo));
                }
                object.insert("People".to_owned(), Value::Array(people));
            }
            Json(item_json).into_response()
        }
        None => {
            let Some(access) = state.access.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            let library_ids = match access.accessible_library_ids(principal).await {
                Ok(library_ids) => library_ids,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            let Some(people) = state.people.as_ref() else {
                return StatusCode::NOT_FOUND.into_response();
            };
            match people.find_person(&library_ids, "Actor", item_id).await {
                Ok(Some(person)) => Json(emby_person_json_with_fields(
                    person,
                    &state.server_id,
                    fields,
                ))
                .into_response(),
                Ok(None) | Err(PeopleError::InvalidComponent(_)) => {
                    StatusCode::NOT_FOUND.into_response()
                }
                Err(PeopleError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
    }
}

pub(super) async fn emby_person_image(
    headers: HeaderMap,
    method: Method,
    Path((person_id, image_type)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    emby_person_image_response(&headers, &method, &person_id, &image_type, &query, &state).await
}

pub(super) async fn emby_person_image_at_index(
    headers: HeaderMap,
    method: Method,
    Path((person_id, image_type, image_index)): Path<(String, String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    if image_index.parse::<i64>().ok() != Some(0) {
        return StatusCode::NOT_FOUND.into_response();
    }
    emby_person_image_response(&headers, &method, &person_id, &image_type, &query, &state).await
}

pub(super) async fn emby_person_image_response(
    headers: &HeaderMap,
    method: &Method,
    person_id: &str,
    image_type: &str,
    query: &EmbyTokenQuery,
    state: &AppState,
) -> Response {
    if normalize_image_type(image_type) != Some("POSTER") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Err(status) = require_emby_user(headers, state, query.api_key.as_deref()).await {
        return status.into_response();
    }
    let Some(people) = state.people.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let image = match people.profile_image_for_emby_name_or_id(person_id).await {
        Ok(Some(image)) => image,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(PeopleError::InvalidComponent(_)) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let etag = format!("\"{}\"", emby_person_image_tag(person_id));
    serve_image_file(
        &image.path,
        image.content_type,
        image.content_length,
        &etag,
        headers,
        method,
    )
    .await
}

pub(super) fn emby_person_json(
    actor: crate::application::people::ActorView,
    server_id: &str,
) -> Value {
    emby_person_json_with_fields(actor, server_id, None)
}

pub(super) fn emby_person_json_with_fields(
    actor: crate::application::people::ActorView,
    server_id: &str,
    fields: Option<&str>,
) -> Value {
    let image_tag = actor
        .image_url
        .as_ref()
        .map(|_| emby_person_image_tag(&actor.id));
    let include = |field| fields.is_none_or(|fields| emby_fields_include(Some(fields), field));
    let image_tags = image_tag
        .clone()
        .map(|tag| json!({"Primary": tag}))
        .unwrap_or_else(|| json!({}));
    let mut object = serde_json::Map::from_iter([
        ("Name".to_owned(), json!(actor.name)),
        ("ServerId".to_owned(), json!(server_id)),
        ("Id".to_owned(), json!(actor.id)),
        ("Type".to_owned(), json!("Person")),
        ("ImageTags".to_owned(), image_tags),
        ("BackdropImageTags".to_owned(), json!([])),
    ]);
    if include("Role")
        && let Some(role) = actor.character
    {
        object.insert("Role".to_owned(), json!(role));
    }
    if include("PrimaryImageTag")
        && let Some(image_tag) = image_tag
    {
        object.insert("PrimaryImageTag".to_owned(), json!(image_tag));
    }
    if include("Overview")
        && let Some(overview) = actor.biography
    {
        object.insert("Overview".to_owned(), json!(overview));
    }
    if include("BirthDate")
        && let Some(birthday) = actor.birthday
    {
        object.insert("BirthDate".to_owned(), json!(birthday));
    }
    if include("DeathDate")
        && let Some(deathday) = actor.deathday
    {
        object.insert("DeathDate".to_owned(), json!(deathday));
    }
    if include("KnownForDepartment")
        && let Some(known_for_department) = actor.known_for_department
    {
        object.insert("KnownForDepartment".to_owned(), json!(known_for_department));
    }
    if include("PlaceOfBirth")
        && let Some(place_of_birth) = actor.place_of_birth
    {
        object.insert("PlaceOfBirth".to_owned(), json!(place_of_birth));
    }
    if include("ProviderIds") {
        object.insert("ProviderIds".to_owned(), json!(actor.provider_ids));
    }
    if include("Genres") {
        object.insert("Genres".to_owned(), json!(actor.genres));
    }
    if include("Tags") {
        object.insert("Tags".to_owned(), json!(actor.tags));
    }
    if include("ProductionLocations") {
        object.insert(
            "ProductionLocations".to_owned(),
            json!(actor.production_locations),
        );
    }
    if include("PremiereDate")
        && let Some(premiere_date) = actor.premiere_date
    {
        object.insert("PremiereDate".to_owned(), json!(premiere_date));
    }
    if include("ProductionYear")
        && let Some(production_year) = actor.production_year
    {
        object.insert("ProductionYear".to_owned(), json!(production_year));
    }
    if include("Taglines") {
        object.insert("Taglines".to_owned(), json!(actor.taglines));
    }
    if include("DateCreated")
        && let Some(date_created) = actor.date_created.and_then(emby_timestamp)
    {
        object.insert("DateCreated".to_owned(), Value::String(date_created));
    }
    Value::Object(object)
}

pub(super) fn emby_stable_named_id(kind: &str, name: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"lux-emby:");
    digest.update(kind.as_bytes());
    digest.update(b":");
    digest.update(name.as_bytes());
    let suffix = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{kind}-{suffix}")
}

pub(super) fn emby_nfo_crew_json(nfo: &LocalNfoDetails) -> Vec<Value> {
    let mut people = Vec::with_capacity(nfo.directors.len() + nfo.writers.len());
    for (person_type, credits) in [("Director", &nfo.directors), ("Writer", &nfo.writers)] {
        for credit in credits {
            let person = json!({
                "Name": credit.name,
                "Id": if credit.provider_id.is_empty() {
                    emby_stable_named_id(person_type, &credit.name)
                } else {
                    credit.provider_id.clone()
                },
                "Type": person_type,
            });
            people.push(person);
        }
    }
    people
}

pub(super) async fn read_local_nfo_details(
    state: &AppState,
    item_id: &str,
) -> Option<LocalNfoDetails> {
    let store = state.local_nfo.as_ref()?;
    match store.read_item(item_id).await {
        Ok(details) => details,
        Err(error) => {
            tracing::warn!(
                item_id,
                %error,
                "derived local NFO cache is unavailable for Emby response"
            );
            None
        }
    }
}

pub(super) fn emby_nfo_fields_requested(fields: Option<&str>) -> bool {
    const NFO_FIELDS: [&str; 16] = [
        "CommunityRating",
        "PremiereDate",
        "EndDate",
        "RunTimeTicks",
        "OriginalLanguage",
        "Status",
        "OfficialRating",
        "ProviderIds",
        "Taglines",
        "Genres",
        "GenreItems",
        "Studios",
        "RemoteTrailers",
        "ExternalUrls",
        "HomePageUrl",
        "People",
    ];
    fields.is_none_or(|fields| {
        NFO_FIELDS
            .iter()
            .any(|field| emby_fields_include(Some(fields), field))
    })
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct EmbyItemDetailWorkPlan {
    pub(super) populate_image_tags: bool,
    pub(super) read_nfo: bool,
    pub(super) read_primary_image_aspect_ratio: bool,
    pub(super) read_people: bool,
}

pub(super) fn emby_item_detail_work_plan(fields: Option<&str>) -> EmbyItemDetailWorkPlan {
    // ShareLevel is a compatibility hint used by Filmly rather than a real
    // field projection. Keep its existing full-detail behavior here too, so
    // callers that pass the raw query cannot accidentally get a partial DTO.
    let normalized_fields = emby_detail_fields(fields);
    let fields = normalized_fields.as_deref();
    let lightweight_media_source_lookup =
        fields.is_some_and(is_lightweight_media_source_lookup_fields);
    EmbyItemDetailWorkPlan {
        populate_image_tags: !lightweight_media_source_lookup
            && (fields.is_none()
                || emby_fields_include(fields, "ImageTags")
                || emby_fields_include(fields, "BackdropImageTags")
                || emby_fields_include(fields, "PrimaryImageItemId")),
        read_nfo: !lightweight_media_source_lookup && emby_nfo_fields_requested(fields),
        read_primary_image_aspect_ratio: !lightweight_media_source_lookup
            && (fields.is_none() || emby_fields_include(fields, "PrimaryImageAspectRatio")),
        // Existing Emby detail responses include People even when the caller's
        // field list omits it. Preserve that compatibility behavior except for
        // the narrowly-scoped Redia media-source lookup.
        read_people: !lightweight_media_source_lookup,
    }
}

pub(super) fn is_lightweight_media_source_lookup_fields(fields: &str) -> bool {
    let mut has_media_sources = false;
    for field in fields
        .split(',')
        .map(str::trim)
        .filter(|field| !field.is_empty())
    {
        match field.to_ascii_lowercase().as_str() {
            "mediasources" => has_media_sources = true,
            "path" => {}
            _ => return false,
        }
    }
    has_media_sources
}

pub(super) fn emby_person_image_tag(person_id: &str) -> String {
    Sha256::digest(person_id.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn emby_page_params(query: &EmbyItemsQuery) -> Result<(i64, i64), StatusCode> {
    let offset = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(50);
    if offset < 0 || !(1..=100).contains(&limit) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok((offset, limit))
}

pub(super) fn emby_person_page_params(query: &EmbyPersonsQuery) -> Result<(i64, i64), StatusCode> {
    let offset = query.start_index.unwrap_or(0);
    let limit = query.limit.unwrap_or(50);
    if offset < 0 || limit < 1 {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok((offset, limit))
}

pub(super) fn emby_person_sort(value: Option<&str>) -> Result<PersonSort, StatusCode> {
    match value
        .unwrap_or("Name")
        .split(',')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Name")
        .to_ascii_lowercase()
        .as_str()
    {
        "name" => Ok(PersonSort::Name),
        "datecreated" => Ok(PersonSort::DateCreated),
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

pub(super) fn emby_person_sort_order(value: Option<&str>) -> Result<bool, StatusCode> {
    match value
        .unwrap_or("Ascending")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "ascending" => Ok(false),
        "descending" => Ok(true),
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

pub(super) fn emby_person_type_filter(person_types: Option<&str>) -> Option<&'static str> {
    let mut requested = person_types
        .unwrap_or("Actor")
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty());
    requested
        .find(|value| value.eq_ignore_ascii_case("Actor"))
        .map(|_| "Actor")
}

pub(super) fn ensure_emby_user_scope(
    user: &UserRecord,
    requested_id: &str,
) -> Result<(), StatusCode> {
    let requested_id = requested_id
        .parse::<crate::domain::ids::UserId>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if user.is_admin || user.id == requested_id {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

pub(super) fn emby_catalog_item_json_with_state(
    item: &CatalogItem,
    server_id: &str,
    user_state: Option<&crate::storage::StoredUserItemState>,
    nfo: Option<&LocalNfoDetails>,
    can_download: bool,
    fields: Option<&str>,
    unplayed_item_count: Option<i64>,
) -> Value {
    emby_catalog_item_json_with_state_and_aspect_ratio(
        item,
        server_id,
        user_state,
        EmbyItemJsonOptions {
            nfo,
            can_download,
            fields,
            primary_image_aspect_ratio: None,
            include_top_level_media_streams: false,
            unplayed_item_count,
        },
    )
}

pub(super) struct EmbyItemJsonOptions<'a> {
    nfo: Option<&'a LocalNfoDetails>,
    can_download: bool,
    fields: Option<&'a str>,
    primary_image_aspect_ratio: Option<f64>,
    include_top_level_media_streams: bool,
    unplayed_item_count: Option<i64>,
}

pub(super) fn emby_catalog_item_json_with_state_and_aspect_ratio(
    item: &CatalogItem,
    server_id: &str,
    user_state: Option<&crate::storage::StoredUserItemState>,
    options: EmbyItemJsonOptions<'_>,
) -> Value {
    let EmbyItemJsonOptions {
        nfo,
        can_download,
        fields,
        primary_image_aspect_ratio,
        include_top_level_media_streams,
        unplayed_item_count,
    } = options;
    let default_source = item
        .media_sources
        .iter()
        .find(|source| source.is_default)
        .or_else(|| item.media_sources.first());
    let runtime_ticks = item
        .runtime_ticks
        .or_else(|| default_source.and_then(|source| source.duration_ticks));
    let played_percentage = user_state.and_then(|state| {
        if state.position_ticks <= 0 {
            return None;
        }
        let runtime_ticks = runtime_ticks.filter(|value| *value > 0)?;
        Some((state.position_ticks.max(0) as f64 * 100.0 / runtime_ticks as f64).clamp(0.0, 100.0))
    });
    let mut image_tags = serde_json::Map::new();
    if let Some(tag) = item.poster_image_tag.as_ref() {
        image_tags.insert("Primary".to_owned(), json!(tag));
    } else if item.item_type == "EPISODE"
        && let Some(tag) = item.thumb_image_tag.as_ref()
    {
        // Filmly requests episode thumbnails through the standard Primary
        // image tag. A local Kodi-style `-thumb` image is the episode's
        // primary artwork when no dedicated poster exists.
        image_tags.insert("Primary".to_owned(), json!(tag));
    }
    if let Some(tag) = item.logo_image_tag.as_ref() {
        image_tags.insert("Logo".to_owned(), json!(tag));
    }
    if let Some(tag) = item.thumb_image_tag.as_ref() {
        image_tags.insert("Thumb".to_owned(), json!(tag));
    }
    if let Some(tag) = item.banner_image_tag.as_ref() {
        image_tags.insert("Banner".to_owned(), json!(tag));
    }
    if let Some(tag) = item.disc_image_tag.as_ref() {
        image_tags.insert("Disc".to_owned(), json!(tag));
    }
    if let Some(tag) = item.art_image_tag.as_ref() {
        image_tags.insert("Art".to_owned(), json!(tag));
    }
    if let Some(tag) = item.wallpaper_image_tag.as_ref() {
        image_tags.insert("Wallpaper".to_owned(), json!(tag));
    }
    let parent_id = item.parent_id.as_deref().unwrap_or(&item.library_id);
    let child_count = match item.item_type.as_str() {
        "SERIES" => item.season_count,
        "SEASON" => item.episode_count,
        _ => None,
    };
    let recursive_item_count = match item.item_type.as_str() {
        "SERIES" | "SEASON" => item.episode_count,
        _ => None,
    };
    let index_number = if item.item_type == "SEASON" {
        item.season_number
    } else {
        item.episode_number
    };
    let season_id = if item.item_type == "EPISODE" {
        item.parent_id.clone()
    } else {
        None
    };
    // Emby always exposes the series relationship on season and episode DTOs.
    // Filmly uses the season's SeriesId to construct the subsequent Episodes URL.
    let series_id = item
        .series_id
        .clone()
        .or_else(|| (item.item_type == "SEASON").then(|| item.parent_id.clone())?);
    let season_name = (item.item_type == "SEASON").then(|| item.title.clone());
    let episode_season_name = (item.item_type == "EPISODE")
        .then_some(item.season_number)
        .flatten()
        .map(|number| format!("Season {number:02}"));
    let is_folder = matches!(
        item.item_type.as_str(),
        "SERIES" | "SEASON" | "BOX_SET" | "FOLDER"
    );
    // Emby advertises sync capability on item details for playable media and
    // series containers. The list projection intentionally keeps the legacy
    // compact capability shape used by existing clients.
    let basic_sync_requested =
        fields.is_some_and(|fields| emby_fields_include(Some(fields), "BasicSyncInfo"));
    let supports_sync = (!is_folder || item.item_type == "SERIES")
        && (include_top_level_media_streams
            || (basic_sync_requested && item.item_type == "EPISODE"));
    let mut user_data = serde_json::Map::from_iter([
        (
            "PlaybackPositionTicks".to_owned(),
            json!(
                user_state
                    .map(|state| state.position_ticks)
                    .unwrap_or_default()
            ),
        ),
        (
            "PlayCount".to_owned(),
            json!(user_state.map(|state| state.play_count).unwrap_or_default()),
        ),
        (
            "IsFavorite".to_owned(),
            json!(user_state.map(|state| state.is_favorite).unwrap_or(false)),
        ),
        (
            "Played".to_owned(),
            json!(user_state.map(|state| state.is_played).unwrap_or(false)),
        ),
    ]);
    if let Some(played_percentage) = played_percentage {
        user_data.insert("PlayedPercentage".to_owned(), json!(played_percentage));
    }
    if let Some(last_played_at) = user_state.and_then(|state| state.last_played_at) {
        if let Some(last_played_date) = emby_timestamp(last_played_at) {
            user_data.insert("LastPlayedDate".to_owned(), json!(last_played_date));
        }
    }
    if let Some(unplayed_item_count) = unplayed_item_count
        && matches!(item.item_type.as_str(), "SERIES" | "SEASON")
    {
        user_data.insert("UnplayedItemCount".to_owned(), json!(unplayed_item_count));
    }

    let mut object = serde_json::Map::from_iter([
        ("Name".to_owned(), json!(item.title)),
        ("Id".to_owned(), json!(item.id)),
        ("ServerId".to_owned(), json!(server_id)),
        ("Type".to_owned(), json!(emby_item_type(&item.item_type))),
        ("MediaType".to_owned(), json!("Video")),
        (
            "IsFolder".to_owned(),
            json!(matches!(
                item.item_type.as_str(),
                "SERIES" | "SEASON" | "BOX_SET" | "FOLDER"
            )),
        ),
        ("ParentId".to_owned(), json!(parent_id)),
        ("ImageTags".to_owned(), Value::Object(image_tags)),
        (
            "BackdropImageTags".to_owned(),
            if item.fanart_image_tags.is_empty() {
                item.fanart_image_tag
                    .as_ref()
                    .map(|tag| json!([tag]))
                    .unwrap_or_else(|| json!([]))
            } else {
                json!(item.fanart_image_tags)
            },
        ),
        ("UserData".to_owned(), Value::Object(user_data)),
    ]);

    if fields.is_none() {
        object.extend([
            ("SortName".to_owned(), json!(item.sort_title)),
            ("ForcedSortName".to_owned(), json!(item.sort_title)),
            (
                "OriginalTitle".to_owned(),
                json!(item.original_title.clone().unwrap_or_default()),
            ),
            ("SupportsSync".to_owned(), json!(supports_sync)),
            ("CanDelete".to_owned(), json!(false)),
            ("LockData".to_owned(), json!(false)),
            ("LockedFields".to_owned(), json!([])),
            ("ExternalUrls".to_owned(), json!([])),
            ("RemoteTrailers".to_owned(), json!([])),
            ("Taglines".to_owned(), json!([])),
            ("Genres".to_owned(), json!([])),
            ("GenreItems".to_owned(), json!([])),
            ("Studios".to_owned(), json!([])),
            ("TagItems".to_owned(), json!([])),
            ("LocalTrailerCount".to_owned(), json!(0)),
            ("Etag".to_owned(), json!(emby_item_etag(&item.id))),
            ("DisplayPreferencesId".to_owned(), json!(item.id)),
            ("PresentationUniqueKey".to_owned(), json!(item.id)),
            (
                "ParentBackdropImageTags".to_owned(),
                json!(item.series_fanart_image_tags),
            ),
            (
                "ProviderIds".to_owned(),
                json!(emby_provider_ids(&item.provider_ids)),
            ),
            ("CanDownload".to_owned(), json!(can_download && !is_folder)),
        ]);
        // Emby always emits the item's creation/modification timestamps and a
        // filesystem path on detail DTOs. Lux ids are UUIDv7, so the embedded
        // timestamp is the real item creation time; the path is a stable,
        // harmless label because Lux never reveals real local paths.
        if let Some(created) = emby_item_timestamp(&item.id) {
            object.insert("DateCreated".to_owned(), json!(created));
            object.insert("DateModified".to_owned(), json!(created));
        }
        object.insert(
            "Path".to_owned(),
            json!(emby_safe_path(item, default_source)),
        );
        if matches!(item.item_type.as_str(), "MOVIE" | "SERIES") {
            object.insert("OfficialRating".to_owned(), json!(""));
        }
        if item.item_type == "SERIES" {
            object.extend([
                ("AirDays".to_owned(), json!([])),
                ("DisplayOrder".to_owned(), json!("Aired")),
            ]);
            emby_insert_optional(&mut object, "Status", item.status.clone().map(Value::from));
        }
        if let Some(file_name) = emby_file_name(item, default_source) {
            object.insert("FileName".to_owned(), json!(file_name));
        }
        if !is_folder {
            emby_insert_optional(&mut object, "PartCount", default_source.map(|_| json!(1)));
            emby_insert_optional(
                &mut object,
                "Container",
                default_source
                    .and_then(|source| source.container.clone())
                    .map(Value::from),
            );
            emby_insert_optional(
                &mut object,
                "Size",
                default_source
                    .and_then(|source| source.size)
                    .map(Value::from),
            );
            emby_insert_optional(
                &mut object,
                "Bitrate",
                default_source
                    .and_then(|source| source.bitrate)
                    .map(Value::from),
            );
            if let Some(width) = emby_video_stream_dimension(default_source, "Width") {
                object.insert("Width".to_owned(), json!(width));
            }
            if let Some(height) = emby_video_stream_dimension(default_source, "Height") {
                object.insert("Height".to_owned(), json!(height));
            }
        }
        emby_insert_optional(
            &mut object,
            "CollectionType",
            (item.item_type == "BOX_SET").then(|| json!("movies")),
        );
        emby_insert_optional(
            &mut object,
            "PrimaryImageItemId",
            item.poster_image_tag
                .as_ref()
                .map(|_| json!(item.id.clone())),
        );
        emby_insert_optional(
            &mut object,
            "SeriesId",
            series_id.as_ref().map(|value| json!(value)),
        );
        emby_insert_optional(
            &mut object,
            "SeriesName",
            item.series_name.clone().map(Value::from),
        );
        emby_insert_optional(
            &mut object,
            "SeriesPrimaryImageTag",
            item.series_primary_image_tag.clone().map(Value::from),
        );
        let episode_season_name = (item.item_type == "EPISODE")
            .then_some(item.season_number)
            .flatten()
            .map(|number| format!("Season {number:02}"));
        emby_insert_optional(
            &mut object,
            "SeasonName",
            season_name.or(episode_season_name).map(Value::from),
        );
        emby_insert_optional(
            &mut object,
            "ParentLogoItemId",
            series_id.as_ref().map(|value| json!(value)),
        );
        emby_insert_optional(
            &mut object,
            "ParentBackdropItemId",
            series_id.as_ref().map(|value| json!(value)),
        );
        emby_insert_optional(
            &mut object,
            "SeasonId",
            season_id.as_ref().map(|value| json!(value)),
        );
        emby_insert_optional(&mut object, "IndexNumber", index_number.map(Value::from));
        emby_insert_optional(
            &mut object,
            "ParentIndexNumber",
            item.season_number.map(Value::from),
        );
        emby_insert_optional(&mut object, "Index", item.episode_number.map(Value::from));
        emby_insert_optional(
            &mut object,
            "ProductionYear",
            item.production_year.map(Value::from),
        );
        emby_insert_optional(
            &mut object,
            "PremiereDate",
            emby_datetime(item.premiere_date.as_deref()),
        );
        emby_insert_optional(&mut object, "CommunityRating", item.rating.map(Value::from));
        emby_insert_optional(
            &mut object,
            "Overview",
            item.overview.clone().map(Value::from),
        );
        emby_insert_optional(&mut object, "RunTimeTicks", runtime_ticks.map(Value::from));
        emby_insert_optional(&mut object, "ChildCount", child_count.map(Value::from));
        emby_insert_optional(
            &mut object,
            "RecursiveItemCount",
            recursive_item_count.map(Value::from),
        );
        if item.item_type == "EPISODE" {
            emby_insert_optional(
                &mut object,
                "ParentLogoImageTag",
                item.series_logo_image_tag.clone().map(Value::from),
            );
            emby_insert_optional(
                &mut object,
                "ParentThumbImageTag",
                item.series_thumb_image_tag.clone().map(Value::from),
            );
            emby_insert_optional(
                &mut object,
                "ParentThumbItemId",
                series_id.as_ref().map(|value| json!(value)),
            );
        }
        if let Some(aspect_ratio) = primary_image_aspect_ratio {
            object.insert("PrimaryImageAspectRatio".to_owned(), json!(aspect_ratio));
        }
    } else {
        if matches!(item.item_type.as_str(), "SEASON" | "EPISODE") {
            emby_insert_optional(
                &mut object,
                "SeriesId",
                series_id.as_ref().map(|value| json!(value)),
            );
            emby_insert_optional(
                &mut object,
                "SeriesName",
                item.series_name.clone().map(|value| json!(value)),
            );
            emby_insert_optional(
                &mut object,
                "SeriesPrimaryImageTag",
                item.series_primary_image_tag
                    .clone()
                    .map(|value| json!(value)),
            );
            emby_insert_optional(
                &mut object,
                "SeasonName",
                season_name
                    .clone()
                    .or_else(|| episode_season_name.clone())
                    .map(|value| json!(value)),
            );
            emby_insert_optional(
                &mut object,
                "ParentLogoItemId",
                series_id.clone().map(|value| json!(value)),
            );
            emby_insert_optional(
                &mut object,
                "ParentBackdropItemId",
                series_id.clone().map(|value| json!(value)),
            );
            object.insert(
                "ParentBackdropImageTags".to_owned(),
                json!(item.series_fanart_image_tags),
            );
            emby_insert_optional(
                &mut object,
                "IndexNumber",
                index_number.map(|value| json!(value)),
            );
            emby_insert_optional(
                &mut object,
                "ChildCount",
                child_count.map(|value| json!(value)),
            );
            if item.item_type == "EPISODE" {
                emby_insert_optional(
                    &mut object,
                    "SeasonId",
                    season_id.clone().map(|value| json!(value)),
                );
                emby_insert_optional(
                    &mut object,
                    "ParentIndexNumber",
                    item.season_number.map(|value| json!(value)),
                );
                emby_insert_optional(
                    &mut object,
                    "Index",
                    item.episode_number.map(|value| json!(value)),
                );
            }
        }
        if emby_fields_include(fields, "BasicSyncInfo") {
            object.insert("SupportsSync".to_owned(), json!(supports_sync));
        }
        if emby_fields_include(fields, "DateModified") {
            if let Some(modified) = emby_item_timestamp(&item.id) {
                object.insert("DateModified".to_owned(), json!(modified));
            }
        }
        if emby_fields_include(fields, "Path") {
            object.insert(
                "Path".to_owned(),
                json!(emby_safe_path(item, default_source)),
            );
        }
        if emby_fields_include(fields, "CanDownload") {
            object.insert("CanDownload".to_owned(), json!(can_download && !is_folder));
        }
        if emby_fields_include(fields, "Overview") {
            emby_insert_optional(
                &mut object,
                "Overview",
                item.overview.clone().map(Value::from),
            );
        }
        if emby_fields_include(fields, "PremiereDate")
            || (item.item_type == "EPISODE" && emby_fields_include(fields, "MediaSources"))
        {
            emby_insert_optional(
                &mut object,
                "PremiereDate",
                emby_datetime(item.premiere_date.as_deref()),
            );
        }
        if emby_fields_include(fields, "ProviderIds") {
            object.insert(
                "ProviderIds".to_owned(),
                json!(emby_provider_ids(&item.provider_ids)),
            );
        }
        if emby_fields_include(fields, "People") {
            // Catalog pages do not load the potentially large people snapshot;
            // preserve Emby's non-null collection contract for clients that
            // map this field eagerly. Full item details add the populated list.
            object.insert("People".to_owned(), json!([]));
        }
        if emby_fields_include(fields, "Genres") {
            object.insert("Genres".to_owned(), json!([]));
            object.insert("GenreItems".to_owned(), json!([]));
        } else if emby_fields_include(fields, "GenreItems") {
            object.insert("GenreItems".to_owned(), json!([]));
        }
        if emby_fields_include(fields, "ProductionYear") {
            emby_insert_optional(
                &mut object,
                "ProductionYear",
                item.production_year.map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "PremiereDate") {
            emby_insert_optional(
                &mut object,
                "PremiereDate",
                emby_datetime(item.premiere_date.as_deref()),
            );
        }
        if emby_fields_include(fields, "CommunityRating") {
            emby_insert_optional(
                &mut object,
                "CommunityRating",
                item.rating.map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "RunTimeTicks") {
            emby_insert_optional(
                &mut object,
                "RunTimeTicks",
                runtime_ticks.map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "ChildCount") {
            emby_insert_optional(
                &mut object,
                "ChildCount",
                child_count.map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "RecursiveItemCount") {
            emby_insert_optional(
                &mut object,
                "RecursiveItemCount",
                recursive_item_count.map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "Container") || emby_fields_include(fields, "MediaSources") {
            emby_insert_optional(
                &mut object,
                "Container",
                default_source
                    .and_then(|source| source.container.clone())
                    .map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "Size") {
            emby_insert_optional(
                &mut object,
                "Size",
                default_source
                    .and_then(|source| source.size)
                    .map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "Bitrate") || emby_fields_include(fields, "MediaSources") {
            emby_insert_optional(
                &mut object,
                "Bitrate",
                default_source
                    .and_then(|source| source.bitrate)
                    .map(|value| json!(value)),
            );
        }
        if item.item_type == "EPISODE" {
            emby_insert_optional(
                &mut object,
                "ParentLogoImageTag",
                item.series_logo_image_tag.clone().map(Value::from),
            );
            emby_insert_optional(
                &mut object,
                "ParentThumbImageTag",
                item.series_thumb_image_tag.clone().map(Value::from),
            );
            emby_insert_optional(
                &mut object,
                "ParentThumbItemId",
                series_id.as_ref().map(|value| json!(value)),
            );
        }
        if emby_fields_include(fields, "PrimaryImageAspectRatio") {
            emby_insert_optional(
                &mut object,
                "PrimaryImageAspectRatio",
                primary_image_aspect_ratio.map(|value| json!(value)),
            );
        }
    }
    let mut value = Value::Object(object);
    let include_media_streams = !is_folder
        && (fields.is_none()
            || emby_fields_include(fields, "MediaStreams")
            || emby_fields_include(fields, "MediaSources"));
    if !is_folder
        && emby_fields_include(fields, "MediaSources")
        && let Value::Object(object) = &mut value
    {
        object.insert(
            "MediaSources".to_owned(),
            Value::Array(
                item.media_sources
                    .iter()
                    .map(|source| {
                        emby_media_source_json_with_resolver_and_chapters(
                            &item.id,
                            source,
                            include_media_streams,
                            false,
                            emby_fields_include(fields, "Chapters"),
                        )
                    })
                    .collect(),
            ),
        );
    }
    if !is_folder
        && include_top_level_media_streams
        && let Value::Object(object) = &mut value
    {
        object.insert(
            "MediaStreams".to_owned(),
            Value::Array(
                default_source
                    .map(|source| source.streams.iter().map(emby_media_stream_json).collect())
                    .unwrap_or_default(),
            ),
        );
    }
    if !is_folder
        && emby_fields_include(fields, "Chapters")
        && let Value::Object(object) = &mut value
    {
        object.insert(
            "Chapters".to_owned(),
            Value::Array(
                default_source
                    .map(|source| source.chapters.iter().map(emby_chapter_json).collect())
                    .unwrap_or_default(),
            ),
        );
    }
    apply_emby_nfo_details(&mut value, item, nfo, fields);
    value
}

pub(super) fn apply_emby_nfo_details(
    value: &mut Value,
    item: &CatalogItem,
    nfo: Option<&LocalNfoDetails>,
    fields: Option<&str>,
) {
    let Some(nfo) = nfo else {
        return;
    };
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let include = |field: &str| emby_fields_include(fields, field);

    if include("CommunityRating")
        && let Some(rating) = nfo.rating
    {
        object.insert("CommunityRating".to_owned(), json!(rating));
    }
    if include("PremiereDate")
        && let Some(date) = nfo
            .premiered
            .as_deref()
            .or(nfo.release_date.as_deref())
            .or(nfo.aired.as_deref())
            .or(item.premiere_date.as_deref())
    {
        object.insert(
            "PremiereDate".to_owned(),
            emby_datetime(Some(date)).unwrap_or(Value::Null),
        );
    }
    if include("EndDate")
        && let Some(date) = nfo.last_air_date.as_deref()
    {
        object.insert(
            "EndDate".to_owned(),
            emby_datetime(Some(date)).unwrap_or(Value::Null),
        );
    }
    if include("RunTimeTicks")
        && let Some(runtime) = nfo.runtime
        && let Some(runtime_ticks) = i64::from(runtime)
            .checked_mul(60)
            .and_then(|value| value.checked_mul(10_000_000))
    {
        object.insert("RunTimeTicks".to_owned(), json!(runtime_ticks));
    }
    if include("OriginalLanguage")
        && let Some(language) = nfo.original_language.as_deref()
    {
        object.insert("OriginalLanguage".to_owned(), json!(language));
    }
    if include("Status")
        && let Some(status) = nfo.status.as_deref()
    {
        object.insert("Status".to_owned(), json!(status));
    }
    if include("OfficialRating")
        && let Some(certification) = nfo.certification.as_deref()
    {
        object.insert("OfficialRating".to_owned(), json!(certification));
    }
    if include("ProviderIds") {
        let mut provider_ids = item.provider_ids.clone();
        provider_ids.extend(nfo.provider_ids.clone());
        object.insert(
            "ProviderIds".to_owned(),
            json!(emby_provider_ids(&provider_ids)),
        );
    }
    if include("Taglines") && !nfo.tagline.as_deref().unwrap_or_default().is_empty() {
        object.insert("Taglines".to_owned(), json!([nfo.tagline]));
    }
    if include("Genres") && !nfo.genres.is_empty() {
        object.insert("Genres".to_owned(), json!(nfo.genres));
    }
    if include("GenreItems") && !nfo.genres.is_empty() {
        object.insert(
            "GenreItems".to_owned(),
            json!(
                nfo.genres
                    .iter()
                    .map(|name| {
                        json!({
                            "Name": name,
                            "Id": emby_stable_named_id("genre", name),
                        })
                    })
                    .collect::<Vec<_>>()
            ),
        );
    }
    if include("Studios") && !nfo.studios.is_empty() {
        object.insert(
            "Studios".to_owned(),
            json!(
                nfo.studios
                    .iter()
                    .map(|name| {
                        json!({
                            "Name": name,
                            "Id": emby_stable_named_id("studio", name),
                        })
                    })
                    .collect::<Vec<_>>()
            ),
        );
    }
    if include("RemoteTrailers") && !nfo.trailers.is_empty() {
        let trailers = nfo
            .trailers
            .iter()
            .enumerate()
            .map(|(index, url)| json!({ "Url": url, "Name": format!("Trailer {}", index + 1) }))
            .collect::<Vec<_>>();
        object.insert("RemoteTrailers".to_owned(), json!(trailers));
    }
    if (include("ExternalUrls") || include("HomePageUrl"))
        && let Some(website) = nfo.website.as_deref()
    {
        object.insert(
            "ExternalUrls".to_owned(),
            json!([{ "Name": "Website", "Url": website }]),
        );
    }
}

pub(super) fn emby_insert_optional(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<Value>,
) {
    if let Some(value) = value {
        object.insert(key.to_owned(), value);
    }
}

/// Stable, server-local identifier that mirrors Emby's per-item Etag. Emby uses
/// a content hash; Lux derives one from the item id so it stays stable across
/// requests without leaking library paths.
pub(super) fn emby_item_etag(item_id: &str) -> String {
    Sha256::digest(item_id.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Emby serializes timestamps as UTC with seven fractional digits, e.g.
/// `2026-03-29T17:51:26.0000000Z`. Lux stores unix seconds in user state.
pub(super) fn emby_timestamp(unix_seconds: i64) -> Option<String> {
    let datetime = time::OffsetDateTime::from_unix_timestamp(unix_seconds).ok()?;
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.0000000Z",
        datetime.year(),
        u8::from(datetime.month()),
        datetime.day(),
        datetime.hour(),
        datetime.minute(),
        datetime.second(),
    ))
}

/// Emby exposes a display filename on every item DTO. Lux does not store source
/// file names, so derive a stable, harmless label from the title and container.
pub(super) fn emby_file_name(
    item: &CatalogItem,
    default_source: Option<&CatalogSource>,
) -> Option<String> {
    if matches!(
        item.item_type.as_str(),
        "SERIES" | "SEASON" | "BOX_SET" | "FOLDER"
    ) {
        return Some(item.title.clone());
    }
    let container = default_source
        .and_then(|source| source.container.as_deref())
        .filter(|value| !value.is_empty())
        .unwrap_or("strm");
    Some(format!("{}.{}", item.title, container))
}

/// Emby always exposes a filesystem path on item DTOs. External proxies need
/// the original STRM target here to map playback, while ordinary local media
/// continues to use a stable, harmless path instead of revealing the real
/// filesystem location.
pub(super) fn emby_safe_path(item: &CatalogItem, default_source: Option<&CatalogSource>) -> String {
    if let Some(target) = default_source
        .filter(|source| source.source_kind == "STRM_URL")
        .and_then(|source| source.external_url.as_deref())
        .filter(|target| !target.is_empty())
    {
        return target.to_owned();
    }

    let title = &item.title;
    if matches!(
        item.item_type.as_str(),
        "SERIES" | "SEASON" | "BOX_SET" | "FOLDER"
    ) {
        return format!("/media/{}/{title}", item.library_id);
    }
    let container = default_source
        .and_then(|source| source.container.as_deref())
        .filter(|value| !value.is_empty())
        .unwrap_or("strm");
    format!("/media/{}/{title}.{container}", item.library_id)
}

/// Extracts the creation timestamp embedded in Lux's UUIDv7 item ids. The first
/// 48 bits of a v7 uuid are Unix milliseconds, which is exactly when Lux
/// generated the id for the media item. Non-v7 ids (imported/migrated data)
/// return None and the field is omitted instead of emitting a fabricated value.
pub(super) fn emby_item_timestamp(item_id: &str) -> Option<String> {
    let compact = item_id.replace('-', "");
    if compact.len() != 32 || compact.as_bytes().get(12).is_none_or(|byte| *byte != b'7') {
        return None;
    }
    let millis = u64::from_str_radix(&compact[..12], 16).ok()?;
    emby_timestamp(i64::try_from(millis / 1000).ok()?)
}

/// Reads a video stream dimension (Width or Height) from the default source's
/// probe details, matching Emby's per-item Width/Height fields.
pub(super) fn emby_video_stream_dimension(
    default_source: Option<&CatalogSource>,
    emby_key: &str,
) -> Option<i64> {
    let stream = default_source?
        .streams
        .iter()
        .find(|stream| stream.stream_type.eq_ignore_ascii_case("video"))?;
    let value = stream
        .details
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(emby_key))?
        .1;
    value.as_i64().or_else(|| {
        value
            .as_str()
            .and_then(|text| text.trim().parse::<i64>().ok())
    })
}

pub(super) async fn emby_primary_image_aspect_ratio(
    state: &AppState,
    principal: AccessPrincipal,
    item_id: &str,
) -> Option<f64> {
    if let Some((width, height)) = state
        .database
        .as_ref()?
        .find_primary_image_dimensions(item_id)
        .await
        .ok()?
    {
        if width > 0 && height > 0 {
            return Some(f64::from(width) / f64::from(height));
        }
    }

    let images = state.images.as_ref()?;
    let image = images
        .resolve(principal, item_id, "POSTER", 0)
        .await
        .ok()??;
    let dimensions = read_image_dimensions(&image.path).await?;
    let width = dimensions.0;
    let height = dimensions.1;
    if width <= 0 || height <= 0 {
        return None;
    }
    if let Some(database) = state.database.as_ref() {
        let _ = database
            .set_item_image_dimensions(item_id, "POSTER", 0, width, height)
            .await;
    }
    Some(f64::from(width) / f64::from(height))
}

pub(super) fn emby_datetime(value: Option<&str>) -> Option<Value> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    let value = if value.contains('T') {
        value.to_owned()
    } else {
        format!("{value}T00:00:00.0000000Z")
    };
    Some(json!(value))
}

pub(super) fn emby_provider_ids(
    provider_ids: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    provider_ids
        .iter()
        .map(|(name, value)| {
            let name = match name.to_ascii_lowercase().as_str() {
                "tmdb" => "Tmdb",
                "tvdb" => "Tvdb",
                "imdb" => "Imdb",
                _ => name,
            };
            (name.to_owned(), value.clone())
        })
        .collect()
}

pub(super) fn emby_media_source_json_with_resolver(
    item_id: &str,
    source: &crate::application::catalog::CatalogSource,
    include_media_streams: bool,
    strm_resolver_available: bool,
) -> Value {
    emby_media_source_json_with_resolver_and_chapters(
        item_id,
        source,
        include_media_streams,
        strm_resolver_available,
        true,
    )
}

pub(super) fn emby_media_source_json_with_resolver_and_chapters(
    item_id: &str,
    source: &crate::application::catalog::CatalogSource,
    include_media_streams: bool,
    strm_resolver_available: bool,
    include_chapters: bool,
) -> Value {
    let strm_target_kind = (source.source_kind == "STRM_URL").then(|| {
        source
            .external_url
            .as_deref()
            .map(classify_strm_target)
            .map_or(StrmTargetKind::Empty, |target| target.kind)
    });
    // External Emby proxies consume the raw Path for both URL and path STRM
    // targets. Keep their wire representation identical while preserving the
    // URL resolver as Lux's direct-play fallback when no proxy takes over.
    let is_proxy_compatible_strm_target = matches!(
        strm_target_kind,
        Some(StrmTargetKind::Url | StrmTargetKind::Path)
    );
    let is_resolver_target = strm_resolver_available
        && matches!(
            strm_target_kind,
            Some(StrmTargetKind::Smb | StrmTargetKind::Ftp)
        );
    let direct_stream_url = if source.source_kind == "LOCAL_FILE"
        || is_proxy_compatible_strm_target
        || is_resolver_target
    {
        Some(emby_media_source_stream_url(item_id, source))
    } else {
        None
    };
    let is_remote_playback = is_resolver_target;
    let is_playable =
        source.source_kind == "LOCAL_FILE" || is_proxy_compatible_strm_target || is_remote_playback;
    let default_audio_stream_index = source
        .streams
        .iter()
        .find(|stream| stream.stream_type == "AUDIO" && stream.is_default)
        .or_else(|| {
            source
                .streams
                .iter()
                .find(|stream| stream.stream_type == "AUDIO")
        })
        .map(|stream| stream.index)
        .unwrap_or(-1);
    let mut value = json!({
        "Id": source.id,
        "ItemId": item_id,
        "Name": source.edition_name,
        "Edition": source.edition_name,
        "Quality": source.quality_label,
        "VideoType": source.quality_label,
        "Container": source.container,
        "Size": source.size,
        "Bitrate": source.bitrate,
        "RunTimeTicks": source.duration_ticks,
        "Path": source.external_url,
        "IsDefault": source.is_default,
        "Protocol": if is_remote_playback { "Http" } else { "File" },
        "Type": "Default",
        "IsRemote": is_remote_playback,
        "SupportsDirectPlay": is_playable,
        "SupportsDirectStream": is_playable,
        "SupportsProbing": !source.probe_status.eq_ignore_ascii_case("FAILED"),
        "SupportsTranscoding": false,
        "DirectStreamUrl": direct_stream_url,
        // Android clients deserialize this compatibility field as a number,
        // even while a source is waiting for media probing and has no audio
        // stream yet. Keep the wire type numeric without selecting a video
        // stream as audio.
        "DefaultAudioStreamIndex": default_audio_stream_index,
        "Formats": [],
        "HasMixedProtocols": false,
        "IsInfiniteStream": false,
        "ReadAtNativeFramerate": false,
        "RequiredHttpHeaders": {},
        "RequiresClosing": false,
        "RequiresOpening": false,
        "RequiresLooping": false,
        // Some Android clients use an independent media request stack and
        // drop the Emby auth headers sent to PlaybackInfo. This standard Emby
        // flag tells them to append the current API key to the direct URL,
        // while keeping the long-lived token out of the response URL itself.
        "AddApiKeyToDirectStreamUrl": true,
    });
    if include_chapters && let Value::Object(object) = &mut value {
        object.insert(
            "Chapters".to_owned(),
            Value::Array(source.chapters.iter().map(emby_chapter_json).collect()),
        );
    }
    if include_media_streams && let Value::Object(object) = &mut value {
        object.insert(
            "MediaStreams".to_owned(),
            Value::Array(source.streams.iter().map(emby_media_stream_json).collect()),
        );
    }
    value
}

pub(super) fn emby_source_needs_strm_resolver(
    source: &crate::application::catalog::CatalogSource,
) -> bool {
    source.source_kind == "STRM_URL"
        && source.external_url.as_deref().is_some_and(|target| {
            matches!(
                classify_strm_target(target).kind,
                StrmTargetKind::Smb | StrmTargetKind::Ftp
            )
        })
}

pub(super) fn emby_media_source_stream_url(
    item_id: &str,
    source: &crate::application::catalog::CatalogSource,
) -> String {
    emby_media_source_stream_url_parts(
        item_id,
        &source.id,
        &source.source_kind,
        source.container.as_deref(),
    )
}

pub(super) fn emby_media_source_stream_url_parts(
    item_id: &str,
    source_id: &str,
    source_kind: &str,
    container: Option<&str>,
) -> String {
    let stream_suffix = container
        .filter(|container| !(source_kind == "STRM_URL" && container.eq_ignore_ascii_case("strm")))
        .map(|container| format!(".{container}"))
        .unwrap_or_default();
    format!("/Videos/{item_id}/stream{stream_suffix}?MediaSourceId={source_id}")
}

pub(super) fn emby_signed_direct_stream_url(
    service: &WebPlaybackSessionService,
    item_id: &str,
    source: &crate::application::catalog::CatalogSource,
    user: &UserRecord,
) -> Option<String> {
    let expires_at = current_unix_timestamp().saturating_add(EMBY_DIRECT_STREAM_TTL_SECONDS);
    let user_id = user.id.to_string();
    let signature = service.sign_emby_direct_stream(
        &user_id,
        user.is_admin,
        item_id,
        &source.id,
        expires_at,
    )?;
    let mut url = emby_media_source_stream_url(item_id, source);
    url.push_str("&luxPlaybackUserId=");
    url.push_str(&percent_encode_filename(&user_id));
    url.push_str("&luxPlaybackAdmin=");
    url.push_str(if user.is_admin { "1" } else { "0" });
    url.push_str("&luxPlaybackExpires=");
    url.push_str(&expires_at.to_string());
    url.push_str("&luxPlaybackSignature=");
    url.push_str(&percent_encode_filename(&signature));
    Some(url)
}

pub(super) fn emby_chapter_json(chapter: &crate::application::catalog::CatalogChapter) -> Value {
    let mut value = json!({
        "StartPositionTicks": chapter.start_position_ticks,
        "MarkerType": match chapter.marker_type.as_str() {
            "INTRO_START" => "IntroStart",
            "INTRO_END" => "IntroEnd",
            "CREDITS_START" => "CreditsStart",
            _ => "Chapter",
        },
        "ChapterIndex": chapter.chapter_index,
    });
    if let Some(name) = chapter.name.as_deref().filter(|name| !name.is_empty())
        && let Value::Object(object) = &mut value
    {
        object.insert("Name".to_owned(), json!(name));
    }
    value
}

pub(super) fn is_http_strm_target(value: &str) -> bool {
    matches!(classify_strm_target(value).kind, StrmTargetKind::Url)
}

pub(super) fn normalize_strm_http_location(value: &str) -> Option<HeaderValue> {
    if value.is_ascii() {
        return HeaderValue::from_str(value).ok();
    }
    if !is_http_strm_target(value) {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    HeaderValue::from_str(url.as_str()).ok()
}

pub(super) fn emby_media_stream_json(stream: &crate::application::catalog::CatalogStream) -> Value {
    let language = stream
        .language
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("und");
    let display_title = stream
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(emby_stream_type(&stream.stream_type));
    let mut value = json!({
        "Index": stream.index,
        "Type": emby_stream_type(&stream.stream_type),
        "Codec": stream.codec,
        "Language": language,
        "DisplayTitle": display_title,
        "AttachmentSize": 0,
        "IsAnamorphic": false,
        "Protocol": if stream.is_external { "Http" } else { "File" },
        "SupportsExternalStream": stream.is_external,
        "IsExternal": stream.is_external,
        "IsDefault": stream.is_default,
        "IsForced": stream.is_forced,
    });
    if let Value::Object(object) = &mut value {
        for (key, detail) in &stream.details {
            let Some(detail) = normalize_emby_media_stream_detail(key, detail) else {
                continue;
            };
            object.entry(key.clone()).or_insert(detail);
        }
    }
    value
}

pub(super) fn normalize_emby_media_stream_detail(key: &str, value: &Value) -> Option<Value> {
    const INTEGER_FIELDS: [&str; 9] = [
        "BitRate",
        "BitDepth",
        "RefFrames",
        "Height",
        "Width",
        "Level",
        "Channels",
        "SampleRate",
        "AttachmentSize",
    ];
    const FLOAT_FIELDS: [&str; 2] = ["AverageFrameRate", "RealFrameRate"];
    const BOOLEAN_FIELDS: [&str; 3] = ["IsInterlaced", "IsHearingImpaired", "IsTextSubtitleStream"];

    if INTEGER_FIELDS
        .iter()
        .any(|field| key.eq_ignore_ascii_case(field))
    {
        return emby_integer_value(value);
    }
    if FLOAT_FIELDS
        .iter()
        .any(|field| key.eq_ignore_ascii_case(field))
    {
        return emby_frame_rate_value(value);
    }
    if BOOLEAN_FIELDS
        .iter()
        .any(|field| key.eq_ignore_ascii_case(field))
    {
        return emby_boolean_value(value);
    }
    (!value.is_null()).then(|| value.clone())
}

pub(super) fn emby_integer_value(value: &Value) -> Option<Value> {
    match value {
        Value::Number(value) if value.as_i64().is_some() => Some(Value::Number(value.clone())),
        Value::String(value) => value.trim().parse::<i64>().ok().map(Value::from),
        _ => None,
    }
}

pub(super) fn emby_frame_rate_value(value: &Value) -> Option<Value> {
    let number = match value {
        Value::Number(value) => value.as_f64()?,
        Value::String(value) => {
            let value = value.trim();
            if let Some((numerator, denominator)) = value.split_once('/') {
                let numerator = numerator.trim().parse::<f64>().ok()?;
                let denominator = denominator.trim().parse::<f64>().ok()?;
                if denominator == 0.0 {
                    return None;
                }
                numerator / denominator
            } else {
                value.parse::<f64>().ok()?
            }
        }
        _ => return None,
    };
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    if number.fract() == 0.0 && number <= i64::MAX as f64 {
        return Some(Value::from(number as i64));
    }
    serde_json::Number::from_f64(number).map(Value::Number)
}

pub(super) fn emby_boolean_value(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(value) => Some(Value::Bool(*value)),
        Value::Number(value) => value.as_i64().map(|value| Value::Bool(value != 0)),
        Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(Value::Bool(true)),
            "false" | "0" | "no" => Some(Value::Bool(false)),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn emby_library_view_json(
    library: &LibraryRecord,
    server_id: &str,
    child_count: i64,
) -> Value {
    json!({
        "Name": library.name,
        "SortName": library.name,
        "Id": library.id,
        "ServerId": server_id,
        "Type": "CollectionFolder",
        "IsFolder": true,
        "MediaType": "Video",
        "CollectionType": emby_collection_type(library.kind),
        "ChildCount": child_count,
        "RecursiveItemCount": child_count,
        "PrimaryImageItemId": library.cover_image_tag.as_ref().map(|_| library.id.to_string()),
        "PrimaryImageTag": library.cover_image_tag,
        "ImageTags": library
            .cover_image_tag
            .as_ref()
            .map(|tag| json!({"Primary": tag}))
            .unwrap_or_else(|| json!({})),
        "BackdropImageTags": [],
        "UserData": {
            "PlaybackPositionTicks": 0,
            "PlayCount": 0,
            "IsFavorite": false,
            "Played": false,
        },
    })
}

pub(super) fn emby_virtual_folder_json(
    view: &LibraryView,
    global_media_strategy: &MediaStrategySettings,
    resume_played_percent: i64,
    resume_min_ticks: i64,
) -> Value {
    let media_strategy = view
        .library
        .media_strategy_json
        .as_deref()
        .and_then(|value| serde_json::from_str::<MediaStrategySettings>(value).ok())
        .unwrap_or_else(|| global_media_strategy.clone());
    let collection_type = emby_collection_type(view.library.kind);
    json!({
        "Name": view.library.name,
        "Locations": view
            .roots
            .iter()
            .map(|root| root.display_path.to_string_lossy().to_string())
            .collect::<Vec<_>>(),
        "CollectionType": collection_type,
        "LibraryOptions": emby_virtual_folder_options_json(
            view,
            &media_strategy,
            resume_played_percent,
            resume_min_ticks,
        ),
        "Id": view.library.id,
        "Guid": view.library.id,
        "ItemId": view.library.id,
        "PrimaryImageItemId": view
            .library
            .cover_image_tag
            .as_ref()
            .map(|_| view.library.id),
        "RefreshProgress": null,
        "RefreshStatus": "Idle",
    })
}

pub(super) fn emby_virtual_folder_options_json(
    view: &LibraryView,
    media_strategy: &MediaStrategySettings,
    resume_played_percent: i64,
    resume_min_ticks: i64,
) -> Value {
    let collection_type = emby_collection_type(view.library.kind);
    let type_options = match view.library.kind {
        LibraryKind::Movie => vec![emby_library_type_options_json("Movie", media_strategy)],
        LibraryKind::Series => vec![emby_library_type_options_json("Series", media_strategy)],
        LibraryKind::Mixed => vec![
            emby_library_type_options_json("Movie", media_strategy),
            emby_library_type_options_json("Series", media_strategy),
        ],
    };
    json!({
        "EnableArchiveMediaFiles": false,
        "EnablePhotos": false,
        "EnableRealtimeMonitor": true,
        "EnableChapterImageExtraction": false,
        "ExtractChapterImagesDuringLibraryScan": false,
        "DownloadImagesInAdvance": false,
        "PathInfos": view.roots.iter().map(|root| json!({
            "Path": root.display_path.to_string_lossy().to_string(),
            "NetworkPath": "",
        })).collect::<Vec<_>>(),
        "SaveLocalMetadata": true,
        "SaveLocalThumbnailSets": false,
        "ImportMissingEpisodes": false,
        "EnableAutomaticSeriesGrouping": false,
        "EnableEmbeddedTitles": false,
        "EnableAudioResume": false,
        "AutomaticRefreshIntervalDays": 0,
        "PreferredMetadataLanguage": media_strategy.metadata_language,
        "ContentType": collection_type,
        "MetadataCountryCode": media_strategy.region,
        "SeasonZeroDisplayName": "Specials",
        "MetadataSavers": ["Nfo"],
        "DisabledLocalMetadataReaders": [],
        "LocalMetadataReaderOrder": ["Nfo"],
        "DisabledSubtitleFetchers": [],
        "SubtitleFetcherOrder": [],
        "SkipSubtitlesIfEmbeddedSubtitlesPresent": true,
        "SkipSubtitlesIfAudioTrackMatches": false,
        "SubtitleDownloadLanguages": media_strategy
            .subtitles
            .languages
            .iter()
            .map(|language| emby_subtitle_language_code(language))
            .collect::<Vec<_>>(),
        "RequirePerfectSubtitleMatch": false,
        "SaveSubtitlesWithMedia": false,
        "ForcedSubtitlesOnly": media_strategy.subtitles.forced_only,
        "TypeOptions": type_options,
        "CollapseSingleItemFolders": false,
        "MinResumePct": 0,
        "MaxResumePct": resume_played_percent,
        "MinResumeDurationSeconds": resume_min_ticks
            .max(0)
            .saturating_add(9_999_999)
            / 10_000_000,
        "ThumbnailImagesIntervalSeconds": 0,
    })
}

pub(super) fn emby_library_type_options_json(
    item_type: &str,
    media_strategy: &MediaStrategySettings,
) -> Value {
    let mut image_options = Vec::new();
    if media_strategy.images.poster {
        image_options.push(json!({
            "Type": "Primary",
            "Limit": 1,
            "MinWidth": media_strategy.images.min_download_width,
        }));
    }
    if media_strategy.images.artwork {
        image_options.push(json!({
            "Type": "Art",
            "Limit": 1,
            "MinWidth": media_strategy.images.min_download_width,
        }));
    }
    if media_strategy.images.banner {
        image_options.push(json!({
            "Type": "Banner",
            "Limit": 1,
            "MinWidth": media_strategy.images.min_download_width,
        }));
    }
    if media_strategy.images.logo {
        image_options.push(json!({
            "Type": "Logo",
            "Limit": 1,
            "MinWidth": media_strategy.images.min_download_width,
        }));
    }
    if media_strategy.images.thumbnail {
        image_options.push(json!({
            "Type": "Thumb",
            "Limit": 1,
            "MinWidth": media_strategy.images.min_download_width,
        }));
    }
    if media_strategy.images.disc {
        image_options.push(json!({
            "Type": "Disc",
            "Limit": 1,
            "MinWidth": media_strategy.images.min_download_width,
        }));
    }
    if media_strategy.images.max_backdrop_count > 0 {
        image_options.push(json!({
            "Type": "Backdrop",
            "Limit": media_strategy.images.max_backdrop_count,
            "MinWidth": media_strategy.images.min_download_width,
        }));
    }

    json!({
        "Type": item_type,
        "MetadataFetchers": [],
        "MetadataFetcherOrder": [],
        "ImageFetchers": [],
        "ImageFetcherOrder": [],
        "ImageOptions": image_options,
    })
}

pub(super) fn emby_subtitle_language_code(language: &str) -> String {
    match language.split('-').next().unwrap_or(language) {
        "zh" => "chi".to_owned(),
        "en" => "eng".to_owned(),
        "ja" => "jpn".to_owned(),
        "ko" => "kor".to_owned(),
        "fr" => "fra".to_owned(),
        "de" => "deu".to_owned(),
        "es" => "spa".to_owned(),
        "it" => "ita".to_owned(),
        "ru" => "rus".to_owned(),
        _ => language.to_owned(),
    }
}

pub(super) fn emby_collection_type(kind: LibraryKind) -> Option<&'static str> {
    match kind {
        LibraryKind::Movie => Some("movies"),
        LibraryKind::Series => Some("tvshows"),
        LibraryKind::Mixed => None,
    }
}

pub(super) fn emby_item_type(item_type: &str) -> &'static str {
    match item_type {
        "MOVIE" => "Movie",
        "SERIES" => "Series",
        "SEASON" => "Season",
        "EPISODE" => "Episode",
        "BOX_SET" => "BoxSet",
        _ => "Folder",
    }
}

pub(super) fn emby_stream_type(stream_type: &str) -> &'static str {
    match stream_type {
        "VIDEO" => "Video",
        "AUDIO" => "Audio",
        "SUBTITLE" => "Subtitle",
        _ => "Unknown",
    }
}
