//! Fase 5 — notifier: pure fire-and-forget WAHA + n8n HTTP delivery.
//!
//! Design correction (#6, established during this plan's design phase): the
//! reference (`spx-portal-ref`) has NO internal pub/sub bus for
//! notifications — delivery is direct, best-effort HTTP:
//! `POST {waha_url}/api/sendText` for WhatsApp, an optional second
//! `POST {n8n_webhook_url}` call. This crate mirrors that: no bus/queue here.
//! Callers `tokio::spawn(async move { notifier::notify_accepted(...).await; })`
//! and drop the handle — a notify failure can NEVER affect an accept that
//! already succeeded, because every `notify_*` fn returns `()` and swallows
//! its own errors (logged via `tracing::warn!`, never propagated).
//!
//! `notifier` knows NOTHING of `executor`/`store`/`poller`/`SpxBooking` — it
//! is a leaf crate that takes plain data (`NotifyBooking`) and plain config
//! (`BotSettings`, including the already-decrypted WAHA API key — decryption
//! of the Fase-3 `spx-client::crypto` WAHA-key envelope happens in whatever
//! layer constructs `BotSettings`, not here).
//!
//! Message templates are ported from spx-portal-ref's
//! `apps/api/src/services/webhook.ts` — see `message.rs` for field-by-field
//! cross-check notes against the actual reference source.
pub mod message;
pub mod push_vapid;
pub mod waha;

pub use message::{
    build_agency_loss_text, build_driver_assigned_message, build_new_tickets_message,
    build_ticket_block, build_wa_message,
};
pub use push_vapid::{build_push_request, send_push_to_account, PushError, PushPayload, PushSubscription, VapidConfig};
pub use waha::{parse_chat_ids, send_n8n, send_to_waha_many};

/// Notification delivery config. The WAHA API key here is the already
/// PLAINTEXT/decrypted key — Fase 3's `spx-client::crypto` envelope
/// decryption of the `site_settings`-stored ciphertext happens in the
/// caller's layer (e.g. whatever assembles `BotSettings` before calling
/// `notify_*`), not in this crate.
#[derive(Debug, Clone, Default)]
pub struct BotSettings {
    pub enabled: bool,
    pub webhook_url: String,
    pub wa_group: String,
    /// The OTP-gate's personal-number delivery target (distinct from
    /// `wa_group` — the reference explicitly rejects `@g.us` group JIDs for
    /// OTP delivery; sending a one-time code to a shared group would defeat
    /// its purpose). Fase 6b's `api-gateway::otp` module is this field's
    /// first real consumer.
    pub wa_number: String,
    pub waha_url: String,
    pub waha_api_key: String,
    pub waha_session: String,
    pub portal_label: String,
}

/// Pure event data a notification template needs. Intentionally decoupled
/// from `executor::SpxBooking` (or any other fase's domain type) — whatever
/// layer has the real booking constructs this from its own fields.
#[derive(Debug, Clone, Default)]
pub struct NotifyBooking {
    pub booking_id: String,
    pub request_id: String,
    pub onsite_id: String,
    pub booking_name: String,
    pub spx_tx_id: String,
    pub vehicle_type: String,
    pub route_stops: Vec<String>,
    pub report_station: String,
    pub cost_type: Option<i64>,
    pub adhoc_tag: Option<i64>,
    pub standby_time: Option<i64>,
    pub period_start: Option<i64>,
    pub period_end: Option<i64>,
    pub bidding_ddl: Option<i64>,
    pub is_coc: bool,
}

/// Fire-and-forget accept notification. Returns `()` — a WAHA/n8n failure
/// only logs (`tracing::warn!` inside `waha::send_*`); it can never bubble up
/// to whatever already-succeeded accept flow spawned this.
pub async fn notify_accepted(settings: &BotSettings, b: &NotifyBooking) {
    if !settings.enabled {
        return;
    }
    let text = build_wa_message(b, &settings.portal_label);
    let _ = waha::send_to_waha_many(settings, &settings.wa_group, &text).await;
    waha::send_n8n(
        settings,
        serde_json::json!({ "event": "booking_accepted", "bookingId": b.booking_id, "message": text }),
    )
    .await;
}

/// Fire-and-forget new-ticket broadcast. Reference flood guard: silently
/// skip a backfill/seed burst of more than 30 tickets rather than blasting
/// the group.
pub async fn notify_new_tickets(settings: &BotSettings, bs: &[NotifyBooking], accept_base: &str) {
    if !settings.enabled || bs.is_empty() || bs.len() > 30 {
        return;
    }
    let text = build_new_tickets_message(bs, &settings.portal_label, accept_base);
    let _ = waha::send_to_waha_many(settings, &settings.wa_group, &text).await;
}

/// Fire-and-forget same-agency-loss alert.
pub async fn notify_agency_loss(settings: &BotSettings, spx_id: &str, rival: &str, latency_ms: i64, rule: Option<&str>) {
    if !settings.enabled {
        return;
    }
    let text = build_agency_loss_text(spx_id, rival, latency_ms, rule);
    let _ = waha::send_to_waha_many(settings, &settings.wa_group, &text).await;
}

/// Fire-and-forget driver-assigned follow-up.
#[allow(clippy::too_many_arguments)]
pub async fn notify_driver_assigned(
    settings: &BotSettings,
    tx_id: &str,
    booking_id: &str,
    onsite_id: &str,
    driver_name: &str,
    plate: &str,
) {
    if !settings.enabled {
        return;
    }
    let text = build_driver_assigned_message(tx_id, booking_id, onsite_id, driver_name, plate, &settings.portal_label);
    let _ = waha::send_to_waha_many(settings, &settings.wa_group, &text).await;
}
