use super::*;

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
    let mut items = Vec::new();
    for view in views {
        let library_id = view.library.id.to_string();
        let child_count =
            emby_library_root_count(state, principal, &library_id, view.library.kind).await?;
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
    if emby_fields_include(fields, "Chapters")
        && catalog.populate_chapters(&mut catalog_items).await.is_err()
    {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let mut items = Vec::with_capacity(catalog_items.len());
    for item in &catalog_items {
        let nfo = if emby_nfo_fields_requested(fields) {
            read_local_nfo_details(state, &item.id).await
        } else {
            None
        };
        let mut value = emby_catalog_item_json_with_state(
            item,
            &state.server_id,
            user_states.get(&item.id),
            nfo.as_ref(),
            can_download,
            fields,
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
        if fields.is_some_and(|fields| emby_fields_include(Some(fields), "People")) {
            let actors = match state.people.as_ref() {
                Some(people) => match people.list_item_actors(&item.id).await {
                    Ok(actors) => actors,
                    Err(error) => {
                        tracing::warn!(
                            item_id = %item.id,
                            %error,
                            "derived actor relation is unavailable for Emby list response"
                        );
                        Vec::new()
                    }
                },
                None => Vec::new(),
            };
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
    let mut item_json = emby_catalog_item_json_with_state(
        &item,
        &state.server_id,
        user_state.as_ref(),
        nfo.as_ref(),
        can_download,
        query.fields.as_deref(),
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
