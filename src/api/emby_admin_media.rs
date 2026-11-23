//! Emby administrator media endpoints: remote metadata search, remote images and image deletion.
//!
//! Every handler is a thin protocol adapter. The work stays in the existing Lux application
//! services (candidate search and selection, image candidate search, image writes), so these
//! endpoints add no new domain rules: they translate the Emby request shape into a Lux service
//! call and the result back into the Emby DTO shape.

use super::*;
use std::collections::BTreeMap;

/// Body of POST /Items/RemoteSearch/{Type}.
///
/// Emby wraps the search terms in a RemoteSearchQuery: the item being identified plus a MovieInfo
/// or SeriesInfo. Yamby sends the same shape through kotlinx serialization, so both the
/// PascalCase and the camelCase spelling are accepted.
#[derive(Deserialize, Default)]
pub(super) struct EmbyRemoteSearchRequest {
    #[serde(
        rename = "ItemId",
        alias = "itemId",
        alias = "Id",
        alias = "id",
        default
    )]
    pub(super) item_id: Option<String>,
    #[serde(rename = "SearchInfo", alias = "searchInfo", default)]
    pub(super) search_info: Option<EmbyRemoteSearchInfo>,
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyRemoteSearchInfo {
    #[serde(rename = "Name", alias = "name", default)]
    pub(super) name: Option<String>,
    #[serde(
        rename = "Year",
        alias = "year",
        alias = "ProductionYear",
        alias = "productionYear",
        default
    )]
    pub(super) year: Option<i32>,
    #[serde(rename = "ProviderIds", alias = "providerIds", default)]
    pub(super) provider_ids: BTreeMap<String, String>,
}

/// Body of POST /Items/RemoteSearch/Apply/{ItemId}: the RemoteSearchResult a client sends back.
#[derive(Deserialize, Default)]
pub(super) struct EmbyRemoteSearchResultRequest {
    #[serde(rename = "Id", alias = "id", default)]
    pub(super) id: Option<String>,
    #[serde(rename = "Name", alias = "name", default)]
    pub(super) name: Option<String>,
    #[serde(
        rename = "ProductionYear",
        alias = "productionYear",
        alias = "Year",
        alias = "year",
        default
    )]
    pub(super) year: Option<i32>,
    #[serde(rename = "ProviderIds", alias = "providerIds", default)]
    pub(super) provider_ids: BTreeMap<String, String>,
}

