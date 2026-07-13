CREATE TABLE route_prices (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    route_code TEXT NOT NULL,
    region TEXT NOT NULL DEFAULT '',
    origin TEXT NOT NULL,
    destinations JSONB NOT NULL,
    price BIGINT NOT NULL,
    vehicle_type TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT route_prices_tenant_code_unique UNIQUE (tenant_id, route_code),
    CONSTRAINT route_prices_destinations_1to5 CHECK (
        jsonb_typeof(destinations) = 'array'
        AND jsonb_array_length(destinations) BETWEEN 1 AND 5
    )
);

CREATE INDEX idx_route_prices_tenant ON route_prices (tenant_id);
