-- Reverts 0023_drop_trust_levels.up.sql in reverse order. Columns,
-- constraints, and the index are recreated with the same shapes and the
-- same default values as in the original migrations (0020, 0022).

ALTER TABLE actors
    ADD COLUMN trust_level smallint NOT NULL DEFAULT 0;

ALTER TABLE actors
    ADD CONSTRAINT ck_actors_trust_level_range CHECK (trust_level BETWEEN 0 AND 2);

CREATE INDEX idx_reports_resolved_recent ON reports (resolved_at, target_id)
    WHERE status = 'resolved';

COMMENT ON COLUMN actors.trust_level IS
    'Trust tier (0-2), the basis of sybil/manipulation defense (see NOTES.md §9.3). '
    'Tier 1: account age >= 24 hours AND at least 1 non-deleted piece of content (post/comment). '
    'Tier 2: age >= 7 days AND net >= 25 votes from outside its own content (i.e. from others) AND '
    'no resolved report in the last 30 days. A resolved report demotes a tier. '
    'TIER 1 DELIBERATELY CARRIES NO KARMA (vote) REQUIREMENT: on a brand-new platform the '
    'first users have no one to vote on each other (cold start) — requiring karma for tier 1 '
    'would lock them into tier 0 forever. Computed inside `crate::actor::'
    'recompute_trust_levels`, re-run for every actor by the periodic job '
    '(see `TRUST_LEVEL_INTERVAL_SECS`). The CONSEQUENCES of the tier (vote weight, '
    'the hot filter, rate limit, storage quota) are deliberately OUTSIDE the scope of this migration.';

ALTER TABLE votes
    ADD COLUMN weight smallint NOT NULL DEFAULT 1;

ALTER TABLE votes
    ADD CONSTRAINT ck_votes_weight_range CHECK (weight IN (0, 1));

COMMENT ON COLUMN votes.weight IS
    'The multiplier for the vote''s contribution to the score (today only 0 or 1, see ck_votes_weight_range). '
    'crate::interaction::set_vote writes this column at the MOMENT the vote is CAST/CHANGED, based on the '
    'voter''s actors.trust_level at that instant — it is NOT recomputed retroactively AFTERWARD. '
    'This is a deliberate choice: every time an actor is promoted/demoted, walking ALL of their '
    'past votes and re-summing contents.score would mean potentially updating thousands of '
    'contents rows on every tier change (see the periodic frequency of '
    'crate::actor::recompute_trust_levels) — the weight is therefore a SNAPSHOT of the moment '
    'the vote was cast, and does not change even once the tier is in the past. '
    'When a vote is changed or withdrawn, crate::interaction::set_vote computes the OLD '
    'contribution using the value STORED in this column, not the voter''s CURRENT tier — '
    'otherwise someone who voted at tier 0 and was later promoted would, upon withdrawing '
    'their vote, cause contents.score to drift negative. The regression test that pinned this '
    'was removed together with the weight column in 0023; rolling this migration forward again '
    'would need it rewritten. Unlike votes.value (the raw direction: -1/1), '
    'this column does NOT affect contents.upvotes/downvotes — those are still a raw vote COUNT, '
    'only contents.score = sum(value * weight) sees this weight.';
