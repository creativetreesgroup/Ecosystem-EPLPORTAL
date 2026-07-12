use std::collections::HashMap;

use crate::location::norm_loc;
use crate::vehicle::{canonical_rule_vehicle_label, norm_vehicle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleMode {
    BookingId,
    Route,
    Filter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RouteMatchMode {
    #[default]
    Strict,
    Flexible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuleBookingType {
    #[default]
    All,
    Spxid,
    Reguler,
}

#[derive(Debug, Clone, Default)]
pub struct RuleConditions {
    pub service_types: Vec<String>,
    pub max_weight: Option<f64>,
    pub coc_only: bool,
    pub non_coc_only: bool,
    pub max_cod_amount: Option<f64>,
    pub booking_ids: Vec<String>,
    pub origin: String,
    pub destinations: Vec<String>,
    pub booking_type: RuleBookingType,
    pub shift_types: Vec<i32>,
    pub trip_types: Vec<i32>,
    pub match_mode: RouteMatchMode,
    pub min_deadline_min: Option<u32>,
    /// 0 = unlimited. See this task's brief header for why this is `u32`, not `Option<u32>`.
    pub max_accept_count: u32,
    pub accepted_count: u32,
}

#[derive(Debug, Clone)]
pub struct AcceptRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub priority: i32,
    pub mode: RuleMode,
    pub conditions: RuleConditions,
}

#[derive(Debug, Clone, Default)]
pub struct MatchState {
    pub rule_accept_counts: HashMap<String, u32>,
}

/// Loosely-typed sanitizer input — mirrors the untrusted/partial shape `sanitizeAcceptRules`
/// accepts in TS (fields may be missing/malformed; this is the boundary that cleans them up
/// into a strict `AcceptRule`). Per-array-entry nullability (TS's defensive `v ?? ''` inside
/// `.map()`) is not modeled here: none of the reference sanitize tests exercise a null entry
/// inside an array, so `Vec<String>` (already-strings) is the faithful, simpler equivalent.
#[derive(Debug, Clone, Default)]
pub struct RawAcceptRule {
    pub id: Option<String>,
    pub name: Option<String>,
    pub enabled: bool,
    pub priority: Option<i64>,
    pub mode: Option<String>,
    pub conditions: RawRuleConditions,
}

#[derive(Debug, Clone, Default)]
pub struct RawRuleConditions {
    pub service_types: Vec<String>,
    pub max_weight: Option<f64>,
    pub coc_only: bool,
    pub non_coc_only: bool,
    pub max_cod_amount: Option<f64>,
    pub booking_ids: Vec<String>,
    pub origin: Option<String>,
    pub destinations: Vec<String>,
    pub booking_type: Option<String>,
    pub shift_types: Vec<i64>,
    pub trip_types: Vec<i64>,
    pub match_mode: Option<String>,
    pub min_deadline_min: Option<f64>,
    pub max_accept_count: Option<f64>,
    pub accepted_count: Option<i64>,
}

pub struct RuleSanitizeResult {
    pub rules: Vec<AcceptRule>,
    pub warnings: Vec<String>,
}

/// Separator-insensitive identity key: lowercase, strip everything but `[a-z0-9]`. Used to
/// dedupe booking-ids/lanes and — critically — reused verbatim by `matches_rule`'s booking_id
/// mode and `matched_booking_id_for` so the two can never disagree (see the module-level
/// warning in Task 7/10's brief about the historical production incident this prevents).
pub(crate) fn norm_id(s: &str) -> String {
    s.to_lowercase().chars().filter(char::is_ascii_alphanumeric).collect()
}

fn uniq_keep_order<F: Fn(&str) -> String>(values: &[String], norm: F) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for raw in values {
        let key = norm(raw);
        if key.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        out.push(raw.clone());
    }
    out
}

fn to_int(v: Option<f64>, fallback: i64) -> i64 {
    match v {
        Some(n) if n.is_finite() => n.trunc() as i64,
        _ => fallback,
    }
}

fn to_optional_non_neg(v: Option<f64>) -> Option<u32> {
    match v {
        Some(n) if n.is_finite() => Some(n.max(0.0).trunc() as u32),
        _ => None,
    }
}

/// Same truncate-and-clamp-to-non-negative semantics as `to_optional_non_neg`, but stays in
/// `f64` for `max_weight`/`max_cod_amount` (weight in kg, COD amount in rupiah) instead of
/// narrowing through `u32`. TS's `Math.max(0, Math.trunc(n))` has no 32-bit ceiling — a large
/// COD cap (e.g. > 4.29 billion) must not silently clip when ported, since this is a
/// money-critical field.
fn to_optional_non_neg_f64(v: Option<f64>) -> Option<f64> {
    match v {
        Some(n) if n.is_finite() => Some(n.max(0.0).trunc()),
        _ => None,
    }
}

pub fn sanitize_accept_rules(rules: &[RawAcceptRule]) -> RuleSanitizeResult {
    let mut warnings = Vec::new();
    let mut out = Vec::with_capacity(rules.len());

    for (idx, raw) in rules.iter().enumerate() {
        let c = &raw.conditions;
        let mode = match raw.mode.as_deref() {
            Some("booking_id") => RuleMode::BookingId,
            Some("route") => RuleMode::Route,
            _ => RuleMode::Filter,
        };

        let id_trimmed = raw.id.as_deref().unwrap_or("").trim().to_string();
        let id = if id_trimmed.is_empty() { format!("rule_{}", idx + 1) } else { id_trimmed };

        let name_trimmed = raw.name.as_deref().unwrap_or("").trim().to_string();
        let name = if name_trimmed.is_empty() { format!("Rule {}", idx + 1) } else { name_trimmed };

        let raw_destinations: Vec<String> =
            c.destinations.iter().map(|v| v.trim().to_string()).filter(|s| !s.is_empty()).collect();

        let service_types_canon: Vec<String> = c
            .service_types
            .iter()
            .map(|v| canonical_rule_vehicle_label(v.trim()))
            .filter(|s| !s.is_empty())
            .collect();
        let service_types = uniq_keep_order(&service_types_canon, norm_vehicle);

        let booking_ids_trimmed: Vec<String> =
            c.booking_ids.iter().map(|v| v.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let booking_ids = uniq_keep_order(&booking_ids_trimmed, norm_id);

        let destinations_capped: Vec<String> = raw_destinations.iter().take(5).cloned().collect();
        let destinations = uniq_keep_order(&destinations_capped, norm_loc);

        let shift_types = dedup_nonneg_ints(&c.shift_types);
        let trip_types = dedup_nonneg_ints(&c.trip_types);

        let origin = c.origin.as_deref().unwrap_or("").trim().to_string();
        let coc_only = c.coc_only;
        let non_coc_only_raw = c.non_coc_only;

        let booking_type = match c.booking_type.as_deref() {
            Some("spxid") => RuleBookingType::Spxid,
            Some("reguler") => RuleBookingType::Reguler,
            _ => RuleBookingType::All,
        };
        let match_mode =
            if c.match_mode.as_deref() == Some("flexible") { RouteMatchMode::Flexible } else { RouteMatchMode::Strict };

        let max_weight = to_optional_non_neg_f64(c.max_weight);
        let max_cod_amount = to_optional_non_neg_f64(c.max_cod_amount);
        let min_deadline_min = to_optional_non_neg(c.min_deadline_min);
        let max_accept_count = to_optional_non_neg(c.max_accept_count).unwrap_or(0);
        let accepted_count = to_int(c.accepted_count.map(|x| x as f64), 0).max(0) as u32;
        let priority = to_int(raw.priority.map(|x| x as f64), 0).clamp(-999, 999) as i32;

        let mut sanitized = AcceptRule {
            id,
            name: name.clone(),
            enabled: raw.enabled,
            priority,
            mode,
            conditions: RuleConditions {
                service_types,
                max_weight,
                coc_only,
                non_coc_only: non_coc_only_raw,
                max_cod_amount,
                booking_ids: booking_ids.clone(),
                origin: origin.clone(),
                destinations: destinations.clone(),
                booking_type,
                shift_types,
                trip_types,
                match_mode,
                min_deadline_min,
                max_accept_count,
                accepted_count,
            },
        };

        if sanitized.mode == RuleMode::BookingId && booking_ids.is_empty() {
            warnings.push(format!("Rule \"{name}\" kosong: mode booking_id tanpa Booking ID"));
        }
        if sanitized.mode == RuleMode::Route && origin.is_empty() && destinations.is_empty() {
            warnings.push(format!("Rule \"{name}\" kosong: mode route tanpa origin/destinasi"));
        }
        if coc_only && non_coc_only_raw {
            sanitized.conditions.non_coc_only = false;
            warnings.push(format!("Rule \"{name}\" bentrok: COC dan Non aktif bersamaan, Non dimatikan"));
        }
        if raw_destinations.len() > 5 {
            warnings.push(format!("Rule \"{name}\" dipotong ke maksimum 5 destinasi"));
        }
        if let Some(raw_name) = &raw.name {
            if raw_name.trim() != name {
                warnings.push(format!("Rule \"{name}\" dirapikan: nama mengandung spasi berlebih"));
            }
        }

        out.push(sanitized);
    }

    RuleSanitizeResult { rules: out, warnings }
}

fn dedup_nonneg_ints(values: &[i64]) -> Vec<i32> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for &v in values {
        if v >= 0 && seen.insert(v) {
            out.push(v as i32);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_rule(mode: &str, conditions: RawRuleConditions) -> RawAcceptRule {
        RawAcceptRule { id: None, name: None, enabled: true, priority: None, mode: Some(mode.to_string()), conditions }
    }

    mod sanitize_accept_rules_tests {
        use super::*;

        #[test]
        fn route_rule_trimmed_canonicalized_capped_and_warns_on_overflow() {
            let rule = RawAcceptRule {
                id: Some(String::new()),
                name: Some("  Pekanbaru   ".to_string()),
                enabled: true,
                priority: None,
                mode: Some("route".to_string()),
                conditions: RawRuleConditions {
                    origin: Some("  Pekanbaru DC  ".to_string()),
                    destinations: vec![" Lampung DC ", "Cileungsi DC", "Cileungsi DC", "A", "B", "C"]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                    service_types: vec!["tronton", "TRONTON (10WH)", " fuso std "].into_iter().map(String::from).collect(),
                    ..Default::default()
                },
            };
            let result = sanitize_accept_rules(&[rule]);
            assert_eq!(result.rules[0].id, "rule_1");
            assert_eq!(result.rules[0].name, "Pekanbaru");
            assert_eq!(result.rules[0].conditions.origin, "Pekanbaru DC");
            assert_eq!(
                result.rules[0].conditions.destinations,
                vec!["Lampung DC", "Cileungsi DC", "A", "B"]
            );
            assert_eq!(result.rules[0].conditions.service_types, vec!["TRONTON", "FUSO"]);
            assert!(result.warnings.iter().any(|w| w.contains("maksimum 5 destinasi")));
        }

        #[test]
        fn conflicting_coc_flags_are_resolved_safely() {
            let rule = raw_rule(
                "filter",
                RawRuleConditions { coc_only: true, non_coc_only: true, ..Default::default() },
            );
            let result = sanitize_accept_rules(&[rule]);
            assert!(result.rules[0].conditions.coc_only);
            assert!(!result.rules[0].conditions.non_coc_only);
            assert!(result.warnings.iter().any(|w| w.contains("bentrok")));
        }

        #[test]
        fn booking_id_rule_without_ids_emits_warning() {
            let rule = raw_rule(
                "booking_id",
                RawRuleConditions { booking_ids: vec!["  ".to_string(), String::new()], ..Default::default() },
            );
            let result = sanitize_accept_rules(&[rule]);
            assert!(result.warnings.iter().any(|w| w.contains("tanpa Booking ID")));
        }
    }
}
