-- Removes the free-form `contents.metadata` JSON column (see REFACTOR.md §5).
-- The only rule the schema ever enforced on it was that the top level be a
-- JSON object (`ck_contents_metadata_object`) — no key allowlist, no size
-- cap, no depth cap. Nothing on the server branches on its contents; it is
-- read straight out of the row and handed back verbatim in responses, with
-- no index behind it. The only consumer that gave it meaning was a frontend
-- badge component reading three specific keys, and that contract lived
-- entirely on the client — the server never promised it. The doc comment
-- claiming it was reserved for things like link-preview data was
-- aspirational: no code path ever wrote that shape.

ALTER TABLE contents DROP CONSTRAINT ck_contents_metadata_object;
ALTER TABLE contents DROP COLUMN metadata;
