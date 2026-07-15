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
pub fn norm_id(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
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
        let id = if id_trimmed.is_empty() {
            format!("rule_{}", idx + 1)
        } else {
            id_trimmed
        };

        let name_trimmed = raw.name.as_deref().unwrap_or("").trim().to_string();
        let name = if name_trimmed.is_empty() {
            format!("Rule {}", idx + 1)
        } else {
            name_trimmed
        };

        let raw_destinations: Vec<String> = c
            .destinations
            .iter()
            .map(|v| v.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let service_types_canon: Vec<String> = c
            .service_types
            .iter()
            .map(|v| canonical_rule_vehicle_label(v.trim()))
            .filter(|s| !s.is_empty())
            .collect();
        let service_types = uniq_keep_order(&service_types_canon, norm_vehicle);

        let booking_ids_trimmed: Vec<String> = c
            .booking_ids
            .iter()
            .map(|v| v.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
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
        let match_mode = if c.match_mode.as_deref() == Some("flexible") {
            RouteMatchMode::Flexible
        } else {
            RouteMatchMode::Strict
        };

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
            warnings.push(format!(
                "Rule \"{name}\" kosong: mode booking_id tanpa Booking ID"
            ));
        }
        if sanitized.mode == RuleMode::Route && origin.is_empty() && destinations.is_empty() {
            warnings.push(format!(
                "Rule \"{name}\" kosong: mode route tanpa origin/destinasi"
            ));
        }
        if coc_only && non_coc_only_raw {
            sanitized.conditions.non_coc_only = false;
            warnings.push(format!(
                "Rule \"{name}\" bentrok: COC dan Non aktif bersamaan, Non dimatikan"
            ));
        }
        if raw_destinations.len() > 5 {
            warnings.push(format!("Rule \"{name}\" dipotong ke maksimum 5 destinasi"));
        }
        if let Some(raw_name) = &raw.name {
            if raw_name.trim() != name {
                warnings.push(format!(
                    "Rule \"{name}\" dirapikan: nama mengandung spasi berlebih"
                ));
            }
        }

        out.push(sanitized);
    }

    RuleSanitizeResult {
        rules: out,
        warnings,
    }
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

fn dedup_keep_order<F: Fn(&str) -> String>(items: &[String], key: F) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for it in items {
        let k = key(it);
        if k.is_empty() || seen.contains(&k) {
            continue;
        }
        seen.insert(k);
        out.push(it.clone());
    }
    out
}

