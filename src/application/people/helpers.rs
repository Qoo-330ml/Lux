use super::*;

pub(super) async fn read_relation(
    path: &Path,
) -> Result<Option<StoredPeopleRelation>, PeopleError> {
    let Some(bytes) = read_people_file(path).await? else {
        return Ok(None);
    };
    parse_relation(&bytes).map(Some)
}

pub(super) fn parse_relation(bytes: &[u8]) -> Result<StoredPeopleRelation, PeopleError> {
    if bytes.len() as u64 > MAX_PEOPLE_FILE_BYTES {
        return Err(PeopleError::Serialization(
            "people data is too large".to_owned(),
        ));
    }
    let value = serde_json::from_slice::<Value>(bytes)
        .map_err(|source| PeopleError::Serialization(source.to_string()))?;
    if value.is_array() {
        let actors = serde_json::from_value::<Vec<StoredActor>>(value)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        return Ok(StoredPeopleRelation {
            schema_version: 0,
            generation: 0,
            source_fingerprint: None,
            item_id: None,
            source_key: None,
            source_root: None,
            source_relative_path: None,
            media_fingerprint: None,
            media_size: None,
            media_modified_at: None,
            media_title: None,
            media_production_year: None,
            actors,
        });
    }
    let relation = serde_json::from_value::<StoredPeopleRelation>(value)
        .map_err(|source| PeopleError::Serialization(source.to_string()))?;
    if relation.schema_version > PEOPLE_RELATION_SCHEMA_VERSION {
        return Err(PeopleError::Serialization(
            "people data schema is newer than supported".to_owned(),
        ));
    }
    Ok(relation)
}

pub(super) fn default_relation_schema_version() -> u32 {
    PEOPLE_RELATION_SCHEMA_VERSION
}

