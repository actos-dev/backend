-- 0005_contents.up.sql'i geri alır: trigger, fonksiyon, tablo ve enum tiplerini
-- ters sırada düşürür.

DROP TRIGGER trg_contents_set_path ON contents;
DROP FUNCTION contents_set_path();
DROP TABLE contents;
DROP TYPE body_format;
DROP TYPE content_type;
