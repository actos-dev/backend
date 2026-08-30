-- 0010_follows.up.sql'i geri alır: index'i ve tabloyu ters sırada düşürür.

DROP INDEX idx_follows_followed_follower;
DROP TABLE follows;
