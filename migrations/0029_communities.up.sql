-- Communities (COMMUNITY_PLAN.md phase 2, public only).
--
-- A community is a container with an owner, members and a description. It is
-- not a tag: tags stay free-form and ownerless, a post may carry both or
-- neither, and the two systems do not interact (COMMUNITY_PLAN.md §1).
--
-- Phase 2 is public-only: the `visibility` column and its enum exist so the
-- shape is final, but the application rejects `private` at creation until
-- phase 4 builds the visibility gate across every read path. Keeping the two
-- states in the schema now avoids a column migration later.

CREATE TYPE community_visibility AS ENUM ('public', 'private');

CREATE TABLE communities (
    id             bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name           citext NOT NULL UNIQUE,
    description    text NOT NULL,
    visibility     community_visibility NOT NULL DEFAULT 'public',
    owner_actor_id bigint NOT NULL REFERENCES actors (id) ON DELETE RESTRICT,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT ck_communities_name_format CHECK ((name)::text ~ '^[a-z0-9_]{3,32}$'),
    CONSTRAINT ck_communities_name_reserved CHECK (name NOT IN (
        'admin','administrator','actos','api','root','system','moderator',
        'support','help','about','me','null','undefined')),
    CONSTRAINT ck_communities_description_length CHECK (char_length(description) BETWEEN 1 AND 10000)
);

COMMENT ON TABLE communities IS
    'A container with one owner and a member list (COMMUNITY_PLAN.md §1). Optional for a post: contents.community_id NULL means an independent post, not a lesser kind of post.';
COMMENT ON COLUMN communities.name IS
    'citext. Same format and reserved list as usernames (§10): impersonation, not routing, is the risk. Reached under /communities/{name}, so it may collide with a username without ambiguity.';
COMMENT ON COLUMN communities.description IS
    'Markdown, stored as text and length-validated only. No server-side render step; clients decide how to display it (§11).';
COMMENT ON COLUMN communities.visibility IS
    'public = listed in the directory, anyone can join. private = unlisted. Phase 2 rejects creating private communities; the value exists so phase 4 only adds the gate, not a migration.';
COMMENT ON COLUMN communities.owner_actor_id IS
    'The single owner. An actor may own at most MAX_COMMUNITIES_PER_OWNER (3) communities; enforced in the application inside the creation transaction (COMMUNITY_PLAN.md §4).';

-- Directory: public communities, newest first. Matches the (visibility,
-- created_at DESC, id DESC) keyset shape used by list_directory.
CREATE INDEX idx_communities_visibility_created ON communities (visibility, created_at DESC, id DESC);

CREATE TABLE community_members (
    community_id bigint NOT NULL REFERENCES communities (id) ON DELETE CASCADE,
    actor_id     bigint NOT NULL REFERENCES actors (id) ON DELETE CASCADE,
    joined_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (community_id, actor_id)
);

COMMENT ON TABLE community_members IS
    'Membership. The owner is inserted here at creation, so an owner is always a member. Public communities join instantly; membership is required to post, never to read (COMMUNITY_PLAN.md §3).';
COMMENT ON COLUMN community_members.joined_at IS
    'When the membership was created. The member list is ordered by this ascending (longest-serving first) because succession, in a later phase, inherits to the longest-serving moderator (COMMUNITY_PLAN.md §4).';

-- "Which communities does this actor belong to" — the reverse of the primary
-- key, which already covers "who is in this community".
CREATE INDEX idx_community_members_actor ON community_members (actor_id, joined_at DESC);

ALTER TABLE contents ADD COLUMN community_id bigint NULL REFERENCES communities (id) ON DELETE SET NULL;

COMMENT ON COLUMN contents.community_id IS
    'The community this post belongs to; NULL means an independent post. ON DELETE SET NULL rather than CASCADE: a public community that empties out releases its posts as independent, it does not delete them (COMMUNITY_PLAN.md §4).';

-- Phase 1 deferred this FK; add it now.
ALTER TABLE permissions ADD CONSTRAINT permissions_community_id_fkey
    FOREIGN KEY (community_id) REFERENCES communities (id) ON DELETE CASCADE;

-- Community feed: the three sorts, restricted to live posts of one community.
-- These mirror idx_contents_new/top/hot with community_id as the leading key.
CREATE INDEX idx_contents_community_new ON contents (community_id, created_at DESC, id DESC)
    WHERE content_type = 'post' AND deleted_at IS NULL AND community_id IS NOT NULL;
CREATE INDEX idx_contents_community_top ON contents (community_id, score DESC, id DESC)
    WHERE content_type = 'post' AND deleted_at IS NULL AND community_id IS NOT NULL;
CREATE INDEX idx_contents_community_hot ON contents (community_id, hot_score DESC, id DESC)
    WHERE content_type = 'post' AND deleted_at IS NULL AND community_id IS NOT NULL;