pub(super) fn encode_fingerprint(fingerprint: &[u8]) -> String {
    let mut encoded = String::with_capacity(fingerprint.len().saturating_mul(2));
    for byte in fingerprint {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

pub(super) fn decode_fingerprint(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.len() % 2 != 0 {
        return None;
    }
    let mut decoded = Vec::with_capacity(value.len() / 2);
    let bytes = value.as_bytes();
    for pair in bytes.chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        decoded.push(((high << 4) | low) as u8);
    }
    Some(decoded)
}

pub(super) async fn read_people_file(path: &Path) -> Result<Option<Vec<u8>>, PeopleError> {
    let Some(metadata) = safe_metadata(path).await? else {
        return Ok(None);
    };
    if !metadata.is_file() {
        return Err(PeopleError::Serialization(
            "people data path is not a file".to_owned(),
        ));
    }
    if metadata.len() > MAX_PEOPLE_FILE_BYTES {
        return Err(PeopleError::Serialization(
            "people data is too large".to_owned(),
        ));
    }
    fs::read(path)
        .await
        .map(Some)
        .map_err(|source| PeopleError::Io {
            path: path.to_owned(),
            source,
        })
}

pub(super) async fn image_from_path(path: &Path) -> Result<Option<PersonImage>, PeopleError> {
    let Some(metadata) = safe_metadata(path).await? else {
        return Ok(None);
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    let content_type = match path.extension().and_then(|value| value.to_str()) {
        Some("jpg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        _ => return Ok(None),
    };
    Ok(Some(PersonImage {
        path: path.to_owned(),
        content_type,
        content_length: metadata.len(),
    }))
}

pub(super) fn person_nfo_bytes(
    name: &str,
    provider: &str,
    provider_id: &str,
    metadata: Option<&PersonMetadata>,
) -> Vec<u8> {
    let metadata = metadata
        .map(|metadata| person_metadata_xml(metadata, provider, provider_id))
        .unwrap_or_default();
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <person><name>{}</name>{}<uniqueid type=\"{}\">{}</uniqueid></person>\n",
        escape(name),
        metadata,
        escape(provider),
        escape(provider_id),
    )
    .into_bytes()
}

#[derive(Default)]
pub(super) struct ParsedPersonNfo {
    pub(super) fields: BTreeMap<String, String>,
    pub(super) repeated_fields: BTreeMap<String, BTreeSet<String>>,
    pub(super) uniqueids: BTreeSet<(String, String)>,
}

pub(super) fn merge_person_nfo_bytes(
    existing: &[u8],
    name: &str,
    provider: &str,
    provider_id: &str,
    metadata: Option<&PersonMetadata>,
) -> Option<Vec<u8>> {
    let parsed = parse_person_nfo(existing)?;
    let mut additions = String::new();
    append_missing_person_nfo_field(&mut additions, &parsed, "name", Some(name));
    for (tag, value) in [
        (
            "biography",
            metadata.and_then(|value| value.biography.as_deref()),
        ),
        (
            "birthday",
            metadata.and_then(|value| value.birthday.as_deref()),
        ),
        (
            "deathday",
            metadata.and_then(|value| value.deathday.as_deref()),
        ),
        (
            "knownfor",
            metadata.and_then(|value| value.known_for_department.as_deref()),
        ),
        (
            "placeofbirth",
            metadata.and_then(|value| value.place_of_birth.as_deref()),
        ),
    ] {
        append_missing_person_nfo_field(&mut additions, &parsed, tag, value);
    }
    let mut known_uniqueids = parsed.uniqueids.clone();
    for (provider, provider_id) in metadata
        .into_iter()
        .flat_map(|metadata| metadata.provider_ids.iter())
    {
        append_missing_person_nfo_uniqueid(
            &mut additions,
            &mut known_uniqueids,
            provider,
            provider_id,
        );
    }
    let provider = provider.trim().to_ascii_lowercase();
    let provider_id = provider_id.trim();
    append_missing_person_nfo_uniqueid(
        &mut additions,
        &mut known_uniqueids,
        &provider,
        provider_id,
    );
    if let Some(metadata) = metadata {
        append_missing_person_nfo_values(&mut additions, &parsed, "genre", &metadata.genres);
        append_missing_person_nfo_values(&mut additions, &parsed, "tag", &metadata.tags);
        append_missing_person_nfo_values(
            &mut additions,
            &parsed,
            "country",
            &metadata.production_locations,
        );
        append_missing_person_nfo_values(&mut additions, &parsed, "tagline", &metadata.taglines);
        append_missing_person_nfo_field(
            &mut additions,
            &parsed,
            "premiered",
            metadata.premiere_date.as_deref(),
        );
        let production_year = metadata.production_year.map(|year| year.to_string());
        append_missing_person_nfo_field(
            &mut additions,
            &parsed,
            "year",
            production_year.as_deref(),
        );
    }
    if additions.is_empty() {
        return Some(existing.to_owned());
    }
    let mut existing = String::from_utf8(existing.to_owned()).ok()?;
    let closing = existing.rfind("</person>")?;
    existing.insert_str(closing, &additions);
    Some(existing.into_bytes())
}

pub(super) fn replace_person_nfo_bytes(
    existing: &[u8],
    name: &str,
    provider: &str,
    provider_id: &str,
    metadata: Option<&PersonMetadata>,
) -> Option<Vec<u8>> {
    parse_person_nfo(existing)?;
    let mut document = String::from_utf8(existing.to_owned()).ok()?;
    for tag in [
        "name",
        "biography",
        "birthday",
        "deathday",
        "knownfor",
        "placeofbirth",
        "uniqueid",
        "genre",
        "tag",
        "country",
        "tagline",
        "premiered",
        "year",
    ] {
        remove_person_nfo_elements(&mut document, tag)?;
    }
    let closing = document.rfind("</person>")?;
    let generated =
        String::from_utf8(person_nfo_bytes(name, provider, provider_id, metadata)).ok()?;
    let generated_start = generated.find("<person>")? + "<person>".len();
    let generated_end = generated.rfind("</person>")?;
    document.insert_str(closing, &generated[generated_start..generated_end]);
    Some(document.into_bytes())
}

pub(super) fn remove_person_nfo_elements(document: &mut String, tag: &str) -> Option<()> {
    let opening = format!("<{tag}");
    let closing = format!("</{tag}>");
    loop {
        let lower = document.to_ascii_lowercase();
        let Some(start) = find_person_nfo_element_start(&lower, &opening) else {
            return Some(());
        };
        let open_end = lower[start..].find('>')? + start;
        if lower.as_bytes().get(open_end.checked_sub(1)?) == Some(&b'/') {
            document.replace_range(start..=open_end, "");
            continue;
        }
        let content_start = open_end + 1;
        let close_start = lower[content_start..].find(&closing)? + content_start;
        let end = close_start + closing.len();
        document.replace_range(start..end, "");
    }
}

pub(super) fn find_person_nfo_element_start(document: &str, opening: &str) -> Option<usize> {
    let mut search_from = 0;
    while let Some(relative_start) = document[search_from..].find(opening) {
        let start = search_from + relative_start;
        let boundary = document.as_bytes().get(start + opening.len()).copied();
        if matches!(
            boundary,
            Some(b'>') | Some(b'/') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        ) {
            return Some(start);
        }
        search_from = start + opening.len();
    }
    None
}

pub(super) fn append_missing_person_nfo_uniqueid(
    additions: &mut String,
    known_uniqueids: &mut BTreeSet<(String, String)>,
    provider: &str,
    provider_id: &str,
) {
    let provider = provider.trim().to_ascii_lowercase();
    let provider_id = provider_id.trim();
    if provider.is_empty()
        || provider_id.is_empty()
        || !known_uniqueids.insert((provider.clone(), provider_id.to_owned()))
    {
        return;
    }
    additions.push_str(&format!(
        "<uniqueid type=\"{}\">{}</uniqueid>",
        escape(&provider),
        escape(provider_id)
    ));
}

pub(super) fn append_missing_person_nfo_field(
    additions: &mut String,
    parsed: &ParsedPersonNfo,
    tag: &str,
    value: Option<&str>,
) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    let already_present = parsed
        .fields
        .get(tag)
        .is_some_and(|value| !value.trim().is_empty());
    if !already_present {
        additions.push_str(&format!("<{tag}>{}</{tag}>", escape(value)));
    }
}

pub(super) fn append_missing_person_nfo_values(
    additions: &mut String,
    parsed: &ParsedPersonNfo,
    tag: &str,
    values: &[String],
) {
    let existing = parsed.repeated_fields.get(tag);
    let mut appended = BTreeSet::new();
    for value in values
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if existing.is_some_and(|values| values.contains(value)) || !appended.insert(value) {
            continue;
        }
        additions.push_str(&format!("<{tag}>{}</{tag}>", escape(value)));
    }
}

