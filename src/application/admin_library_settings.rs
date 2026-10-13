use crate::{
    application::{
        home::HomeService,
        libraries::{
            AddRootResult, LibraryService, LibraryServiceError, LibrarySettingsPatch, LibraryView,
        },
        plugins::PluginService,
        scanner::{LibraryDeletionGuard, ScanJobError, ScanJobService},
    },
    domain::ids::LibraryId,
    library::{LibraryKind, LibraryRecord, LibraryScraper},
};

#[derive(Debug)]
pub(crate) enum LibrarySettingsWriteError {
    Library(LibraryServiceError),
    ChapterSchedule(crate::application::plugins::PluginServiceError),
}

pub(crate) enum DeleteLibraryError {
    Library(LibraryServiceError),
    Scan(ScanJobError),
}

pub(crate) async fn delete_library(
    libraries: &LibraryService,
    scan_jobs: Option<&ScanJobService>,
    plugins: Option<&PluginService>,
    home: Option<&HomeService>,
    library_id: LibraryId,
) -> Result<Option<LibraryDeletionGuard>, DeleteLibraryError> {
    let deletion_guard = match scan_jobs {
        Some(scan_jobs) => Some(
            scan_jobs
                .prepare_library_deletion(library_id)
                .await
                .map_err(DeleteLibraryError::Scan)?,
        ),
        None => None,
    };
    libraries
        .delete_library(library_id)
        .await
        .map_err(DeleteLibraryError::Library)?;
    if let Some(plugins) = plugins {
        if let Err(error) = plugins.prune_media_info_library_ids().await {
            tracing::error!(
                library_id = %library_id,
                %error,
                "library deleted but STRM media-info plugin configuration could not be pruned"
            );
        }
        if let Err(error) = plugins.prune_danmaku_library_ids().await {
            tracing::error!(
                library_id = %library_id,
                %error,
                "library deleted but danmaku plugin configuration could not be pruned"
            );
        }
    }
    if let Some(home) = home {
        home.invalidate();
    }
    Ok(deletion_guard)
}

pub(crate) async fn create_library(
    libraries: &LibraryService,
    home: Option<&HomeService>,
    plugins: Option<&PluginService>,
    name: &str,
    kind: LibraryKind,
    realtime_watch_enabled: bool,
    scrapers: &[LibraryScraper],
    chapter_source_id: Option<&str>,
    realtime_metadata_auto_match_enabled: bool,
) -> Result<LibraryRecord, LibrarySettingsWriteError> {
    let library = libraries
        .create_library_with_scrapers_and_chapter_source(
            name,
            kind,
            realtime_watch_enabled,
            scrapers,
            chapter_source_id,
            realtime_metadata_auto_match_enabled,
        )
        .await
        .map_err(LibrarySettingsWriteError::Library)?;
    library_configuration_changed(home, plugins).await?;
    Ok(library)
}

pub(crate) async fn update_library_settings(
    libraries: &LibraryService,
    home: Option<&HomeService>,
    plugins: Option<&PluginService>,
    library_id: LibraryId,
    settings: LibrarySettingsPatch,
) -> Result<LibraryView, LibrarySettingsWriteError> {
    let view = libraries
        .update_settings(library_id, settings)
        .await
        .map_err(LibrarySettingsWriteError::Library)?;
    library_configuration_changed(home, plugins).await?;
    Ok(view)
}

async fn library_configuration_changed(
    home: Option<&HomeService>,
    plugins: Option<&PluginService>,
) -> Result<(), LibrarySettingsWriteError> {
    if let Some(home) = home {
        home.invalidate();
    }
    if let Some(plugins) = plugins {
        plugins
            .sync_chapter_detection_scheduled_tasks()
            .await
            .map_err(LibrarySettingsWriteError::ChapterSchedule)?;
    }
    Ok(())
}

pub(crate) async fn add_library_root(
    libraries: &LibraryService,
    home: Option<&HomeService>,
    library_id: LibraryId,
    path: &str,
) -> Result<AddRootResult, LibraryServiceError> {
    let result = libraries.add_root(library_id, path).await?;
    if let Some(home) = home {
        home.invalidate();
    }
    Ok(result)
}

pub(crate) async fn delete_library_root(
    libraries: &LibraryService,
    home: Option<&HomeService>,
    library_id: LibraryId,
    root_id: crate::domain::ids::LibraryRootId,
) -> Result<(), LibraryServiceError> {
    libraries.delete_root(library_id, root_id).await?;
    if let Some(home) = home {
        home.invalidate();
    }
    Ok(())
}
