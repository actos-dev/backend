-- Revert migration 0031 (the visibility gate). No tables were touched, so
-- dropping the function restores the pre-gate state completely.
DROP FUNCTION content_visible_to(bigint, bigint[]);
