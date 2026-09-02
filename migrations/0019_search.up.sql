-- Tam metin arama: `contents` (post/yorum) ve `actors` (kullanıcı) için
-- tsvector generated column + GIN index. PLAN.md Faz 15: Türkçe + İngilizce
-- karışık içerik için ayrı bir dil tespiti YOK, `simple` konfigürasyonu
-- kullanılıyor — ama aşağıdaki `unaccent` eklenmiş özel kopyasıyla, aksanlı
-- ve aksansız yazımlar aynı lexeme'e insin diye (`sürücü` ~ `surucu`).
--
-- ## `unaccent()` neden generated column'da DOĞRUDAN kullanılamıyor
--
-- `unaccent()` fonksiyonu STABLE'dır, IMMUTABLE DEĞİL — bu canlı PostgreSQL
-- 18 üzerinde bizzat doğrulandı. Bu yüzden
--     to_tsvector('simple', unaccent(title))
-- bir generated column ifadesi olarak yazılırsa PostgreSQL
-- `ERROR: generation expression is not immutable` ile reddeder: generated
-- column'un ifadesi IMMUTABLE olmak zorunda (satır ne zaman okunursa
-- okunsun aynı girdi için aynı çıktıyı üretmeli garantisi).
--
-- Yaygın bir "kaçış yolu", `unaccent()`'i IMMUTABLE olduğunu iddia eden bir
-- SQL sarmalayıcı fonksiyona sarıp PostgreSQL'i buna inandırmaktır. Burada
-- BİLİNÇLİ OLARAK kullanılmıyor: bu, fonksiyonun gerçek kararlılık
-- sözleşmesi hakkında veritabanına yalan söylemektir (unaccent sözlüğü bir
-- `ALTER TEXT SEARCH DICTIONARY` ile değiştirilirse — ki mümkündür —
-- STORED bir generated column'daki önceden hesaplanmış değerler sessizce
-- bayatlar, hiçbir uyarı vermeden).
--
-- Doğrulanmış çözüm: `unaccent`'i kendi sözlük zincirine gömen özel bir
-- text search configuration yaratmak. `to_tsvector(regconfig, text)`'in iki
-- argümanlı hâli IMMUTABLE'dır (regconfig sabit bir isim olduğu sürece),
-- dolayısıyla bu, generated column içinde sorunsuz çalışır. Canlı
-- veritabanında doğrulanmış sonuçlar: `q='surucu'` → `sürücü`'yü buluyor,
-- `q='CAFE'` → `Café`'yi buluyor, `q='sürücü'` → yine buluyor.
CREATE EXTENSION IF NOT EXISTS unaccent;

CREATE TEXT SEARCH CONFIGURATION actos_simple (COPY = simple);

-- Token listesi canlı PostgreSQL 18'de doğrulandı (`\dF+ simple` çıktısı) —
-- tam olarak bu dokuz token tipi var. `numhword_part` diye bir token tipi
-- YOKTUR: onu bu listeye eklemek `ALTER MAPPING FOR ... numhword_part`'ı
-- "böyle bir token tipi yok" hatasıyla reddeder VE (bizzat yaşandı) hata
-- fark edilmezse unaccent o token tipi için hiç devreye girmemiş olur —
-- yani listeyi elle genişletmeden önce `\dF+ simple` ile teyit edin.
ALTER TEXT SEARCH CONFIGURATION actos_simple
    ALTER MAPPING FOR asciiword, asciihword, hword_asciipart,
                      word, hword, hword_part, hword_numpart,
                      numword, numhword
    WITH unaccent, simple;

-- `contents`: title A ağırlıklı, body B ağırlıklı. `title` yorumlarda NULL
-- (bkz. ck_contents_shape), `coalesce` onu boş metne indirger — bir
-- yorumun arama vektörü yalnızca gövdesinden gelir.
ALTER TABLE contents
    ADD COLUMN search_vector tsvector
    GENERATED ALWAYS AS (
        setweight(to_tsvector('actos_simple', coalesce(title, '')), 'A')
        || setweight(to_tsvector('actos_simple', coalesce(body, '')), 'B')
    ) STORED;

-- Kısmi index (docs/db-conventions.md kural 6): yalnızca canlı satırlar
-- aranıyor — `actos_core::content` ile aynı "silinmiş içerik listelerde/
-- aramada hiç görünmez" kararı (bkz. `crate::search` modül dokümantasyonu).
-- Silinmiş satırların vektörünü index'e taşımanın bir karşılığı yok.
CREATE INDEX idx_contents_search_vector ON contents USING GIN (search_vector)
    WHERE deleted_at IS NULL;

-- `actors`: username + display_name A ağırlıklı (ikisi de "bu hesabın
-- kimliği" bilgisini taşıyor, aralarında öncelik farkı yok), bio B
-- ağırlıklı. `username` citext; `to_tsvector` `text` beklediği için cast
-- ediliyor (0007_tags'teki aynı gerekçe: citext operatörleri arama
-- vektörü üretiminde işimize yaramıyor, düz text istiyoruz).
ALTER TABLE actors
    ADD COLUMN search_vector tsvector
    GENERATED ALWAYS AS (
        setweight(to_tsvector('actos_simple', coalesce(username::text, '')), 'A')
        || setweight(to_tsvector('actos_simple', coalesce(display_name, '')), 'A')
        || setweight(to_tsvector('actos_simple', coalesce(bio, '')), 'B')
    ) STORED;

CREATE INDEX idx_actors_search_vector ON actors USING GIN (search_vector)
    WHERE deleted_at IS NULL;

-- Kullanıcı adı önek/trigram araması için (bkz. `crate::search` modül
-- dokümantasyonu — Faz 10'un `tag.rs::search` dersiyle aynı gerekçe: kısa
-- sorgularda trigram benzerliği tek başına yetmiyor, önek eşleşmesi de
-- gerekiyor). `gin_trgm_ops`'lu bir GIN index hem `similarity()`'yi hem de
-- `LIKE '<önek>%'`'i hızlandırır (pg_trgm, LIKE/ILIKE operatörlerini de
-- destekler, yalnızca `%` benzerlik operatörünü değil) — 0007_tags'teki
-- `idx_tags_name_trgm` ile birebir aynı desen.
CREATE INDEX idx_actors_username_trgm ON actors USING GIN ((username::text) gin_trgm_ops)
    WHERE deleted_at IS NULL;

COMMENT ON TEXT SEARCH CONFIGURATION actos_simple IS
    '`simple` konfigürasyonunun unaccent sözlüğü eklenmiş kopyası. Ayrı bir dil tespiti '
    'yapılmıyor (bkz. PLAN.md Faz 15); unaccent sayesinde aksanlı/aksansız yazımlar '
    '(sürücü ~ surucu) ve büyük/küçük harf farkları aynı lexeme''e iner.';
COMMENT ON COLUMN contents.search_vector IS
    'Tam metin arama vektörü: title A ağırlıklı, body B ağırlıklı. unaccent() STABLE olduğu '
    'için doğrudan kullanılamadı, bunun yerine actos_simple konfigürasyonu (unaccent gömülü) '
    'kullanılıyor (bkz. bu migration''ın başındaki gerekçe).';
COMMENT ON COLUMN actors.search_vector IS
    'Tam metin arama vektörü: username + display_name A ağırlıklı, bio B ağırlıklı.';
