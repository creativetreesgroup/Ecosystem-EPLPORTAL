pub mod booking;
pub mod coc;
pub mod location;
pub mod matching;
pub mod route_parse;
pub mod rule;
pub mod vehicle;

pub use booking::{Booking, BookingType};
pub use coc::{booking_type_of, is_coc, is_coc_name};
pub use location::{loc_match, loc_match_normalized, norm_loc};
pub use matching::{find_best_matching_rule, matched_booking_id_for, matches_rule, CompiledRule, RuleRank};
pub use route_parse::{parse_route_detail_list, parse_route_stops, RouteNode};
pub use rule::{
    dedupe_rules, sanitize_accept_rules, AcceptRule, MatchState, RawAcceptRule, RawRuleConditions,
    RouteMatchMode, RuleBookingType, RuleConditions, RuleMode, RuleSanitizeResult,
};
pub use vehicle::{
    canonical_rule_vehicle_label, norm_vehicle, vehicle_match, vehicle_match_normalized,
};

#[cfg(test)]
pub(crate) mod test_support {
    use crate::booking::Booking;
    use crate::rule::{AcceptRule, MatchState, RuleConditions, RuleMode};

    /// Mirrors the TS test helper `mkRule(mode, conditions, extra)`: build a minimal rule with
    /// sensible defaults (id "r1", name "test", enabled true, priority 0), then override
    /// specific fields with Rust struct-update syntax at the call site, e.g.
    /// `AcceptRule { priority: 9, id: "rt".into(), ..mk_rule(RuleMode::Route, conditions) }`.
    pub(crate) fn mk_rule(mode: RuleMode, conditions: RuleConditions) -> AcceptRule {
        AcceptRule { id: "r1".to_string(), name: "test".to_string(), enabled: true, priority: 0, mode, conditions }
    }

    pub(crate) fn mk_state() -> MatchState {
        MatchState::default()
    }

    /// Mirrors the TS test helper `mkBooking(routeStops, extra)`: build a booking with the
    /// given stops and every other field at its zero value, then override specific fields with
    /// Rust struct-update syntax at the call site, e.g. `Booking { spx_tx_id: "X".into(), ..mk_booking(&[]) }`.
    pub(crate) fn mk_booking(route_stops: &[&str]) -> Booking {
        Booking { route_stops: route_stops.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }
}
