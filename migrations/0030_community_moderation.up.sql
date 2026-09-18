-- Community moderation (COMMUNITY_PLAN.md phase 3, §5-7).
--
-- Three things happen here:
--
--   1. Bans gain a community dimension. NULL keeps meaning platform-wide,
--      exactly as before; a non-NULL value is a ban from that one community.
--      The single-column primary key is replaced by two partial unique
--      indexes so an actor can be banned once globally and once per
--      community without the rows colliding.
--   2. Reports gain the community of the content they concern, so a report
--      about community content reaches that community's moderators first.
--   3. A small background queue carries "ban and delete this account's
--      posts in a community" work. Doing the deletions inline would hold a
--      request open for an unbounded number of rows; the ban itself must
--      not depend on them finishing (COMMUNITY_PLAN.md §6).
--
-- The policies are one vocabulary with a scope (§5): a community owner holds
-- every community-scoped permission for their own community, inserted as
-- real rows so no hidden owner superuser exists. The backfill at the bottom
-- grants those to owners of communities created before this migration;
-- `community::create_community` grants them to owners created after it.

-- --- Bans -----------------------------------------------------------------

ALTER TABLE bans ADD COLUMN community_id bigint NULL REFERENCES communities (id) ON DELETE CASCADE;

COMMENT ON COLUMN bans.community_id IS
    'NULL = platform-wide ban. Non-NULL = ban from that one community. Two partial unique indexes enforce at most one global ban per actor and at most one ban per (community, actor).';

ALTER TABLE bans DROP CONSTRAINT bans_pkey;

-- Two partial unique indexes rather than a three-column key: NULL is not
-- equal to itself, so a plain UNIQUE (community_id, actor_id) would let an
-- actor accumulate unlimited global bans.
CREATE UNIQUE INDEX uq_bans_global ON bans (actor_id) WHERE community_id IS NULL;
CREATE UNIQUE INDEX uq_bans_community ON bans (community_id, actor_id) WHERE community_id IS NOT NULL;
CREATE INDEX idx_bans_community ON bans (community_id) WHERE community_id IS NOT NULL;

-- --- Reports --------------------------------------------------------------

ALTER TABLE reports ADD COLUMN community_id bigint NULL REFERENCES communities (id) ON DELETE SET NULL;

COMMENT ON COLUMN reports.community_id IS
    'The community of the reported content. NULL for an independent post. ON DELETE SET NULL: if the community disappears the report becomes an independent-content report, it is not deleted.';

CREATE INDEX idx_reports_community_pending ON reports (community_id, created_at) WHERE status = 'pending';

-- --- Moderation job queue -------------------------------------------------

CREATE TYPE moderation_job_kind AS ENUM ('delete_actor_content_in_community');

CREATE TABLE moderation_jobs (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind         moderation_job_kind NOT NULL,
    community_id bigint NOT NULL REFERENCES communities (id) ON DELETE CASCADE,
    actor_id     bigint NOT NULL REFERENCES actors (id) ON DELETE CASCADE,
    requested_by bigint NOT NULL REFERENCES actors (id) ON DELETE RESTRICT,
    created_at   timestamptz NOT NULL DEFAULT now(),
    processed_at timestamptz NULL
);

COMMENT ON TABLE moderation_jobs IS
    'Background work queued by moderation actions. `delete_actor_content_in_community` soft-deletes the named actor''s live contents in one community after a ban-with-delete (COMMUNITY_PLAN.md §6).';

CREATE INDEX idx_moderation_jobs_pending ON moderation_jobs (created_at) WHERE processed_at IS NULL;

-- --- Comment community inheritance ---------------------------------------

-- Comments inherit the community of their root post, so every content row
-- carries its own community and the phase 4 visibility gate can be uniform.
UPDATE contents child
SET community_id = root.community_id
FROM contents root
WHERE child.content_type = 'comment'
  AND child.community_id IS NULL
  AND root.id = child.root_post_id
  AND root.community_id IS NOT NULL;

-- --- Owner permissions backfill ------------------------------------------

-- A community owner holds every community-scoped permission for their own
-- community. This is how ownership becomes authority under the one-
-- vocabulary model: no hidden owner superuser, just rows.
INSERT INTO permissions (actor_id, permission, scope, community_id, granted_by)
SELECT c.owner_actor_id, p.permission, 'community', c.id, NULL
FROM communities c
CROSS JOIN (VALUES
    ('content.delete'::permission),
    ('community.edit'::permission),
    ('community.close'::permission),
    ('member.invite'::permission),
    ('member.approve'::permission),
    ('member.kick'::permission),
    ('member.ban'::permission),
    ('role.grant'::permission),
    ('report.view'::permission),
    ('report.resolve'::permission)
) AS p(permission)
ON CONFLICT DO NOTHING;
