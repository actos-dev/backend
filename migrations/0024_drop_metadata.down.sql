-- Reverts 0024_drop_metadata.up.sql. Column and constraint are recreated
-- with the same shape and default as the original `0005_contents.up.sql`.

ALTER TABLE contents
    ADD COLUMN metadata jsonb NOT NULL DEFAULT '{}';

ALTER TABLE contents
    ADD CONSTRAINT ck_contents_metadata_object CHECK (jsonb_typeof(metadata) = 'object');

COMMENT ON COLUMN contents.metadata IS
    'Free-form additional data (e.g. URL preview info for link posts). Empty object = no data.';
