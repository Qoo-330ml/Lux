use crate::application::{catalog::CatalogService, home::HomeService};

pub(crate) fn invalidate_home(home: Option<&HomeService>) {
    if let Some(home) = home {
        home.invalidate();
    }
}

pub(crate) fn invalidate_media_catalog(
    home: Option<&HomeService>,
    catalog: Option<&CatalogService>,
) {
    if let Some(home) = home {
        home.invalidate();
    } else if let Some(catalog) = catalog {
        catalog.invalidate_library_pages();
    }
}
