CREATE TABLE rule_booking_targets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    rule_id UUID NOT NULL REFERENCES accept_rules(id) ON DELETE CASCADE,
    booking_id_raw TEXT NOT NULL,
    booking_id_norm TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT rule_booking_targets_tenant_norm_unique UNIQUE (tenant_id, booking_id_norm)
);

CREATE INDEX idx_rule_booking_targets_rule ON rule_booking_targets (rule_id);
