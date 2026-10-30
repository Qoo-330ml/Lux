use crate::{
    application::{
        home::HomeService,
        libraries::{
            AddRootResult, LibraryService, LibraryServiceError, LibrarySettingsPatch, LibraryView,
        },
        plugins::PluginService,
    },
    domain::ids::LibraryId,
    library::{LibraryKind, LibraryRecord, LibraryScraper},
};

#[derive(Debug)]
pub(crate) enum LibrarySettingsWriteError {
    Library(LibraryServiceError),
    ChapterSchedule(crate::application::plugins::PluginServiceError),
}

pub(crate) struct CreateLibraryInput<'a> {
    pub(crate) name: &'a str,
    pub(crate) kind: LibraryKind,
    pub(crate) realtime_watch_enabled: bool,
    pub(crate) scrapers: &'a [LibraryScraper],
    pub(crate) chapter_source_id: Option<&'a str>,
    pub(crate) realtime_metadata_auto_match_enabled: bool,
}

pub(crate) async fn create_library(
    libraries: &LibraryService,
    home: Option<&HomeService>,
    plugins: Option<&PluginService>,
    input: CreateLibraryInput<'_>,
) -> Result<LibraryRecord, LibrarySettingsWriteError> {
    let library = libraries
        .create_library_with_scrapers_and_chapter_source(
            input.name,
            input.kind,
            input.realtime_watch_enabled,
            input.scrapers,
            input.chapter_source_id,
            input.realtime_metadata_auto_match_enabled,
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
        home.invalidate_directory();
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
        home.invalidate_directory();
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
        home.invalidate_directory();
    }
    Ok(())
}
