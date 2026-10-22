-- Record how many rows a library deletion job has to remove so the admin view can
-- show a percentage instead of an ever-growing counter. Existing jobs backfill on
-- their next run from the remaining rows plus the rows already processed.
ALTER TABLE library_deletion_jobs
    ADD COLUMN total_count BIGINT;