pub(super) fn parse_person_nfo(bytes: &[u8]) -> Option<ParsedPersonNfo> {
    if bytes.len() as u64 > MAX_PEOPLE_FILE_BYTES {
        return None;
    }
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut result = ParsedPersonNfo::default();
    let mut active: Option<(String, Option<String>, String)> = None;
    loop {
        match reader.read_event_into(&mut buffer).ok()? {
            Event::Eof => return Some(result),
            Event::Start(event) => {
                let tag = String::from_utf8(event.name().as_ref().to_ascii_lowercase()).ok()?;
                if tag == "uniqueid" {
                    let mut provider = None;
                    for attribute in event.attributes() {
                        let attribute = attribute.ok()?;
                        if attribute.key.as_ref() == b"type" {
                            provider = Some(attribute.unescape_value().ok()?.into_owned());
                        }
                    }
                    active = Some((tag, provider, String::new()));
                } else if matches!(
                    tag.as_str(),
                    "name"
                        | "biography"
                        | "birthday"
                        | "deathday"
                        | "knownfor"
                        | "placeofbirth"
                        | "premiered"
                        | "year"
                ) || matches!(tag.as_str(), "genre" | "tag" | "country" | "tagline")
                {
                    active = Some((tag, None, String::new()));
                }
            }
            Event::Text(text) => {
                if let Some((_, _, value)) = active.as_mut() {
                    let decoded = text.decode().ok()?;
                    value.push_str(unescape(decoded.as_ref()).ok()?.as_ref());
                }
            }
            Event::End(_) => {
                if let Some((tag, provider, value)) = active.take() {
                    let value = value.trim().to_owned();
                    if tag == "uniqueid" {
                        if let Some(provider) = provider
                            .map(|provider| provider.trim().to_ascii_lowercase())
                            .filter(|provider| !provider.is_empty())
                        {
                            if !value.is_empty() {
                                result.uniqueids.insert((provider, value));
                            }
                        }
                    } else if matches!(tag.as_str(), "genre" | "tag" | "country" | "tagline") {
                        result.repeated_fields.entry(tag).or_default().insert(value);
                    } else {
                        result.fields.entry(tag).or_insert(value);
                    }
                }
            }
            _ => {}
        }
        buffer.clear();
    }
}

