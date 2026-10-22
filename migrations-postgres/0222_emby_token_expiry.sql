-- Emby access tokens now expire unless refreshed. Existing tokens receive a full window from
-- the upgrade so no client is signed out by the migration itself.
ALTER TABLE access_tokens ADD COLUMN expires_at BIGINT;

UPDATE access_tokens
   SET expires_at = unixepoch() + 15552000
 WHERE revoked_at IS NULL;
