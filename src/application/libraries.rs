use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::Path,
};

use crate::{
    application::{
        probe::{
            DEFAULT_PROBE_CONCURRENCY as DEFAULT_PROBE_CONCURRENCY_USIZE,
            MAX_EFFECTIVE_PROBE_CONCURRENCY,
        },
        schedule::{DEFAULT_METADATA_SCHEDULE, DEFAULT_RECONCILIATION_SCHEDULE, validate_cron},
    },
    domain::ids::{LibraryId, LibraryRootId},
    library::{
        LibraryKind, LibraryRecord, LibraryRootRecord, LibraryScraper, LibraryScraperRole,
        RootOverlap, RootPathError, classify_root_overlap, inspect_root_path,
    },
    storage::{
        Database, LibrarySettingsUpdate, NewLibrary, NewLibraryRoot, StorageError, StoredLibrary,
        StoredLibraryRoot,
    },
};

const DEFAULT_SCAN_CONCURRENCY: i64 = 2;
const DEFAULT_PROBE_CONCURRENCY: i64 = DEFAULT_PROBE_CONCURRENCY_USIZE as i64;
const MAX_SCAN_CONCURRENCY: i64 = 64;
const MAX_PROBE_CONCURRENCY: i64 = MAX_EFFECTIVE_PROBE_CONCURRENCY as i64;
const MAX_SCHEDULE_LENGTH: usize = 128;
const MAX_LIBRARY_SCRAPERS: usize = 16;

#[derive(Clone)]
pub struct LibraryService {
    database: Database,
}

impl LibraryService {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn create_library(
        &self,
        name: &str,
        kind: LibraryKind,
        realtime_watch_enabled: bool,
    ) -> Result<LibraryRecord, LibraryServiceError> {
        self.create_library_with_scraper(name, kind, realtime_watch_enabled, None, false)
            .await
    }

    pub async fn create_library_with_scraper(
        &self,
        name: &str,
        kind: LibraryKind,
        _realtime_watch_enabled: bool,
        scraper_id: Option<&str>,
        realtime_metadata_auto_match_enabled: bool,
    ) -> Result<LibraryRecord, LibraryServiceError> {
        self.create_library_with_scraper_and_chapter_source(
            name,
            kind,
            _realtime_watch_enabled,
            scraper_id,
            None,
            realtime_metadata_auto_match_enabled,
        )
        .await
    }

    pub async fn create_library_with_scrapers(
        &self,
        name: &str,
        kind: LibraryKind,
        realtime_watch_enabled: bool,
        scrapers: &[LibraryScraper],
        realtime_metadata_auto_match_enabled: bool,
    ) -> Result<LibraryRecord, LibraryServiceError> {
        self.create_library_with_scrapers_and_chapter_source(
            name,
            kind,
            realtime_watch_enabled,
            scrapers,
            None,
            realtime_metadata_auto_match_enabled,
        )
        .await
    }

    pub async fn create_library_with_scraper_and_chapter_source(
        &self,
        name: &str,
        kind: LibraryKind,
        _realtime_watch_enabled: bool,
        scraper_id: Option<&str>,
        chapter_source_id: Option<&str>,
        realtime_metadata_auto_match_enabled: bool,
    ) -> Result<LibraryRecord, LibraryServiceError> {
        let scrapers = scraper_id
            .map(|scraper_id| LibraryScraper {
                scraper_id: scraper_id.to_owned(),
                position: 0,
                role: LibraryScraperRole::Primary,
            })
            .into_iter()
            .collect::<Vec<_>>();
        self.create_library_with_scrapers_and_chapter_source(
            name,
            kind,
            _realtime_watch_enabled,
            &scrapers,
            chapter_source_id,
            realtime_metadata_auto_match_enabled,
        )
        .await
    }