/// Collapse rules that target the SAME thing (operators re-enter the same lane / booking-id
/// many times). Run on every save so duplicates can never accumulate.
pub fn dedupe_rules(rules: &[AcceptRule]) -> Vec<AcceptRule> {
    // Claim booking-ids ENABLED-first, then input order within a status: a disabled rule
    // entered earlier must not steal an id from an enabled rule entered later (C1 — the id
    // would silently vanish from the active rule on save otherwise).
    let mut claimed_id: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut id_keep: HashMap<usize, Vec<String>> = HashMap::new();
    for &want_enabled in &[true, false] {
        for (idx, r) in rules.iter().enumerate() {
            if r.mode != RuleMode::BookingId || r.enabled != want_enabled {
                continue;
            }
            let mut keep = Vec::new();
            for raw in &r.conditions.booking_ids {
                let n = norm_id(raw);
                if n.is_empty() || claimed_id.contains(&n) {
                    continue;
                }
                claimed_id.insert(n);
                keep.push(raw.clone());
            }
            id_keep.insert(idx, keep);
        }
    }

    let mut out: Vec<AcceptRule> = Vec::new();
    let mut route_at: HashMap<String, usize> = HashMap::new();

    for (idx, r) in rules.iter().enumerate() {
        let c = &r.conditions;

        if r.mode == RuleMode::Route {
            let dests_sig: String = c
                .destinations
                .iter()
                .map(|d| norm_loc(d))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(">");
            let service_types_sig: String = {
                let mut v: Vec<String> = c
                    .service_types
                    .iter()
                    .map(|s| s.to_lowercase().trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                v.sort();
                v.join(",")
            };
            let mode_str = match c.match_mode {
                RouteMatchMode::Flexible => "flexible",
                RouteMatchMode::Strict => "strict",
            };
            let booking_type_str = match c.booking_type {
                RuleBookingType::Spxid => "spxid",
                RuleBookingType::Reguler => "reguler",
                RuleBookingType::All => "all",
            };
            let sig = format!(
                "{}|{}|{}|{}|{}",
                norm_loc(&c.origin),
                dests_sig,
                mode_str,
                booking_type_str,
                service_types_sig
            );

            if let Some(&at) = route_at.get(&sig) {
                // Same lane already present → MERGE, never silently shrink capacity or lose
                // progress: keep the most permissive cap (0 = unlimited wins), the higher
                // accepted_count, enabled if either side is.
                let a = out[at].conditions.max_accept_count;
                let b = c.max_accept_count;
                out[at].conditions.max_accept_count = if a == 0 || b == 0 { 0 } else { a.max(b) };
                out[at].conditions.accepted_count =
                    out[at].conditions.accepted_count.max(c.accepted_count);
                out[at].enabled = out[at].enabled || r.enabled;
                continue;
            }

            route_at.insert(sig, out.len());
            let mut merged = r.clone();
            merged.conditions.destinations = dedup_keep_order(&c.destinations, norm_loc);
            out.push(merged);
            continue;
        }

        if r.mode == RuleMode::BookingId {
            let ids = id_keep.get(&idx).cloned().unwrap_or_default();
            if !ids.is_empty() {
                let mut merged = r.clone();
                merged.conditions.booking_ids = ids;
                out.push(merged);
            }
            continue;
        }

        out.push(r.clone()); // filter / other modes: untouched
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_rule(mode: &str, conditions: RawRuleConditions) -> RawAcceptRule {
        RawAcceptRule {
            id: None,
            name: None,
            enabled: true,
            priority: None,
            mode: Some(mode.to_string()),
            conditions,
        }
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
                    destinations: vec![
                        " Lampung DC ",
                        "Cileungsi DC",
                        "Cileungsi DC",
                        "A",
                        "B",
                        "C",
                    ]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                    service_types: vec!["tronton", "TRONTON (10WH)", " fuso std "]
                        .into_iter()
                        .map(String::from)
                        .collect(),
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
            assert_eq!(
                result.rules[0].conditions.service_types,
                vec!["TRONTON", "FUSO"]
            );
            assert!(result
                .warnings
                .iter()
                .any(|w| w.contains("maksimum 5 destinasi")));
        }

        #[test]
        fn conflicting_coc_flags_are_resolved_safely() {
            let rule = raw_rule(
                "filter",
                RawRuleConditions {
                    coc_only: true,
                    non_coc_only: true,
                    ..Default::default()
                },
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
                RawRuleConditions {
                    booking_ids: vec!["  ".to_string(), String::new()],
                    ..Default::default()
                },
            );
            let result = sanitize_accept_rules(&[rule]);
            assert!(result
                .warnings
                .iter()
                .any(|w| w.contains("tanpa Booking ID")));
        }

        // Money-critical regression guard: `to_optional_non_neg_f64` exists specifically to keep
        // max_weight/max_cod_amount in f64 instead of routing them through the u32-narrowing
        // `to_optional_non_neg` helper (used for min_deadline_min/max_accept_count). If a future
        // refactor accidentally swapped the helper back, a cap above u32::MAX (~4.29 billion)
        // would silently clip/wrap instead of surviving sanitize intact.
        #[test]
        fn max_cod_amount_and_max_weight_survive_sanitize_without_u32_precision_loss() {
            let rule = raw_rule(
                "filter",
                RawRuleConditions {
                    coc_only: true, // keep filter_active satisfied independent of these two fields
                    max_cod_amount: Some(5_000_000_000.0),
                    max_weight: Some(4_500_000_000.0),
                    ..Default::default()
                },
            );
            let result = sanitize_accept_rules(&[rule]);
            assert_eq!(
                result.rules[0].conditions.max_cod_amount,
                Some(5_000_000_000.0)
            );
            assert_eq!(result.rules[0].conditions.max_weight, Some(4_500_000_000.0));
        }
    }

    mod norm_id_visibility_tests {
        // `super::super::norm_id` would work from inside the crate regardless of visibility —
        // this test instead calls it via the CRATE ROOT path a downstream crate (`store`) would
        // use, which only compiles once `norm_id` is `pub` and re-exported from `lib.rs`.
        use crate::norm_id;

        #[test]
        fn norm_id_reachable_from_crate_root() {
            assert_eq!(norm_id("SPXID_VM_001397509"), "spxidvm001397509");
        }
    }

    mod dedupe_rules_tests {
        use super::*;

        fn route_rule(id: &str, origin: &str, dests: &[&str]) -> AcceptRule {
            AcceptRule {
                id: id.to_string(),
                name: id.to_string(),
                enabled: true,
                priority: 0,
                mode: RuleMode::Route,
                conditions: RuleConditions {
                    origin: origin.to_string(),
                    destinations: dests.iter().map(|s| s.to_string()).collect(),
                    ..Default::default()
                },
            }
        }

        fn route_rule_capped(
            id: &str,
            origin: &str,
            dests: &[&str],
            max_accept_count: u32,
            accepted_count: u32,
        ) -> AcceptRule {
            let mut r = route_rule(id, origin, dests);
            r.conditions.max_accept_count = max_accept_count;
            r.conditions.accepted_count = accepted_count;
            r
        }

        fn bkid_rule(id: &str, ids: &[&str], enabled: bool) -> AcceptRule {
            AcceptRule {
                id: id.to_string(),
                name: id.to_string(),
                enabled,
                priority: 0,
                mode: RuleMode::BookingId,
                conditions: RuleConditions {
                    booking_ids: ids.iter().map(|s| s.to_string()).collect(),
                    ..Default::default()
                },
            }
        }

        #[test]
        fn same_lane_entered_3x_collapses_to_1() {
            let out = dedupe_rules(&[
                route_rule("a", "Padang DC", &["Cileungsi DC"]),
                route_rule("b", "Padang DC", &["Cileungsi DC"]),
                route_rule("c", "Padang DC", &["Cileungsi DC"]),
            ]);
            assert_eq!(out.len(), 1);
        }

        #[test]
        fn separator_variant_of_same_lane_still_collapses() {
            let out = dedupe_rules(&[
                route_rule("a", "Padang DC", &["Cileungsi DC"]),
                route_rule("b", "Padang-DC", &["Cileungsi_DC"]),
            ]);
            assert_eq!(out.len(), 1);
        }

        #[test]
        fn different_lanes_are_kept() {
            let out = dedupe_rules(&[
                route_rule("a", "Padang DC", &["Cileungsi DC"]),
                route_rule("b", "Aceh DC", &["Cileungsi DC"]),
            ]);
            assert_eq!(out.len(), 2);
        }

        #[test]
        fn collapse_keeps_most_permissive_cap_and_higher_accepted_count() {
            let out = dedupe_rules(&[
                route_rule_capped("a", "Padang DC", &["Cileungsi DC"], 1, 1),
                route_rule_capped("b", "Padang DC", &["Cileungsi DC"], 5, 0),
            ]);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].conditions.max_accept_count, 5);
            assert_eq!(out[0].conditions.accepted_count, 1);
        }

        #[test]
        fn booking_id_repeated_within_and_across_rules_deduped() {
            let out = dedupe_rules(&[
                bkid_rule("a", &["SPXID_VM_001397509", "SPXID VM 001397509"], true),
                bkid_rule("b", &["SPXID_VM_001397509"], true),
            ]);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].conditions.booking_ids, vec!["SPXID_VM_001397509"]);
        }

        #[test]
        fn disabled_rule_entered_earlier_does_not_steal_id_from_enabled_rule_later_c1() {
            let out = dedupe_rules(&[
                bkid_rule("old", &["SPXID_VM_001402220"], false),
                bkid_rule("new", &["SPXID_VM_001402220"], true),
            ]);
            let ids: Vec<&str> = out.iter().map(|r| r.id.as_str()).collect();
            assert_eq!(ids, vec!["new"]);
            assert_eq!(out[0].conditions.booking_ids, vec!["SPXID_VM_001402220"]);
        }

        #[test]
        fn two_enabled_rules_share_id_earlier_one_wins() {
            let out = dedupe_rules(&[
                bkid_rule("a", &["SPXID_VM_001402220", "SPXID_VM_001402221"], true),
                bkid_rule("b", &["SPXID VM 001402220"], true),
            ]);
            let ids: Vec<&str> = out.iter().map(|r| r.id.as_str()).collect();
            assert_eq!(ids, vec!["a"]);
            assert_eq!(out[0].conditions.booking_ids.len(), 2);
        }

        #[test]
        fn unique_disabled_rule_is_kept_not_a_duplicate_of_anyone() {
            let out = dedupe_rules(&[bkid_rule("solo", &["SPXID_VM_001999999"], false)]);
            let ids: Vec<&str> = out.iter().map(|r| r.id.as_str()).collect();
            assert_eq!(ids, vec!["solo"]);
        }

        #[test]
        fn sanitize_then_dedupe_chain_drops_empty_booking_id_rule() {
            let raw = raw_rule(
                "booking_id",
                RawRuleConditions {
                    booking_ids: vec![],
                    ..Default::default()
                },
            );
            let sanitized = sanitize_accept_rules(&[raw]);
            assert_eq!(dedupe_rules(&sanitized.rules).len(), 0);
        }
    }
}
