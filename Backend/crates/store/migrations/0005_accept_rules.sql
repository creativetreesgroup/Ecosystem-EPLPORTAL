-- `array_to_string` is marked STABLE (not IMMUTABLE) in Postgres's catalog
-- (confirmed via `pg_proc.provolatile` on Postgres 16) — polymorphic array
-- functions that invoke an element type's output function are conservatively
-- classified STABLE across the board, even though for a plain TEXT[] joined
-- with a literal separator the result is fully deterministic. A `GENERATED
-- ALWAYS AS ... STORED` column requires every function in its expression to
-- be IMMUTABLE, so `array_to_string(destinations, '>')` directly inside the
-- generated expression below fails at CREATE TABLE time with "generation
-- expression is not immutable". This thin SQL wrapper re-declares the same
-- (in this restricted usage, genuinely deterministic) behavior as IMMUTABLE.
CREATE FUNCTION accept_rules_destinations_join_immutable(arr TEXT[], sep TEXT)
RETURNS TEXT
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT array_to_string(arr, sep);
$$;

CREATE TABLE accept_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT false,
    priority INT NOT NULL DEFAULT 0,
    mode TEXT NOT NULL CHECK (mode IN ('booking_id', 'route', 'filter')),
    service_types TEXT[] NOT NULL DEFAULT '{}',
    max_weight REAL,
    coc_only BOOLEAN NOT NULL DEFAULT false,
    non_coc_only BOOLEAN NOT NULL DEFAULT false,
    max_cod_amount REAL,
    origin TEXT NOT NULL DEFAULT '',
    destinations TEXT[] NOT NULL DEFAULT '{}',
    booking_type TEXT NOT NULL DEFAULT 'all' CHECK (booking_type IN ('spxid', 'reguler', 'all')),
    shift_types INT[] NOT NULL DEFAULT '{}',
    trip_types INT[] NOT NULL DEFAULT '{}',
    match_mode TEXT NOT NULL DEFAULT 'strict' CHECK (match_mode IN ('strict', 'flexible')),
    min_deadline_min INT,
    max_accept_count INT NOT NULL DEFAULT 0,
    accepted_count INT NOT NULL DEFAULT 0,
    route_signature TEXT GENERATED ALWAYS AS (
        lower(regexp_replace(origin, '[^a-zA-Z0-9]+', ' ', 'g')) || '|' ||
        accept_rules_destinations_join_immutable(destinations, '>') || '|' || match_mode || '|' || booking_type
    ) STORED,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT accept_rules_destinations_max5 CHECK (
        array_length(destinations, 1) IS NULL OR array_length(destinations, 1) <= 5
    )
);

CREATE INDEX idx_accept_rules_tenant ON accept_rules (tenant_id);
-- Dedup lane: only one ROUTE-mode rule per tenant may occupy a given
-- normalized lane signature. booking_id/filter modes are unrestricted here
-- (their own dedup semantics live in core-domain's dedupe_rules, applied
-- before insert — this index only enforces the route-lane invariant at the
-- DB level as a backstop).
CREATE UNIQUE INDEX idx_accept_rules_route_dedup ON accept_rules (tenant_id, route_signature)
    WHERE mode = 'route';