pub(super) fn person_metadata_xml(
    metadata: &PersonMetadata,
    primary_provider: &str,
    primary_id: &str,
) -> String {
    let mut xml = String::new();
    for (tag, value) in [
        ("biography", metadata.biography.as_deref()),
        ("birthday", metadata.birthday.as_deref()),
        ("deathday", metadata.deathday.as_deref()),
        ("knownfor", metadata.known_for_department.as_deref()),
        ("placeofbirth", metadata.place_of_birth.as_deref()),
    ] {
        if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
            xml.push_str(&format!("<{tag}>{}</{tag}>", escape(value)));
        }
    }
    let mut uniqueids = BTreeSet::new();
    let primary_provider = primary_provider.trim().to_ascii_lowercase();
    let primary_id = primary_id.trim();
    if !primary_provider.is_empty() && !primary_id.is_empty() {
        uniqueids.insert((primary_provider, primary_id.to_owned()));
    }
    for (provider, provider_id) in &metadata.provider_ids {
        append_uniqueid_xml(&mut xml, &mut uniqueids, provider, provider_id);
    }
    for (tag, values) in [
        ("genre", &metadata.genres),
        ("tag", &metadata.tags),
        ("country", &metadata.production_locations),
        ("tagline", &metadata.taglines),
    ] {
        let mut seen = BTreeSet::new();
        for value in values
            .iter()
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !seen.insert(value) {
                continue;
            }
            xml.push_str(&format!("<{tag}>{}</{tag}>", escape(value)));
        }
    }
    if let Some(value) = metadata
        .premiere_date
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        xml.push_str(&format!("<premiered>{}</premiered>", escape(value)));
    }
    if let Some(value) = metadata.production_year {
        xml.push_str(&format!("<year>{value}</year>"));
    }
    xml
}

pub(super) fn append_uniqueid_xml(
    xml: &mut String,
    known_uniqueids: &mut BTreeSet<(String, String)>,
    provider: &str,
    provider_id: &str,
) {
    let provider = provider.trim().to_ascii_lowercase();
    let provider_id = provider_id.trim();
    if provider.is_empty()
        || provider_id.is_empty()
        || !known_uniqueids.insert((provider.clone(), provider_id.to_owned()))
    {
        return;
    }
    xml.push_str(&format!(
        "<uniqueid type=\"{}\">{}</uniqueid>",
        escape(&provider),
        escape(provider_id)
    ));
}

pub(super) fn default_provider() -> String {
    "tmdb".to_owned()
}

pub(super) fn actor_id_from_stored_actor(actor: &StoredActor) -> String {
    actor
        .id
        .as_deref()
        .filter(|id| is_valid_person_id(id))
        .map(str::to_owned)
        .or_else(|| {
            actor
                .identities
                .iter()
                .find(|identity| {
                    is_valid_person_id(&identity.provider) && is_valid_person_id(&identity.id)
                })
                .map(|identity| identity.id.clone())
        })
        .unwrap_or_else(|| local_actor_id(&actor.name, actor.character.as_deref()))
}

pub(super) fn actor_provider_from_stored_actor(actor: &StoredActor) -> Option<String> {
    if actor.id.as_deref().is_some_and(is_valid_person_id)
        && !actor.provider.is_empty()
        && validate_component(&actor.provider).is_ok()
    {
        return Some(actor.provider.clone());
    }
    actor
        .identities
        .iter()
        .find(|identity| is_valid_person_id(&identity.provider) && is_valid_person_id(&identity.id))
        .map(|identity| identity.provider.clone())
}

pub(super) fn actor_provider_matches(
    stored: &StoredActor,
    enriched: &ActorCredit,
    fallback_provider: &str,
) -> bool {
    let enriched_provider = enriched
        .provider
        .as_deref()
        .unwrap_or(fallback_provider)
        .trim();
    actor_provider_from_stored_actor(stored)
        .is_none_or(|stored_provider| stored_provider.eq_ignore_ascii_case(enriched_provider))
}

