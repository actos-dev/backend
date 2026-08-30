-- Post/yorum şikayetleri; moderasyon kuyruğunu besler. target_id her zaman
-- contents.id'ye referanstır (contents post ve yorumları tek tabloda tutar,
-- bkz. 0005_contents); target_type bu id'nin post mu yorum mu olduğunu ayırt eder.

CREATE TYPE report_target_type AS ENUM ('post', 'comment');
CREATE TYPE report_status AS ENUM ('pending', 'resolved', 'dismissed');

CREATE TABLE reports (
    id                 bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    reporter_actor_id  bigint NOT NULL REFERENCES actors (id) ON DELETE RESTRICT,
    target_type        report_target_type NOT NULL,
    target_id          bigint NOT NULL REFERENCES contents (id) ON DELETE RESTRICT,
    reason             text NOT NULL,
    status             report_status NOT NULL DEFAULT 'pending',
    notes              text NULL,
    created_at         timestamptz NOT NULL DEFAULT now(),
    resolved_by        bigint NULL REFERENCES actors (id) ON DELETE RESTRICT,
    resolved_at        timestamptz NULL,

    CONSTRAINT uq_reports_reporter_target UNIQUE (reporter_actor_id, target_type, target_id),
    CONSTRAINT ck_reports_reason_length CHECK (char_length(reason) BETWEEN 1 AND 1000),
    CONSTRAINT ck_reports_notes_length CHECK (char_length(notes) <= 1000),
    CONSTRAINT ck_reports_resolution_shape CHECK (
        (status = 'pending' AND resolved_by IS NULL AND resolved_at IS NULL)
        OR
        (status <> 'pending' AND resolved_by IS NOT NULL AND resolved_at IS NOT NULL)
    )
);

-- Moderasyon kuyruğu: bekleyen şikayetleri en eskiden yeniye (veya tersi)
-- listelemek için kısmi index.
CREATE INDEX idx_reports_pending_queue ON reports (status, created_at) WHERE status = 'pending';

COMMENT ON TABLE reports IS
    'Post/yorum şikayetleri; moderasyon kuyruğunu besler. uq_reports_reporter_target aynı '
    'actor''ın aynı hedefi tekrar tekrar raporlayarak kuyruğu şişirmesini engeller.';
COMMENT ON COLUMN reports.target_type IS
    'Şikayet edilen içeriğin türü (post ya da comment). target_id her iki durumda da '
    'contents.id''dir; contents post ve yorumları tek tabloda tuttuğu için ayrı bir FK hedefi '
    'gerekmez, target_type sadece anlam ayrımı içindir.';
COMMENT ON COLUMN reports.target_id IS
    'contents.id''ye referans. Hedef silinemez ama soft-delete edilebilir; bu yüzden RESTRICT.';
COMMENT ON COLUMN reports.notes IS
    'Moderatörün rapor üzerine düştüğü not, en fazla 1000 karakter. Çözülene kadar NULL olabilir.';
COMMENT ON COLUMN reports.status IS
    'pending: henüz incelenmedi. resolved/dismissed: incelendi. ck_reports_resolution_shape, '
    'pending dışındaki her durumda resolved_by/resolved_at''in dolu olmasını, pending''de ise '
    'ikisinin de NULL olmasını zorunlu kılar.';
COMMENT ON COLUMN reports.resolved_by IS
    'Şikayeti çözen/reddeden moderatörün actor_id''si. status=''pending'' iken NULL olmalıdır.';
