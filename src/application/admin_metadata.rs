use crate::application::{
    candidates::{
        MetadataSelectionError, MetadataSelectionMode, MetadataSelectionReport,
        MetadataSelectionService,
    },
    reidentify::MetadataReidentifyService,
};

pub(crate) async fn select_metadata_candidate(
    selection: &MetadataSelectionService,
    reidentify: Option<&MetadataReidentifyService>,
    item_id: &str,
    candidate_id: &str,
    mode: MetadataSelectionMode,
) -> Result<MetadataSelectionReport, MetadataSelectionError> {
    let report = selection.select(item_id, candidate_id, mode).await?;
    if let Some(reidentify) = reidentify
        && let Err(error) = reidentify
            .enqueue_selected_actor_enrichment(&report.item_id, &report.candidate_id)
            .await
    {
        tracing::warn!(
            item_id = %report.item_id,
            candidate_id = %report.candidate_id,
            %error,
            "selected actor metadata could not be queued"
        );
    }
    Ok(report)
}
