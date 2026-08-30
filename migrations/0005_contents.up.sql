-- Post'ları ve yorumları tek tabloda tutar (Reddit/HN tarzı ağaç). `path`
-- (ltree), `depth` ve `root_post_id` uygulama tarafından değil, BEFORE INSERT
-- trigger'ı tarafından hesaplanır — bkz. aşağıdaki trigger yorumu ve
-- docs/db-conventions.md.

CREATE TYPE content_type AS ENUM ('post', 'comment');
CREATE TYPE body_format AS ENUM ('markdown', 'plain');

CREATE TABLE contents (
    id                 bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    actor_id           bigint NOT NULL REFERENCES actors (id) ON DELETE RESTRICT,
    root_post_id       bigint NULL REFERENCES contents (id) ON DELETE RESTRICT,
    parent_content_id  bigint NULL REFERENCES contents (id) ON DELETE RESTRICT,
    path               ltree NOT NULL,
    depth              int NOT NULL,
    content_type       content_type NOT NULL,
    title              text NULL,
    body               text NOT NULL,
    body_format        body_format NOT NULL DEFAULT 'markdown',
    metadata           jsonb NOT NULL DEFAULT '{}',
    score              int NOT NULL DEFAULT 0,
    upvotes            int NOT NULL DEFAULT 0,
    downvotes          int NOT NULL DEFAULT 0,
    comment_count      int NOT NULL DEFAULT 0,
    hot_score          double precision NOT NULL DEFAULT 0,
    created_at         timestamptz NOT NULL DEFAULT now(),
    edited_at          timestamptz NULL,
    deleted_at         timestamptz NULL,

    CONSTRAINT ck_contents_shape CHECK (
        (content_type = 'post' AND title IS NOT NULL AND parent_content_id IS NULL)
        OR
        (content_type = 'comment' AND title IS NULL AND parent_content_id IS NOT NULL)
    ),
    CONSTRAINT ck_contents_depth CHECK (depth >= 0 AND depth <= 32),
    CONSTRAINT ck_contents_post_depth_zero CHECK (content_type <> 'post' OR depth = 0),
    CONSTRAINT ck_contents_title_length CHECK (char_length(title) <= 300),
    CONSTRAINT ck_contents_body_length CHECK (char_length(body) <= 100000),
    CONSTRAINT ck_contents_metadata_object CHECK (jsonb_typeof(metadata) = 'object'),
    CONSTRAINT ck_contents_upvotes_nonneg CHECK (upvotes >= 0),
    CONSTRAINT ck_contents_downvotes_nonneg CHECK (downvotes >= 0),
    CONSTRAINT ck_contents_comment_count_nonneg CHECK (comment_count >= 0)
);

-- `path`, `depth` ve `root_post_id` neden trigger ile dolduruluyor (uygulama
-- katmanında değil): PostgreSQL'de sütun varsayılanları (DEFAULT ve IDENTITY)
-- BEFORE trigger'ından ÖNCE uygulanır, bu yüzden `NEW.id` bu trigger içinde
-- zaten hazırdır. Böylece "önce INSERT, sonra path'i hesapla, sonra UPDATE et"
-- iki adımlı turuna gerek kalmaz; satır DB'ye asla tutarsız (path'siz veya
-- yanlış depth'li) haliyle yazılmaz, ve tüm insert yolları (API, seed, testler,
-- ileride toplu import) aynı kuralı otomatik olarak alır.
CREATE FUNCTION contents_set_path()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, public
AS $$
DECLARE
    parent           RECORD;
    root_deleted_at  timestamptz;
