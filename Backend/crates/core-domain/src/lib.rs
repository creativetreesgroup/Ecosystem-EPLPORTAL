pub mod booking;
pub mod coc;
pub mod location;
pub mod route_parse;
pub mod rule;
pub mod vehicle;

pub use booking::{Booking, BookingType};
pub use coc::{booking_type_of, is_coc, is_coc_name};
pub use location::{loc_match, loc_match_normalized, norm_loc};
pub use route_parse::{parse_route_detail_list, parse_route_stops, RouteNode};
pub use rule::{
    sanitize_accept_rules, AcceptRule, MatchState, RawAcceptRule, RawRuleConditions, RouteMatchMode,
    RuleBookingType, RuleConditions, RuleMode, RuleSanitizeResult,
};
pub use vehicle::{
    canonical_rule_vehicle_label, norm_vehicle, vehicle_match, vehicle_match_normalized,
};

#[cfg(test)]
pub(crate) mod test_support {
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
}
