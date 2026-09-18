-- Revert migration 0033 (invitations and applications, phase 4B-2).
--
-- Drops the tables, their indexes and the two enum types. The indexes are
-- named explicitly for symmetry with the up migration even though
-- `DROP TABLE` would remove them anyway (an index cannot outlive its table);
-- the explicit form keeps the revert readable as "everything the up
-- migration created is gone".
--
-- **The three `notification_kind` values cannot be removed.** Postgres has no
-- `DROP VALUE` for an enum (`ALTER TYPE ... DROP VALUE` does not exist, and
-- deleting the `pg_enum` row directly is not supported). So after a revert
-- the type still carries `community_invitation`, `community_application` and
-- `community_application_result`; re-running the up migration succeeds
-- because it adds them with `IF NOT EXISTS`. This is the one asymmetry in the
-- round trip and it is deliberate, not an oversight.

DROP INDEX idx_community_applications_queue;
DROP INDEX uq_community_applications_pending;
DROP INDEX idx_community_invitations_invitee;
DROP INDEX uq_community_invitations_pending;

DROP TABLE community_applications;
DROP TABLE community_invitations;

DROP TYPE application_status;
DROP TYPE invitation_status;
