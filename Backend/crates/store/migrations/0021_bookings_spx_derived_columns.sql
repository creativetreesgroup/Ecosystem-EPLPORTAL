-- Backend/crates/store/migrations/0021_bookings_spx_derived_columns.sql
-- Exposes fields spx_client::normalize_booking (Backend/crates/spx-client/src/booking.rs)
-- already derives Rust-side from `raw_data`, as real generated columns so the QueryBuilder-based
-- list/filter/sort endpoints can target them in SQL. Mirrors the SAME key-priority order as
-- normalize_booking — the implementer changing either side must keep them in lockstep, or the
-- table row and its detail drawer (which still derives Rust-side) can disagree.
--
-- Two small IMMUTABLE helper functions avoid repeating the same multi-key-fallback CASE
-- expression 5+ times (DRY) — both are pure (no table access, no volatile builtins), so they're
-- safe to use inside GENERATED ALWAYS AS (...) STORED, which Postgres requires to be immutable.

CREATE OR REPLACE FUNCTION tower_pick_text(raw JSONB, keys TEXT[])
RETURNS TEXT
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT NULLIF(raw->>key, '')
    FROM unnest(keys) AS key
    WHERE raw->>key IS NOT NULL AND raw->>key <> ''
    LIMIT 1;
$$;

-- toMs port (spx-client/src/booking.rs's to_ms): 0 -> NULL (no deadline); >1e12 already
-- epoch-ms; else epoch-seconds. A non-numeric picked value becomes NULL rather than erroring
-- the whole INSERT/UPDATE (real SPX data is defensively parsed everywhere else in this
-- codebase; a generated column must not be the one place a malformed field breaks writes).
CREATE OR REPLACE FUNCTION tower_pick_epoch_ms(raw JSONB, keys TEXT[])
RETURNS TIMESTAMPTZ
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT CASE
        WHEN v IS NULL OR v !~ '^-?[0-9]+(\.[0-9]+)?$' THEN NULL
        WHEN v::numeric = 0 THEN NULL
        WHEN v::numeric > 1000000000000 THEN to_timestamp(v::numeric / 1000.0)
        ELSE to_timestamp(v::numeric)
    END
    FROM (SELECT tower_pick_text(raw, keys) AS v) picked;
$$;

ALTER TABLE bookings ADD COLUMN spx_request_id TEXT GENERATED ALWAYS AS (
    tower_pick_text(raw_data, ARRAY['request_id', 'requestId', 'req_id'])
) STORED;

ALTER TABLE bookings ADD COLUMN spx_onsite_id TEXT GENERATED ALWAYS AS (
    tower_pick_text(raw_data, ARRAY['onsite_id', 'onsiteId'])
) STORED;

-- Falls back to spx_id (this row's own column) when no tx-name key is present, matching
-- normalize_booking's `if v.is_empty() { booking_id.clone() } else { v }` fallback for spx_tx_id.
ALTER TABLE bookings ADD COLUMN spx_tx_id TEXT GENERATED ALWAYS AS (
    COALESCE(
        tower_pick_text(raw_data, ARRAY['booking_name', 'spx_tx_id', 'spxTxId', 'tx_id', 'tracking_no']),
        spx_id
    )
) STORED;

-- Prefers a display-name key; else a code key, EXCLUDING a purely-numeric code (an internal id,
-- not a real vehicle type — mirrors numeric_only_vehicle_type_is_discarded in booking.rs).
ALTER TABLE bookings ADD COLUMN spx_vehicle_type TEXT GENERATED ALWAYS AS (
    CASE
        WHEN tower_pick_text(raw_data, ARRAY['vehicle_type_name', 'right_vehicle_type_name', 'sgi_vehicle_name']) IS NOT NULL
            THEN tower_pick_text(raw_data, ARRAY['vehicle_type_name', 'right_vehicle_type_name', 'sgi_vehicle_name'])
        WHEN tower_pick_text(raw_data, ARRAY['truck_type', 'vehicle_type', 'vehicleType', 'service_type']) ~ '^[0-9]+$'
            THEN NULL
        ELSE tower_pick_text(raw_data, ARRAY['truck_type', 'vehicle_type', 'vehicleType', 'service_type'])
    END
) STORED;

ALTER TABLE bookings ADD COLUMN spx_deadline_at TIMESTAMPTZ GENERATED ALWAYS AS (
    tower_pick_epoch_ms(raw_data, ARRAY['bidding_ddl', 'deadline_at', 'pickup_time_ms', 'expired_at'])
) STORED;

-- Falls back to the same keys as spx_deadline_at (not to the generated column itself, as Postgres
-- forbids referencing one generated column from another) — matches normalize_booking's
-- `None => deadline_at` fallback for pickup_ms exactly.
ALTER TABLE bookings ADD COLUMN spx_pickup_time TIMESTAMPTZ GENERATED ALWAYS AS (
    COALESCE(
        tower_pick_epoch_ms(raw_data, ARRAY['booking_date', 'schedule_at', 'pickup_time', 'pickup_date']),
        tower_pick_epoch_ms(raw_data, ARRAY['bidding_ddl', 'deadline_at', 'pickup_time_ms', 'expired_at'])
    )
) STORED;

-- Absent -> NULL (distinct from an explicit 0, which is itself a meaningful trip_type value —
-- unlike normalize_booking's pick_num, which defaults absent to 0.0, a persisted/filterable
-- column must not conflate "no data" with "explicitly type 0".
ALTER TABLE bookings ADD COLUMN spx_trip_type INT GENERATED ALWAYS AS (
    NULLIF(raw_data->>'trip_type', '')::int
) STORED;

-- Deliberate simplification (see design doc's Open Questions): only the route_detail_list path
-- is replicated here, not normalize_booking's full sgi_province_name/string-split fallback chain.
-- Postgres 12+ jsonb path operators support negative array indices (-1 = last element).
ALTER TABLE bookings ADD COLUMN spx_origin_station TEXT GENERATED ALWAYS AS (
    NULLIF(raw_data #>> '{route_detail_list,0,node_info_list,0,name}', '')
) STORED;

ALTER TABLE bookings ADD COLUMN spx_dest_station TEXT GENERATED ALWAYS AS (
    NULLIF(raw_data #>> '{route_detail_list,-1,node_info_list,-1,name}', '')
) STORED;

CREATE INDEX idx_bookings_spx_deadline ON bookings (tenant_id, spx_deadline_at);
CREATE INDEX idx_bookings_spx_vehicle_type ON bookings (tenant_id, spx_vehicle_type);
CREATE INDEX idx_bookings_spx_trip_type ON bookings (tenant_id, spx_trip_type);
CREATE INDEX idx_bookings_spx_stations ON bookings (tenant_id, spx_origin_station, spx_dest_station);
