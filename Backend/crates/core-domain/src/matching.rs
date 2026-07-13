use crate::booking::{Booking, BookingType};
use crate::location::loc_match_normalized;
use crate::rule::{
    norm_id, AcceptRule, MatchState, RouteMatchMode, RuleBookingType, RuleConditions, RuleMode,
};
use crate::vehicle::{norm_vehicle, vehicle_match_normalized};

/// `[mode_score, priority, dest_count, has_origin, is_strict, service_type_count]`. Derived
/// `Ord` gives exactly the reference `compareRuleRank`'s tuple comparison: first differing
/// element decides, higher wins — mode dominance beats priority beats specificity (CP-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuleRank([i32; 6]);

fn rule_rank(rule: &AcceptRule) -> RuleRank {
    let c = &rule.conditions;
    let mode_score = match rule.mode {
        RuleMode::BookingId => 3,
        RuleMode::Route => 2,
        RuleMode::Filter => 1,
    };
    let is_route = rule.mode == RuleMode::Route;
    let dest_count = if is_route {
        c.destinations
            .iter()
            .map(|d| d.trim())
            .filter(|d| !d.is_empty())
            .count() as i32
    } else {
        0
    };
    let has_origin = i32::from(is_route && !c.origin.trim().is_empty());
    let is_strict = i32::from(is_route && c.match_mode == RouteMatchMode::Strict);
    let service_type_count = c.service_types.len() as i32;
    RuleRank([
        mode_score,
        rule.priority,
        dest_count,
        has_origin,
        is_strict,
        service_type_count,
    ])
}

pub struct CompiledRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub priority: i32,
    pub mode: RuleMode,
    pub conditions: RuleConditions,
    rank: RuleRank,
    // Precomputed once at compile() time, not re-derived per booking. `origin_norm`/
    // `destinations_norm`/`service_types_norm` are read by `matches_route` (Task 8);
    // `service_types_norm` will also be read by `matches_filter` (Task 9). `booking_ids_norm`
    // is read by `matches_booking_id`.
    //
    // `has_origin` is computed from the RAW trimmed `origin` string (matching the TS
    // reference's `if (origin)` gate, where `origin = (c.origin ?? '').trim()`), NOT from
    // whether `origin_norm` happens to be empty. A punctuation-only origin like "---" is
    // non-empty raw but normalizes to "" — the TS reference still treats that as "this rule
    // HAS an origin requirement" (an unsatisfiable one, since `locMatch` needs a non-empty
    // needle), and rejects everything. Gating on `origin_norm.is_empty()` instead would
    // silently skip the origin requirement entirely, letting such a rule match on
    // destinations/filters alone — an over-accept bug.
    has_origin: bool,
    origin_norm: String,
    destinations_norm: Vec<String>,
    service_types_norm: Vec<String>,
    booking_ids_norm: Vec<String>,
}

impl CompiledRule {
    pub fn compile(rule: &AcceptRule) -> Self {
        use crate::location::norm_loc;

        let origin_trimmed = rule.conditions.origin.trim();
        let destinations_norm: Vec<String> = rule
            .conditions
            .destinations
            .iter()
            .map(|d| d.trim())
            .filter(|d| !d.is_empty())
            .map(norm_loc)
            .collect();
        let service_types_norm: Vec<String> = rule
            .conditions
            .service_types
            .iter()
            .map(|s| norm_vehicle(s))
            .collect();
        let booking_ids_norm: Vec<String> = rule
            .conditions
            .booking_ids
            .iter()
            .map(|s| norm_id(s))
            .filter(|s| !s.is_empty())
            .collect();

        CompiledRule {
            id: rule.id.clone(),
            name: rule.name.clone(),
            enabled: rule.enabled,
            priority: rule.priority,
            mode: rule.mode,
            conditions: rule.conditions.clone(),
            rank: rule_rank(rule),
            has_origin: !origin_trimmed.is_empty(),
            origin_norm: norm_loc(origin_trimmed),
            destinations_norm,
            service_types_norm,
            booking_ids_norm,
        }
    }

    pub fn rank(&self) -> RuleRank {
        self.rank
    }

    pub fn matches(&self, booking: &Booking, state: &MatchState) -> bool {
        if !self.enabled {
            return false;
        }
        let c = &self.conditions;

        if c.max_accept_count > 0 {
            let used =
                c.accepted_count + state.rule_accept_counts.get(&self.id).copied().unwrap_or(0);
            if used >= c.max_accept_count {
                return false;
            }
        }

        if self.mode == RuleMode::BookingId {
            return self.matches_booking_id(booking);
        }

        if !c.shift_types.is_empty() && !c.shift_types.contains(&booking.shift_type) {
            return false;
        }
        if !c.trip_types.is_empty() && !c.trip_types.contains(&booking.trip_type) {
            return false;
        }

        match self.mode {
            RuleMode::Route => self.matches_route(booking), // implemented in Task 8
            RuleMode::Filter => self.matches_filter(booking), // implemented in Task 9
            RuleMode::BookingId => unreachable!("handled above"),
        }
    }

    fn matches_booking_id(&self, booking: &Booking) -> bool {
        if self.booking_ids_norm.is_empty() {
            return false;
        }
        let tx = norm_id(&booking.spx_tx_id);
        let bk = norm_id(&booking.booking_id);
        let rq = norm_id(&booking.request_id);
        self.booking_ids_norm.iter().any(|id| {
            tx == *id || bk == *id || rq == *id || (id.len() >= 9 && tx.contains(id.as_str()))
        })
    }

