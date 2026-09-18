-- Invitations and applications (COMMUNITY_PLAN.md §3, §5, §12, phase 4B-2).
--
-- Private communities admit members in two directions, and both produce an
-- inbox item (§3):
--
--   * `community_invitations` is the moderator-initiated path. An actor
--     holding `member.invite` scoped to the community adds someone by
--     username; the invitee is not a member until they accept.
--   * `community_applications` is the person-initiated path. Someone who
--     knows the community's name writes a reason and submits; a moderator
--     holding `member.approve` accepts or rejects it. The written reason is
--     stored in full because it is the only thing the queue has to judge.
--
-- Public communities take neither path: joining one is instant
-- (`community::join_community`), so an invitation or an application against
-- a public community is a client error, not a no-op.
--
-- Both tables are append-only in spirit: a resolved row is never deleted and
-- never returns to `pending`. The resolution-shape CHECK constraints make the
-- pending/resolved states exhaustive: a pending row has no resolution
-- timestamp, a resolved row must have one (and an application must also name
-- the moderator who resolved it).
--
-- The partial unique indexes enforce **at most one pending invitation and one
-- pending application per (community, actor)**. The domain layer turns a
-- duplicate into `409 Conflict` (see `community::invite_member` /
-- `community::apply_to_community`), so re-inviting does not stack rows and no
-- second notification goes out. A resolved row falls out of the index, which
-- is exactly right: the same actor may be invited again later.
--
-- The queue index is oldest-first (§3): the application list behind
-- `member.approve` is a work queue, ordered like the reports queue, so the
-- person who applied first is seen first.

CREATE TYPE invitation_status AS ENUM ('pending', 'accepted', 'declined');

CREATE TABLE community_invitations (
    id               bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    community_id     bigint NOT NULL REFERENCES communities (id) ON DELETE CASCADE,
    invited_actor_id bigint NOT NULL REFERENCES actors (id) ON DELETE CASCADE,
    invited_by       bigint NOT NULL REFERENCES actors (id) ON DELETE RESTRICT,
    status           invitation_status NOT NULL DEFAULT 'pending',
    created_at       timestamptz NOT NULL DEFAULT now(),
    resolved_at      timestamptz NULL,
    CONSTRAINT ck_community_invitations_resolution_shape CHECK (
        (status = 'pending' AND resolved_at IS NULL)
        OR (status <> 'pending' AND resolved_at IS NOT NULL))
);

COMMENT ON TABLE community_invitations IS
    'A moderator adds an actor by username to a private community (§3). The actor is not a member until they accept. `invited_by` is RESTRICT: the audit meaning of the row outlives the inviter.';
COMMENT ON COLUMN community_invitations.invited_by IS
    'The moderator who sent the invitation. RESTRICT like other audit-bearing foreign keys: actors are soft-deleted in practice, so a hard delete must deliberately clear these rows first.';
COMMENT ON COLUMN community_invitations.resolved_at IS
    'NULL while pending; set once on accept or decline. `ck_community_invitations_resolution_shape` ties this to `status`. The row is kept as a record of the action.';

CREATE UNIQUE INDEX uq_community_invitations_pending
    ON community_invitations (community_id, invited_actor_id) WHERE status = 'pending';

CREATE INDEX idx_community_invitations_invitee
    ON community_invitations (invited_actor_id, created_at DESC, id DESC);

CREATE TYPE application_status AS ENUM ('pending', 'accepted', 'rejected');

CREATE TABLE community_applications (
    id                  bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    community_id        bigint NOT NULL REFERENCES communities (id) ON DELETE CASCADE,
    applicant_actor_id  bigint NOT NULL REFERENCES actors (id) ON DELETE CASCADE,
    reason              text NOT NULL,
    status              application_status NOT NULL DEFAULT 'pending',
    created_at          timestamptz NOT NULL DEFAULT now(),
    resolved_by         bigint NULL REFERENCES actors (id) ON DELETE RESTRICT,
    resolved_at         timestamptz NULL,
    CONSTRAINT ck_community_applications_reason_length CHECK (char_length(reason) BETWEEN 1 AND 2000),
    CONSTRAINT ck_community_applications_resolution_shape CHECK (
        (status = 'pending' AND resolved_by IS NULL AND resolved_at IS NULL)
        OR (status <> 'pending' AND resolved_by IS NOT NULL AND resolved_at IS NOT NULL))
);

COMMENT ON TABLE community_applications IS
    'A person who knows a private community''s name writes why they want in (§3). Lands in the `member.approve` queue; the applicant is not a member until a moderator accepts.';
COMMENT ON COLUMN community_applications.reason IS
    'The applicant''s written reason, `ck_community_applications_reason_length` (1-2000 characters). This is the whole thing the moderator has to judge, so it is stored verbatim.';
COMMENT ON COLUMN community_applications.resolved_by IS
    'The moderator who accepted or rejected the application. Required once resolved (`ck_community_applications_resolution_shape`), NULL while pending.';
COMMENT ON COLUMN community_applications.resolved_at IS
    'NULL while pending; set once on accept or reject, together with `resolved_by`.';

CREATE UNIQUE INDEX uq_community_applications_pending
    ON community_applications (community_id, applicant_actor_id) WHERE status = 'pending';

CREATE INDEX idx_community_applications_queue
    ON community_applications (community_id, created_at) WHERE status = 'pending';

-- Three new notification kinds. Postgres 18 (this repo's target) allows
-- `ALTER TYPE ... ADD VALUE` inside the migration transaction: the type was
-- created by migration 0021, not by this transaction, so the restriction that
-- used to forbid this does not apply.
--
-- `IF NOT EXISTS` so re-running the up migration after a revert is a no-op
-- for these three values: `down` cannot remove them (Postgres has no
-- `DROP VALUE`), so on a scratch database that has been round-tripped the
-- values are already there.
ALTER TYPE notification_kind ADD VALUE IF NOT EXISTS 'community_invitation';
ALTER TYPE notification_kind ADD VALUE IF NOT EXISTS 'community_application';
ALTER TYPE notification_kind ADD VALUE IF NOT EXISTS 'community_application_result';
