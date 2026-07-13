CREATE TABLE notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    channel TEXT NOT NULL CHECK (channel IN ('whatsapp', 'push')),
    payload JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed')),
    attempts INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at TIMESTAMPTZ
);

-- `status = 'pending'` is an immutable literal comparison (unlike `now()`), so
-- this partial index is valid and is exactly what a `SELECT ... FOR UPDATE
-- SKIP LOCKED` worker-claim query (Fase 5) will scan.
CREATE INDEX idx_notifications_pending ON notifications (created_at) WHERE status = 'pending';
