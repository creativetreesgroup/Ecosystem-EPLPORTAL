// Backend/crates/poller/src/state.rs
//! Per-account owned state (`PollerState`) + the global shared context
//! (`PollerShared`) + config. `PollerState` is owned by exactly one Tokio task,
//! so its mutation is single-threaded BY CONSTRUCTION — no `polling` flag, no
//! interior mutability for the hot fields. The only cross-task sharing is the
//! per-account `Arc<Notify>` (poke, written by the notif watcher) and the
//! `Arc<AccountDedupState>` (restored before first poll).
use std::sync::Arc;

use core_domain::{CompiledRule, MatchState};
use dashmap::DashMap;
use executor::{AccountDedupState, ExecutorHandle};
use secrecy::SecretString;
use spx_client::{SpxClient, SpxCookies};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Reference env defaults (spx-portal-ref apps/api/src/env.ts): interval 100,
/// page size 50, max pages 10, FULL_SYNC_EVERY 3, fast-detect OFF, hedge OFF,
/// notif-watch 50ms, notif concurrency 2.
#[derive(Debug, Clone)]
pub struct PollerConfig {
    pub poll_interval_ms: u64,
    pub page_size: u32,
    pub max_pages: u32,
    pub full_sync_every: u64,
    pub fast_detect_pages: u32,
    pub sweep_hedge_ms: u64,
    pub notif_watch_ms: u64,
    pub notif_watch_concurrency: u32,
    pub primary_account_id: String,
}

impl Default for PollerConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 100,
            page_size: 50,
            max_pages: 10,
            full_sync_every: 3,
            fast_detect_pages: 0, // OFF (correction #1)
            sweep_hedge_ms: 0,    // OFF (correction #1)
            notif_watch_ms: 50,
            notif_watch_concurrency: 2,
            primary_account_id: String::new(),
        }
    }
}

impl PollerConfig {
    pub fn from_env() -> Self {
        fn u64v(k: &str, d: u64) -> u64 {
            std::env::var(k)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(d)
        }
        fn u32v(k: &str, d: u32) -> u32 {
            std::env::var(k)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(d)
        }
        let def = PollerConfig::default();
        PollerConfig {
            poll_interval_ms: u64v("SPX_POLL_INTERVAL_MS", def.poll_interval_ms),
            page_size: u32v("SPX_PAGE_SIZE", def.page_size),
            max_pages: u32v("SPX_MAX_PAGES", def.max_pages),
            full_sync_every: u64v("SPX_FULL_SYNC_EVERY", def.full_sync_every),
            fast_detect_pages: u32v("SPX_FAST_DETECT_PAGES", def.fast_detect_pages),
            sweep_hedge_ms: u64v("SPX_SWEEP_HEDGE_MS", def.sweep_hedge_ms),
            notif_watch_ms: u64v("SPX_NOTIF_WATCH_MS", def.notif_watch_ms),
            notif_watch_concurrency: u32v(
                "SPX_NOTIF_WATCH_CONCURRENCY",
                def.notif_watch_concurrency,
            ),
            primary_account_id: std::env::var("PORTAL_USERNAME")
                .unwrap_or_default()
                .trim()
                .to_lowercase(),
        }
    }
}

