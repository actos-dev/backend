-- Revert migration 0029 (communities, phase 2).

DROP INDEX idx_contents_community_new;
DROP INDEX idx_contents_community_top;
DROP INDEX idx_contents_community_hot;

ALTER TABLE permissions DROP CONSTRAINT permissions_community_id_fkey;

ALTER TABLE contents DROP COLUMN community_id;

DROP INDEX idx_community_members_actor;
DROP TABLE community_members;

DROP INDEX idx_communities_visibility_created;
DROP TABLE communities;

DROP TYPE community_visibility;
