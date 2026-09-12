-- Closes the upload-then-attach flow for good (REFACTOR.md §4: "this is not
-- an image host" — an attachment is now created inside the same transaction
-- as the post/comment it belongs to, see `crate::attachment::
-- create_for_content`). `attachments.content_id` was `NULL` while a file sat
-- between `POST /uploads` and being attached to a post/comment; that
-- standalone upload step no longer exists in the application, so a `NULL`
-- here is no longer a representable state.
--
-- Any row still holding `content_id IS NULL` at the moment this migration
-- runs is exactly that: an upload from the old flow that was never attached
-- to anything (an abandoned draft, a client that uploaded and never
-- followed up). There is no content for it to retroactively belong to, and
-- no code path left that reads a `content_id IS NULL` row, so it is deleted
-- outright below. This is a plain SQL `DELETE`, not a call through
-- `attachment::delete_attachment` — it never touches S3. That deliberately
-- leaves each row's object (and its `.thumb.webp` companion) behind in the
-- bucket as garbage with no bookkeeping row pointing at it any more. This is
-- not accidental data loss to pretend away: it is the same trade-off
-- `migrations/0026_drop_avatar_attachments.up.sql` made for the old avatar
-- rows, applied here to the remaining never-attached uploads. Reclaiming
-- that storage, if it is ever worth doing, requires a one-off bucket scan
-- against the surviving `attachments.object_key`s — outside the scope of a
-- schema migration.
DELETE FROM attachments WHERE content_id IS NULL;

-- The orphan-cleanup job (`attachment::cleanup_orphaned`) that used to scan
-- this index is deleted along with it — there is no more "unattached"
-- state left to sweep.
DROP INDEX idx_attachments_orphaned;

ALTER TABLE attachments ALTER COLUMN content_id SET NOT NULL;

COMMENT ON TABLE attachments IS
    'Images that travel with a post or comment. Created inside the same transaction as the '
    'content row they belong to (crate::attachment::create_for_content) — there is no '
    'standalone upload step (REFACTOR.md §4).';
COMMENT ON COLUMN attachments.content_id IS
    'The content this attachment belongs to. Set at INSERT time, in the same '
    'transaction as that content row. Never NULL.';
