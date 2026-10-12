use super::*;
#[path = "metadata_reidentify/fill_missing.rs"]
mod fill_missing;

#[path = "metadata_reidentify/items.rs"]
mod items;

#[path = "metadata_reidentify/lifecycle.rs"]
mod lifecycle;

pub(super) const METADATA_REIDENTIFY_PRIORITY_CASE: &str = "CASE
    WHEN item_type IN ('MOVIE', 'SERIES') THEN 0
    WHEN item_type = 'SEASON' THEN 1
    WHEN item_type = 'EPISODE' THEN 2
    ELSE 3
END";
