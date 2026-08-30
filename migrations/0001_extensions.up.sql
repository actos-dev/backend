-- Platformun geri kalanının bağımlı olduğu PostgreSQL eklentilerini açar:
-- ltree (içerik ağacı), citext (case-insensitive username/tag), pg_trgm
-- (etiket autocomplete), pgcrypto (gen_random_uuid()). Bu dosya istisnadır:
-- eklentiler için IF NOT EXISTS kullanmak sözleşmede serbest bırakılmıştır.

CREATE EXTENSION IF NOT EXISTS ltree;
CREATE EXTENSION IF NOT EXISTS citext;
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE EXTENSION IF NOT EXISTS pgcrypto;