    pub async fn create_library_with_scrapers_and_chapter_source(
        &self,
        name: &str,
        kind: LibraryKind,
        _realtime_watch_enabled: bool,
        requested_scrapers: &[LibraryScraper],
        chapter_source_id: Option<&str>,
        realtime_metadata_auto_match_enabled: bool,
    ) -> Result<LibraryRecord, LibraryServiceError> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 128 {
            return Err(LibraryServiceError::InvalidName);
        }
        let scrapers = normalize_scrapers(requested_scrapers)?;
        let scraper_id = scrapers.first().map(|scraper| scraper.scraper_id.as_str());
        let chapter_source_id = normalize_chapter_source_id(chapter_source_id)?;
        if chapter_source_id.is_some() && !kind.supports_chapter_source() {
            return Err(LibraryServiceError::InvalidChapterSourceId);
        }
        let id = LibraryId::new();
        self.database
            .insert_library(NewLibrary {
                id: &id.to_string(),
                name,
                kind: kind.as_str(),
                scraper_id,
                scrapers: &scrapers,
                realtime_watch_enabled: true,
                realtime_metadata_auto_match_enabled,
                reconciliation_schedule: Some(DEFAULT_RECONCILIATION_SCHEDULE),
                metadata_schedule: (!scrapers.is_empty()).then_some(DEFAULT_METADATA_SCHEDULE),
                scan_concurrency: DEFAULT_SCAN_CONCURRENCY,
                probe_concurrency: DEFAULT_PROBE_CONCURRENCY,
                chapter_source_id: chapter_source_id.as_deref(),
            })
            .await?;
        let stored = self
            .database
            .find_library(&id.to_string())
            .await?
            .ok_or(LibraryServiceError::LibraryNotFound)?;
        stored_library(stored)
    }

    pub async fn list_libraries(&self) -> Result<Vec<LibraryView>, LibraryServiceError> {
        let libraries = self.database.list_libraries().await?;
        let library_ids = libraries
            .iter()
            .map(|library| library.id.clone())
            .collect::<Vec<_>>();
        let mut roots_by_library = self
            .database
            .list_library_roots_by_library_ids(&library_ids)
            .await?;
        let mut views = Vec::with_capacity(libraries.len());
        for library in libraries {
            let id = library.id.clone();
            let library = stored_library(library)?;
            let roots = roots_by_library
                .remove(&id)
                .unwrap_or_default()
                .into_iter()
                .map(stored_library_root)
                .collect::<Result<Vec<_>, _>>()?;
            views.push(LibraryView { library, roots });
        }
        Ok(views)
    }

    pub async fn list_libraries_for_user(
        &self,
        user_id: &str,
        accessible_library_ids: &[String],
    ) -> Result<Vec<LibraryView>, LibraryServiceError> {
        let mut views = self.list_libraries().await?;
        let order = self.database.user_library_order(user_id).await?;
        order_library_views(&mut views, &order);
        let accessible = accessible_library_ids.iter().collect::<HashSet<_>>();
        views.retain(|view| {
            view.library.is_enabled && accessible.contains(&view.library.id.to_string())
        });
        Ok(views)
    }

    pub async fn saved_library_order(
        &self,
        user_id: &str,
    ) -> Result<Vec<String>, LibraryServiceError> {
        Ok(self.database.user_library_order(user_id).await?)
    }

    pub async fn saved_library_order_for_user(
        &self,
        user_id: &str,
        accessible_library_ids: &[String],
    ) -> Result<Vec<String>, LibraryServiceError> {
        let accessible = accessible_library_ids.iter().collect::<HashSet<_>>();
        Ok(self
            .saved_library_order(user_id)
            .await?
            .into_iter()
            .filter(|library_id| accessible.contains(library_id))
            .collect())
    }

    pub async fn set_library_order(
        &self,
        user_id: &str,
        requested_library_ids: &[String],
        accessible_library_ids: &[String],
    ) -> Result<Vec<String>, LibraryServiceError> {
        let accessible = accessible_library_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let mut seen = HashSet::with_capacity(requested_library_ids.len());
        let mut ordered = Vec::with_capacity(accessible.len());
        for library_id in requested_library_ids {
            if !accessible.contains(library_id) {
                return Err(LibraryServiceError::InvalidLibraryOrder(
                    "媒体库不可访问或不存在".to_owned(),
                ));
            }
            if !seen.insert(library_id.clone()) {
                return Err(LibraryServiceError::InvalidLibraryOrder(
                    "媒体库排序不能包含重复项".to_owned(),
                ));
            }
            ordered.push(library_id.clone());
        }
        let views = self.list_libraries().await?;
        for view in views {
            let library_id = view.library.id.to_string();
            if accessible.contains(&library_id) && seen.insert(library_id.clone()) {
                ordered.push(library_id);
            }
        }
        self.database
            .replace_user_library_order(user_id, &ordered)
            .await?;
        Ok(ordered)
    }

    pub async fn get_library(
        &self,
        library_id: LibraryId,
    ) -> Result<LibraryRecord, LibraryServiceError> {
        let stored = self
            .database
            .find_library(&library_id.to_string())
            .await?
            .ok_or(LibraryServiceError::LibraryNotFound)?;
        stored_library(stored)
    }

    pub async fn update_settings(
        &self,
        library_id: LibraryId,
        settings: LibrarySettingsPatch,
    ) -> Result<LibraryView, LibraryServiceError> {
        validate_scan_concurrency(settings.scan_concurrency)?;
        validate_probe_concurrency(settings.probe_concurrency)?;
        let reconciliation_schedule = normalize_schedule(settings.reconciliation_schedule)?;
        let metadata_schedule = normalize_schedule(settings.metadata_schedule)?;
        let name = settings
            .name
            .as_deref()
            .map(normalize_library_name)
            .transpose()?;
        let requested_kind = settings.kind;
        let kind = requested_kind.map(LibraryKind::as_str);
        let scraper_id = normalize_scraper_patch(settings.scraper_id)?;
        let scrapers = settings
            .scrapers
            .as_deref()
            .map(normalize_scrapers)
            .transpose()?;
        let mut chapter_source_id = normalize_chapter_source_patch(settings.chapter_source_id)?;
        let current = self
            .database
            .find_library(&library_id.to_string())
            .await?
            .ok_or(LibraryServiceError::LibraryNotFound)?;
        let current_kind = current
            .kind
            .parse::<LibraryKind>()
            .map_err(|error| LibraryServiceError::InvalidKind(error.to_string()))?;
        let effective_kind = requested_kind.unwrap_or(current_kind);
        if chapter_source_id
            .as_ref()
            .is_some_and(|value| value.is_some())
            && !effective_kind.supports_chapter_source()
        {
            return Err(LibraryServiceError::InvalidChapterSourceId);
        }
        if !effective_kind.supports_chapter_source() {
            chapter_source_id = Some(None);
        }

        let updated = self
            .database
            .update_library_settings(
                &library_id.to_string(),
                LibrarySettingsUpdate {
                    name: name.as_deref(),
                    kind,
                    is_enabled: settings.is_enabled,
                    realtime_watch_enabled: settings.realtime_watch_enabled.map(|_| true),
                    realtime_metadata_auto_match_enabled: settings
                        .realtime_metadata_auto_match_enabled,
                    reconciliation_schedule: reconciliation_schedule
                        .as_ref()
                        .map(|value| value.as_deref()),
                    metadata_schedule: metadata_schedule.as_ref().map(|value| value.as_deref()),
                    scraper_id: scraper_id.as_ref().map(|value| value.as_deref()),
                    scrapers: scrapers.as_deref(),
                    chapter_source_id: chapter_source_id.as_ref().map(|value| value.as_deref()),
                    media_strategy_json: settings
                        .media_strategy_json
                        .as_ref()
                        .map(|value| value.as_deref()),
                    scan_concurrency: settings.scan_concurrency,
                    probe_concurrency: settings.probe_concurrency,
                },
            )
            .await?;
        if !updated {
            return Err(LibraryServiceError::LibraryNotFound);
        }
        let library = self
            .database
            .find_library(&library_id.to_string())
            .await?
            .ok_or(LibraryServiceError::LibraryNotFound)
            .and_then(stored_library)?;
        let roots = self
            .database
            .list_library_roots(&library_id.to_string())
            .await?
            .into_iter()
            .map(stored_library_root)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(LibraryView { library, roots })
    }

    pub async fn add_root(
        &self,
        library_id: LibraryId,
        display_path: &str,
    ) -> Result<AddRootResult, LibraryServiceError> {
        let library_id_text = library_id.to_string();
        if self
            .database
            .find_library(&library_id_text)
            .await?
            .is_none()
        {
            return Err(LibraryServiceError::LibraryNotFound);
        }

        let inspection = inspect_root_path(Path::new(display_path)).await?;
        let existing_roots = self.database.list_all_library_roots().await?;
        let mut warnings = Vec::new();
        for existing in existing_roots {
            let existing_path = Path::new(&existing.canonical_path);
            let overlap = classify_root_overlap(&inspection.canonical_path, existing_path);
            if overlap == RootOverlap::Disjoint {
                continue;
            }
            if existing.library_id == library_id_text {
                return Err(match overlap {
                    RootOverlap::Exact => LibraryServiceError::DuplicateRoot,
                    RootOverlap::Nested => LibraryServiceError::OverlappingRoot,
                    RootOverlap::Disjoint => unreachable!(),
                });
            }
            warnings.push(LibraryWarningCode::CrossLibraryOverlap);
        }

        if !inspection.is_writable {
            warnings.push(LibraryWarningCode::PathNotWritable);
        }

        let canonical_path = inspection.canonical_path.to_string_lossy().into_owned();
        let id = self
            .database
            .find_deleted_library_root_id(&library_id_text, &canonical_path)
            .await?
            .map(|value| {
                value
                    .parse::<LibraryRootId>()
                    .map_err(|_| LibraryServiceError::RootNotFoundAfterInsert)
            })
            .transpose()?
            .unwrap_or_else(LibraryRootId::new);
        self.database
            .insert_library_root(NewLibraryRoot {
                id: &id.to_string(),
                library_id: &library_id_text,
                canonical_path: &canonical_path,
                display_path,
                is_available: inspection.is_available,
                is_writable: inspection.is_writable,
            })
            .await?;
        self.database
            .delete_library_root_history(&library_id_text, &canonical_path)
            .await?;
        let root = self
            .database
            .find_library_root(&id.to_string())
            .await?
            .ok_or(LibraryServiceError::RootNotFoundAfterInsert)
            .and_then(stored_library_root)?;
        Ok(AddRootResult { root, warnings })
    }

    pub async fn delete_root(
        &self,
        library_id: LibraryId,
        root_id: LibraryRootId,
    ) -> Result<(), LibraryServiceError> {
        if !self
            .database
            .delete_library_root(&library_id.to_string(), &root_id.to_string())
            .await?
        {
            return Err(LibraryServiceError::RootNotFound);
        }
        Ok(())
    }

    pub async fn delete_library(&self, library_id: LibraryId) -> Result<(), LibraryServiceError> {
        let library_id_text = library_id.to_string();
        if self
            .database
            .find_library(&library_id_text)
            .await?
            .is_none()
        {
            return Err(LibraryServiceError::LibraryNotFound);
        }
        if self
            .database
            .find_active_scan_job_for_library(&library_id_text)
            .await?
            .is_some()
        {
            return Err(LibraryServiceError::LibraryBusy);
        }
        if !self.database.delete_library(&library_id_text).await? {
            return Err(LibraryServiceError::LibraryNotFound);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryView {
    pub library: LibraryRecord,
    pub roots: Vec<LibraryRootRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddRootResult {
    pub root: LibraryRootRecord,
    pub warnings: Vec<LibraryWarningCode>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LibrarySettingsPatch {
    pub name: Option<String>,
    pub kind: Option<LibraryKind>,
    pub is_enabled: Option<bool>,
    pub realtime_watch_enabled: Option<bool>,
    pub realtime_metadata_auto_match_enabled: Option<bool>,
    pub reconciliation_schedule: Option<Option<String>>,
    pub metadata_schedule: Option<Option<String>>,
    pub scraper_id: Option<Option<String>>,
    pub scrapers: Option<Vec<LibraryScraper>>,
    pub chapter_source_id: Option<Option<String>>,
    pub media_strategy_json: Option<Option<String>>,
    pub scan_concurrency: Option<i64>,
    pub probe_concurrency: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LibraryWarningCode {
    CrossLibraryOverlap,
    PathNotWritable,
}

impl LibraryWarningCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CrossLibraryOverlap => "CROSS_LIBRARY_OVERLAP",
            Self::PathNotWritable => "LIBRARY_PATH_NOT_WRITABLE",
        }
    }
}

#[derive(Debug)]
pub enum LibraryServiceError {
    InvalidName,
    InvalidSchedule,
    InvalidConcurrency,
    InvalidLibraryId(String),
    InvalidLibraryOrder(String),
    InvalidRootId(String),
    InvalidKind(String),
    InvalidScraperId,
    InvalidScraperRole(String),
    InvalidScraperOrder(String),
    InvalidChapterSourceId,
    LibraryNotFound,
    LibraryBusy,
    RootNotFound,
    RootNotFoundAfterInsert,
    DuplicateRoot,
    OverlappingRoot,
    Path(RootPathError),
    Storage(StorageError),
}

impl fmt::Display for LibraryServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName => formatter.write_str("library name must be 1-128 characters"),
            Self::InvalidSchedule => {
                formatter.write_str("library schedule must be a valid five-field cron expression")
            }
            Self::InvalidConcurrency => {
                write!(
                    formatter,
                    "library scan concurrency must be between 1 and {MAX_SCAN_CONCURRENCY}; probe concurrency must be between 1 and {MAX_PROBE_CONCURRENCY}"
                )
            }
            Self::InvalidLibraryId(error) => write!(formatter, "invalid library ID: {error}"),
            Self::InvalidLibraryOrder(error) => write!(formatter, "invalid library order: {error}"),
            Self::InvalidRootId(error) => write!(formatter, "invalid library root ID: {error}"),
            Self::InvalidKind(error) => write!(formatter, "invalid library kind: {error}"),
            Self::InvalidScraperId => formatter.write_str("invalid library scraper ID"),
            Self::InvalidScraperRole(error) => {
                write!(formatter, "invalid library scraper role: {error}")
            }
            Self::InvalidScraperOrder(error) => {
                write!(formatter, "invalid library scraper order: {error}")
            }
            Self::InvalidChapterSourceId => {
                formatter.write_str("invalid library chapter source ID")
            }
            Self::LibraryNotFound => formatter.write_str("library not found"),
            Self::LibraryBusy => formatter.write_str("library has an active scan"),
            Self::RootNotFound => formatter.write_str("library root not found"),
            Self::RootNotFoundAfterInsert => {
                formatter.write_str("library root was inserted but could not be read back")
            }
            Self::DuplicateRoot => formatter.write_str("the root path is already in this library"),
            Self::OverlappingRoot => {
                formatter.write_str("the root path overlaps another root in this library")
            }
            Self::Path(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LibraryServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Path(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::InvalidName
            | Self::InvalidSchedule
            | Self::InvalidConcurrency
            | Self::InvalidLibraryId(_)
            | Self::InvalidLibraryOrder(_)
            | Self::InvalidRootId(_)
            | Self::InvalidKind(_)
            | Self::InvalidScraperId
            | Self::InvalidScraperRole(_)
            | Self::InvalidScraperOrder(_)
            | Self::InvalidChapterSourceId
            | Self::LibraryNotFound
            | Self::LibraryBusy
            | Self::RootNotFound
            | Self::RootNotFoundAfterInsert
            | Self::DuplicateRoot
            | Self::OverlappingRoot => None,
        }
    }
}

fn order_library_views(views: &mut [LibraryView], saved_order: &[String]) {
    let positions = saved_order
        .iter()
        .enumerate()
        .map(|(position, library_id)| (library_id.as_str(), position))
        .collect::<HashMap<_, _>>();
    views.sort_by(|left, right| {
        let left_id = left.library.id.to_string();
        let right_id = right.library.id.to_string();
        positions
            .get(left_id.as_str())
            .unwrap_or(&usize::MAX)
            .cmp(positions.get(right_id.as_str()).unwrap_or(&usize::MAX))
            .then_with(|| left.library.name.cmp(&right.library.name))
            .then_with(|| left_id.cmp(&right_id))
    });
}

fn validate_scan_concurrency(value: Option<i64>) -> Result<(), LibraryServiceError> {
    if value.is_some_and(|value| !(1..=MAX_SCAN_CONCURRENCY).contains(&value)) {
        return Err(LibraryServiceError::InvalidConcurrency);
    }
    Ok(())
}

fn validate_probe_concurrency(value: Option<i64>) -> Result<(), LibraryServiceError> {
    if value.is_some_and(|value| !(1..=MAX_PROBE_CONCURRENCY).contains(&value)) {
        return Err(LibraryServiceError::InvalidConcurrency);
    }
    Ok(())
}

fn normalize_library_name(value: &str) -> Result<String, LibraryServiceError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 128 {
        return Err(LibraryServiceError::InvalidName);
    }
    Ok(value.to_owned())
}

