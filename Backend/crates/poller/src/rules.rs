// Backend/crates/poller/src/rules.rs
//! Loads a tenant's persisted `accept_rules`/`rule_booking_targets` rows into a compiled,
//! ready-to-match `RuleSet` (`state.rs`, Task 6). The ONLY place `core_domain::CompiledRule::compile`
//! is called in production — every `PollerState.rules` entry traces back through here, either at
//! boot (`reactor-core`'s bootstrap loop, this file's caller) or via a live reload (Task 11's
//! `PUT /bookings/settings` handler, same function, same caller contract).
use std::collections::HashMap;
use std::sync::Arc;

use core_domain::{
    AcceptRule as CoreAcceptRule, CompiledRule, RouteMatchMode, RuleBookingType, RuleConditions,
    RuleMode,
};
use uuid::Uuid;

use crate::dispatch::RuleMeta;
use crate::state::RuleSet;

fn mode_from_text(s: &str) -> RuleMode {
    match s {
        "booking_id" => RuleMode::BookingId,
        "route" => RuleMode::Route,
        _ => RuleMode::Filter,
    }
}

fn booking_type_from_text(s: &str) -> RuleBookingType {
    match s {
        "spxid" => RuleBookingType::Spxid,
        "reguler" => RuleBookingType::Reguler,
        _ => RuleBookingType::All,
    }
}

fn match_mode_from_text(s: &str) -> RouteMatchMode {
    match s {
        "flexible" => RouteMatchMode::Flexible,
        _ => RouteMatchMode::Strict,
    }
}

/// Loads every `accept_rules` row for `tenant_id`, joins in each rule's `rule_booking_targets`
/// (only `BookingId`-mode rules have any — other modes get an empty `booking_ids`, matching
/// `core_domain::RuleConditions`'s own "unused for this mode" convention), and compiles the
/// result. `rules[i]`/`rule_meta[i]` are built in the same loop iteration, so index-alignment
/// (the contract `dispatch.rs::dispatch_booking`'s `st.rule_meta[idx]` lookup relies on) holds
/// by construction.
pub async fn load_compiled_rules(
    pool: &store::PgPool,
    tenant_id: Uuid,
) -> Result<RuleSet, sqlx::Error> {
    let rows = store::accept_rules::list_all(pool, tenant_id).await?;
    let targets = store::rule_booking_targets::list_for_tenant(pool, tenant_id).await?;

    let mut targets_by_rule: HashMap<Uuid, Vec<String>> = HashMap::new();
    for t in targets {
        targets_by_rule
            .entry(t.rule_id)
            .or_default()
            .push(t.booking_id_raw);
    }

    let mut compiled = Vec::with_capacity(rows.len());
    let mut meta = Vec::with_capacity(rows.len());
    for row in rows {
        let booking_ids = targets_by_rule.remove(&row.id).unwrap_or_default();
        let core_rule = CoreAcceptRule {
            id: row.id.to_string(),
            name: row.name.clone(),
            enabled: row.enabled,
            priority: row.priority,
            mode: mode_from_text(&row.mode),
            conditions: RuleConditions {
                service_types: row.service_types.clone(),
                max_weight: row.max_weight,
                coc_only: row.coc_only,
                non_coc_only: row.non_coc_only,
                max_cod_amount: row.max_cod_amount,
                booking_ids,
                origin: row.origin.clone(),
                destinations: row.destinations.clone(),
                booking_type: booking_type_from_text(&row.booking_type),
                shift_types: row.shift_types.clone(),
                trip_types: row.trip_types.clone(),
                match_mode: match_mode_from_text(&row.match_mode),
                min_deadline_min: row.min_deadline_min.map(|m| m.max(0) as u32),
                max_accept_count: row.max_accept_count.max(0) as u32,
                accepted_count: row.accepted_count.max(0) as u32,
            },
        };
        compiled.push(CompiledRule::compile(&core_rule));
        meta.push(RuleMeta {
            uuid: row.id,
            cap: row.max_accept_count as i64,
            accepted_count: row.accepted_count as i64,
            name: row.name,
        });
    }

    Ok(RuleSet {
        rules: Arc::new(compiled),
        rule_meta: Arc::new(meta),
    })
}
