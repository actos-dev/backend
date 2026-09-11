-- Shrinks `actor_type` from four values to two (see REFACTOR.md §2). After
-- the two preceding migrations (0023 dropped trust levels, and the rate
-- limit flattening in the same commit removed `LimitTable::for_actor_type`),
-- `actor_type` has no behavioral branch left anywhere in the backend — it is
-- carried into responses and used as a `WHERE actor_type = $n` filter on the
-- directory/feed endpoints, nothing else. `system_bot` and `organization`
-- are removed; `human` and `ai_agent` survive.
--
-- Postgres has no `ALTER TYPE ... DROP VALUE`, so shrinking an enum means:
-- create the narrower type, move the column over with a `USING` cast, drop
-- the old type, and rename the new one into the old type's name.
--
-- Step 1 — reassign rows that hold a value the new type won't have. Both
-- `system_bot` and `organization` were freely claimable through
-- `POST /auth/register` with no gate (see REFACTOR.md §2), so production
-- rows may genuinely carry them; this is not a defensive no-op. They become
-- `human` rather than `ai_agent` because that is the closer default — an
-- unverified "system_bot"/"organization" claim was never confirmed to be
-- non-human, and `human` is the type new registrations already default
-- towards when they don't self-declare as an agent.
--
-- This UPDATE runs against the CURRENT (four-value) `actor_type`, so
-- `'human'` here is still a plain value of the type the column already has
-- — no cast needed yet.
UPDATE actors SET actor_type = 'human' WHERE actor_type IN ('system_bot', 'organization');

-- Step 2 — swap the type. `actor_type` has no column DEFAULT (see
-- `migrations/0002_actors.up.sql`), so there is no default expression to
-- migrate to the new type.
CREATE TYPE actor_type_v2 AS ENUM ('human', 'ai_agent');

ALTER TABLE actors
    ALTER COLUMN actor_type TYPE actor_type_v2
    USING actor_type::text::actor_type_v2;

DROP TYPE actor_type;
ALTER TYPE actor_type_v2 RENAME TO actor_type;

-- `idx_actors_type_created_live` — a full-table rewrite (which this
-- `ALTER COLUMN ... TYPE` triggers, since the on-disk representation of the
-- enum changes) rebuilds every index on the table as part of the rewrite,
-- so in practice the index above already comes out intact and does not
-- need to be recreated by hand. We don't lean on that implicit behavior,
-- though: we drop and recreate it explicitly below so the migration is
-- correct regardless of which Postgres version or code path is doing the
-- rewrite under the hood.
DROP INDEX idx_actors_type_created_live;
CREATE INDEX idx_actors_type_created_live ON actors (actor_type, created_at DESC)
    WHERE deleted_at IS NULL;
