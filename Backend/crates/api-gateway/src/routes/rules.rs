// Backend/crates/api-gateway/src/routes/rules.rs
//! `GET`/`PUT /bookings/settings` — the rules editor + the `autoAccept` global kill switch,
//! OTP-gated on its `false→true` transition. See this task's own plan-doc header for the
//! replace-all persistence strategy and the OTP-consumption contract; both are load-bearing
//! design decisions, not incidental implementation details.
use axum::extract::{Extension, State};
use axum::routing::get;
use axum::{Json, Router};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::auth::permission::{require_permission, Permission};
use crate::auth::{session_auth, CurrentUser};
use crate::error::ApiError;
use crate::state::AppState;

fn mode_to_text(m: core_domain::RuleMode) -> &'static str {
    match m {
        core_domain::RuleMode::BookingId => "booking_id",
        core_domain::RuleMode::Route => "route",
        core_domain::RuleMode::Filter => "filter",
    }
}
fn booking_type_to_text(t: core_domain::RuleBookingType) -> &'static str {
    match t {
        core_domain::RuleBookingType::All => "all",
        core_domain::RuleBookingType::Spxid => "spxid",
        core_domain::RuleBookingType::Reguler => "reguler",
    }
}
fn match_mode_to_text(m: core_domain::RouteMatchMode) -> &'static str {
    match m {
        core_domain::RouteMatchMode::Strict => "strict",
        core_domain::RouteMatchMode::Flexible => "flexible",
    }
}

#[derive(Debug, Deserialize)]
pub struct RuleInput {
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    pub priority: Option<i64>,
    pub mode: Option<String>,
    #[serde(default)]
    pub service_types: Vec<String>,
    pub max_weight: Option<f64>,
    #[serde(default)]
    pub coc_only: bool,
    #[serde(default)]
    pub non_coc_only: bool,
    pub max_cod_amount: Option<f64>,
    #[serde(default)]
    pub booking_ids: Vec<String>,
    pub origin: Option<String>,
    #[serde(default)]
    pub destinations: Vec<String>,
    pub booking_type: Option<String>,
    #[serde(default)]
    pub shift_types: Vec<i64>,
    #[serde(default)]
    pub trip_types: Vec<i64>,
    pub match_mode: Option<String>,
    pub min_deadline_min: Option<f64>,
    pub max_accept_count: Option<f64>,
    pub accepted_count: Option<i64>,
}

fn to_raw_rule(input: &RuleInput) -> core_domain::RawAcceptRule {
    core_domain::RawAcceptRule {
        id: None,
        name: input.name.clone(),
        enabled: input.enabled,
        priority: input.priority,
        mode: input.mode.clone(),
        conditions: core_domain::RawRuleConditions {
            service_types: input.service_types.clone(),
            max_weight: input.max_weight,
            coc_only: input.coc_only,
            non_coc_only: input.non_coc_only,
            max_cod_amount: input.max_cod_amount,
            booking_ids: input.booking_ids.clone(),
            origin: input.origin.clone(),
            destinations: input.destinations.clone(),
            booking_type: input.booking_type.clone(),
            shift_types: input.shift_types.clone(),
            trip_types: input.trip_types.clone(),
            match_mode: input.match_mode.clone(),
            min_deadline_min: input.min_deadline_min,
            max_accept_count: input.max_accept_count,
            accepted_count: input.accepted_count,
        },
    }
}

fn to_new_accept_rule(r: &core_domain::AcceptRule) -> store::NewAcceptRule {
    store::NewAcceptRule {
        name: r.name.clone(),
        enabled: r.enabled,
        priority: r.priority,
        mode: mode_to_text(r.mode).to_string(),
        service_types: r.conditions.service_types.clone(),
        max_weight: r.conditions.max_weight,
        coc_only: r.conditions.coc_only,
        non_coc_only: r.conditions.non_coc_only,
        max_cod_amount: r.conditions.max_cod_amount,
        origin: r.conditions.origin.clone(),
        destinations: r.conditions.destinations.clone(),
        booking_type: booking_type_to_text(r.conditions.booking_type).to_string(),
        shift_types: r.conditions.shift_types.clone(),
        trip_types: r.conditions.trip_types.clone(),
        match_mode: match_mode_to_text(r.conditions.match_mode).to_string(),
        min_deadline_min: r.conditions.min_deadline_min.map(|v| v as i32),
        max_accept_count: r.conditions.max_accept_count as i32,
        accepted_count: r.conditions.accepted_count as i32,
    }
}

#[derive(Debug, Serialize)]
pub struct RuleOutput {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub priority: i32,
    pub mode: String,
    pub service_types: Vec<String>,
    pub max_weight: Option<f64>,
    pub coc_only: bool,
    pub non_coc_only: bool,
    pub max_cod_amount: Option<f64>,
    pub booking_ids: Vec<String>,
    pub origin: String,
    pub destinations: Vec<String>,
    pub booking_type: String,
    pub shift_types: Vec<i32>,
    pub trip_types: Vec<i32>,
    pub match_mode: String,
    pub min_deadline_min: Option<i32>,
    pub max_accept_count: i32,
    pub accepted_count: i32,
}