fn normalize_scraper_id(value: Option<&str>) -> Result<Option<String>, LibraryServiceError> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.chars().count() > 64
                || !value.chars().all(|character| {
                    character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || "-_.".contains(character)
                })
            {
                Err(LibraryServiceError::InvalidScraperId)
            } else {
                Ok(value.to_owned())
            }
        })
        .transpose()
}

fn normalize_scraper_patch(
    value: Option<Option<String>>,
) -> Result<Option<Option<String>>, LibraryServiceError> {
    match value {
        None => Ok(None),
        Some(None) => Ok(Some(None)),
        Some(Some(value)) => normalize_scraper_id(Some(&value)).map(Some),
    }
}

fn normalize_scrapers(
    requested: &[LibraryScraper],
) -> Result<Vec<LibraryScraper>, LibraryServiceError> {
    if requested.len() > MAX_LIBRARY_SCRAPERS {
        return Err(LibraryServiceError::InvalidScraperOrder(format!(
            "刮削器数量不能超过 {MAX_LIBRARY_SCRAPERS}"
        )));
    }
    let mut seen = HashSet::with_capacity(requested.len());
    let mut normalized = Vec::with_capacity(requested.len());
    for (position, scraper) in requested.iter().enumerate() {
        let scraper_id = normalize_scraper_id(Some(&scraper.scraper_id))?
            .ok_or(LibraryServiceError::InvalidScraperId)?;
        if !seen.insert(scraper_id.clone()) {
            return Err(LibraryServiceError::InvalidScraperOrder(
                "刮削器不能重复".to_owned(),
            ));
        }
        let position = i64::try_from(position).map_err(|_| {
            LibraryServiceError::InvalidScraperOrder("刮削器位置超出范围".to_owned())
        })?;
        if scraper.position != position {
            return Err(LibraryServiceError::InvalidScraperOrder(
                "刮削器位置必须连续且从 0 开始".to_owned(),
            ));
        }
        if position == 0 && scraper.role != LibraryScraperRole::Primary {
            return Err(LibraryServiceError::InvalidScraperRole(
                "首位刮削器必须是 PRIMARY".to_owned(),
            ));
        }
        if position > 0 && scraper.role == LibraryScraperRole::Primary {
            return Err(LibraryServiceError::InvalidScraperRole(
                "只有首位刮削器可以是 PRIMARY".to_owned(),
            ));
        }
        normalized.push(LibraryScraper {
            scraper_id,
            position,
            role: scraper.role,
        });
    }
    Ok(normalized)
}