pub(super) fn person_credit_from_stored_actor(actor: &StoredActor) -> NewPersonCredit {
    NewPersonCredit {
        person_id: actor_id_from_stored_actor(actor),
        lux_person_id: actor
            .person_key
            .as_deref()
            .filter(|person_key| person_key.starts_with("lux-"))
            .map(str::to_owned),
        person_type: "Actor".to_owned(),
        person_name: actor.name.clone(),
        provider: actor_provider_from_stored_actor(actor).unwrap_or_default(),
        role: actor.character.clone().unwrap_or_default(),
        sort_order: i64::from(actor.order.unwrap_or(i32::MAX)),
        biography: actor
            .person
            .as_ref()
            .and_then(|person| person.biography.clone()),
        birthday: actor
            .person
            .as_ref()
            .and_then(|person| person.birthday.clone()),
        deathday: actor
            .person
            .as_ref()
            .and_then(|person| person.deathday.clone()),
        known_for_department: actor
            .person
            .as_ref()
            .and_then(|person| person.known_for_department.clone()),
        place_of_birth: actor
            .person
            .as_ref()
            .and_then(|person| person.place_of_birth.clone()),
        provider_ids: actor
            .person
            .as_ref()
            .map(|person| person.provider_ids.clone())
            .unwrap_or_default(),
        genres: actor
            .person
            .as_ref()
            .map(|person| person.genres.clone())
            .unwrap_or_default(),
        tags: actor
            .person
            .as_ref()
            .map(|person| person.tags.clone())
            .unwrap_or_default(),
        production_locations: actor
            .person
            .as_ref()
            .map(|person| person.production_locations.clone())
            .unwrap_or_default(),
        premiere_date: actor
            .person
            .as_ref()
            .and_then(|person| person.premiere_date.clone()),
        production_year: actor
            .person
            .as_ref()
            .and_then(|person| person.production_year.map(i64::from)),
        taglines: actor
            .person
            .as_ref()
            .map(|person| person.taglines.clone())
            .unwrap_or_default(),
    }
}

pub(super) fn person_match_candidate_view(
    candidate: StoredPersonMatchCandidate,
) -> PersonMatchCandidateView {
    let candidate_person_ids =
        serde_json::from_str(&candidate.candidate_person_ids_json).unwrap_or_default();
    let evidence = serde_json::from_str(&candidate.evidence_json).unwrap_or(Value::Null);
    PersonMatchCandidateView {
        id: candidate.id,
        item_id: candidate.item_id,
        provider: candidate.provider,
        provider_id: candidate.provider_id,
        candidate_person_ids,
        status: candidate.status,
        score: candidate.score,
        evidence,
        created_at: candidate.created_at,
        updated_at: candidate.updated_at,
    }
}

pub(super) fn actor_image_url(provider: &str, person_id: &str) -> Option<String> {
    if provider.eq_ignore_ascii_case("tmdb") {
        Some(format!("/api/v1/people/{person_id}/image"))
    } else if validate_component(provider).is_ok() {
        Some(format!("/api/v1/people/{provider}/{person_id}/image"))
    } else {
        None
    }
}

pub(super) fn validate_component(value: &str) -> Result<(), PeopleError> {
    if value.is_empty()
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(PeopleError::InvalidComponent(value.to_owned()));
    }
    Ok(())
}

pub(super) fn is_valid_person_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

pub(super) fn is_valid_person_lookup(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.contains('/')
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
        && !matches!(value, "." | "..")
}

pub(super) fn actor_identities(
    actor: &ActorCredit,
    fallback_provider: &str,
) -> Vec<PersonIdentity> {
    let mut identities = actor
        .identities
        .iter()
        .filter_map(|identity| {
            let provider = identity.provider.trim().to_ascii_lowercase();
            let id = identity.id.trim().to_owned();
            (is_valid_person_id(&provider) && is_valid_person_id(&id))
                .then_some(PersonIdentity { provider, id })
        })
        .collect::<Vec<_>>();
    let provider = actor
        .provider
        .as_deref()
        .unwrap_or(fallback_provider)
        .trim()
        .to_ascii_lowercase();
    let id = actor.id.trim();
    if is_valid_person_id(&provider)
        && is_valid_person_id(id)
        && !identities
            .iter()
            .any(|identity| identity.provider == provider && identity.id == id)
    {
        identities.push(PersonIdentity {
            provider,
            id: id.to_owned(),
        });
    }
    identities.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then(left.id.cmp(&right.id))
    });
    identities.dedup_by(|left, right| left.provider == right.provider && left.id == right.id);
    identities
}

