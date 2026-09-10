use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use tokio::fs;

use crate::{
    application::images::write_image_atomically,
    storage::{Database, StorageError, StoredItemImagePathConflict},
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

        let mut repaired = false;
        for variant in 0..1000_usize {
            let Some(target) = episode_thumbnail_repair_target(&source, variant) else {
                break;
            };
            if database
                .item_image_path_is_in_use(&thumbnail.item_id, &target, &thumbnail.id)
                .await?
            {
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
