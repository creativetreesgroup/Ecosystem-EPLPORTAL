-- IMMUTABLE wrapper: array_to_string() is STABLE in Postgres's catalog (a
-- conservative classification for polymorphic array functions in general),
-- even though for a fixed-separator TEXT[] join the result is fully
-- deterministic. Generated-column expressions require every function used
-- to be IMMUTABLE, so this narrow (TEXT[], TEXT) -> TEXT wrapper — not left
-- polymorphic — re-labels exactly the deterministic case.
CREATE OR REPLACE FUNCTION accept_rules_destinations_join_immutable(arr TEXT[], sep TEXT)
RETURNS TEXT
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT array_to_string(arr, sep);
$$;

-- Mirrors core_domain::norm_loc exactly: lowercase, collapse any run of
-- non-alphanumeric characters to a single space, trim leading/trailing space.
CREATE OR REPLACE FUNCTION accept_rules_norm_loc_immutable(s TEXT)
RETURNS TEXT
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT btrim(regexp_replace(lower(s), '[^a-z0-9]+', ' ', 'g'));
$$;

-- Mirrors core_domain::dedupe_rules's dests_sig: each destination run through
-- norm_loc, empties dropped, joined with '>' (order preserved, NOT sorted —
-- matches the Rust implementation).
CREATE OR REPLACE FUNCTION accept_rules_destinations_sig_immutable(arr TEXT[])
RETURNS TEXT
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT accept_rules_destinations_join_immutable(
        ARRAY(
            SELECT accept_rules_norm_loc_immutable(elem)
            FROM unnest(arr) AS elem
            WHERE accept_rules_norm_loc_immutable(elem) <> ''
        ),
        '>'
    );
$$;

-- Mirrors core_domain::dedupe_rules's service_types_sig: each entry
-- lowercased+trimmed, empties dropped, SORTED (unlike destinations), joined
-- with ','.
CREATE OR REPLACE FUNCTION accept_rules_service_types_sig_immutable(arr TEXT[])
RETURNS TEXT
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT array_to_string(
        ARRAY(
            SELECT lower(btrim(elem))
            FROM unnest(arr) AS elem
            WHERE btrim(elem) <> ''
            ORDER BY 1
        ),
        ','
    );
$$;

CREATE TABLE accept_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT false,
    priority INT NOT NULL DEFAULT 0,
    mode TEXT NOT NULL CHECK (mode IN ('booking_id', 'route', 'filter')),
    service_types TEXT[] NOT NULL DEFAULT '{}',
    max_weight DOUBLE PRECISION,
    coc_only BOOLEAN NOT NULL DEFAULT false,
    non_coc_only BOOLEAN NOT NULL DEFAULT false,
    max_cod_amount DOUBLE PRECISION,
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
        accept_rules_norm_loc_immutable(origin) || '|' ||
        accept_rules_destinations_sig_immutable(destinations) || '|' ||
        match_mode || '|' || booking_type || '|' ||
        accept_rules_service_types_sig_immutable(service_types)
    ) STORED,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT accept_rules_destinations_max5 CHECK (
        array_length(destinations, 1) IS NULL OR array_length(destinations, 1) <= 5
    )
);

CREATE INDEX idx_accept_rules_tenant ON accept_rules (tenant_id);
-- Dedup lane: only one ROUTE-mode rule per tenant may occupy a given
-- normalized lane signature (origin + destinations + match_mode +
-- booking_type + service_types, all normalized identically to
-- core_domain::dedupe_rules). booking_id/filter modes are unrestricted here
-- (their own dedup semantics live in core-domain's dedupe_rules, applied
-- before insert — this index only enforces the route-lane invariant at the
-- DB level as a backstop, and must use the SAME key as the Rust dedup or it
-- will either miss real duplicates or reject legitimate distinct rules).
CREATE UNIQUE INDEX idx_accept_rules_route_dedup ON accept_rules (tenant_id, route_signature)
    WHERE mode = 'route';
