// Backend/crates/poller/tests/watchdog.rs
//! Task 8 DoD: the durable-primary watchdog.
//!
//! Four tests, deliberately split by what real I/O each one needs, mirroring
//! this crate's own established convention (see `src/hedge.rs`'s
//! `race_tests` module doc: a real wiremock socket "cannot use
//! `tokio::time::pause`" for governing ITS delay — so tests that must GOVERN
//! timing via the paused clock keep their I/O to small, fast, ungated calls
//! (Redis only), while tests that exercise the full real pipeline (wiremock +
//! Postgres) run on real time instead, relying on the documented fact that
//! `tokio::time::interval`'s FIRST `tick()` resolves immediately — so
//! "recreate on dead" is observable without any wall-clock wait at all):
//!
//! 1. `heartbeat_writes_a_ttl_key_to_redis` — real time, real Redis only:
//!    direct proof of the `heartbeat()` helper's SET+EX.
//! 2. `watchdog_is_a_noop_when_no_primary_account_configured` — real time,
//!    no I/O: the `primary.is_empty()` early return.
//! 3. `watchdog_recreates_a_dead_primary_and_preserves_restore_contract` —
//!    real time (immediate first tick), real Redis + real Postgres (unused,
//!    only to satisfy `PollerShared`'s type) + wiremock SPX (empty pages, so
//!    no Postgres writes ever actually happen): a GENUINELY dead task (one
//!    that panicked) is recreated via the REAL `ensure_restored_then_spawn`,
//!    and the restore-before-first-poll contract (Task 6/CP-7) is proven to
//!    have run as part of that respawn.
//! 4. `watchdog_does_not_respawn_a_healthy_running_primary` — PAUSED clock,
//!    advanced past TWO 60s cycles: a genuinely alive task (never resolves)
//!    must never trigger `respawn`. The only I/O under the paused clock here
//!    is the watchdog's own small Redis heartbeat SET each cycle.
//! 5. `watchdog_respawns_a_primary_that_dies_between_ticks_counter_only` —
//!    PAUSED clock, THREE ticks (t=0, t=60s, t=120s), respawn closure does
//!    ZERO real I/O (just an `AtomicUsize::fetch_add`). Closes the review
//!    gap left by (3) and (4): neither of those proves the watchdog keeps
//!    re-checking on a tick AFTER the first. This test seeds a healthy
//!    primary, advances past ticks 1 and 2 (0 respawns), kills the primary
//!    mid-run, then advances past tick 3 (exactly 1 respawn) — proving
//!    periodic re-detection, not just boot-time detection.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use dashmap::DashMap;
use executor::{AccountDedupState, ExecutorHandle, RedisPool};
use poller::{spawn_watchdog, PollerConfig, PollerShared, PollerState, SidecarClient};
use redis::AsyncCommands;
use secrecy::SecretString;
use spx_client::{SpxClient, SpxCookies};
use tokio::sync::{oneshot, Notify};
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}
fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

fn acct(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4().simple())
}