fn normalize_chapter_source_id(value: Option<&str>) -> Result<Option<String>, LibraryServiceError> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.chars().count() > 128
                || !value.chars().all(|character| {
                    character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || "-_.".contains(character)
                })
            {
                Err(LibraryServiceError::InvalidChapterSourceId)
            } else {
                Ok(value.to_owned())
            }
        })
        .transpose()
}

fn normalize_chapter_source_patch(
    value: Option<Option<String>>,
) -> Result<Option<Option<String>>, LibraryServiceError> {
    match value {
        None => Ok(None),
        Some(None) => Ok(Some(None)),
        Some(Some(value)) => normalize_chapter_source_id(Some(&value)).map(Some),
    }
}

fn normalize_schedule(
    value: Option<Option<String>>,
) -> Result<Option<Option<String>>, LibraryServiceError> {
    value
        .map(|schedule| {
            schedule
                .map(|schedule| {
                    let schedule = schedule.trim().to_owned();
                    if schedule.is_empty()
                        || schedule.chars().count() > MAX_SCHEDULE_LENGTH
                        || validate_cron(&schedule).is_err()
                    {
                        Err(LibraryServiceError::InvalidSchedule)
                    } else {
                        Ok(schedule)
                    }
                })
                .transpose()
        })
        .transpose()
}

