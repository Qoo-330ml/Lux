use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedMovieFilename {
    pub title: String,
    pub sort_title: String,
    pub production_year: Option<i32>,
    pub edition_name: Option<String>,
    pub quality_label: Option<String>,
    pub provider_ids: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedEpisodeFilename {
    pub title: String,
    pub sort_title: String,
    pub production_year: Option<i32>,
    pub season: u32,
    pub episode: u32,
    pub absolute_number: Option<u32>,
    pub edition_name: Option<String>,
    pub quality_label: Option<String>,
    pub provider_ids: BTreeMap<String, String>,
}

#[derive(Clone, Copy)]
pub(super) enum MixedClassification {
    Movie,
    Episode,
    Unresolved,
}

pub(super) enum ManifestFilenameInput {
    MovieLibrary,
    SeriesLibrary,
    HomeVideos,
    LegacyMixed(Option<MixedClassification>),
    PreparedMixed(MixedManifestClassification),
}

impl ManifestFilenameInput {
    pub(super) fn is_movie(&self) -> bool {
        matches!(
            self,
            Self::MovieLibrary
                | Self::LegacyMixed(Some(MixedClassification::Movie))
                | Self::PreparedMixed(MixedManifestClassification::Movie(_))
        )
    }
}

pub(super) enum PreparedManifestFilename {
    Movie(ParsedMovieFilename),
    Episode(ParsedEpisodeFilename),
    Unresolved,
    HomeVideo,
}

pub(super) enum MixedManifestClassification {
    Movie(Option<ParsedMovieFilename>),
    Episode(Option<ParsedEpisodeFilename>),
    Unresolved,
}

pub(super) fn prepare_manifest_filename(
    filename: &str,
    input: ManifestFilenameInput,
) -> Option<PreparedManifestFilename> {
    prepare_manifest_filename_with_variant_suffix(filename, input, None)
}

pub(super) fn prepare_manifest_filename_with_variant_suffix(
    filename: &str,
    input: ManifestFilenameInput,
    inferred_suffix: Option<&str>,
) -> Option<PreparedManifestFilename> {
    if input.is_movie()
        && let Some(suffix) = inferred_suffix
    {
        return parse_movie_filename_with_variant_suffix(filename, suffix)
            .map(PreparedManifestFilename::Movie);
    }
    match input {
        ManifestFilenameInput::MovieLibrary => {
            let parsed_name = parse_movie_filename(filename);
            Some(parsed_name.map_or(
                PreparedManifestFilename::Unresolved,
                PreparedManifestFilename::Movie,
            ))
        }
        ManifestFilenameInput::SeriesLibrary => {
            let parsed_name = parse_episode_filename(filename);
            Some(parsed_name.map_or(
                PreparedManifestFilename::Unresolved,
                PreparedManifestFilename::Episode,
            ))
        }
        ManifestFilenameInput::HomeVideos => Some(PreparedManifestFilename::HomeVideo),
        ManifestFilenameInput::LegacyMixed(preclassified) => match preclassified? {
            MixedClassification::Movie => {
                parse_movie_filename(filename).map(PreparedManifestFilename::Movie)
            }
            MixedClassification::Episode => {
                parse_episode_filename(filename).map(PreparedManifestFilename::Episode)
            }
            MixedClassification::Unresolved => Some(PreparedManifestFilename::Unresolved),
        },
        ManifestFilenameInput::PreparedMixed(classification) => match classification {
            MixedManifestClassification::Movie(parsed) => {
                parsed.map(PreparedManifestFilename::Movie)
            }
            MixedManifestClassification::Episode(parsed) => {
                parsed.map(PreparedManifestFilename::Episode)
            }
            MixedManifestClassification::Unresolved => Some(PreparedManifestFilename::Unresolved),
        },
    }
}

#[derive(Default)]
pub(super) struct MixedClassificationCache {
    pub(super) nfo_exists: HashMap<PathBuf, bool>,
    pub(super) nfo_roots: HashMap<(PathBuf, String), bool>,
}

pub(super) async fn classify_mixed_file(
    root: &Path,
    path: &Path,
    cache: &mut MixedClassificationCache,
) -> MixedClassification {
    match classify_mixed_file_with_parsed_name(root, path, cache, false).await {
        MixedManifestClassification::Movie(_) => MixedClassification::Movie,
        MixedManifestClassification::Episode(_) => MixedClassification::Episode,
        MixedManifestClassification::Unresolved => MixedClassification::Unresolved,
    }
}

pub(super) async fn classify_mixed_manifest_file(
    root: &Path,
    path: &Path,
    cache: &mut MixedClassificationCache,
) -> MixedManifestClassification {
    classify_mixed_file_with_parsed_name(root, path, cache, true).await
}

pub(super) async fn classify_mixed_file_with_parsed_name(
    root: &Path,
    path: &Path,
    cache: &mut MixedClassificationCache,
    retain_parsed_name: bool,
) -> MixedManifestClassification {
    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return MixedManifestClassification::Unresolved;
    };
    if let Some(parsed_name) = parse_episode_filename(file_name) {
        return MixedManifestClassification::Episode(retain_parsed_name.then_some(parsed_name));
    }
    let series_nfo = path
        .strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .map(|first| root.join(first.as_os_str()).join("tvshow.nfo"));
    if let Some(series_nfo) = series_nfo
        && cached_nfo_root_is(cache, &series_nfo, "tvshow").await
    {
        return MixedManifestClassification::Unresolved;
    }
    let movie_nfo = if let Some(candidate) =
        path.parent().map(|directory| directory.join("movie.nfo"))
        && cached_nfo_exists(cache, &candidate).await
    {
        Some(candidate)
    } else {
        let candidate = path.with_extension("nfo");
        cached_nfo_exists(cache, &candidate)
            .await
            .then_some(candidate)
    };
    if let Some(movie_nfo) = movie_nfo
        && cached_nfo_root_is(cache, &movie_nfo, "movie").await
    {
        let parsed_name = retain_parsed_name
            .then(|| parse_movie_filename(file_name))
            .flatten();
        return MixedManifestClassification::Movie(parsed_name);
    }
    let parsed_name = parse_movie_filename(file_name);
    if parsed_name
        .as_ref()
        .is_some_and(|parsed| parsed.production_year.is_some())
    {
        MixedManifestClassification::Movie(retain_parsed_name.then_some(parsed_name).flatten())
    } else {
        MixedManifestClassification::Unresolved
    }
}

