-- Post ve yorumlara verilen yukarı/aşağı oylar. Saf ilişki tablosu olduğu
-- için her iki FK de ON DELETE CASCADE (bkz. docs/db-conventions.md).

CREATE TABLE votes (
    actor_id    bigint NOT NULL REFERENCES actors (id) ON DELETE CASCADE,
    content_id  bigint NOT NULL REFERENCES contents (id) ON DELETE CASCADE,
    value       smallint NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),

    PRIMARY KEY (actor_id, content_id),

    -- 0 diye bir değer yok: oy geri çekilince satır silinir, güncellenmez.
    CONSTRAINT ck_votes_value CHECK (value IN (-1, 1))
);

-- "Bu içeriğin oyları" sorgusu için ters index (PK sadece actor_id ile
-- başlayan sorguları hızlandırır, content_id ile başlayanları değil).
CREATE INDEX idx_votes_content ON votes (content_id);

COMMENT ON COLUMN votes.value IS
    'Sadece -1 (aşağı) veya 1 (yukarı). Nötr/geri çekilmiş oy, satırın kendisinin silinmesiyle '
    'ifade edilir.';
COMMENT ON COLUMN votes.updated_at IS
    'Bir actor oyunu -1''den 1''e (veya tersi) çevirdiğinde güncellenir; ilk oyda created_at ile aynıdır.';
COMMENT ON TABLE votes IS
    'Bir actor''ın bir içeriğe verdiği tek oy. contents.score/upvotes/downvotes sayaçları bu '
    'tablodan trigger ile TÜRETİLMEZ; oyu yazan işlemle aynı transaction içinde uygulama '
    'katmanı günceller (bkz. docs/db-conventions.md, "Sayaç kolonları"). PK (actor_id, content_id) '
    'aynı actor''ın aynı içeriğe iki kez oy vermesini engeller. Oy geri çekildiğinde satır silinir '
    '(value=0 yok).';
