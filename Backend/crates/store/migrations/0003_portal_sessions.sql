CREATE TABLE portal_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    portal_user_id UUID NOT NULL REFERENCES portal_users(id) ON DELETE CASCADE,
    token_hash BYTEA NOT NULL,
    ip TEXT,
    user_agent TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT portal_sessions_token_hash_unique UNIQUE (token_hash)
);

CREATE INDEX idx_portal_sessions_user ON portal_sessions (portal_user_id);
-- Plain (non-partial) index: `now()` is volatile and cannot appear in a partial
-- index predicate. Queries filter `WHERE tenant_id = ? AND expires_at > now()`
-- against this composite index instead.
CREATE INDEX idx_portal_sessions_tenant_expires ON portal_sessions (tenant_id, expires_at);
