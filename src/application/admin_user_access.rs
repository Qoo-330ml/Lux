use crate::{
    application::{admin_cache, home::HomeService},
    storage::Database,
};

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