/// Per-account state, owned by one task.
pub struct PollerState {
    pub account_id: String,
    pub tenant_id: Uuid,
    pub agency_id: i64,
    pub poll_count: u64,
    pub cookies: SpxCookies,
    pub consecutive_401s: u32,
    pub last_pending_count: i64,
    pub self_email: Option<String>,
    pub dedup: Arc<AccountDedupState>,
    // Relogin bookkeeping (used by Task 7, wired into poll_once by Task 7b).
    pub last_relogin_attempt_ms: i64,
    pub last_daily_relogin_day: String,
    /// Credentials for `login::auto_login` (Task 7b). `SecretString` (not a
    /// bare `String`) so a stray `Debug`/log of `PollerState` — or of these
    /// fields directly — never prints the plaintext password/username;
    /// `secrecy` redacts `Debug`/`Display` and zeroizes the backing memory on
    /// drop. Populated once at account construction from already-decrypted
    /// credentials — decrypting from `store::agency_credentials` is a
    /// bootstrap-layer concern for whichever later fase constructs a real
    /// `PollerState`, NOT this task's job (same division Task 6 established
    /// for `rules`/`rule_meta`).
    pub username: SecretString,
    pub password: SecretString,
    /// Compiled accept rules for this account, index-aligned with `rule_meta`
    /// (`rule_meta[i]` is the DB identity — `Uuid`/cap/accepted_count — for
    /// `rules[i]`). Empty until the account bootstrap loads them from `store`
    /// (Task 6 focuses on the dispatch pipeline itself, not that loader).
    pub rules: Arc<Vec<CompiledRule>>,
    pub rule_meta: Arc<Vec<crate::dispatch::RuleMeta>>,
    pub match_state: MatchState,
}

impl PollerState {
    pub fn new(
        account_id: String,
        tenant_id: Uuid,
        agency_id: i64,
        cookies: SpxCookies,
        username: SecretString,
        password: SecretString,
    ) -> Self {
        Self {
            account_id,
            tenant_id,
            agency_id,
            poll_count: 0,
            cookies,
            consecutive_401s: 0,
            last_pending_count: -1,
            self_email: None,
            dedup: Arc::new(AccountDedupState::new()),
            last_relogin_attempt_ms: 0,
            last_daily_relogin_day: String::new(),
            username,
            password,
            rules: Arc::new(Vec::new()),
            rule_meta: Arc::new(Vec::new()),
            match_state: MatchState::default(),
        }
    }
}

/// A running account's control handle (poke to wake early; join to await stop).
pub struct AccountHandle {
    pub poke: Arc<Notify>,
    pub join: JoinHandle<()>,
}

/// Global, clone-shared context. `SpxClient`/`ExecutorHandle` are shared via
/// `Arc`; `PgPool` is itself an `Arc` clone.
#[derive(Clone)]
pub struct PollerShared {
    pub executor: Arc<ExecutorHandle>,
    pub client: Arc<SpxClient>,
    pub pool: store::PgPool,
    pub config: PollerConfig,
    pub accounts: Arc<DashMap<String, AccountHandle>>,
    /// Tier-1 login HTTP client (`auth-sidecar`, Task 9) — used by
    /// `login::auto_login`'s relogin wiring in `schedule::poll_once` (Task
    /// 7b). Shared (not per-account) since it is a stateless internal-HTTP
    /// client keyed by `account_id` per call, same reuse pattern as `client`.
    pub sidecar: Arc<crate::login::SidecarClient>,
    /// Fire-and-forget WAHA/n8n notification settings (Task 10b): `dispatch::finalize_win`
    /// and the `LostToAgency` branch spawn `notifier::notify_accepted`/`notify_agency_loss`
    /// through this when present. `Arc` because `BotSettings` is read-only, shared across
    /// every account's task, and cloned into a `tokio::spawn`'d future per accept — same
    /// sharing rationale as `client`/`executor`. Still an `Option` (not a bare value)
    /// because tests that don't exercise notification delivery construct `PollerShared`
    /// with `notifier: None`, a safe no-op the same way `redis: None` already is.
    pub notifier: Option<Arc<notifier::BotSettings>>,
    /// ws-hub Redis pub/sub publisher (Task 13): `dispatch::finalize_win`
    /// publishes a `ticket_accepted` event to `acct:<account_id>` through this
    /// when present. Unlike `notifier` above, this is the REAL type (not a
    /// deferred `()`) — Task 13 builds `RedisPublisher` itself. Still an
    /// `Option` (rather than a bare `RedisPublisher`) because tests that don't
    /// exercise ws delivery construct `PollerShared` with `redis: None`, a
    /// safe no-op the same way `notifier: None` is.
    pub redis: Option<crate::publish::RedisPublisher>,
}
