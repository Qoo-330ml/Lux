CREATE TABLE user_content_restrictions (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    rating_system TEXT NOT NULL
        CHECK (length(trim(rating_system)) BETWEEN 1 AND 64),
    maximum_rating TEXT
        CHECK (maximum_rating IS NULL OR length(trim(maximum_rating)) BETWEEN 1 AND 64),
    allow_unrated BIGINT NOT NULL CHECK (allow_unrated IN (0, 1)),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE user_content_restriction_tags (
    user_id TEXT NOT NULL REFERENCES user_content_restrictions(user_id) ON DELETE CASCADE,
    tag_kind TEXT NOT NULL CHECK (tag_kind IN ('ALLOW', 'BLOCK')),
    tag TEXT NOT NULL CHECK (length(trim(tag)) BETWEEN 1 AND 128),
    PRIMARY KEY (user_id, tag_kind, tag)
);
