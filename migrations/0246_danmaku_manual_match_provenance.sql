-- ADR-052 P5: record what a danmaku job is and which provider the administrator chose.
--
-- `kind` tells a manual match (one administrator-selected episode) apart from a library sweep, and
-- `provider` carries the chosen provider label on the job item, so the worker no longer has to infer
-- either of them from a `danmaku_tracks` row. Existing rows are library sweeps without a recorded
-- provider, which is exactly the documented default.
ALTER TABLE danmaku_match_jobs
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'SWEEP'
        CHECK (kind IN ('SWEEP', 'MANUAL'));

ALTER TABLE danmaku_match_job_items
    ADD COLUMN provider TEXT;
