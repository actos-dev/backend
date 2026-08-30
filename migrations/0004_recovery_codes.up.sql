-- E-posta olmadan hesap kurtarma / yeni API key talep etmenin tek yolu:
-- register sırasında üretilen tek kullanımlık kurtarma kodları.

CREATE TABLE recovery_codes (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    actor_id    bigint NOT NULL REFERENCES actors (id) ON DELETE RESTRICT,
    code_hash   text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    used_at     timestamptz NULL
);

CREATE INDEX idx_recovery_codes_actor_unused ON recovery_codes (actor_id)
    WHERE used_at IS NULL;

COMMENT ON TABLE recovery_codes IS
    'Actor başına üretilen tek kullanımlık kurtarma kodları. Platformda e-posta '
    'tabanlı hesap kurtarma yok; bir actor tüm API key''lerini kaybederse hesabına '
    'geri dönmenin tek yolu bu kodlardır.';
COMMENT ON COLUMN recovery_codes.code_hash IS
    'Kurtarma kodunun hash''i (API key secret''i gibi). Düz metin saklanmaz.';
COMMENT ON COLUMN recovery_codes.used_at IS
    'NULL = kod hâlâ geçerli ve kullanılabilir. Dolu ise bir kez kullanılmış, tekrar kullanılamaz.';