    fn matches_route(&self, booking: &Booking) -> bool {
        let c = &self.conditions;
        let stops: Vec<String> = booking
            .route_stops
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let flexible = c.match_mode == RouteMatchMode::Flexible;

        // SAFETY GUARD: a route rule with no origin, no destinations, AND no other active
        // filter would match EVERY ticket if uncapped — an empty route rule must match NOTHING.
        let has_other_filter = !c.service_types.is_empty()
            || c.max_weight.is_some()
            || c.max_cod_amount.is_some()
            || c.coc_only
            || c.non_coc_only
            || c.booking_type != RuleBookingType::All;
        if !self.has_origin && self.destinations_norm.is_empty() && !has_other_filter {
            return false;
        }

        if self.has_origin {
            // Origin must be the REAL start point: report_station_name first, then the first
            // stop. Region/province labels never satisfy a route rule (Booking doesn't even
            // carry those fields — see Task 1).
            let by_report_station =
                loc_match_normalized(&booking.report_station, &self.origin_norm);
            let by_first_stop = stops
                .first()
                .map(|s| loc_match_normalized(s, &self.origin_norm))
                .unwrap_or(false);
            if !by_report_station && !by_first_stop {
                return false;
            }
        }

        if !self.destinations_norm.is_empty() {
            if stops.is_empty() {
                return false;
            }
            let origin_consumes_first_stop = if self.has_origin {
                stops
                    .first()
                    .map(|s| loc_match_normalized(s, &self.origin_norm))
                    .unwrap_or(false)
            } else {
                false
            };
            let start_idx = usize::from(origin_consumes_first_stop);
            if !self.destinations_match_in_order(&stops, start_idx, flexible) {
                return false;
            }
        }

        if c.booking_type != RuleBookingType::All {
            let want = match c.booking_type {
                RuleBookingType::Spxid => BookingType::Spxid,
                RuleBookingType::Reguler => BookingType::Reguler,
                RuleBookingType::All => unreachable!(),
            };
            if booking.booking_type != want {
                return false;
            }
        }
        if !self.service_types_norm.is_empty() {
            let ticket_norm = norm_vehicle(&booking.vehicle_type);
            if !self
                .service_types_norm
                .iter()
                .any(|r| vehicle_match_normalized(&ticket_norm, r))
            {
                return false;
            }
        }
        if let Some(max_weight) = c.max_weight {
            if booking.weight > max_weight {
                return false;
            }
        }
        if let Some(max_cod) = c.max_cod_amount {
            if booking.cod_amount > max_cod {
                return false;
            }
        }
        if c.coc_only && booking.booking_type != BookingType::Spxid {
            return false;
        }
        if c.non_coc_only && booking.booking_type == BookingType::Spxid {
            return false;
        }
        true
    }

    /// Ordered, whole-word walk through `stops` starting at `start_idx`, consuming
    /// `self.destinations_norm` in order. STRICT: any destination not found → false
    /// immediately. FLEXIBLE: an intermediate (non-last) destination may be absent and is
    /// skipped without advancing the cursor; the LAST destination (the endpoint) must still be
    /// found or the whole match fails.
    fn destinations_match_in_order(
        &self,
        stops: &[String],
        start_idx: usize,
        flexible: bool,
    ) -> bool {
        let mut idx = start_idx;
        let dests = &self.destinations_norm;
        for (d, want_norm) in dests.iter().enumerate() {
            let mut found: Option<usize> = None;
            for (j, stop) in stops.iter().enumerate().skip(idx) {
                if loc_match_normalized(stop, want_norm) {
                    found = Some(j);
                    break;
                }
            }
            match found {
                Some(j) => idx = j + 1,
                None => {
                    if flexible && d != dests.len() - 1 {
                        continue;
                    }
                    return false;
                }
            }
        }
        true
    }

    fn matches_filter(&self, booking: &Booking) -> bool {
        let c = &self.conditions;
        // CP-4: an enabled filter rule with ZERO active conditions must match NOTHING — without
        // this guard every check below is skipped and the function falls through to `true`,
        // turning a blank/misconfigured filter rule into a blanket accept of the entire pool.
        let filter_active = c.max_weight.is_some()
            || c.max_cod_amount.is_some()
            || c.coc_only
            || c.non_coc_only
            || !self.service_types_norm.is_empty();
        if !filter_active {
            return false;
        }
        if let Some(max_weight) = c.max_weight {
            if booking.weight > max_weight {
                return false;
            }
        }
        if let Some(max_cod) = c.max_cod_amount {
            if booking.cod_amount > max_cod {
                return false;
            }
        }
        // Line-haul "COC" means an SPXID ticket, not the COD/cash-on-delivery flag — these are
        // deliberately separate concepts (see coc.rs's module doc).
        if c.coc_only && booking.booking_type != BookingType::Spxid {
            return false;
        }
        if c.non_coc_only && booking.booking_type == BookingType::Spxid {
            return false;
        }
        if !self.service_types_norm.is_empty() {
            let ticket_norm = norm_vehicle(&booking.vehicle_type);
            if !self
                .service_types_norm
                .iter()
                .any(|r| vehicle_match_normalized(&ticket_norm, r))
            {
                return false;
            }
        }
        true
    }
}

/// Convenience wrapper matching the reference `matchesRule(booking, rule, state)` signature
/// exactly, for callers/tests that don't need to reuse a compiled rule across many bookings.
/// The real hot path compiles once via `CompiledRule::compile` and calls `.matches()` directly.
pub fn matches_rule(booking: &Booking, rule: &AcceptRule, state: &MatchState) -> bool {
    CompiledRule::compile(rule).matches(booking, state)
}

/// Compiles every candidate and returns the highest-ranked match, or `None`. Task 10 is where
/// this gets its own dedicated ranking/overlap tests beyond this task's CP-6 smoke tests.
pub fn find_best_matching_rule(
    booking: &Booking,
    rules: &[AcceptRule],
    state: &MatchState,
) -> Option<CompiledRule> {
    let mut best: Option<CompiledRule> = None;
    for rule in rules {
        let compiled = CompiledRule::compile(rule);
        if !compiled.matches(booking, state) {
            continue;
        }
        best = match best {
            None => Some(compiled),
            Some(b) => Some(if compiled.rank() > b.rank() {
                compiled
            } else {
                b
            }),
        };
    }
    best
}

/// Hot-path variant of [`find_best_matching_rule`] over ALREADY-compiled rules:
/// returns the INDEX of the highest-ranked matching rule, or `None`. Tie-break is
/// **first-wins** — the strict `>` means a later rule that only TIES the current
/// best does not replace it, so the first rule to reach the top rank wins (never
/// `Iterator::max_by_key`, which is last-wins on ties and would diverge from the
/// reference on same-rank overlaps). Behaviorally identical to
/// `find_best_matching_rule`; it only differs in taking `&[CompiledRule]` (so the
/// caller reuses one compilation across many bookings) and returning an index.
pub fn find_best_matching_rule_compiled(
    rules: &[CompiledRule],
    booking: &Booking,
    state: &MatchState,
) -> Option<usize> {
    let mut best: Option<(usize, RuleRank)> = None;
    for (i, rule) in rules.iter().enumerate() {
        if !rule.matches(booking, state) {
            continue;
        }
        let rank = rule.rank();
        match best {
            Some((_, best_rank)) if rank > best_rank => best = Some((i, rank)),
            None => best = Some((i, rank)),
            _ => {} // equal or lower rank → keep the earlier (first-wins)
        }
    }
    best.map(|(i, _)| i)
}