pub(super) fn same_media_bridge_candidates<'a>(
    relation: Option<&'a StoredPeopleRelation>,
    actor: &ActorCredit,
) -> Vec<&'a StoredActor> {
    let Some(relation) = relation else {
        return Vec::new();
    };
    let name = normalize_person_match_text(&actor.name);
    relation
        .actors
        .iter()
        .filter(|previous| {
            previous
                .person_key
                .as_deref()
                .is_some_and(|person_key| person_key.starts_with("lux-"))
                && normalize_person_match_text(&previous.name) == name
                && match (actor.character.as_deref(), previous.character.as_deref()) {
                    (Some(current), Some(previous)) => {
                        normalize_person_match_text(current)
                            == normalize_person_match_text(previous)
                    }
                    _ => true,
                }
                && match (actor.order, previous.order) {
                    (Some(current), Some(previous)) => current == previous,
                    _ => true,
                }
                && birthdays_compatible(
                    actor
                        .person
                        .as_ref()
                        .and_then(|person| person.birthday.as_deref()),
                    previous
                        .person
                        .as_ref()
                        .and_then(|person| person.birthday.as_deref()),
                )
        })
        .filter(|previous| {
            actor
                .character
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
                && previous
                    .character
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                || actor.order.is_some() && previous.order.is_some()
        })
        .collect::<Vec<_>>()
}

pub(super) fn normalize_person_match_text(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(super) fn birthdays_compatible(current: Option<&str>, previous: Option<&str>) -> bool {
    match (birthday_parts(current), birthday_parts(previous)) {
        (Some(current), Some(previous)) => {
            current.0 == previous.0
                && current.1 == previous.1
                && match (current.2, previous.2) {
                    (Some(current), Some(previous)) => current == previous,
                    _ => true,
                }
        }
        _ => true,
    }
}

pub(super) fn birthday_parts(value: Option<&str>) -> Option<(u32, u32, Option<u32>)> {
    let value = value?.trim();
    let components = value
        .split(|character: char| !character.is_ascii_digit())
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    if !matches!(components.len(), 2 | 3) {
        return None;
    }
    Some((
        components[0].parse().ok()?,
        components[1].parse().ok()?,
        components
            .get(2)
            .and_then(|component| component.parse().ok()),
    ))
}

pub(super) fn valid_person_manifest(manifest: &PersonManifest) -> bool {
    let Some(sequence) = manifest.lux_person_id.strip_prefix("lux-") else {
        return false;
    };
    if !matches!(manifest.schema_version, 1..=PERSON_MANIFEST_SCHEMA_VERSION)
        || sequence.len() < 6
        || !sequence.chars().all(|character| character.is_ascii_digit())
        || manifest.display_name.trim().is_empty()
        || manifest.checksum.is_empty()
        || manifest.identities.iter().any(|identity| {
            !is_valid_person_id(&identity.provider) || !is_valid_person_id(&identity.id)
        })
    {
        return false;
    }
    let mut unsigned = manifest.clone();
    let expected = unsigned.checksum.clone();
    unsigned.checksum.clear();
    let Ok(bytes) = serde_json::to_vec(&unsigned) else {
        return false;
    };
    let digest = Sha256::digest(bytes);
    let actual = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    actual == expected
}

pub(super) fn valid_person_match_snapshot(snapshot: &PersonMatchCandidateSnapshot) -> bool {
    if snapshot.schema_version != PERSON_MATCH_SNAPSHOT_SCHEMA_VERSION
        || !is_valid_person_id(&snapshot.id)
        || !is_valid_person_id(&snapshot.provider)
        || !is_valid_person_id(&snapshot.provider_id)
        || !matches!(
            snapshot.status.as_str(),
            "PENDING" | "CONFIRMED" | "REJECTED"
        )
        || snapshot.checksum.is_empty()
    {
        return false;
    }
    let mut unsigned = snapshot.clone();
    let expected = unsigned.checksum.clone();
    unsigned.checksum.clear();
    let Ok(bytes) = serde_json::to_vec(&unsigned) else {
        return false;
    };
    let actual = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    actual == expected
}

pub(super) fn valid_person_decision_operation(operation: &PersonDecisionOperation) -> bool {
    if operation.schema_version != PERSON_DECISION_OPERATION_SCHEMA_VERSION
        || !is_valid_person_id(&operation.operation_id)
        || !is_valid_person_id(&operation.candidate_id)
        || !is_valid_person_id(&operation.provider)
        || !is_valid_person_id(&operation.provider_id)
        || !is_valid_person_id(&operation.target_person_id)
        || !matches!(operation.operation.as_str(), "CONFIRM" | "UNDO")
        || !matches!(
            operation.state.as_str(),
            "PREPARED" | "COMMITTED" | "COMPLETED"
        )
        || operation.checksum.is_empty()
    {
        return false;
    }
    let mut unsigned = operation.clone();
    let expected = unsigned.checksum.clone();
    unsigned.checksum.clear();
    let Ok(bytes) = serde_json::to_vec(&unsigned) else {
        return false;
    };
    let actual = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    actual == expected
}

pub(super) fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

pub(super) fn current_people_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

pub(super) fn person_metadata_fields(metadata: &PersonMetadata) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if metadata.biography.is_some() {
        fields.push("biography");
    }
    if metadata.birthday.is_some() {
        fields.push("birthday");
    }
    if metadata.deathday.is_some() {
        fields.push("deathday");
    }
    if metadata.known_for_department.is_some() {
        fields.push("knownForDepartment");
    }
    if metadata.place_of_birth.is_some() {
        fields.push("placeOfBirth");
    }
    if !metadata.provider_ids.is_empty() {
        fields.push("providerIds");
    }
    if !metadata.genres.is_empty() {
        fields.push("genres");
    }
    if !metadata.tags.is_empty() {
        fields.push("tags");
    }
    if !metadata.production_locations.is_empty() {
        fields.push("productionLocations");
    }
    if metadata.premiere_date.is_some() {
        fields.push("premiereDate");
    }
    if metadata.production_year.is_some() {
        fields.push("productionYear");
    }
    if !metadata.taglines.is_empty() {
        fields.push("taglines");
    }
    fields
}