/// Poll `cond` on a real, small budget (this crate's established convention
/// for real-I/O tests — see `poke_pool_changed.rs::wait_for_request_count` /
/// `restore_before_first_poll.rs::wait_for_status`), returning whether it
/// became true within the budget.
async fn wait_until_real(mut cond: impl FnMut() -> bool, budget: Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        if cond() {
            return true;
        }
        if start.elapsed() > budget {
            return cond();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Step the PAUSED clock forward `total` of virtual time, in small `step`
/// increments (this crate's established pattern — see
/// `schedule_singleflight.rs::advance_until` — a single large `advance()` can
/// jump past a timer that has not yet been (re-)registered by a task that
/// hasn't had a poll turn). Each step also yields the executor a turn, so any
/// pending real I/O (e.g. the watchdog's Redis heartbeat write) gets a chance
/// to make progress between virtual-time jumps.
async fn advance_paused(total: Duration, step: Duration) {
    let mut elapsed = Duration::ZERO;
    while elapsed < total {
        tokio::time::advance(step).await;
        elapsed += step;
    }
}

fn no_dispatch_config(primary: &str) -> PollerConfig {
    PollerConfig {
        // Deliberately huge: only the account loop's FIRST cycle can ever run
        // within these tests' budgets, and its only observable effect must be
        // "hit wiremock once, hit Postgres never" (see the mock: empty pages,
        // so `poll_once`'s per-booking loop and anti-drift both no-op).
        poll_interval_ms: 3_600_000,
        page_size: 10,
        max_pages: 1,
        full_sync_every: 1_000_000, // cadence must never force a full sweep
        fast_detect_pages: 0,       // no extra HTTP, no pool_changed signal
        sweep_hedge_ms: 0,
        notif_watch_ms: 0,
        notif_watch_concurrency: 1,
        primary_account_id: primary.to_string(),
    }
}

// ---------------------------------------------------------------------------
// 1. heartbeat() itself
// ---------------------------------------------------------------------------

#[tokio::test]
async fn heartbeat_writes_a_ttl_key_to_redis() {
    let executor = ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect redis");
    let account = acct("hb");
    let key = format!("spx:poller_heartbeat:{account}");

    let before_ms = chrono::Utc::now().timestamp_millis();
    poller::heartbeat(&executor, &account).await;
    let after_ms = chrono::Utc::now().timestamp_millis();

    let raw_pool = RedisPool::open(&redis_url()).expect("open");
    let mut con = raw_pool.conn().await.expect("conn");
    let val: Option<i64> = con.get(&key).await.expect("get");
    let ttl: i64 = con.ttl(&key).await.expect("ttl");
    let _: () = con.del(&key).await.expect("del"); // clean up

    let v = val.expect("heartbeat() must have written spx:poller_heartbeat:<acct>");
    assert!(
        v >= before_ms && v <= after_ms,
        "value must be the current epoch-ms timestamp: got {v}, window [{before_ms}, {after_ms}]"
    );
    assert!(
        ttl > 0 && ttl <= 120,
        "TTL must be set and at most 120s: got {ttl}"
    );
}

// ---------------------------------------------------------------------------
// 2. no-primary-configured early return
// ---------------------------------------------------------------------------

#[tokio::test]
async fn watchdog_is_a_noop_when_no_primary_account_configured() {
    let executor = Arc::new(
        ExecutorHandle::connect(&redis_url())
            .await
            .expect("connect redis"),
    );
    let client = Arc::new(SpxClient::new("http://127.0.0.1:1").expect("client"));
    let pool = store::connect(&database_url()).await.expect("connect pg");
    let shared = Arc::new(PollerShared {
        executor,
        client,
        pool,
        config: no_dispatch_config(""), // EMPTY primary_account_id
        accounts: Arc::new(DashMap::new()),
        notifier: None,
        redis: None,
        sidecar: Arc::new(SidecarClient::new("http://127.0.0.1:1")),
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });

    let respawn_calls = Arc::new(AtomicUsize::new(0));
    let rc = respawn_calls.clone();
    let respawn: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |_id: String| {
        rc.fetch_add(1, Ordering::SeqCst);
    });

    // The task's `primary.is_empty()` branch returns immediately — no
    // interval tick is ever awaited, so this resolves on real time with no
    // wall-clock cost at all (no `advance`/`sleep` needed).
    let handle = spawn_watchdog(shared, respawn);
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("watchdog task must return promptly when no primary is configured")
        .expect("watchdog task must not panic");

    assert_eq!(
        respawn_calls.load(Ordering::SeqCst),
        0,
        "no primary configured -> respawn must never be called"
    );
}

// ---------------------------------------------------------------------------
// 3. dead primary -> recreate via the REAL ensure_restored_then_spawn
// ---------------------------------------------------------------------------

#[tokio::test]
async fn watchdog_recreates_a_dead_primary_and_preserves_restore_contract() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    let executor = Arc::new(
        ExecutorHandle::connect(&redis_url())
            .await
            .expect("connect redis"),
    );

    // wiremock SPX: every page request returns an EMPTY list, so the
    // respawned account's first (background) poll cycle never touches
    // Postgres and never dispatches anything — this test only needs to
    // observe `ensure_restored_then_spawn`'s FIRST step (the Layer-3 Redis
    // restore), not the poll loop's steady state.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/line_haul/agency/booking/bidding/list"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": { "list": [] } })),
        )
        .mount(&server)
        .await;
    let client = Arc::new(SpxClient::new(server.uri()).expect("client"));

    let primary = acct("wd-dead");

    // Simulate a booking accepted in a PREVIOUS process lifetime for this
    // account (Layer-3 durable ZSET) — the restore-before-first-poll
    // contract (Task 6/CP-7) says a FRESH PollerState's dedup must already
    // know this id BEFORE its first poll, and ONLY `ensure_restored_then_spawn`
    // (not a bare `spawn_account_loop`) enforces that.
    executor
        .record_durable_accept(&primary, "probe-1")
        .await
        .expect("seed durable accept");

    let shared = Arc::new(PollerShared {
        executor: executor.clone(),
        client,
        pool,
        config: no_dispatch_config(&primary),
        accounts: Arc::new(DashMap::new()),
        notifier: None,
        redis: None,
        sidecar: Arc::new(SidecarClient::new("http://127.0.0.1:1")),
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });

    // A GENUINELY dead task: it panics (tokio catches the unwind and reports
    // it via the JoinHandle — the process does not crash). Give it real
    // scheduler turns to actually run and finish before asserting on it, so
    // the test's own premise ("this handle is dead") is verified, not assumed.
    let dead_join = tokio::spawn(async {
        panic!("simulated poller crash (expected — this proves is_finished() detection)");
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
        if dead_join.is_finished() {
            break;
        }
    }
    assert!(
        dead_join.is_finished(),
        "sanity: the dead task must have finished (panicked) before the watchdog ever looks at it"
    );
    shared.accounts.insert(
        primary.clone(),
        poller::AccountHandle {
            poke: Arc::new(Notify::new()),
            join: dead_join,
            dedup: Arc::new(AccountDedupState::new()),
            manual_accept: tokio::sync::mpsc::channel(8).0,
        },
    );

    // The respawn closure the mount layer would wire: build a fresh
    // PollerState and drive it through the REAL, crate-public
    // `ensure_restored_then_spawn` — not a hand-rolled shortcut that skips
    // the restore step. A clone of the fresh `dedup` is stashed so the test
    // can inspect it after respawn without reaching into the spawned task.
    let dedup_probe: Arc<StdMutex<Option<Arc<AccountDedupState>>>> = Arc::new(StdMutex::new(None));
    let respawn_calls = Arc::new(AtomicUsize::new(0));
    let shared_for_respawn = shared.clone();
    let dedup_probe_w = dedup_probe.clone();
    let respawn_calls_w = respawn_calls.clone();
    let respawn: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |account_id: String| {
        respawn_calls_w.fetch_add(1, Ordering::SeqCst);
        let shared2 = shared_for_respawn.clone();
        let dedup_probe2 = dedup_probe_w.clone();
        tokio::spawn(async move {
            let dedup = Arc::new(AccountDedupState::new());
            *dedup_probe2.lock().unwrap() = Some(dedup.clone());
            let st = PollerState {
                account_id: account_id.clone(),
                tenant_id: Uuid::nil(),
                agency_id: 1,
                poll_count: 0,
                cookies: SpxCookies::default(),
                consecutive_401s: 0,
                last_pending_count: -1,
                self_email: None,
                dedup,
                last_relogin_attempt_ms: 0,
                // Seeded to TODAY so the empty-sentinel daily-relogin trigger
                // doesn't fire an unrelated HTTP attempt against an unmounted
                // sidecar (same rationale as `restore_before_first_poll.rs`).
                last_daily_relogin_day: poller::wib_day(chrono::Utc::now()),
                username: SecretString::from("u"),
                password: SecretString::from("p"),
                rules: Arc::new(Vec::new()),
                rule_meta: Arc::new(Vec::new()),
                match_state: core_domain::MatchState::default(),
                rules_rx: None,
            };
            let handle = poller::ensure_restored_then_spawn(shared2.clone(), st).await;
            shared2.accounts.insert(account_id, handle);
        });
    });

    let watchdog_handle = spawn_watchdog(shared.clone(), respawn);

    // `tokio::time::interval`'s FIRST tick resolves immediately (no wait
    // needed at all) — real time is used here specifically so this doesn't
    // need to fight a paused clock against the real Redis/Postgres/HTTP I/O
    // the respawn path performs; a small bounded real-time poll is this
    // crate's established way to wait on that (see `wait_until_real`).
    let primary_for_check = primary.clone();
    let recreated = wait_until_real(
        || {
            respawn_calls.load(Ordering::SeqCst) >= 1
                && shared
                    .accounts
                    .get(&primary_for_check)
                    .map(|h| !h.join.is_finished())
                    .unwrap_or(false)
        },
        Duration::from_secs(5),
    )
    .await;
    assert!(
        recreated,
        "watchdog must have called respawn and a live AccountHandle must now be present \
         for the primary within the budget"
    );
    assert_eq!(
        respawn_calls.load(Ordering::SeqCst),
        1,
        "exactly one respawn for the one genuinely-dead cycle observed"
    );

    // THE restore-contract proof: the fresh PollerState's dedup was NOT
    // populated by us — only `ensure_restored_then_spawn`'s internal
    // `executor.restore_accepted_ids` call could have put "probe-1" into it,
    // and it MUST have completed before `ensure_restored_then_spawn` returned
    // (hence before `shared.accounts` was ever updated, which `recreated`
    // above already waited on).
    let dedup = dedup_probe
        .lock()
        .unwrap()
        .clone()
        .expect("respawn must have constructed a PollerState/dedup");
    assert!(
        dedup.is_known("probe-1"),
        "the respawned account's dedup must already know the pre-restart accept — \
         this is only true if respawn went through ensure_restored_then_spawn's \
         restore-before-first-poll contract, not a shortcut straight to spawn_account_loop"
    );

    watchdog_handle.abort();
    if let Some(h) = shared.accounts.get(&primary) {
        h.join.abort();
    };
}

