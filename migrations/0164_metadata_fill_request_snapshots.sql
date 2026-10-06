ALTER TABLE metadata_reidentify_job_items
    ADD COLUMN request_fingerprint BLOB;

ALTER TABLE metadata_reidentify_job_items
    ADD COLUMN request_capabilities_json TEXT NOT NULL DEFAULT '[]';

ALTER TABLE metadata_reidentify_job_items
    ADD COLUMN claimed_request_fingerprint BLOB;

ALTER TABLE metadata_reidentify_job_items
    ADD COLUMN claimed_request_capabilities_json TEXT NOT NULL DEFAULT '[]';
