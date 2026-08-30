-- 0007_tags.up.sql'i geri alır: index'leri ve tabloları ters sırada düşürür.

DROP INDEX idx_tags_name_trgm;
DROP INDEX idx_content_tags_tag_content;
DROP TABLE content_tags;
DROP TABLE tags;
