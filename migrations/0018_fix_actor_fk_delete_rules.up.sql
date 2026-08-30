-- 0012 ve 0013, admin_roles.actor_id ve bans.actor_id için yanlışlıkla
-- ON DELETE CASCADE kullandı. docs/db-conventions.md kuralına göre actor'a
-- bağlı tablolar ON DELETE RESTRICT kullanmalı; CASCADE sadece saf ilişki
-- tabloları (votes, follows, saves, content_tags) içindir. Bir actor
-- hard-delete edildiğinde ban/rol kaydının sessizce CASCADE ile silinmesi
-- moderasyon geçmişinin kaybı demektir. RESTRICT, bu tür bir hard-delete'in
-- önce ban/rol kaydı bilinçli olarak kaldırılmadan başarısız olmasını
-- sağlar. (Actor'lar normalde soft-delete edildiği için bu yol yalnızca
-- istisnai, ör. yasal silme taleplerinde işler; oradaki "yanlışlıkla
-- olmama" tam olarak istenen davranıştır.)
--
-- Bu migration 0012 ve 0013'ü değiştirmez (uygulanmış migration'lar
-- değiştirilemez, bkz. docs/db-conventions.md); onun yerine iki FK'yi
-- düşürüp RESTRICT ile yeniden oluşturur. Kısıt adları canlı veritabanından
-- teyit edildi (psql \d admin_roles / \d bans): şemadaki her FK gibi
-- PostgreSQL'in varsayılan `<tablo>_<kolon>_fkey` kalıbını kullanıyorlar
-- (repo'da ayrıca isimlendirilmiş `fk_...` kısıt yok), bu yeniden
-- oluşturmada da aynı isimler korunuyor.

ALTER TABLE admin_roles
    DROP CONSTRAINT admin_roles_actor_id_fkey;

ALTER TABLE admin_roles
    ADD CONSTRAINT admin_roles_actor_id_fkey
    FOREIGN KEY (actor_id) REFERENCES actors (id) ON DELETE RESTRICT;

ALTER TABLE bans
    DROP CONSTRAINT bans_actor_id_fkey;

ALTER TABLE bans
    ADD CONSTRAINT bans_actor_id_fkey
    FOREIGN KEY (actor_id) REFERENCES actors (id) ON DELETE RESTRICT;
