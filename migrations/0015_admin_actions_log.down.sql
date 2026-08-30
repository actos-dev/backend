-- 0015_admin_actions_log.up.sql'i geri alır: trigger, fonksiyon ve tabloyu
-- ters sırada düşürür.

DROP TRIGGER trg_admin_actions_log_append_only ON admin_actions_log;
DROP FUNCTION forbid_mutation();
DROP TABLE admin_actions_log;
