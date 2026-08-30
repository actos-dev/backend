-- Admin/moderatör eylemlerinin hesap verebilirlik kaydı. Append-only: satır
-- eklendikten sonra UPDATE veya DELETE edilemez (bkz. aşağıdaki
-- trg_admin_actions_log_append_only). Bu bir denetim izidir; sonradan
-- düzenlenebilen ya da silinebilen bir denetim izinin -- "admin ne yaptı"
-- sorusuna güvenilir yanıt verme açısından -- hiçbir değeri yoktur.

CREATE TABLE admin_actions_log (
    id              bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    admin_actor_id  bigint NOT NULL REFERENCES actors (id) ON DELETE RESTRICT,
    action_type     text NOT NULL,
    target_type     text NOT NULL,
    target_id       bigint NOT NULL,
    reason          text NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),

    CONSTRAINT ck_admin_actions_log_action_type_length CHECK (char_length(action_type) BETWEEN 1 AND 64),
    CONSTRAINT ck_admin_actions_log_target_type_length CHECK (char_length(target_type) BETWEEN 1 AND 32),
    CONSTRAINT ck_admin_actions_log_reason_length CHECK (char_length(reason) <= 1000)
);

-- "Bu admin ne yaptı" sorgusu, en yeniden eskiye.
CREATE INDEX idx_admin_actions_log_admin_created ON admin_actions_log (admin_actor_id, created_at DESC);
-- Genel denetim akışı (tüm adminler), en yeniden eskiye.
CREATE INDEX idx_admin_actions_log_created ON admin_actions_log (created_at DESC);

-- Bir tabloyu append-only yapmak için genel amaçlı trigger fonksiyonu: hangi
-- tabloya bağlandıysa (TG_TABLE_NAME) ve hangi işlem denendiyse (TG_OP) onu
-- mesaja gömer, RAISE EXCEPTION ile durur. OLD, hem UPDATE hem DELETE
-- trigger'ında mevcuttur.
CREATE FUNCTION forbid_mutation()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, public
AS $$
BEGIN
    RAISE EXCEPTION
        '%.% append-only bir tablodur: % işlemi reddedildi (id=%)',
        TG_TABLE_SCHEMA, TG_TABLE_NAME, TG_OP, OLD.id;
    RETURN NULL;
END;
$$;

CREATE TRIGGER trg_admin_actions_log_append_only
    BEFORE UPDATE OR DELETE ON admin_actions_log
    FOR EACH ROW
    EXECUTE FUNCTION forbid_mutation();

COMMENT ON TABLE admin_actions_log IS
    'Admin/moderatör eylemlerinin hesap verebilirlik kaydı. Append-only (bkz. '
    'trg_admin_actions_log_append_only): sonradan düzenlenebilen bir denetim izinin değeri yoktur.';
COMMENT ON COLUMN admin_actions_log.action_type IS
    'Eylem türünü belirten serbest metin (ör. ''ban_actor'', ''delete_content'', ''resolve_report''), 1-64 karakter.';
COMMENT ON COLUMN admin_actions_log.target_type IS
    'Hedefin türünü belirten serbest metin (ör. ''actor'', ''content''), 1-32 karakter. '
    'target_id''nin hangi tabloya işaret ettiğini uygulama katmanı bu değere göre çözer.';
COMMENT ON COLUMN admin_actions_log.target_id IS
    'Eylemin hedefinin id''si. Bilerek FK DEĞİL: hedef bazen bir actor (ör. ban_actor), bazen '
    'bir content (ör. delete_content) olabiliyor -- tek bir FK ikisini birden karşılayamaz. '
    'target_type ile birlikte yorumlanır.';
COMMENT ON COLUMN admin_actions_log.reason IS
    'Eylemin gerekçesi, en fazla 1000 karakter. Bazı eylem türlerinde (ör. otomatik işlemler) NULL olabilir.';
COMMENT ON FUNCTION forbid_mutation() IS
    'Genel amaçlı BEFORE UPDATE OR DELETE trigger fonksiyonu: bağlandığı tabloyu append-only '
    'yapar, her mutasyon denemesinde RAISE EXCEPTION ile durur. admin_actions_log üzerinde kullanılır.';
