-- The visibility gate (COMMUNITY_PLAN.md phase 4A, §2 + §9).
--
-- Private communities are unlisted, not secret: their contents must not
-- appear in any public surface, and a member may only read them while they
-- are actually a member (or hold a scoped permission there). Phase 4A is
-- *only* the read path; creating private communities through the API stays
-- rejected until phase 4B flips the switch.
--
-- One shared predicate, used by every read query, rather than each query
-- growing its own check. Missing one is a leak, not a wrong answer
-- (COMMUNITY_PLAN.md §12), so there is exactly one place to get right.
--
-- Semantics (§9, "the privacy rule"):
--
--   * `community_id IS NULL` — an independent post/comment, always visible.
--   * the community is `public` — always visible.
--   * `community_id = ANY (viewer_communities)` — visible because the
--     viewer is a member of it, or holds a community-scoped permission.
--
-- An EMPTY `viewer_communities` (`'{}'`) therefore means "unconditional
-- public-only". Every public surface that lists someone else's activity —
-- the main feed, the following feed, search, tag pages, a profile's
-- post/comment lists and statistics — passes `'{}'` even when the viewer is
-- a member, so the number shown is the same for every viewer. A person's own
-- lists (`/me/saves`, the inbox, `/me/votes`) and direct single-item reads
-- pass the viewer's actual communities.
--
-- The function is STABLE: it depends only on table contents within a single
-- statement, which lets the planner inline it and use the community indexes
-- instead of treating it as an opaque per-row call.

CREATE FUNCTION content_visible_to(community_id bigint, viewer_communities bigint[])
RETURNS boolean
LANGUAGE sql
STABLE
AS $$
    SELECT community_id IS NULL
        OR EXISTS (
            SELECT 1 FROM communities c
            WHERE c.id = community_id AND c.visibility = 'public'
        )
        OR community_id = ANY (viewer_communities)
$$;

COMMENT ON FUNCTION content_visible_to(bigint, bigint[]) IS
    'COMMUNITY_PLAN.md §9. TRUE for independent content (community_id IS NULL), for content in a public community, and for content in a community listed in viewer_communities. An empty viewer_communities means public-only, which is what every public surface uses unconditionally (main feed, following feed, search, tag pages, a profile''s lists and statistics). A person''s own lists and direct single-item reads pass their current memberships/permissions so private content is readable by members. Every read path must go through this one function; a missing call is a leak.';
