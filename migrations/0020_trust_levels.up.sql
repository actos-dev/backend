-- Güven kademesi (trust level) TEMELİ — bkz. NOTES.md §9.3 ve PLAN.md
-- Faz 18.A "Güven kademeleri". Bu migration yalnızca taşıyıcı sütunu ve
-- onu besleyecek periyodik işin ihtiyaç duyduğu index'i kurar. Kademenin
-- ETKİLERİ (oy ağırlığı, hot filtresi, rate limit, depolama kotası) BİLEREK
-- burada YOK — NOTES.md'nin "kimlik değil kademe" savunmasının geri kalanı
-- ayrı bir görevde uygulanacak; bu migration yalnızca temeli atıyor.

ALTER TABLE actors
    ADD COLUMN trust_level smallint NOT NULL DEFAULT 0;

-- Üç kademe: 0 (yeni/doğrulanmamış), 1 (asgari etkinlik), 2 (kurulmuş).
-- Şema seviyesinde sınırlamak, uygulama katmanındaki bir hesaplama
-- hatasının (ör. yanlış bir +1) sessizce geçersiz bir kademe yazmasını
-- imkânsız kılıyor — `docs/db-conventions.md`in "veri bütünlüğü kuralı
-- CHECK'tir, sadece uygulama kodu değil" ilkesiyle aynı.
ALTER TABLE actors
    ADD CONSTRAINT ck_actors_trust_level_range CHECK (trust_level BETWEEN 0 AND 2);

-- `crate::actor::recompute_trust_levels`in periyodik işi, bir actor'ün
-- kademesini düşürecek "son 30 günde onaylanmış (resolved) rapor var mı"
-- sorusunu, o actor'ün İÇERİĞİNİ hedef alan `reports` satırları üzerinden
-- yanıtlıyor (`reports.target_id` → `contents.id` → `contents.actor_id`).
-- `reports.target_id` bir FOREIGN KEY ama Postgres FK'ler için otomatik
-- index ÜRETMEZ; `0014_reports.up.sql`'deki tek index de yalnızca
-- `status = 'pending'` kısmi index'i (moderasyon kuyruğu için) — bizim
-- filtremiz (`status = 'resolved'`) onu hiç kullanamaz. Bu index olmadan
-- her recompute turu `reports` tablosunun tamamını tarardı. Kısmi index
-- yalnızca `resolved` satırları kapsıyor (pending/dismissed asla girmiyor,
-- ki bunlar hacmin büyük çoğunluğu) ve `target_id`'yi de taşıdığı için
-- `contents` JOIN'inden önce ekstra bir heap erişimi gerekmiyor.
CREATE INDEX idx_reports_resolved_recent ON reports (resolved_at, target_id)
    WHERE status = 'resolved';

COMMENT ON COLUMN actors.trust_level IS
    'Güven kademesi (0-2), sybil/manipülasyon savunmasının temeli (bkz. NOTES.md §9.3). '
    'Seviye 1: hesap yaşı >= 24 saat VE en az 1 silinmemiş içerik (post/yorum). '
    'Seviye 2: yaş >= 7 gün VE kendi içeriği dışından (yani başkalarından) net >= 25 oy VE '
    'son 30 günde onaylanmış (resolved) rapor yok. Onaylanmış bir rapor bir kademe düşürür. '
    'SEVİYE 1 BİLEREK KARMA (oy) ŞARTI TAŞIMIYOR: yeni bir platformda ilk kullanıcıların '
    'birbirine oy verebileceği kimse yok (soğuk başlangıç) — seviye 1''e karma şartı '
    'koymak onları sonsuza dek seviye 0''da kilitlerdi. Hesaplama `crate::actor::'
    'recompute_trust_levels` içinde, periyodik iş tarafından tüm actor''ler için yeniden '
    'çalıştırılır (bkz. `TRUST_LEVEL_INTERVAL_SECS`). Kademenin SONUÇLARI (oy ağırlığı, '
    'hot filtresi, rate limit, depolama kotası) bilerek bu migration''ın kapsamı DIŞINDA.';
