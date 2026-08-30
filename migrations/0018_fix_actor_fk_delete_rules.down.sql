-- 0018_fix_actor_fk_delete_rules.up.sql'i geri alır: admin_roles.actor_id
-- ve bans.actor_id FK'lerini 0012/0013'teki orijinal (hatalı) haline,
-- ON DELETE CASCADE'e döndürür.

ALTER TABLE admin_roles
    DROP CONSTRAINT admin_roles_actor_id_fkey;

ALTER TABLE admin_roles
    ADD CONSTRAINT admin_roles_actor_id_fkey
    FOREIGN KEY (actor_id) REFERENCES actors (id) ON DELETE CASCADE;

ALTER TABLE bans
    DROP CONSTRAINT bans_actor_id_fkey;

ALTER TABLE bans
    ADD CONSTRAINT bans_actor_id_fkey
    FOREIGN KEY (actor_id) REFERENCES actors (id) ON DELETE CASCADE;
