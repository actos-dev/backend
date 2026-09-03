-- Oy ağırlığı (vote weight) — güven kademesinin OY tarafındaki etkisi.
-- Temel (`actors.trust_level` sütunu) `migrations/0020_trust_levels.up.sql`de
-- atıldı; o migration BİLEREK "kademenin ETKİLERİ ayrı bir görevde" diyordu.
-- Bu migration NOTES.md §9.3'teki "oy ağırlığı hesabın kademesine bağlıdır;
-- taze hesabın oyu sıralamayı kıpırdatmaz (silinmez, sayılır, ama ağırlığı
-- düşüktür)" kararının somutlaşmış hâli.

ALTER TABLE votes
    ADD COLUMN weight smallint NOT NULL DEFAULT 1;

-- Bugünkü kademe → ağırlık eşlemesi (bkz. `crate::interaction::set_vote`):
-- seviye 0 → 0 (oy kaydedilir, sayaçlara işler, ama skora hiç katkı vermez),
-- seviye 1 ve 2 → 1 (tam ağırlık). Üç kademe var ama iki ağırlık değeri —
-- seviye 1 ile 2 arasında oy gücü bakımından bugün fark yok, fark rate
-- limit/depolama kotası gibi başka etkilerde (ayrı görevler). Şema bunu
-- CHECK ile kilitliyor ki uygulama katmanındaki bir hesaplama hatası (ör.
-- "seviye 2 → ağırlık 2" gibi henüz var olmayan bir kural) sessizce geçersiz
-- bir değer yazamasın — `migrations/0020_trust_levels.up.sql`deki
-- `ck_actors_trust_level_range` ile aynı gerekçe. Eşleme ileride değişirse
-- (ör. seviye 2'ye daha yüksek bir ağırlık verilirse) bu CHECK'in de
-- birlikte güncellenmesi gerekir.
ALTER TABLE votes
    ADD CONSTRAINT ck_votes_weight_range CHECK (weight IN (0, 1));

-- DEFAULT 1 seçildi ki bu migration çalıştığında tabloda zaten duran
-- oylar (bu migration öncesi yazılmış, ağırlık kavramı hiç yokken "tam
-- ağırlıklı" sayılan oylar) `contents.score`'un o ana kadarki değeriyle
-- tutarlı kalsın — geriye dönük hiçbir oyun katkısı sessizce değişmiyor.
-- Yeni oylar için uygulama katmanı `INSERT`/`UPDATE`'te ağırlığı HER ZAMAN
-- açıkça veriyor (bkz. aşağıdaki COMMENT); DEFAULT yalnızca bu geçmiş
-- veriyi ve ileride açıkça ağırlık vermeyen olası bir kod yolunu (ör. elle
-- SQL) "en güvenli" değere (tam ağırlık, mevcut davranış) düşürmek için bir
-- güvenlik ağı.
COMMENT ON COLUMN votes.weight IS
    'Oyun skora katkısının çarpanı (bugün yalnızca 0 ya da 1, bkz. ck_votes_weight_range). '
    'crate::interaction::set_vote oy YAZILDIĞI/DEĞİŞTİRİLDİĞİ ANDA oy verenin o anki '
    'actors.trust_level''ına bakıp bu sütuna yazıyor — SONRADAN geriye dönük yeniden '
    'HESAPLANMIYOR. Bu bilinçli bir tercih: bir actor terfi/rütbe düşürme her yaşadığında '
    'onun verdiği TÜM geçmiş oyları dolaşıp contents.score''u yeniden toplamak, her '
    'kademe değişiminde potansiyel olarak binlerce contents satırını güncellemek '
    'demek olurdu (bkz. crate::actor::recompute_trust_levels''ın periyodik sıklığı) — '
    'ağırlık bu yüzden oy anının bir FOTOĞRAFI, kademe geçmişte kalsa da değişmiyor. '
    'crate::interaction::set_vote bir oy değiştirildiğinde/geri çekildiğinde ESKİ '
    'katkıyı bu sütunda SAKLI DURAN değerle hesaplar, oy verenin BUGÜNKÜ kademesiyle '
    'değil — aksi hâlde seviye 0''ken oy verip sonra terfi eden biri oyunu geri '
    'çekince contents.score eksiye kayardı (bkz. crates/actos-core/tests/interaction.rs, '
    '"terfi_sonrasi_geri_cekilen..." testi). votes.value''in aksine (ham yön: -1/1), '
    'bu sütun contents.upvotes/downvotes''u ETKİLEMİYOR — onlar hâlâ ham oy SAYISI, '
    'yalnızca contents.score = sum(value * weight) bu ağırlığı görüyor.';
