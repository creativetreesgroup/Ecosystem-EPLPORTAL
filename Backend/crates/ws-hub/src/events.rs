// Backend/crates/ws-hub/src/events.rs
//! The WS event union, ported from spx-portal-ref apps/api/src/ws/hub.ts:6-20.
//! `#[serde(tag="type", content="data")]` → `{"type":"...","data":...}`. Tag
//! strings are the EXACT reference snake_case; data field names are camelCase
//! (the reference UI protocol). `serde_json::Value` is used where the reference
//! carried open `& Record<string, unknown>` shapes.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WsEvent {
    #[serde(rename = "new_tickets")]
    NewTickets(Vec<Value>),
    #[serde(rename = "ticket_accepted")]
    TicketAccepted(Value),
    #[serde(rename = "ticket_rejected")]
    TicketRejected {
        #[serde(rename = "bookingId")]
        booking_id: String,
    },
    #[serde(rename = "ticket_simulated")]
    TicketSimulated(Value),
    #[serde(rename = "tickets_removed")]
    TicketsRemoved { ids: Vec<String> },
    #[serde(rename = "stats_update")]
    StatsUpdate(Value),
    #[serde(rename = "poller_status")]
    PollerStatus(Value),
    #[serde(rename = "cookies_expired")]
    CookiesExpired { message: String },
    #[serde(rename = "auto_relogin")]
    AutoRelogin { message: String },
    #[serde(rename = "connected")]
    Connected { session: String },
    #[serde(rename = "rules_updated")]
    RulesUpdated {
        #[serde(rename = "acceptRules")]
        accept_rules: Vec<Value>,
    },
    #[serde(rename = "pause_expired")]
    PauseExpired { message: String },
    #[serde(rename = "booking_enriched")]
    BookingEnriched(Value),
    #[serde(rename = "error")]
    Error { message: String },
}

impl WsEvent {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{\"type\":\"error\",\"data\":{\"message\":\"serialize\"}}".to_string())
    }
}
