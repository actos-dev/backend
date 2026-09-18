-- Revert migration 0030 (community moderation, phase 3).
--
-- Order matters: the moderation job table (and its enum) must go first
-- because it references communities; the report index before its column;
-- and the ban column last, after the two partial unique indexes that
-- replaced the primary key are dropped and the primary key is restored.

-- --- Moderation job queue -------------------------------------------------
DROP INDEX idx_moderation_jobs_pending;
DROP TABLE moderation_jobs;
DROP TYPE moderation_job_kind;

-- --- Reports --------------------------------------------------------------
DROP INDEX idx_reports_community_pending;
ALTER TABLE reports DROP COLUMN community_id;

-- --- Bans -----------------------------------------------------------------
DROP INDEX idx_bans_community;
DROP INDEX uq_bans_community;
DROP INDEX uq_bans_global;
ALTER TABLE bans DROP COLUMN community_id;
ALTER TABLE bans ADD PRIMARY KEY (actor_id);

-- --- Owner permissions backfill ------------------------------------------

-- Community-scoped grants only exist because this migration introduced
-- them; removing them restores the phase-2 state. (Global grants are left
-- untouched — they predate this migration.)
DELETE FROM permissions WHERE scope = 'community';

-- Note: the comment community_id backfill is not reversed. A comment that
-- inherited its root post's community is indistinguishable from one that
-- carried it explicitly, and nulling them all would be wrong for comments
-- written after this migration. Phase 2 never wrote the column, so leaving
-- it populated in a reverted database only means comments carry a value
-- the old code ignores.
