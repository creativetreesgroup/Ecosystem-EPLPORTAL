-- `store::portal_sessions::find_valid_by_hash` looks up a bearer session by
-- its opaque token hash BEFORE the caller knows which tenant it belongs to
-- (discovering the tenant/user IS the point of the lookup — the session
-- middleware has nothing else to key on yet). Same chicken-and-egg shape as
-- `tenants::find_by_slug` (see migration 0017), but `portal_sessions`
-- genuinely IS in migration 0016's RLS loop (`ENABLE`/`FORCE ROW LEVEL
-- SECURITY` + the `tenant_isolation` policy keyed on
-- `current_setting('app.tenant_id', true)`), because it holds
-- bearer-session secrets that must otherwise stay strictly tenant-scoped.
-- Once Fase 6a Task 9 switches `reactor-core`'s production pool to
-- `app_role` (a genuinely RLS-restricted role, unlike today's superuser
-- `tower`, which bypasses RLS entirely and made this gap unobservable until
-- now), a plain `SELECT ... WHERE token_hash = $1` against the base table
-- would silently return zero rows for every login attempt — no
-- `app.tenant_id` is set yet at this point, and
-- `NULLIF(current_setting(...), '')::uuid` never matches NULL.
--
-- A blanket extra "app_role may SELECT any row" policy would fix that but
-- would ALSO defeat tenant isolation for every OTHER `portal_sessions` read
-- in the system (Postgres ORs multiple permissive policies together for the
-- same command) — RLS policies filter by row content, not by which WHERE
-- clause the caller used, so there is no way to scope a policy to "only
-- this lookup shape" directly.
--
-- Instead, expose exactly the one lookup shape this function needs via a
-- `SECURITY DEFINER` function. Row security is evaluated against the
-- function's OWNER (the migration-running role, `tower` — a superuser, so
-- unconditionally RLS-exempt), not its caller, so the function transparently
-- bypasses `tenant_isolation` for this one hard-coded query —
-- `token_hash = $1 AND expires_at > now()`, and `token_hash` is UNIQUE per
-- migration 0003 so at most one row can ever match. `app_role` gains the
-- ability to run only this exact shape, never unconditional table access:
-- it is granted EXECUTE on the function but no direct SELECT bypass on
-- `portal_sessions` itself.
CREATE FUNCTION portal_sessions_find_valid_by_hash(p_token_hash BYTEA)
RETURNS SETOF portal_sessions
LANGUAGE sql
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
    SELECT * FROM portal_sessions
    WHERE token_hash = p_token_hash AND expires_at > now()
$$;

REVOKE ALL ON FUNCTION portal_sessions_find_valid_by_hash(BYTEA) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION portal_sessions_find_valid_by_hash(BYTEA) TO app_role;
