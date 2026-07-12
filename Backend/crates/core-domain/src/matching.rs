use crate::booking::Booking;
use crate::rule::{norm_id, AcceptRule, MatchState, RuleConditions, RuleMode, RouteMatchMode};
use crate::vehicle::norm_vehicle;

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
        c.destinations.iter().map(|d| d.trim()).filter(|d| !d.is_empty()).count() as i32
    } else {
        0
    };
    let has_origin = i32::from(is_route && !c.origin.trim().is_empty());
    let is_strict = i32::from(is_route && c.match_mode == RouteMatchMode::Strict);
    let service_type_count = c.service_types.len() as i32;
    RuleRank([mode_score, rule.priority, dest_count, has_origin, is_strict, service_type_count])
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
    // `destinations_norm`/`service_types_norm` are not yet read by any code in THIS task —
    // they're consumed by `matches_route`/`matches_filter`, which Tasks 8/9 fill in — hence the
    // explicit `allow` instead of leaving them unread by accident. `booking_ids_norm` IS already
    // read, by this task's own `matches_booking_id`.
    #[allow(dead_code)]
    origin_norm: String,
    #[allow(dead_code)]
    destinations_norm: Vec<String>,
    #[allow(dead_code)]
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
        let service_types_norm: Vec<String> = rule.conditions.service_types.iter().map(|s| norm_vehicle(s)).collect();
        let booking_ids_norm: Vec<String> =
            rule.conditions.booking_ids.iter().map(|s| norm_id(s)).filter(|s| !s.is_empty()).collect();

        CompiledRule {
            id: rule.id.clone(),
            name: rule.name.clone(),
            enabled: rule.enabled,
            priority: rule.priority,
            mode: rule.mode,
            conditions: rule.conditions.clone(),
            rank: rule_rank(rule),
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
            let used = c.accepted_count + state.rule_accept_counts.get(&self.id).copied().unwrap_or(0);
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
            RuleMode::Route => self.matches_route(booking),   // implemented in Task 8
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
        self.booking_ids_norm
            .iter()
            .any(|id| tx == *id || bk == *id || rq == *id || (id.len() >= 9 && tx.contains(id.as_str())))
    }

    // Task 8 fills this in (strict + flexible route matching, ordered destination walk, guards).
    fn matches_route(&self, _booking: &Booking) -> bool {
        unimplemented!("implemented in Task 8")
    }

    // Task 9 fills this in (filter-mode conditions + CP-4 empty-filter guard).
    fn matches_filter(&self, _booking: &Booking) -> bool {
        unimplemented!("implemented in Task 9")
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
pub fn find_best_matching_rule(booking: &Booking, rules: &[AcceptRule], state: &MatchState) -> Option<CompiledRule> {
    let mut best: Option<CompiledRule> = None;
    for rule in rules {
        let compiled = CompiledRule::compile(rule);
        if !compiled.matches(booking, state) {
            continue;
        }
        best = match best {
            None => Some(compiled),
            Some(b) => Some(if compiled.rank() > b.rank() { compiled } else { b }),
        };
    }
    best
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
            let mut conditions = RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() };
            conditions.max_accept_count = 1;
            conditions.accepted_count = 1;
            let r = mk_rule(RuleMode::Route, conditions);
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn cap_reached_via_in_flight_rule_accept_counts_is_false() {
            let mut conditions = RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() };
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
        #[ignore = "requires matches_route (Task 8) — RuleMode::Route rule can't actually match yet"]
        fn under_cap_still_matches() {
            let mut conditions = RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() };
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
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["SPXID_VM_001396561".into()], ..Default::default() });
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_001396561".into();
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn short_partial_id_under_9_chars_does_not_substring_match() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["12345".into()], ..Default::default() });
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_12345_VM".into();
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn full_numeric_id_9_or_more_chars_substring_matches() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["001396561".into()], ..Default::default() });
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
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["SPXID VM 001397509".into()], ..Default::default() });
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_001397509".into();
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn separator_tolerant_stray_underscore_space_still_matches() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["SPXID_ VM_001397492C".into()], ..Default::default() });
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
                ..mk_rule(RuleMode::Route, RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() })
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
        #[ignore = "requires matches_route (Task 8) — RuleMode::Route rule can't actually match yet"]
        fn exact_booking_id_rule_beats_higher_priority_route_rule_on_same_ticket() {
            let mut b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            b.booking_id = "BKID12345678".into();
            b.spx_tx_id = "BKID12345678".into();
            let bkid = AcceptRule {
                id: "bk".into(),
                priority: 0,
                ..mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["BKID12345678".into()], ..Default::default() })
            };
            let route = AcceptRule {
                id: "rt".into(),
                priority: 9,
                ..mk_rule(RuleMode::Route, RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() })
            };
            let best = find_best_matching_rule(&b, &[route, bkid], &mk_state());
            let best = best.expect("expected a match");
            assert_eq!(best.id, "bk");
            assert_eq!(best.mode, RuleMode::BookingId);
        }

        #[test]
        #[ignore = "requires matches_route (Task 8) — RuleMode::Route rule can't actually match yet"]
        fn among_two_route_rules_higher_priority_still_wins() {
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            let conditions = || RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() };
            let lo = AcceptRule { id: "lo".into(), priority: 1, ..mk_rule(RuleMode::Route, conditions()) };
            let hi = AcceptRule { id: "hi".into(), priority: 5, ..mk_rule(RuleMode::Route, conditions()) };
            let best = find_best_matching_rule(&b, &[lo, hi], &mk_state()).expect("expected a match");
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
                ..mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["BKID12345678".into()], ..Default::default() })
            };
            let route = AcceptRule {
                id: "rt".into(),
                priority: 9,
                ..mk_rule(RuleMode::Route, RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() })
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
            let conditions = || RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() };
            let lo = AcceptRule { id: "lo".into(), priority: 1, ..mk_rule(RuleMode::Route, conditions()) };
            let hi = AcceptRule { id: "hi".into(), priority: 5, ..mk_rule(RuleMode::Route, conditions()) };
            assert!(rule_rank(&hi) > rule_rank(&lo));
        }
    }
}
