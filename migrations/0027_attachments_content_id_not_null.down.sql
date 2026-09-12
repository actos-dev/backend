-- Reverses the schema changes: `content_id` becomes nullable again and the
-- orphan-scanning index comes back.
--
-- The `DELETE FROM attachments WHERE content_id IS NULL` in `up` is NOT
-- reversible — those rows are gone, and with them any bookkeeping for the
-- S3 objects they pointed at (already left behind as garbage by `up`, see
-- its comment). There is nothing here to recreate them from: like
-- `migrations/0026_drop_avatar_attachments.down.sql`, this migration only
-- undoes the schema shape, not the data a forward deploy already removed.
ALTER TABLE attachments ALTER COLUMN content_id DROP NOT NULL;

CREATE INDEX idx_attachments_orphaned ON attachments (created_at)
    WHERE content_id IS NULL;

COMMENT ON TABLE attachments IS
    'Yüklenen dosyalar (görsel vb.). Yükleme ile bir içeriğe bağlanma iki ayrı adımdır; '
    'content_id NULL olan satırlar henüz hiçbir post/yoruma bağlanmamış yüklemelerdir.';
COMMENT ON COLUMN attachments.content_id IS
    'NULL olabilir: dosya önce yüklenir, sonra bir içeriğe bağlanır. Bağlanmamış (NULL) ve '
    'belli bir süreden eski satırları temizleyen bir job idx_attachments_orphaned''i kullanır.';