/// POST /Items/RemoteSearch/{Movie|Series}.
///
/// Lux answers with the pending candidates the configured scrapers stored for the item. A library
/// without a usable scraper is 409 instead of an empty list, so the admin sees the real reason.
pub(super) async fn emby_remote_search(
    headers: HeaderMap,
    Path(search_type): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<ReadyAppState>,
    Json(request): Json<EmbyRemoteSearchRequest>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !auth_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let search_type = search_type.trim();
    if !matches_image_search_type(search_type) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(item_id) = request
        .item_id
        .as_deref()
        .map(emby_internal_id)
        .filter(|item_id| !item_id.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let search_info = request.search_info.unwrap_or_default();
    let search = search_info
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    // Provider ids are an alternative search key: a client that already knows the TMDb/IMDb id can
    // ask for the match directly, without a name. Blank ids are dropped so they cannot turn an
    // otherwise empty request into a search.
    let provider_ids = search_info
        .provider_ids
        .into_iter()
        .filter(|(key, value)| !key.trim().is_empty() && !value.trim().is_empty())
        .collect::<BTreeMap<_, _>>();
    if search.is_none() && provider_ids.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let provider_ids = (!provider_ids.is_empty()).then_some(provider_ids);
    let reidentify = state.metadata_reidentify();
    let page = match reidentify
        .search_item_candidates_with_provider_ids(
            &item_id,
            search,
            search_info.year,
            provider_ids.as_ref(),
        )
        .await
    {
        Ok(page) => page,
        Err(error) => return emby_remote_search_error(&headers, error),
    };
    // Emby clients show the list in the order it arrives. When the request carried provider ids,
    // the candidate those ids point at goes first so the administrator does not have to hunt for
    // the row the server already knows is right.
    let (preferred, rest): (Vec<_>, Vec<_>) = page
        .items
        .iter()
        .partition(|candidate| provider_id_matches(candidate, provider_ids.as_ref()));
    let results = preferred
        .into_iter()
        .chain(rest)
        .map(remote_search_result_json)
        .collect::<Vec<_>>();
    Json(results).into_response()
}

fn provider_id_matches(
    candidate: &crate::application::candidates::MetadataCandidateView,
    provider_ids: Option<&BTreeMap<String, String>>,
) -> bool {
    let candidate_id = candidate.provider_id.trim();
    !candidate_id.is_empty()
        && provider_ids.is_some_and(|provider_ids| {
            provider_ids.values().any(|value| {
                let value = value.trim();
                !value.is_empty() && value.eq_ignore_ascii_case(candidate_id)
            })
        })
}

fn matches_image_search_type(search_type: &str) -> bool {
    search_type.eq_ignore_ascii_case("Movie")
        || search_type.eq_ignore_ascii_case("Series")
        || search_type.eq_ignore_ascii_case("BoxSet")
}

/// One Emby RemoteSearchResult built from a stored Lux candidate.
///
/// Id is the stored candidate id: Apply uses it to select exactly the row the client picked, and
/// the candidate JSON already uses the Emby field names the scrapers return.
fn remote_search_result_json(
    candidate: &crate::application::candidates::MetadataCandidateView,
) -> Value {
    let payload = &candidate.candidate;
    let mut object = serde_json::Map::new();
    object.insert("Id".to_owned(), json!(candidate.id));
    for (key, field) in [
        ("Name", "Name"),
        ("OriginalTitle", "OriginalTitle"),
        ("Overview", "Overview"),
        ("ProductionYear", "ProductionYear"),
        ("PremiereDate", "PremiereDate"),
        ("ImageUrl", "ImageUrl"),
    ] {
        if let Some(value) = payload.get(field) {
            object.insert(key.to_owned(), value.clone());
        }
    }
    let provider_ids = payload
        .get("ProviderIds")
        .cloned()
        .unwrap_or_else(|| json!({}));
    object.insert("ProviderIds".to_owned(), provider_ids);
    let provider_name = payload
        .get("SearchProviderName")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| candidate.provider.clone());
    object.insert("SearchProviderName".to_owned(), json!(provider_name));
    object.insert("SearchProviderId".to_owned(), json!(candidate.provider));
    object.insert("ProductionYearValue".to_owned(), Value::Null);
    object.remove("ProductionYearValue");
    Value::Object(object)
}

