use std::collections::HashMap;

use crate::storage::{Database, StorageError, StoredJobActivityItem};

const ACTIVITY_JOB_LIMIT: i64 = 100;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AdminTaskActivity {
    Scan {
        id: String,
        task_type: String,
        library_id: String,
        status: String,
        processed_count: i64,
        total_count: i64,
        discovery_completed: bool,
        cancel_requested: bool,
        current_item: Option<String>,
        scan_phase: String,
    },
    Metadata {
        id: String,
        task_type: String,
        library_id: Option<String>,
        status: String,
        processed_count: i64,
        total_count: i64,
        cancel_requested: bool,
        current_item: Option<String>,
    },
    Strm {
        id: String,
        library_id: String,
        status: String,
        processed_count: i64,
        total_count: i64,
        cancel_requested: bool,
    },
    Chapter {
        id: String,
        library_id: String,
        status: String,
        processed_count: i64,
        total_count: i64,
        cancel_requested: bool,
        current_item: Option<String>,
    },
    Danmaku {
        id: String,
        library_id: String,
        status: String,
        processed_count: i64,
        total_count: i64,
        cancel_requested: bool,
        current_item: Option<String>,
    },
    Cover {
        id: String,
        library_id: String,
        status: String,
        processed_count: i64,
        total_count: i64,
    },
}

impl AdminTaskActivity {
    fn id(&self) -> &str {
        match self {
            Self::Scan { id, .. }
            | Self::Metadata { id, .. }
            | Self::Strm { id, .. }
            | Self::Chapter { id, .. }
            | Self::Danmaku { id, .. }
            | Self::Cover { id, .. } => id,
        }
    }

    fn status(&self) -> &str {
        match self {
            Self::Scan { status, .. }
            | Self::Metadata { status, .. }
            | Self::Strm { status, .. }
            | Self::Chapter { status, .. }
            | Self::Danmaku { status, .. }
            | Self::Cover { status, .. } => status,
        }
    }
}

pub(crate) async fn list_admin_task_activity(
    database: &Database,
) -> Result<Vec<AdminTaskActivity>, StorageError> {
    let mut activities = Vec::new();

    for job in database
        .list_scan_jobs_for_activity(ACTIVITY_JOB_LIMIT)
        .await?
    {
        activities.push(AdminTaskActivity::Scan {
            id: job.id,
            task_type: job.job_type,
            library_id: job.library_id,
            status: job.status,
            processed_count: job.processed_count,
            total_count: job.total_count,
            discovery_completed: job.discovery_completed,
            cancel_requested: job.cancel_requested,
            current_item: job.current_item,
            scan_phase: job.scan_phase,
        });
    }

    let metadata_jobs = database
        .list_metadata_reidentify_jobs_for_activity(ACTIVITY_JOB_LIMIT)
        .await?;
    let metadata_labels = activity_item_labels(
        database
            .list_current_metadata_reidentify_items(
                &metadata_jobs
                    .iter()
                    .map(|job| job.id.clone())
                    .collect::<Vec<_>>(),
            )
            .await?,
    );
    for job in metadata_jobs {
        activities.push(AdminTaskActivity::Metadata {
            current_item: metadata_labels.get(&job.id).cloned(),
            id: job.id,
            task_type: job.mode,
            library_id: job.library_id,
            status: job.status,
            processed_count: job.processed_count,
            total_count: job.total_count,
            cancel_requested: job.cancel_requested,
        });
    }

    for status in ["PENDING", "RUNNING"] {
        for job in database
            .list_strm_probe_jobs(Some(status), 0, ACTIVITY_JOB_LIMIT)
            .await?
        {
            activities.push(AdminTaskActivity::Strm {
                id: job.id,
                library_id: job.library_id,
                status: job.status,
                processed_count: job.processed_count,
                total_count: job.total_count,
                cancel_requested: job.cancel_requested,
            });
        }

        let chapter_jobs = database
            .list_chapter_detection_jobs(Some(status), 0, ACTIVITY_JOB_LIMIT)
            .await?;
        let chapter_labels = activity_item_labels(
            database
                .list_current_chapter_detection_items(
                    &chapter_jobs
                        .iter()
                        .map(|job| job.id.clone())
                        .collect::<Vec<_>>(),
                )
                .await?,
        );
        for job in chapter_jobs {
            activities.push(AdminTaskActivity::Chapter {
                current_item: chapter_labels.get(&job.id).cloned(),
                id: job.id,
                library_id: job.library_id,
                status: job.status,
                processed_count: job.processed_count,
                total_count: job.total_count,
                cancel_requested: job.cancel_requested,
            });
        }

        let danmaku_jobs = database
            .list_danmaku_match_jobs(Some(status), 0, ACTIVITY_JOB_LIMIT)
            .await?;
        let danmaku_labels = activity_item_labels(
            database
                .list_current_danmaku_match_items(
                    &danmaku_jobs
                        .iter()
                        .map(|job| job.id.clone())
                        .collect::<Vec<_>>(),
                )
                .await?,
        );
        for job in danmaku_jobs {
            activities.push(AdminTaskActivity::Danmaku {
                current_item: danmaku_labels.get(&job.id).cloned(),
                id: job.id,
                library_id: job.library_id,
                status: job.status,
                processed_count: job.processed_count,
                total_count: job.total_count,
                cancel_requested: job.cancel_requested,
            });
        }

        for job in database
            .list_library_cover_jobs(Some(status), 0, ACTIVITY_JOB_LIMIT)
            .await?
        {
            activities.push(AdminTaskActivity::Cover {
                id: job.id,
                library_id: job.library_id,
                status: job.status,
                processed_count: job.processed_count,
                total_count: job.total_count,
            });
        }
    }

    activities.sort_by(|left, right| {
        right
            .status()
            .cmp(left.status())
            .then_with(|| left.id().cmp(right.id()))
    });
    Ok(activities)
}

fn activity_item_labels(items: Vec<(String, StoredJobActivityItem)>) -> HashMap<String, String> {
    items
        .into_iter()
        .map(|(job_id, item)| (job_id, activity_item_label(&item)))
        .collect()
}

fn activity_item_label(item: &StoredJobActivityItem) -> String {
    if item.item_type != "EPISODE" {
        return item.title.clone();
    }
    let series_title = item
        .series_title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(item.title.as_str());
    let episode_number = item.episode_number.map(|number| format!("E{number:02}"));
    let season_number = item.season_number.map(|number| format!("S{number:02}"));
    let season_episode = match (season_number, episode_number) {
        (Some(season), Some(episode)) => Some(format!("{season}{episode}")),
        (Some(season), None) => Some(season),
        (None, Some(episode)) => Some(episode),
        (None, None) => None,
    };
    [
        Some(series_title.to_owned()),
        season_episode,
        Some(item.title.clone()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}