impl From<RootPathError> for LibraryServiceError {
    fn from(error: RootPathError) -> Self {
        Self::Path(error)
    }
}

impl From<StorageError> for LibraryServiceError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

fn stored_library(stored: StoredLibrary) -> Result<LibraryRecord, LibraryServiceError> {
    let id = stored
        .id
        .parse()
        .map_err(|error: uuid::Error| LibraryServiceError::InvalidLibraryId(error.to_string()))?;
    let kind = stored
        .kind
        .parse()
        .map_err(|error: crate::library::LibraryKindError| {
            LibraryServiceError::InvalidKind(error.to_string())
        })?;
    Ok(LibraryRecord {
        id,
        name: stored.name,
        kind,
        scraper_id: stored.scraper_id,
        scrapers: stored
            .scrapers
            .into_iter()
            .map(|scraper| {
                Ok(LibraryScraper {
                    scraper_id: scraper.scraper_id,
                    position: scraper.position,
                    role: scraper
                        .role
                        .parse::<LibraryScraperRole>()
                        .map_err(|error| {
                            LibraryServiceError::InvalidScraperRole(error.to_string())
                        })?,
                })
            })
            .collect::<Result<Vec<_>, LibraryServiceError>>()?,
        chapter_source_id: stored.chapter_source_id,
        cover_image_path: stored.cover_image_path,
        cover_image_content_type: stored.cover_image_content_type,
        cover_image_size: stored.cover_image_size,
        cover_image_tag: stored.cover_image_tag,
        is_enabled: stored.is_enabled,
        realtime_watch_enabled: stored.realtime_watch_enabled,
        realtime_metadata_auto_match_enabled: stored.realtime_metadata_auto_match_enabled,
        incremental_schedule: stored.incremental_schedule,
        reconciliation_schedule: stored.reconciliation_schedule,
        metadata_schedule: stored.metadata_schedule,
        media_strategy_json: stored.media_strategy_json,
        scan_concurrency: stored.scan_concurrency,
        probe_concurrency: stored.probe_concurrency,
        last_scan_at: stored.last_scan_at,
    })
}

fn stored_library_root(
    stored: StoredLibraryRoot,
) -> Result<LibraryRootRecord, LibraryServiceError> {
    let id = stored
        .id
        .parse()
        .map_err(|error: uuid::Error| LibraryServiceError::InvalidRootId(error.to_string()))?;
    let library_id = stored
        .library_id
        .parse()
        .map_err(|error: uuid::Error| LibraryServiceError::InvalidLibraryId(error.to_string()))?;
    Ok(LibraryRootRecord {
        id,
        library_id,
        canonical_path: stored.canonical_path.into(),
        display_path: stored.display_path.into(),
        is_available: stored.is_available,
        is_writable: stored.is_writable,
        last_checked_at: stored.last_checked_at,
        unavailable_since: stored.unavailable_since,
        scan_cursor: stored.scan_cursor,
    })
}
