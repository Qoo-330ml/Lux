mod repository;

#[allow(dead_code)] // Scanner jobs build these values after evaluating local metadata.
pub(crate) struct NewItemMetadataCompletenessResult<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) capability: &'a str,
    pub(crate) input_fingerprint: &'a [u8],
    pub(crate) is_missing: bool,
    pub(crate) checked_at: i64,
}

#[derive(Clone, Copy)]
pub(crate) struct NewItemMetadataCompletenessCheck<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) capability: &'a str,
    pub(crate) input_fingerprint: &'a [u8],
}

#[allow(dead_code)] // Scanner jobs use the queued IDs to wake the existing worker.
#[derive(Debug, Default)]
pub(crate) struct ItemMetadataCompletenessCommit {
    pub(crate) updated_count: usize,
    pub(crate) scheduled_job_ids: Vec<String>,
}

pub(crate) use repository::MAX_PLAYBACK_SESSION_WINDOW_SECONDS;
pub use repository::{
    Database, DatabaseDiagnosticsSnapshot, DatabaseLifecycleCleanupReport, PersonListOptions,
    PersonSort, StorageError,
};

#[allow(unused_imports)]
pub(crate) use repository::{
    CatalogFilterQuery, CatalogSort, ChapterDetectionOutcomeUpdate,
    ChapterDetectionSourceStateUpdate, DEFAULT_PLAYED_PERCENT, DashboardStats,
    DatabasePoolSnapshot, DevicePairingRedeemResult, EmbyMigrationHandledItemBatch,
    EmbyMigrationImportRecordBatch, EmbyMigrationItemMatchBatch, EmbyMigrationItemPageBatch,
    EmbyMigrationJobProgress, EmbyMigrationPersonFavoriteBatch,
    EmbyMigrationPersonFavoriteStateBatch, EmbyMigrationUserItemStateBatch,
    EmbyMigrationUserItemStateFields, ExternalSubtitleUpdate, FilesystemEntryMove,
    ItemImageBatchInsert, ItemImageInsert, ItemImageMetadata, LibrarySettingsUpdate,
    MANIFEST_POSTPROCESSING_TARGET_PAGE_SIZE, ManifestDeltaBatchCommit,
    ManifestDeltaBatchCommitResult, ManifestDiscoveryCommitResult, ManifestExistingFileUpdate,
    ManifestPostprocessingTargetBatchResult, ManifestPostprocessingTargetPage,
    MediaInfoChapterUpdate, MediaMetadataUpdate, MediaProbeUpdate, MediaStreamUpdate,
    MetadataCapabilityResult, MetadataImageAttemptUpdate, MetadataImageUnavailable,
    MigrationMediaIdentityLookup, MigrationPersonIdentityLookup, NewAccessToken, NewAuditEvent,
    NewChapterDetectionJob, NewChapterDetectionJobItem, NewCollection, NewDanmakuMatchJob,
    NewDanmakuTrack, NewDeviceAccessToken, NewDevicePairing, NewEmbyMigrationJob, NewEpisodeFile,
    NewFilesystemEntry, NewHierarchyItem, NewLibrary, NewLibraryRoot, NewMediaChapterMarker,
    NewMediaItem, NewMediaSource, NewMetadataCandidate, NewMovieFile, NewNotificationDestination,
    NewNotificationEvent, NewPersonCredit, NewPlaybackEvent, NewScanLocalMetadataBatch,
    NewScanManifest, NewScanManifestDelta, NewScanManifestDiscoveryChunk, NewScanManifestEntry,
    NewScanManifestFilesystemEntry, NewScanManifestIndexedFile, NewScanManifestPositiveIndex,
    NewScanManifestRoot, NewScanManifestSeenFilesystemEntry, NewScanManifestSidecarEntry,
    NewScanManifestUnresolvedFile, NewStrmProbeJob, NewWebPlaybackEvent, NewWebPlaybackSession,
    PLAYBACK_SESSION_STALE_AFTER_SECONDS, PersonMatchCandidateRestore, ReconciliationBatchCommit,
    ReconciliationBatchCommitResult, ResumeItemsQuery, SelectedMetadataUpdate,
    StoredAccessTokenDevice, StoredActivityEvent, StoredCanonicalPerson,
    StoredCanonicalPersonMatch, StoredCatalogDetail, StoredCatalogImageTag,
    StoredCatalogItemCounts, StoredCatalogRow, StoredChapterDetectionItem,
    StoredChapterDetectionJob, StoredChapterDetectionSource, StoredChapterDetectionSourceState,
    StoredCollectionRefresh, StoredDanmakuMatchItem, StoredDanmakuMatchJob, StoredDanmakuSource,
    StoredDownloadSource, StoredEmbyCollection, StoredEmbyMigrationImportRecord,
    StoredEmbyMigrationItemMatch, StoredEmbyMigrationJob, StoredEmbyMigrationPersonFavorite,
    StoredEmbyMigrationSource, StoredEmbyMigrationUserBinding, StoredEmbyMigrationUserLink,
    StoredEpisodeIdentityCandidate, StoredExternalSubtitle, StoredFilesystemEntry,
    StoredFolderScanPath, StoredImageIdentity, StoredItemImage, StoredItemImageCandidate,
    StoredItemImagePathConflict, StoredItemMetadataCompleteness, StoredItemScanPath,
    StoredItemSourceLocator, StoredJobActivityItem, StoredLibrary, StoredLibraryCoverJob,
    StoredLibraryIdentity, StoredLibraryPoster, StoredLibraryRoot, StoredLibraryScraper,
    StoredMediaChapter, StoredMediaItem, StoredMediaItemKind, StoredMediaMerge,
    StoredMediaMetadata, StoredMediaSourcePath, StoredMediaWritebackContext,
    StoredMetadataCandidate, StoredMetadataCapabilityAttempt, StoredMetadataImageAttempt,
    StoredMetadataReidentifyItem, StoredMetadataReidentifyJob, StoredMigrationMediaIdentity,
    StoredMigrationPersonIdentity, StoredMovieIdentity, StoredNotificationDelivery,
    StoredNotificationDestination, StoredPersonCredit, StoredPersonIdentityMove,
    StoredPersonIndexRebuildJob, StoredPersonMatchCandidate, StoredPlaybackHistoryEvent,
    StoredPlaybackSession, StoredPlaybackSource, StoredReconciliationScanEntry, StoredScanJob,
    StoredScanJobCounts, StoredScanJobPath, StoredScanLocalMetadataBackfillPage,
    StoredScanLocalMetadataBatch, StoredScanLocalMetadataSource, StoredScanManifest,
    StoredScanManifestDelta, StoredScanManifestDiffCandidate, StoredScanManifestDirectory,
    StoredScanManifestFilesystemBaseline, StoredScanManifestPostprocessingRoot,
    StoredScanManifestRemovalCandidate, StoredScheduledTaskConfig, StoredScheduledTaskPlan,
    StoredScheduledTaskPlanLibrary, StoredSeriesMetadataSource, StoredStrmMediaSource,
    StoredStrmProbeJob, StoredSubtitleStream, StoredThumbnailScraperRetry, StoredThumbnailSource,
    StoredUser, StoredUserItemState, StoredWebPlaybackSession, StoredWebSession,
    StoredWebSessionSummary, UpdateNotificationDestination, UpdateUser, WebPlaybackEventClaim,
    WebPlaybackTranscodingDetails, is_lite_manifest_discovery, movie_parent_folder_identity,
    recommendation_batch_key_at,
};
