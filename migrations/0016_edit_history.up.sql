-- Post/yorum düzenleme geçmişi: bir content satırı her düzenlendiğinde
-- (title ve/veya body değiştiğinde) düzenlemeden önceki hali burada bir satır
-- olarak saklanır. v1'de uygulama katmanı tarafından doldurulur; geçmişi
-- gösteren bir okuma endpoint'i sonraki bir fazda açılacak -- bu migration
-- sadece veriyi biriktirmeyi garanti eder.

CREATE TABLE edit_history (
    id              bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    content_id      bigint NOT NULL REFERENCES contents (id) ON DELETE CASCADE,
    previous_title  text NULL,
    previous_body   text NOT NULL,
    edited_at       timestamptz NOT NULL DEFAULT now()
);

-- "Bu içeriğin düzenleme geçmişi" sorgusu, en yeniden eskiye.
CREATE INDEX idx_edit_history_content_edited ON edit_history (content_id, edited_at DESC);

COMMENT ON TABLE edit_history IS
    'Bir content satırı her düzenlendiğinde eski halini (previous_title/previous_body) saklar. '
    'v1''de uygulama katmanı düzenleme işlemiyle aynı transaction''da doldurur; bu veriyi okuyan '
    'bir endpoint henüz yok, sonraki bir fazda açılacak.';
COMMENT ON COLUMN edit_history.previous_title IS
    'Düzenlemeden önceki title. content_type=''comment'' satırlarında contents.title zaten NULL '
    'olduğu için burada da NULL olabilir (bkz. 0005_contents ck_contents_shape).';
COMMENT ON COLUMN edit_history.previous_body IS
    'Düzenlemeden önceki body. contents.body her satırda NOT NULL olduğu için burası da her zaman doludur.';
COMMENT ON COLUMN edit_history.content_id IS
    'Düzenlenen content''e referans. contents soft-delete kullandığı için (hard delete yok, bkz. '
    'docs/db-conventions.md) ON DELETE CASCADE pratikte tetiklenmez; yine de şema seviyesinde '
    'tutarlılık için tanımlanmıştır.';