/// Returns the registered booking-ID string (original case/spacing) this booking matches, or
/// `None`. MUST use the same normalization (`norm_id`) as `CompiledRule::matches_booking_id` —
/// see this task's brief header for the production incident this invariant prevents: an earlier
/// version normalized differently between the two, so a rule could WIN a match via
/// `matches_rule`/`matches_booking_id` but this function would return `None` for it — the
/// booking-id was never consumed, and the rule stayed armed forever, re-matching an
/// already-won ticket repeatedly.
pub fn matched_booking_id_for(booking: &Booking, rule: &AcceptRule) -> Option<String> {
    let tx = norm_id(&booking.spx_tx_id);
    let bk = norm_id(&booking.booking_id);
    let rq = norm_id(&booking.request_id);
    for raw in &rule.conditions.booking_ids {
        let id = norm_id(raw);
        if id.is_empty() {
            continue;
        }
        if tx == id || bk == id || rq == id || (id.len() >= 9 && tx.contains(id.as_str())) {
            return Some(raw.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{RuleConditions, RuleMode};
    use crate::test_support::{mk_booking, mk_rule, mk_state};

    mod max_accept_cap {
        use super::*;

        #[test]
        fn cap_reached_via_persisted_accepted_count_is_false() {
            let mut conditions = RuleConditions {
                origin: "Padang DC".into(),
                destinations: vec!["Cileungsi DC".into()],
                ..Default::default()
            };
            conditions.max_accept_count = 1;
            conditions.accepted_count = 1;
            let r = mk_rule(RuleMode::Route, conditions);
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn cap_reached_via_in_flight_rule_accept_counts_is_false() {
            let mut conditions = RuleConditions {
                origin: "Padang DC".into(),
                destinations: vec!["Cileungsi DC".into()],
                ..Default::default()
            };
            conditions.max_accept_count = 2;
            conditions.accepted_count = 1;
            let r = mk_rule(RuleMode::Route, conditions);
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            let mut state = mk_state();
            state.rule_accept_counts.insert(r.id.clone(), 1);
            assert!(!CompiledRule::compile(&r).matches(&b, &state));
        }

        // NOTE: this test is ported verbatim from matching.test.ts's "under cap → still matches"
        // (lines 84-88), but that TS test runs against the FULL matchesRule (route mode fully
        // implemented in the same file). Here it exercises a RuleMode::Route rule end-to-end,
        // which falls through to `matches_route`, still `unimplemented!()` in this task per the
        // brief (route mode is Task 8's job). This is a gap in how the plan carved the single TS
        // test suite across tasks 7/8, not a Task 7 defect — ignored until Task 8 lands, then
        // un-ignore (the assertion itself needs no change).
        #[test]
        fn under_cap_still_matches() {
            let mut conditions = RuleConditions {
                origin: "Padang DC".into(),
                destinations: vec!["Cileungsi DC".into()],
                ..Default::default()
            };
            conditions.max_accept_count = 2;
            let r = mk_rule(RuleMode::Route, conditions);
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }
    }

    mod booking_id_mode {
        use super::*;

        #[test]
        fn exact_spx_tx_id_match_is_true() {
            let r = mk_rule(
                RuleMode::BookingId,
                RuleConditions {
                    booking_ids: vec!["SPXID_VM_001396561".into()],
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_001396561".into();
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn short_partial_id_under_9_chars_does_not_substring_match() {
            let r = mk_rule(
                RuleMode::BookingId,
                RuleConditions {
                    booking_ids: vec!["12345".into()],
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_12345_VM".into();
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn full_numeric_id_9_or_more_chars_substring_matches() {
            let r = mk_rule(
                RuleMode::BookingId,
                RuleConditions {
                    booking_ids: vec!["001396561".into()],
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_001396561".into();
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn empty_booking_ids_is_false() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions::default());
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "X".into();
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn separator_tolerant_spaces_in_pasted_id_still_match_underscore_booking_name() {
            let r = mk_rule(
                RuleMode::BookingId,
                RuleConditions {
                    booking_ids: vec!["SPXID VM 001397509".into()],
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_001397509".into();
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn separator_tolerant_stray_underscore_space_still_matches() {
            let r = mk_rule(
                RuleMode::BookingId,
                RuleConditions {
                    booking_ids: vec!["SPXID_ VM_001397492C".into()],
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_001397492C".into();
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }
    }

    mod guards {
        use super::*;

        #[test]
        fn disabled_rule_never_matches() {
            let r = AcceptRule {
                enabled: false,
                ..mk_rule(
                    RuleMode::Route,
                    RuleConditions {
                        origin: "Padang DC".into(),
                        destinations: vec!["Cileungsi DC".into()],
                        ..Default::default()
                    },
                )
            };
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }
    }

    mod cp6_ranking {
        use super::*;

        // NOTE: ported verbatim from matching.test.ts's CP-6 tests (lines 334-345, 347-351), but
        // those run against the FULL matchesRule (route mode implemented in the same TS file).
        // Here `find_best_matching_rule` must call `.matches()` on the route-mode candidate to
        // even consider it, which falls through to `matches_route` — still `unimplemented!()`
        // in this task per the brief (route mode is Task 8's job). Ignored until Task 8 lands;
        // the assertions themselves need no change. See `rule_rank_booking_id_dominates_*` below
        // for a same-property check that doesn't require route matching to work.
        #[test]
        fn exact_booking_id_rule_beats_higher_priority_route_rule_on_same_ticket() {
            let mut b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            b.booking_id = "BKID12345678".into();
            b.spx_tx_id = "BKID12345678".into();
            let bkid = AcceptRule {
                id: "bk".into(),
                priority: 0,
                ..mk_rule(
                    RuleMode::BookingId,
                    RuleConditions {
                        booking_ids: vec!["BKID12345678".into()],
                        ..Default::default()
                    },
                )
            };
            let route = AcceptRule {
                id: "rt".into(),
                priority: 9,
                ..mk_rule(
                    RuleMode::Route,
                    RuleConditions {
                        origin: "Padang DC".into(),
                        destinations: vec!["Cileungsi DC".into()],
                        ..Default::default()
                    },
                )
            };
            let best = find_best_matching_rule(&b, &[route, bkid], &mk_state());
            let best = best.expect("expected a match");
            assert_eq!(best.id, "bk");
            assert_eq!(best.mode, RuleMode::BookingId);
        }

        #[test]
        fn among_two_route_rules_higher_priority_still_wins() {
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            let conditions = || RuleConditions {
                origin: "Padang DC".into(),
                destinations: vec!["Cileungsi DC".into()],
                ..Default::default()
            };
            let lo = AcceptRule {
                id: "lo".into(),
                priority: 1,
                ..mk_rule(RuleMode::Route, conditions())
            };
            let hi = AcceptRule {
                id: "hi".into(),
                priority: 5,
                ..mk_rule(RuleMode::Route, conditions())
            };
            let best =
                find_best_matching_rule(&b, &[lo, hi], &mk_state()).expect("expected a match");
            assert_eq!(best.id, "hi");
        }

        // Direct, always-runnable proof of the CP-6 dominance property that the two ignored
        // tests above can't exercise yet: compares `rule_rank` output directly (bypassing
        // `.matches()`/`matches_route` entirely) to confirm mode_score (array index 0) decides
        // before priority (index 1) is ever consulted — a booking_id rule (mode_score=3, priority
        // 0) must outrank a route rule (mode_score=2, priority 9).
        #[test]
        fn rule_rank_booking_id_dominates_higher_priority_route_directly() {
            let bkid = AcceptRule {
                id: "bk".into(),
                priority: 0,
                ..mk_rule(
                    RuleMode::BookingId,
                    RuleConditions {
                        booking_ids: vec!["BKID12345678".into()],
                        ..Default::default()
                    },
                )
            };
            let route = AcceptRule {
                id: "rt".into(),
                priority: 9,
                ..mk_rule(
                    RuleMode::Route,
                    RuleConditions {
                        origin: "Padang DC".into(),
                        destinations: vec!["Cileungsi DC".into()],
                        ..Default::default()
                    },
                )
            };
            assert!(
                rule_rank(&bkid) > rule_rank(&route),
                "mode_score=3 (booking_id) must outrank mode_score=2 (route) regardless of priority 0 vs 9"
            );
        }

        // Same direct check for the tie-break regression guard: within the SAME mode, priority
        // still decides (indices 0 tie at mode_score=2, index 1 breaks the tie: 5 > 1).
        #[test]
        fn rule_rank_among_two_route_rules_higher_priority_ranks_higher_directly() {
            let conditions = || RuleConditions {
                origin: "Padang DC".into(),
                destinations: vec!["Cileungsi DC".into()],
                ..Default::default()
            };
            let lo = AcceptRule {
                id: "lo".into(),
                priority: 1,
                ..mk_rule(RuleMode::Route, conditions())
            };
            let hi = AcceptRule {
                id: "hi".into(),
                priority: 5,
                ..mk_rule(RuleMode::Route, conditions())
            };
            assert!(rule_rank(&hi) > rule_rank(&lo));
        }
    }

    mod route_mode_tests {
        use super::*;

        fn route(origin: &str, dests: &[&str]) -> RuleConditions {
            RuleConditions {
                origin: origin.into(),
                destinations: dests.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }
        }

        #[test]
        fn origin_and_dest_match_in_order_is_true() {
            let r = mk_rule(RuleMode::Route, route("Padang DC", &["Cileungsi DC"]));
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn bali_origin_does_not_sweep_balikpapan_dc_route() {
            let conditions = RuleConditions {
                origin: "bali".into(),
                destinations: vec![],
                booking_type: RuleBookingType::All,
                service_types: vec!["x".into()],
                ..Default::default()
            };
            let r = mk_rule(RuleMode::Route, conditions);
            let b = mk_booking(&["Balikpapan DC", "Pontianak DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn wrong_destination_is_false() {
            let r = mk_rule(RuleMode::Route, route("Padang DC", &["Cileungsi DC"]));
            let b = mk_booking(&["Padang DC", "Surabaya DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn strict_order_enforced_dest_must_come_after_origin() {
            let r = mk_rule(RuleMode::Route, route("Padang DC", &["Cileungsi DC"]));
            let b = mk_booking(&["Cileungsi DC", "Padang DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn flexible_mode_only_endpoint_must_match_intermediate_hubs_ignored() {
            let mut conditions = route("Pekanbaru DC", &["Cileungsi DC"]);
            conditions.match_mode = RouteMatchMode::Flexible;
            let r = mk_rule(RuleMode::Route, conditions);
            let b = mk_booking(&["Pekanbaru DC", "Palembang DC", "Cileungsi DC"]);
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn empty_route_rule_no_origin_dest_filter_matches_nothing() {
            let r = mk_rule(RuleMode::Route, route("", &[]));
            let b = mk_booking(&["Anywhere DC", "Else DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn origin_can_match_report_station_name_even_when_stops_are_partial() {
            let r = mk_rule(RuleMode::Route, route("Padang DC", &["Cileungsi DC"]));
            let mut b = mk_booking(&["Cileungsi DC"]);
            b.report_station = "Padang DC".into();
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn origin_must_not_fall_back_to_province_region_labels() {
            // report_station is deliberately left empty and originRegion/originProvince (not
            // modeled on Booking at all — see Task 1's design note) are absent — the matcher
            // must reject on the actual stop name alone, proving those TS fields are unread.
            let r = mk_rule(RuleMode::Route, route("Padang DC", &["Cileungsi DC"]));
            let b = mk_booking(&["Bukittinggi DC", "Cileungsi DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn final_destination_must_be_an_actual_route_stop_not_only_dest_region() {
            let r = mk_rule(RuleMode::Route, route("Padang DC", &["Cileungsi DC"]));
            let b = mk_booking(&["Padang DC", "Bekasi DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn punctuation_only_origin_is_unsatisfiable_not_skipped() {
            let r = mk_rule(RuleMode::Route, route("---", &["Cileungsi DC"]));
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }
    }

    mod shift_trip_targeting {
        use super::*;

        #[test]
        fn no_shift_trip_condition_unaffected_matches() {
            let mut b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            b.shift_type = 1;
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Padang DC".into(),
                    destinations: vec!["Cileungsi DC".into()],
                    ..Default::default()
                },
            );
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn shift_types_filter_matches_when_booking_shift_in_list() {
            let mut b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            b.shift_type = 1;
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Padang DC".into(),
                    destinations: vec!["Cileungsi DC".into()],
                    shift_types: vec![1, 2],
                    ..Default::default()
                },
            );
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn shift_types_filter_rejects_when_booking_shift_not_in_list() {
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]); // shift_type defaults to 0
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Padang DC".into(),
                    destinations: vec!["Cileungsi DC".into()],
                    shift_types: vec![2],
                    ..Default::default()
                },
            );
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn trip_types_filter_rejects_when_booking_trip_not_in_list() {
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]); // trip_type defaults to 0
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Padang DC".into(),
                    destinations: vec!["Cileungsi DC".into()],
                    trip_types: vec![1],
                    ..Default::default()
                },
            );
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn shift_and_trip_both_required_both_must_match() {
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Padang DC".into(),
                    destinations: vec!["Cileungsi DC".into()],
                    shift_types: vec![1],
                    trip_types: vec![2],
                    ..Default::default()
                },
            );
            let compiled = CompiledRule::compile(&r);
            let mut ok = mk_booking(&["Padang DC", "Cileungsi DC"]);
            ok.shift_type = 1;
            ok.trip_type = 2;
            assert!(compiled.matches(&ok, &mk_state()));
            let mut bad = mk_booking(&["Padang DC", "Cileungsi DC"]);
            bad.shift_type = 1;
            bad.trip_type = 0;
            assert!(!compiled.matches(&bad, &mk_state()));
        }
    }

    // REAL production lane — booking 5996405, SPXID_VM_001397649: Origin "Aceh DC" → Dest1
    // "Cileungsi DC", strict, TRONTON, COC.
    mod real_lane_aceh_to_cileungsi {
        use super::*;

        fn aceh_rule() -> AcceptRule {
            mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Aceh DC".into(),
                    destinations: vec!["Cileungsi DC".into()],
                    match_mode: RouteMatchMode::Strict,
                    booking_type: RuleBookingType::Spxid,
                    service_types: vec!["TRONTON".into()],
                    coc_only: true,
                    ..Default::default()
                },
            )
        }

        fn real_booking(stops: &[&str]) -> Booking {
            let mut b = mk_booking(stops);
            b.booking_type = BookingType::Spxid;
            b.vehicle_type = "TRONTON (10WH)".into();
            b
        }

        #[test]
        fn the_real_ticket_5996405_matches_the_rule() {
            let compiled = CompiledRule::compile(&aceh_rule());
            assert!(compiled.matches(&real_booking(&["Aceh DC", "Cileungsi DC"]), &mk_state()));
        }

        #[test]
        fn tronton_10wh_vehicle_satisfies_service_type_tronton_suffix_tolerated() {
            let compiled = CompiledRule::compile(&aceh_rule());
            let mut b = real_booking(&["Aceh DC", "Cileungsi DC"]);
            b.vehicle_type = "TRONTON (10WH)".into();
            assert!(compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn different_origin_multi_hop_does_not_match_strict_aceh_rule() {
            let compiled = CompiledRule::compile(&aceh_rule());
            let b = real_booking(&["Tegal 2 DC", "Bekasi DC", "Cileungsi DC"]);
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn a_reguler_non_spxid_aceh_to_cileungsi_ticket_is_rejected_by_coc_only() {
            let compiled = CompiledRule::compile(&aceh_rule());
            let mut b = real_booking(&["Aceh DC", "Cileungsi DC"]);
            b.booking_type = BookingType::Reguler;
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn flexible_variant_catches_any_origin_to_cileungsi_endpoint() {
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    destinations: vec!["Cileungsi DC".into()],
                    match_mode: RouteMatchMode::Flexible,
                    booking_type: RuleBookingType::Spxid,
                    service_types: vec!["TRONTON".into()],
                    coc_only: true,
                    ..Default::default()
                },
            );
            let compiled = CompiledRule::compile(&r);
            let b = real_booking(&["Tegal 2 DC", "Bekasi DC", "Cileungsi DC"]);
            assert!(compiled.matches(&b, &mk_state()));
        }
    }

    // REAL production lane (target): Kosambi DC → Mataram DC → Mataram 2 DC. Rule kcxv1i3omgm
    // from Redis. Real recurring ticket 6091653 / SPXID_VM_001399072, vehicle "TRONTON (10WH)".
    mod real_lane_kosambi_to_mataram {
        use super::*;

        fn kosambi_rule() -> AcceptRule {
            AcceptRule {
                id: "kcxv1i3omgm".into(),
                name: "Route Rule".into(),
                ..mk_rule(
                    RuleMode::Route,
                    RuleConditions {
                        origin: "Kosambi DC".into(),
                        destinations: vec!["Mataram DC".into(), "Mataram 2 DC".into()],
                        match_mode: RouteMatchMode::Strict,
                        booking_type: RuleBookingType::Spxid,
                        service_types: vec!["TRONTON".into()],
                        coc_only: true,
                        max_accept_count: 1,
                        accepted_count: 0,
                        ..Default::default()
                    },
                )
            }
        }

        fn real_booking(stops: &[&str]) -> Booking {
            let mut b = mk_booking(stops);
            b.booking_type = BookingType::Spxid;
            b.vehicle_type = "TRONTON (10WH)".into();
            b.report_station = "Kosambi DC".into();
            b.spx_tx_id = "SPXID_VM_001399072".into();
            b.booking_id = "6091653".into();
            b
        }

        #[test]
        fn the_real_target_ticket_matches() {
            let compiled = CompiledRule::compile(&kosambi_rule());
            assert!(compiled.matches(
                &real_booking(&["Kosambi DC", "Mataram DC", "Mataram 2 DC"]),
                &mk_state()
            ));
        }

        #[test]
        fn find_best_matching_rule_returns_the_route_rule() {
            let b = real_booking(&["Kosambi DC", "Mataram DC", "Mataram 2 DC"]);
            let best = find_best_matching_rule(&b, &[kosambi_rule()], &mk_state())
                .expect("expected a match");
            assert_eq!(best.name, "Route Rule");
        }

        #[test]
        fn tronton_10wh_satisfies_service_type_tronton_capacity_suffix_tolerated() {
            assert!(vehicle_match_normalized(
                &norm_vehicle("TRONTON (10WH)"),
                &norm_vehicle("TRONTON")
            ));
        }

        #[test]
        fn wrong_vehicle_cdd_long_is_false() {
            let compiled = CompiledRule::compile(&kosambi_rule());
            let mut b = real_booking(&["Kosambi DC", "Mataram DC", "Mataram 2 DC"]);
            b.vehicle_type = "CDD LONG (6WH)".into();
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn wrong_booking_type_reguler_not_spxid_is_false() {
            let compiled = CompiledRule::compile(&kosambi_rule());
            let mut b = real_booking(&["Kosambi DC", "Mataram DC", "Mataram 2 DC"]);
            b.booking_type = BookingType::Reguler;
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn route_out_of_order_mataram_2_before_mataram_is_false() {
            let compiled = CompiledRule::compile(&kosambi_rule());
            let b = real_booking(&["Kosambi DC", "Mataram 2 DC", "Mataram DC"]);
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn origin_not_kosambi_is_false() {
            let compiled = CompiledRule::compile(&kosambi_rule());
            let mut b = real_booking(&["Surabaya DC", "Mataram DC", "Mataram 2 DC"]);
            b.report_station = "Surabaya DC".into();
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn destination_mataram_dc_present_but_mataram_2_dc_missing_is_false() {
            let compiled = CompiledRule::compile(&kosambi_rule());
            let b = real_booking(&["Kosambi DC", "Mataram DC"]);
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn whole_word_safety_mataram_dc_must_not_satisfy_mataram_2_dc_leg() {
            let compiled = CompiledRule::compile(&kosambi_rule());
            let b = real_booking(&["Kosambi DC", "Mataram DC", "Denpasar DC"]);
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn reversed_dest_duplicate_rule_does_not_match_forward_route() {
            let reversed = AcceptRule {
                id: "cgkf87q3cpl".into(),
                name: "Route Rule".into(),
                ..mk_rule(
                    RuleMode::Route,
                    RuleConditions {
                        origin: "Kosambi DC".into(),
                        destinations: vec!["Mataram 2 DC".into(), "Mataram DC".into()],
                        match_mode: RouteMatchMode::Strict,
                        booking_type: RuleBookingType::Spxid,
                        service_types: vec!["TRONTON".into()],
                        coc_only: true,
                        ..Default::default()
                    },
                )
            };
            let compiled = CompiledRule::compile(&reversed);
            assert!(!compiled.matches(
                &real_booking(&["Kosambi DC", "Mataram DC", "Mataram 2 DC"]),
                &mk_state()
            ));
        }

        #[test]
        fn cap_reached_rearm_guard_is_false_until_daily_reset() {
            let mut r = kosambi_rule();
            r.conditions.max_accept_count = 1;
            r.conditions.accepted_count = 1;
            let compiled = CompiledRule::compile(&r);
            assert!(!compiled.matches(
                &real_booking(&["Kosambi DC", "Mataram DC", "Mataram 2 DC"]),
                &mk_state()
            ));
        }
    }

    // Regresi F2: flexible = superset strict (ekor hub setelah DC tujuan)
    mod flexible_superset_strict_f2 {
        use super::*;

        #[test]
        fn rute_berekor_hub_setelah_dc_tujuan_tetap_match_flexible() {
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Surabaya DC".into(),
                    destinations: vec!["Denpasar DC".into()],
                    match_mode: RouteMatchMode::Flexible,
                    ..Default::default()
                },
            );
            let b = mk_booking(&["Surabaya DC", "Denpasar DC", "Badung Hub"]);
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn paritas_strict_juga_match_kasus_ekor_hub_yang_sama() {
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Surabaya DC".into(),
                    destinations: vec!["Denpasar DC".into()],
                    match_mode: RouteMatchMode::Strict,
                    ..Default::default()
                },
            );
            let b = mk_booking(&["Surabaya DC", "Denpasar DC", "Badung Hub"]);
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn destinasi_sama_sekali_tidak_ada_di_rute_flexible_tetap_false() {
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Surabaya DC".into(),
                    destinations: vec!["Denpasar DC".into()],
                    match_mode: RouteMatchMode::Flexible,
                    ..Default::default()
                },
            );
            let b = mk_booking(&["Surabaya DC", "Malang DC", "Badung Hub"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn flexible_tanpa_origin_endpoint_di_mana_pun_di_rute_match() {
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    destinations: vec!["Cileungsi DC".into()],
                    match_mode: RouteMatchMode::Flexible,
                    booking_type: RuleBookingType::Spxid,
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&["Tegal 2 DC", "Cileungsi DC", "Bekasi Hub"]);
            b.booking_type = BookingType::Spxid;
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn flexible_destinasi_yang_hanya_muncul_di_posisi_origin_tidak_dihitung() {
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Denpasar DC".into(),
                    destinations: vec!["Denpasar DC".into()],
                    match_mode: RouteMatchMode::Flexible,
                    ..Default::default()
                },
            );
            let b = mk_booking(&["Denpasar DC", "Badung Hub"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn flexible_dengan_rute_kosong_belum_enrich_false() {
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Surabaya DC".into(),
                    destinations: vec!["Denpasar DC".into()],
                    match_mode: RouteMatchMode::Flexible,
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&[]);
            b.report_station = "Surabaya DC".into();
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }
    }

    // Kontrak kendaraan: kosong = terima SEMUA jenis; terisi = wajib cocok
    mod vehicle_empty_means_all {
        use super::*;

        #[test]
        fn service_types_empty_accepts_any_vehicle() {
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Surabaya DC".into(),
                    destinations: vec!["Denpasar DC".into()],
                    service_types: vec![],
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&["Surabaya DC", "Denpasar DC"]);
            b.vehicle_type = "BLINDVAN (4WH)".into();
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn service_types_filled_rejects_unlisted_vehicle() {
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Surabaya DC".into(),
                    destinations: vec!["Denpasar DC".into()],
                    service_types: vec!["TRONTON".into()],
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&["Surabaya DC", "Denpasar DC"]);
            b.vehicle_type = "BLINDVAN (4WH)".into();
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }
    }

    mod filter_mode_coc_semantics {
        use super::*;

        #[test]
        fn coc_only_treats_spxid_as_coc_even_when_cod_flag_is_false() {
            let r = mk_rule(
                RuleMode::Filter,
                RuleConditions {
                    coc_only: true,
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&[]);
            b.booking_type = BookingType::Spxid;
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn coc_only_rejects_reguler_even_when_cod_flag_is_true() {
            let r = mk_rule(
                RuleMode::Filter,
                RuleConditions {
                    coc_only: true,
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&[]);
            b.booking_type = BookingType::Reguler;
            b.cod_amount = 1.0; // stand-in for "COD flag true"; matches_filter must not read this as COC
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn non_coc_only_rejects_spxid_even_when_cod_flag_is_false() {
            let r = mk_rule(
                RuleMode::Filter,
                RuleConditions {
                    non_coc_only: true,
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&[]);
            b.booking_type = BookingType::Spxid;
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }
    }

    mod weight_and_cod_gate {
        use super::*;

        #[test]
        fn max_weight_rejects_heavier_booking_accepts_lighter_or_equal() {
            let r = mk_rule(
                RuleMode::Filter,
                RuleConditions {
                    max_weight: Some(1000.0),
                    ..Default::default()
                },
            );
            let compiled = CompiledRule::compile(&r);
            let mut heavy = mk_booking(&[]);
            heavy.weight = 1000.1;
            assert!(!compiled.matches(&heavy, &mk_state()));
            let mut ok = mk_booking(&[]);
            ok.weight = 1000.0;
            assert!(compiled.matches(&ok, &mk_state()));
        }

        #[test]
        fn max_cod_amount_gate_holds_at_a_value_that_would_overflow_u32() {
            // 5 billion exceeds u32::MAX (~4.29 billion) — if a future refactor accidentally
            // routed max_cod_amount through a u32-narrowing helper again, this value would
            // silently clip and this test would start failing (rejecting a booking it should
            // accept, or vice versa).
            let r = mk_rule(
                RuleMode::Filter,
                RuleConditions {
                    max_cod_amount: Some(5_000_000_000.0),
                    ..Default::default()
                },
            );
            let compiled = CompiledRule::compile(&r);
            let mut under = mk_booking(&[]);
            under.cod_amount = 4_999_999_999.0;
            assert!(compiled.matches(&under, &mk_state()));
            let mut over = mk_booking(&[]);
            over.cod_amount = 5_000_000_001.0;
            assert!(!compiled.matches(&over, &mk_state()));
        }
    }

    mod cp4_empty_filter_safety {
        use super::*;

        #[test]
        fn empty_filter_rule_matches_nothing_no_blanket_accept() {
            let r = mk_rule(RuleMode::Filter, RuleConditions::default());
            let b = mk_booking(&["Anywhere DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn filter_rule_with_active_condition_still_matches_correctly() {
            let r = mk_rule(
                RuleMode::Filter,
                RuleConditions {
                    coc_only: true,
                    ..Default::default()
                },
            );
            let compiled = CompiledRule::compile(&r);
            let mut spxid = mk_booking(&["X"]);
            spxid.booking_type = BookingType::Spxid;
            assert!(compiled.matches(&spxid, &mk_state()));
            let mut reguler = mk_booking(&["X"]);
            reguler.booking_type = BookingType::Reguler;
            assert!(!compiled.matches(&reguler, &mk_state()));
        }
    }

    // Regresi audit F2 lanjutan: flexible multi-destinasi tidak boleh salah-arah
    mod flexible_multi_destination {
        use super::*;

        fn rule() -> AcceptRule {
            mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Jakarta Hub".into(),
                    destinations: vec!["Bandung DC".into(), "Surabaya DC".into()],
                    match_mode: RouteMatchMode::Flexible,
                    ..Default::default()
                },
            )
        }

        #[test]
        fn rute_salah_urutan_endpoint_sebelum_dest_perantara_ditolak() {
            let b = mk_booking(&["Jakarta Hub", "Surabaya DC", "Bandung DC"]);
            assert!(!CompiledRule::compile(&rule()).matches(&b, &mk_state()));
        }

        #[test]
        fn dest_perantara_absen_tapi_endpoint_hadir_match() {
            let b = mk_booking(&["Jakarta Hub", "Cirebon DC", "Surabaya DC"]);
            assert!(CompiledRule::compile(&rule()).matches(&b, &mk_state()));
        }

        #[test]
        fn urutan_lengkap_plus_ekor_hub_match() {
            let b = mk_booking(&["Jakarta Hub", "Bandung DC", "Surabaya DC", "Sidoarjo Hub"]);
            assert!(CompiledRule::compile(&rule()).matches(&b, &mk_state()));
        }

        #[test]
        fn endpoint_absen_sama_sekali_ditolak_walau_dest_perantara_hadir() {
            let b = mk_booking(&["Jakarta Hub", "Bandung DC", "Semarang DC"]);
            assert!(!CompiledRule::compile(&rule()).matches(&b, &mk_state()));
        }
    }

    mod overlapping_rules_tests {
        use super::*;

        #[test]
        fn prefers_more_specific_multi_destination_route_over_generic_endpoint_rule() {
            let mut b = mk_booking(&["Pekanbaru DC", "Palembang DC", "Cileungsi DC"]);
            b.booking_type = BookingType::Spxid;
            b.vehicle_type = "TRONTON (10WH)".into();

            let base = || RuleConditions {
                origin: "Pekanbaru DC".into(),
                booking_type: RuleBookingType::Spxid,
                service_types: vec!["TRONTON".into()],
                coc_only: true,
                match_mode: RouteMatchMode::Strict,
                ..Default::default()
            };
            let generic = AcceptRule {
                id: "generic".into(),
                name: "generic".into(),
                ..mk_rule(
                    RuleMode::Route,
                    RuleConditions {
                        destinations: vec!["Cileungsi DC".into()],
                        ..base()
                    },
                )
            };
            let specific = AcceptRule {
                id: "specific".into(),
                name: "specific".into(),
                ..mk_rule(
                    RuleMode::Route,
                    RuleConditions {
                        destinations: vec!["Palembang DC".into(), "Cileungsi DC".into()],
                        ..base()
                    },
                )
            };

            let best = find_best_matching_rule(&b, &[generic, specific], &mk_state())
                .expect("expected a match");
            assert_eq!(best.id, "specific");
        }

        #[test]
        fn booking_id_target_beats_a_broad_route_rule_for_the_same_ticket() {
            let mut b = mk_booking(&["Aceh DC", "Cileungsi DC"]);
            b.spx_tx_id = "SPXID_VM_001397649".into();
            b.booking_id = "5996405".into();
            b.booking_type = BookingType::Spxid;
            b.vehicle_type = "TRONTON (10WH)".into();

            let route = AcceptRule {
                id: "route".into(),
                ..mk_rule(
                    RuleMode::Route,
                    RuleConditions {
                        origin: "Aceh DC".into(),
                        destinations: vec!["Cileungsi DC".into()],
                        booking_type: RuleBookingType::Spxid,
                        service_types: vec!["TRONTON".into()],
                        coc_only: true,
                        ..Default::default()
                    },
                )
            };
            let target = AcceptRule {
                id: "target".into(),
                ..mk_rule(
                    RuleMode::BookingId,
                    RuleConditions {
                        booking_ids: vec!["SPXID_VM_001397649".into()],
                        ..Default::default()
                    },
                )
            };

            let best = find_best_matching_rule(&b, &[route, target], &mk_state())
                .expect("expected a match");
            assert_eq!(best.id, "target");
        }
    }

    mod matched_booking_id_for_tests {
        use super::*;

        #[test]
        fn separator_beda_spasi_vs_underscore_tetap_mengembalikan_raw_id() {
            let r = mk_rule(
                RuleMode::BookingId,
                RuleConditions {
                    booking_ids: vec!["SPXID VM 001402220".into()],
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_001402220".into();
            assert_eq!(
                matched_booking_id_for(&b, &r),
                Some("SPXID VM 001402220".to_string())
            );
        }

        #[test]
        fn match_via_booking_id_bukan_spx_tx_id() {
            let r = mk_rule(
                RuleMode::BookingId,
                RuleConditions {
                    booking_ids: vec!["6254861".into()],
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&[]);
            b.booking_id = "6254861".into();
            assert_eq!(matched_booking_id_for(&b, &r), Some("6254861".to_string()));
        }

        #[test]
        fn match_via_request_id() {
            let r = mk_rule(
                RuleMode::BookingId,
                RuleConditions {
                    booking_ids: vec!["REQ-000123".into()],
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&[]);
            b.request_id = "REQ_000123".into();
            assert_eq!(
                matched_booking_id_for(&b, &r),
                Some("REQ-000123".to_string())
            );
        }

        #[test]
        fn substring_hanya_untuk_id_panjang_9_plus() {
            let short = mk_rule(
                RuleMode::BookingId,
                RuleConditions {
                    booking_ids: vec!["12345".into()],
                    ..Default::default()
                },
            );
            let mut b_short = mk_booking(&[]);
            b_short.spx_tx_id = "SPXID_12345_VM".into();
            assert_eq!(matched_booking_id_for(&b_short, &short), None);

            let long = mk_rule(
                RuleMode::BookingId,
                RuleConditions {
                    booking_ids: vec!["001402220".into()],
                    ..Default::default()
                },
            );
            let mut b_long = mk_booking(&[]);
            b_long.spx_tx_id = "SPXID_VM_001402220".into();
            assert_eq!(
                matched_booking_id_for(&b_long, &long),
                Some("001402220".to_string())
            );
        }

        #[test]
        fn tidak_cocok_null_daftar_kosong_null() {
            let r = mk_rule(
                RuleMode::BookingId,
                RuleConditions {
                    booking_ids: vec!["SPXID_VM_001402220".into()],
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_9".into();
            assert_eq!(matched_booking_id_for(&b, &r), None);

            let empty_rule = mk_rule(RuleMode::BookingId, RuleConditions::default());
            let mut b2 = mk_booking(&[]);
            b2.spx_tx_id = "X".into();
            assert_eq!(matched_booking_id_for(&b2, &empty_rule), None);
        }

        #[test]
        fn paritas_dengan_matches_rule_pada_kasus_separator_kontrak_anti_drift() {
            let r = mk_rule(
                RuleMode::BookingId,
                RuleConditions {
                    booking_ids: vec!["SPXID_ VM_001397492C".into()],
                    ..Default::default()
                },
            );
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_001397492C".into();
            assert!(matches_rule(&b, &r, &mk_state()));
            assert_eq!(
                matched_booking_id_for(&b, &r),
                Some("SPXID_ VM_001397492C".to_string())
            );
        }
    }

    mod precompute_at_save_tests {
        use super::*;

        #[test]
        fn compile_once_then_matches_many_times_against_different_bookings() {
            // Demonstrates the master spec's "compile at save, not per ticket" requirement:
            // origin/destinations are normalized exactly once here, then `matches` is called
            // against several distinct bookings without re-deriving `origin_norm`/
            // `destinations_norm` — inspect `CompiledRule::compile` (Task 7) to confirm the
            // normalization happens inside `compile`, not inside `matches`/`matches_route`.
            let rule = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Padang DC".into(),
                    destinations: vec!["Cileungsi DC".into()],
                    ..Default::default()
                },
            );
            let compiled = CompiledRule::compile(&rule);

            assert!(compiled.matches(&mk_booking(&["Padang DC", "Cileungsi DC"]), &mk_state()));
            assert!(!compiled.matches(&mk_booking(&["Padang DC", "Surabaya DC"]), &mk_state()));
            assert!(compiled.matches(
                &mk_booking(&["Padang DC", "Jakarta Hub", "Cileungsi DC"]),
                &mk_state()
            ));
            assert!(!compiled.matches(&mk_booking(&["Bandung DC", "Cileungsi DC"]), &mk_state()));

            // Precomputed fields are stable across all four calls above — confirm directly.
            assert_eq!(compiled.origin_norm, "padang dc");
            assert_eq!(compiled.destinations_norm, vec!["cileungsi dc".to_string()]);
        }
    }

    mod compiled_variant_tests {
        use super::*; // brings in CompiledRule, AcceptRule, Booking, BookingType,
                      // RuleMode, RuleConditions, RouteMatchMode, mk_* helpers, etc.

        // First-wins: two filter rules with IDENTICAL rank both match the same
        // booking; the FIRST index must win (not the last).
        #[test]
        fn first_wins_on_equal_rank() {
            let first = AcceptRule {
                id: "first".into(),
                ..mk_rule(
                    RuleMode::Filter,
                    RuleConditions {
                        coc_only: true,
                        ..Default::default()
                    },
                )
            };
            let second = AcceptRule {
                id: "second".into(),
                ..mk_rule(
                    RuleMode::Filter,
                    RuleConditions {
                        coc_only: true,
                        ..Default::default()
                    },
                )
            };
            let compiled = vec![
                CompiledRule::compile(&first),
                CompiledRule::compile(&second),
            ];
            let mut b = mk_booking(&[]);
            b.booking_type = BookingType::Spxid;

            // Both match with equal rank → index 0 (the first) must win.
            let idx = find_best_matching_rule_compiled(&compiled, &b, &mk_state());
            assert_eq!(idx, Some(0), "equal-rank tie must resolve to the FIRST rule");
            assert_eq!(compiled[idx.unwrap()].id, "first");
        }

        // Cross-check: on a shared corpus, the compiled-index variant agrees with
        // the existing `find_best_matching_rule` (same winning rule id, or both
        // None) — proving the hot-path variant is not a divergent reimplementation.
        #[test]
        fn agrees_with_find_best_matching_rule_on_corpus() {
            let rules = vec![
                AcceptRule {
                    id: "route-generic".into(),
                    priority: 1,
                    ..mk_rule(
                        RuleMode::Route,
                        RuleConditions {
                            destinations: vec!["Cileungsi DC".into()],
                            match_mode: RouteMatchMode::Flexible,
                            ..Default::default()
                        },
                    )
                },
                AcceptRule {
                    id: "route-specific".into(),
                    priority: 1,
                    ..mk_rule(
                        RuleMode::Route,
                        RuleConditions {
                            origin: "Padang DC".into(),
                            destinations: vec!["Cileungsi DC".into()],
                            ..Default::default()
                        },
                    )
                },
                AcceptRule {
                    id: "bkid".into(),
                    ..mk_rule(
                        RuleMode::BookingId,
                        RuleConditions {
                            booking_ids: vec!["SPXID_VM_001397649".into()],
                            ..Default::default()
                        },
                    )
                },
                AcceptRule {
                    id: "filter-coc".into(),
                    ..mk_rule(
                        RuleMode::Filter,
                        RuleConditions {
                            coc_only: true,
                            ..Default::default()
                        },
                    )
                },
            ];
            let compiled: Vec<CompiledRule> =
                rules.iter().map(CompiledRule::compile).collect();

            // A small corpus of bookings hitting different modes / no-match.
            let mut corpus: Vec<Booking> = Vec::new();
            corpus.push(mk_booking(&["Padang DC", "Cileungsi DC"])); // route
            let mut spx = mk_booking(&["Aceh DC", "Cileungsi DC"]);
            spx.spx_tx_id = "SPXID_VM_001397649".into();
            spx.booking_type = BookingType::Spxid;
            corpus.push(spx); // booking-id target should dominate
            let mut coc = mk_booking(&[]);
            coc.booking_type = BookingType::Spxid;
            corpus.push(coc); // filter-coc
            corpus.push(mk_booking(&["Nowhere DC", "Elsewhere DC"])); // likely no match

            for booking in &corpus {
                let via_owned = find_best_matching_rule(booking, &rules, &mk_state());
                let via_index = find_best_matching_rule_compiled(&compiled, booking, &mk_state());
                match (via_owned, via_index) {
                    (Some(owned), Some(i)) => assert_eq!(
                        owned.id, compiled[i].id,
                        "both variants must pick the same rule"
                    ),
                    (None, None) => {}
                    (owned_opt, idx_opt) => panic!(
                        "variants disagree: owned={:?} index={:?}",
                        owned_opt.map(|r| r.id),
                        idx_opt
                    ),
                }
            }
        }
    }
}
