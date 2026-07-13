-- Row-Level Security for all 13 tenant-scoped business tables.
--
-- `tenants` itself is excluded: it has no `tenant_id` column to key a policy
-- on (it IS the tenant). `archive_runs` is excluded per Task 6: it's a
-- system-wide maintenance record, not tenant-scoped.
--
-- `ENABLE ROW LEVEL SECURITY` alone does NOT restrict the table owner —
-- only `FORCE ROW LEVEL SECURITY` does. Both are applied to every table
-- below; do not drop FORCE, or ordinary owner/superuser connections (the
-- common simple-single-role production topology this project runs) would
-- silently bypass tenant isolation entirely.
--
-- The policy predicate uses `current_setting('app.tenant_id', true)` — the
-- `true` (missing_ok) means an unset `app.tenant_id` yields NULL rather than
-- raising, so `tenant_id = NULL` is simply never true (zero rows) instead of
-- erroring. See `begin_tenant_tx` in `pool.rs`, which sets `app.tenant_id`
-- via `set_config(..., true)` (transaction-local) for every tenant-scoped
-- query path.
--
-- The predicate additionally wraps that call in `NULLIF(..., '')`. This is
-- NOT optional polish — without it, the "unset -> silently zero rows, never
-- an error" guarantee above only holds for a session that has *never* once
-- referenced `app.tenant_id`. Postgres custom GUCs are lazily created on
-- first reference; once any transaction on a given (pooled, long-lived)
-- connection has called `set_config('app.tenant_id', ..., true)`, that
-- placeholder now exists for the rest of the session, and `set_config`'s
-- transaction-local ("SET LOCAL"-equivalent) semantics revert its value to
-- an EMPTY STRING at commit/rollback, not back to a "never set" NULL state.
-- `current_setting('app.tenant_id', true)` on such a connection then returns
-- `''`, and `''::uuid` raises `invalid input syntax for type uuid` instead
-- of matching nothing. In a real pooled deployment, essentially every
-- connection reaches this state after its first tenant-scoped transaction —
-- so without `NULLIF`, any later code path that forgot to call
-- `begin_tenant_tx` would hard-error (or worse, depending on caller error
-- handling) rather than the documented fail-closed "sees nothing" behavior.
-- `NULLIF(x, '')` collapses that empty string back to NULL before the
-- `::uuid` cast, restoring the invariant for every connection regardless of
-- its transaction history.
DO $$
DECLARE
    t TEXT;
BEGIN
    FOREACH t IN ARRAY ARRAY[
        'portal_users', 'portal_sessions', 'agency_credentials', 'accept_rules',
        'rule_booking_targets', 'bookings', 'accept_events', 'notifications',
        'push_subscriptions', 'automation_settings', 'site_settings',
        'route_prices', 'route_locations'
    ]
    LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON %I USING (tenant_id = NULLIF(current_setting(''app.tenant_id'', true), '''')::uuid)',
            t
        );
    END LOOP;
END
$$;

-- Grant `app_role` (created by migration 0008) the CRUD it needs on these 12
-- tables (all 13 above minus `accept_events`, handled separately below) so a
-- non-superuser, non-BYPASSRLS connection can actually exercise them.
--
-- This matters because ENABLE/FORCE ROW LEVEL SECURITY only restrict the
-- table owner — they do nothing at all for a superuser or any role with the
-- BYPASSRLS attribute, which Postgres unconditionally exempts from row
-- security regardless of FORCE (this is documented core Postgres behavior,
-- not something any ALTER TABLE flag can change). In this project's current
-- dev/CI topology the only configured Postgres login (`tower`,
-- `POSTGRES_USER` in `Docker/docker-compose.yml`) IS a superuser (Postgres's
-- official image always makes the bootstrap `POSTGRES_USER` a superuser) —
-- so the RLS policies above, while correct, are unobservable through that
-- connection: `tower` bypasses every policy on every one of these tables no
-- matter what the policy says. `app_role` is a plain `NOLOGIN` role (no
-- SUPERUSER, no BYPASSRLS) reachable only via `SET ROLE app_role` from a
-- role granted membership (`tower`, per migration 0008's
-- `GRANT app_role TO CURRENT_USER`) — it is genuinely subject to RLS, and is
-- the role the running application is meant to use for its tenant-scoped
-- queries (see design doc / master plan).
--
-- `accept_events` is intentionally excluded from this loop: migration 0008
-- already grants it SELECT/INSERT and explicitly REVOKEs UPDATE/DELETE from
-- app_role to enforce its append-only invariant (Aturan Keras). Re-granting
-- broader access to it here would silently undo that.
DO $$
DECLARE
    t TEXT;
BEGIN
    FOREACH t IN ARRAY ARRAY[
        'portal_users', 'portal_sessions', 'agency_credentials', 'accept_rules',
        'rule_booking_targets', 'bookings', 'notifications',
        'push_subscriptions', 'automation_settings', 'site_settings',
        'route_prices', 'route_locations'
    ]
    LOOP
        EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON %I TO app_role', t);
    END LOOP;
END
$$;
