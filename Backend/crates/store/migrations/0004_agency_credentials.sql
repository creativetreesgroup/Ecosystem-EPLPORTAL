CREATE TABLE agency_credentials (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    label TEXT NOT NULL,
    username TEXT NOT NULL,
    ciphertext BYTEA NOT NULL,
    nonce BYTEA NOT NULL,
    key_version INT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT agency_credentials_tenant_label_unique UNIQUE (tenant_id, label)
);

CREATE INDEX idx_agency_credentials_tenant ON agency_credentials (tenant_id);