/// POST /Items/RemoteSearch/Apply/{ItemId}.
///
/// Clients echo the RemoteSearchResult they picked. Lux resolves it to the stored pending
/// candidate and runs the same selection the Lux admin API runs, so NFO and image write-back,
/// expiry and locking behave identically.
pub(super) async fn emby_remote_search_apply(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<ReadyAppState>,
    Json(request): Json<EmbyRemoteSearchResultRequest>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !auth_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let item_id = emby_internal_id(&item_id);
    let candidates = state.metadata_candidates();
    let page = match candidates.list_for_item(&item_id, None, 0, 100).await {
        Ok(page) => page,
        Err(crate::application::candidates::MetadataCandidateError::ItemNotFound) => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let selected = page.items.iter().find(|candidate| {
        if let Some(id) = request.id.as_deref()
            && id == candidate.id
        {
            return true;
        }
        request
            .provider_ids
            .values()
            .any(|value| !value.trim().is_empty() && value.trim() == candidate.provider_id.trim())
    });
    let candidate_id = match selected {
        Some(selected) => selected.id.clone(),
        None => {
            // Pending candidates expire, and a client may apply a result that it cached across a
            // restart. Re-running the same search once keeps the pick usable without trusting an
            // identifier the client made up; a body without a name cannot be re-searched.
            let Some(name) = request
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                return StatusCode::NOT_FOUND.into_response();
            };
            let reidentify = state.metadata_reidentify();
            let refreshed = match reidentify
                .search_item_candidates(&item_id, name, request.year)
                .await
            {
                Ok(page) => page,
                Err(error) => return emby_remote_search_error(&headers, error),
            };
            let Some(candidate) = refreshed.items.first() else {
                return StatusCode::NOT_FOUND.into_response();
            };
            candidate.id.clone()
        }
    };
    let Some(selection) = state.metadata_selection() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match crate::application::admin_metadata::select_metadata_candidate(
        selection,
        Some(state.metadata_reidentify()),
        &item_id,
        &candidate_id,
        crate::application::candidates::MetadataSelectionMode::FillMissing,
    )
    .await
    {
        Ok(report) => {
            record_audit_event(
                &state,
                &headers,
                "METADATA_SELECTED",
                Some("item"),
                Some(&report.item_id),
                r#"{"source":"emby_remote_search_apply"}"#,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

fn emby_remote_search_error(
    headers: &HeaderMap,
    error: crate::application::candidates::MetadataCandidateError,
) -> Response {
    use crate::application::candidates::MetadataCandidateError;
    match error {
        MetadataCandidateError::ItemNotFound => StatusCode::NOT_FOUND.into_response(),
        MetadataCandidateError::InvalidSearch => StatusCode::BAD_REQUEST.into_response(),
        MetadataCandidateError::VideoItemUnsupported => {
            StatusCode::UNPROCESSABLE_ENTITY.into_response()
        }
        MetadataCandidateError::ScraperNotConfigured => {
            let _ = headers;
            StatusCode::CONFLICT.into_response()
        }
        MetadataCandidateError::ScraperFailed(_)
        | MetadataCandidateError::Scraper(_)
        | MetadataCandidateError::InvalidCandidateJson(_) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyRemoteImagesQuery {
    #[serde(flatten)]
    pub(super) auth: EmbyTokenQuery,
    #[serde(rename = "Type", alias = "type", default)]
    pub(super) image_type: Option<String>,
    #[serde(rename = "ProviderName", alias = "providerName", default)]
    pub(super) provider_name: Option<String>,
    #[serde(rename = "StartIndex", alias = "startIndex", default)]
    pub(super) start_index: Option<i64>,
    #[serde(rename = "Limit", alias = "limit", default)]
    pub(super) limit: Option<i64>,
}

/// Emby image type to the Lux scraper image type it maps to.
fn lux_image_type(emby_type: &str) -> Option<&'static str> {
    let value = emby_type.trim();
    if value.eq_ignore_ascii_case("Primary") || value.eq_ignore_ascii_case("Poster") {
        return Some("POSTER");
    }
    if value.eq_ignore_ascii_case("Backdrop") || value.eq_ignore_ascii_case("Fanart") {
        return Some("BACKDROP");
    }
    ["LOGO", "THUMB", "BANNER", "DISC", "ART"]
        .into_iter()
        .find(|image_type| value.eq_ignore_ascii_case(image_type))
}

/// Stored image types that satisfy an Emby image type, used when deleting by type and index.
fn stored_image_types(emby_type: &str) -> &'static [&'static str] {
    let value = emby_type.trim();
    if value.eq_ignore_ascii_case("Primary") || value.eq_ignore_ascii_case("Poster") {
        return &["POSTER"];
    }
    if value.eq_ignore_ascii_case("Backdrop") || value.eq_ignore_ascii_case("Fanart") {
        return &["BACKDROP", "FANART"];
    }
    if value.eq_ignore_ascii_case("Logo") {
        return &["LOGO"];
    }
    if value.eq_ignore_ascii_case("Thumb") {
        return &["THUMB"];
    }
    if value.eq_ignore_ascii_case("Banner") {
        return &["BANNER"];
    }
    if value.eq_ignore_ascii_case("Disc") {
        return &["DISC"];
    }
    if value.eq_ignore_ascii_case("Art") {
        return &["ART"];
    }
    &[]
}

/// GET /Items/{ItemId}/RemoteImages: the scraper images a client can assign to the item.
pub(super) async fn emby_remote_images(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<EmbyRemoteImagesQuery>,
    State(state): State<ReadyAppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.auth.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !auth_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let item_id = emby_internal_id(&item_id);
    let emby_type = query.image_type.as_deref().unwrap_or("Primary");
    let Some(image_type) = lux_image_type(emby_type) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let candidates = state.image_candidates();
    let outcome = match candidates
        .search(
            &item_id,
            image_type,
            // The image search language comes from the library policy, not from the request.
            None,
            query.provider_name.as_deref(),
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => return image_candidate_error(&headers, error),
    };
    let start = query.start_index.unwrap_or(0).max(0) as usize;
    let limit = query.limit.unwrap_or(100).clamp(1, 1000) as usize;
    let images = outcome
        .images
        .iter()
        .skip(start)
        .take(limit)
        .map(|image| {
            json!({
                "Url": image.url,
                "Type": emby_type,
                "Language": image.language,
                "ProviderName": image.source,
                "Width": image.width,
                "Height": image.height,
                "ImageIndex": image.image_index,
            })
        })
        .collect::<Vec<_>>();
    Json(json!({
        "Images": images,
        "TotalRecordCount": outcome.images.len(),
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyRemoteImageDownloadRequest {
    #[serde(rename = "Type", alias = "type", default)]
    pub(super) image_type: Option<String>,
    #[serde(
        rename = "ImageUrl",
        alias = "imageUrl",
        alias = "Url",
        alias = "url",
        default
    )]
    pub(super) url: Option<String>,
}

/// POST /Items/{ItemId}/RemoteImages/Download: write one scraper image into the item.
pub(super) async fn emby_remote_image_download(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<ReadyAppState>,
    Json(request): Json<EmbyRemoteImageDownloadRequest>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !auth_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let item_id = emby_internal_id(&item_id);
    let emby_type = request.image_type.as_deref().unwrap_or("Primary");
    let Some(image_type) = lux_image_type(emby_type) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(url) = request
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(images) = state.image_writes() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match images
        .download_item_image_from_scraper_candidate(&item_id, image_type, url)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => image_write_error(&headers, error),
    }
}

/// DELETE /Items/{ItemId}/Images/{Type}/{Index}/Delete: drop one stored image.
pub(super) async fn emby_delete_item_image(
    headers: HeaderMap,
    Path((item_id, image_type, image_index)): Path<(String, String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<ReadyAppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    if !auth_principal.can_manage_server() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let item_id = emby_internal_id(&item_id);
    let Ok(image_index) = image_index.trim().parse::<i64>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let stored_types = stored_image_types(&image_type);
    if stored_types.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(images) = state.image_writes() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let stored = match images.list_item_images(&item_id).await {
        Ok(images) => images,
        Err(error) => return image_write_error(&headers, error),
    };
    let Some(image) = stored.into_iter().find(|image| {
        image.image_index == image_index
            && stored_types
                .iter()
                .any(|stored_type| image.image_type.eq_ignore_ascii_case(stored_type))
    }) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match images.delete_item_image(&item_id, &image.id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => image_write_error(&headers, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate() -> crate::application::candidates::MetadataCandidateView {
        crate::application::candidates::MetadataCandidateView {
            id: "candidate-1".to_owned(),
            item_id: "item-1".to_owned(),
            item_title: "Example".to_owned(),
            provider: "org.lux.tmdb".to_owned(),
            provider_id: "550".to_owned(),
            candidate: json!({
                "Name": "Fight Club",
                "ProductionYear": 1999,
                "ImageUrl": "https://img.example/poster.jpg",
                "ProviderIds": {"Tmdb": "550"},
                "SearchProviderName": "TMDb",
                "metadataDetailsVersion": 3,
            }),
            score: 9.0,
            status: "PENDING".to_owned(),
            expires_at: None,
            field_diffs: Vec::new(),
        }
    }

    #[test]
    fn remote_search_result_uses_the_emby_field_names() {
        let result = remote_search_result_json(&candidate());
        assert_eq!(result["Id"], "candidate-1");
        assert_eq!(result["Name"], "Fight Club");
        assert_eq!(result["ProductionYear"], 1999);
        assert_eq!(result["ProviderIds"]["Tmdb"], "550");
        assert_eq!(result["SearchProviderName"], "TMDb");
        assert_eq!(result["SearchProviderId"], "org.lux.tmdb");
        assert!(result.get("metadataDetailsVersion").is_none());
    }

    #[test]
    fn image_types_map_both_ways() {
        assert_eq!(lux_image_type("Primary"), Some("POSTER"));
        assert_eq!(lux_image_type("backdrop"), Some("BACKDROP"));
        assert_eq!(lux_image_type("Logo"), Some("LOGO"));
        assert_eq!(lux_image_type("Nope"), None);
        assert_eq!(stored_image_types("Backdrop"), ["BACKDROP", "FANART"]);
        assert_eq!(stored_image_types("Primary"), ["POSTER"]);
        assert!(stored_image_types("Nope").is_empty());
    }

    #[test]
    fn provider_ids_match_the_stored_candidate() {
        let candidate = candidate();
        let mut ids = BTreeMap::new();
        ids.insert("Tmdb".to_owned(), "550".to_owned());
        assert!(provider_id_matches(&candidate, Some(&ids)));
        assert!(!provider_id_matches(&candidate, None));
        ids.insert("Tmdb".to_owned(), "551".to_owned());
        assert!(!provider_id_matches(&candidate, Some(&ids)));
    }
}
