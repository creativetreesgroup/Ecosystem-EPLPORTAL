CREATE TABLE accept_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    booking_id UUID REFERENCES bookings(id) ON DELETE SET NULL,
    rule_id UUID REFERENCES accept_rules(id) ON DELETE SET NULL,
    outcome TEXT NOT NULL CHECK (outcome IN (
        'accepted', 'rejected', 'skipped', 'taken_by_agency', 'failed', 'agency_dup_unverified'
    )),
    local_dispatch_us BIGINT,
    accept_e2e_ms BIGINT,
    detail JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_accept_events_tenant_created ON accept_events (tenant_id, created_at DESC);
CREATE INDEX idx_accept_events_created_brin ON accept_events USING BRIN (created_at);

-- Append-only enforcement: `app_role` may SELECT/INSERT but never UPDATE/DELETE.
-- `CREATE ROLE IF NOT EXISTS` doesn't exist in Postgres — guard with a DO block
-- so this migration is safe to run against a cluster where the role already
-- exists (e.g. a test DB recreated without recreating the whole cluster).
DO $$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'app_role') THEN
        CREATE ROLE app_role NOLOGIN;
    END IF;
END
$$;

-- Grant app_role to whichever role runs this migration, so that role (the
-- same one the application connects as, in a simple single-role setup) can
-- `SET ROLE app_role` to prove/exercise the restricted grants.
GRANT app_role TO CURRENT_USER;
GRANT SELECT, INSERT ON accept_events TO app_role;
REVOKE UPDATE, DELETE ON accept_events FROM app_role;