// ---------------------------------------------------------------------------
// 4. healthy primary -> no spurious respawn, across TWO paused 60s cycles
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn watchdog_does_not_respawn_a_healthy_running_primary() {
    // NOTE on why this test's `executor`/`pool` deliberately do NOT point at
    // real infra (unlike every other test in this file): under
    // `start_paused = true`, establishing a NEW real connection (Redis's
    // `ConnectionManager`, or `sqlx::PgPoolOptions::connect`) races its own
    // internal tokio-time-based timeout against the paused clock — with
    // nothing else scheduled, tokio's documented idle-auto-advance (see
    // `schedule_singleflight.rs`'s doc comment on this exact behavior)
    // immediately fast-forwards straight to that timeout, so the connection
    // attempt spuriously fails EVERY time in this environment (verified: both
    // a fresh `RedisPool::open(..).conn()` and `store::connect(..)` were
    // tried here and both reliably errored — `Redis(timed out)` /
    // `PoolTimedOut` — even at virtual t=0). This is exactly the class of
    // problem `src/hedge.rs`'s `race_tests` doc already flags for real
    // sockets and paused time; it turns out to apply to plain connection
    // establishment too, not just to governing an injected delay. This test
    // only needs to prove the DECISION logic (is_finished-gated respawn), so
    // it points `executor`/`pool` at addresses nothing listens on:
    // `heartbeat_set`/`ensure_restored_then_spawn` are never reached anyway
    // (the healthy handle means `respawn` is never called), and
    // `heartbeat()`'s own Redis write is best-effort/error-swallowed by
    // design, so it silently no-ops here without affecting the loop. The
    // real, positive proof that `heartbeat()` genuinely writes a working key
    // lives in `heartbeat_writes_a_ttl_key_to_redis` (real time, real Redis).
    let executor = Arc::new(
        ExecutorHandle::connect("redis://127.0.0.1:16999")
            .await
            .expect("open offline (parses the URL only; no connection attempted yet)"),
    );
    let client = Arc::new(SpxClient::new("http://127.0.0.1:1").expect("client"));
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy(&database_url())
        .expect("build lazy pg pool (no real connection attempted)");

    let primary = acct("wd-healthy");

    // A GENUINELY alive task: `std::future::pending` never resolves — no
    // timers, no I/O, so it is unaffected by the paused clock and can only
    // ever show `is_finished() == false`.
    let alive_join = tokio::spawn(std::future::pending::<()>());
    let shared = Arc::new(PollerShared {
        executor,
        client,
        pool,
        config: no_dispatch_config(&primary),
        accounts: Arc::new(DashMap::new()),
        notifier: None,
        redis: None,
        sidecar: Arc::new(SidecarClient::new("http://127.0.0.1:1")),
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });
    shared.accounts.insert(
        primary.clone(),
        poller::AccountHandle {
            poke: Arc::new(Notify::new()),
            join: alive_join,
            dedup: Arc::new(AccountDedupState::new()),
            manual_accept: tokio::sync::mpsc::channel(8).0,
        },
    );

    let respawn_calls = Arc::new(AtomicUsize::new(0));
    let rc = respawn_calls.clone();
    let respawn: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |_id: String| {
        rc.fetch_add(1, Ordering::SeqCst);
    });

    let watchdog_handle = spawn_watchdog(shared.clone(), respawn);

    // Let the (immediate) first tick run, then advance in small steps past
    // TWO full 60s cycles (130s total) — small steps because a single giant
    // jump can race ahead of a timer that hasn't been (re-)registered yet by
    // a task that hasn't had a poll turn (this crate's own documented
    // `advance_until` rationale in `schedule_singleflight.rs`).
    advance_paused(Duration::from_secs(130), Duration::from_millis(250)).await;

    assert_eq!(
        respawn_calls.load(Ordering::SeqCst),
        0,
        "a healthy, still-running primary must NEVER be respawned, across two full 60s cycles"
    );
    let still_alive = shared
        .accounts
        .get(&primary)
        .map(|h| !h.join.is_finished())
        .unwrap_or(false);
    assert!(
        still_alive,
        "the original AccountHandle must be untouched (still present, still not finished)"
    );

    watchdog_handle.abort();
    if let Some(h) = shared.accounts.get(&primary) {
        h.join.abort();
    };
}

