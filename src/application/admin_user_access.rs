use crate::{
    application::{admin_cache, home::HomeService},
    storage::Database,
};
use std::collections::HashSet;

#[derive(Debug)]
pub(crate) enum SetLibraryAccessError {
    UserNotFound,
    LibraryNotFound,
    Storage,
}

pub(crate) async fn set_library_access(
    database: &Database,
    home: Option<&HomeService>,
    user_id: &str,
    library_id: &str,
    can_view: bool,
) -> Result<(), SetLibraryAccessError> {
    if !database
        .user_exists(user_id)
        .await
        .map_err(|_| SetLibraryAccessError::Storage)?
    {
        return Err(SetLibraryAccessError::UserNotFound);
    }
    if !database
        .library_exists(library_id)
        .await
        .map_err(|_| SetLibraryAccessError::Storage)?
    {
        return Err(SetLibraryAccessError::LibraryNotFound);
    }
    database
        .set_user_library_access(user_id, library_id, can_view)
        .await
        .map_err(|_| SetLibraryAccessError::Storage)?;
    admin_cache::invalidate_home(home);
    Ok(())
}

#[derive(Debug)]
pub(crate) enum ReplaceLibraryAccessError {
    UserNotFound,
    LibraryNotFound,
    InvalidMode,
    DuplicateLibrary,
    Storage,
}

pub(crate) async fn replace_library_access(
    database: &Database,
    home: Option<&HomeService>,
    user_id: &str,
    requested_mode: &str,
    library_ids: &[String],
) -> Result<(String, Vec<String>), ReplaceLibraryAccessError> {
    if !database
        .user_exists(user_id)
        .await
        .map_err(|_| ReplaceLibraryAccessError::Storage)?
    {
        return Err(ReplaceLibraryAccessError::UserNotFound);
    }
    if !matches!(requested_mode, "ALL" | "SELECTED" | "NONE") {
        return Err(ReplaceLibraryAccessError::InvalidMode);
    }

    let mode = if requested_mode == "SELECTED" && library_ids.is_empty() {
        "NONE"
    } else {
        requested_mode
    };
    let selected_ids = if mode == "SELECTED" {
        let mut seen = HashSet::with_capacity(library_ids.len());
        for library_id in library_ids {
            if !seen.insert(library_id) {
                return Err(ReplaceLibraryAccessError::DuplicateLibrary);
            }
        }
        let enabled_library_ids = database
            .list_enabled_library_ids()
            .await
            .map_err(|_| ReplaceLibraryAccessError::Storage)?;
        if library_ids
            .iter()
            .any(|library_id| !enabled_library_ids.contains(library_id))
        {
            return Err(ReplaceLibraryAccessError::LibraryNotFound);
        }
        library_ids.to_vec()
    } else {
        Vec::new()
    };
    let updated = database
        .replace_user_library_access_mode(user_id, mode, &selected_ids)
        .await
        .map_err(|_| ReplaceLibraryAccessError::Storage)?;
    if !updated {
        return Err(ReplaceLibraryAccessError::UserNotFound);
    }
    admin_cache::invalidate_home(home);
    Ok((mode.to_owned(), selected_ids))
}