BEGIN
    IF NEW.parent_content_id IS NULL THEN
        -- Ana post: kendi etiketinden oluşan tek düğümlük path, kendi kökü.
        NEW.path := ('c' || NEW.id)::ltree;
        NEW.depth := 0;
        NEW.root_post_id := NEW.id;
    ELSE
        SELECT path, root_post_id, deleted_at
          INTO parent
          FROM contents
         WHERE id = NEW.parent_content_id;

        IF NOT FOUND THEN
            RAISE EXCEPTION
                'contents: parent_content_id % için bir satır bulunamadı (yeni içerik id=%)',
                NEW.parent_content_id, NEW.id;
        END IF;

        -- Silinmiş içeriğe doğrudan yanıt verilemez: ebeveyn soft-delete
        -- edilmişse burada durur.
        IF parent.deleted_at IS NOT NULL THEN
            RAISE EXCEPTION
                'contents: silinmiş içeriğe yanıt verilemez (parent_content_id=%)',
                NEW.parent_content_id;
        END IF;

        NEW.path := parent.path || ('c' || NEW.id)::ltree;
        NEW.depth := nlevel(NEW.path) - 1;
        -- Ebeveynin root_post_id'si her zaman dolu olur (post satırlarında
        -- kendi id'sine eşit, yorum satırlarında da bu trigger tarafından
        -- doldurulmuştur); COALESCE beklenmedik biçimde NULL gelme ihtimaline
        -- karşı bir güvenlik ağı, bu durumda ebeveynin kendi id'sine düşer.
        NEW.root_post_id := COALESCE(parent.root_post_id, NEW.parent_content_id);

        -- Kök post silinmişse de reddet — ebeveyn kökün kendisiyse yukarıdaki
        -- kontrol zaten yeterli, ama ağacın ortasındaki canlı bir yoruma
        -- yanıt verilerek silinmiş bir kök post'un dolaylı olarak
        -- "canlandırılmasını" (yeni yorumlarla yeniden görünür kılınmasını)
        -- engellemek için kökü ayrı bir SELECT ile kontrol ediyoruz.
        IF NEW.root_post_id <> NEW.parent_content_id THEN
            SELECT deleted_at INTO root_deleted_at FROM contents WHERE id = NEW.root_post_id;

            IF root_deleted_at IS NOT NULL THEN
                RAISE EXCEPTION
                    'contents: kök post silinmiş, bu ağaca yanıt verilemez (root_post_id=%)',
                    NEW.root_post_id;
            END IF;
        END IF;

        IF NEW.depth > 32 THEN
            RAISE EXCEPTION
                'contents: maksimum içerik ağacı derinliği (32) aşıldı (yeni içerik id=%, hesaplanan depth=%)',
                NEW.id, NEW.depth;
        END IF;
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER trg_contents_set_path
    BEFORE INSERT ON contents
    FOR EACH ROW
    EXECUTE FUNCTION contents_set_path();

COMMENT ON TABLE contents IS
    'Post''lar ve yorumlar tek tabloda; ağaç yapısı ltree `path` ile temsil edilir. '
    'Post: parent_content_id NULL, depth 0. Yorum: parent_content_id dolu, path ebeveynden türer.';
COMMENT ON COLUMN contents.root_post_id IS
    'En üstteki post''un id''si. Post satırında kendi id''sine eşittir; bir yorum ağacının '
    'tamamını tek koşulla (root_post_id = X) çekebilmek için denormalize edilmiştir.';
COMMENT ON COLUMN contents.parent_content_id IS
    'NULL ise bu satır bir post''tur (ana içerik). Dolu ise doğrudan cevap verdiği içerik.';
COMMENT ON COLUMN contents.path IS
    'Kök post''tan bu satıra kadar olan zincir, ör. c1.c42.c93. `c` öneki bilinçli: saf '
    'sayısal ltree etiketleri okunurluğu düşürüyor. BEFORE INSERT trigger''ı tarafından doldurulur.';
COMMENT ON COLUMN contents.depth IS
    'nlevel(path) - 1. Post için 0. Uygulama tarafından değil, trigger tarafından hesaplanır.';
COMMENT ON COLUMN contents.title IS
    'Sadece content_type=''post'' satırlarında dolu; yorumlarda NULL (bkz. ck_contents_shape).';
COMMENT ON COLUMN contents.metadata IS
    'Serbest biçimli ek veri (ör. link post''ları için URL önizleme bilgisi). Boş obje = veri yok.';
COMMENT ON COLUMN contents.score IS
    'Denormalize sayaç: upvotes - downvotes. Negatif olabilir. Faz 9/11''de aynı transaction '
    'içinde güncellenir; bu migration sadece kolonu tanımlar (bkz. docs/db-conventions.md).';
COMMENT ON COLUMN contents.hot_score IS
    'Zaman-ağırlıklı sıralama skoru (Reddit "hot" algoritması benzeri). Periyodik job ile yeniden hesaplanır.';
COMMENT ON COLUMN contents.edited_at IS
    'NULL = hiç düzenlenmedi. Dolu ise body ve/veya title en az bir kez değiştirilmiş.';
COMMENT ON COLUMN contents.deleted_at IS
    'NULL = canlı. Dolu ise soft-delete edilmiş; ağaçtaki yeri (path) korunur ki alt yorumlar '
    'yetim kalmasın, sadece içerik gizlenir.';
COMMENT ON FUNCTION contents_set_path() IS
    'BEFORE INSERT trigger fonksiyonu: contents.path/depth/root_post_id''i NEW.id üzerinden '
    'hesaplar. IDENTITY sütun varsayılanı BEFORE trigger''dan önce uygulandığı için NEW.id '
    'burada zaten atanmış olur (bkz. docs/db-conventions.md). Ayrıca silinmiş bir içeriğe veya '
    'kök post''u silinmiş bir ağaca yanıt verilmesini şema seviyesinde engeller.';
