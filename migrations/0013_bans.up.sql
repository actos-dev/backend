-- Actor ban'leri. Süresiz (expires_at NULL) ya da süreli olabilir.

CREATE TABLE bans (
    actor_id    bigint NOT NULL PRIMARY KEY REFERENCES actors (id) ON DELETE CASCADE,
    banned_by   bigint NOT NULL REFERENCES actors (id) ON DELETE RESTRICT,
    reason      text NOT NULL,
    banned_at   timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NULL,

    CONSTRAINT ck_bans_reason_length CHECK (char_length(reason) BETWEEN 1 AND 1000),
    CONSTRAINT ck_bans_expires_after_banned CHECK (expires_at IS NULL OR expires_at > banned_at)
);

-- Süresi dolmuş ban'leri temizleyen/görmezden gelen periyodik job için.
-- Kalıcı ban'ler (expires_at NULL) hiç dolmayacağı için index dışında
-- bırakılıyor.
CREATE INDEX idx_bans_expires_at ON bans (expires_at) WHERE expires_at IS NOT NULL;

COMMENT ON TABLE bans IS
    'Actor ban''leri. PK actor_id: bir actor''ın aynı anda en fazla bir aktif ban kaydı olabilir.';
COMMENT ON COLUMN bans.reason IS
    'Ban gerekçesi, 1-1000 karakter (ck_bans_reason_length).';
COMMENT ON COLUMN bans.expires_at IS
    'NULL = kalıcı ban. Dolu ise bu zamandan sonra ban''in süresi dolar; idx_bans_expires_at''i '
    'kullanan bir job süresi dolanları temizler/görmezden gelir. banned_at''ten sonra olmalıdır '
    '(ck_bans_expires_after_banned).';
