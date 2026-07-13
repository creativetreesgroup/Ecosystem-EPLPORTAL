CREATE TABLE bookings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    spx_id TEXT NOT NULL,
    raw_data JSONB NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    is_coc BOOLEAN GENERATED ALWAYS AS (
        spx_id ~* '^\s*SPXID' OR COALESCE(raw_data->>'booking_name', '') ~* '^\s*SPXID'
    ) STORED,
    needs_enrichment BOOLEAN GENERATED ALWAYS AS (
        (raw_data->>'route_detail_list' IS NULL) AND (raw_data->>'route_stops' IS NULL)
    ) STORED,
    service_type TEXT,
    weight DOUBLE PRECISION NOT NULL DEFAULT 0,
    cod_amount DOUBLE PRECISION NOT NULL DEFAULT 0,
    auto_accepted BOOLEAN NOT NULL DEFAULT false,
    accept_latency_ms INT,
    rule_matched UUID REFERENCES accept_rules(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT bookings_tenant_spx_id_unique UNIQUE (tenant_id, spx_id)
);

-- Hot-path: newest-first pending list.
CREATE INDEX idx_bookings_pending ON bookings (tenant_id, created_at DESC) WHERE status = 'pending';
-- Covering index for the live-list UI query (avoids a heap fetch for the common columns).
CREATE INDEX idx_bookings_live_covering ON bookings (tenant_id, status, created_at DESC)
    INCLUDE (spx_id, service_type, weight, cod_amount, auto_accepted);
-- BRIN: bookings is large and append-mostly by created_at — BRIN is far cheaper than
-- B-tree for time-range scans on a table shaped like this.
CREATE INDEX idx_bookings_created_brin ON bookings USING BRIN (created_at);
