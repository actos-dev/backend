-- 0017_triggers.up.sql'i geri alır: trigger'ları ve fonksiyonu ters sırada düşürür.

DROP TRIGGER trg_votes_set_updated_at ON votes;
DROP TRIGGER trg_actors_set_updated_at ON actors;
DROP FUNCTION set_updated_at();
