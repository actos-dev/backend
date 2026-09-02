-- 0019_search.up.sql'i geri alır: index'leri, generated column'ları, text
-- search configuration'ı ve unaccent eklentisini ters sırada düşürür.

DROP INDEX idx_actors_username_trgm;
DROP INDEX idx_actors_search_vector;
ALTER TABLE actors DROP COLUMN search_vector;

DROP INDEX idx_contents_search_vector;
ALTER TABLE contents DROP COLUMN search_vector;

DROP TEXT SEARCH CONFIGURATION actos_simple;
DROP EXTENSION IF EXISTS unaccent;
