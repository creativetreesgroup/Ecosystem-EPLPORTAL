-- 0022_retention_role.sql
-- Least-privilege role for the Fase 8 retention worker. app_role is REVOKEd
-- DELETE on accept_events (append-only, migration 0008), so retention cannot
-- run as app_role. This role can SELECT/DELETE exactly the three growth tables
-- retention targets, and write archive_runs. NOLOGIN: a login role is GRANTed
-- this role in a hardened deploy; local dev connects as the `tower` owner.
-- Idempotent role creation (Postgres has no CREATE ROLE IF NOT EXISTS).
DO $$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'retention_role') THEN
        CREATE ROLE retention_role NOLOGIN;
    END IF;
END
$$;

-- So whichever role runs migrations (the `tower` owner) can SET ROLE to it in tests.
GRANT retention_role TO CURRENT_USER;

GRANT SELECT, DELETE ON bookings TO retention_role;
GRANT SELECT, DELETE ON accept_events TO retention_role;
GRANT SELECT, DELETE ON notifications TO retention_role;
GRANT SELECT, INSERT, UPDATE ON archive_runs TO retention_role;