#[derive(Debug, Serialize)]
pub struct SettingsResponse {
    pub auto_accept_enabled: bool,
    pub rules: Vec<RuleOutput>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SettingsRequest {
    pub auto_accept_enabled: bool,
    #[serde(default)]
    pub rules: Vec<RuleInput>,
}

async fn build_response(
    pool: &store::PgPool,
    tenant_id: Uuid,
    warnings: Vec<String>,
) -> Result<SettingsResponse, ApiError> {
    let settings = store::get_automation_settings(pool, tenant_id).await?;
    let auto_accept_enabled = settings.map(|s| s.auto_accept_enabled).unwrap_or(false);

    let rows = store::accept_rules::list_all(pool, tenant_id).await?;
    let targets = store::rule_booking_targets::list_for_tenant(pool, tenant_id).await?;
    let mut targets_by_rule: HashMap<Uuid, Vec<String>> = HashMap::new();
    for t in targets {
        targets_by_rule
            .entry(t.rule_id)
            .or_default()
            .push(t.booking_id_raw);
    }

    let rules = rows
        .into_iter()
        .map(|r| {
            let booking_ids = targets_by_rule.remove(&r.id).unwrap_or_default();
            RuleOutput {
                id: r.id,
                name: r.name,
                enabled: r.enabled,
                priority: r.priority,
                mode: r.mode,
                service_types: r.service_types,
                max_weight: r.max_weight,
                coc_only: r.coc_only,
                non_coc_only: r.non_coc_only,
                max_cod_amount: r.max_cod_amount,
                booking_ids,
                origin: r.origin,
                destinations: r.destinations,
                booking_type: r.booking_type,
                shift_types: r.shift_types,
                trip_types: r.trip_types,
                match_mode: r.match_mode,
                min_deadline_min: r.min_deadline_min,
                max_accept_count: r.max_accept_count,
                accepted_count: r.accepted_count,
            }
        })
        .collect();

    Ok(SettingsResponse {
        auto_accept_enabled,
        rules,
        warnings,
    })
}

async fn get_settings(
    State(state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
) -> Result<Json<SettingsResponse>, ApiError> {
    Ok(Json(
        build_response(&state.poller.pool, user.tenant_id, vec![]).await?,
    ))
}

async fn put_settings(
    State(mut state): State<AppState>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<SettingsRequest>,
) -> Result<Json<SettingsResponse>, ApiError> {
    require_permission(&user, Permission::ManageRules)?;

    let current = store::get_automation_settings(&state.poller.pool, user.tenant_id).await?;
    let currently_enabled = current.map(|s| s.auto_accept_enabled).unwrap_or(false);

    if body.auto_accept_enabled && !currently_enabled {
        require_permission(&user, Permission::ArmAutoAccept)?;
        let key = crate::otp::pwverify_key(user.tenant_id, user.portal_user_id);
        let proof: Option<String> = state
            .redis
            .get(&key)
            .await
            .map_err(|e| ApiError::Internal(format!("redis get pwverify: {e}")))?;
        if proof.is_none() {
            return Err(ApiError::Unauthorized);
        }
        let _: () = state
            .redis
            .del(&key)
            .await
            .map_err(|e| ApiError::Internal(format!("redis del pwverify: {e}")))?;
    }

    let raw_rules: Vec<core_domain::RawAcceptRule> = body.rules.iter().map(to_raw_rule).collect();
    let sanitized = core_domain::sanitize_accept_rules(&raw_rules);
    let deduped = core_domain::dedupe_rules(&sanitized.rules);

    // Review finding (Task 11): the three writes below are NOT one atomic transaction —
    // `replace_all` is its own transaction, each `replace_for_rule` call below is a SEPARATE
    // transaction per rule, and `set_auto_accept_enabled` is a third. A mid-loop failure can
    // leave `accept_rules` fully replaced but some `BookingId` rules missing their
    // `rule_booking_targets`, with `auto_accept_enabled` never flipped. This is fail-safe by
    // construction, not silently assumed away: the arm never completes (no broadcast below
    // runs), nothing inconsistent is left reachable by a live poller account, and the very next
    // successful `PUT` self-heals (`replace_all` wipes every existing row fresh regardless of
    // this call's partial state).
    let new_rows: Vec<store::NewAcceptRule> = deduped.iter().map(to_new_accept_rule).collect();
    let inserted =
        store::accept_rules::replace_all(&state.poller.pool, user.tenant_id, &new_rows).await?;

    for (rule, row) in deduped.iter().zip(inserted.iter()) {
        if rule.mode == core_domain::RuleMode::BookingId {
            store::rule_booking_targets::replace_for_rule(
                &state.poller.pool,
                user.tenant_id,
                row.id,
                &rule.conditions.booking_ids,
            )
            .await?;
        }
    }

    store::set_auto_accept_enabled(&state.poller.pool, user.tenant_id, body.auto_accept_enabled)
        .await?;

    // Push the freshly committed rule set to every running poller account (Task 6/7's
    // channel). Reloaded fresh from the DB rather than re-derived from `deduped`/`inserted` in
    // memory, so the broadcast is guaranteed to match what was actually persisted.
    if let Ok(fresh) = poller::rules::load_compiled_rules(&state.poller.pool, user.tenant_id).await
    {
        let _ = state.poller.rules_tx.send(fresh);
    }

    Ok(Json(
        build_response(&state.poller.pool, user.tenant_id, sanitized.warnings).await?,
    ))
}

/// Nested at `/bookings` by `build_router`, alongside Task 8/9/10's `bookings_router` — two
/// separate routers sharing one prefix (`/live`, `/history`, `/:id/detail`, `/spx-log`,
/// `/:id/accept` from one; `/settings` from this one), merged in `lib.rs`.
pub fn rules_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/settings", get(get_settings).put(put_settings))
        .route_layer(axum::middleware::from_fn_with_state(state, session_auth))
}