pub(super) async fn cached_nfo_exists(cache: &mut MixedClassificationCache, path: &Path) -> bool {
    if let Some(exists) = cache.nfo_exists.get(path) {
        return *exists;
    }
    let exists = fs::metadata(path)
        .await
        .is_ok_and(|metadata| metadata.is_file());
    cache.nfo_exists.insert(path.to_owned(), exists);
    exists
}

pub(super) async fn cached_nfo_root_is(
    cache: &mut MixedClassificationCache,
    path: &Path,
    expected: &str,
) -> bool {
    let key = (path.to_owned(), expected.to_owned());
    if let Some(is_expected) = cache.nfo_roots.get(&key) {
        return *is_expected;
    }
    let is_expected = nfo_root_is(path, expected).await;
    cache.nfo_roots.insert(key, is_expected);
    is_expected
}

pub(super) async fn nfo_root_is(path: &Path, expected: &str) -> bool {
    let Ok(bytes) = fs::read(path).await else {
        return false;
    };
    let mut reader = Reader::from_reader(bytes.as_slice());
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) | Ok(Event::Empty(event)) => {
                return event
                    .name()
                    .as_ref()
                    .eq_ignore_ascii_case(expected.as_bytes());
            }
            Ok(Event::Eof) | Err(_) => return false,
            Ok(_) => buffer.clear(),
        }
    }
}

pub fn parse_movie_filename(filename: &str) -> Option<ParsedMovieFilename> {
    parse_media_name(filename, MediaKind::Movie).map(|parsed| ParsedMovieFilename {
        title: parsed.title,
        sort_title: parsed.sort_title,
        production_year: parsed.production_year,
        edition_name: parsed.edition_name,
        quality_label: parsed.quality_label,
        provider_ids: parsed.provider_ids,
    })
}

