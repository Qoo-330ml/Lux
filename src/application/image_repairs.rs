use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use tokio::fs;

use crate::{
    application::images::write_image_atomically,
    storage::{Database, StorageError, StoredItemImage, StoredItemImagePathConflict},
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EpisodeImagePathRepairReport {
    pub repaired: usize,
    pub skipped: usize,
}

pub async fn repair_episode_image_path_conflicts(
    database: &Database,
) -> Result<EpisodeImagePathRepairReport, StorageError> {
    let conflicts = database.list_item_image_path_conflicts().await?;
    let mut groups = HashMap::<(String, String), Vec<StoredItemImagePathConflict>>::new();
    for conflict in conflicts {
        groups
            .entry((conflict.item_id.clone(), conflict.local_path.clone()))
            .or_default()
            .push(conflict);
    }
    let mut item_ids = groups
        .keys()
        .map(|(item_id, _)| item_id.clone())
        .collect::<Vec<_>>();
    item_ids.sort_unstable();
    item_ids.dedup();
    let images_by_item = database.list_item_images_by_ids(&item_ids).await?;

    let mut report = EpisodeImagePathRepairReport::default();
    for conflicts in groups.into_values() {
        let Some(thumbnail) = conflicts
            .iter()
            .find(|conflict| conflict.image_type.eq_ignore_ascii_case("THUMB"))
        else {
            report.skipped = report.skipped.saturating_add(1);
            continue;
        };
        let source = PathBuf::from(&thumbnail.local_path);
        let source_metadata = match fs::symlink_metadata(&source).await {
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() > 0 =>
            {
                metadata
            }
            Ok(_) | Err(_) => {
                report.skipped = report.skipped.saturating_add(1);
                continue;
            }
        };
        let source_bytes = match fs::read(&source).await {
            Ok(bytes) => bytes,
            Err(_) => {
                report.skipped = report.skipped.saturating_add(1);
                continue;
            }
        };
        if u64::try_from(source_bytes.len()).ok() != Some(source_metadata.len()) {
            report.skipped = report.skipped.saturating_add(1);
            continue;
        }

        let indexed_images = images_by_item
            .get(&thumbnail.item_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut repaired = false;
        for variant in 0..1000_usize {
            let Some(target) = episode_thumbnail_repair_target(&source, variant) else {
                break;
            };
            if image_path_is_in_use(indexed_images, &target, &thumbnail.id) {
                continue;
            }
            let target_exists = match fs::symlink_metadata(&target).await {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    match fs::read(&target).await {
                        Ok(bytes) if bytes == source_bytes => true,
                        Ok(_) => continue,
                        Err(_) => continue,
                    }
                }
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(_) => continue,
            };
            if !target_exists {
                match fs::hard_link(&source, &target).await {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(_) => {
                        if write_image_atomically(&target, &source_bytes)
                            .await
                            .is_err()
                        {
                            continue;
                        }
                    }
                }
            }
            if database
                .item_image_path_is_in_use(&thumbnail.item_id, &target, &thumbnail.id)
                .await?
            {
                continue;
            }
            if database
                .update_item_image_local_path(&thumbnail.id, &target)
                .await?
            {
                report.repaired = report.repaired.saturating_add(1);
                repaired = true;
                break;
            }
        }
        if !repaired {
            report.skipped = report.skipped.saturating_add(1);
        }
    }
    Ok(report)
}

fn episode_thumbnail_repair_target(path: &Path, variant: usize) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    let lower_stem = stem.to_ascii_lowercase();
    let base = lower_stem.rfind("-thumb").and_then(|position| {
        let suffix = &stem[position + "-thumb".len()..];
        (suffix.is_empty() || suffix.chars().all(|character| character.is_ascii_digit()))
            .then_some(&stem[..position])
    });
    let base = base.unwrap_or(stem);
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("jpg");
    let suffix = if variant == 0 {
        "-thumbnail".to_owned()
    } else {
        format!("-thumbnail-{variant}")
    };
    Some(path.with_file_name(format!("{base}{suffix}.{extension}")))
}

fn image_path_is_in_use(
    images: &[StoredItemImage],
    local_path: &Path,
    excluded_image_id: &str,
) -> bool {
    let local_path = local_path.to_string_lossy();
    images
        .iter()
        .any(|image| image.id != excluded_image_id && image.local_path == local_path)
}

#[cfg(test)]
mod tests {
    use super::image_path_is_in_use;
    use crate::storage::StoredItemImage;
    use std::path::Path;

    fn image(id: &str, local_path: &str) -> StoredItemImage {
        StoredItemImage {
            id: id.to_owned(),
            item_id: "item".to_owned(),
            image_type: "THUMB".to_owned(),
            image_index: 0,
            local_path: local_path.to_owned(),
            file_size: None,
            content_tag: None,
            source: "TEST".to_owned(),
            root_path: None,
        }
    }

    #[test]
    fn image_path_usage_excludes_the_image_being_repaired() {
        let images = vec![
            image("thumb", "/media/thumb.jpg"),
            image("other", "/media/thumb.jpg"),
        ];
        assert!(image_path_is_in_use(
            &images,
            Path::new("/media/thumb.jpg"),
            "thumb"
        ));
        assert!(!image_path_is_in_use(
            &images,
            Path::new("/media/other.jpg"),
            "thumb"
        ));
        assert!(!image_path_is_in_use(
            &images[..1],
            Path::new("/media/thumb.jpg"),
            "thumb"
        ));
    }
}
