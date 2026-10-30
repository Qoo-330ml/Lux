use crate::application::{catalog::CatalogService, home::HomeService};

/// Directory-level invalidation for changes every user can see.
///
/// Use this whenever the change cannot be proven to be private to one user: scans, metadata,
/// media merges and deletions, library lifecycle and server-wide settings.
pub(crate) fn invalidate_home(home: Option<&HomeService>) {
    if let Some(home) = home {
        home.invalidate_directory();
    }
}

/// User-level invalidation for state only the owning user can see.
///
/// The caller must pass the canonical user id; other users keep their cached home snapshots.
pub(crate) fn invalidate_home_for_user(home: Option<&HomeService>, user_id: &str) {
    if let Some(home) = home {
        home.invalidate_user(user_id);
    }
}

pub(crate) fn invalidate_media_catalog(
    home: Option<&HomeService>,
    catalog: Option<&CatalogService>,
) {
    if let Some(home) = home {
        home.invalidate_directory();
    } else if let Some(catalog) = catalog {
        catalog.invalidate_library_pages();
    }
}
