-- Bir actor'ün birden fazla aktif API key'i olabilir (CLI, web, otomasyon
-- script'i için ayrı ayrı). Key doğrulaması `id` ile satır bulup tek bir
-- hash karşılaştırması yapma prensibine dayanır (bkz. aşağıdaki COMMENT'ler).

CREATE TABLE api_keys (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    actor_id      bigint NOT NULL REFERENCES actors (id) ON DELETE RESTRICT,
    secret_hash   text NOT NULL,
    label         text NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_used_at  timestamptz NULL,
    revoked_at    timestamptz NULL,

    CONSTRAINT ck_api_keys_label_length CHECK (char_length(label) <= 64)
);

CREATE INDEX idx_api_keys_actor_active ON api_keys (actor_id)
    WHERE revoked_at IS NULL;

COMMENT ON TABLE api_keys IS
    'Actor''lara ait API key''ler (Bearer token). Bir actor''ün birden fazla aktif key''i olabilir.';
COMMENT ON COLUMN api_keys.id IS
    'Kasıtlı olarak bigint DEĞİL, uuid: API key string''inin içinde (actos_<id_b62>_<secret_b62>) '
    'açıkça taşınır, bu yüzden tahmin edilemez olmalı. Kendisi hash''lenmez: doğrulama akışı önce '
    'bu id ile tek bir satır bulur, sonra sadece o satırın secret_hash''ine karşı tek bir '
    'doğrulaması yapar. Sadece hash saklansaydı (id olmadan) her istekte tüm satırları hash''lemek '
    'gerekirdi — bu hem yavaş hem DoS''a açık olurdu.';
COMMENT ON COLUMN api_keys.secret_hash IS
    'Key''in secret bölümünün SHA-256 hash''i (hex). Düz metin secret hiçbir zaman saklanmaz. '
    'Argon2 değil çünkü secret 256 bit rastgele: yavaş KDF''in kazancı yok, bedeli her istekte ödenirdi. '
    'Recovery kodları ise (kısa ve insan tarafından yazılabilir olduğu için) Argon2id ile hash''lenir.';
COMMENT ON COLUMN api_keys.label IS
    'Kullanıcının key''e verdiği serbest metin etiket, ör. "cli-macbook".';
COMMENT ON COLUMN api_keys.revoked_at IS
    'NULL = key hâlâ geçerli. Dolu ise iptal edilmiş, bir daha kullanılamaz.';
