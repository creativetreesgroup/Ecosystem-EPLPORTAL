CREATE TABLE push_subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    portal_user_id UUID NOT NULL REFERENCES portal_users(id) ON DELETE CASCADE,
    endpoint TEXT NOT NULL,
    p256dh TEXT NOT NULL,
    auth TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT push_subscriptions_tenant_endpoint_unique UNIQUE (tenant_id, endpoint)
);

CREATE INDEX idx_push_subscriptions_user ON push_subscriptions (portal_user_id);