pub(super) fn parse_movie_filename_with_variant_suffix(
    filename: &str,
    suffix: &str,
) -> Option<ParsedMovieFilename> {
    parse_media_name_with_variant_suffix(filename, MediaKind::Movie, Some(suffix)).map(|parsed| {
        ParsedMovieFilename {
            title: parsed.title,
            sort_title: parsed.sort_title,
            production_year: parsed.production_year,
            edition_name: parsed.edition_name,
            quality_label: parsed.quality_label,
            provider_ids: parsed.provider_ids,
        }
    })
}

pub(super) fn movie_provider_ids(
    path: &Path,
    file_provider_ids: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    merge_movie_provider_ids(file_provider_ids.clone(), &movie_folder_provider_ids(path))
}

pub(super) fn merge_movie_provider_ids(
    mut file_provider_ids: BTreeMap<String, String>,
    folder_provider_ids: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    for (provider, provider_id) in folder_provider_ids {
        file_provider_ids
            .entry(provider.clone())
            .or_insert_with(|| provider_id.clone());
    }
    file_provider_ids
}

pub(super) fn movie_folder_provider_ids_for_directory(
    directory: &Path,
) -> BTreeMap<String, String> {
    let Some(folder_name) = directory.file_name().and_then(|name| name.to_str()) else {
        return BTreeMap::new();
    };
    parse_media_name(folder_name, MediaKind::Movie)
        .map(|folder| folder.provider_ids)
        .unwrap_or_default()
}

pub(super) fn movie_folder_provider_ids(path: &Path) -> BTreeMap<String, String> {
    let Some(directory) = path.parent() else {
        return BTreeMap::new();
    };
    movie_folder_provider_ids_for_directory(directory)
}

pub(super) fn provider_ids_json(provider_ids: &BTreeMap<String, String>) -> Option<String> {
    (!provider_ids.is_empty())
        .then(|| serde_json::to_string(provider_ids).unwrap_or_else(|_| "{}".to_owned()))
}

pub fn parse_episode_filename(filename: &str) -> Option<ParsedEpisodeFilename> {
    let parsed = parse_media_name(filename, MediaKind::Episode)?;
    let season = parsed.season?;
    let episode = parsed.episode?;
    let title = if parsed.title.is_empty() {
        format!("Episode {episode:02}")
    } else {
        parsed.title
    };
    Some(ParsedEpisodeFilename {
        title,
        sort_title: parsed.sort_title,
        production_year: parsed.production_year,
        season,
        episode,
        absolute_number: parsed.absolute_number,
        edition_name: parsed.edition_name,
        quality_label: parsed.quality_label,
        provider_ids: parsed.provider_ids,
    })
}

