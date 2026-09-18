-- Revert the scoped-permission model back to admin_roles.
--
-- The mapping is lossy in one direction: a `role.grant` global holder was an
-- admin, a bare `content.delete` global holder was a moderator. Anyone who
-- held only a permission outside that set is not representable and is
-- dropped — the old model could not express them in the first place.

CREATE TYPE admin_role AS ENUM ('admin', 'moderator');

CREATE TABLE admin_roles (
    actor_id    bigint NOT NULL PRIMARY KEY REFERENCES actors (id) ON DELETE RESTRICT,
    role        admin_role NOT NULL,
    granted_by  bigint NULL REFERENCES actors (id) ON DELETE RESTRICT,
    granted_at  timestamptz NOT NULL DEFAULT now()
);

INSERT INTO admin_roles (actor_id, role, granted_by, granted_at)
SELECT actor_id, 'admin'::admin_role, granted_by, granted_at
FROM permissions
WHERE permission = 'role.grant' AND scope = 'global'
ON CONFLICT (actor_id) DO NOTHING;

INSERT INTO admin_roles (actor_id, role, granted_by, granted_at)
SELECT actor_id, 'moderator'::admin_role, granted_by, granted_at
FROM permissions
WHERE permission = 'content.delete' AND scope = 'global'
ON CONFLICT (actor_id) DO NOTHING;

DROP TABLE permissions;

DROP TYPE permission;

DROP TYPE permission_scope;
