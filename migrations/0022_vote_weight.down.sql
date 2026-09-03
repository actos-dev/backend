-- 0022_vote_weight.up.sql'i ters sırada geri alır.

ALTER TABLE votes DROP CONSTRAINT ck_votes_weight_range;
ALTER TABLE votes DROP COLUMN weight;
