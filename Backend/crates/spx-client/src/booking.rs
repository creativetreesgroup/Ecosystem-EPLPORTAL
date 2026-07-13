// Backend/crates/spx-client/src/booking.rs
//! Full SPX booking shape (mirror of the reference `SpxBooking`, spx.ts:7-37),
//! normalize_booking (spx.ts:116-195), and a mapping down to Fase 1's
//! core_domain::Booking (which is NOT modified).
use chrono::{DateTime, FixedOffset, Utc};
use core_domain::{booking_type_of, parse_route_detail_list, parse_route_stops, BookingType};
use serde_json::Value;

/// 29-field SPX booking. `booking_type` reuses `core_domain::BookingType`; the
/// timestamp fields are epoch-ms (`deadline_at`/`created_at`) or preformatted
/// strings (`pickup_time` = ISO-8601 UTC, `pickup_time_str` = WIB HH:MM). Not
/// serde-derived: `core_domain::BookingType` is not `Serialize` and Fase 1 must
/// not be modified — the API-serialization layer (Fase 6) handles conversion.
#[derive(Debug, Clone)]
pub struct SpxBooking {
    pub id: String,
    pub booking_id: String,
    pub request_id: String,
    pub onsite_id: Option<String>,
    pub spx_tx_id: String,
    pub booking_type: BookingType,
    pub status: String,
    pub vehicle_type: String,
    pub vehicle_capacity: String,
    pub weight: f64,
    pub cod: bool,
    pub cod_amount: f64,
    pub coc_count: i64,
    pub shift_type: i32,
    pub trip_type: i32,
    pub tier_per_round: i64,
    pub time_per_round: i64,
    pub award_logic: i64,
    pub route_stops: Vec<String>,
    pub report_station: String,
    pub origin_province: String,
    pub origin_region: String,
    pub destination_province: String,
    pub destination_region: String,
    pub pickup_time: String,
    pub pickup_time_str: String,
    pub deadline_at: i64,
    pub created_at: i64,
    pub raw: Value,
}

// ── Coercion helpers (mirror the reference's pick/Number/String/toMs) ──────────

/// `pick(obj, ...keys)`: first key present, non-null, non-empty-string.
fn pick<'a>(raw: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let obj = raw.as_object()?;
    for k in keys {
        if let Some(v) = obj.get(*k) {
            let empty = v.is_null() || v.as_str() == Some("");
            if !empty {
                return Some(v);
            }
        }
    }
    None
}

/// `String(pick(...) ?? '')`.
fn pick_str(raw: &Value, keys: &[&str]) -> String {
    match pick(raw, keys) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

/// `Number(pick(...) ?? 0)` — JSON numbers and numeric strings coerce; else 0.
fn to_num(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse::<f64>().unwrap_or(0.0),
        Value::Bool(true) => 1.0,
        Value::Bool(false) => 0.0,
        _ => 0.0,
    }
}

fn pick_num(raw: &Value, keys: &[&str]) -> f64 {
    pick(raw, keys).map(to_num).unwrap_or(0.0)
}

/// `Boolean(pick(...) ?? false)` — JS truthiness of the picked value.
fn pick_bool(raw: &Value, keys: &[&str]) -> bool {
    match pick(raw, keys) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
        None => false,
    }
}

