-- Complete removal of the trust level system (see REFACTOR.md §3). The tier
-- system didn't provide a practical security guarantee and carried four
-- separate connection points (periodic job, advisory lock, column, rate
-- limit multiplier) — none of them paid for themselves.
-- `migrations/0020_trust_levels.up.sql` added `actors.trust_level`, and
-- `0022_vote_weight.up.sql` added `votes.weight`; this migration rips out
-- both at once because the rate limit multiplier and vote weight were fed
-- by the same tier (see REFACTOR.md "Suggested order" — the two are a
-- single unit of work).
--
-- As a result:
-- * Rate limit capacity is now a single table for all authenticated
--   actors (see `crates/actos-core/src/config.rs`), no tier multiplier.
-- * `contents.score` is now a raw sum of votes (`sum(value)`), not
--   weighted.
-- * The storage quota is a single flat value (see `StorageQuotaConfig`).
-- * The `hot` ordering's "author tier >= 1" filter is gone — new accounts
--   can show up in `hot` instantly.
--
-- `recompute_trust_levels`'s periodic job and its advisory lock were
-- removed from `crates/actos-core/src/actor.rs`, and the job that
-- triggered it from `main.rs` — this migration is the schema side only.

ALTER TABLE votes DROP CONSTRAINT ck_votes_weight_range;
ALTER TABLE votes DROP COLUMN weight;

DROP INDEX idx_reports_resolved_recent;

ALTER TABLE actors DROP CONSTRAINT ck_actors_trust_level_range;
ALTER TABLE actors DROP COLUMN trust_level;
