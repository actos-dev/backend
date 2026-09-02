-- 0020_trust_levels.up.sql'i ters sırada geri alır.

DROP INDEX idx_reports_resolved_recent;

ALTER TABLE actors DROP CONSTRAINT ck_actors_trust_level_range;
ALTER TABLE actors DROP COLUMN trust_level;
