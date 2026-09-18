-- Revert migration 0032 (community lifecycle, phase 4B-1).
--
-- Dropping the columns discards closure and succession state, which is what
-- reverting the phase means: the community rows themselves are untouched.
-- Order matters only in that the index's predicate references `closed_at`,
-- so the index goes first.

DROP INDEX idx_communities_open_visibility_created;

ALTER TABLE communities DROP COLUMN successor_actor_id;
ALTER TABLE communities DROP COLUMN closed_at;
