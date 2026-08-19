ALTER TABLE person_credits ADD COLUMN provider_ids_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE person_credits ADD COLUMN genres_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE person_credits ADD COLUMN tags_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE person_credits ADD COLUMN production_locations_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE person_credits ADD COLUMN premiere_date TEXT;
ALTER TABLE person_credits ADD COLUMN production_year INTEGER;
ALTER TABLE person_credits ADD COLUMN taglines_json TEXT NOT NULL DEFAULT '[]';
