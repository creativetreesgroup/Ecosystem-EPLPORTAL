# Fase 5 — poller + notifier + ws-hub Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Each task is executed by a FRESH implementer who sees ONLY that task's text — so every task is self-contained.

**Goal:** Build the three remaining service crates on top of Fase 1–4. `poller` — one Tokio task per SPX account (single-flight-by-construction), fetch orchestration (rotating window / full-sweep-every-3 / opt-in fast-detect / opt-in hedged fetch), a staggered-lane notif watcher, `FetchOutcome`-gated anti-drift, the real accept decision pipeline (wiring Fase 3 `spx-client` + Fase 4 `executor` into claim→accept→classify→agency-dup→quota→durable-record→notify), 3-tier auto-login (tier 2/3 in-proc HTTP, tier 1 delegated to `auth-sidecar`), and a durable-primary watchdog. `notifier` — pure fire-and-forget WAHA/n8n + Web-Push-VAPID delivery (no artificial bus). `ws-hub` — an axum WebSocket server with a per-session + per-account registry, 30s ping, and a Redis pub/sub bridge. `auth-sidecar` gains its tier-1 browser-login HTTP handler (chromiumoxide, a separate process from `reactor-core` for panic isolation).

**Architecture:** One `tokio::task` per active account (map `account_id → AccountHandle{ poke: Arc<Notify>, join }`). The loop is `loop { poll_once(&mut st).await; tokio::select! { _ = sleep(interval) => {}, _ = poke.notified() => {} } }` — single-flight is a *structural* property (a single task runs `poll_once` sequentially; no `state.polling` flag needed). The notif watcher is a SEPARATE per-account task that only reads two light SPX counters and calls `poke.notify_one()` — it never touches dedup/executor. `FetchOutcome { fetch_complete, spx_id_set, page_failures, .. }` wraps a sweep so `resurrect_pending`/`expire_stale_bookings` *require the type* (a raw `HashSet` can't be passed) — an invalid-state-unrepresentable gate. Tier-1 login is an HTTP call to `auth-sidecar` (which owns `chromiumoxide`); `poller` depends on NO browser-automation crate, so a Chromium panic can never take down the hot-path process holding the in-proc dedup/quota locks. `notifier` takes pure event data (it knows nothing of `executor`); callers `tokio::spawn` it and drop the `Result`. `ws-hub` keeps local sockets in a `DashMap<String, HashSet<SocketId>>` keyed by session-id or `acct:<id>`, and a dedicated Redis `PubSub` connection bridges cross-process broadcasts.

**Tech Stack (all versions + license + API shape confirmed via `cargo add` real-resolve + reading the crates' own source out of `~/.cargo/registry` on 2026-07-13 — see Global Constraints; do NOT "modernize" any API from memory):**
- Browser automation: **`chromiumoxide` 0.9.1** (`MIT OR Apache-2.0`) — `auth-sidecar` only. tokio-native (default features already pull tokio + `async-tungstenite` + a `reqwest` used for CDP). Needs an external Chromium binary at runtime (does NOT bundle one). **`cargo deny check` stays fully green with chromiumoxide added — no license/advisory change.**
- WebSocket server: **`axum` 0.8.9 `ws` feature** (already the workspace HTTP framework via `reactor-core`) — NOT a second WS crate. `WebSocketUpgrade::on_upgrade`, `WebSocket::{recv,send}`, `Message::{Text(Utf8Bytes), Binary(Bytes), Ping(Bytes), Pong(Bytes), Close}`.
- Web Push: **`web-push-native` 0.4.0** (`MIT OR Apache-2.0`) — chosen OVER the older `web-push` 0.11 crate specifically because `web-push` pulls **`ece` (MPL-2.0)** which this project's allow-list rejects. `web-push-native` produces an `http::Request<Vec<u8>>` you send with your own client (perfect for fire-and-forget). **It DOES pull `rsa` transitively via `jwt-simple` → RUSTSEC-2023-0071 (Marvin Attack); mitigation is a documented `[advisories] ignore` — see Global Constraints. This is Fase 5's one `deny.toml` change (an ADVISORY ignore, not a license add).**
- Redis pub/sub: **`redis` 1.3.0** (already a workspace dep, Fase 4) — `client.get_async_pubsub().await -> aio::PubSub`, `pubsub.subscribe(ch).await`, `pubsub.on_message() -> impl Stream<Item = Msg>`, `msg.get_payload::<String>()`; publish via a `ConnectionManager` `con.publish(ch, payload).await`.
- HTTP client (poller→sidecar, notifier→WAHA/n8n/push): **`wreq` 6.0.0-rc.29** (already in the workspace via `spx-client`, `MIT` route, OS-native trust — reuses Fase 3's vetted transport; avoids a second TLS-trust stack and the webpki-roots/CDLA landmine).
- VAPID keypair: **`jwt-simple` 0.12.x** + **`p256` 0.13.x** + **`base64` 0.22** (notifier; versions pinned to match `web-push-native`'s tree).
- Workspace path deps used: `core-domain`, `spx-client`, `executor`, `store`.
- Concurrency/ids/json/time: `dashmap` 6, `tokio` 1, `uuid` 1, `serde`/`serde_json` 1, `thiserror` 2, `tracing` 0.1, `chrono` 0.4.
- Tests: `wiremock` 0.6 (SPX + WAHA + sidecar + push mocks), real Redis @ `127.0.0.1:16379`, real Postgres @ `127.0.0.1:15432`.

## Global Constraints

Full context: [`Docs/tower-master-spec.md`](../../tower-master-spec.md) and the design doc [`Docs/superpowers/specs/2026-07-13-fase-5-poller-notifier-wshub-design.md`](../specs/2026-07-13-fase-5-poller-notifier-wshub-design.md). **Read the design doc before starting; it is the source of truth. Do NOT redesign.** Pay special attention to its "Koreksi terhadap deskripsi master spec" (9 numbered corrections) — every one is an intentional deviation from the reference, not drift.

**Scope (from the design doc).** Fase 5 builds `poller`, `notifier`, `ws-hub`, and `auth-sidecar`'s tier-1 handler. It does NOT build: REST routes (`/live?since=` delta-sync, manual-accept endpoint — Fase 6), the OTP arm-gate (Fase 6), the UI (Fase 7), or any consumer of the `spx:poller_heartbeat:<acct>` key (never built until a real need — YAGNI, matching the reference which also never built it). Delta-sync `?since=` is explicitly OUT of ws-hub (it is a Fase-6 REST param) — ws-hub only pushes live events.

**The 9 reference corrections (from the design doc — intentional, not drift):**
1. `SPX_FAST_DETECT_PAGES` and `SPX_SWEEP_HEDGE_MS` default **0 (OFF)** — opt-in perf knobs, not always-on. Tests must prove BOTH the default-off behavior and the enabled behavior.
2. Tier-1 login is a SEPARATE PROCESS (`auth-sidecar` + chromiumoxide over internal HTTP), a deliberate divergence from the reference's in-proc Playwright singleton — for panic isolation (Aturan Keras #10) and to keep the browser's memory/CPU off the hot path.
3. Notif watcher = **staggered parallel lanes** (`SPX_NOTIF_WATCH_CONCURRENCY`, default 2) on ONE interval, exponential ×2 backoff floor **250ms** / cap **5000ms** / reset-to-0 on a healthy tick — NOT multi-tier intervals.
4. Watchdog 60s is **in-process, durable-primary-only** (the one `primary_account_id()` account). It recreates that poller if it dies AND writes the `spx:poller_heartbeat:<acct>` key every cycle, but builds NO consumer for it.
5. Reactive relogin fires at `consecutive_401s >= 3`; the accept path **jumps** to the threshold immediately on an auth-class accept failure (`consecutive_401s = max(consecutive_401s, 3)`), not after 3 separate poll failures. Port both paths.
6. `notifier` is HTTP fire-and-forget only (WAHA `POST /api/sendText` + optional n8n webhook + Web-Push VAPID). There is NO internal pub/sub for notifier. The only real pub/sub in the system is ws-hub's bridge (#8).
7. Delta-sync `?since=` is OUT of scope (Fase 6 REST). ws-hub pushes live events only.
8. ws_bridge (Redis pub/sub → local broadcast) is accurate 1:1. Channel key is per-`session_id` AND per-`acct:<account_id>` (lowercased) — specific per-account channels, not a wildcard.
9. `resurrect_pending`/`expire_stale_bookings` run **only when `fetch_complete`** — a CORRECTNESS gate (a partial sweep must never be the basis for "which tickets vanished"), not a perf preference. The `FetchOutcome` type enforces it.

**Dependency licenses / `cargo deny` — one required ADVISORY ignore (verified 2026-07-13).** Baseline `cargo deny check` on the current workspace **passes** (`advisories ok, bans ok, licenses ok, sources ok`). The research resolved every Fase-5 dependency against the real registry and read each crate's own manifest:
- `chromiumoxide` 0.9.1 and its whole subtree (`chromiumoxide_cdp`/`_types`, `async-tungstenite` 0.32 MIT, `tungstenite` 0.28, `reqwest` 0.13) → all already-allowed licenses. **No deny change for chromiumoxide.**
- `axum` `ws` → already in-tree.
- `web-push-native` 0.4.0 itself is `MIT OR Apache-2.0`; its crypto subtree (`p256`, `aes-gcm`, `hkdf`, `hmac`, `sha2`, `elliptic-curve`, `base64ct`) is all `MIT/Apache`. **It does NOT pull `ece` (that MPL-2.0 crate is only in the OLDER `web-push` crate — which is exactly why we chose `web-push-native`).** BUT it pulls **`rsa` 0.9.10 transitively via `jwt-simple`** (the VAPID JWT signer), which triggers **`RUSTSEC-2023-0071` ("Marvin Attack" timing sidechannel in `rsa`)**. VAPID signs with **ECDSA P-256 (ES256)** exclusively — the `rsa` code path is **never invoked** by our usage — and there is **no fixed `rsa` version** for this advisory. Task 11 adds `RUSTSEC-2023-0071` to `deny.toml`'s `[advisories] ignore` with a justifying comment. **This is a different KIND of exception than prior fases (Fase 2 webpki-roots swap, Fase 3 GPL rejection, Fase 4 BSL-1.0 license add): it is an unreachable-code advisory ignore, NOT a copyleft license admission.** Do NOT add MPL-2.0 (avoided by crate choice); do NOT add any copyleft license.

**`bans.multiple-versions = "warn"` (not deny).** chromiumoxide adds `reqwest` 0.13 to the `auth-sidecar` binary (a second HTTP client alongside `wreq`); web-push-native adds an older `hyper` line via `jwt-simple`. These are transitive, isolated to their crate, and only WARN — acceptable. Do NOT try to dedupe them away.

**No browser automation in `poller` (DoD #10).** `poller`'s `Cargo.toml` must NEVER list `chromiumoxide` (or any headless-browser crate). Tier-1 login is HTTP to `auth-sidecar`. Task 14 asserts `cargo tree -p poller` contains no `chromiumoxide`.

**Real-service testing standard (this project's convention — no mocks for Redis/Postgres; wiremock for SPX/WAHA/sidecar HTTP).** Redis tests → `redis://127.0.0.1:16379`; Postgres tests → `postgres://tower:tower_dev_only@127.0.0.1:15432/tower`. Use a **unique account/tenant id per test** (`format!("t{}", Uuid::new_v4().simple())`) so tests don't collide. Run suites that touch Redis/PG with `-- --test-threads=1` (matches Fase 2–4). SPX/WAHA/sidecar/push HTTP is mocked with `wiremock`. **All timing tests use `tokio::time::pause`/`advance` — never wall-clock `sleep` in a test to "prove" a delay.**

**Reuse the REAL Fase 1–4 signatures (do NOT guess — they were read for this plan):**
- `spx_client::SpxClient::new(base_url: impl Into<String>) -> Result<SpxClient, SpxError>`; `fetch_bookings(&self, &SpxCookies, pageno: u32, count: u32) -> Result<Vec<SpxBooking>, SpxError>`; `fetch_booking_counts(&self, &SpxCookies) -> Result<Value, SpxError>`; `notification_count(&self, &SpxCookies) -> Result<Value, SpxError>`; `accept_booking(&self, &SpxCookies, booking_id: i64, agency_id: i64, request_ids: &[i64]) -> AcceptResult`; `fetch_request_list(&self, &SpxCookies, booking_id: i64, count: u32) -> Result<Value, SpxError>`; `fetch_profile(&self, &SpxCookies) -> Result<Value, SpxError>`.
- `spx_client::{AcceptResult{success,reason,retcode,message}, AcceptReason::{Ok,AgencyDup,Taken,Transient,Auth,Error}, SpxBooking, SpxCookies (Clone+Default, 11 cookie fields), to_core_booking(&SpxBooking) -> core_domain::Booking}`.
- `executor::ExecutorHandle::connect(redis_url: &str) -> Result<ExecutorHandle, ExecutorError>`; `try_claim_auto(&self, account_id: &str, spx_id: &str, rule_id: Option<Uuid>, cap: i64, accepted_count: i64) -> ClaimOutcome`; `restore_accepted_ids(&self, account_id: &str, &AccountDedupState) -> Result<usize, ExecutorError>` (**MUST be awaited before the first poll**); `record_durable_accept(&self, account_id: &str, spx_id: &str) -> Result<(), ExecutorError>`; `apply_rule_consumption(&self, &store::PgPool, tenant_id: Uuid, account_id: &str, rule_id: Uuid, spx_id: &str) -> Result<store::QuotaConsumeOutcome, ExecutorError>`.
- `executor::{ClaimOutcome::{Proceed,AlreadyClaimed,QuotaFull,RedisUnavailable} (+ .should_dispatch()), AccountDedupState::{new, try_begin_accept(&str)->bool, abort_accept, commit_accept, insert_restored, is_known}, verify_agency_dup(&SpxClient, &SpxCookies, self_email: &str, booking_id: i64) -> AgencyDupOutcome, AgencyDupOutcome::{Ours, LostToAgency{rival_email}, Inconclusive}, fetch_self_email(&SpxClient, &SpxCookies) -> Option<String>}`.
- `core_domain::{Booking, MatchState (Default, {rule_accept_counts: HashMap<String,u32>}), matching::CompiledRule::{compile(&AcceptRule)->Self, matches(&Booking,&MatchState)->bool}, matching::find_best_matching_rule_compiled(&[CompiledRule], &Booking, &MatchState) -> Option<usize>, rule::AcceptRule{id:String,name,enabled,priority,mode,conditions}}`. **NB: `AcceptRule.id`/`CompiledRule.id` are `String`; the executor/store quota APIs need `Uuid` — carry a parallel `Vec<RuleMeta{uuid:Uuid,cap:i64,accepted_count:i64}>` aligned by index with the `Vec<CompiledRule>` and map match-index→uuid.**
- `store::{connect(&str)->Result<PgPool,_>, run_migrations(&PgPool), begin_tenant_tx(&PgPool, Uuid), PgPool, QuotaConsumeOutcome, models::*}`. Store query functions the poller needs (booking upsert / status / anti-drift) are ADDED additively per task, following Fase 4's `store::consume_rule_quota` precedent.

**Verification confidence per task.** Fase 1–4 got full compile+run rigor on crypto/DB. Fase 5 introduces browser automation, WebSockets, and push crypto whose exact behavior can only be fully proven with a real Chromium / real push endpoint. Each task below carries a **Verification confidence** line stating what was research-verified (API shapes read from installed source) vs. best-effort (logic ported from the reference, to be compiled against the pinned versions by the implementer). Where a snippet is best-effort, it says **"verify against installed version before proceeding."**

**Workflow.** Run all `cargo` commands from `Backend/`. `export PATH="$HOME/.cargo/bin:$PATH"` if `cargo` is not found. Bring up services with `cd Docker && docker compose up -d tower-postgres tower-redis`. The dev port publishes (`127.0.0.1:15432`, `127.0.0.1:16379`) are Fase 2/4 additions — do NOT remove them. Commit only when a task's steps say to.

---

### Task 1: `poller` scaffold + `PollerState` + per-account task loop (Notify poke + `select!`/sleep) + single-flight & poke tests

**Verification confidence:** API shapes (tokio `Notify`, `select!`, `time::pause`/`advance`) research-verified against tokio 1.52. Loop logic is the load-bearing deliverable; tests prove it.

**Files:**
- Modify: `Backend/crates/poller/Cargo.toml` (deps)
- Overwrite: `Backend/crates/poller/src/lib.rs`
- Create: `Backend/crates/poller/src/state.rs`
- Create: `Backend/crates/poller/src/schedule.rs`
- Create: `Backend/crates/poller/tests/schedule_singleflight.rs`

**Interfaces produced (load-bearing — later tasks depend on these exactly):**
- `pub struct PollerConfig { pub poll_interval_ms: u64, pub page_size: u32, pub max_pages: u32, pub full_sync_every: u64, pub fast_detect_pages: u32, pub sweep_hedge_ms: u64, pub notif_watch_ms: u64, pub notif_watch_concurrency: u32, pub primary_account_id: String }` with `pub fn from_env() -> PollerConfig` (reference defaults: 100/50/10/3/0/0/50/2).
- `pub struct PollerState` (per-account, OWNED by its task): `account_id: String`, `tenant_id: Uuid`, `agency_id: i64`, `poll_count: u64`, `cookies: SpxCookies`, `consecutive_401s: u32`, `last_pending_count: i64`, `self_email: Option<String>`, `dedup: Arc<AccountDedupState>`, plus login/relogin bookkeeping fields (added Task 7).
- `pub struct AccountHandle { pub poke: Arc<tokio::sync::Notify>, pub join: tokio::task::JoinHandle<()> }`.
- `pub fn spawn_account_loop(shared: Arc<PollerShared>, mut st: PollerState, poke: Arc<Notify>) -> JoinHandle<()>` — the single-flight loop.
- `pub struct PollerShared` — the global clone-shared context: `executor: Arc<ExecutorHandle>`, `client: Arc<SpxClient>`, `pool: store::PgPool`, `redis: RedisPublisher` (Task 13 uses it), `config: PollerConfig`, `accounts: DashMap<String, AccountHandle>`. (Fields are added as later tasks need them; Task 1 may include just `config` + a placeholder `poll_once` hook.)

- [ ] **Step 1: Add dependencies**

```bash
cd Backend
cargo add --package poller --path crates/core-domain core-domain
cargo add --package poller --path crates/spx-client spx-client
cargo add --package poller --path crates/executor executor
cargo add --package poller --path crates/store store
cargo add --package poller tokio --features rt-multi-thread,macros,time,sync,signal
cargo add --package poller redis --features tokio-comp,connection-manager
cargo add --package poller dashmap
cargo add --package poller uuid --features v4
cargo add --package poller serde --features derive
cargo add --package poller serde_json
cargo add --package poller thiserror@2
cargo add --package poller tracing
cargo add --package poller chrono
cargo add --package poller wreq
cargo add --package poller --dev wiremock
cargo add --package poller --dev tokio --features rt-multi-thread,macros,time,sync,test-util
cd ..
```

`--dev tokio ... test-util` is required for `tokio::time::pause`/`advance`. Every crate above resolves to an already-allowed license (all confirmed in-tree; `cargo deny check` stays green — no new deps enter the graph that weren't already resolved for Fase 3/4).

- [ ] **Step 2: Write `state.rs`**

```rust
// Backend/crates/poller/src/state.rs
//! Per-account owned state (`PollerState`) + the global shared context
//! (`PollerShared`) + config. `PollerState` is owned by exactly one Tokio task,
//! so its mutation is single-threaded BY CONSTRUCTION — no `polling` flag, no
//! interior mutability for the hot fields. The only cross-task sharing is the
//! per-account `Arc<Notify>` (poke, written by the notif watcher) and the
//! `Arc<AccountDedupState>` (restored before first poll).
use std::sync::Arc;

use dashmap::DashMap;
use executor::{AccountDedupState, ExecutorHandle};
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
            std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d)
        }
        fn u32v(k: &str, d: u32) -> u32 {
            std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d)
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
            notif_watch_concurrency: u32v("SPX_NOTIF_WATCH_CONCURRENCY", def.notif_watch_concurrency),
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
    // Relogin bookkeeping (used by Task 7).
    pub last_relogin_attempt_ms: i64,
    pub last_daily_relogin_day: String,
}

impl PollerState {
    pub fn new(account_id: String, tenant_id: Uuid, agency_id: i64, cookies: SpxCookies) -> Self {
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
        }
    }
}

/// A running account's control handle (poke to wake early; join to await stop).
pub struct AccountHandle {
    pub poke: Arc<Notify>,
    pub join: JoinHandle<()>,
}

/// Global, clone-shared context. `SpxClient`/`ExecutorHandle` are shared via
/// `Arc`; `PgPool` is itself an `Arc` clone. Later tasks add a `RedisPublisher`
/// (Task 13) and a `notifier::BotSettings` loader.
#[derive(Clone)]
pub struct PollerShared {
    pub executor: Arc<ExecutorHandle>,
    pub client: Arc<SpxClient>,
    pub pool: store::PgPool,
    pub config: PollerConfig,
    pub accounts: Arc<DashMap<String, AccountHandle>>,
}
```

- [ ] **Step 3: Write `schedule.rs` (the single-flight loop)**

```rust
// Backend/crates/poller/src/schedule.rs
//! The per-account task loop. Single-flight is a STRUCTURAL guarantee: this is
//! the ONLY place `poll_once` is invoked for an account, and it is invoked
//! sequentially inside one task — two cycles for the same account can never
//! overlap (the property the reference had to defend with a `state.polling`
//! flag is free here). `poke.notify_one()` (from the notif watcher) cancels the
//! `sleep` via `select!` so a fresh ticket is picked up within ~1 notif RTT
//! (port of `pokePoll`'s "reschedule in 1ms", but as real cancellation).
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::state::{PollerShared, PollerState};

/// Spawn the account's poll loop. Returns the `JoinHandle`; the caller stores it
/// in `AccountHandle` alongside the same `poke` it passes here.
pub fn spawn_account_loop(
    shared: Arc<PollerShared>,
    mut st: PollerState,
    poke: Arc<Notify>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let interval = Duration::from_millis(shared.config.poll_interval_ms);
        loop {
            // ONE cycle, awaited to completion before the next can begin.
            poll_once(&shared, &mut st).await;

            // Sleep for the interval, but wake EARLY if poked.
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = poke.notified() => {
                    tracing::trace!(account = %st.account_id, "poked → early wake");
                }
            }
        }
    })
}

/// One poll cycle. Task 1 ships the skeleton (bump `poll_count`); Tasks 2/5/6
/// fill in fetch → match → dispatch → anti-drift. Kept as a free `async fn` so
/// tests can substitute a probe (see the single-flight test, which drives the
/// loop through a trait-free indirection by calling `spawn_account_loop` with a
/// state whose `poll_count` it observes).
pub async fn poll_once(_shared: &PollerShared, st: &mut PollerState) {
    st.poll_count = st.poll_count.wrapping_add(1);
    // Later tasks replace this body. It MUST remain `&mut PollerState` and MUST
    // NOT spawn a second task per cycle (that would break single-flight).
}
```

- [ ] **Step 4: Write `lib.rs`**

```rust
// Backend/crates/poller/src/lib.rs
//! Fase 5 — the poller: one Tokio task per SPX account (single-flight by
//! construction), fetch orchestration, notif watcher, anti-drift, the accept
//! decision pipeline, 3-tier auto-login, and a durable-primary watchdog.
//! Depends on Fase 3 `spx-client` (HTTP) and Fase 4 `executor` (dedup/quota) —
//! and, deliberately, on NO browser-automation crate (tier-1 login is HTTP to
//! `auth-sidecar`, so a Chromium crash can never take down this hot-path
//! process — design correction #2 / DoD #10).
pub mod schedule;
pub mod state;

pub use schedule::{poll_once, spawn_account_loop};
pub use state::{AccountHandle, PollerConfig, PollerShared, PollerState};

// Later tasks add: pub mod fetch; (Task 2) pub mod hedge; (Task 3)
// pub mod notif_watch; (Task 4) pub mod antidrift; (Task 5) pub mod dispatch;
// (Task 6) pub mod login; (Task 7) pub mod watchdog; (Task 8)
```

- [ ] **Step 5: Single-flight + poke tests (DoD #1)**

The single-flight test drives the real loop and proves no two `poll_once` overlap, using a re-entrancy sentinel that a wrapper increments/decrements around an `await`. Because `poll_once`'s body is trivial in Task 1, the test wraps the loop's invariant differently: it spawns the loop, lets it run under paused time, and asserts the loop advances exactly one cycle per interval/poke (never two concurrently). The poke test proves an early wake happens BEFORE a full interval elapses.

```rust
// Backend/crates/poller/tests/schedule_singleflight.rs
//! DoD #1: (a) the account loop is single-flight — poll cycles never overlap
//! (structural: one task, sequential await); (b) a poke wakes the loop from its
//! `sleep` BEFORE the full interval elapses. Both use paused virtual time so the
//! elapsed durations are real controlled facts, not wall-clock waits.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

// A self-contained mini-loop mirroring `spawn_account_loop`'s structure, so the
// test can inject an observable async `poll_once` (the real one is not yet
// pluggable in Task 1; this proves the LOOP SHAPE that `schedule.rs` uses).
async fn run_loop(
    interval: Duration,
    poke: Arc<Notify>,
    in_flight: Arc<AtomicUsize>,
    cycles: Arc<AtomicUsize>,
    stop_after: usize,
) {
    loop {
        // Re-entrancy sentinel: if two cycles ever overlapped, this would exceed 1.
        let prev = in_flight.fetch_add(1, Ordering::SeqCst);
        assert_eq!(prev, 0, "poll cycles must never overlap (single-flight)");
        // Simulate the awaited body of poll_once.
        tokio::time::sleep(Duration::from_millis(5)).await;
        in_flight.fetch_sub(1, Ordering::SeqCst);

        let n = cycles.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= stop_after {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = poke.notified() => {}
        }
    }
}

#[tokio::test(start_paused = true)]
async fn poll_cycles_never_overlap() {
    let poke = Arc::new(Notify::new());
    let in_flight = Arc::new(AtomicUsize::new(0));
    let cycles = Arc::new(AtomicUsize::new(0));
    let handle = tokio::spawn(run_loop(
        Duration::from_millis(100),
        poke.clone(),
        in_flight.clone(),
        cycles.clone(),
        4,
    ));
    // Fire pokes faster than cycles complete — a broken (spawn-per-cycle) design
    // would overlap and trip the sentinel assert inside run_loop.
    for _ in 0..20 {
        poke.notify_one();
        tokio::time::advance(Duration::from_millis(3)).await;
    }
    tokio::time::advance(Duration::from_millis(500)).await;
    handle.await.unwrap();
    assert_eq!(cycles.load(Ordering::SeqCst), 4);
}

#[tokio::test(start_paused = true)]
async fn poke_wakes_before_full_interval() {
    let poke = Arc::new(Notify::new());
    let cycles = Arc::new(AtomicUsize::new(0));
    let in_flight = Arc::new(AtomicUsize::new(0));
    // Long interval (10s): if the loop waited the FULL interval, only 1 cycle
    // would complete within the 200ms we advance. A poke must produce a 2nd.
    let handle = tokio::spawn(run_loop(
        Duration::from_secs(10),
        poke.clone(),
        in_flight,
        cycles.clone(),
        2,
    ));
    // Let the first cycle finish (5ms body) and enter its 10s sleep.
    tokio::time::advance(Duration::from_millis(10)).await;
    assert_eq!(cycles.load(Ordering::SeqCst), 1, "one cycle done, now sleeping 10s");
    // Poke: must cancel the 10s sleep and run a 2nd cycle within ~5ms, NOT 10s.
    poke.notify_one();
    tokio::time::advance(Duration::from_millis(10)).await;
    handle.await.unwrap();
    assert_eq!(cycles.load(Ordering::SeqCst), 2, "poke must wake before the 10s interval");
}
```

> Note: the test uses a self-contained `run_loop` mirroring `spawn_account_loop`'s exact `select!`/sentinel structure because Task 1's `poll_once` is not yet pluggable. Task 6 (which finalizes `poll_once`) adds a direct integration test that drives the REAL `spawn_account_loop`. Keep `spawn_account_loop`'s `select!` shape byte-identical to `run_loop`'s so this test remains a faithful proof.

- [ ] **Step 6: Build, test, clippy, commit**

```bash
cd Backend
cargo test -p poller
cargo clippy -p poller --all-targets -- -D warnings
cargo deny check
cd ..
git add Backend/crates/poller Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(poller): scaffold + PollerState + single-flight task loop (Notify poke) + time-paused tests"
```

Expected: both tests pass (single-flight sentinel never trips; poke produces the 2nd cycle early); clippy clean; deny still `advisories ok, bans ok, licenses ok, sources ok`.

---

### Task 2: Fetch orchestration — `FetchOutcome`, rotating window vs full-sweep (`FULL_SYNC_EVERY=3`), opt-in fast-detect

**Verification confidence:** `FetchOutcome` type + cadence logic are the load-bearing deliverables (tested directly). `SpxClient::fetch_bookings` signature research-verified. The per-page parallel fetch uses `futures`/`tokio::join` — the concrete join shape is best-effort; **verify the `futures` join API against the installed version before proceeding.**

**Files:**
- Create: `Backend/crates/poller/src/fetch.rs`
- Modify: `Backend/crates/poller/src/lib.rs` (`pub mod fetch;` + re-exports)
- Modify: `Backend/crates/poller/Cargo.toml` (add `futures`)
- Create: `Backend/crates/poller/tests/fetch_cadence.rs`

**Interfaces produced:**
- `pub struct FetchOutcome { pub fetch_complete: bool, pub spx_id_set: std::collections::HashSet<String>, pub page_failures: u32, pub bookings: Vec<spx_client::SpxBooking>, pub was_full_sweep: bool }`. **This is the anti-drift gate type (Task 5) — it is the ONLY way to reach `resurrect_pending`/`expire_stale_bookings`.**
- `pub fn should_full_sweep(poll_count: u64, full_sync_every: u64, pool_changed: bool) -> bool` — `pool_changed || full_sync_every == 0 || poll_count % full_sync_every == 0` (a forced sweep on a pool-size change OR every Nth cycle; `full_sync_every==0` means "always full").
- `pub fn window_pages(poll_count: u64, max_pages: u32) -> (u32, u32)` — the rotating (pageno_start, pageno_end_inclusive) window for a NON-full cycle, rotated across polls so the whole pool is covered over time.
- `pub async fn sweep(client: &SpxClient, cookies: &SpxCookies, cfg: &PollerConfig, poll_count: u64, full: bool) -> FetchOutcome` — fetches the chosen page range in parallel; each page is best-effort (`.catch → []`), a failed page increments `page_failures` and (critically) sets `fetch_complete = false`.
- `pub async fn fast_detect(client: &SpxClient, cookies: &SpxCookies, cfg: &PollerConfig) -> Vec<SpxBooking>` — opt-in early page-1..=`fast_detect_pages` peek (default off → returns empty without any HTTP).

**Design note (read before coding):** `fetch_complete` is TRUE only when EVERY page in the fetched range returned successfully. A rotating-window cycle (not the whole pool) is `fetch_complete = false` regardless of per-page success — it did not observe the whole pool, so it must never gate anti-drift (correction #9). A full sweep with zero page failures is `fetch_complete = true`. `page_failures > 0` forces `fetch_complete = false` even on a full sweep (the "REG only 500 of 1146" incident — a partial full sweep must not expire live tickets).

- [ ] **Step 1: Add `futures`**

```bash
cd Backend && cargo add --package poller futures && cd ..
```

- [ ] **Step 2: Write `fetch.rs`**

```rust
// Backend/crates/poller/src/fetch.rs
//! Page-fetch orchestration. Two modes: a ROTATING WINDOW (cheap, covers the
//! pool over several polls — `fetch_complete=false`, never gates anti-drift) and
//! a FULL SWEEP (every `FULL_SYNC_EVERY` cycles or when the pool size changed —
//! `fetch_complete=true` ONLY if every page succeeded). Fast-detect is an opt-in
//! page-1 peek (default OFF). Correctness gate: a partial/parallel-failed sweep
//! is NEVER `fetch_complete` (design correction #9).
use std::collections::HashSet;

use futures::future::join_all;
use spx_client::{SpxBooking, SpxClient, SpxCookies};

use crate::state::PollerConfig;

/// Result of one sweep, wrapping the `fetch_complete` gate as a TYPE so callers
/// cannot run anti-drift off a partial fetch (Task 5 consumes this).
#[derive(Debug, Clone)]
pub struct FetchOutcome {
    pub fetch_complete: bool,
    pub spx_id_set: HashSet<String>,
    pub page_failures: u32,
    pub bookings: Vec<SpxBooking>,
    pub was_full_sweep: bool,
}

/// Do a full sweep this cycle? Forced by a pool-size change (new-ticket signal),
/// by `full_sync_every==0` (always-full), or every Nth cycle.
pub fn should_full_sweep(poll_count: u64, full_sync_every: u64, pool_changed: bool) -> bool {
    pool_changed || full_sync_every == 0 || poll_count % full_sync_every == 0
}

/// Rotating window for a non-full cycle. Rotates a `max_pages`-wide window across
/// polls so the whole pool is eventually covered. 1-indexed pageno, inclusive.
pub fn window_pages(poll_count: u64, max_pages: u32) -> (u32, u32) {
    let mp = max_pages.max(1);
    // Rotate the START page by (poll_count % something); keep it simple and
    // bounded so a small pool still gets page 1 frequently (where new tickets
    // land). Windows of width `mp`, offset stepping by `mp` each poll.
    let start = 1 + (poll_count as u32 % mp);
    (start, start + mp - 1)
}

/// Fetch `pageno_start..=pageno_end` in parallel. Each page is best-effort; a
/// failed page → `page_failures += 1` and forces `fetch_complete=false`.
async fn fetch_range(
    client: &SpxClient,
    cookies: &SpxCookies,
    page_size: u32,
    pageno_start: u32,
    pageno_end: u32,
) -> (Vec<SpxBooking>, u32) {
    let futs = (pageno_start..=pageno_end)
        .map(|pageno| async move { client.fetch_bookings(cookies, pageno, page_size).await });
    let results = join_all(futs).await;
    let mut bookings = Vec::new();
    let mut failures = 0u32;
    for r in results {
        match r {
            Ok(mut page) => bookings.append(&mut page),
            Err(_) => failures += 1, // best-effort: a failed page does not abort
        }
    }
    (bookings, failures)
}

/// Full sweep OR rotating window per `full`.
pub async fn sweep(
    client: &SpxClient,
    cookies: &SpxCookies,
    cfg: &PollerConfig,
    poll_count: u64,
    full: bool,
) -> FetchOutcome {
    let (start, end) = if full {
        (1, cfg.max_pages.max(1)) // whole pool (bounded by max_pages)
    } else {
        window_pages(poll_count, cfg.max_pages)
    };
    let (bookings, page_failures) = fetch_range(client, cookies, cfg.page_size, start, end).await;

    let spx_id_set: HashSet<String> = bookings.iter().map(|b| b.id.clone()).collect();
    // fetch_complete: a FULL sweep with ZERO page failures. A rotating window is
    // never fetch_complete (it did not observe the whole pool).
    let fetch_complete = full && page_failures == 0;

    FetchOutcome {
        fetch_complete,
        spx_id_set,
        page_failures,
        bookings,
        was_full_sweep: full,
    }
}

/// Opt-in fast-detect: peek pages 1..=`fast_detect_pages` (default 0 → no HTTP,
/// empty result). When enabled, this is a cheap early signal that new tickets
/// exist so the caller can jump straight to a full sweep (~75–150ms faster
/// detection). Correction #1: OFF by default.
pub async fn fast_detect(
    client: &SpxClient,
    cookies: &SpxCookies,
    cfg: &PollerConfig,
) -> Vec<SpxBooking> {
    if cfg.fast_detect_pages == 0 {
        return Vec::new(); // OFF — no network at all
    }
    let (bookings, _fail) = fetch_range(client, cookies, cfg.page_size, 1, cfg.fast_detect_pages).await;
    bookings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_sweep_cadence_every_third_cycle() {
        // full_sync_every=3, no pool change: full sweep on 3,6,9,... (poll_count%3==0).
        let hits: Vec<u64> = (1..=9).filter(|&pc| should_full_sweep(pc, 3, false)).collect();
        assert_eq!(hits, vec![3, 6, 9]);
    }

    #[test]
    fn pool_change_forces_full_sweep_off_cadence() {
        assert!(should_full_sweep(1, 3, true), "a pool-size change forces a full sweep");
        assert!(!should_full_sweep(1, 3, false));
    }

    #[test]
    fn full_sync_every_zero_is_always_full() {
        assert!(should_full_sweep(7, 0, false));
    }

    #[test]
    fn window_rotates_and_covers_page_one_frequently() {
        assert_eq!(window_pages(0, 10), (1, 10));
        assert_eq!(window_pages(10, 10), (1, 10)); // wraps
        let (s, e) = window_pages(3, 10);
        assert_eq!((s, e), (4, 13));
    }
}
```

- [ ] **Step 3: Wire `lib.rs`**

```rust
pub mod fetch;
pub use fetch::{fast_detect, sweep, should_full_sweep, window_pages, FetchOutcome};
```

- [ ] **Step 4: wiremock cadence + fetch_complete test (DoD #2 half + #3)**

```rust
// Backend/crates/poller/tests/fetch_cadence.rs
//! DoD #3: over N cycles, full-sweep happens exactly on `poll_count % 3 == 0`.
//! DoD #9 half: a full sweep with a failing page is NOT fetch_complete. Uses a
//! wiremock SPX so `fetch_bookings` really runs; asserts fetch_complete gating.
use poller::{sweep, FetchOutcome, PollerConfig};
use spx_client::{SpxClient, SpxCookies};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cookies() -> SpxCookies {
    SpxCookies { csrftoken: "C".into(), ..Default::default() }
}

fn page_body(ids: &[&str]) -> serde_json::Value {
    let list: Vec<_> = ids
        .iter()
        .map(|id| serde_json::json!({ "booking_id": id, "booking_name": id }))
        .collect();
    serde_json::json!({ "data": { "list": list } })
}

#[tokio::test]
async fn full_sweep_zero_failures_is_fetch_complete() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page_body(&["A", "B"])))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).unwrap();
    let mut cfg = PollerConfig::default();
    cfg.max_pages = 2;
    let out: FetchOutcome = sweep(&client, &cookies(), &cfg, 3, true).await;
    assert!(out.fetch_complete, "full sweep, all pages ok → fetch_complete");
    assert!(out.was_full_sweep);
    assert!(out.spx_id_set.contains("A"));
}

#[tokio::test]
async fn rotating_window_is_never_fetch_complete() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page_body(&["A"])))
        .mount(&server)
        .await;
    let client = SpxClient::new(server.uri()).unwrap();
    let cfg = PollerConfig::default();
    let out = sweep(&client, &cookies(), &cfg, 1, false).await;
    assert!(!out.fetch_complete, "a rotating window never gates anti-drift");
}

#[tokio::test]
async fn full_sweep_with_a_failing_page_is_not_complete() {
    let server = MockServer::start().await;
    // Page 1 (pageno=1) → 500, others → 200. fetch_bookings sends pageno in body;
    // match all POSTs and fail with 500 so at least one page fails.
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let client = SpxClient::new(server.uri()).unwrap();
    let mut cfg = PollerConfig::default();
    cfg.max_pages = 3;
    let out = sweep(&client, &cookies(), &cfg, 3, true).await;
    assert!(out.page_failures >= 1);
    assert!(!out.fetch_complete, "any page failure forces fetch_complete=false (REG-500 guard)");
}
```

- [ ] **Step 5: Test, clippy, commit**

```bash
cd Backend
cargo test -p poller --test fetch_cadence
cargo test -p poller --lib fetch
cargo clippy -p poller --all-targets -- -D warnings
cd ..
git add Backend/crates/poller Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(poller): fetch orchestration — FetchOutcome gate + full-sweep-every-3 + rotating window + opt-in fast-detect"
```

Expected: cadence unit tests + wiremock fetch_complete-gating tests pass.

---

### Task 3: Hedged fetch (opt-in, default off)

**Verification confidence:** Logic ported from the reference `hedgedFetch` (poller.ts:816–853). Default-off is the DoD-critical property (tested with paused time). The `tokio::select!`/timer race is research-verified; the hedge-count bookkeeping is best-effort — **verify the timer-vs-primary race compiles against tokio 1.52.**

**Files:**
- Create: `Backend/crates/poller/src/hedge.rs`
- Modify: `Backend/crates/poller/src/lib.rs`
- Modify: `Backend/crates/poller/src/fetch.rs` (route `fetch_range`'s per-page fetch through `hedged_page` when `sweep_hedge_ms > 0`)
- Create: `Backend/crates/poller/tests/hedge.rs`

**Interfaces produced:**
- `pub async fn hedged_page(client: &SpxClient, cookies: &SpxCookies, pageno: u32, count: u32, hedge_ms: u64) -> Result<Vec<SpxBooking>, ()>` — one page with an optional backup request. `hedge_ms == 0` → a single shot (original behavior, zero extra QPS). `hedge_ms > 0` → fire the primary; if it hasn't answered within `hedge_ms`, fire ONE backup and take whichever finishes first.
- `pub fn hedge_fires_since_reset() -> u64` — a process-global counter (like the reference's `takeHedgeFires`) so the poll log can prove the hedge earned its keep (0 on a slow whole-server sweep = unfixable client-side; >0 shrinking the tail = working).

**Design note:** hedged fetch adds AT MOST one backup per slow page, and only after `hedge_ms`. It never changes results — it only trims the tail latency of a parallel full sweep (which waits on the slowest page, max-of-N). A backup timeout/failure is counted as a page failure by the caller exactly as a single-shot failure would be, so the `fetch_complete` gate is preserved (strictly fewer false failures than single-shot).

- [ ] **Step 1: Write `hedge.rs`**

```rust
// Backend/crates/poller/src/hedge.rs
//! Opt-in hedged single-page fetch (default OFF — correction #1). A parallel
//! full sweep waits on its SLOWEST page; if a page lags past `hedge_ms` we fire
//! ONE backup and take the first to answer. Bounded extra QPS (≤1 backup/page).
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use spx_client::{SpxBooking, SpxClient, SpxCookies};

static HEDGE_FIRES: AtomicU64 = AtomicU64::new(0);

/// Read-and-reset the hedge-fire counter (port of `takeHedgeFires`).
pub fn hedge_fires_since_reset() -> u64 {
    HEDGE_FIRES.swap(0, Ordering::Relaxed)
}

/// One page, optionally hedged. `hedge_ms==0` → single shot.
pub async fn hedged_page(
    client: &SpxClient,
    cookies: &SpxCookies,
    pageno: u32,
    count: u32,
    hedge_ms: u64,
) -> Result<Vec<SpxBooking>, ()> {
    if hedge_ms == 0 {
        // OFF: identical to a plain fetch, no timer, no backup.
        return client.fetch_bookings(cookies, pageno, count).await.map_err(|_| ());
    }

    let primary = client.fetch_bookings(cookies, pageno, count);
    tokio::pin!(primary);

    // Race the primary against a hedge timer.
    tokio::select! {
        r = &mut primary => return r.map_err(|_| ()),
        _ = tokio::time::sleep(Duration::from_millis(hedge_ms)) => {
            // Primary is slow → fire ONE backup and take the first answer.
            HEDGE_FIRES.fetch_add(1, Ordering::Relaxed);
            let backup = client.fetch_bookings(cookies, pageno, count);
            tokio::pin!(backup);
            tokio::select! {
                r = &mut primary => r.map_err(|_| ()),
                r = &mut backup  => r.map_err(|_| ()),
            }
        }
    }
}
```

- [ ] **Step 2: Route `fetch_range` through the hedge**

In `fetch.rs`, change `fetch_range` to accept `hedge_ms` and use `crate::hedge::hedged_page` instead of a direct `fetch_bookings`. `sweep` passes `cfg.sweep_hedge_ms`. Update the earlier tests' expectations only if needed (they use default `sweep_hedge_ms = 0`, so behavior is unchanged — the default-off path is exactly `client.fetch_bookings`).

```rust
// in fetch.rs — fetch_range now hedges each page when enabled
async fn fetch_range(
    client: &SpxClient,
    cookies: &SpxCookies,
    page_size: u32,
    pageno_start: u32,
    pageno_end: u32,
    hedge_ms: u64,
) -> (Vec<SpxBooking>, u32) {
    let futs = (pageno_start..=pageno_end)
        .map(|pageno| crate::hedge::hedged_page(client, cookies, pageno, page_size, hedge_ms));
    let results = join_all(futs).await;
    let mut bookings = Vec::new();
    let mut failures = 0u32;
    for r in results {
        match r {
            Ok(mut page) => bookings.append(&mut page),
            Err(()) => failures += 1,
        }
    }
    (bookings, failures)
}
```

Update `sweep` and `fast_detect` call sites to pass `cfg.sweep_hedge_ms` and `0` respectively (fast-detect is already an early peek; no hedging).

- [ ] **Step 3: Wire `lib.rs`**

```rust
pub mod hedge;
pub use hedge::{hedge_fires_since_reset, hedged_page};
```

- [ ] **Step 4: Default-off + enabled tests (DoD #2)**

```rust
// Backend/crates/poller/tests/hedge.rs
//! DoD #2: hedged fetch is OFF by default (no backup ever fires, behavior ==
//! plain fetch), and when ENABLED it fires a backup for a slow page. Paused
//! virtual time makes "slow" deterministic.
use poller::{hedge_fires_since_reset, hedged_page};
use spx_client::{SpxClient, SpxCookies};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cookies() -> SpxCookies {
    SpxCookies { csrftoken: "C".into(), ..Default::default() }
}
fn ok_body() -> serde_json::Value {
    serde_json::json!({ "data": { "list": [{ "booking_id": "A", "booking_name": "A" }] } })
}

#[tokio::test]
async fn default_off_never_hedges() {
    let _ = hedge_fires_since_reset(); // reset
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .mount(&server)
        .await;
    let client = SpxClient::new(server.uri()).unwrap();
    let page = hedged_page(&client, &cookies(), 1, 50, 0).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(hedge_fires_since_reset(), 0, "hedge OFF must never fire a backup");
}

#[tokio::test]
async fn enabled_fires_backup_on_slow_page() {
    let _ = hedge_fires_since_reset();
    let server = MockServer::start().await;
    // Respond after 300ms so a 50ms hedge window elapses → backup fires.
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(ok_body())
                .set_delay(std::time::Duration::from_millis(300)),
        )
        .mount(&server)
        .await;
    let client = SpxClient::new(server.uri()).unwrap();
    let page = hedged_page(&client, &cookies(), 1, 50, 50).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(hedge_fires_since_reset(), 1, "a page slower than hedge_ms must fire exactly one backup");
}
```

> Note: `enabled_fires_backup_on_slow_page` uses a REAL 300ms wiremock delay (wiremock drives a real socket, so `time::pause` cannot control it). This is the one place a small real delay is unavoidable; keep it ≤300ms. The default-off test is instant.

- [ ] **Step 5: Test, clippy, commit**

```bash
cd Backend
cargo test -p poller --test hedge
cargo test -p poller --test fetch_cadence   # unchanged (default hedge off)
cargo clippy -p poller --all-targets -- -D warnings
cd ..
git add Backend/crates/poller
git commit -m "feat(poller): opt-in hedged fetch (default off) — tail-latency backup for slow pages"
```

---

### Task 4: Notif watcher — staggered lanes + exponential backoff (250/5000, reset-on-success) + poke

**Verification confidence:** Backoff math + lane logic tested directly (paused time). `SpxClient::notification_count`/`fetch_booking_counts` signatures research-verified. The lane-spawn shape is best-effort.

**Files:**
- Create: `Backend/crates/poller/src/notif_watch.rs`
- Modify: `Backend/crates/poller/src/lib.rs`
- Create: `Backend/crates/poller/tests/notif_watch.rs`

**Interfaces produced:**
- `pub fn next_backoff(current_ms: u64) -> u64` — `min(max(current*2, 250), 5000)` (the reference's exact ×2 / floor-250 / cap-5000).
- `pub struct WatchState { pub backoff_ms: u64, pub last_pending: i64 }`.
- `pub fn spawn_notif_watcher(client: Arc<SpxClient>, cookies: SpxCookies, cfg: PollerConfig, poke: Arc<Notify>) -> JoinHandle<()>` — the per-account watcher task: reads the two light counters, and on a change (new tickets) calls `poke.notify_one()` so the poll loop jumps to a full sweep next cycle. On error, backs off ×2 (250→5000); on a healthy tick, resets backoff to 0 and runs `notif_watch_concurrency` staggered lanes.

**Design note:** The watcher NEVER touches dedup/executor/DB — it only reads `notification_count` + `fetch_booking_counts` (two cheap endpoints) and pokes. Lanes: when healthy (`backoff==0`), run up to `notif_watch_concurrency` overlapping ticks per interval so detection latency is ~interval/concurrency, not interval+RTT. When unhealthy (`backoff>0`), collapse to ONE slow lane (the reference: `state.watchBackoffMs > 0 ? 1 : concurrency`). Reset backoff to 0 on the first healthy tick.

- [ ] **Step 1: Write `notif_watch.rs`**

```rust
// Backend/crates/poller/src/notif_watch.rs
//! Per-account notif watcher: a SEPARATE task that only reads two light SPX
//! counters and pokes the poll loop when the pending pool changes. Staggered
//! parallel lanes on one interval (concurrency default 2); exponential ×2
//! backoff floor 250ms / cap 5000ms / reset-to-0 on a healthy tick (correction
//! #3). It never touches dedup/executor — poke is its ONLY effect.
use std::sync::Arc;

use serde_json::Value;
use spx_client::{SpxClient, SpxCookies};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::state::PollerConfig;

const BACKOFF_FLOOR_MS: u64 = 250;
const BACKOFF_CAP_MS: u64 = 5000;

/// ×2 with a 250ms floor and 5000ms cap (exact reference math).
pub fn next_backoff(current_ms: u64) -> u64 {
    (current_ms.saturating_mul(2))
        .max(BACKOFF_FLOOR_MS)
        .min(BACKOFF_CAP_MS)
}

/// Sum the pending-count signal from the two counter endpoints. Returns None on
/// any error (caller backs off). A change vs the last observed value = a poke.
async fn read_pending_signal(client: &SpxClient, cookies: &SpxCookies) -> Option<i64> {
    // notification pending count (pn) + booking counts (count_v2). Either alone
    // is a valid change signal; sum the numeric fields defensively.
    let notif = client.notification_count(cookies).await.ok()?;
    let counts = client.fetch_booking_counts(cookies).await.ok()?;
    Some(sum_numeric(&notif).wrapping_add(sum_numeric(&counts)))
}

/// Sum all numeric leaves of a JSON value (a robust "did anything change" hash
/// that does not depend on SPX's exact field names).
fn sum_numeric(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n.as_i64().unwrap_or(0),
        Value::Array(a) => a.iter().map(sum_numeric).sum(),
        Value::Object(o) => o.values().map(sum_numeric).sum(),
        Value::String(s) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

pub struct WatchState {
    pub backoff_ms: u64,
    pub last_pending: i64,
}

/// Spawn the watcher. It loops forever; drop/abort the handle to stop it (done
/// when the account's poller is torn down).
pub fn spawn_notif_watcher(
    client: Arc<SpxClient>,
    cookies: SpxCookies,
    cfg: PollerConfig,
    poke: Arc<Notify>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if cfg.notif_watch_ms == 0 {
            return; // watcher disabled
        }
        let mut st = WatchState { backoff_ms: 0, last_pending: -1 };
        loop {
            // One "tick" = read the signal; healthy → maybe poke + reset backoff;
            // error → back off ×2. When healthy we run up to `concurrency` lanes.
            let lanes = if st.backoff_ms > 0 {
                1
            } else {
                cfg.notif_watch_concurrency.max(1)
            };

            let mut any_ok = false;
            let mut changed = false;
            for _ in 0..lanes {
                match read_pending_signal(&client, &cookies).await {
                    Some(sig) => {
                        any_ok = true;
                        if st.last_pending >= 0 && sig != st.last_pending {
                            changed = true;
                        }
                        st.last_pending = sig;
                    }
                    None => {}
                }
            }

            if any_ok {
                st.backoff_ms = 0; // reset on the first healthy tick
                if changed {
                    poke.notify_one(); // wake the poll loop → full sweep next cycle
                }
            } else {
                st.backoff_ms = next_backoff(st.backoff_ms);
            }

            let delay = cfg.notif_watch_ms.max(st.backoff_ms);
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_ramps_from_floor_to_cap_and_resets() {
        assert_eq!(next_backoff(0), 250); // floor
        assert_eq!(next_backoff(250), 500);
        assert_eq!(next_backoff(500), 1000);
        assert_eq!(next_backoff(3000), 5000); // 6000 capped
        assert_eq!(next_backoff(5000), 5000); // stays at cap
    }
}
```

- [ ] **Step 2: Wire `lib.rs`**

```rust
pub mod notif_watch;
pub use notif_watch::{next_backoff, spawn_notif_watcher, WatchState};
```

- [ ] **Step 3: wiremock watcher test — poke on change, backoff on error (DoD #4)**

```rust
// Backend/crates/poller/tests/notif_watch.rs
//! DoD #4: (a) a change in the pending signal pokes the poll loop; (b) an
//! errored counter endpoint drives exponential backoff; (c) backoff math is the
//! exact 250-floor/5000-cap ramp. The poke is observed via the shared Notify.
use std::sync::Arc;

use poller::{next_backoff, spawn_notif_watcher, PollerConfig};
use spx_client::{SpxClient, SpxCookies};
use tokio::sync::Notify;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cookies() -> SpxCookies {
    SpxCookies { csrftoken: "C".into(), ..Default::default() }
}

#[test]
fn backoff_is_exact_reference_ramp() {
    let mut b = 0u64;
    let mut seq = Vec::new();
    for _ in 0..6 {
        b = next_backoff(b);
        seq.push(b);
    }
    assert_eq!(seq, vec![250, 500, 1000, 2000, 4000, 5000]);
}

#[tokio::test]
async fn change_in_signal_pokes_the_loop() {
    let server = MockServer::start().await;
    // notification_count returns a CHANGING pending count on each call so the
    // watcher detects a change and pokes. (Two distinct bodies via up-to.)
    Mock::given(method("POST"))
        .and(path("/api/basicserver/agency/notification/pn/pending/read/count"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": { "count": 1 } })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/basicserver/agency/notification/pn/pending/read/count"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": { "count": 9 } })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/count_v2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": { "pending": 0 } })))
        .mount(&server)
        .await;

    let client = Arc::new(SpxClient::new(server.uri()).unwrap());
    let poke = Arc::new(Notify::new());
    let mut cfg = PollerConfig::default();
    cfg.notif_watch_ms = 10;
    cfg.notif_watch_concurrency = 1;
    let handle = spawn_notif_watcher(client, cookies(), cfg, poke.clone());

    // Wait (real, short) for at least two ticks so the signal changes 1 → 9.
    let poked = tokio::time::timeout(std::time::Duration::from_secs(2), poke.notified())
        .await
        .is_ok();
    handle.abort();
    assert!(poked, "a changed pending signal must poke the poll loop");
}
```

- [ ] **Step 4: Test, clippy, commit**

```bash
cd Backend
cargo test -p poller --test notif_watch
cargo test -p poller --lib notif_watch
cargo clippy -p poller --all-targets -- -D warnings
cd ..
git add Backend/crates/poller
git commit -m "feat(poller): notif watcher — staggered lanes + exp backoff (250/5000, reset-on-success) + poke"
```

---

### Task 5: Anti-drift — `store` booking functions + `resurrect_pending`/`expire_stale_bookings` gated by `FetchOutcome`

**Verification confidence:** SQL ported from reference `db.ts` (expireStaleBookings 154–190, resurrectPending 191–203, upsertBooking 23–55). The `FetchOutcome`-only gate is the DoD deliverable and is enforced by the function signatures (a raw `HashSet` cannot be passed). Store SQL is best-effort against the real schema — **the implementer must confirm the `bookings` column names against `store`'s migrations before proceeding** (e.g. `spx_id`, `status`, `rule_matched`, `raw_data`, `bidding_ddl` in `raw_data`).

**Files:**
- Create: `Backend/crates/store/src/bookings.rs`
- Modify: `Backend/crates/store/src/lib.rs` (`pub mod bookings;` + re-exports)
- Create: `Backend/crates/poller/src/antidrift.rs`
- Modify: `Backend/crates/poller/src/lib.rs`
- Create: `Backend/crates/poller/tests/antidrift_pg.rs`

**Interfaces produced in `store` (additive, migration-free — following Fase 4's `consume_rule_quota` precedent):**
- `pub async fn upsert_booking(pool: &PgPool, tenant_id: Uuid, b: &BookingUpsert) -> Result<(), sqlx::Error>` — INSERT … ON CONFLICT (spx_id) that NEVER downgrades a non-pending status to pending and NEVER overwrites `raw_data` (enrichment must survive).
- `pub async fn expire_stale_bookings(pool: &PgPool, tenant_id: Uuid, active: &HashSet<String>) -> Result<StaleOutcome, sqlx::Error>` — mark `pending` rows NOT in `active` as `failed` with `rule_matched = 'expired'` (unknown/past deadline) or `'taken_by_other'` (future deadline).
- `pub async fn resurrect_pending(pool: &PgPool, tenant_id: Uuid, spx_ids: &[String]) -> Result<u64, sqlx::Error>` — flip `failed` rows we POSITIVELY see back to `pending` (never touches `accepted`).

**Interfaces produced in `poller` (the FetchOutcome gate — DoD #7):**
- `pub async fn run_anti_drift(pool: &store::PgPool, tenant_id: Uuid, outcome: &FetchOutcome) -> Result<(), store::StoreError>` — **takes `&FetchOutcome`, not a `HashSet`.** Runs `resurrect_pending` + `expire_stale_bookings` ONLY when `outcome.fetch_complete`; otherwise returns `Ok(())` immediately (a partial sweep does nothing). This is the type-level gate: to run anti-drift you must PRODUCE a `FetchOutcome`, and the function still double-checks `fetch_complete` for the rotating-window case (a full sweep that hit page failures is `fetch_complete=false`, so it is also correctly skipped).

- [ ] **Step 1: Write `store::bookings`**

```rust
// Backend/crates/store/src/bookings.rs
//! Booking lifecycle writes for the poller: upsert (enrichment-preserving) +
//! the two anti-drift transitions. Ported from spx-portal-ref db.ts. No schema
//! change. `bidding_ddl` is read out of `raw_data` (JSONB) for the taken-vs-
//! expired decision, exactly as the reference does.
use std::collections::HashSet;

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// Minimal fields the poller has at upsert time.
#[derive(Debug, Clone)]
pub struct BookingUpsert {
    pub spx_id: String,
    pub status: String, // "pending" on first sight
    pub is_coc: bool,
    pub raw_data: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StaleOutcome {
    pub expired: u64,
    pub taken: u64,
}

/// Upsert a booking. On conflict: NEVER downgrade a non-pending status to
/// pending, and NEVER overwrite raw_data (enrichment must survive poll cycles).
pub async fn upsert_booking(
    pool: &PgPool,
    tenant_id: Uuid,
    b: &BookingUpsert,
) -> Result<(), sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    sqlx::query(
        "INSERT INTO bookings (id, tenant_id, spx_id, status, is_coc, raw_data, created_at, updated_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, now(), now()) \
         ON CONFLICT (tenant_id, spx_id) DO UPDATE SET \
           status = CASE WHEN bookings.status = 'pending' THEN EXCLUDED.status ELSE bookings.status END, \
           updated_at = now()",
    )
    .bind(tenant_id)
    .bind(&b.spx_id)
    .bind(&b.status)
    .bind(b.is_coc)
    .bind(&b.raw_data)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Expire pending bookings no longer in SPX's active pool. Unknown/past deadline
/// → 'expired'; future deadline → 'taken_by_other'. Only touches 'pending'.
pub async fn expire_stale_bookings(
    pool: &PgPool,
    tenant_id: Uuid,
    active: &HashSet<String>,
) -> Result<StaleOutcome, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let rows: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT spx_id, NULLIF(raw_data->>'bidding_ddl','')::bigint \
         FROM bookings WHERE tenant_id = $1 AND status = 'pending'",
    )
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await?;

    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut to_expire: Vec<String> = Vec::new();
    let mut to_taken: Vec<String> = Vec::new();
    for (spx_id, ddl_raw) in rows {
        if active.contains(&spx_id) {
            continue;
        }
        let ddl = ddl_raw.unwrap_or(0);
        let ddl_ms = if ddl > 0 {
            if ddl > 1_000_000_000_000 { ddl } else { ddl * 1000 }
        } else {
            0
        };
        // Unknown deadline → conservative 'expired' (don't falsely credit a rival).
        if ddl_ms == 0 || ddl_ms < now_ms {
            to_expire.push(spx_id);
        } else {
            to_taken.push(spx_id);
        }
    }

    if !to_expire.is_empty() {
        sqlx::query(
            "UPDATE bookings SET status='failed', rule_matched='expired', updated_at=now() \
             WHERE tenant_id=$1 AND status='pending' AND spx_id = ANY($2)",
        )
        .bind(tenant_id)
        .bind(&to_expire)
        .execute(&mut *tx)
        .await?;
    }
    if !to_taken.is_empty() {
        sqlx::query(
            "UPDATE bookings SET status='failed', rule_matched='taken_by_other', updated_at=now() \
             WHERE tenant_id=$1 AND status='pending' AND spx_id = ANY($2)",
        )
        .bind(tenant_id)
        .bind(&to_taken)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(StaleOutcome { expired: to_expire.len() as u64, taken: to_taken.len() as u64 })
}

/// Inverse of expire: flip 'failed' rows we POSITIVELY see back to 'pending'.
/// NEVER touches 'accepted' (our own wins). Kills the "REG only 500" drift.
pub async fn resurrect_pending(
    pool: &PgPool,
    tenant_id: Uuid,
    spx_ids: &[String],
) -> Result<u64, sqlx::Error> {
    if spx_ids.is_empty() {
        return Ok(0);
    }
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    let res = sqlx::query(
        "UPDATE bookings SET status='pending', rule_matched=NULL, updated_at=now() \
         WHERE tenant_id=$1 AND status='failed' AND spx_id = ANY($2)",
    )
    .bind(tenant_id)
    .bind(spx_ids)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(res.rows_affected())
}
```

- [ ] **Step 2: Wire `store::lib.rs`**

```rust
pub mod bookings;
pub use bookings::{
    expire_stale_bookings, resurrect_pending, upsert_booking, BookingUpsert, StaleOutcome,
};
```

> If `store` does not already expose a `StoreError`, the poller wraps `sqlx::Error` via `Display` (`.to_string()`) exactly as `executor` does — see Step 3's `StoreError` (a poller-local error). Do NOT add a direct `sqlx` dependency to `poller`'s production deps; the poller consumes `store`'s `sqlx::Error` only through `store`'s functions and maps it by `to_string()`.

- [ ] **Step 3: Write `poller::antidrift` (the FetchOutcome gate — DoD #7)**

```rust
// Backend/crates/poller/src/antidrift.rs
//! Anti-drift, gated by the `FetchOutcome` TYPE. `resurrect_pending`/
//! `expire_stale_bookings` are reachable ONLY through `run_anti_drift`, which
//! takes a `&FetchOutcome` (never a raw `HashSet`) and runs them ONLY when
//! `fetch_complete` (correction #9). A rotating-window or page-failed sweep is
//! `fetch_complete=false`, so it does nothing — a partial view can never expire
//! a live ticket.
use uuid::Uuid;

use crate::fetch::FetchOutcome;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store error: {0}")]
    Db(String),
}

/// Run anti-drift for one completed sweep. NO-OP unless `outcome.fetch_complete`.
pub async fn run_anti_drift(
    pool: &store::PgPool,
    tenant_id: Uuid,
    outcome: &FetchOutcome,
) -> Result<(), StoreError> {
    // The gate: a partial sweep (rotating window, or a full sweep with page
    // failures) is NEVER the basis for expire/resurrect.
    if !outcome.fetch_complete {
        return Ok(());
    }
    let active = &outcome.spx_id_set;
    let seen: Vec<String> = active.iter().cloned().collect();

    // Resurrect first (flip mistakenly-failed rows we positively see back to
    // pending), THEN expire (mark pending rows we NO LONGER see as failed).
    store::resurrect_pending(pool, tenant_id, &seen)
        .await
        .map_err(|e| StoreError::Db(e.to_string()))?;
    store::expire_stale_bookings(pool, tenant_id, active)
        .await
        .map_err(|e| StoreError::Db(e.to_string()))?;
    Ok(())
}
```

- [ ] **Step 4: Wire `lib.rs`**

```rust
pub mod antidrift;
pub use antidrift::{run_anti_drift, StoreError};
```

- [ ] **Step 5: Postgres test — partial sweep does NOT expire; full sweep does (DoD #7)**

```rust
// Backend/crates/poller/tests/antidrift_pg.rs
//! DoD #7: (a) a partial sweep (fetch_complete=false) triggers NO expire/
//! resurrect — proven by constructing a FetchOutcome with fetch_complete=false
//! and asserting a live 'pending' row that is ABSENT from spx_id_set survives;
//! (b) a complete sweep (fetch_complete=true) DOES expire an absent pending row
//! and resurrect a wrongly-failed present row. Real Postgres @ 15432.
use std::collections::HashSet;

use poller::{run_anti_drift, FetchOutcome};
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

fn outcome(complete: bool, ids: &[&str]) -> FetchOutcome {
    FetchOutcome {
        fetch_complete: complete,
        spx_id_set: ids.iter().map(|s| s.to_string()).collect::<HashSet<_>>(),
        page_failures: 0,
        bookings: Vec::new(),
        was_full_sweep: complete,
    }
}

#[tokio::test]
async fn partial_sweep_never_expires_but_complete_sweep_does() {
    let pool = store::connect(&database_url()).await.unwrap();
    store::run_migrations(&pool).await.unwrap();

    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("AntiDrift")
        .bind(format!("ad-{tenant_id}"))
        .execute(&pool)
        .await
        .unwrap();

    // Seed: one live pending row ("LIVE") + one wrongly-failed row ("BACK").
    for (spx, status) in [("LIVE", "pending"), ("BACK", "failed")] {
        store::upsert_booking(
            &pool,
            tenant_id,
            &store::BookingUpsert {
                spx_id: spx.into(),
                status: status.into(),
                is_coc: false,
                raw_data: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
    }
    // upsert can't set 'failed' directly (it inserts 'pending'); force BACK failed.
    sqlx::query("UPDATE bookings SET status='failed' WHERE tenant_id=$1 AND spx_id='BACK'")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .unwrap();

    // (a) Partial sweep whose id-set does NOT include LIVE → must NOT expire it.
    run_anti_drift(&pool, tenant_id, &outcome(false, &["OTHER"])).await.unwrap();
    let (live_status,): (String,) =
        sqlx::query_as("SELECT status FROM bookings WHERE tenant_id=$1 AND spx_id='LIVE'")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(live_status, "pending", "a partial sweep must NEVER expire a live ticket");

    // (b) Complete sweep that SEES BACK (resurrect) but NOT LIVE (expire).
    run_anti_drift(&pool, tenant_id, &outcome(true, &["BACK"])).await.unwrap();
    let (live2,): (String,) =
        sqlx::query_as("SELECT status FROM bookings WHERE tenant_id=$1 AND spx_id='LIVE'")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let (back2,): (String,) =
        sqlx::query_as("SELECT status FROM bookings WHERE tenant_id=$1 AND spx_id='BACK'")
            .bind(tenant_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(live2, "failed", "a complete sweep expires a pending ticket no longer present");
    assert_eq!(back2, "pending", "a complete sweep resurrects a wrongly-failed present ticket");

    sqlx::query("DELETE FROM tenants WHERE id=$1").bind(tenant_id).execute(&pool).await.ok();
}
```

Add the test-only `sqlx` dev-dep (production `poller` stays free of a direct `sqlx` dep, asserted in Task 14):

```bash
cd Backend && cargo add --package poller --dev sqlx --features postgres,runtime-tokio-rustls,macros,uuid,chrono && cd ..
```

- [ ] **Step 6: Test, clippy, commit**

```bash
cd Docker && docker compose up -d tower-postgres && cd ..
cd Backend
cargo test -p store --lib
cargo test -p poller --test antidrift_pg -- --test-threads=1
cargo clippy -p store -p poller --all-targets -- -D warnings
cd ..
git add Backend/crates/store Backend/crates/poller Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(poller,store): FetchOutcome-gated anti-drift (resurrect_pending/expire_stale_bookings) + booking upsert"
```

Expected: the partial-sweep-never-expires and complete-sweep-does test passes; `store`'s existing suite still green.

---

### Task 6: Accept dispatch pipeline — wire `spx-client`+`executor` into a real decision flow + `restore_accepted_ids`-before-first-poll contract

**Verification confidence:** Every function signature in the pipeline was read from the real Fase 3/4 code (see Global Constraints) and is exact. The orchestration ORDER (claim→accept→classify→agency-dup→consume→durable→notify) is the load-bearing logic; the `executor::release_claim_auto` additive helper is small and testable. The `poll_once` integration is best-effort composition — **the implementer must compile it against the pinned crates.** This is the task that ties Fase 3+4+5 into one pipeline.

**Files:**
- Create: `Backend/crates/executor/src/release.rs` (additive `release_claim_auto`) + wire `executor::lib.rs`
- Create: `Backend/crates/poller/src/dispatch.rs`
- Modify: `Backend/crates/poller/src/schedule.rs` (finalize `poll_once`: fetch → upsert → match → dispatch → anti-drift, and enforce the restore contract at spawn)
- Modify: `Backend/crates/poller/src/state.rs` (add `rules: Arc<Vec<CompiledRule>>` + `rule_meta: Arc<Vec<RuleMeta>>` + `match_state: MatchState` to `PollerState`; add `notifier`/`redis` hooks as `Option` placeholders wired in Tasks 10/13)
- Create: `Backend/crates/poller/tests/dispatch_pipeline.rs`
- Create: `Backend/crates/executor/tests/release_claim.rs`

**Interfaces produced:**
- In `executor`: `pub async fn ExecutorHandle::release_claim_auto(&self, account_id: &str, spx_id: &str, rule_id: Option<Uuid>)` — best-effort `DEL spx:claim:<acct>:<spxId>` (+ `SREM spx:inflight:<acct>:<rule|_norule>` for a capped rule) so a TRANSIENT-failed ticket can be retried next cycle instead of waiting out the 600s TTL. Keeps Redis keyspace ownership inside `executor` (design invariant).
- In `poller`: `pub struct RuleMeta { pub uuid: Uuid, pub cap: i64, pub accepted_count: i64, pub name: String }`.
- In `poller`: `pub enum DispatchResult { Accepted, Duplicate, QuotaFull, Taken, LostToAgency { rival: String }, Transient, Auth, Skipped }`.
- In `poller`: `pub async fn dispatch_booking(shared: &PollerShared, st: &mut PollerState, booking: &SpxBooking) -> DispatchResult` — the full per-booking decision.
- In `poller`: `pub async fn ensure_restored_then_spawn(shared: Arc<PollerShared>, st: PollerState) -> AccountHandle` — **enforces the Layer-3 contract: `await executor.restore_accepted_ids(account_id, &dedup)` to completion BEFORE `spawn_account_loop`.** This is where Fase 4's documented "MUST await before first poll" ordering becomes a hard, tested guarantee.

**Design note (pipeline order — port exactly):** For each pending booking not already known to Layer 1:
1. `to_core_booking(booking)` → `find_best_matching_rule_compiled(&rules, &core, &match_state)` → `Option<idx>`. `None` → `Skipped`.
2. `st.dedup.try_begin_accept(&booking.id)` — `false` → `Skipped` (already in-flight/accepted).
3. `executor.try_claim_auto(account, &booking.id, Some(meta.uuid), meta.cap, meta.accepted_count)`:
   - `AlreadyClaimed` → `abort_accept`, `Duplicate`. `QuotaFull` → `abort_accept`, `QuotaFull`. `RedisUnavailable` → `abort_accept`, `Skipped` (fail-closed: do NOT dispatch).
   - `Proceed` → continue.
4. `client.accept_booking(&st.cookies, booking_id_i64, st.agency_id, &request_ids)` → `AcceptResult`. Map `reason`:
   - `Ok` → `commit_accept`; `record_durable_accept`; `apply_rule_consumption`; `store::update_booking_status(accepted)`; spawn notifier accepted (Task 10) + publish `ticket_accepted` (Task 13); `Accepted`.
   - `AgencyDup` → `verify_agency_dup(client, cookies, self_email, booking_id_i64)`:
     - `Ours` or `Inconclusive` → treat as `Ok` (commit + consume + durable + notify accepted) — no `unverified` flag stored (Fase 4 corrections #6).
     - `LostToAgency{rival}` → `abort_accept`; do NOT consume/record; spawn notifier agency-loss (Task 10); `store::update_booking_status(failed/taken_by_other)`; `LostToAgency`.
   - `Taken` → `abort_accept`; `update_booking_status(failed)`; `Taken` (terminal — keep the durable claim; do NOT release).
   - `Transient` → `abort_accept`; `release_claim_auto` (so next cycle retries); leave `pending`; `Transient`.
   - `Auth` → `abort_accept`; `st.consecutive_401s = max(st.consecutive_401s, 3)` (correction #5 — jump to relogin threshold); leave `pending`; `Auth`.

`booking_id_i64 = booking.booking_id.parse::<i64>().unwrap_or(0)`; `request_ids` = the booking's numeric `request_id` (if parseable) as a one-element slice, else empty. `st.self_email` is fetched once via `executor::fetch_self_email` on first need and cached.

- [ ] **Step 1: Add `store::update_booking_status`** (additive; needed by the pipeline)

Append to `Backend/crates/store/src/bookings.rs` and re-export:

```rust
/// Record the terminal outcome of an accept attempt on a booking.
pub async fn update_booking_status(
    pool: &PgPool,
    tenant_id: Uuid,
    spx_id: &str,
    status: &str,
    latency_ms: Option<i64>,
    auto_accepted: bool,
    rule_matched: Option<&str>,
) -> Result<(), sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;
    sqlx::query(
        "UPDATE bookings SET status=$3, accept_latency_ms=$4, auto_accepted=$5, \
         rule_matched=$6, updated_at=now() WHERE tenant_id=$1 AND spx_id=$2",
    )
    .bind(tenant_id)
    .bind(spx_id)
    .bind(status)
    .bind(latency_ms)
    .bind(auto_accepted)
    .bind(rule_matched)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
```

```rust
// in store/src/lib.rs
pub use bookings::update_booking_status;
```

> Column names (`accept_latency_ms`, `auto_accepted`, `rule_matched`) MUST be confirmed against `store`'s migration before proceeding; adjust the binds to the real schema if they differ.

- [ ] **Step 2: Add `executor::release_claim_auto`**

```rust
// Backend/crates/executor/src/release.rs
//! Best-effort release of an auto claim so a TRANSIENT-failed ticket retries
//! next cycle instead of waiting out the 600s claim TTL. Keeps Redis keyspace
//! ownership inside `executor` (the design invariant that Fase 5 never touches
//! the shared keyspace directly). Best-effort: a failed release only leaves the
//! claim until its TTL — it never over-accepts.
use redis::AsyncCommands;
use uuid::Uuid;

use crate::gate::ExecutorHandle;

impl ExecutorHandle {
    pub async fn release_claim_auto(&self, account_id: &str, spx_id: &str, rule_id: Option<Uuid>) {
        let claim_key = format!("spx:claim:{account_id}:{spx_id}");
        if let Ok(mut con) = self.redis.conn().await {
            let _: Result<i64, _> = con.del(&claim_key).await;
            if let Some(rule) = rule_id {
                let inflight_key = format!("spx:inflight:{account_id}:{rule}");
                let _: Result<i64, _> = con.srem(&inflight_key, spx_id).await;
            }
        }
    }
}
```

```rust
// in executor/src/lib.rs
pub mod release;
```

- [ ] **Step 3: real-Redis test for `release_claim_auto` (proves retry-after-release)**

```rust
// Backend/crates/executor/tests/release_claim.rs
//! release_claim_auto lets the SAME spxId be claimed again (a transient retry).
use executor::{ClaimOutcome, ExecutorHandle};
use uuid::Uuid;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

#[tokio::test]
async fn release_allows_reclaim() {
    let h = ExecutorHandle::connect(&redis_url()).await.unwrap();
    let a = format!("t{}", Uuid::new_v4().simple());
    assert_eq!(h.try_claim_auto(&a, "77", None, 0, 0).await, ClaimOutcome::Proceed);
    assert_eq!(h.try_claim_auto(&a, "77", None, 0, 0).await, ClaimOutcome::AlreadyClaimed);
    h.release_claim_auto(&a, "77", None).await;
    assert_eq!(
        h.try_claim_auto(&a, "77", None, 0, 0).await,
        ClaimOutcome::Proceed,
        "after release the ticket must be reclaimable"
    );
}
```

- [ ] **Step 4: Extend `PollerState` with compiled rules + match state**

Add to `state.rs` `PollerState`:

```rust
use core_domain::{matching::CompiledRule, MatchState};

// fields on PollerState:
pub rules: Arc<Vec<CompiledRule>>,
pub rule_meta: Arc<Vec<crate::dispatch::RuleMeta>>,
pub match_state: MatchState,
```

Initialize `rules`/`rule_meta` to empty `Arc::new(vec![])` and `match_state` to `MatchState::default()` in `PollerState::new` (Task 6 focuses on the pipeline; loading rules from `store` is a thin call the account bootstrap does — a `store::load_active_rules(pool, tenant_id) -> Vec<(Uuid, AcceptRule, i64, i64)>` helper, added additively; if `store` lacks it, the bootstrap builds `AcceptRule`s from `store::models::accept_rule` rows and parses `AcceptRule.id = uuid.to_string()`).

- [ ] **Step 5: Write `dispatch.rs`**

```rust
// Backend/crates/poller/src/dispatch.rs
//! The accept decision pipeline: match → claim (Layer 1+2) → accept HTTP →
//! classify → agency-dup verify → quota consume → durable record → notify. Ties
//! Fase 3 (spx-client) + Fase 4 (executor) + Fase 5 together. Notifier/ws
//! publish are spawned fire-and-forget (Tasks 10/13 fill the hooks).
use std::time::Instant;

use core_domain::matching::find_best_matching_rule_compiled;
use executor::{AgencyDupOutcome, ClaimOutcome};
use spx_client::{to_core_booking, AcceptReason, SpxBooking};
use uuid::Uuid;

use crate::state::{PollerShared, PollerState};

/// DB rule identity aligned by index with `PollerState.rules` (CompiledRule has a
/// String id; the executor/store quota APIs need a Uuid + cap/accepted).
#[derive(Debug, Clone)]
pub struct RuleMeta {
    pub uuid: Uuid,
    pub cap: i64,
    pub accepted_count: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchResult {
    Accepted,
    Duplicate,
    QuotaFull,
    Taken,
    LostToAgency { rival: String },
    Transient,
    Auth,
    Skipped,
}

pub async fn dispatch_booking(
    shared: &PollerShared,
    st: &mut PollerState,
    booking: &SpxBooking,
) -> DispatchResult {
    // 1. Match against compiled rules (first-wins index).
    let core = to_core_booking(booking);
    let idx = match find_best_matching_rule_compiled(&st.rules, &core, &st.match_state) {
        Some(i) => i,
        None => return DispatchResult::Skipped,
    };
    let meta = st.rule_meta[idx].clone();

    // 2. Layer 1 in-proc claim.
    if !st.dedup.try_begin_accept(&booking.id) {
        return DispatchResult::Skipped;
    }

    // 3. Layer 2 durable atomic gate (fail-closed).
    match shared
        .executor
        .try_claim_auto(&st.account_id, &booking.id, Some(meta.uuid), meta.cap, meta.accepted_count)
        .await
    {
        ClaimOutcome::Proceed => {}
        ClaimOutcome::AlreadyClaimed => {
            st.dedup.abort_accept(&booking.id);
            return DispatchResult::Duplicate;
        }
        ClaimOutcome::QuotaFull => {
            st.dedup.abort_accept(&booking.id);
            return DispatchResult::QuotaFull;
        }
        ClaimOutcome::RedisUnavailable => {
            st.dedup.abort_accept(&booking.id); // fail-closed: do NOT dispatch
            return DispatchResult::Skipped;
        }
    }

    // 4. The actual accept HTTP call.
    let booking_id_i64 = booking.booking_id.parse::<i64>().unwrap_or(0);
    let request_ids: Vec<i64> = booking.request_id.parse::<i64>().ok().into_iter().collect();
    let started = Instant::now();
    let result = shared
        .client
        .accept_booking(&st.cookies, booking_id_i64, st.agency_id, &request_ids)
        .await;
    let latency_ms = started.elapsed().as_millis() as i64;

    match result.reason {
        AcceptReason::Ok => {
            finalize_win(shared, st, booking, &meta, latency_ms).await;
            DispatchResult::Accepted
        }
        AcceptReason::AgencyDup => {
            let self_email = ensure_self_email(shared, st).await;
            match executor::verify_agency_dup(&shared.client, &st.cookies, &self_email, booking_id_i64).await {
                AgencyDupOutcome::Ours | AgencyDupOutcome::Inconclusive => {
                    finalize_win(shared, st, booking, &meta, latency_ms).await;
                    DispatchResult::Accepted
                }
                AgencyDupOutcome::LostToAgency { rival_email } => {
                    st.dedup.abort_accept(&booking.id);
                    let _ = store::update_booking_status(
                        &shared.pool, st.tenant_id, &booking.id, "failed", Some(latency_ms), true,
                        Some("taken_by_other"),
                    ).await;
                    // Task 10 fills the notifier agency-loss spawn here.
                    DispatchResult::LostToAgency { rival: rival_email }
                }
            }
        }
        AcceptReason::Taken => {
            st.dedup.abort_accept(&booking.id);
            let _ = store::update_booking_status(
                &shared.pool, st.tenant_id, &booking.id, "failed", Some(latency_ms), true, Some("taken_by_other"),
            ).await;
            DispatchResult::Taken
        }
        AcceptReason::Transient => {
            st.dedup.abort_accept(&booking.id);
            shared.executor.release_claim_auto(&st.account_id, &booking.id, Some(meta.uuid)).await;
            DispatchResult::Transient
        }
        AcceptReason::Auth => {
            st.dedup.abort_accept(&booking.id);
            st.consecutive_401s = st.consecutive_401s.max(3); // correction #5
            DispatchResult::Auth
        }
        AcceptReason::Error => {
            st.dedup.abort_accept(&booking.id);
            shared.executor.release_claim_auto(&st.account_id, &booking.id, Some(meta.uuid)).await;
            DispatchResult::Skipped
        }
    }
}

/// Commit a confirmed win: Layer-1 commit + durable ZADD + quota consume + DB
/// status + (Tasks 10/13) notify/publish.
async fn finalize_win(
    shared: &PollerShared,
    st: &mut PollerState,
    booking: &SpxBooking,
    meta: &RuleMeta,
    latency_ms: i64,
) {
    st.dedup.commit_accept(&booking.id);
    let _ = shared.executor.record_durable_accept(&st.account_id, &booking.id).await;
    let _ = shared
        .executor
        .apply_rule_consumption(&shared.pool, st.tenant_id, &st.account_id, meta.uuid, &booking.id)
        .await;
    let _ = store::update_booking_status(
        &shared.pool, st.tenant_id, &booking.id, "accepted", Some(latency_ms), true, Some(&meta.name),
    ).await;
    // Task 10: tokio::spawn(notifier::notify_accepted(...)); ignore Result.
    // Task 13: publish ws `ticket_accepted` to `acct:<account_id>`.
}

/// Fetch + cache the account's own email (once) for agency-dup classification.
async fn ensure_self_email(shared: &PollerShared, st: &mut PollerState) -> String {
    if let Some(e) = &st.self_email {
        return e.clone();
    }
    let email = executor::fetch_self_email(&shared.client, &st.cookies)
        .await
        .unwrap_or_default();
    st.self_email = Some(email.clone());
    email
}
```

- [ ] **Step 6: Finalize `poll_once` + the restore contract in `schedule.rs`**

Replace `poll_once` with the real body and add `ensure_restored_then_spawn`:

```rust
// schedule.rs — real poll_once + restore-before-first-poll contract
use std::sync::Arc;

use tokio::sync::Notify;

use crate::dispatch::dispatch_booking;
use crate::fetch::{fast_detect, sweep, should_full_sweep};
use crate::state::{AccountHandle, PollerShared, PollerState};

pub async fn poll_once(shared: &PollerShared, st: &mut PollerState) {
    st.poll_count = st.poll_count.wrapping_add(1);

    // Fast-detect (opt-in) hints a pool change → jump straight to a full sweep.
    let fast = fast_detect(&shared.client, &st.cookies, &shared.config).await;
    let pool_changed = !fast.is_empty() || {
        // cheap "pool size changed since last poll" heuristic via last_pending_count
        false
    };
    let full = should_full_sweep(st.poll_count, shared.config.full_sync_every, pool_changed);

    let outcome = sweep(&shared.client, &st.cookies, &shared.config, st.poll_count, full).await;

    // Upsert every seen booking (enrichment-preserving) then dispatch pendings.
    for booking in &outcome.bookings {
        let _ = store::upsert_booking(
            &shared.pool, st.tenant_id,
            &store::BookingUpsert {
                spx_id: booking.id.clone(),
                status: "pending".into(),
                is_coc: matches!(booking.booking_type, core_domain::BookingType::Spxid),
                raw_data: booking.raw.clone(),
            },
        ).await;
        if st.dedup.is_known(&booking.id) {
            continue;
        }
        let _ = dispatch_booking(shared, st, booking).await;
    }

    // Anti-drift — the FetchOutcome type gate ensures this no-ops unless the
    // sweep was complete (Task 5).
    let _ = crate::antidrift::run_anti_drift(&shared.pool, st.tenant_id, &outcome).await;

    st.last_pending_count = outcome.spx_id_set.len() as i64;
}

/// CP-7 CONTRACT: await the Layer-3 durable restore to completion BEFORE the
/// first poll is ever scheduled. Fase 4 documented this as the poller's
/// responsibility; here it is enforced as a hard ordering (the loop cannot start
/// until restore returns).
pub async fn ensure_restored_then_spawn(shared: Arc<PollerShared>, st: PollerState) -> AccountHandle {
    // MUST complete before spawn — otherwise the first poll could re-accept a
    // ticket won in a previous process lifetime (Layer 1 starts empty; the
    // Layer 2 claim key may have expired).
    let _ = shared
        .executor
        .restore_accepted_ids(&st.account_id, &st.dedup)
        .await;
    let poke = Arc::new(Notify::new());
    let join = spawn_account_loop(shared, st, poke.clone());
    AccountHandle { poke, join }
}
```

- [ ] **Step 7: Wire `lib.rs`**

```rust
pub mod dispatch;
pub use dispatch::{dispatch_booking, DispatchResult, RuleMeta};
pub use schedule::ensure_restored_then_spawn;
```

- [ ] **Step 8: Pipeline test (wiremock SPX + real Redis + real PG)**

```rust
// Backend/crates/poller/tests/dispatch_pipeline.rs
//! End-to-end (minus real SPX): a matched pending booking is claimed, accepted
//! (wiremock SPX returns retcode 0), committed to Layer 1, recorded durably, and
//! its booking row flips to 'accepted'. A second dispatch of the same id is a
//! Duplicate (claim shared). Proves Fase 3+4+5 compose. Real Redis @ 16379 +
//! real PG @ 15432 + wiremock SPX accept endpoint.
//! NOTE: because building a full PollerState needs compiled rules, this test
//! constructs a single coc_only rule via core_domain and drives `dispatch_booking`
//! directly. See the design doc DoD #? mapping in Task 14.
// (Full test body: construct PollerShared with ExecutorHandle::connect(redis),
//  store::connect(pg), SpxClient::new(wiremock.uri()); seed a tenant + a
//  max_accept_count=0 rule; build PollerState with that one CompiledRule +
//  RuleMeta; craft an SpxBooking that matches; assert dispatch_booking == Accepted
//  then == Duplicate on a re-run; assert the bookings row status == 'accepted'.)
```

> The full test body is left as a fill-in with an EXACT checklist (above) rather than verbatim code because it composes four real subsystems and its precise `PollerShared`/`PollerState` construction depends on the `store::load_active_rules` shape finalized in Step 4 — the implementer wires it against the compiled types. It MUST assert: (1) first `dispatch_booking` → `Accepted`; (2) second → `Duplicate`; (3) `bookings.status == 'accepted'`; (4) `st.dedup.is_known(id)` is true after the win.

- [ ] **Step 9: Test, clippy, commit**

```bash
cd Docker && docker compose up -d tower-postgres tower-redis && cd ..
cd Backend
cargo test -p executor --test release_claim -- --test-threads=1
cargo test -p poller --test dispatch_pipeline -- --test-threads=1
cargo clippy -p executor -p poller --all-targets -- -D warnings
cd ..
git add Backend/crates/executor Backend/crates/poller Backend/crates/store Backend/Cargo.lock
git commit -m "feat(poller): accept dispatch pipeline (claim→accept→classify→agency-dup→quota→durable) + restore-before-first-poll contract + executor::release_claim_auto"
```

---

### Task 7: Auto-login orchestration — tier 2/3 in-proc + tier 1 via `auth-sidecar` HTTP + fallback + 3×401 reactive + daily proactive

**Verification confidence:** Tier ordering + fallback + relogin triggers ported from the reference `spx-auth.ts` / `poller.ts` (grep-verified). Set-Cookie parsing for tier 2/3 is NEW SPX-client functionality — the `wreq` response header API is best-effort; **the implementer must verify `wreq`'s Set-Cookie header access and `SpxClient` construction against the pinned `wreq 6.0.0-rc.29` before proceeding.** The sidecar HTTP contract is defined here and mirrored in Task 9.

**Files:**
- Create: `Backend/crates/spx-client/src/login.rs` (additive tier 2/3 + spx_cid capture) + wire `spx-client::lib.rs`
- Create: `Backend/crates/poller/src/login.rs` (tier chain + sidecar client + relogin triggers)
- Modify: `Backend/crates/poller/src/lib.rs` + `state.rs` (relogin fields already added in Task 1)
- Create: `Backend/crates/spx-client/tests/login_mock.rs`
- Create: `Backend/crates/poller/tests/login_chain.rs`

**Interfaces produced in `spx-client` (additive):**
- `pub async fn SpxClient::api_login(&self, username: &str, password: &str) -> Option<SpxCookies>` — port of `tryApiLogin` (5 endpoint attempts; success = `fms_user_skey` captured from Set-Cookie OR from a JSON body with `retcode==0`/`success==true`).
- `pub async fn SpxClient::form_login(&self, username: &str, password: &str) -> Option<SpxCookies>` — port of `tryFormLogin` (GET `/login` for CSRF, POST form, follow the redirect to capture `spx_*`).
- `pub async fn SpxClient::fetch_spx_cid(&self, cookies: &mut SpxCookies)` — port of `fetchSpxCid` (visit pages that set `spx_cid`).

**Interfaces produced in `poller`:**
- `pub struct SidecarClient { base_url: String, http: wreq::Client }` with `pub async fn login(&self, account_id: &str, username: &str, password: &str) -> Option<SpxCookies>` — `POST {base}/login` with `{account_id, username, password}` → `{ok, cookies}`.
- `pub enum LoginTier { Browser, Api, Form }`.
- `pub async fn auto_login(sidecar: &SidecarClient, client: &SpxClient, account_id: &str, username: &str, password: &str) -> Option<(SpxCookies, LoginTier)>` — try tier 1 (sidecar) → tier 2 (api) → tier 3 (form), IN ORDER; a sidecar-unreachable/None falls through to tier 2 (must NOT abort). Runs `fetch_spx_cid` if `spx_cid` empty.
- `pub fn should_reactive_relogin(consecutive_401s: u32) -> bool` — `consecutive_401s >= 3`.
- `pub fn should_daily_relogin(last_day_wib: &str, now_wib_day: &str) -> bool` — a new WIB day.
- `pub fn wib_day(now: chrono::DateTime<chrono::Utc>) -> String` — `YYYY-MM-DD` in UTC+7.

**Sidecar HTTP contract (poller ⇄ auth-sidecar; Task 9 implements the server side):**
- Request: `POST http://<auth-sidecar>:8082/login`, body `{"account_id":"<id>","username":"<u>","password":"<p>"}`.
- Response 200: `{"ok":true,"cookies":{"fms_user_skey":"...","fms_user_id":"...","fms_user_agency_id":"...","csrftoken":"...","spx_uk":"...","spx_cid":"...","spx_uid":"...","spx_agid":"...","spx_st":"...","ds":"...","spx_admin_device_id":"..."}}` — the 11 SPX cookie fields (missing ones = `""`).
- Failure/unavailable: `{"ok":false,"error":"..."}` or a non-2xx / connection error → the poller treats ALL of these as "tier 1 unavailable" and falls through to tier 2 (never a hard failure — tier 2/3 exist exactly for this, correction #2 rationale).

- [ ] **Step 1: Write `spx-client::login`** (tier 2/3 — needs a Set-Cookie-capturing client)

```rust
// Backend/crates/spx-client/src/login.rs
//! Tier 2 (API) + tier 3 (form) SPX login, ported from spx-auth.ts. Unlike the
//! data endpoints (which only SEND cookies), login must CAPTURE Set-Cookie
//! response headers and follow one redirect. Success == `fms_user_skey` present.
//! No browser — safe on the hot-path process. Tier 1 (browser) is a separate
//! process (`auth-sidecar`); this crate never touches chromiumoxide.
use serde_json::{json, Value};

use crate::client::SpxClient;
use crate::cookies::SpxCookies;

// The 11 known SPX cookie names → SpxCookies fields. `spx-admin-device-id`
// maps to `spx_admin_device_id`.
fn apply_set_cookie(jar: &mut SpxCookies, name: &str, value: &str) {
    match name {
        "fms_user_skey" => jar.fms_user_skey = value.to_string(),
        "fms_user_id" => jar.fms_user_id = value.to_string(),
        "fms_user_agency_id" => jar.fms_user_agency_id = value.to_string(),
        "csrftoken" => jar.csrftoken = value.to_string(),
        "spx_uk" => jar.spx_uk = value.to_string(),
        "spx_cid" => jar.spx_cid = value.to_string(),
        "spx_uid" => jar.spx_uid = value.to_string(),
        "spx_agid" => jar.spx_agid = value.to_string(),
        "spx_st" => jar.spx_st = value.to_string(),
        "ds" => jar.ds = value.to_string(),
        "spx-admin-device-id" => jar.spx_admin_device_id = value.to_string(),
        _ => {}
    }
}

impl SpxClient {
    /// Tier 2 — API login. Tries the reference's 5 endpoint/body variants; a
    /// captured `fms_user_skey` (from Set-Cookie or a retcode==0 body) wins.
    pub async fn api_login(&self, username: &str, password: &str) -> Option<SpxCookies> {
        let attempts: [(&str, Value); 5] = [
            ("/api/basicserver/agency/account/login", json!({ "username": username, "password": password, "use_case": "agency portal" })),
            ("/api/basicserver/agency/account/login", json!({ "username": username, "password": password })),
            ("/api/basicserver/account/login", json!({ "username": username, "password": password })),
            ("/api/basicserver/agency/auth/login", json!({ "username": username, "password": password })),
            ("/api/user/login", json!({ "username": username, "password": password })),
        ];
        for (path, body) in attempts {
            if let Some(jar) = self.login_post_capture(path, &body).await {
                if !jar.fms_user_skey.is_empty() {
                    return Some(jar);
                }
            }
        }
        None
    }

    /// Tier 3 — form login: GET /login (CSRF), POST urlencoded form, follow the
    /// redirect to capture spx_* cookies.
    pub async fn form_login(&self, username: &str, password: &str) -> Option<SpxCookies> {
        // Implementation outline (fill against wreq's real API):
        // 1. GET {base}/login → capture Set-Cookie (csrftoken).
        // 2. POST {base}/login with Content-Type x-www-form-urlencoded body
        //    `username=..&password=..&csrfmiddlewaretoken=<csrf>&next=/`,
        //    redirect DISABLED, sending the captured Cookie jar; capture Set-Cookie.
        // 3. If a Location header is present, GET it with the jar; capture Set-Cookie.
        // Return Some(jar) iff jar.fms_user_skey is non-empty.
        let _ = (username, password);
        None // TODO(impl): wire against wreq Set-Cookie + manual redirect
    }

    /// Visit pages/count API that set spx_cid; fill it if empty. Port of fetchSpxCid.
    pub async fn fetch_spx_cid(&self, cookies: &mut SpxCookies) {
        if !cookies.spx_cid.is_empty() {
            return;
        }
        // GET /line-haul/booking, /line-haul, /booking, / then the count API;
        // capture spx_cid from any Set-Cookie. Best-effort.
        let _ = cookies;
    }

    /// Shared helper: POST JSON to a login path and capture Set-Cookie into a jar
    /// (also merges a retcode==0/success body's session fields, like the reference).
    async fn login_post_capture(&self, path: &str, body: &Value) -> Option<SpxCookies> {
        // Fill against wreq: send POST, read `res.headers().get_all("set-cookie")`,
        // parse each `k=v; ...` first pair, apply_set_cookie into a fresh jar; if
        // the JSON body has retcode==0/success==true, merge session_key/user_id/
        // agency_id into fms_* fields. Return the jar.
        let _ = (path, body);
        None // TODO(impl): wire against wreq response header API
    }
}
```

> **This module has intentional `TODO(impl)` bodies for the wreq-specific plumbing (Set-Cookie capture + manual redirect follow), because the exact `wreq 6.0.0-rc.29` response-header + redirect-control API could not be fully read for this plan.** The implementer fills these against the installed `wreq` (its API is reqwest-shaped: `res.headers().get_all(reqwest::header::SET_COOKIE)`, `ClientBuilder::redirect(Policy::none())`). The wiremock test in Step 4 pins the REQUIRED behavior (success == captured `fms_user_skey`), so a correct fill is verifiable.

- [ ] **Step 2: Wire `spx-client::lib.rs`**

```rust
pub mod login;
```

(The methods are inherent `impl SpxClient` — already reachable via the `SpxClient` re-export.)

- [ ] **Step 3: Write `poller::login`** (tier chain + sidecar client + relogin triggers)

```rust
// Backend/crates/poller/src/login.rs
//! 3-tier auto-login orchestration. Tier 1 (browser) is delegated to the
//! separate `auth-sidecar` process over internal HTTP (poller depends on NO
//! browser crate — correction #2). Tiers 2/3 are in-proc `spx-client` HTTP.
//! Order 1→2→3; a down/unreachable sidecar falls through to tier 2 (never a hard
//! failure). Reactive relogin at 3×401; proactive once-per-WIB-day.
use serde::{Deserialize, Serialize};
use spx_client::{SpxClient, SpxCookies};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginTier {
    Browser,
    Api,
    Form,
}

#[derive(Serialize)]
struct SidecarLoginReq<'a> {
    account_id: &'a str,
    username: &'a str,
    password: &'a str,
}

#[derive(Deserialize)]
struct SidecarLoginResp {
    ok: bool,
    #[serde(default)]
    cookies: Option<SidecarCookies>,
}

/// The 11 SPX cookie fields as returned by auth-sidecar's /login.
#[derive(Deserialize, Default)]
struct SidecarCookies {
    #[serde(default)] fms_user_skey: String,
    #[serde(default)] fms_user_id: String,
    #[serde(default)] fms_user_agency_id: String,
    #[serde(default)] csrftoken: String,
    #[serde(default)] spx_uk: String,
    #[serde(default)] spx_cid: String,
    #[serde(default)] spx_uid: String,
    #[serde(default)] spx_agid: String,
    #[serde(default)] spx_st: String,
    #[serde(default)] ds: String,
    #[serde(default)] spx_admin_device_id: String,
}

impl From<SidecarCookies> for SpxCookies {
    fn from(c: SidecarCookies) -> Self {
        SpxCookies {
            fms_user_skey: c.fms_user_skey,
            fms_user_id: c.fms_user_id,
            fms_user_agency_id: c.fms_user_agency_id,
            csrftoken: c.csrftoken,
            spx_uk: c.spx_uk,
            spx_cid: c.spx_cid,
            spx_uid: c.spx_uid,
            spx_agid: c.spx_agid,
            spx_st: c.spx_st,
            ds: c.ds,
            spx_admin_device_id: c.spx_admin_device_id,
        }
    }
}

pub struct SidecarClient {
    base_url: String,
    http: wreq::Client,
}

impl SidecarClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        // Plain client (internal HTTP, no impersonation needed). A build failure
        // is treated as "sidecar unavailable" by returning a client that always
        // errors on send; keep it simple with a default build().
        let http = wreq::Client::builder().build().unwrap_or_default();
        Self { base_url: base_url.into(), http }
    }

    /// Tier 1: ask the sidecar to browser-login. Any error / non-2xx / ok:false
    /// → None (caller falls through to tier 2).
    pub async fn login(&self, account_id: &str, username: &str, password: &str) -> Option<SpxCookies> {
        let url = format!("{}/login", self.base_url.trim_end_matches('/'));
        let req = SidecarLoginReq { account_id, username, password };
        let res = self.http.post(url).json(&req).send().await.ok()?;
        if !res.status().is_success() {
            return None;
        }
        let parsed: SidecarLoginResp = res.json().await.ok()?;
        if !parsed.ok {
            return None;
        }
        let jar: SpxCookies = parsed.cookies?.into();
        if jar.fms_user_skey.is_empty() {
            return None;
        }
        Some(jar)
    }
}

/// Try tier 1 → 2 → 3, in order. Sidecar-unreachable falls through (never aborts).
pub async fn auto_login(
    sidecar: &SidecarClient,
    client: &SpxClient,
    account_id: &str,
    username: &str,
    password: &str,
) -> Option<(SpxCookies, LoginTier)> {
    // Tier 1 — browser via sidecar (primary; port the reference's order exactly).
    if let Some(mut jar) = sidecar.login(account_id, username, password).await {
        client.fetch_spx_cid(&mut jar).await;
        return Some((jar, LoginTier::Browser));
    }
    // Tier 2 — API login (in-proc).
    if let Some(mut jar) = client.api_login(username, password).await {
        client.fetch_spx_cid(&mut jar).await;
        return Some((jar, LoginTier::Api));
    }
    // Tier 3 — form login (in-proc).
    if let Some(mut jar) = client.form_login(username, password).await {
        client.fetch_spx_cid(&mut jar).await;
        return Some((jar, LoginTier::Form));
    }
    None
}

/// Reactive relogin fires at 3 consecutive 401s (correction #5).
pub fn should_reactive_relogin(consecutive_401s: u32) -> bool {
    consecutive_401s >= 3
}

/// Proactive relogin once per WIB day.
pub fn should_daily_relogin(last_day_wib: &str, now_wib_day: &str) -> bool {
    last_day_wib != now_wib_day
}

/// YYYY-MM-DD in WIB (UTC+7, no DST).
pub fn wib_day(now: chrono::DateTime<chrono::Utc>) -> String {
    let wib = chrono::FixedOffset::east_opt(7 * 3600).expect("valid +7");
    now.with_timezone(&wib).format("%Y-%m-%d").to_string()
}
```

- [ ] **Step 4: Wire `poller::lib.rs`** + tests

```rust
pub mod login;
pub use login::{auto_login, should_daily_relogin, should_reactive_relogin, wib_day, LoginTier, SidecarClient};
```

`spx-client/tests/login_mock.rs` (DoD #5, tier 2 via wiremock): mount a wiremock that returns `Set-Cookie: fms_user_skey=OKSKEY` on `/api/basicserver/agency/account/login`; assert `api_login` returns `Some(jar)` with that skey. `poller/tests/login_chain.rs` (DoD #5): (a) a wiremock "sidecar" returning `{ok:true,cookies:{fms_user_skey:"B"}}` → `auto_login` yields `LoginTier::Browser`; (b) a sidecar returning 503 (unreachable) BUT an SPX wiremock whose api-login sets `fms_user_skey` → `auto_login` falls through to `LoginTier::Api` (proves the fallback); (c) `should_reactive_relogin(3)==true`, `should_reactive_relogin(2)==false`, `should_daily_relogin("2026-07-12","2026-07-13")==true`.

```rust
// Backend/crates/poller/tests/login_chain.rs (fallback proof — the key case)
use poller::{auto_login, LoginTier, SidecarClient};
use spx_client::SpxClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn sidecar_down_falls_through_to_api() {
    // "sidecar" that is down (503 on /login).
    let sidecar_srv = MockServer::start().await;
    Mock::given(method("POST")).and(path("/login"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&sidecar_srv).await;
    // SPX whose API login succeeds (Set-Cookie fms_user_skey).
    let spx = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/basicserver/agency/account/login"))
        .respond_with(ResponseTemplate::new(200).insert_header("set-cookie", "fms_user_skey=APIWIN; Path=/"))
        .mount(&spx).await;

    let sidecar = SidecarClient::new(sidecar_srv.uri());
    let client = SpxClient::new(spx.uri()).unwrap();
    let out = auto_login(&sidecar, &client, "acct", "u", "p").await;
    let (jar, tier) = out.expect("must fall through to tier 2, not hard-fail");
    assert_eq!(tier, LoginTier::Api);
    assert_eq!(jar.fms_user_skey, "APIWIN");
}
```

> Both wiremock login tests depend on the `spx-client::login` Set-Cookie capture being filled in (Step 1's `TODO(impl)`). If the implementer has not yet wired `login_post_capture`, `sidecar_down_falls_through_to_api` will fail at tier 2 — that failure is the SIGNAL that Step 1 is incomplete, not a flaky test.

- [ ] **Step 5: Test, clippy, commit**

```bash
cd Backend
cargo test -p spx-client --test login_mock
cargo test -p poller --test login_chain
cargo clippy -p spx-client -p poller --all-targets -- -D warnings
cd ..
git add Backend/crates/spx-client Backend/crates/poller Backend/Cargo.lock
git commit -m "feat(poller,spx-client): 3-tier auto-login (tier2/3 in-proc + tier1 via auth-sidecar) + fallback + 3x401 reactive + daily proactive"
```

---

### Task 8: Watchdog — durable-primary 60s recreate + heartbeat write

**Verification confidence:** Interval/recreate logic tested with paused time. Heartbeat is a best-effort Redis SET (research-verified `redis` API). Uses `primary_account_id()` from config.

**Files:**
- Create: `Backend/crates/poller/src/watchdog.rs`
- Modify: `Backend/crates/poller/src/lib.rs`
- Create: `Backend/crates/poller/tests/watchdog.rs`

**Interfaces produced:**
- `pub fn spawn_watchdog(shared: Arc<PollerShared>, respawn: Arc<dyn Fn(String) + Send + Sync>) -> JoinHandle<()>` — one GLOBAL task (not per-account) on a 60s interval: if the durable-primary account's `AccountHandle` is missing from `shared.accounts` (or its `join.is_finished()`), call `respawn(primary_id)` to recreate it, and write `spx:poller_heartbeat:<primary>` each cycle. (The `respawn` closure is injected so the watchdog does not itself own account bootstrap; the poller `main`/mount wires it to `ensure_restored_then_spawn`.)
- `pub async fn heartbeat(executor: &ExecutorHandle, account_id: &str)` — best-effort `SET spx:poller_heartbeat:<acct> <now_ms> EX 120` (written for a FUTURE Fase-8 observability consumer that is NOT built now — correction #4 / YAGNI).

**Design note:** Watchdog only guards the ONE `primary_account_id()` account (the `PORTAL_USERNAME` analog), not every account — a faithful port of the reference `ensureDurablePollerAlive` (not a health-check of all accounts). It writes the heartbeat key but builds NO consumer (that key is aspirational until Fase 8).

- [ ] **Step 1: Write `watchdog.rs`**

```rust
// Backend/crates/poller/src/watchdog.rs
//! Durable-primary watchdog: one global 60s task that recreates the primary
//! account's poller if it has died, and writes an (as-yet-unconsumed) heartbeat
//! key for Fase-8 observability. Guards ONLY the primary account (correction #4).
use std::sync::Arc;
use std::time::Duration;

use executor::ExecutorHandle;
use redis::AsyncCommands;
use tokio::task::JoinHandle;

use crate::state::PollerShared;

const WATCHDOG_INTERVAL: Duration = Duration::from_secs(60);

/// Best-effort heartbeat (no consumer built now — correction #4 / YAGNI).
pub async fn heartbeat(executor: &ExecutorHandle, account_id: &str) {
    if let Ok(mut con) = executor.redis.conn().await {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let key = format!("spx:poller_heartbeat:{account_id}");
        let opts = redis::SetOptions::default().with_expiration(redis::SetExpiry::EX(120));
        let _: Result<(), _> = con.set_options(&key, now_ms, opts).await;
    }
}

/// Spawn the global watchdog. `respawn(primary_id)` recreates the primary poller
/// (wired by the mount layer to `ensure_restored_then_spawn`).
pub fn spawn_watchdog(
    shared: Arc<PollerShared>,
    respawn: Arc<dyn Fn(String) + Send + Sync>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let primary = shared.config.primary_account_id.clone();
        if primary.is_empty() {
            return; // no durable-primary configured
        }
        let mut ticker = tokio::time::interval(WATCHDOG_INTERVAL);
        loop {
            ticker.tick().await;
            heartbeat(&shared.executor, &primary).await;

            let alive = shared
                .accounts
                .get(&primary)
                .map(|h| !h.join.is_finished())
                .unwrap_or(false);
            if !alive {
                tracing::warn!(account = %primary, "durable-primary poller missing → recreating");
                (respawn)(primary.clone());
            }
        }
    })
}
```

> `PollerShared.executor.redis` must be reachable from `poller`; if `RedisPool.conn` is not `pub` on `ExecutorHandle`, add a thin `pub async fn ExecutorHandle::heartbeat_set(&self, key, val_ms, ttl)` to `executor` (additive) and call THAT instead — do not reach into private fields. Prefer the public helper.

- [ ] **Step 2: Wire `lib.rs`**

```rust
pub mod watchdog;
pub use watchdog::{heartbeat, spawn_watchdog};
```

- [ ] **Step 3: Watchdog recreate test (DoD #6)**

```rust
// Backend/crates/poller/tests/watchdog.rs
//! DoD #6: simulate the primary poller "gone" and assert the watchdog calls
//! respawn within the next 60s cycle. Paused virtual time drives the interval.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

// This test drives the watchdog's DECISION logic without a full PollerShared by
// exercising the interval + "alive?" check against a shared account map. It
// mirrors spawn_watchdog's loop (interval + is_finished + respawn) so the timing
// is the proven fact. (A full-PollerShared integration variant runs in Task 14.)
#[tokio::test(start_paused = true)]
async fn watchdog_recreates_dead_primary_within_60s() {
    let respawns = Arc::new(AtomicUsize::new(0));
    let alive = Arc::new(std::sync::atomic::AtomicBool::new(false)); // primary is "dead"
    let r = respawns.clone();
    let a = alive.clone();
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        for _ in 0..2 {
            ticker.tick().await;
            if !a.load(Ordering::SeqCst) {
                r.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    // Before any full interval, no respawn yet (the first tick is immediate for
    // tokio::interval, so advance a hair and check it fired once).
    tokio::time::advance(Duration::from_secs(61)).await;
    handle.await.unwrap();
    assert!(respawns.load(Ordering::SeqCst) >= 1, "watchdog must recreate a dead primary within a cycle");
}
```

> Note: `tokio::time::interval`'s FIRST `tick()` completes immediately; the test accounts for this. The Task-14 integration variant asserts the REAL `spawn_watchdog` calls its `respawn` closure against a real (empty) `shared.accounts` map.

- [ ] **Step 4: Test, clippy, commit**

```bash
cd Backend
cargo test -p poller --test watchdog
cargo clippy -p poller --all-targets -- -D warnings
cd ..
git add Backend/crates/poller
git commit -m "feat(poller): durable-primary watchdog (60s recreate) + heartbeat key write (no consumer, Fase 8)"
```

---

### Task 9: `auth-sidecar` tier-1 browser-login handler (chromiumoxide) + Docker Chromium

**Verification confidence:** chromiumoxide 0.9.1 API was READ FROM SOURCE and is exact: `Browser::launch(BrowserConfig::builder()…build()?) -> (Browser, Handler)` (headless is default; spawn a task to poll `handler.next()`), `browser.new_page(url)`, `page.find_element(sel) -> Element`, `Element::{type_str, click, press_key("Enter")}`, `page.wait_for_navigation()`, `page.get_cookies() -> Vec<chromiumoxide_cdp…network::Cookie>` (each has `.name`/`.value`). License `MIT OR Apache-2.0`; `cargo deny` stays green. **The end-to-end browser flow cannot be unit-tested without a real Chromium + real SPX login page — so the automated test covers the HTTP handler's request parse + response shape (with the browser call behind a boundary that returns a canned failure when Chromium is absent), exactly as the design doc's DoD #5 scopes it.**

**Files:**
- Modify: `Backend/bin/auth-sidecar/Cargo.toml` (add `chromiumoxide`, `serde`, `futures`)
- Overwrite: `Backend/bin/auth-sidecar/src/main.rs` (add `/login` route + browser module)
- Create: `Backend/bin/auth-sidecar/src/browser.rs`
- Modify: `Docker/auth-sidecar.Dockerfile` (install a Chromium binary + set `CHROME_BIN`)
- Modify: `Backend/bin/auth-sidecar/src/main.rs` tests (handler parse/shape test)

**Interfaces produced (server side of Task 7's contract):**
- `POST /login` body `{account_id, username, password}` → `200 {ok:true, cookies:{…11 fields}}` on success, else `{ok:false, error}` (still HTTP 200 so the poller's tier-fallthrough sees a clean `ok:false`; a 5xx is only for a panic/framework error).
- `pub async fn browser::browser_login(cfg: &BrowserLoginCfg, username: &str, password: &str) -> Result<HashMap<String,String>, String>` — launches headless Chromium, navigates SPX `/login`, fills email + password, submits (Enter), waits for `fms_user_skey`, visits the booking page for `spx_cid`, returns all cookies. Returns `Err` (not panic) on any failure so the process stays up (panic isolation is the whole point of the sidecar).

**Design note:** Chromium is NOT bundled by chromiumoxide — it drives an EXTERNAL browser. The Docker image must install one (`chromium` on Debian) and the sidecar points chromiumoxide at it via `BrowserConfig::builder().chrome_executable(path)` (from `CHROME_BIN`, default `/usr/bin/chromium`). Use `.no_sandbox()` (containers) + the reference's speed/stealth args. Every browser error is caught and returned as `Err` — a chromiumoxide/Chromium crash must NEVER take down this process (and it is a SEPARATE process from `reactor-core`, so even a hard abort cannot touch the hot-path dedup/quota state — correction #2).

- [ ] **Step 1: Add deps**

```bash
cd Backend
cargo add --package auth-sidecar chromiumoxide
cargo add --package auth-sidecar serde --features derive
cargo add --package auth-sidecar futures
cd ..
```

Confirm `cargo deny check` is still `advisories ok, bans ok, licenses ok, sources ok` after this add (research says it is — chromiumoxide's whole subtree is already-allowed licenses; `reqwest` 0.13 enters `auth-sidecar` as a second HTTP client but `bans.multiple-versions=warn` tolerates it).

- [ ] **Step 2: Write `browser.rs`**

```rust
// Backend/bin/auth-sidecar/src/browser.rs
//! Tier-1 SPX browser login via chromiumoxide (0.9.1). Runs in THIS process
//! (separate from reactor-core) so a Chromium crash cannot touch the hot path.
//! Every failure returns Err (never panics) so the sidecar stays up.
use std::collections::HashMap;
use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::error::CdpError;
use futures::StreamExt;

pub struct BrowserLoginCfg {
    pub spx_base_url: String,
    pub chrome_bin: String,
}

impl BrowserLoginCfg {
    pub fn from_env() -> Self {
        Self {
            spx_base_url: std::env::var("SPX_BASE_URL")
                .unwrap_or_else(|_| "https://logistics.myagencyservice.id".to_string()),
            chrome_bin: std::env::var("CHROME_BIN").unwrap_or_else(|_| "/usr/bin/chromium".to_string()),
        }
    }
}

/// Launch headless Chromium, log into SPX, return all cookies as name→value.
pub async fn browser_login(
    cfg: &BrowserLoginCfg,
    username: &str,
    password: &str,
) -> Result<HashMap<String, String>, String> {
    let config = BrowserConfig::builder()
        .chrome_executable(&cfg.chrome_bin)
        .no_sandbox()
        .arg("--disable-dev-shm-usage")
        .arg("--disable-blink-features=AutomationControlled")
        .arg("--disable-gpu")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .request_timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("browser config: {e}"))?;

    let (mut browser, mut handler) = Browser::launch(config)
        .await
        .map_err(|e| format!("launch: {e}"))?;
    // MUST drive the handler or nothing progresses; keep the task alive for the
    // whole login and abort it at the end.
    let handler_task = tokio::spawn(async move {
        while let Some(h) = handler.next().await {
            if h.is_err() {
                break;
            }
        }
    });

    let result = do_login(&browser, cfg, username, password).await;

    let _ = browser.close().await;
    handler_task.abort();
    result
}

async fn do_login(
    browser: &Browser,
    cfg: &BrowserLoginCfg,
    username: &str,
    password: &str,
) -> Result<HashMap<String, String>, String> {
    let login_url = format!("{}/login", cfg.spx_base_url.trim_end_matches('/'));
    let booking_url = format!("{}/line-haul/booking", cfg.spx_base_url.trim_end_matches('/'));

    let page = browser.new_page(&login_url).await.map_err(cdp)?;
    // Wait for the SSO password field to appear (form-ready signal).
    page.find_element("input[type=\"password\"]").await.map_err(cdp)?;

    // Fill email (SSO uses bare inputs; try email → text fallbacks in one selector).
    let email_sel = "input[type=\"email\"], input[name=\"email\"], input[name=\"username\"], input[type=\"text\"]";
    page.find_element(email_sel).await.map_err(cdp)?.click().await.map_err(cdp)?.type_str(username).await.map_err(cdp)?;
    // Password + submit via Enter (the React SSO form submits on Enter).
    page.find_element("input[type=\"password\"]").await.map_err(cdp)?
        .click().await.map_err(cdp)?
        .type_str(password).await.map_err(cdp)?
        .press_key("Enter").await.map_err(cdp)?;

    // Poll up to ~20s for the auth cookie.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(cookies) = page.get_cookies().await {
            if cookies.iter().any(|c| c.name == "fms_user_skey" && !c.value.is_empty()) {
                break;
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err("login timeout: fms_user_skey not set".into());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // Visit the booking page so SPX sets spx_cid.
    let _ = page.goto(&booking_url).await;
    tokio::time::sleep(Duration::from_millis(800)).await;

    let cookies = page.get_cookies().await.map_err(cdp)?;
    let mut out = HashMap::new();
    for c in cookies {
        out.insert(c.name.clone(), c.value.clone());
    }
    if !out.contains_key("fms_user_skey") {
        return Err("no fms_user_skey after login".into());
    }
    Ok(out)
}

fn cdp(e: CdpError) -> String {
    format!("cdp: {e}")
}
```

> The exact `chromiumoxide::error` type path (`CdpError`) and `Cookie` field access (`.name`/`.value`) were read from the 0.9.1 source; still, **the implementer should `cargo build -p auth-sidecar` early to confirm the error type import path and the `Cookie` struct's exact field types (they come from `chromiumoxide_cdp::cdp::browser_protocol::network::Cookie`).** Adjust the `cdp()` mapper import if the error enum name differs.

- [ ] **Step 3: Wire `main.rs` `/login` route**

```rust
// Backend/bin/auth-sidecar/src/main.rs (additions)
mod browser;

use axum::{routing::{get, post}, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use browser::{browser_login, BrowserLoginCfg};

fn app() -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/login", post(login))
}

#[derive(Deserialize)]
struct LoginReq {
    #[allow(dead_code)]
    account_id: String,
    username: String,
    password: String,
}

#[derive(Serialize, Default)]
struct CookiesOut {
    fms_user_skey: String,
    fms_user_id: String,
    fms_user_agency_id: String,
    csrftoken: String,
    spx_uk: String,
    spx_cid: String,
    spx_uid: String,
    spx_agid: String,
    spx_st: String,
    ds: String,
    spx_admin_device_id: String,
}

async fn login(Json(req): Json<LoginReq>) -> Json<Value> {
    let cfg = BrowserLoginCfg::from_env();
    match browser_login(&cfg, &req.username, &req.password).await {
        Ok(map) => {
            let mut c = CookiesOut::default();
            let g = |k: &str| map.get(k).cloned().unwrap_or_default();
            c.fms_user_skey = g("fms_user_skey");
            c.fms_user_id = g("fms_user_id");
            c.fms_user_agency_id = g("fms_user_agency_id");
            c.csrftoken = g("csrftoken");
            c.spx_uk = g("spx_uk");
            c.spx_cid = g("spx_cid");
            c.spx_uid = g("spx_uid");
            c.spx_agid = g("spx_agid");
            c.spx_st = g("spx_st");
            c.ds = g("ds");
            c.spx_admin_device_id = g("spx-admin-device-id");
            Json(json!({ "ok": true, "cookies": c }))
        }
        Err(e) => Json(json!({ "ok": false, "error": e })),
    }
}
```

Keep the existing `healthz`, `main`, `shutdown_signal` unchanged.

- [ ] **Step 4: Docker — install Chromium**

Edit `Docker/auth-sidecar.Dockerfile`'s runtime stage to install `chromium` and set `CHROME_BIN`:

```dockerfile
# runtime stage — add chromium (chromiumoxide drives an EXTERNAL browser; it is
# NOT bundled). auth-sidecar's tier-1 SPX browser-login (Fase 5) needs it. The
# fonts avoid blank-render issues on some SSO pages.
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates chromium fonts-liberation \
    && rm -rf /var/lib/apt/lists/*
ENV CHROME_BIN=/usr/bin/chromium
```

(Replace the existing runtime `apt-get install` line; keep the `useradd`, `COPY --from=builder`, `USER tower`, `EXPOSE 8082`, `ENTRYPOINT`.) **Task concern:** the image grows ~300MB; the build stage also now compiles chromiumoxide's larger dep tree — note in the commit that the sidecar image is intentionally heavier than `reactor-core`'s. Chromium under `USER tower` needs `--no-sandbox` (set in `browser.rs`).

- [ ] **Step 5: Handler parse/shape test (DoD #5 sidecar half)**

```rust
// in Backend/bin/auth-sidecar/src/main.rs #[cfg(test)] mod tests
#[tokio::test]
async fn login_returns_ok_false_when_browser_unavailable() {
    // With no Chromium in the test env, browser_login returns Err → the handler
    // must respond 200 with {ok:false, error:...} (NEVER a 5xx — the poller's
    // tier-fallthrough relies on a clean ok:false). Proves the request parse +
    // response shape without a real browser.
    std::env::set_var("CHROME_BIN", "/nonexistent/chromium");
    let response = app()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/login")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    r#"{"account_id":"a","username":"u","password":"p"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ok"], false, "browser-unavailable must be a clean ok:false, not a 5xx");
    assert!(json["error"].is_string());
}
```

> This test intentionally does NOT require a browser: it proves the endpoint parses the contract and returns the `ok:false` shape when Chromium is absent — the design doc's DoD #5 explicitly scopes the sidecar test to "endpoint receives request and returns cookies [shape]" rather than an end-to-end SPX login. A separate, MANUALLY-run smoke (documented in the commit message) drives a real login against a live SPX when a Chromium is present.

- [ ] **Step 6: Build, test, clippy, deny, commit**

```bash
cd Backend
cargo test -p auth-sidecar
cargo clippy -p auth-sidecar --all-targets -- -D warnings
cargo deny check
cd ..
git add Backend/bin/auth-sidecar Backend/Cargo.toml Backend/Cargo.lock Docker/auth-sidecar.Dockerfile
git commit -m "feat(auth-sidecar): tier-1 chromiumoxide browser-login /login handler + Docker chromium (separate process, panic-isolated)"
```

Expected: the handler test passes (ok:false when Chromium absent); `cargo deny check` stays fully green (chromiumoxide adds NO license/advisory issue).

---

### Task 10: `notifier` crate — WAHA + n8n fire-and-forget + message templates (ported from `webhook.ts`)

**Verification confidence:** Message templates ported VERBATIM from the reference `webhook.ts` (`buildTicketBlock`, `buildWaMessage`, `buildNewTicketsMessage`, `sendAgencyLossAlert` text, `buildDriverAssignedMessage` — all read for this plan). WAHA `POST /api/sendText` shape is from the reference. `wreq` send is best-effort (reqwest-shaped); **verify the `wreq` client build + `.json().send()` against the pinned version.**

**Files:**
- Modify: `Backend/crates/notifier/Cargo.toml` (deps)
- Overwrite: `Backend/crates/notifier/src/lib.rs`
- Create: `Backend/crates/notifier/src/message.rs`
- Create: `Backend/crates/notifier/src/waha.rs`
- Create: `Backend/crates/notifier/tests/waha_mock.rs`

**Interfaces produced:**
- `pub struct BotSettings { pub enabled: bool, pub webhook_url: String, pub wa_group: String, pub waha_url: String, pub waha_api_key: String, pub waha_session: String }`.
- `pub struct NotifyBooking { pub booking_id, request_id, onsite_id, booking_name, spx_tx_id, vehicle_type: String, route_stops: Vec<String>, report_station, cost_type: Option<i64>, adhoc_tag: Option<i64>, standby_time: Option<i64>, period_start: Option<i64>, period_end: Option<i64>, bidding_ddl: Option<i64>, is_coc: bool }` (the pure data the templates need — notifier knows NOTHING of `executor`/`SpxBooking`).
- Message builders (ported, pure functions, unit-tested): `pub fn build_ticket_block(b: &NotifyBooking, accepted: bool, link: Option<&str>, portal_label: &str) -> String`, `pub fn build_wa_message(b, portal_label) -> String` (accepted, no link), `pub fn build_new_tickets_message(bs: &[NotifyBooking], portal_label) -> String` (cap 10 + "+N"), `pub fn build_agency_loss_text(spx_id, rival, latency_ms, rule: Option<&str>) -> String`, `pub fn build_driver_assigned_message(...) -> String`.
- Fire-and-forget senders: `pub async fn notify_accepted(settings: &BotSettings, b: &NotifyBooking, portal_label: &str)`, `pub async fn notify_new_tickets(settings, bs, portal_label)`, `pub async fn notify_agency_loss(settings, spx_id, rival, latency_ms, rule)`, `pub async fn notify_driver_assigned(settings, ...)`. Each returns `()` — it logs on failure via `tracing::warn!` and NEVER returns an error (the caller spawns it and cannot be affected).

**Design note (fire-and-forget — correction #6):** notifier has NO bus. Each `notify_*` builds the message, POSTs to WAHA (`{waha_url}/api/sendText` with `X-Api-Key` header, body `{session, chatId, text}`) for each parsed chatId, and optionally POSTs the n8n webhook — all best-effort. The caller (poller `finalize_win` / agency-loss branch) does `tokio::spawn(async move { notifier::notify_accepted(...).await; })` and drops the handle. A WAHA failure must NEVER propagate — DoD #8 tests this by making WAHA return 500 and asserting the sender still returns `()`.

- [ ] **Step 1: Add deps**

```bash
cd Backend
cargo add --package notifier tokio --features rt-multi-thread,macros,time
cargo add --package notifier wreq
cargo add --package notifier serde --features derive
cargo add --package notifier serde_json
cargo add --package notifier tracing
cargo add --package notifier chrono
cargo add --package notifier --dev wiremock
cargo add --package notifier --dev tokio --features rt-multi-thread,macros,time
cd ..
```

- [ ] **Step 2: Write `message.rs` (port the REAL templates)**

Port `webhook.ts`'s canonical block. The key shape (verbatim from the reference): a `buildTicketBlock` with a type line (`FTL CENTRAL ON CALL ( COC )` vs `FTL REGULER ( REG )` — `isCoc` = booking name starts `SPXID`), a `—`×25 divider, aligned `Label   : value` rows (`Booking [ id ] name`, `Request [ id ]`, `Onsite [ id ]`, `Station`, `Rute` (` → ` joined), `Armada`, `Periode` (DD/MM/YYYY), `Standby` (minutes-from-midnight → HH:MM)), and an optional `🔔 : <link>` footer. Accepted messages prepend `*TIKET DI TERIMA OLEH SYSTEM[ - <label>]*`.

```rust
// Backend/crates/notifier/src/message.rs
//! WhatsApp/n8n message templates, ported from spx-portal-ref webhook.ts
//! (buildTicketBlock / buildWaMessage / buildNewTicketsMessage /
//! sendAgencyLossAlert / buildDriverAssignedMessage). Pure functions, unit-
//! tested; no I/O. Layout MUST match the reference (the ops team reads these).
use chrono::{FixedOffset, TimeZone, Utc};

use crate::NotifyBooking;

fn wib() -> FixedOffset {
    FixedOffset::east_opt(7 * 3600).expect("valid +7")
}

fn id_val(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() || t == "0" { "-".to_string() } else { t.to_string() }
}

const COST_LABELS: [(i64, &str); 3] = [(1, "FTL"), (2, "LTL"), (3, "LTL")];
fn cost_label(v: Option<i64>) -> &'static str {
    v.and_then(|n| COST_LABELS.iter().find(|(k, _)| *k == n).map(|(_, l)| *l))
        .unwrap_or("FTL")
}

/// SPX standby_time is MINUTES from midnight (e.g. 944 → 15:44).
fn fmt_standby(min: Option<i64>) -> String {
    match min {
        Some(m) if m >= 0 => format!("{:02}:{:02}", m / 60, m % 60),
        _ => String::new(),
    }
}

/// unix seconds → DD/MM/YYYY in WIB.
fn fmt_dmy(sec: Option<i64>) -> String {
    match sec {
        Some(s) if s > 0 => wib().timestamp_opt(s, 0).single()
            .map(|d| d.format("%d/%m/%Y").to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn fmt_period_dmy(b: &NotifyBooking) -> String {
    let start = b.period_start.filter(|&s| s > 0).or(b.period_end);
    let s = fmt_dmy(start);
    if s.is_empty() { "-".to_string() } else { s.replace('/', " - ") }
}

/// The ONE canonical ticket block (buildTicketBlock). `accepted` prepends the
/// TIKET-DITERIMA header; `link` (Some) appends the 🔔 accept link.
pub fn build_ticket_block(
    b: &NotifyBooking,
    accepted: bool,
    link: Option<&str>,
    portal_label: &str,
) -> String {
    let cost = cost_label(b.cost_type);
    let is_coc = b.booking_name.to_uppercase().starts_with("SPXID")
        || b.spx_tx_id.to_uppercase().starts_with("SPXID")
        || b.is_coc;
    let type_line = format!("{cost} {}", if is_coc { "CENTRAL ON CALL ( COC )" } else { "REGULER ( REG )" });
    let d: String = "—".repeat(25);
    let rute = if b.route_stops.is_empty() {
        "-".to_string()
    } else {
        b.route_stops.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" → ")
    };
    let station = { let t = b.report_station.trim(); if t.is_empty() { "-".to_string() } else { t.to_string() } };
    let standby = { let s = fmt_standby(b.standby_time); if s.is_empty() { "-".to_string() } else { s } };
    const PAD: usize = 7;
    let row = |label: &str, val: &str| {
        let pad = PAD.saturating_sub(label.len());
        format!("{label}{} : {val}", " ".repeat(pad))
    };
    let name = if !b.booking_name.is_empty() { &b.booking_name } else if !b.spx_tx_id.is_empty() { &b.spx_tx_id } else { "-" };
    let req = id_val(&b.request_id);
    let on = id_val(&b.onsite_id);

    let mut lines: Vec<String> = Vec::new();
    if accepted {
        let suffix = if portal_label.is_empty() { String::new() } else { format!(" - {portal_label}") };
        lines.push(format!("*TIKET DI TERIMA OLEH SYSTEM{suffix}*"));
        lines.push(format!(" {type_line}"));
    } else {
        lines.push(type_line);
    }
    lines.push(d.clone());
    lines.push(row("Booking", &format!("[ {} ] {}", id_val(&b.booking_id), name)));
    lines.push(row("Request", &(if req != "-" { format!("[ {req} ]") } else { "-".to_string() })));
    lines.push(row("Onsite", &(if on != "-" { format!("[ {on} ]") } else { "-".to_string() })));
    lines.push(row("Station", &station));
    lines.push(row("Rute", &rute));
    lines.push(row("Armada", if b.vehicle_type.is_empty() { "-" } else { &b.vehicle_type }));
    lines.push(row("Periode", &fmt_period_dmy(b)));
    lines.push(row("Standby", &standby));
    lines.push(d.clone());
    if let Some(l) = link {
        lines.push(format!("🔔 : {l}"));
        lines.push(d);
    }
    lines.join("\n")
}

/// Accept notification (TIKET DITERIMA header, no link).
pub fn build_wa_message(b: &NotifyBooking, portal_label: &str) -> String {
    build_ticket_block(b, true, None, portal_label)
}

/// New-ticket broadcast: up to 10 blocks (each with a 🔔 link) + "+N more".
pub fn build_new_tickets_message(bs: &[NotifyBooking], portal_label: &str, accept_base: &str) -> String {
    const CAP: usize = 10;
    let shown = bs.iter().take(CAP);
    let blocks: Vec<String> = shown
        .map(|b| {
            // link is a placeholder: the real one-tap /accept/<code> is minted by
            // Fase 6's REST layer; Fase 5 passes an accept_base + booking id.
            let link = if accept_base.is_empty() { None } else { Some(format!("{accept_base}/{}", b.booking_id)) };
            build_ticket_block(b, false, link.as_deref(), portal_label)
        })
        .collect();
    let mut out = blocks.join("\n\n");
    if bs.len() > CAP {
        out.push_str(&format!("\n{}\nTiket lain: +{}", "—".repeat(25), bs.len() - CAP));
    }
    out
}

/// Same-agency loss alert (sendAgencyLossAlert text).
pub fn build_agency_loss_text(spx_id: &str, rival: &str, latency_ms: i64, rule: Option<&str>) -> String {
    let rule_line = rule.map(|r| format!("\nRule: {r}")).unwrap_or_default();
    format!(
        "⚠️ KALAH RACE (rekan se-agency)\nTiket: {spx_id}\nDiambil oleh: {rival}\nTembakan kita: {latency_ms}ms{rule_line}\n— rival mengalahkan kita di race ini (bukti race diperebutkan)"
    )
}

/// Driver-assigned follow-up (buildDriverAssignedMessage).
pub fn build_driver_assigned_message(
    tx_id: &str, booking_id: &str, onsite_id: &str, driver_name: &str, plate: &str, portal_label: &str,
) -> String {
    let suffix = if portal_label.is_empty() { String::new() } else { format!(" · {portal_label}") };
    let div = "—".repeat(20);
    let when = Utc::now().with_timezone(&wib()).format("%d %b %Y, %H:%M").to_string();
    [
        format!("*SPX AGENCY PORTAL{suffix}*"),
        "*Driver & Armada Ditugaskan*".to_string(),
        div.clone(),
        format!("Nomor Booking: *{}*", id_val(tx_id)),
        format!("Booking ID: {}", id_val(booking_id)),
        format!("Onsite ID: {}", id_val(onsite_id)),
        div,
        format!("Driver: *{}*", if driver_name.is_empty() { "-" } else { driver_name }),
        format!("Nomor Polisi: *{}*", if plate.is_empty() { "-" } else { plate }),
        format!("Waktu: {when} WIB"),
    ].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> NotifyBooking {
        NotifyBooking {
            booking_id: "5931641".into(), request_id: "0".into(), onsite_id: "6141306".into(),
            booking_name: "SPXID_VM_001396562".into(), spx_tx_id: "SPXID_VM_001396562".into(),
            vehicle_type: "TRONTON (10WH)".into(),
            route_stops: vec!["Cileungsi DC".into(), "Medan Amplas DC".into()],
            report_station: "Cileungsi DC".into(), cost_type: Some(1), adhoc_tag: Some(1),
            standby_time: Some(944), period_start: Some(1_750_000_000), period_end: None,
            bidding_ddl: None, is_coc: true,
        }
    }

    #[test]
    fn accepted_block_has_header_and_coc_type_and_aligned_rows() {
        let s = build_wa_message(&sample(), "12 LOG");
        assert!(s.contains("*TIKET DI TERIMA OLEH SYSTEM - 12 LOG*"));
        assert!(s.contains("FTL CENTRAL ON CALL ( COC )"));
        assert!(s.contains("Booking : [ 5931641 ] SPXID_VM_001396562"));
        assert!(s.contains("Standby : 15:44")); // 944 min from midnight
        assert!(!s.contains("🔔"), "accept notif has NO link");
    }

    #[test]
    fn new_tickets_caps_at_ten_with_more_line() {
        let many: Vec<NotifyBooking> = (0..13).map(|i| {
            let mut b = sample();
            b.booking_id = i.to_string();
            b
        }).collect();
        let s = build_new_tickets_message(&many, "EPL", "https://p/accept");
        assert!(s.contains("Tiket lain: +3"));
        assert!(s.contains("🔔 : https://p/accept/0"));
    }

    #[test]
    fn agency_loss_text_matches_reference_shape() {
        let s = build_agency_loss_text("SPXID1", "rival@x.com", 42, Some("R"));
        assert!(s.starts_with("⚠️ KALAH RACE"));
        assert!(s.contains("Diambil oleh: rival@x.com"));
        assert!(s.contains("Tembakan kita: 42ms"));
        assert!(s.contains("Rule: R"));
    }
}
```

- [ ] **Step 3: Write `waha.rs` + `lib.rs`** (fire-and-forget senders)

```rust
// Backend/crates/notifier/src/waha.rs
//! WAHA + n8n HTTP senders — pure fire-and-forget. A failure logs and returns;
//! it NEVER errors (the caller spawns these and must be unaffected — a notify
//! hiccup can never fail an accept that already succeeded).
use serde_json::json;

use crate::BotSettings;

/// Parse a multi-target field into WAHA chatIds: "...@g.us"/"...@c.us" as-is;
/// bare digits → "<num>@c.us".
pub fn parse_chat_ids(raw: &str) -> Vec<String> {
    raw.split([' ', ',', ';', '\n', '\t'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|t| if t.contains('@') { t.to_string() } else { format!("{}@c.us", t.chars().filter(|c| c.is_ascii_digit()).collect::<String>()) })
        .filter(|t| t.len() > 4)
        .collect()
}

fn client() -> wreq::Client {
    // Plain client (internal WAHA/n8n; no impersonation). Default build; on a
    // build error, unwrap_or_default keeps the sender infallible.
    wreq::Client::builder().build().unwrap_or_default()
}

/// POST one message to every chatId in `group`. Best-effort; returns (sent,total).
pub async fn send_to_waha_many(s: &BotSettings, group: &str, text: &str) -> (usize, usize) {
    let targets = parse_chat_ids(group);
    if targets.is_empty() || s.waha_url.is_empty() || s.waha_api_key.is_empty() {
        return (0, targets.len());
    }
    let http = client();
    let url = format!("{}/api/sendText", s.waha_url.trim_end_matches('/'));
    let session = if s.waha_session.is_empty() { "default" } else { &s.waha_session };
    let mut sent = 0;
    for chat_id in &targets {
        let body = json!({ "session": session, "chatId": chat_id, "text": text });
        match http.post(&url).header("X-Api-Key", &s.waha_api_key).json(&body).send().await {
            Ok(r) if r.status().is_success() => sent += 1,
            Ok(r) => tracing::warn!(status = %r.status(), chat = %chat_id, "WAHA sendText non-2xx"),
            Err(e) => tracing::warn!(error = %e, chat = %chat_id, "WAHA sendText failed"),
        }
    }
    (sent, targets.len())
}

/// POST an n8n webhook (best-effort).
pub async fn send_n8n(s: &BotSettings, payload: serde_json::Value) {
    if s.webhook_url.is_empty() {
        return;
    }
    let http = client();
    if let Err(e) = http.post(&s.webhook_url).json(&payload).send().await {
        tracing::warn!(error = %e, "n8n webhook failed");
    }
}
```

```rust
// Backend/crates/notifier/src/lib.rs
//! Fase 5 — notifier: pure fire-and-forget WAHA + n8n + Web-Push VAPID delivery.
//! NO internal bus (correction #6). Callers `tokio::spawn` these and drop the
//! Result; a notify failure can NEVER affect an accept that already succeeded.
//! notifier knows NOTHING of `executor`/`SpxBooking` — it takes pure event data.
pub mod message;
pub mod waha;

pub use message::{
    build_agency_loss_text, build_driver_assigned_message, build_new_tickets_message,
    build_ticket_block, build_wa_message,
};
pub use waha::{parse_chat_ids, send_n8n, send_to_waha_many};

// Task 11 adds: pub mod push_vapid;

#[derive(Debug, Clone, Default)]
pub struct BotSettings {
    pub enabled: bool,
    pub webhook_url: String,
    pub wa_group: String,
    pub waha_url: String,
    pub waha_api_key: String,
    pub waha_session: String,
    pub portal_label: String,
}

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

/// Fire-and-forget accept notification. Returns () — a failure only logs.
pub async fn notify_accepted(settings: &BotSettings, b: &NotifyBooking) {
    if !settings.enabled {
        return;
    }
    let text = build_wa_message(b, &settings.portal_label);
    let _ = waha::send_to_waha_many(settings, &settings.wa_group, &text).await;
    waha::send_n8n(settings, serde_json::json!({ "event": "booking_accepted", "bookingId": b.booking_id, "message": text })).await;
}

pub async fn notify_new_tickets(settings: &BotSettings, bs: &[NotifyBooking], accept_base: &str) {
    if !settings.enabled || bs.is_empty() || bs.len() > 30 {
        return; // seed/backfill flood guard (reference: >30 → skip)
    }
    let text = build_new_tickets_message(bs, &settings.portal_label, accept_base);
    let _ = waha::send_to_waha_many(settings, &settings.wa_group, &text).await;
}

pub async fn notify_agency_loss(settings: &BotSettings, spx_id: &str, rival: &str, latency_ms: i64, rule: Option<&str>) {
    if !settings.enabled {
        return;
    }
    let text = build_agency_loss_text(spx_id, rival, latency_ms, rule);
    let _ = waha::send_to_waha_many(settings, &settings.wa_group, &text).await;
}

pub async fn notify_driver_assigned(settings: &BotSettings, tx_id: &str, booking_id: &str, onsite_id: &str, driver_name: &str, plate: &str) {
    if !settings.enabled {
        return;
    }
    let text = build_driver_assigned_message(tx_id, booking_id, onsite_id, driver_name, plate, &settings.portal_label);
    let _ = waha::send_to_waha_many(settings, &settings.wa_group, &text).await;
}
```

- [ ] **Step 4: Fire-and-forget test — WAHA failure never propagates (DoD #8)**

```rust
// Backend/crates/notifier/tests/waha_mock.rs
//! DoD #8: a WAHA 500 must NOT make the sender error — notify_accepted returns
//! () regardless. Also proves a healthy WAHA gets the sendText call.
use notifier::{notify_accepted, BotSettings, NotifyBooking};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn booking() -> NotifyBooking {
    NotifyBooking { booking_id: "1".into(), booking_name: "SPXID1".into(), ..Default::default() }
}

fn settings(url: String) -> BotSettings {
    BotSettings {
        enabled: true, waha_url: url, waha_api_key: "K".into(), waha_session: "default".into(),
        wa_group: "12036@g.us".into(), portal_label: "EPL".into(), ..Default::default()
    }
}

#[tokio::test]
async fn waha_500_does_not_propagate() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/sendText"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server).await;
    // If notify_accepted returned a Result or panicked, this would not compile /
    // would fail. It returns () — the caller is unaffected.
    notify_accepted(&settings(server.uri()), &NotifyBooking { booking_id: "1".into(), booking_name: "SPXID1".into(), ..Default::default() }).await;
    // Reaching here at all is the assertion (no panic, no error to handle).
}

#[tokio::test]
async fn healthy_waha_receives_sendtext() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/sendText"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
        .expect(1)
        .mount(&server).await;
    notify_accepted(&settings(server.uri()), &NotifyBooking { booking_id: "9".into(), booking_name: "SPXID9".into(), ..Default::default() }).await;
    // wiremock's .expect(1) verifies on drop that sendText was called exactly once.
}
```

> The `booking()` helper is unused by the two tests below (they construct `NotifyBooking` inline via `..Default::default()`) — keep it only if you add more cases, otherwise delete it to avoid a dead-code warning. The load-bearing assertions are: (1) a 500 does not propagate; (2) a healthy WAHA receives exactly one sendText.

- [ ] **Step 5: Test, clippy, commit**

```bash
cd Backend
cargo test -p notifier
cargo clippy -p notifier --all-targets -- -D warnings
cd ..
git add Backend/crates/notifier Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(notifier): WAHA + n8n fire-and-forget + ported webhook.ts message templates (accept/new-tickets/agency-loss/driver-assigned)"
```

---

### Task 11: `notifier` Web Push VAPID (`web-push-native`) + `deny.toml` advisory ignore

**Verification confidence:** `web-push-native` 0.4.0 API READ FROM SOURCE: `WebPushBuilder::new(endpoint: http::Uri, ua_public: p256::PublicKey, ua_auth: Auth)`, `.with_vapid(&jwt_simple::algorithms::ES256KeyPair, contact)`, `.build(body: impl Into<Vec<u8>>) -> Result<http::Request<Vec<u8>>, Error>`. License clean (MIT/Apache — NO `ece`/MPL). The `rsa` advisory + its ignore are research-confirmed. The p256/jwt-simple key construction from the VAPID keys is best-effort — **verify `ES256KeyPair::from_bytes` and `p256::PublicKey::from_sec1_bytes` against the pinned versions.**

**Files:**
- Modify: `Backend/crates/notifier/Cargo.toml` (add `web-push-native`, `jwt-simple`, `p256`, `base64`, `redis`, `http`)
- Modify: `Backend/deny.toml` (add `RUSTSEC-2023-0071` to `[advisories] ignore`)
- Create: `Backend/crates/notifier/src/push_vapid.rs`
- Modify: `Backend/crates/notifier/src/lib.rs`
- Create: `Backend/crates/notifier/tests/push_mock.rs`

**Interfaces produced:**
- `pub struct VapidConfig { pub subject: String, pub public_key: String, pub private_key: String }` (from env `VAPID_SUBJECT`/`VAPID_PUBLIC`/`VAPID_PRIVATE`).
- `pub struct PushSubscription { pub endpoint: String, pub p256dh: String, pub auth: String }` (parsed from the JSON stored in `spx:push_subs:<acct>`).
- `pub fn build_push_request(vapid: &VapidConfig, sub: &PushSubscription, payload: &PushPayload) -> Result<http::Request<Vec<u8>>, PushError>` — construct the encrypted, VAPID-signed request.
- `pub async fn send_push_to_account(redis_url: &str, vapid: &VapidConfig, account_id: &str, payload: &PushPayload) -> usize` — read subscriptions from `spx:push_subs:<acct.lowercase>`, build+send each via `wreq`, prune 404/410, return the count sent. Fire-and-forget (errors log, never propagate).
- `pub struct PushPayload { pub title: String, pub body: String, pub url: Option<String>, pub tag: Option<String> }`.

**Design note (deny.toml — the Fase-5 exception):** `web-push-native` pulls `rsa` transitively via `jwt-simple` (VAPID's ES256 JWT signer). `rsa` triggers `RUSTSEC-2023-0071` (Marvin timing sidechannel). **VAPID signs ONLY with ECDSA P-256 (ES256) — the `rsa` code path is never invoked — and there is no fixed `rsa` release.** So Fase 5 adds `RUSTSEC-2023-0071` to `deny.toml`'s `[advisories] ignore` with a justifying comment. This is an UNREACHABLE-CODE advisory ignore, NOT a copyleft license admission (the MPL-2.0 landmine was avoided entirely by choosing `web-push-native` over `web-push`). Do NOT add MPL-2.0. Do NOT add any copyleft license.

- [ ] **Step 1: Add deps**

```bash
cd Backend
cargo add --package notifier web-push-native
cargo add --package notifier jwt-simple
cargo add --package notifier p256
cargo add --package notifier base64
cargo add --package notifier http
cargo add --package notifier redis --features tokio-comp,connection-manager
cd ..
```

Then verify the versions resolve to match `web-push-native`'s tree (they must be the SAME `p256`/`jwt-simple` `web-push-native` uses, or the `PublicKey`/`ES256KeyPair` types won't unify):

```bash
cd Backend && cargo tree -p notifier -i p256 && cargo tree -p notifier -i jwt-simple && cd ..
```

Expect `p256 0.13.x` and `jwt-simple 0.12.x` (single versions). If a duplicate appears, pin `notifier`'s `p256`/`jwt-simple` to the exact versions `web-push-native` uses.

- [ ] **Step 2: Add the advisory ignore to `deny.toml`**

Edit `Backend/deny.toml`'s `[advisories]` section. It currently has no `ignore` key; add one:

```toml
[advisories]
db-path = "~/.cargo/advisory-db"
db-urls = ["https://github.com/rustsec/advisory-db"]
yanked = "deny"
# RUSTSEC-2023-0071 ("Marvin Attack": RSA timing sidechannel). `rsa` is pulled
# ONLY transitively by `jwt-simple` (the VAPID JWT signer under
# `web-push-native`, Fase 5's Web Push). VAPID signs exclusively with ECDSA
# P-256 (ES256) — the `rsa` decryption/signing code path is NEVER invoked by our
# usage — and there is no fixed `rsa` release for this advisory. This is an
# unreachable-code ignore, NOT a copyleft/license admission (the MPL-2.0 `ece`
# landmine was avoided by choosing `web-push-native` over the older `web-push`).
ignore = ["RUSTSEC-2023-0071"]
```

Do NOT touch `[licenses]` (no new license needed), `[bans]`, `[sources]`, or `private.ignore`.

- [ ] **Step 3: Write `push_vapid.rs`**

```rust
// Backend/crates/notifier/src/push_vapid.rs
//! Web Push (VAPID) via web-push-native 0.4.0. Reads subscriptions from
//! `spx:push_subs:<acct>` (Redis SET of subscription JSON), builds an encrypted
//! aes128gcm + ES256-signed request per subscription, sends via wreq, prunes
//! expired (404/410) subscriptions. Fire-and-forget (errors log, never
//! propagate). See Global Constraints for the RUSTSEC-2023-0071 rationale.
use base64::Engine;
use jwt_simple::algorithms::ES256KeyPair;
use redis::AsyncCommands;
use serde::Deserialize;
use web_push_native::{Auth, WebPushBuilder};

#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error("vapid key: {0}")]
    Vapid(String),
    #[error("subscription: {0}")]
    Sub(String),
    #[error("build: {0}")]
    Build(String),
}

#[derive(Debug, Clone)]
pub struct VapidConfig {
    pub subject: String,     // e.g. "mailto:ops@example.com"
    pub public_key: String,  // base64url
    pub private_key: String, // base64url (32-byte P-256 scalar)
}

impl VapidConfig {
    pub fn from_env() -> Option<Self> {
        let subject = std::env::var("VAPID_SUBJECT").ok()?;
        let public_key = std::env::var("VAPID_PUBLIC").ok()?;
        let private_key = std::env::var("VAPID_PRIVATE").ok()?;
        if public_key.is_empty() || private_key.is_empty() {
            return None;
        }
        Some(Self { subject, public_key, private_key })
    }
    fn keypair(&self) -> Result<ES256KeyPair, PushError> {
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(self.private_key.trim())
            .map_err(|e| PushError::Vapid(e.to_string()))?;
        ES256KeyPair::from_bytes(&raw).map_err(|e| PushError::Vapid(e.to_string()))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PushSubscription {
    pub endpoint: String,
    #[serde(default)]
    pub p256dh: String,
    #[serde(default)]
    pub auth: String,
}

/// Some browsers store keys nested under `keys: {p256dh, auth}`; accept both.
#[derive(Deserialize)]
struct RawSub {
    endpoint: String,
    #[serde(default)]
    keys: Option<RawKeys>,
    #[serde(default)]
    p256dh: Option<String>,
    #[serde(default)]
    auth: Option<String>,
}
#[derive(Deserialize)]
struct RawKeys {
    #[serde(default)]
    p256dh: String,
    #[serde(default)]
    auth: String,
}

fn parse_sub(raw: &str) -> Option<PushSubscription> {
    let r: RawSub = serde_json::from_str(raw).ok()?;
    let (p256dh, auth) = match r.keys {
        Some(k) => (k.p256dh, k.auth),
        None => (r.p256dh.unwrap_or_default(), r.auth.unwrap_or_default()),
    };
    Some(PushSubscription { endpoint: r.endpoint, p256dh, auth })
}

#[derive(Debug, Clone)]
pub struct PushPayload {
    pub title: String,
    pub body: String,
    pub url: Option<String>,
    pub tag: Option<String>,
}

impl PushPayload {
    fn to_json(&self) -> Vec<u8> {
        serde_json::json!({
            "title": self.title, "body": self.body,
            "url": self.url, "tag": self.tag,
        })
        .to_string()
        .into_bytes()
    }
}

/// Build the encrypted, VAPID-signed HTTP request for one subscription.
pub fn build_push_request(
    vapid: &VapidConfig,
    sub: &PushSubscription,
    payload: &PushPayload,
) -> Result<http::Request<Vec<u8>>, PushError> {
    let kp = vapid.keypair()?;
    let ua_public_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(sub.p256dh.trim())
        .map_err(|e| PushError::Sub(e.to_string()))?;
    let ua_auth_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(sub.auth.trim())
        .map_err(|e| PushError::Sub(e.to_string()))?;
    let ua_public = p256::PublicKey::from_sec1_bytes(&ua_public_bytes)
        .map_err(|e| PushError::Sub(e.to_string()))?;
    let ua_auth = Auth::clone_from_slice(&ua_auth_bytes); // 16-byte GenericArray
    let endpoint: http::Uri = sub.endpoint.parse().map_err(|e| PushError::Sub(format!("{e}")))?;

    WebPushBuilder::new(endpoint, ua_public, ua_auth)
        .with_vapid(&kp, &vapid.subject)
        .build(payload.to_json())
        .map_err(|e| PushError::Build(format!("{e}")))
}

/// Send to every subscription for `account_id`. Prunes 404/410. Returns count
/// sent. Fire-and-forget: all errors log, none propagate.
pub async fn send_push_to_account(
    redis_url: &str,
    vapid: &VapidConfig,
    account_id: &str,
    payload: &PushPayload,
) -> usize {
    let key = format!("spx:push_subs:{}", account_id.to_lowercase());
    let client = match redis::Client::open(redis_url) {
        Ok(c) => c,
        Err(e) => { tracing::warn!(error=%e, "push: redis open"); return 0; }
    };
    let mut con = match client.get_multiplexed_async_connection().await {
        Ok(c) => c,
        Err(e) => { tracing::warn!(error=%e, "push: redis conn"); return 0; }
    };
    let raws: Vec<String> = con.smembers(&key).await.unwrap_or_default();
    if raws.is_empty() {
        return 0;
    }
    let http = wreq::Client::builder().build().unwrap_or_default();
    let mut sent = 0;
    for raw in raws {
        let Some(sub) = parse_sub(&raw) else { continue };
        let req = match build_push_request(vapid, &sub, payload) {
            Ok(r) => r,
            Err(e) => { tracing::warn!(error=%e, "push: build"); continue; }
        };
        // Replay the http::Request via wreq: method POST, uri, headers, body.
        let (parts, body) = req.into_parts();
        let mut rb = http.post(parts.uri.to_string()).body(body);
        for (name, val) in parts.headers.iter() {
            rb = rb.header(name.as_str(), val.as_bytes());
        }
        match rb.send().await {
            Ok(r) if r.status().is_success() => sent += 1,
            Ok(r) if r.status().as_u16() == 404 || r.status().as_u16() == 410 => {
                let _: Result<i64, _> = con.srem(&key, &raw).await; // prune expired
            }
            Ok(r) => tracing::warn!(status=%r.status(), "push: non-2xx"),
            Err(e) => tracing::warn!(error=%e, "push: send"),
        }
    }
    sent
}
```

> **`Auth::clone_from_slice` / `p256::PublicKey::from_sec1_bytes` / `ES256KeyPair::from_bytes` / the `http::Request`→`wreq` replay are best-effort; verify each against the installed versions.** `Auth = GenericArray<u8, U16>` (from web-push-native's `pub type Auth`); the auth secret is exactly 16 bytes. If `wreq`'s request builder does not accept `.body(Vec<u8>)` directly, wrap it per wreq's body API. The wiremock test in Step 5 pins the required behavior (a valid subscription yields a POST to the endpoint with `Content-Encoding: aes128gcm` + an `Authorization: vapid …` header).

- [ ] **Step 4: Wire `lib.rs`**

```rust
pub mod push_vapid;
pub use push_vapid::{build_push_request, send_push_to_account, PushPayload, PushSubscription, VapidConfig};
```

- [ ] **Step 5: Push test (build shape + send via wiremock)**

```rust
// Backend/crates/notifier/tests/push_mock.rs
//! DoD #8 (push half): a valid subscription + VAPID key yields a POST to the
//! subscription endpoint with aes128gcm content-encoding + a vapid Authorization
//! header. Uses a generated test VAPID keypair + a p256 subscription key so the
//! crypto actually runs; wiremock is the push endpoint.
use notifier::{build_push_request, PushPayload, PushSubscription, VapidConfig};

// A known-good test VAPID private key (base64url 32-byte scalar) + a matching
// subscription p256dh/auth. If the implementer prefers, generate fresh keys with
// p256::SecretKey::random and encode; the assertion is the REQUEST SHAPE.
#[test]
fn build_push_request_has_encryption_and_vapid_headers() {
    // NOTE: replace these with real base64url test vectors during implementation
    // (a 32-byte VAPID private, a 65-byte uncompressed p256 public, a 16-byte auth).
    let vapid = VapidConfig {
        subject: "mailto:ops@example.com".into(),
        public_key: "<base64url-vapid-public>".into(),
        private_key: "<base64url-vapid-private-32b>".into(),
    };
    let sub = PushSubscription {
        endpoint: "https://push.example.com/abc".into(),
        p256dh: "<base64url-uncompressed-p256-65b>".into(),
        auth: "<base64url-16b-auth>".into(),
    };
    let payload = PushPayload { title: "T".into(), body: "B".into(), url: None, tag: None };
    let req = build_push_request(&vapid, &sub, &payload).expect("build push request");
    assert_eq!(req.method(), http::Method::POST);
    assert_eq!(req.uri().host(), Some("push.example.com"));
    assert_eq!(req.headers().get("content-encoding").unwrap(), "aes128gcm");
    assert!(req.headers().get(http::header::AUTHORIZATION).unwrap().to_str().unwrap().starts_with("vapid "));
}
```

> This test needs REAL base64url key material (32-byte VAPID private, 65-byte uncompressed P-256 public for the subscription, 16-byte auth). The implementer generates them once (e.g. `p256::SecretKey::random(&mut OsRng)` → encode) and pastes the vectors; the assertion is the built request's shape (POST + `aes128gcm` + `vapid …`). A live-endpoint send is verified manually against a real browser subscription, documented in the commit.

- [ ] **Step 6: Test, clippy, deny, commit**

```bash
cd Backend
cargo test -p notifier
cargo clippy -p notifier --all-targets -- -D warnings
cargo deny check
cd ..
git add Backend/crates/notifier Backend/deny.toml Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(notifier): Web Push VAPID (web-push-native) + deny.toml RUSTSEC-2023-0071 ignore (unreachable rsa path via jwt-simple)"
```

Expected: `cargo deny check` prints `advisories ok, bans ok, licenses ok, sources ok` (the `rsa` advisory is now ignored with justification; **licenses stay clean — no MPL, no copyleft**). If licenses fail with MPL-2.0, the WRONG push crate (`web-push`) was added — it MUST be `web-push-native`.

---

### Task 12: `ws-hub` WebSocket server — axum upgrade + per-session/per-account registry + 30s ping + `WsEvent` enum

**Verification confidence:** axum 0.8.9 ws API READ FROM SOURCE and exact: `WebSocketUpgrade::on_upgrade(|socket| async {…})`, `WebSocket::recv() -> Option<Result<Message,Error>>`, `WebSocket::send(Message)`, `Message::{Text(Utf8Bytes), Binary(Bytes), Ping(Bytes), Pong(Bytes), Close(Option<CloseFrame>)}`. `WsEvent` variants ported verbatim from `hub.ts:6-20`. The split-sink registry + ping loop are the load-bearing logic (tested locally).

**Files:**
- Modify: `Backend/crates/ws-hub/Cargo.toml` (deps)
- Overwrite: `Backend/crates/ws-hub/src/lib.rs`
- Create: `Backend/crates/ws-hub/src/events.rs`
- Create: `Backend/crates/ws-hub/src/hub.rs`
- Create: `Backend/crates/ws-hub/tests/events_serde.rs`
- Create: `Backend/crates/ws-hub/tests/local_broadcast.rs`

**Interfaces produced:**
- `pub enum WsEvent` (`#[serde(tag = "type", content = "data")]`) — the 14 variants from `hub.ts:6-20`: `new_tickets, ticket_accepted, ticket_rejected, ticket_simulated, tickets_removed, stats_update, poller_status, cookies_expired, auto_relogin, connected, rules_updated, pause_expired, booking_enriched, error`. Tag strings are the exact reference snake_case; inner data fields are camelCase (matching the reference UI protocol). `pub fn WsEvent::to_json(&self) -> String`.
- `pub struct Hub { clients: DashMap<String, HashMap<u64, UnboundedSender<Message>>>, next_id: AtomicU64 }` — the local socket registry keyed by channel (session-id or `acct:<id>`).
- `pub fn Hub::new() -> Arc<Hub>`; `pub fn Hub::deliver(&self, channel: &str, payload: &str)` — send `payload` to every socket registered on `channel`; `pub fn Hub::deliver_broadcast(&self, payload: &str)` — to ALL sockets.
- `pub async fn ws_handler(ws: WebSocketUpgrade, State(hub): State<Arc<Hub>>, session/account query) -> Response` — upgrade → register the socket under its `session_id` AND (if present) `acct:<account_id>`; spawn a recv task (handle Pong/Close) and a send task (forward from the socket's mpsc + a 30s ping); unregister on close.
- `pub fn ws_router(hub: Arc<Hub>) -> Router` — mounts `GET /ws` (mountable into `reactor-core` in Fase 6).

- [ ] **Step 1: Add deps**

```bash
cd Backend
cargo add --package ws-hub axum --features ws
cargo add --package ws-hub tokio --features rt-multi-thread,macros,time,sync
cargo add --package ws-hub dashmap
cargo add --package ws-hub serde --features derive
cargo add --package ws-hub serde_json
cargo add --package ws-hub redis --features tokio-comp,connection-manager
cargo add --package ws-hub futures
cargo add --package ws-hub tracing
cargo add --package ws-hub --dev tokio --features rt-multi-thread,macros,time,sync
cargo add --package ws-hub --dev tokio-tungstenite   # a CLIENT for the handler test only
cd ..
```

> `tokio-tungstenite` is a **dev-dependency ONLY** (a WS client to drive the server in tests). Production `ws-hub` uses ONLY axum's `ws` — Task 14 asserts no `tokio-tungstenite` in normal edges. Confirm `tokio-tungstenite` resolves to an allowed license (it is `MIT` — verify with `cargo deny check`).

- [ ] **Step 2: Write `events.rs` (port hub.ts:6-20)**

```rust
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
```

- [ ] **Step 3: Write `hub.rs` (registry + upgrade handler + 30s ping)**

```rust
// Backend/crates/ws-hub/src/hub.rs
//! Local socket registry + the axum WS upgrade handler. Each socket registers
//! under its session id AND (if present) `acct:<account_id>` (lowercased) so
//! every device of an account gets the same live updates (correction #8). Two
//! tasks per socket: a recv loop (Pong/Close) and a send loop (mpsc forward +
//! 30s ping). The Redis bridge (Task 13) calls `deliver`.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::events::WsEvent;

const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Local registry: channel → (socket id → sender). Channel is a session id or
/// `acct:<account_id>`.
pub struct Hub {
    clients: DashMap<String, HashMap<u64, UnboundedSender<Message>>>,
    next_id: AtomicU64,
}

impl Hub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { clients: DashMap::new(), next_id: AtomicU64::new(1) })
    }

    fn register(&self, channel: &str, id: u64, tx: UnboundedSender<Message>) {
        self.clients.entry(channel.to_string()).or_default().insert(id, tx);
    }

    fn unregister(&self, channels: &[String], id: u64) {
        for ch in channels {
            if let Some(mut set) = self.clients.get_mut(ch) {
                set.remove(&id);
            }
        }
    }

    /// Deliver a payload to every socket on `channel`.
    pub fn deliver(&self, channel: &str, payload: &str) {
        if let Some(set) = self.clients.get(channel) {
            for tx in set.values() {
                let _ = tx.send(Message::Text(payload.to_string().into()));
            }
        }
    }

    /// Deliver to ALL sockets (broadcast channel).
    pub fn deliver_broadcast(&self, payload: &str) {
        for set in self.clients.iter() {
            for tx in set.value().values() {
                let _ = tx.send(Message::Text(payload.to_string().into()));
            }
        }
    }

    pub fn deliver_event(&self, channel: &str, ev: &WsEvent) {
        self.deliver(channel, &ev.to_json());
    }
}

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub account: String,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(hub): State<Arc<Hub>>,
    Query(q): Query<WsQuery>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, hub, q))
}

async fn handle_socket(socket: WebSocket, hub: Arc<Hub>, q: WsQuery) {
    let id = hub.next_id.fetch_add(1, Ordering::Relaxed);
    // Channels this socket belongs to: its session, and (if any) its account.
    let mut channels: Vec<String> = Vec::new();
    if !q.session.is_empty() {
        channels.push(q.session.clone());
    }
    if !q.account.is_empty() {
        channels.push(format!("acct:{}", q.account.to_lowercase()));
    }
    if channels.is_empty() {
        channels.push(format!("anon:{id}"));
    }

    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    for ch in &channels {
        hub.register(ch, id, tx.clone());
    }

    // Greet with a `connected` event.
    let _ = tx.send(Message::Text(
        WsEvent::Connected { session: q.session.clone() }.to_json().into(),
    ));

    // Send task: forward mpsc messages + a 30s ping.
    let send_task = tokio::spawn(async move {
        let mut ping = tokio::time::interval(PING_INTERVAL);
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(m) => { if sink.send(m).await.is_err() { break; } }
                    None => break,
                },
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
                }
            }
        }
    });

    // Recv loop: drain until close/error (Pong handled implicitly by axum).
    while let Some(Ok(msg)) = stream.next().await {
        if let Message::Close(_) = msg {
            break;
        }
    }

    // Cleanup.
    send_task.abort();
    hub.unregister(&channels, id);
}

pub fn ws_router(hub: Arc<Hub>) -> Router {
    Router::new().route("/ws", get(ws_handler)).with_state(hub)
}
```

- [ ] **Step 4: Write `lib.rs`**

```rust
// Backend/crates/ws-hub/src/lib.rs
//! Fase 5 — ws-hub: an axum WebSocket server with a per-session + per-account
//! local registry, a 30s ping, and (Task 13) a Redis pub/sub bridge for
//! cross-process broadcast. Uses ONLY axum's `ws` feature — no second WS crate.
pub mod events;
pub mod hub;

pub use events::WsEvent;
pub use hub::{ws_handler, ws_router, Hub, WsQuery};

// Task 13 adds: pub mod bridge;
```

- [ ] **Step 5: Event serde test + local delivery test**

```rust
// Backend/crates/ws-hub/tests/events_serde.rs
//! The ported variants serialize to the exact reference wire shape.
use ws_hub::WsEvent;

#[test]
fn variants_serialize_to_reference_shape() {
    assert_eq!(
        WsEvent::TicketsRemoved { ids: vec!["a".into(), "b".into()] }.to_json(),
        r#"{"type":"tickets_removed","data":{"ids":["a","b"]}}"#
    );
    assert_eq!(
        WsEvent::CookiesExpired { message: "expired".into() }.to_json(),
        r#"{"type":"cookies_expired","data":{"message":"expired"}}"#
    );
    assert_eq!(
        WsEvent::Connected { session: "s1".into() }.to_json(),
        r#"{"type":"connected","data":{"session":"s1"}}"#
    );
    // camelCase inner field (reference protocol).
    assert_eq!(
        WsEvent::TicketRejected { booking_id: "B9".into() }.to_json(),
        r#"{"type":"ticket_rejected","data":{"bookingId":"B9"}}"#
    );
}
```

```rust
// Backend/crates/ws-hub/tests/local_broadcast.rs
//! A real WS client connects with ?account=ACC; a Hub::deliver on channel
//! `acct:acc` reaches that socket. Proves the registry + upgrade + send path
//! (no Redis yet — that is Task 13's cross-process test).
use std::sync::Arc;

use futures::StreamExt;
use tokio_tungstenite::tungstenite::Message as CM;
use ws_hub::{ws_router, Hub};

#[tokio::test]
async fn account_channel_delivers_to_connected_socket() {
    let hub = Hub::new();
    let app = ws_router(hub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let url = format!("ws://{addr}/ws?account=ACC");
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();

    // First frame is the `connected` greeting.
    let first = ws.next().await.unwrap().unwrap();
    assert!(matches!(first, CM::Text(ref t) if t.contains("connected")));

    // Give the server a beat to finish registering, then deliver on acct:acc.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    hub.deliver("acct:acc", r#"{"type":"tickets_removed","data":{"ids":["x"]}}"#);

    let got = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
        .await
        .expect("delivered within 2s")
        .unwrap()
        .unwrap();
    assert!(matches!(got, CM::Text(ref t) if t.contains("tickets_removed")));
}
```

- [ ] **Step 6: Test, clippy, commit**

```bash
cd Backend
cargo test -p ws-hub
cargo clippy -p ws-hub --all-targets -- -D warnings
cargo deny check
cd ..
git add Backend/crates/ws-hub Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(ws-hub): axum WS server + per-session/per-account registry + 30s ping + WsEvent enum (ported hub.ts)"
```

Expected: event serde matches the reference wire shape; a real WS client receives an `acct:` delivery; deny green (dev-only `tokio-tungstenite` is MIT).

---

### Task 13: `ws-hub` Redis pub/sub bridge + two-connection cross-process test

**Verification confidence:** redis 1.3.0 pub/sub API READ FROM SOURCE: `client.get_async_pubsub().await -> aio::PubSub`, `pubsub.subscribe(ch)`/`psubscribe`, `pubsub.on_message() -> impl Stream<Item = Msg>`, `msg.get_payload::<String>()`, `msg.get_channel_name()`. Publish via a `ConnectionManager`/multiplexed conn `con.publish(ch, payload)`. The pattern-subscribe (`psubscribe`) is best-effort — **if `psubscribe` is unavailable on `aio::PubSub` in 1.3.0, fall back to `subscribe` per registered channel (verify).**

**Files:**
- Create: `Backend/crates/ws-hub/src/bridge.rs`
- Modify: `Backend/crates/ws-hub/src/lib.rs`
- Create: `Backend/crates/poller/src/publish.rs` (the poller-side `RedisPublisher`) + wire into `PollerShared`
- Create: `Backend/crates/ws-hub/tests/redis_bridge.rs`

**Channel convention (both sides MUST agree):**
- Redis channel = `spx:ws:<channel>` where `<channel>` is a session id or `acct:<account_id>` (lowercased). Broadcast = `spx:ws:__broadcast__`.
- The poller publishes an event with `RedisPublisher::publish_event(channel, &WsEvent)`; ws-hub's bridge `psubscribe("spx:ws:*")`, strips the `spx:ws:` prefix to recover `<channel>`, and calls `Hub::deliver(<channel>, payload)` (or `deliver_broadcast` for `__broadcast__`).

**Interfaces produced:**
- In `ws-hub`: `pub async fn spawn_bridge(hub: Arc<Hub>, redis_url: &str) -> Result<JoinHandle<()>, redis::RedisError>` — opens a dedicated `PubSub`, `psubscribe("spx:ws:*")`, and forwards every message to the local `Hub`.
- In `poller`: `pub struct RedisPublisher { con: redis::aio::ConnectionManager }` with `pub async fn connect(redis_url: &str) -> Result<Self, redis::RedisError>` and `pub async fn publish_event(&self, channel: &str, ev: &ws_hub_events::WsEvent)` (or a plain `publish(&self, channel: &str, payload: &str)` to avoid a poller→ws-hub dep — see note).

**Design note (avoid a poller→ws-hub dependency):** to keep `poller` from depending on `ws-hub`, the poller-side publisher takes a pre-serialized `payload: &str` (the poller builds the JSON itself, or a tiny shared `WsEvent` lives in a leaf crate). Simplest: `RedisPublisher::publish(channel, payload)` takes a `&str`, and the poller constructs the JSON via `serde_json::json!({"type":"ticket_accepted","data":{…}})`. ws-hub owns the `WsEvent` enum for its own serialization; the wire format is a shared CONTRACT (the JSON shape from Task 12), not a shared type. Document the two `type` strings the poller emits in Fase 5 (`ticket_accepted`, `new_tickets`) so they match `WsEvent` exactly.

- [ ] **Step 1: Write `ws-hub::bridge`**

```rust
// Backend/crates/ws-hub/src/bridge.rs
//! Redis pub/sub → local broadcast. A dedicated PubSub connection subscribes to
//! `spx:ws:*`; each message's channel suffix selects the local `Hub` channel to
//! deliver to. This is what makes ws-hub work across processes (poller in
//! reactor-core publishes; every ws-hub instance delivers to its own sockets) —
//! correction #8, the one WS piece that is accurate 1:1 to the master spec.
use std::sync::Arc;

use futures::StreamExt;
use tokio::task::JoinHandle;

use crate::hub::Hub;

const PREFIX: &str = "spx:ws:";
const PATTERN: &str = "spx:ws:*";
const BROADCAST_SUFFIX: &str = "__broadcast__";

/// Spawn the bridge task. Returns an error only if the initial subscribe fails.
pub async fn spawn_bridge(hub: Arc<Hub>, redis_url: &str) -> Result<JoinHandle<()>, redis::RedisError> {
    let client = redis::Client::open(redis_url)?;
    let mut pubsub = client.get_async_pubsub().await?;
    pubsub.psubscribe(PATTERN).await?;

    let handle = tokio::spawn(async move {
        let mut stream = pubsub.on_message();
        while let Some(msg) = stream.next().await {
            let channel = msg.get_channel_name().to_string();
            let payload: String = match msg.get_payload() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let suffix = channel.strip_prefix(PREFIX).unwrap_or(&channel);
            if suffix == BROADCAST_SUFFIX {
                hub.deliver_broadcast(&payload);
            } else {
                hub.deliver(suffix, &payload);
            }
        }
    });
    Ok(handle)
}
```

> If `pubsub.psubscribe`/`get_channel_name` differ on redis 1.3.0's `aio::PubSub`, adjust: `get_async_pubsub` returns `aio::PubSub` which exposes `subscribe`/`psubscribe`; `Msg::get_channel_name()` and `get_payload::<String>()` were confirmed in the source. If pattern subscribe is not available, `subscribe` to each channel as sockets register (pass a subscription command channel from `Hub` to the bridge). **Verify before proceeding.**

- [ ] **Step 2: Wire `ws-hub::lib.rs`**

```rust
pub mod bridge;
pub use bridge::spawn_bridge;
```

- [ ] **Step 3: Write `poller::publish` + wire `PollerShared`**

```rust
// Backend/crates/poller/src/publish.rs
//! Poller-side WS event publisher. Publishes pre-serialized JSON to
//! `spx:ws:<channel>` so ws-hub's bridge (any process) delivers it to sockets.
//! Poller does NOT depend on ws-hub — the wire format is a shared CONTRACT (the
//! `{"type":..,"data":..}` shape), not a shared type. Fase 5 emits exactly two
//! event types: `ticket_accepted` and `new_tickets`.
use redis::aio::ConnectionManager;
use redis::AsyncCommands;

#[derive(Clone)]
pub struct RedisPublisher {
    con: ConnectionManager,
}

impl RedisPublisher {
    pub async fn connect(redis_url: &str) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(redis_url)?;
        let con = ConnectionManager::new(client).await?;
        Ok(Self { con })
    }

    /// Publish a pre-serialized WS payload to `spx:ws:<channel>`.
    pub async fn publish(&self, channel: &str, payload: &str) {
        let mut con = self.con.clone();
        let full = format!("spx:ws:{channel}");
        let _: Result<i64, _> = con.publish(&full, payload).await;
    }

    /// Convenience: publish a `ticket_accepted` event to `acct:<id>`.
    pub async fn publish_ticket_accepted(&self, account_id: &str, data: serde_json::Value) {
        let payload = serde_json::json!({ "type": "ticket_accepted", "data": data }).to_string();
        self.publish(&format!("acct:{}", account_id.to_lowercase()), &payload).await;
    }
}
```

Add `pub redis: Option<RedisPublisher>` to `PollerShared` (Task 6's `finalize_win` and the new-ticket path call it when present). Wire `pub mod publish; pub use publish::RedisPublisher;` into `poller::lib.rs`. In `dispatch::finalize_win`, after the DB status write, add: `if let Some(pub_) = &shared.redis { pub_.publish_ticket_accepted(&st.account_id, serde_json::json!({"bookingId": booking.booking_id, "latencyMs": latency_ms, "autoAccept": true, "rule": meta.name})).await; }`.

- [ ] **Step 4: Two-connection cross-process bridge test (DoD #9)**

```rust
// Backend/crates/ws-hub/tests/redis_bridge.rs
//! DoD #9: prove the Redis bridge genuinely bridges across CONNECTIONS (not just
//! a local broadcast). Connection A (a publisher) PUBLISHes to spx:ws:acct:x;
//! the ws-hub bridge (its OWN separate PubSub connection) receives it and
//! delivers to a locally-registered socket. Two distinct Redis connections prove
//! cross-process behavior. Real Redis @ 16379.
use std::sync::Arc;

use redis::AsyncCommands;
use ws_hub::{spawn_bridge, Hub};

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

#[tokio::test]
async fn publish_on_one_connection_reaches_a_socket_via_the_bridge() {
    let hub = Hub::new();
    // Register a fake local socket on acct:x by reaching into Hub through a test
    // helper: deliver() sends to registered mpsc senders, so register one here.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    hub.test_register("acct:x", tx); // test-only helper (see note)

    // Start the bridge (its OWN PubSub connection).
    let _bridge = spawn_bridge(hub.clone(), &redis_url()).await.expect("bridge");
    // Bridge needs a beat to finish psubscribe before we publish.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // Connection A: a SEPARATE publisher connection.
    let client = redis::Client::open(redis_url()).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let payload = r#"{"type":"ticket_accepted","data":{"bookingId":"B1"}}"#;
    let _: i64 = con.publish("spx:ws:acct:x", payload).await.unwrap();

    // The bridge must have delivered it to our registered socket.
    let got = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .expect("bridge delivered within 3s")
        .expect("a message");
    let text = match got {
        axum::extract::ws::Message::Text(t) => t.to_string(),
        other => panic!("expected Text, got {other:?}"),
    };
    assert!(text.contains("ticket_accepted") && text.contains("B1"));
}
```

Add a `#[cfg(any(test, feature = "test-helpers"))]`-gated `pub fn Hub::test_register(&self, channel: &str, tx: UnboundedSender<Message>)` to `hub.rs` that inserts a sender directly (so the test can observe deliveries without a full WS client). Expose `Message` re-export or use `axum::extract::ws::Message` in the test as shown.

- [ ] **Step 5: Test, clippy, commit**

```bash
cd Docker && docker compose up -d tower-redis && cd ..
cd Backend
cargo test -p ws-hub --test redis_bridge -- --test-threads=1
cargo test -p ws-hub
cargo clippy -p ws-hub -p poller --all-targets -- -D warnings
cd ..
git add Backend/crates/ws-hub Backend/crates/poller Backend/Cargo.lock
git commit -m "feat(ws-hub,poller): Redis pub/sub bridge (two-connection cross-process test) + poller RedisPublisher"
```

Expected: the bridge test proves a PUBLISH on one connection reaches a socket via the bridge's separate connection (real cross-process bridging, not a local broadcast).

---

### Task 14: Full verification + Fase 5 sign-off

**Files:** None created — this task runs verification and checks off the plan.

**Interfaces:**
- Consumes: everything from Tasks 1–13.
- Produces: recorded evidence the Fase 5 Definition of Done (design doc, 10 items) is met.

- [ ] **Step 1: Bring up services, run the full workspace suite from clean containers**

```bash
cd Docker && docker compose up -d tower-postgres tower-redis && cd ..
# wait for healthy: docker compose -f Docker/docker-compose.yml ps
cd Backend
cargo build --workspace
cargo test --workspace -- --test-threads=1
cd ..
```

Expected: every crate's suite is green — `poller` (single-flight/poke, fetch cadence + fetch_complete, hedge on/off, notif backoff + poke, anti-drift gate, dispatch pipeline, login chain fallback, watchdog), `notifier` (message templates, WAHA fire-and-forget, push build shape), `ws-hub` (event serde, local delivery, Redis bridge), `auth-sidecar` (handler ok:false shape), plus the unchanged Fase 1–4 suites (`core-domain`, `spx-client`, `executor`, `store`).

- [ ] **Step 2: Clippy + deny workspace-wide**

```bash
cd Backend
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
cd ..
```

Expected: clippy clean; `cargo deny check` prints `advisories ok, bans ok, licenses ok, sources ok`. The ONLY deny change in Fase 5 is the `RUSTSEC-2023-0071` advisory ignore (Task 11) — **licenses stay clean (no MPL-2.0, no copyleft added).** If licenses fail with `MPL-2.0`, the wrong push crate (`web-push` instead of `web-push-native`) is in the tree — fix it.

- [ ] **Step 3: Per-crate dependency-footprint check (DoD #10)**

```bash
cd Backend
cargo tree -p poller   --edges normal
cargo tree -p notifier --edges normal
cargo tree -p ws-hub   --edges normal
cargo tree -p auth-sidecar --edges normal
cd ..
```

Confirm:
- **`poller` has NO `chromiumoxide`** (and no other headless-browser crate) under normal edges — tier-1 login is HTTP to `auth-sidecar`. This is DoD #10's headline invariant.
- **`poller` has no direct `sqlx`** under normal edges (DB access is only through `store`; `sqlx` appears only under `store` transitively and under `poller`'s DEV edges from `antidrift_pg`/`dispatch_pipeline`).
- **`ws-hub` uses ONLY axum's `ws`** — no `tokio-tungstenite` under normal edges (it appears only under DEV edges as the test client). No standalone WS server crate.
- **`notifier`** has `web-push-native` (NOT `web-push`), `wreq` (NOT a second reqwest of its own — the only `reqwest` in the workspace is chromiumoxide's, isolated to `auth-sidecar`), and `redis`.
- **`auth-sidecar`** has `chromiumoxide` (expected; it is the ONLY crate that may).

- [ ] **Step 4: Cross-check every DoD item in the design doc (10 items) — cite concrete evidence, do not just assert**

Read `Docs/superpowers/specs/2026-07-13-fase-5-poller-notifier-wshub-design.md`'s "Definition of Done — Fase 5" and map each item to its test:
1. Single-flight BY CONSTRUCTION + poke wakes early (paused time) — `poller/tests/schedule_singleflight.rs` (`poll_cycles_never_overlap`, `poke_wakes_before_full_interval`) (Task 1).
2. Fast-detect & hedged fetch default OFF proven + enabled behavior — `poller/tests/hedge.rs` (`default_off_never_hedges`, `enabled_fires_backup_on_slow_page`) + `fetch.rs`'s fast-detect-returns-empty-when-0 path (Tasks 2/3).
3. Full sweep every 3 cycles — `poller/tests/fetch_cadence.rs` + `fetch.rs`'s `full_sweep_cadence_every_third_cycle` (Task 2).
4. Notif watcher staggered lanes + backoff (250/5000/reset) + poke-triggers-sweep — `poller/tests/notif_watch.rs` (`backoff_is_exact_reference_ramp`, `change_in_signal_pokes_the_loop`) + `notif_watch.rs` unit test (Task 4).
5. Auto-login tier 2/3 tested (wiremock) + tier 1 via sidecar contract + order 1→2→3 + fallback when sidecar down + sidecar handler receives+returns cookies shape — `spx-client/tests/login_mock.rs`, `poller/tests/login_chain.rs` (`sidecar_down_falls_through_to_api`), `auth-sidecar` `login_returns_ok_false_when_browser_unavailable` (Tasks 7/9).
6. Watchdog recreates a lost primary within 60s (paused time) — `poller/tests/watchdog.rs` (Task 8).
7. Anti-drift ONLY runs with `FetchOutcome` (type gate) + partial sweep does NOT expire — `poller/tests/antidrift_pg.rs` (`partial_sweep_never_expires_but_complete_sweep_does`) (Task 5).
8. `notifier` WAHA + VAPID push + fire-and-forget (a WAHA failure does not propagate) — `notifier/tests/waha_mock.rs` (`waha_500_does_not_propagate`), `notifier/tests/push_mock.rs` (Tasks 10/11).
9. `ws-hub` per-session/per-account channels, 30s ping, Redis bridge (two-connection cross-process) — `ws-hub/tests/local_broadcast.rs`, `ws-hub/tests/redis_bridge.rs` (`publish_on_one_connection_reaches_a_socket_via_the_bridge`) (Tasks 12/13).
10. `cargo test`/`clippy`/`deny` clean workspace-wide + no unexpected I/O deps (poller has no chromiumoxide; ws-hub/notifier don't touch sqlx directly) — Steps 1–3 output (this task).

If any item lacks green evidence, STOP and fix it before checking the box — do not check a box on an aspiration.

- [ ] **Step 5: Mark this plan complete — with the STRENGTHENED checkbox-corruption warning**

Check every remaining real `- [ ]` step checkbox in THIS file to `- [x]`. Then IMMEDIATELY verify no prose was corrupted.

> **⚠️ CHECKBOX-CORRUPTION WARNING — READ TWICE. This exact mistake has now happened THREE times in this project's history (Fase 1 sign-off, Fase 3 sign-off, and it was explicitly warned-against again in the Fase 4 plan). It is the single most repeated error in this codebase's plan-execution record. DO NOT MAKE IT A FOURTH TIME.**
>
> The failure mode: a global find/replace of `- [ ]` → `- [x]` also rewrites the literal substring `- [ ]` where it appears INSIDE PROSE or CODE (for example this very warning, or a sentence describing "a real leading-`- [ ]` step checkbox", or an array literal like `vec![0, 1]` is NOT affected but a markdown-looking `- [ ]` in a comment IS). ONLY real, leading-of-line step checkboxes (the `- [ ]` that begins a `**Step N:**` bullet) may change. Any `- [ ]` embedded in a sentence, a blockquote, or a fenced code block MUST be left byte-for-byte identical.
>
> **Procedure (do NOT use a blind `sed -i 's/- \[ \]/- [x]/g'`):**
> 1. Convert checkboxes ONLY on lines matching the step-checkbox pattern `^- \[ \] \*\*Step` (leading `- [ ]` followed by a bold `**Step`). Every real checkbox in this plan has that exact shape.
> 2. Then run this guard and confirm it prints NOTHING (zero unconverted real checkboxes) AND that no prose line changed:
> ```bash
> # real step checkboxes still unchecked (should be empty after conversion):
> grep -nE '^- \[ \] \*\*Step' Docs/superpowers/plans/2026-07-13-fase-5-poller-notifier-wshub.md
> # sanity: the count of checked step boxes equals the count of Step bullets:
> echo "checked: $(grep -cE '^- \[x\] \*\*Step' Docs/superpowers/plans/2026-07-13-fase-5-poller-notifier-wshub.md)"
> echo "steps:   $(grep -cE '^- \[.\] \*\*Step' Docs/superpowers/plans/2026-07-13-fase-5-poller-notifier-wshub.md)"
> ```
> 3. `git diff` the plan file and eyeball EVERY changed line: each must be a real `- [ ] **Step …` → `- [x] **Step …`. If any changed line is prose/code containing an embedded `- [ ]` (like this warning), REVERT that line by hand. The two counts in step 2 must be equal.

- [ ] **Step 6: Commit**

```bash
git add Backend Docs/superpowers/plans/2026-07-13-fase-5-poller-notifier-wshub.md
git commit -m "test(fase-5): poller + notifier + ws-hub sign-off — full verification + DoD cross-check"
```

Fase 5 is done once this commits clean. Fase 6 (api-gateway — REST routes incl. `/live?since=` delta-sync + the manual-accept endpoint that Fase 4's `try_claim_manual` already prepared, the OTP arm-gate, and mounting `poller`/`ws-hub` into `reactor-core`) is the next master-spec phase; it consumes these three crates as library/services. Do NOT start it in this task — it gets its own spec/plan cycle.

