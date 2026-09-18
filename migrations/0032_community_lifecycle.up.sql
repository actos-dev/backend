-- Community lifecycle: closure and succession (COMMUNITY_PLAN.md §2 + §4,
-- phase 4B-1).
--
-- Two nullable columns and one partial index. Phase 4B-1 is the server half
-- of private communities: creating them, the cover page for a viewer who
-- may not see inside, closing them, and handing the owner's seat to a
-- successor when the owner departs.
--
--   * `closed_at` marks a community that has emptied out. A closed
--     community is `404` on every community endpoint; its posts were either
--     released (public community) or soft-deleted (private community) at
--     close time. The row itself is kept so the name stays taken and the
--     history remains inspectable; it is a tombstone, not a delete.
--   * `successor_actor_id` is the seat the owner designated. On the owner's
--     departure the community passes to it if the actor is still live, and
--     otherwise to the longest-serving community-scoped permission holder
--     (§4). `ON DELETE SET NULL` because a hard-deleted actor has no seat
--     to occupy; actors are soft-deleted in practice, so the fallback rule
--     does the real work.
--
-- The partial index narrows the directory's existing keyset scan to open
-- communities; a closed community is never listed.

ALTER TABLE communities ADD COLUMN closed_at timestamptz NULL;

COMMENT ON COLUMN communities.closed_at IS
    'NULL = open. Non-NULL = closed: every community endpoint returns 404 (§4). Public communities released their posts (contents.community_id = NULL); private communities soft-deleted theirs. The row survives so the name stays reserved.';

ALTER TABLE communities ADD COLUMN successor_actor_id bigint NULL REFERENCES actors (id) ON DELETE SET NULL;

COMMENT ON COLUMN communities.successor_actor_id IS
    'The owner-designated successor (§4). Used when the owner departs only if the actor is still live; otherwise ownership falls to the longest-serving community-scoped permission holder. NULL when none was designated or after a transfer consumed it.';

-- Directory keyset scan restricted to open public communities. Mirrors
-- idx_communities_visibility_created with the closed filter pushed into the
-- index predicate.
CREATE INDEX idx_communities_open_visibility_created
    ON communities (visibility, created_at DESC, id DESC) WHERE closed_at IS NULL;

COMMENT ON INDEX idx_communities_open_visibility_created IS
    'Directory/keyset scan for open communities only (COMMUNITY_PLAN.md §2): a closed community is never listed.';
