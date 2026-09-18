-- Cross-posts (COMMUNITY_PLAN.md §8, phase 5).
--
-- A cross-post is a reference, not a copy: the row stores only the source's
-- id and the card is resolved at read time with the reader's permissions.
-- Storing a copy would freeze the source as it was at the moment of posting,
-- and that snapshot would survive an edit, a delete, or a move behind a
-- private door (§8).
--
-- Three rules follow, and each gets its guard where it can actually live:
--
--   * Nothing leaves a private community. Enforced in the application
--     (`content::create_post` returns `403`), not by a CHECK: a single
--     contents row cannot see a community's visibility.
--   * An unreachable source renders as a tombstone, deleted or invisible
--     alike, with no disclosed reason. That is a read-time decision
--     (`content::resolve_cross_posts`), not a storage one.
--   * Depth is capped at one level. The schema enforces the part it can
--     (`ck_contents_cross_post_is_post`), but "the source is not itself a
--     cross-post" needs to read the source row and so lives in the insert
--     path; a cross-row CHECK is not expressible.
--
-- `ON DELETE RESTRICT`: content is soft-deleted (`deleted_at`) throughout
-- this codebase, so a hard delete is a deliberate upkeep act and the
-- reference must not silently dangle. `ck_contents_cross_post_not_self`
-- blocks the one-row cycle a self-reference would create.
--
-- A cross-post carries no title of its own (the source's title is resolved
-- at read time), so `ck_contents_shape` is relaxed: a post's title may be
-- NULL exactly when `cross_post_source_id` is set.

ALTER TABLE contents ADD COLUMN cross_post_source_id bigint NULL REFERENCES contents (id) ON DELETE RESTRICT;

COMMENT ON COLUMN contents.cross_post_source_id IS
    'NULL = an ordinary post. Non-NULL = a cross-post: this row is a reference to that source content (COMMUNITY_PLAN.md §8), never a copy. Resolved at read time with the reader''s permissions; an unreachable source renders as a tombstone. ON DELETE RESTRICT because content is soft-deleted in practice, so a hard delete is deliberate.';

CREATE INDEX idx_contents_cross_post_source ON contents (cross_post_source_id) WHERE cross_post_source_id IS NOT NULL;

COMMENT ON INDEX idx_contents_cross_post_source IS
    'Both directions of the reference: finding the cross-posts of a source (e.g. before a hard delete) and batch-loading a page''s sources for `content::resolve_cross_posts`. Partial because most rows have no source.';

ALTER TABLE contents DROP CONSTRAINT ck_contents_shape;
ALTER TABLE contents ADD CONSTRAINT ck_contents_shape CHECK (
    (content_type = 'post' AND parent_content_id IS NULL AND (title IS NOT NULL OR cross_post_source_id IS NOT NULL))
    OR
    (content_type = 'comment' AND title IS NULL AND parent_content_id IS NOT NULL)
);

ALTER TABLE contents ADD CONSTRAINT ck_contents_cross_post_is_post CHECK (
    cross_post_source_id IS NULL OR content_type = 'post'
);

ALTER TABLE contents ADD CONSTRAINT ck_contents_cross_post_not_self CHECK (
    cross_post_source_id IS NULL OR cross_post_source_id <> id
);

COMMENT ON CONSTRAINT ck_contents_shape ON contents IS
    'A post has no parent and carries a title unless it is a cross-post, whose title is resolved from the source. A comment has a parent and no title.';
COMMENT ON CONSTRAINT ck_contents_cross_post_is_post ON contents IS
    'Only a post can be a cross-post (COMMUNITY_PLAN.md §8): cross-posting is a post-level act, and a comment has neither a title nor a reason to carry a source.';
COMMENT ON CONSTRAINT ck_contents_cross_post_not_self ON contents IS
    'A row cannot cross-post itself. The one-level depth cap as a whole (a cross-post cannot be cross-posted) needs to read the source row, so it lives in `content::create_post`.';
