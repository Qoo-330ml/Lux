-- no-transaction
-- SQLite requires the media_items table rebuild to run on the same connection
-- with foreign-key checks temporarily disabled; Database::connect performs it
-- immediately after this version marker is applied.
SELECT 1;