pub(super) fn clean_hierarchy_title(value: &str) -> String {
    clean_title(value)
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct EpisodeHierarchy {
    pub(super) series_path: String,
    pub(super) series_title: String,
    pub(super) production_year: Option<i32>,
    pub(super) provider_ids: BTreeMap<String, String>,
    pub(super) season_number: u32,
}

pub(super) fn episode_hierarchy(
    relative_path: &str,
    parsed: &ParsedEpisodeFilename,
) -> EpisodeHierarchy {
    let components = relative_path
        .split(['/', '\\'])
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let directories = components
        .split_last()
        .map(|(_, directories)| directories)
        .unwrap_or(&[]);
    let season_directory_index = directories
        .iter()
        .rposition(|value| parse_season_directory_name(value).is_some());
    let series_components = season_directory_index
        .map(|index| &directories[..index])
        .unwrap_or(directories);
    let series_path = if series_components.is_empty() {
        "Series".to_owned()
    } else {
        series_components.join("/")
    };
    let parsed_series = series_components
        .last()
        .and_then(|value| parse_media_name(value, MediaKind::Series));
    let series_title = parsed_series
        .as_ref()
        .map(|value| value.title.clone())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Series".to_owned());
    let production_year = parsed_series
        .as_ref()
        .and_then(|value| value.production_year)
        .or(parsed.production_year);
    let provider_ids = parsed_series
        .map(|value| value.provider_ids.clone())
        .filter(|provider_ids| !provider_ids.is_empty())
        .unwrap_or_else(|| parsed.provider_ids.clone());
    let season_number = season_directory_number(directories).unwrap_or(parsed.season);
    EpisodeHierarchy {
        series_path,
        series_title,
        production_year,
        provider_ids,
        season_number,
    }
}

pub(super) fn legacy_series_identity(
    root: &StoredLibraryRoot,
    hierarchy: &EpisodeHierarchy,
) -> Option<String> {
    hierarchy
        .series_path
        .split('/')
        .next()
        .filter(|component| !component.is_empty())
        .map(|component| format!("series:{}:{component}", root.id))
}

pub(super) fn season_directory_number(components: &[&str]) -> Option<u32> {
    components
        .iter()
        .rev()
        .find_map(|value| parse_season_directory_name(value))
}

pub(super) fn parse_season_directory_name(value: &str) -> Option<u32> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized == "specials" {
        return Some(0);
    }
    let digits = normalized
        .strip_prefix("season")
        .or_else(|| normalized.strip_prefix('s'))?
        .trim();
    let digits = if let Some((prefix, suffix)) = digits.split_once('(') {
        suffix.strip_suffix(')').filter(|value| {
            !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
        })?;
        prefix.trim()
    } else {
        digits
    };
    digits.parse::<u32>().ok()
}

pub(super) fn trailing_hyphen_variant_candidates(filename: &str) -> Option<Vec<(&str, &str)>> {
    let stem = Path::new(filename).file_stem()?.to_str()?;
    if has_multi_part_marker(filename) || has_source_variant_marker(filename) {
        return None;
    }
    let candidates = stem
        .match_indices('-')
        .filter_map(|(index, _)| {
            let base = &stem[..index];
            let suffix = &stem[index + 1..];
            (!base.is_empty()
                && !suffix.is_empty()
                && suffix.trim() == suffix
                && suffix.chars().any(char::is_alphanumeric)
                && suffix.chars().all(|character| {
                    character.is_alphanumeric()
                        || matches!(character, '-' | '_' | '.' | ' ' | '\'' | '’')
                }))
            .then_some((base, suffix))
        })
        .collect::<Vec<_>>();
    (!candidates.is_empty()).then_some(candidates)
}

pub(super) async fn infer_sibling_movie_variant_suffix(path: &Path) -> Option<String> {
    infer_sibling_movie_variant_suffix_with_probe(path, |sibling| async move {
        fs::symlink_metadata(sibling)
            .await
            .is_ok_and(|metadata| metadata.file_type().is_file())
    })
    .await
}

pub(super) async fn infer_sibling_movie_variant_suffix_with_probe<F, Fut>(
    path: &Path,
    mut is_regular_file: F,
) -> Option<String>
where
    F: FnMut(PathBuf) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let filename = path.file_name()?.to_str()?;
    let candidates = trailing_hyphen_variant_candidates(filename)?;
    let directory = path.parent()?;
    let extensions = movie_variant_extensions(path);
    let mut matched_suffixes = HashSet::new();
    for (base, suffix) in candidates.iter().rev() {
        for extension in &extensions {
            let sibling = directory.join(format!("{base}.{extension}"));
            if is_regular_file(sibling).await {
                matched_suffixes.insert(*suffix);
                break;
            }
        }
        if matched_suffixes.len() > 1 {
            return None;
        }
    }
    if matched_suffixes.len() == 1 {
        matched_suffixes.into_iter().next().map(str::to_owned)
    } else {
        None
    }
}

pub(super) fn movie_variant_extensions(path: &Path) -> Vec<String> {
    let mut extensions = Vec::with_capacity(7);
    if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
        extensions.push(extension.to_owned());
    }
    for extension in ["mkv", "MKV", "mp4", "MP4", "strm", "STRM"] {
        if !extensions.iter().any(|existing| existing == extension) {
            extensions.push(extension.to_owned());
        }
    }
    extensions
}
