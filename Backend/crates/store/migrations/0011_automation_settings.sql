CREATE TABLE automation_settings (
    tenant_id UUID PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    auto_accept_enabled BOOLEAN NOT NULL DEFAULT false,
    poll_interval_ms INT NOT NULL DEFAULT 1000,
    smart_paused BOOLEAN NOT NULL DEFAULT false,
    smart_paused_until TIMESTAMPTZ,
    smart_dry_run BOOLEAN NOT NULL DEFAULT false,
    smart_schedule JSONB NOT NULL DEFAULT '{}',
    smart_blacklist TEXT[] NOT NULL DEFAULT '{}',
    counter_reset_hour INT,
    counter_reset_last_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
