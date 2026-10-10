ALTER TABLE scan_local_metadata_batches
    ADD COLUMN non_retryable_item_ids_json TEXT NOT NULL DEFAULT '[]';

ALTER TABLE scan_local_metadata_backfills
    ADD COLUMN non_retryable_item_ids_json TEXT NOT NULL DEFAULT '[]';