pub(super) fn stable_source_key(root_path: &str, relative_path: &str) -> String {
    let mut source = Vec::with_capacity(root_path.len() + relative_path.len() + 1);
    source.extend_from_slice(root_path.as_bytes());
    source.push(0);
    source.extend_from_slice(relative_path.as_bytes());
    Sha256::digest(source)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn relation_source_snapshot_matches(
    relation: &StoredPeopleRelation,
    current: &crate::storage::StoredItemSourceLocator,
) -> bool {
    if relation.source_root.as_deref() != Some(current.root_path.as_str())
        || relation.source_relative_path.as_deref() != Some(current.relative_path.as_str())
    {
        return false;
    }
    relation_media_snapshot_matches(relation, current)
}

pub(super) fn relation_media_snapshot_matches(
    relation: &StoredPeopleRelation,
    current: &crate::storage::StoredItemSourceLocator,
) -> bool {
    if let Some(expected_fingerprint) = relation.media_fingerprint.as_deref() {
        let matches = current
            .fingerprint
            .as_deref()
            .map(|fingerprint| encode_fingerprint(fingerprint) == expected_fingerprint)
            .unwrap_or(false);
        if !matches {
            return false;
        }
        if relation.media_title.as_deref().is_some_and(|title| {
            normalize_person_match_text(title) != normalize_person_match_text(&current.title)
        }) {
            return false;
        }
        return relation.media_production_year.is_none()
            || relation.media_production_year == current.production_year;
    }

    let Some(expected_size) = relation.media_size else {
        return false;
    };
    let Some(expected_modified_at) = relation.media_modified_at else {
        return false;
    };
    if expected_size != current.size || expected_modified_at != current.modified_at {
        return false;
    }
    if relation.media_title.as_deref().is_some_and(|title| {
        normalize_person_match_text(title) != normalize_person_match_text(&current.title)
    }) {
        return false;
    }
    relation.media_production_year.is_none()
        || relation.media_production_year == current.production_year
}

pub(super) fn person_key_for_identities(identities: &[PersonIdentity]) -> Option<String> {
    if identities.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    for identity in identities {
        hasher.update(identity.provider.as_bytes());
        hasher.update(*b":");
        hasher.update(identity.id.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(encoded, "{byte:02x}");
    }
    Some(format!("person-{encoded}"))
}

pub(super) fn local_actor_id(name: &str, character: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.trim().as_bytes());
    hasher.update([0]);
    hasher.update(character.unwrap_or_default().trim().as_bytes());
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(encoded, "{byte:02x}");
    }
    format!("local-{encoded}")
}

