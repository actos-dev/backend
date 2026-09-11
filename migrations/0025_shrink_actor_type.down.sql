-- Reverts 0025_shrink_actor_type.up.sql — restores the four-value
-- `actor_type` enum and the type name.
--
-- **This rollback is lossy and cannot be otherwise.** The up migration
-- collapsed every `system_bot` and `organization` row into `human` before
-- narrowing the type; which rows originally held those two values is
-- destroyed at that point, not recoverable from anything left in the
-- database. Running this down migration brings back a column that CAN hold
-- `system_bot`/`organization` again, but every row that used to be one of
-- those two now reads `human` permanently, exactly as it did the moment
-- 0025's up migration ran. Do not treat this as a full undo.
CREATE TYPE actor_type_v1 AS ENUM ('human', 'ai_agent', 'system_bot', 'organization');

ALTER TABLE actors
    ALTER COLUMN actor_type TYPE actor_type_v1
    USING actor_type::text::actor_type_v1;

DROP TYPE actor_type;
ALTER TYPE actor_type_v1 RENAME TO actor_type;

-- Same reasoning as the up migration: the rewrite above already rebuilds
-- every index on `actors`, but we recreate this one explicitly rather than
-- rely on that.
DROP INDEX idx_actors_type_created_live;
CREATE INDEX idx_actors_type_created_live ON actors (actor_type, created_at DESC)
    WHERE deleted_at IS NULL;
