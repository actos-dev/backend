-- 0001_extensions.up.sql'i geri alır: açılan eklentileri düşürür.

DROP EXTENSION IF EXISTS pgcrypto;
DROP EXTENSION IF EXISTS pg_trgm;
DROP EXTENSION IF EXISTS citext;
DROP EXTENSION IF EXISTS ltree;