// ---------------------------------------------------------------------------
// 5. periodic cadence: healthy survives ticks 1 & 2, a mid-run death is
//    caught on tick 3 — counter-only respawn closure, ZERO real I/O
// ---------------------------------------------------------------------------

/// Closes the review-found gap: test 3 only proves the watchdog's very
/// FIRST tick (boot-with-already-dead-primary, on real time); test 4's
/// `respawn_calls == 0` assertion is vacuously true even if the interval
/// never ticks again after the first cycle. Neither positively proves the
/// loop keeps re-checking on tick 2, tick 3, ... .
///
/// This test drives a PAUSED clock across THREE ticks (t=0 immediate,
/// t=60s, t=120s) with a respawn closure that does ZERO real I/O — it only
/// increments an `AtomicUsize` — so, unlike test 3, it cannot hit the
/// per-command response-timeout-vs-paused-clock race that forces test 3
/// onto real time (see this file's module doc, and test 4's own comment on
/// the same hazard applying to plain connection establishment).
///
/// Sequence: seed a genuinely-alive primary (an `AccountHandle` whose task
/// blocks on a `oneshot::Receiver`, so it is unaffected by the paused clock
/// until WE decide to kill it) -> advance ~65s (covers the tick at t=0 AND
/// the tick at t=60s) -> assert 0 respawns (healthy primary survives PAST
/// the first tick). Then kill the handle (drop the paired
/// `oneshot::Sender`) -> advance another ~65s (covers the tick at t=120s)
/// -> assert exactly 1 respawn (the watchdog genuinely re-checked and
/// reacted on a LATER tick, not the first).
#[tokio::test(start_paused = true)]
async fn watchdog_respawns_a_primary_that_dies_between_ticks_counter_only() {
    // Same rationale as `watchdog_does_not_respawn_a_healthy_running_primary`
    // above: under a paused clock, establishing a NEW real connection races
    // its own timeout against the paused clock's idle-auto-advance. This
    // test's respawn closure does zero I/O, and `heartbeat()` is
    // best-effort (error-swallowed on a failed connection — see
    // `executor::heartbeat_set`'s `if let Ok(mut con) = ...`), so pointing
    // `executor`/`pool` at addresses nothing listens on is safe and keeps
    // this test entirely on the paused clock.
    let executor = Arc::new(
        ExecutorHandle::connect("redis://127.0.0.1:16999")
            .await
            .expect("open offline (parses the URL only; no connection attempted yet)"),
    );
    let client = Arc::new(SpxClient::new("http://127.0.0.1:1").expect("client"));
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy(&database_url())
        .expect("build lazy pg pool (no real connection attempted)");

    let primary = acct("wd-cadence");

    // A genuinely alive task, under our explicit control: it blocks on a
    // oneshot receiver and only finishes when we drop the paired sender —
    // no timers, no I/O, so it is unaffected by the paused clock until we
    // decide to kill it.
    let (kill_tx, kill_rx) = oneshot::channel::<()>();
    let primary_join = tokio::spawn(async move {
        let _ = kill_rx.await;
    });

    let shared = Arc::new(PollerShared {
        executor,
        client,
        pool,
        config: no_dispatch_config(&primary),
        accounts: Arc::new(DashMap::new()),
        notifier: None,
        redis: None,
        sidecar: Arc::new(SidecarClient::new("http://127.0.0.1:1")),
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });
    shared.accounts.insert(
        primary.clone(),
        poller::AccountHandle {
            poke: Arc::new(Notify::new()),
            join: primary_join,
            dedup: Arc::new(AccountDedupState::new()),
            manual_accept: tokio::sync::mpsc::channel(8).0,
        },
    );

    // Counter-only respawn: zero real I/O at all — exactly what the review
    // asked for — so this closure cannot race any timeout against the
    // paused clock.
    let respawn_calls = Arc::new(AtomicUsize::new(0));
    let rc = respawn_calls.clone();
    let respawn: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |_id: String| {
        rc.fetch_add(1, Ordering::SeqCst);
    });

    let watchdog_handle = spawn_watchdog(shared.clone(), respawn);

    // Stage 1: advance past the immediate first tick (t=0) AND the second
    // cycle's tick (t=60s), with a small buffer past the exact 60s boundary
    // (same margin convention as test 4's 130s-for-two-cycles). The primary
    // is still alive throughout.
    advance_paused(Duration::from_secs(65), Duration::from_millis(250)).await;
    assert_eq!(
        respawn_calls.load(Ordering::SeqCst),
        0,
        "a healthy primary must survive PAST the first tick (t=0 and t=60s) with zero respawns"
    );
    assert!(
        shared
            .accounts
            .get(&primary)
            .map(|h| !h.join.is_finished())
            .unwrap_or(false),
        "sanity: the original handle must still be alive going into stage 2"
    );

    // Kill the primary's task NOW (between the t=60s and t=120s ticks) and
    // give it real scheduler turns to actually finish, so the test's own
    // premise ("this handle is now dead") is verified, not assumed — same
    // pattern as the panic-based dead task in test 3.
    drop(kill_tx);
    let mut now_dead = false;
    for _ in 0..10 {
        tokio::task::yield_now().await;
        if shared
            .accounts
            .get(&primary)
            .map(|h| h.join.is_finished())
            .unwrap_or(false)
        {
            now_dead = true;
            break;
        }
    }
    assert!(
        now_dead,
        "sanity: the primary's AccountHandle must be observably dead before stage 2's tick"
    );

    // Stage 2: advance past the THIRD tick (t=120s) — a tick the watchdog
    // only reaches by having genuinely kept looping past ticks 1 and 2.
    // This is the assertion that proves periodic re-detection, not just
    // boot-time detection.
    advance_paused(Duration::from_secs(65), Duration::from_millis(250)).await;
    assert_eq!(
        respawn_calls.load(Ordering::SeqCst),
        1,
        "a primary that died AFTER surviving two prior ticks must be respawned exactly once \
         on the next (third, t=120s) tick — proving the watchdog re-checks on later ticks, \
         not just the first"
    );

    watchdog_handle.abort();
}
