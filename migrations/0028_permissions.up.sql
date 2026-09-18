-- Scoped permissions (COMMUNITY_PLAN.md phase 1).
--
-- Replaces the single-role `admin_roles` table with one vocabulary of
-- permissions, each carried at a scope: `global` (platform-wide) or
-- `community` (one community). Phase 1 has no communities yet, so
-- `community_id` is a plain bigint with no foreign key; migration 0029
-- (phase 2) adds `REFERENCES communities (id)` once that table exists.
--
-- The community-only and global-only check constraints are deliberate:
-- `member.invite`/`approve`/`kick` have no meaning outside a community
-- ("how do I kick someone from the platform" is a ban, a different
-- permission), and reading the audit trail is a global operation until the
-- trail gains a community column.

CREATE TYPE permission_scope AS ENUM ('global', 'community');

CREATE TYPE permission AS ENUM (
    'content.delete',
    'community.edit',
    'community.close',
    'member.invite',
    'member.approve',
    'member.kick',
    'member.ban',
    'role.grant',
    'report.view',
    'report.resolve',
    'audit.view'
);

CREATE TABLE permissions (
    actor_id     bigint NOT NULL REFERENCES actors (id) ON DELETE RESTRICT,
    permission   permission NOT NULL,
    scope        permission_scope NOT NULL,
    community_id bigint NULL,
    granted_by   bigint NULL REFERENCES actors (id) ON DELETE RESTRICT,
    granted_at   timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT ck_permissions_scope_community CHECK (
        (scope = 'global' AND community_id IS NULL)
        OR (scope = 'community' AND community_id IS NOT NULL)
    ),
    CONSTRAINT ck_permissions_community_only CHECK (
        permission NOT IN ('member.invite', 'member.approve', 'member.kick')
        OR scope = 'community'
    ),
    CONSTRAINT ck_permissions_global_only CHECK (
        permission <> 'audit.view' OR scope = 'global'
    )
);

COMMENT ON TABLE permissions IS
    'Scoped authority grants. Replaces admin_roles (migration 0012). A global grant has community_id NULL; a community grant carries the community id (FK added with communities in migration 0029).';
COMMENT ON COLUMN permissions.granted_by IS
    'The actor who granted this permission. NULL only for the first admin, created directly by the seed binary.';
COMMENT ON COLUMN permissions.community_id IS
    'Community scope. NULL for global grants. The REFERENCES communities (id) constraint lands in migration 0029 (phase 2).';

-- Two partial unique indexes rather than one: NULLs are distinct in a
-- plain UNIQUE, so a single index would allow duplicate global grants.
CREATE UNIQUE INDEX uq_permissions_global
    ON permissions (actor_id, permission) WHERE community_id IS NULL;
CREATE UNIQUE INDEX uq_permissions_community
    ON permissions (actor_id, permission, community_id) WHERE community_id IS NOT NULL;

-- Every read starts by actor.
CREATE INDEX idx_permissions_actor ON permissions (actor_id);

-- --- Data migration: admin_roles -> permissions ----------------------------
--
-- A moderator held: report queue, content deletion, platform bans, audit
-- trail. An admin held all of those plus role management. The community-*
-- permissions did not exist, so nobody gets them here.

INSERT INTO permissions (actor_id, permission, scope, granted_by, granted_at)
SELECT ar.actor_id, v.permission, 'global', ar.granted_by, ar.granted_at
FROM admin_roles ar
CROSS JOIN (VALUES
    ('content.delete'::permission),
    ('member.ban'::permission),
    ('report.view'::permission),
    ('report.resolve'::permission),
    ('audit.view'::permission)
) AS v(permission);

INSERT INTO permissions (actor_id, permission, scope, granted_by, granted_at)
SELECT ar.actor_id, v.permission, 'global', ar.granted_by, ar.granted_at
FROM admin_roles ar
CROSS JOIN (VALUES
    ('community.edit'::permission),
    ('community.close'::permission),
    ('role.grant'::permission)
) AS v(permission)
WHERE ar.role = 'admin';

DROP TABLE admin_roles;

DROP TYPE admin_role;
