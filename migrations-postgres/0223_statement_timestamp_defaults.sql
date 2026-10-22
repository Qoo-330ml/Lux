-- After 0149 the timestamp convention is `unixepoch()` (statement_timestamp), but these tables
-- were created with the transaction timestamp, so rows written in one transaction share a
-- timestamp and long transactions can look "old". Reset the defaults rather than editing the
-- already-applied migrations (their checksums must not change).
ALTER TABLE media_info_chapters
    ALTER COLUMN created_at SET DEFAULT unixepoch(),
    ALTER COLUMN updated_at SET DEFAULT unixepoch();

ALTER TABLE user_access_blacklist_rules
    ALTER COLUMN created_at SET DEFAULT unixepoch(),
    ALTER COLUMN updated_at SET DEFAULT unixepoch();

ALTER TABLE mail_outbox
    ALTER COLUMN available_at SET DEFAULT unixepoch(),
    ALTER COLUMN created_at SET DEFAULT unixepoch(),
    ALTER COLUMN updated_at SET DEFAULT unixepoch();

ALTER TABLE mail_challenges
    ALTER COLUMN created_at SET DEFAULT unixepoch();