/// `toMs(v)`: 0 -> 0; > 1e12 already ms; else seconds*1000.
fn to_ms(v: f64) -> i64 {
    if v == 0.0 {
        0
    } else if v > 1e12 {
        v as i64
    } else {
        (v * 1000.0) as i64
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// ISO-8601 UTC + WIB (UTC+7 fixed offset, no DST) HH:MM.
fn format_times(ms: i64) -> (String, String) {
    let dt: DateTime<Utc> = DateTime::<Utc>::from_timestamp_millis(ms).unwrap_or_else(Utc::now);
    let iso = dt.to_rfc3339();
    let wib = FixedOffset::east_opt(7 * 3600).expect("valid +7 offset");
    let hhmm = dt.with_timezone(&wib).format("%H:%M").to_string();
    (iso, hhmm)
}

/// `x ?? y ?? z` (nullish) as a String: null/missing skip, a present empty
/// string is kept (distinct from `pick`, which also skips empty strings). Used
/// for the province chain, which the reference resolves with `??`, not `pick`.
fn nullish_str(raw: &Value, keys: &[&str]) -> Option<String> {
    let obj = raw.as_object()?;
    for k in keys {
        match obj.get(*k) {
            None | Some(Value::Null) => continue,
            Some(Value::String(s)) => return Some(s.clone()),
            Some(Value::Number(n)) => return Some(n.to_string()),
            Some(Value::Bool(b)) => return Some(b.to_string()),
            Some(v) => return Some(v.to_string()),
        }
    }
    None
}

struct Provinces {
    origin: String,
    dest: String,
    origin_region: String,
    dest_region: String,
}

/// Port of parseProvinces (spx.ts:92-114).
fn parse_provinces(raw: &Value) -> Provinces {
    let nodes = parse_route_detail_list(raw);
    if nodes.len() >= 2 {
        let first = &nodes[0];
        let last = &nodes[nodes.len() - 1];
        return Provinces {
            origin: first.province.clone(),
            dest: last.province.clone(),
            origin_region: first.name.clone(),
            dest_region: last.name.clone(),
        };
    }
    let province_full = nullish_str(raw, &["sgi_province_name", "province_name"]).unwrap_or_default();
    let parts: Vec<&str> = province_full.split(" -> ").collect();
    let first_part = parts.first().copied().unwrap_or("").to_string();
    let last_part = parts.last().copied().unwrap_or("").to_string();
    Provinces {
        origin: nullish_str(raw, &["origin_province", "pickup_province"]).unwrap_or(first_part),
        dest: nullish_str(raw, &["dest_province", "delivery_province"]).unwrap_or(last_part),
        origin_region: nullish_str(raw, &["origin_dc_name", "origin_hub", "report_station_name"])
            .unwrap_or_default(),
        dest_region: nullish_str(raw, &["dest_dc_name", "dest_hub"]).unwrap_or_default(),
    }
}

/// Port of normalizeBooking (spx.ts:116-195).
pub fn normalize_booking(raw: &Value) -> SpxBooking {
    let booking_id = pick_str(raw, &["booking_id", "bookingId", "booking_sn", "id"]);
    let request_id = pick_str(raw, &["request_id", "requestId", "req_id"]);
    let spx_tx_id = {
        let v = pick_str(raw, &["booking_name", "spx_tx_id", "spxTxId", "tx_id", "tracking_no"]);
        if v.is_empty() {
            booking_id.clone()
        } else {
            v
        }
    };

    let route_stops = parse_route_stops(raw);
    let provinces = parse_provinces(raw);

    let deadline_at = match pick(raw, &["bidding_ddl", "deadline_at", "pickup_time_ms", "expired_at"]) {
        Some(v) => to_ms(to_num(v)),
        None => now_ms() + 3_600_000,
    };
    let pickup_ms = match pick(raw, &["booking_date", "schedule_at", "pickup_time", "pickup_date"]) {
        Some(v) => to_ms(to_num(v)),
        None => deadline_at,
    };
    let (pickup_time, pickup_time_str) = format_times(pickup_ms);
    let created_at = match pick(raw, &["ctime", "created_at", "create_time", "createdAt"]) {
        Some(v) => to_ms(to_num(v)),
        None => now_ms(),
    };

    // Vehicle type: prefer display name; a BARE-NUMERIC code is discarded (M5).
    let vtype_name = pick_str(raw, &["vehicle_type_name", "right_vehicle_type_name", "sgi_vehicle_name"]);
    let vtype_code = pick_str(raw, &["truck_type", "vehicle_type", "vehicleType", "service_type"]);
    let vtype_code_clean = {
        let t = vtype_code.trim();
        if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) {
            String::new()
        } else {
            vtype_code.clone()
        }
    };
    let vehicle_type = if !vtype_name.is_empty() {
        vtype_name
    } else {
        vtype_code_clean
    };
    let vehicle_capacity = pick_str(raw, &["truck_capacity", "vehicle_capacity", "vehicleCapacity"]);

    // Status: numeric 1/2/3 -> pending/accepted/failed, else stringify (default pending).
    let status = {
        let v = pick(raw, &["request_acceptance_status", "status", "booking_status"]);
        let code = v.and_then(|v| match v {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.trim().parse::<i64>().ok(),
            _ => None,
        });
        match code {
            Some(1) => "pending".to_string(),
            Some(2) => "accepted".to_string(),
            Some(3) => "failed".to_string(),
            _ => match v {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => "pending".to_string(),
            },
        }
    };

    // COC/SPXID type from the REAL transaction name (booking_name), NOT the
    // bookingId fallback (M4). Absent real name -> reguler.
    let tx_name_for_type =
        pick_str(raw, &["booking_name", "spx_tx_id", "spxTxId", "tx_id", "tracking_no"]);
    let booking_type = booking_type_of(&tx_name_for_type);

    let onsite_raw = pick_str(raw, &["onsite_id", "onsiteId"]);
    let onsite_id = if onsite_raw.is_empty() {
        None
    } else {
        Some(onsite_raw)
    };

    let id = {
        if !booking_id.is_empty() {
            booking_id.clone()
        } else if !request_id.is_empty() {
            request_id.clone()
        } else {
            spx_tx_id.clone()
        }
    };

    SpxBooking {
        id,
        booking_id,
        request_id,
        onsite_id,
        spx_tx_id,
        booking_type,
        status,
        vehicle_type,
        vehicle_capacity,
        weight: pick_num(raw, &["weight", "total_weight", "item_weight"]),
        cod: pick_bool(raw, &["is_coc", "is_cod", "cod", "has_coc"]),
        cod_amount: pick_num(raw, &["cod_amount", "coc_amount", "codAmount"]),
        coc_count: pick_num(raw, &["coc_count", "cod_count"]) as i64,
        shift_type: pick_num(raw, &["shift_type"]) as i32,
        trip_type: pick_num(raw, &["trip_type"]) as i32,
        tier_per_round: pick_num(raw, &["tier_per_round"]) as i64,
        time_per_round: pick_num(raw, &["time_per_round"]) as i64,
        award_logic: pick_num(raw, &["award_logic"]) as i64,
        route_stops,
        report_station: pick_str(raw, &["report_station_name"]),
        origin_province: provinces.origin,
        origin_region: provinces.origin_region,
        destination_province: provinces.dest,
        destination_region: provinces.dest_region,
        pickup_time,
        pickup_time_str,
        deadline_at,
        created_at,
        raw: raw.clone(),
    }
}

/// Map the full SpxBooking down to Fase 1's core_domain::Booking (11 fields).
/// core_domain::Booking is NOT modified; this only consumes it.
pub fn to_core_booking(b: &SpxBooking) -> core_domain::Booking {
    core_domain::Booking {
        route_stops: b.route_stops.clone(),
        report_station: b.report_station.clone(),
        spx_tx_id: b.spx_tx_id.clone(),
        booking_id: b.booking_id.clone(),
        request_id: b.request_id.clone(),
        booking_type: b.booking_type,
        vehicle_type: b.vehicle_type.clone(),
        weight: b.weight,
        cod_amount: b.cod_amount,
        shift_type: b.shift_type,
        trip_type: b.trip_type,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // NOTE (DoD #9): every fixture below is SYNTHESIZED from the documented
    // multi-key field names in normalizeBooking (spx.ts:116-195). There are NO
    // recorded real SPX booking bodies anywhere in the reference repo — these are
    // NOT captured payloads, and are not represented as such.

    #[test]
    fn multi_key_fallback_priority() {
        // booking_id empty -> skip -> bookingId wins; id is never reached.
        let raw = json!({ "booking_id": "", "bookingId": "B123", "id": "IGNORED" });
        let b = normalize_booking(&raw);
        assert_eq!(b.booking_id, "B123");
    }

    #[test]
    fn numeric_only_vehicle_type_is_discarded() {
        // A bare numeric code ("3") is an internal id, not a vehicle TYPE -> "".
        let raw = json!({ "vehicle_type": "3" });
        assert_eq!(normalize_booking(&raw).vehicle_type, "");
        // A real name is kept.
        let raw2 = json!({ "vehicle_type": "CDD" });
        assert_eq!(normalize_booking(&raw2).vehicle_type, "CDD");
        // Display name beats a numeric code.
        let raw3 = json!({ "vehicle_type_name": "CDD LONG (6WH)", "vehicle_type": "3" });
        assert_eq!(normalize_booking(&raw3).vehicle_type, "CDD LONG (6WH)");
    }

    #[test]
    fn status_code_mapping() {
        assert_eq!(normalize_booking(&json!({ "status": 1 })).status, "pending");
        assert_eq!(normalize_booking(&json!({ "status": "2" })).status, "accepted");
        assert_eq!(normalize_booking(&json!({ "request_acceptance_status": 3 })).status, "failed");
        assert_eq!(normalize_booking(&json!({ "status": "weird" })).status, "weird");
        assert_eq!(normalize_booking(&json!({})).status, "pending");
    }

    #[test]
    fn booking_type_from_booking_name_not_booking_id_fallback() {
        // A real booking_name of SPXID... classifies as spxid, even when the
        // numeric booking_id would look "reguler".
        let coc = json!({ "booking_id": "884412771", "booking_name": "SPXID99887766" });
        assert_eq!(normalize_booking(&coc).booking_type, BookingType::Spxid);
        // The M4 guarantee: with NO real booking_name and a non-SPXID booking_id,
        // the type must be reguler (it must NOT be inferred from anything but the
        // real transaction name; an absent name cannot prove SPXID).
        let reg = json!({ "booking_id": "884412771" });
        assert_eq!(normalize_booking(&reg).booking_type, BookingType::Reguler);
    }

    #[test]
    fn to_core_booking_maps_all_11_fields() {
        let raw = json!({
            "booking_id": "B1", "request_id": "R1", "booking_name": "SPXID1",
            "vehicle_type_name": "CDD", "weight": 12.5, "cod_amount": 300000,
            "shift_type": 1, "trip_type": 2, "report_station_name": "Padang DC",
            "route_stops": ["Padang DC", "Cileungsi DC"]
        });
        let b = normalize_booking(&raw);
        let core = to_core_booking(&b);
        assert_eq!(core.booking_id, "B1");
        assert_eq!(core.request_id, "R1");
        assert_eq!(core.spx_tx_id, "SPXID1");
        assert_eq!(core.booking_type, BookingType::Spxid);
        assert_eq!(core.vehicle_type, "CDD");
        assert_eq!(core.weight, 12.5);
        assert_eq!(core.cod_amount, 300000.0);
        assert_eq!(core.shift_type, 1);
        assert_eq!(core.trip_type, 2);
        assert_eq!(core.report_station, "Padang DC");
        assert_eq!(core.route_stops, vec!["Padang DC", "Cileungsi DC"]);
    }
}
