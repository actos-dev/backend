-- Admin/moderatör rol ataması. Bilerek actors'a bir "role" kolonu eklemek
-- yerine ayrı, seyrek dolu bir tabloda tutulur (bkz. aşağıdaki COMMENT ON
-- TABLE): admin/moderatör sayısı actor sayısına kıyasla ihmal edilebilir
-- düzeydedir ve rol atama nadir, hassas bir işlemdir.

CREATE TYPE admin_role AS ENUM ('admin', 'moderator');

CREATE TABLE admin_roles (
    actor_id    bigint NOT NULL PRIMARY KEY REFERENCES actors (id) ON DELETE CASCADE,
    role        admin_role NOT NULL,
    granted_by  bigint NULL REFERENCES actors (id) ON DELETE RESTRICT,
    granted_at  timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE admin_roles IS
    'Admin/moderatör rol ataması. actors''a rol kolonu eklemek yerine bilinçli olarak ayrı ve '
    'seyrek bir tabloda tutulur: (a) actors''ın her satırı çoğu zaman kullanılmayan bir rol '
    'kolonu taşımaz, (b) granted_by/granted_at gibi atamanın kendi denetim bilgisi rolle '
    'birlikte, ayrı bir satırda yaşar, (c) "bu actor admin mi?" sorgusu actors''ın tamamını '
    'tarayan bir kolon filtresi yerine küçük bir tabloda ucuz bir EXISTS ile yanıtlanır.';
COMMENT ON COLUMN admin_roles.actor_id IS
    'Aynı zamanda PK: bir actor''ın en fazla bir rolü olabilir (admin XOR moderator, ikisi birden değil).';
COMMENT ON COLUMN admin_roles.granted_by IS
    'Rolü veren admin''in actor_id''si. NULL olabilir: platformun ilk admin''i SSH ile '
    'veritabanına doğrudan INSERT edilerek atanır; o anda rolü veren başka bir admin yoktur.';
COMMENT ON COLUMN admin_roles.granted_at IS
    'Rolün verildiği zaman. İlk admin için migration/seed''in çalıştığı zamana denk gelir.';
