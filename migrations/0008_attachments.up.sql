-- Yüklenen dosyalar (görsel vb.). Yükleme ve içeriğe bağlama iki ayrı adım:
-- dosya önce object storage'a yüklenir ve burada bir satır oluşturur
-- (content_id NULL), sonra post/yorum kaydedilirken content_id set edilir.

CREATE TABLE attachments (
    id                bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    content_id        bigint NULL REFERENCES contents (id) ON DELETE CASCADE,
    actor_id          bigint NOT NULL REFERENCES actors (id) ON DELETE RESTRICT,
    object_key        text NOT NULL UNIQUE,
    byte_size         bigint NOT NULL,
    mime_type         text NOT NULL,
    width             int NULL,
    height            int NULL,
    checksum_sha256   text NOT NULL,
    created_at        timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT ck_attachments_byte_size_positive CHECK (byte_size > 0),
    CONSTRAINT ck_attachments_checksum_sha256_format CHECK (checksum_sha256 ~ '^[0-9a-f]{64}$')
);

-- Bir içeriğin eklerini çekmek için.
CREATE INDEX idx_attachments_content ON attachments (content_id);

-- Bağlanmamış (yetim) yüklemeleri temizleyen job için: content_id NULL olan
-- ve belli bir süreden eski satırları bulur.
CREATE INDEX idx_attachments_orphaned ON attachments (created_at)
    WHERE content_id IS NULL;

COMMENT ON TABLE attachments IS
    'Yüklenen dosyalar (görsel vb.). Yükleme ile bir içeriğe bağlanma iki ayrı adımdır; '
    'content_id NULL olan satırlar henüz hiçbir post/yoruma bağlanmamış yüklemelerdir.';
COMMENT ON COLUMN attachments.content_id IS
    'NULL olabilir: dosya önce yüklenir, sonra bir içeriğe bağlanır. Bağlanmamış (NULL) ve '
    'belli bir süreden eski satırları temizleyen bir job idx_attachments_orphaned''i kullanır.';
COMMENT ON COLUMN attachments.object_key IS
    'MinIO/S3 üzerindeki dosyanın object key''i (URL değil), actors.avatar_object_key ile aynı kalıp.';
COMMENT ON COLUMN attachments.checksum_sha256 IS
    'Dosya içeriğinin SHA-256''sı, hex string (64 karakter). Bütünlük doğrulaması ve '
    'aynı dosyanın tekrar yüklenmesini tespit etmek için.';
COMMENT ON COLUMN attachments.width IS
    'Görsel/video ekler için piksel genişliği. Görsel olmayan dosyalarda (ör. PDF) NULL.';
