-- 0006_contents_indexes.up.sql'i geri alır: tüm index'leri ters sırada düşürür.

DROP INDEX idx_contents_parent;
DROP INDEX idx_contents_top;
DROP INDEX idx_contents_new;
DROP INDEX idx_contents_hot;
DROP INDEX idx_contents_actor_live;
DROP INDEX idx_contents_root_path;
DROP INDEX idx_contents_path;