pub(super) fn deserialize_person_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(value) => Ok(value),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(serde::de::Error::custom(
            "person ID must be a string or number",
        )),
    }
}

pub(super) fn deserialize_optional_person_id<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value)),
        Value::Number(value) => Ok(Some(value.to_string())),
        _ => Err(serde::de::Error::custom(
            "person ID must be null, a string, or a number",
        )),
    }
}

pub(super) async fn safe_metadata(path: &Path) -> Result<Option<std::fs::Metadata>, PeopleError> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(PeopleError::Symlink(path.to_owned()))
        }
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(PeopleError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

pub(super) async fn create_private_dir(path: &Path) -> Result<(), PeopleError> {
    let mut current = Some(path.to_owned());
    while let Some(candidate) = current {
        if let Some(metadata) = safe_metadata(&candidate).await? {
            if !metadata.is_dir() {
                return Err(PeopleError::Io {
                    path: candidate,
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotADirectory,
                        "metadata path component is not a directory",
                    ),
                });
            }
            break;
        }
        current = candidate.parent().map(Path::to_owned);
    }
    fs::create_dir_all(path)
        .await
        .map_err(|source| PeopleError::Io {
            path: path.to_owned(),
            source,
        })?;
    restrict_permissions(path, true).await
}

pub(super) async fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), PeopleError> {
    let parent = path.parent().ok_or_else(|| PeopleError::Io {
        path: path.to_owned(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing parent"),
    })?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| PeopleError::Io {
            path: path.to_owned(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid file name"),
        })?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::now_v7()));
    let result = async {
        let mut file = fs::File::create(&temporary)
            .await
            .map_err(|source| PeopleError::Io {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(bytes)
            .await
            .map_err(|source| PeopleError::Io {
                path: temporary.clone(),
                source,
            })?;
        file.sync_all().await.map_err(|source| PeopleError::Io {
            path: temporary.clone(),
            source,
        })?;
        drop(file);
        fs::rename(&temporary, path)
            .await
            .map_err(|source| PeopleError::Io {
                path: path.to_owned(),
                source,
            })?;
        restrict_permissions(path, false).await
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result
}

pub(super) async fn acquire_person_manifest_lock(manifest_path: &Path) -> Result<(), PeopleError> {
    acquire_exclusive_file_lock(&manifest_path.with_file_name(".person.json.lock")).await
}

pub(super) async fn acquire_exclusive_file_lock(lock_path: &Path) -> Result<(), PeopleError> {
    for _ in 0..100 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .await
        {
            Ok(mut file) => {
                file.write_all(Uuid::now_v7().to_string().as_bytes())
                    .await
                    .map_err(|source| PeopleError::Io {
                        path: lock_path.to_owned(),
                        source,
                    })?;
                file.sync_all().await.map_err(|source| PeopleError::Io {
                    path: lock_path.to_owned(),
                    source,
                })?;
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = safe_metadata(lock_path)
                    .await?
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_secs(300));
                if stale {
                    let _ = fs::remove_file(&lock_path).await;
                } else {
                    sleep(Duration::from_millis(10)).await;
                }
            }
            Err(source) => {
                return Err(PeopleError::Io {
                    path: lock_path.to_owned(),
                    source,
                });
            }
        }
    }
    Err(PeopleError::Io {
        path: lock_path.to_owned(),
        source: std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "person manifest lock could not be acquired",
        ),
    })
}

pub(super) async fn restrict_permissions(path: &Path, directory: bool) -> Result<(), PeopleError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if directory { 0o700 } else { 0o600 };
        fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .await
            .map_err(|source| PeopleError::Io {
                path: path.to_owned(),
                source,
            })?;
    }
    #[cfg(not(unix))]
    let _ = (path, directory);
    Ok(())
}

pub(super) fn valid_image(content_type: &str, bytes: &[u8]) -> bool {
    match content_type {
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        _ => false,
    }
}
