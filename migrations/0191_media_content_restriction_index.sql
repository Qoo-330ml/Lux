CREATE TABLE content_rating_levels (
    rating_system TEXT NOT NULL CHECK (rating_system IN ('MPA', 'TVPG', 'BBFC')),
    rating_code TEXT NOT NULL CHECK (length(trim(rating_code)) BETWEEN 1 AND 16),
    rating_order INTEGER NOT NULL CHECK (rating_order >= 0),
    PRIMARY KEY (rating_system, rating_code),
    CHECK (
        (rating_system = 'MPA' AND rating_code = 'G' AND rating_order = 0) OR
        (rating_system = 'MPA' AND rating_code = 'PG' AND rating_order = 1) OR
        (rating_system = 'MPA' AND rating_code = 'PG-13' AND rating_order = 2) OR
        (rating_system = 'MPA' AND rating_code = 'R' AND rating_order = 3) OR
        (rating_system = 'MPA' AND rating_code = 'NC-17' AND rating_order = 4) OR
        (rating_system = 'TVPG' AND rating_code = 'TV-Y' AND rating_order = 0) OR
        (rating_system = 'TVPG' AND rating_code = 'TV-G' AND rating_order = 1) OR
        (rating_system = 'TVPG' AND rating_code = 'TV-Y7' AND rating_order = 2) OR
        (rating_system = 'TVPG' AND rating_code = 'TV-PG' AND rating_order = 3) OR
        (rating_system = 'TVPG' AND rating_code = 'TV-14' AND rating_order = 4) OR
        (rating_system = 'TVPG' AND rating_code = 'TV-MA' AND rating_order = 5) OR
        (rating_system = 'BBFC' AND rating_code = 'U' AND rating_order = 0) OR
        (rating_system = 'BBFC' AND rating_code = 'PG' AND rating_order = 1) OR
        (rating_system = 'BBFC' AND rating_code IN ('12', '12A') AND rating_order = 2) OR
        (rating_system = 'BBFC' AND rating_code = '15' AND rating_order = 3) OR
        (rating_system = 'BBFC' AND rating_code = '18' AND rating_order = 4) OR
        (rating_system = 'BBFC' AND rating_code = 'R18' AND rating_order = 5)
    )
);

INSERT INTO content_rating_levels (rating_system, rating_code, rating_order) VALUES
    ('MPA', 'G', 0),
    ('MPA', 'PG', 1),
    ('MPA', 'PG-13', 2),
    ('MPA', 'R', 3),
    ('MPA', 'NC-17', 4),
    ('TVPG', 'TV-Y', 0),
    ('TVPG', 'TV-G', 1),
    ('TVPG', 'TV-Y7', 2),
    ('TVPG', 'TV-PG', 3),
    ('TVPG', 'TV-14', 4),
    ('TVPG', 'TV-MA', 5),
    ('BBFC', 'U', 0),
    ('BBFC', 'PG', 1),
    ('BBFC', '12', 2),
    ('BBFC', '12A', 2),
    ('BBFC', '15', 3),
    ('BBFC', '18', 4),
    ('BBFC', 'R18', 5);

CREATE TABLE media_content_restriction_index (
    item_id TEXT PRIMARY KEY NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    certification_code TEXT CHECK (
        certification_code IS NULL OR (
            certification_code = upper(trim(certification_code)) AND
            length(certification_code) BETWEEN 1 AND 128
        )
    ),
    indexed_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE media_content_restriction_tags (
    item_id TEXT NOT NULL REFERENCES media_content_restriction_index(item_id) ON DELETE CASCADE,
    tag TEXT NOT NULL CHECK (length(trim(tag)) BETWEEN 1 AND 128),
    PRIMARY KEY (item_id, tag)
);

CREATE INDEX idx_media_content_restriction_index_certification
    ON media_content_restriction_index(certification_code, item_id);
CREATE INDEX idx_media_content_restriction_tags_tag
    ON media_content_restriction_tags(tag, item_id);
