-- Revert migration 0034 (cross-posts, phase 5).
--
-- **This revert requires that no cross-posts exist.** The original
-- `ck_contents_shape` required every post to have a title, and a cross-post
-- has none — restoring the constraint on a database that still holds one
-- fails cleanly (`ALTER TABLE` aborts the whole transaction). That is
-- deliberate: a revert that silently deleted or re-titled rows would lose
-- data. Delete the cross-posts first if the constraint refuses.
--
-- Order: the two cross-post constraints reference the column, so they go
-- before it; the original `ck_contents_shape` is restored before the column
-- is dropped (it does not reference the column), so the table is never left
-- without a shape check.

ALTER TABLE contents DROP CONSTRAINT ck_contents_cross_post_not_self;
ALTER TABLE contents DROP CONSTRAINT ck_contents_cross_post_is_post;

ALTER TABLE contents DROP CONSTRAINT ck_contents_shape;
ALTER TABLE contents ADD CONSTRAINT ck_contents_shape CHECK (
    (content_type = 'post' AND title IS NOT NULL AND parent_content_id IS NULL)
    OR
    (content_type = 'comment' AND title IS NULL AND parent_content_id IS NOT NULL)
);

DROP INDEX idx_contents_cross_post_source;

ALTER TABLE contents DROP COLUMN cross_post_source_id;
