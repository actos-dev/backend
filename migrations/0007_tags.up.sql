-- Etiketler ve içerik-etiket ilişkisi. Post başına maksimum etiket sayısı
-- (10) uygulama katmanında zorlanır (bkz. PLAN.md Faz 3).

CREATE TABLE tags (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name        citext NOT NULL UNIQUE,
    created_at  timestamptz NOT NULL DEFAULT now(),

    -- Dikkat: citext üzerinde `~` operatörü de büyük/küçük harf duyarsız
    -- çalışır (0002_actors'ta aynı hataya düşülmüştü, bkz. oradaki yorum).
    -- text'e cast ederek kuralı harfiyen uyguluyoruz: etiket adları her zaman
    -- küçük harf saklanır. Benzersizlik citext sayesinde harf durumundan
    -- bağımsız kalmaya devam eder.
    CONSTRAINT ck_tags_name_format CHECK ((name)::text ~ '^[a-z0-9][a-z0-9-]{0,31}$')
);

CREATE TABLE content_tags (
    content_id  bigint NOT NULL REFERENCES contents (id) ON DELETE CASCADE,
    tag_id      bigint NOT NULL REFERENCES tags (id) ON DELETE CASCADE,

    PRIMARY KEY (content_id, tag_id)
);

-- "Bu etiketteki içerikler" sorgusu için ters index (PK sadece content_id ile
-- başlayan sorguları hızlandırır, tag_id ile başlayanları değil).
CREATE INDEX idx_content_tags_tag_content ON content_tags (tag_id, content_id);

-- Etiket autocomplete: kullanıcı birkaç harf yazarken benzer isimleri bulmak
-- için trigram GIN index. gin_trgm_ops sadece text tipi için tanımlı, citext
-- için değil; bu yüzden ifadeyi text'e cast ediyoruz.
CREATE INDEX idx_tags_name_trgm ON tags USING GIN ((name::text) gin_trgm_ops);

COMMENT ON TABLE tags IS
    'Post''lara eklenebilen etiketler. İsimler citext (case-insensitive) ve benzersiz.';
COMMENT ON COLUMN tags.name IS
    'Etiket adı, her zaman küçük harf saklanır (ck_tags_name_format). citext sayesinde '
    'karşılaştırma harf durumundan bağımsızdır.';
COMMENT ON TABLE content_tags IS
    'contents ve tags arasındaki çoktan-çoğa ilişki. Saf ilişki tablosu olduğu için her iki '
    'FK de ON DELETE CASCADE (bkz. docs/db-conventions.md).';
