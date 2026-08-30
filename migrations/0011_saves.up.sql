-- Actor'ların içerikleri kaydetmesi (bookmark). Saf ilişki tablosu olduğu
-- için her iki FK de ON DELETE CASCADE (bkz. docs/db-conventions.md).

CREATE TABLE saves (
    actor_id    bigint NOT NULL REFERENCES actors (id) ON DELETE CASCADE,
    content_id  bigint NOT NULL REFERENCES contents (id) ON DELETE CASCADE,
    created_at  timestamptz NOT NULL DEFAULT now(),

    PRIMARY KEY (actor_id, content_id)
);

-- "Kaydettiklerim, yeniden eskiye" sayfalaması için index. PK zaten
-- (actor_id, content_id) ile başlıyor ama created_at DESC sıralamasını
-- desteklemiyor.
CREATE INDEX idx_saves_actor_created ON saves (actor_id, created_at DESC);

COMMENT ON TABLE saves IS
    'Bir actor''ın kaydettiği (bookmark) içerikler. PK (actor_id, content_id) aynı içeriğin aynı '
    'actor tarafından iki kez kaydedilmesini engeller.';
