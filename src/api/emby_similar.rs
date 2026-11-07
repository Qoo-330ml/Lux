use super::*;

/// Similar-items page size when the caller does not send `Limit`, matching the other Emby item
/// list endpoints. The page cap is the shared `EMBY_MAX_PAGE_LIMIT`.
const EMBY_SIMILAR_DEFAULT_LIMIT: i64 = 50;

/// Query parameters of `GET /Items/{id}/Similar`.
///
/// The captured Hills and Yamby requests send `UserId`, `Fields`, `Limit`, `ImageTypeLimit` and
/// `EnableImageTypes`. An empty `Limit`/`ImageTypeLimit` (query string `Limit=`) is treated as an
/// absent parameter by the shared `deserialize_optional_i64`, the same blank-value rule LUX-888
/// applies to the other Emby query values. An explicitly empty `EnableImageTypes=` is likewise
/// treated as absent: `parse_emby_image_types` yields no requested types, so the response keeps
/// every image tag instead of hiding them.
#[derive(Deserialize, Default)]
pub(super) struct EmbySimilarQuery {
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
    #[serde(rename = "Fields", default)]
    pub(super) fields: Option<String>,
    #[serde(
        rename = "Limit",
        default,
        deserialize_with = "deserialize_optional_i64"
    )]
    pub(super) limit: Option<i64>,
    #[serde(
        rename = "EnableUserData",
        alias = "enableUserData",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    pub(super) enable_user_data: Option<bool>,
    #[serde(
        rename = "ImageTypeLimit",
        default,
        deserialize_with = "deserialize_optional_i64"
    )]
    pub(super) image_type_limit: Option<i64>,
    #[serde(rename = "EnableImageTypes", default)]
    pub(super) enable_image_types: Option<String>,
}

fn emby_similar_limit(limit: Option<i64>) -> Result<i64, StatusCode> {
    let limit = match limit {
        None | Some(0) => EMBY_SIMILAR_DEFAULT_LIMIT,
        Some(value) if value < 0 => return Err(StatusCode::BAD_REQUEST),
        Some(value) => value,
    };
    if limit > EMBY_MAX_PAGE_LIMIT {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(limit)
}

/// `GET /Items/{id}/Similar`, also served under `/emby`.
///
/// The item lookup goes through the regular catalog service, so an unknown id and an item outside
/// the caller's libraries both answer 404 exactly like `/Items/{id}`. The item DTOs reuse the Emby
/// list projection, which honours `Fields`; `EnableUserData`, `ImageTypeLimit` and
/// `EnableImageTypes` are applied here because the other list endpoints do not implement them yet.
pub(super) async fn emby_item_similar(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<EmbySimilarQuery>,
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
    let limit = match emby_similar_limit(query.limit) {
        Ok(limit) => limit,
        Err(status) => return status.into_response(),
    };
    // The native Lux route is the authoritative implementation of this query; this handler only
    // adapts the result to the Emby `Items`/`TotalRecordCount` DTO and its query parameters.
    let page = match similar_items_page(&state, principal, &emby_internal_id(&item_id), limit).await
    {
        Ok(Some(page)) => page,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(CatalogError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::AccessDenied | CatalogError::LibraryNotFound) => {
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let mut items = match emby_catalog_items_for_user_with_preferred_source(
        &state,
        &principal.user_id_string(),
        &page,
        query.fields.as_deref(),
        auth_principal.can_download(),
        auth_principal.can_manage_server(),
        None,
    )
    .await
    {
        Ok(items) => items,
        Err(status) => return status.into_response(),
    };
    apply_similar_item_options(&mut items, &query);
    Json(json!({
        "Items": items,
        "TotalRecordCount": page.total,
    }))
    .into_response()
}

fn apply_similar_item_options(items: &mut [Value], query: &EmbySimilarQuery) {
    let enabled_image_types = query
        .enable_image_types
        .as_deref()
        .map(parse_emby_image_types)
        .filter(|types| !types.is_empty());
    for item in items {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        if query.enable_user_data == Some(false) {
            object.remove("UserData");
        }
        // `ImageTypeLimit=0` means "no images"; negative values keep Emby's "unlimited" meaning.
        if query.image_type_limit == Some(0) {
            object.insert("ImageTags".to_owned(), json!({}));
            object.insert("BackdropImageTags".to_owned(), json!([]));
            continue;
        }
        if let Some(allowed) = enabled_image_types.as_ref() {
            filter_image_tags(object, allowed);
        }
        if let Some(limit) = query.image_type_limit.filter(|limit| *limit > 0)
            && let Some(Value::Array(backdrops)) = object.get_mut("BackdropImageTags")
            && backdrops.len() > limit as usize
        {
            backdrops.truncate(limit as usize);
        }
    }
}

fn parse_emby_image_types(value: &str) -> Vec<String> {
    value
        .split([',', '|'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

fn filter_image_tags(object: &mut serde_json::Map<String, Value>, allowed: &[String]) {
    if let Some(Value::Object(tags)) = object.get_mut("ImageTags") {
        tags.retain(|key, _| allowed.contains(&key.to_ascii_lowercase()));
    }
    if !allowed.iter().any(|allowed| allowed == "backdrop") {
        object.insert("BackdropImageTags".to_owned(), json!([]));
    }
}
