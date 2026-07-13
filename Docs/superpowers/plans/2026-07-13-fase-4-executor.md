# Fase 4 — executor (3-layer dedup) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `executor` crate — a pure library that prevents double-accept via a 3-layer dedup (in-proc `DashSet`/`DashMap`, a fail-closed Redis Lua gate + a fail-open manual variant sharing the same claim key, and a durable 7-day ZSET restore) plus `verify_agency_dup` (0/500/1500ms acceptor probe) and re-read-in-lock per-rule quota consumption — all callable by Fase 5 (poller) and Fase 6 (api-gateway) without either owning the shared Redis keyspace.

**Architecture:** Layer 1 is one `AccountDedupState { accepting_now: DashSet<String>, accepted_ids: DashMap<String, Instant> }` per account; `DashSet::insert(...) -> bool` gives atomic per-key claim (closing the reference's `has()`+`add()` race). Layer 2 is `ACCEPT_GATE_LUA` (ported byte-for-byte) run via `redis::Script`, whose `invoke_async` already does EVALSHA→(on `NOSCRIPT`)→`SCRIPT LOAD`+retry natively — so no hand-rolled SHA transport is needed. `try_claim_auto` is fail-closed (any Redis error → do not dispatch); `try_claim_manual` shares the exact same `spx:claim:<acct>:<spxId>` key but skips the quota Lua and is fail-open. Layer 3 restores the durable `spx:accepted:<acct>` ZSET (trimmed to 7 days) into Layer 1. Quota is persisted through a new additive `store::consume_rule_quota` (atomic conditional `UPDATE`), guarded by a per-account `tokio::sync::Mutex`. A purely additive `core_domain::find_best_matching_rule_compiled(&[CompiledRule], …) -> Option<usize>` (first-wins) rounds out the Fase 1 caveat.

**Tech Stack (all versions confirmed real via `cargo add --dry-run` + compiled-and-run against a live Redis 7 on 2026-07-13 — see Global Constraints; do not "modernize" the redis API shape from memory, redis 1.x is a recent major that differs from the 0.2x-era APIs most training data reflects):**
- Redis: **`redis` 1.3.0** (MIT) — features `["tokio-comp", "connection-manager", "script"]`. `redis::aio::ConnectionManager` (auto-reconnecting, `Clone`), `redis::Script` (built-in NOSCRIPT fallback), `redis::AsyncCommands`.
- Concurrency: **`dashmap` 6.2.1** (MIT) — `DashSet<String>`, `DashMap<String, Instant>`; `tokio::sync::{Mutex, OnceCell}` (already in the workspace via `tokio`).
- Errors/ids/json: `thiserror` 2, `uuid` 1, `serde_json` 1.
- Workspace path deps: `core-domain`, `spx-client`, `store`.
- Tests: `tokio` (dev features), `wiremock` 0.6 (MIT, dev-dep — for the `verify_agency_dup` acceptor-probe mock).

## Global Constraints

Full context: [`Docs/tower-master-spec.md`](../../tower-master-spec.md) and [`Docs/superpowers/specs/2026-07-13-fase-4-executor-design.md`](../specs/2026-07-13-fase-4-executor-design.md). **Read the design doc before starting; it is the source of truth for scope and exact behavior. Do not redesign.**

**Scope (from the design doc).** Fase 4 builds the executor **library only** — the dedup/dispatch/verify/quota functions that Fase 5 (poller) and Fase 6 (api-gateway, manual accept) will call. Fase 4 does NOT build: the poller state machine, auto-login, HTTP routes, or WA/push delivery. `verify_agency_dup` only returns a signal (`AgencyDupOutcome::LostToAgency { rival_email }`); the notifier that consumes it is Fase 5. `restore_accepted_ids` is a public function Fase 5 MUST `.await` before scheduling the first poll — Fase 4 cannot enforce that ordering (documented in the function's doc-comment, tested only in isolation).

**The 8 reference corrections (from the design doc — these are intentional, not drift):**
1. Layer 1 uses concurrent structures (`DashSet`/`DashMap`), not JS `Set` — `DashSet::insert() -> bool` is atomic check-and-set per key, closing the `has()`+`add()` race a literal port would introduce.
2. Layer 2 uses `SCRIPT LOAD`+`EVALSHA` (via `redis::Script`) rather than the reference's per-call `redis.eval(fullText)` — semantically identical, cheaper transport, and this satisfies the master spec's "EVALSHA" wording literally.
3. Quota is **per-rule** (`accept_rules.max_accept_count`/`accepted_count`), checked atomically inside the same Lua as the claim — not a global quota.
4. `with_account_lock` is an **in-proc `tokio::sync::Mutex` per account**, not a distributed Redis lock (no Redlock).
5. `agency_dup` retry timing is **0 / 500 / 1500 ms** (sleep BEFORE attempts 2 and 3, confirmed exact in the reference).
6. There is **no `unverified` flag stored anywhere** — an inconclusive probe is simply treated as "ours" (reclassify to ok), matching the real reference behavior (the master spec's "optimistik+unverified" overstates it).
7. **Fail-closed applies only to the auto path (`try_claim_auto`).** The manual path (`try_claim_manual`) is deliberately **fail-open** (Redis error → proceed) — a conscious human action, protected from poller contention by the poller's own fail-closed gate.
8. **Manual accept shares the Redis claim key** (`spx:claim:<acct>:<spxId>`) with auto but **skips the per-rule quota check** (a human chose the ticket; quota is irrelevant). Both behaviors are ported separately and explicitly.

**`redis::Script` handles NOSCRIPT natively — do NOT hand-roll SHA transport (verified by reading redis 1.3.0's `src/script.rs`).** `redis::Script::new(code)` computes the SHA1 once at construction (`sha1_smol`), and `ScriptInvocation::invoke_async` runs `EVALSHA <hash> …`; on a `ServerErrorKind::NoScript` error it calls `SCRIPT LOAD` then retries `EVALSHA` exactly once — transparent to the caller. This is precisely the design doc's "SCRIPT LOAD once + EVALSHA + NOSCRIPT-fallback" requirement, so Task 3 stores one `Script` in the handle and calls `.invoke_async` — no manual `EVALSHA`/SHA bookkeeping. The handle's constructor additionally does one best-effort `SCRIPT LOAD` (`gate.prepare_invoke().load_async(...)`) so the first real claim is a single round-trip instead of EVALSHA→NOSCRIPT→LOAD→EVALSHA; errors there are ignored because `invoke_async` reloads on `NOSCRIPT` anyway and Redis may be briefly unavailable at startup.

**`RedisPool` is lazy over `ConnectionManager` (verified against a dead port).** `redis::Client::open(url)` only parses the URL (no I/O). `ConnectionManager::new` on an unreachable port blocks ~9.5s of internal retries then errors — so building the pool eagerly would make the fail-closed test slow and unbuildable. Instead `RedisPool` holds the `Client` plus a `tokio::sync::OnceCell<ConnectionManager>` and connects lazily via `get_or_try_init` with a fast-fail `ConnectionManagerConfig` (`set_number_of_retries(1)`, `set_connection_timeout(Some(500ms))`, `set_response_timeout(Some(500ms))` — note these setters take `Option<Duration>` in 1.3.0). Result: the pool is always constructible offline (so a dead-port pool exercises fail-closed/fail-open at the command level), `conn()` fails in ~150–500ms not 9.5s, `OnceCell` is not poisoned on failure (a later `conn()` retries), and once connected the `ConnectionManager` clone is reused (auto-reconnecting, idiomatic for a long-lived service). `ConnectionManager` is `Clone` (cheap Arc); each op takes a clone.

**Redis command API (redis 1.3.0, via `use redis::AsyncCommands;` — compiled+run-verified against Redis 7):**
- `SET … NX EX`: `con.set_options(key, "1", redis::SetOptions::default().conditional_set(redis::ExistenceCheck::NX).with_expiration(redis::SetExpiry::EX(600)))` returns `bool` (`true` = set, `false` = key existed). Verified: first NX → true, second → false.
- ZSET: `con.zadd(key, member, score_i64)`, `con.zrembyscore(key, min, max) -> usize` (this IS `ZREMRANGEBYSCORE`), `con.zrange(key, 0, -1) -> Vec<String>`, `con.zscore(key, member) -> Option<f64>`.
- SET: `con.sismember(key, m) -> bool`, `con.scard(key) -> usize`, `con.sadd`, `con.srem`, `con.exists(key) -> bool`, `con.del(key)`, `con.expire(key, secs_i64) -> bool`.

**Real-Redis testing standard (this project's convention — no mocks for Redis).** Tests touching Redis connect to the `tower-redis` container at **`redis://127.0.0.1:16379`** (the temporary dev port publish added in Task 1). Use a **unique account id per test** (`format!("t{}", Uuid::new_v4())`) so tests don't collide on shared keys and can run without `FLUSHALL`. Run integration tests with `-- --test-threads=1` (matches Fase 2/3). The `verify_agency_dup` tests (Task 6) use `wiremock` for the SPX HTTP side (not Redis) — that is HTTP, not the Redis "no mocks" rule.

**Byte-exact Lua (do NOT reformat one character).** `ACCEPT_GATE_LUA` is copied verbatim from the design doc — 12 lines, 2-space indents on the nested block. It was byte-diffed against the design doc (identical) and run against Redis 7 confirming the three return values (`1` new claim, `0` duplicate, `-1` quota full), the `DEL` of the claim key on `-1`, and the `SISMEMBER` short-circuit that lets an already-in-flight spxId bypass the cap. Redis Lua scripts are matched by content hash; any character change silently changes the SHA and desynchronizes load/eval. Task 3 includes a test asserting the literal string equals the design doc's.

**Dependency licenses / `cargo deny` — one required allow-list addition (verified 2026-07-13):** Baseline `cargo deny check` on the current workspace **passes** (`advisories ok, bans ok, licenses ok, sources ok`). Adding `redis` 1.3.0 pulls a **new mandatory transitive dependency `xxhash-rust` 0.8.16, licensed `BSL-1.0`** (Boost Software License 1.0), which is NOT yet in the allow-list, so `cargo deny check` will fail until it is added. `xxhash-rust` is **not optional** (used in redis's `src/types.rs`; no `optional = true`), so it cannot be feature-gated away. **BSL-1.0 is a permissive, OSI-approved, FSF Free/Libre, non-copyleft license** (comparable to or more permissive than MIT) — safe to allow for this proprietary codebase (unlike the GPL-3.0 landmine correctly rejected in Fase 3). Task 1 adds `"BSL-1.0"` to `deny.toml`'s allow-list with a justifying comment. `redis` itself is MIT; `dashmap` is MIT; every other new transitive dep (`backon`, `arc-swap`, `combine`, `sha1_smol`, `arcstr`, `num-bigint`, `xxhash-rust` aside) resolves to an already-allowed license (confirmed: `cargo deny check licenses` on the redis+dashmap subtree passes with only `BSL-1.0` added).

**`store` gets one additive, migration-free change (a deviation from the design doc's file list, reasoned).** The design doc says quota is "re-read from `store` … persisted to DB" and lists `store` (not `sqlx`) as an executor dependency, but `store` currently exposes no rule-quota function. To keep all DB access behind `store` (so executor's only I/O deps stay exactly `redis`/`store`/`spx-client`/`tokio`, per DoD #9), Task 5 adds `store::consume_rule_quota` + `store::QuotaConsumeOutcome` + a `store::PgPool` re-export. No schema change, no migration. Executor consumes the `sqlx::Error` from `store` only via `Display` (`.to_string()`) and never names `sqlx::Error`/`sqlx::PgPool` directly, so **executor takes no direct `sqlx` dependency** (Task 8 asserts this).

**`find_best_matching_rule_compiled` is purely additive (Fase 1 caveat).** Add exactly one public function to `core-domain`'s `matching.rs`; change no existing type, function, or test (the 127 existing tests must still pass unchanged). Tie-break is **first-wins** via a manual loop with strict `>` — never `Iterator::max_by_key` (last-wins on ties, which would diverge from the reference on same-rank overlaps).

**Workflow.** Run all `cargo` commands from `Backend/` (workspace root). Bring up Redis with `cd Docker && docker compose up -d tower-redis` (and `tower-postgres` for Task 5). Reuse `store::{connect, run_migrations, begin_tenant_tx}`. The temporary `127.0.0.1:16379` Redis publish (Task 1) and the existing `127.0.0.1:15432` Postgres publish are dev-only and must NOT be removed before Fase 8.

---

### Task 1: `executor` scaffold + Redis connectivity (`RedisPool`, `ExecutorHandle`) + dev port publish + round-trip test

**Files:**
- Modify: `Backend/crates/executor/Cargo.toml` (add deps)
- Modify: `Backend/deny.toml` (add `"BSL-1.0"` to the allow-list)
- Modify: `Docker/docker-compose.yml` (publish `tower-redis` on `127.0.0.1:16379`, mirroring `tower-postgres`)
- Overwrite: `Backend/crates/executor/src/lib.rs`
- Create: `Backend/crates/executor/src/gate.rs` (RedisPool + ExecutorHandle skeleton + ExecutorError + the Lua const, filled in Task 3)
- Create: `Backend/crates/executor/tests/redis_roundtrip.rs`

**Interfaces:**
- Consumes: nothing (first task; `executor` is already a registered workspace member with an empty `lib.rs`).
- Produces (signatures are load-bearing — later tasks depend on them exactly):
  - `pub struct RedisPool` with `pub fn open(url: &str) -> Result<RedisPool, ExecutorError>` and `pub async fn conn(&self) -> Result<redis::aio::ConnectionManager, ExecutorError>`.
  - `pub struct ExecutorHandle` with `pub async fn connect(redis_url: &str) -> Result<ExecutorHandle, ExecutorError>`. Holds `redis: RedisPool`, `gate: redis::Script`, and `account_locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>`.
  - `pub enum ExecutorError { Redis(#[from] redis::RedisError), Db(String) }`.
  - `pub const ACCEPT_GATE_LUA: &str` (declared here, its body is the verbatim script — used by Task 3).

- [ ] **Step 1: Register nothing new; add dependencies**

`executor` is already in `Backend/Cargo.toml`'s `members`. Add its deps (run from `Backend/`):

```bash
cd Backend
cargo add --package executor redis --features tokio-comp,connection-manager,script
cargo add --package executor dashmap
cargo add --package executor thiserror@2
cargo add --package executor uuid --features v4
cargo add --package executor serde_json
cargo add --package executor tokio --features sync,time
cargo add --package executor --path crates/core-domain core-domain
cargo add --package executor --path crates/spx-client spx-client
cargo add --package executor --path crates/store store
cargo add --package executor --dev tokio --features rt-multi-thread,macros,time,sync
cargo add --package executor --dev wiremock
cd ..
```

Expected resolution: `redis` **1.3.0**, `dashmap` **6.2.1**, `wiremock` **0.6.x**. `redis`/`dashmap`/`wiremock` are MIT; the rest are `MIT OR Apache-2.0`. The only license action needed is `BSL-1.0` (Step 2) for `redis`'s mandatory `xxhash-rust` transitive dep.

- [ ] **Step 2: Add `BSL-1.0` to the `deny.toml` allow-list**

Edit `Backend/deny.toml`, adding `"BSL-1.0"` to the `[licenses] allow` array (after `"Zlib",`):

```toml
    "Zlib",
    # BSL-1.0 (Boost Software License 1.0): permissive, OSI-approved, FSF
    # Free/Libre, non-copyleft (MIT-class). Pulled in transitively by `redis`
    # 1.3.0's mandatory `xxhash-rust` dependency (not feature-gateable). Safe
    # for a proprietary codebase — unlike the GPL-3.0 case rejected in Fase 3.
    "BSL-1.0",
```

Do NOT touch `[bans]`, `[sources]`, `[advisories]`, `private = { ignore = true }`, or `confidence-threshold`.

- [ ] **Step 3: Publish `tower-redis` on a temporary dev port (mirror `tower-postgres`)**

In `Docker/docker-compose.yml`, add a `ports:` block to the `tower-redis` service. Use **`127.0.0.1:16379:6379`** — port 6379 is commonly occupied by another Redis on the dev host, so map to 16379 (the same "+10000" convention Fase 2 used for Postgres 15432). Do NOT touch `tower-postgres`'s existing `127.0.0.1:15432:5432` block. Insert the block between `restart: unless-stopped` and `volumes:`:

```yaml
  tower-redis:
    image: redis:7
    container_name: tower-redis
    restart: unless-stopped
    # TEMPORARY (dev-time convenience, mirrors tower-postgres's 15432 publish
    # from Fase 2): publishes Redis to localhost only, for the executor crate's
    # real-Redis tests (`cargo test -p executor`) run from the host outside any
    # container — this project's testing standard uses a real Redis, not mocks.
    # Scoped to 127.0.0.1 so it is not reachable off-box; Fase 8's VPS overlay
    # treats Redis as internal-only, so this does not violate Fase 0's "no
    # published ports except edge" intent. Port 6379 is often taken by a
    # pre-existing Redis on the dev host, so this maps to 16379 instead —
    # executor's tests and every later fase's local dev docs must use 16379.
    # Fase 4 needs this (host-run tests) and so will Fase 5-6 as their crates
    # grow Redis-backed tests — remove only alongside Fase 8's real deployment
    # tooling, not before.
    ports:
      - "127.0.0.1:16379:6379"
    volumes:
      - tower-redis-data:/data
    networks:
      - tower-net
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 10s
      timeout: 3s
      retries: 5
```

- [ ] **Step 4: Write `gate.rs` (pool, handle skeleton, error, Lua const)**

The `ACCEPT_GATE_LUA` body here is the verbatim design-doc script; Task 3 adds the `try_claim_*` methods that use it. Reproduce the Redis API shape exactly (see Global Constraints — it was compiled and run against Redis 7).

```rust
// Backend/crates/executor/src/gate.rs
//! Layer 2 — the Redis claim gate (`ACCEPT_GATE_LUA` + transport) plus the
//! shared `RedisPool`/`ExecutorHandle`/`ExecutorError` types.
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::Script;
use tokio::sync::{Mutex, OnceCell};

/// Ported byte-for-byte from the Fase 4 design doc (do NOT reformat — Redis
/// hashes script content; any change desynchronizes EVALSHA). Returns `1` (new
/// claim), `0` (already claimed), or `-1` (per-rule quota full). `KEYS[1]` =
/// `spx:claim:<acct>:<spxId>`, `KEYS[2]` = `spx:inflight:<acct>:<ruleId|_norule>`;
/// `ARGV = [spxId, cap, acceptedCount, "600"]`.
pub const ACCEPT_GATE_LUA: &str = r#"local ok = redis.call('SET', KEYS[1], '1', 'NX', 'EX', ARGV[4])
if not ok then return 0 end
local cap = tonumber(ARGV[2])
if cap > 0 then
  if redis.call('SISMEMBER', KEYS[2], ARGV[1]) == 0 and (tonumber(ARGV[3]) + redis.call('SCARD', KEYS[2])) >= cap then
    redis.call('DEL', KEYS[1])
    return -1
  end
  redis.call('SADD', KEYS[2], ARGV[1])
  redis.call('EXPIRE', KEYS[2], 600)
end
return 1"#;

#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    #[error("redis error")]
    Redis(#[from] redis::RedisError),
    #[error("database error: {0}")]
    Db(String),
}

/// Lazy, auto-reconnecting Redis pool. `open` is offline (parses the URL only);
/// the `ConnectionManager` is established on first `conn()` via `OnceCell`, with
/// a fast-fail config so an unreachable Redis errors in ~500ms (not ~9.5s) and
/// the auto path can fail-closed promptly. `OnceCell` is not poisoned on a
/// failed init — a later `conn()` retries. Once connected, the `ConnectionManager`
/// clone is reused (cheap Arc, auto-reconnecting).
pub struct RedisPool {
    client: redis::Client,
    cell: OnceCell<ConnectionManager>,
}

impl RedisPool {
    pub fn open(url: &str) -> Result<Self, ExecutorError> {
        Ok(Self {
            client: redis::Client::open(url)?,
            cell: OnceCell::new(),
        })
    }

    pub async fn conn(&self) -> Result<ConnectionManager, ExecutorError> {
        let cm = self
            .cell
            .get_or_try_init(|| async {
                // NB: 1.3.0's setters take `Option<Duration>`.
                let cfg = ConnectionManagerConfig::new()
                    .set_number_of_retries(1)
                    .set_connection_timeout(Some(Duration::from_millis(500)))
                    .set_response_timeout(Some(Duration::from_millis(500)));
                ConnectionManager::new_with_config(self.client.clone(), cfg).await
            })
            .await?
            .clone();
        Ok(cm)
    }
}

/// The long-lived executor handle: one Redis pool, the compiled claim `Script`
/// (its SHA1 is computed once here), and the per-account async lock registry.
/// Clone-free; share via `Arc` in the caller (Fase 5/6).
pub struct ExecutorHandle {
    pub(crate) redis: RedisPool,
    pub(crate) gate: Script,
    pub(crate) account_locks: DashMap<String, Arc<Mutex<()>>>,
}

impl ExecutorHandle {
    pub async fn connect(redis_url: &str) -> Result<Self, ExecutorError> {
        let redis = RedisPool::open(redis_url)?;
        let gate = Script::new(ACCEPT_GATE_LUA);
        // Best-effort SCRIPT LOAD once so the first real claim is a single
        // EVALSHA. Ignored on error: `invoke_async` reloads on NOSCRIPT anyway,
        // and Redis may be briefly unavailable at startup.
        if let Ok(mut con) = redis.conn().await {
            let _: redis::RedisResult<String> = gate.prepare_invoke().load_async(&mut con).await;
        }
        Ok(Self {
            redis,
            gate,
            account_locks: DashMap::new(),
        })
    }
}
```

- [ ] **Step 5: Write `lib.rs`**

```rust
// Backend/crates/executor/src/lib.rs
//! Fase 4 — the executor library: 3-layer accept dedup, agency-dup verification,
//! and per-rule quota consumption. A pure library called by Fase 5 (poller) and
//! Fase 6 (api-gateway, manual accept); it owns the shared Redis keyspace so the
//! two callers cannot diverge.
pub mod gate;

pub use gate::{ExecutorError, ExecutorHandle, RedisPool, ACCEPT_GATE_LUA};

// Later tasks add: pub mod dedup; (Task 2) pub mod restore; (Task 4)
// pub mod account_lock; pub mod quota; (Task 5) pub mod agency_dup; (Task 6)
```

- [ ] **Step 6: Write the connection round-trip test**

```rust
// Backend/crates/executor/tests/redis_roundtrip.rs
//! Basic real-Redis connectivity: open a pool against the tower-redis container
//! (127.0.0.1:16379), PING, and SET/GET round-trip through a unique key. Proves
//! the RedisPool lazy-connect + ConnectionManager path works end to end.
use executor::RedisPool;
use redis::AsyncCommands;
use uuid::Uuid;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

#[tokio::test]
async fn pool_connects_and_round_trips() {
    let pool = RedisPool::open(&redis_url()).expect("open");
    let mut con = pool.conn().await.expect("conn");

    let pong: String = redis::cmd("PING").query_async(&mut con).await.expect("ping");
    assert_eq!(pong, "PONG");

    let key = format!("executor:test:{}", Uuid::new_v4());
    let _: () = con.set(&key, "hello").await.expect("set");
    let got: String = con.get(&key).await.expect("get");
    assert_eq!(got, "hello");
    let _: () = con.del(&key).await.expect("del");
}
```

- [ ] **Step 7: Bring up Redis, build, test, clippy, deny**

```bash
cd Docker && docker compose up -d tower-redis && cd ..
# wait for healthy: docker compose -f Docker/docker-compose.yml ps
cd Backend
cargo test -p executor --test redis_roundtrip -- --test-threads=1
cargo clippy -p executor --all-targets -- -D warnings
cargo deny check
cd ..
```

Expected: the round-trip test passes; clippy clean; `cargo deny check` prints `advisories ok, bans ok, licenses ok, sources ok` (it fails with a `BSL-1.0` rejection if Step 2 was skipped). If `conn()` cannot connect, confirm `tower-redis` is healthy and that Step 3's `127.0.0.1:16379` publish is present.

- [ ] **Step 8: Commit**

```bash
git add Backend/crates/executor Backend/Cargo.toml Backend/Cargo.lock Backend/deny.toml Docker/docker-compose.yml
git commit -m "feat(executor): crate scaffold + RedisPool/ExecutorHandle + tower-redis dev port + BSL-1.0 allow"
```

---

### Task 2: Layer 1 — `AccountDedupState` (`DashSet`/`DashMap`, evict-oldest-past-5000) + concurrency test

**Files:**
- Create: `Backend/crates/executor/src/dedup.rs`
- Modify: `Backend/crates/executor/src/lib.rs` (`pub mod dedup;` + re-export)

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `pub struct AccountDedupState` with `pub fn new() -> Self` (+ `Default`).
  - `pub fn try_begin_accept(&self, spx_id: &str) -> bool` — atomic Layer-1 in-flight claim (`true` = this caller won).
  - `pub fn abort_accept(&self, spx_id: &str)` — release an in-flight claim without recording an accept.
  - `pub fn commit_accept(&self, spx_id: &str)` — move from in-flight to accepted (evicts oldest past 5000).
  - `pub fn insert_restored(&self, spx_id: &str)` — used by Layer 3 restore (Task 4) to seed `accepted_ids`.
  - `pub fn is_known(&self, spx_id: &str) -> bool` — in `accepting_now` OR `accepted_ids` (used by `try_claim_manual`, Task 3).
  - `pub fn accepted_len(&self) -> usize`.

- [ ] **Step 1: Write `dedup.rs`**

```rust
// Backend/crates/executor/src/dedup.rs
//! Layer 1 (in-proc, fastest): per-account in-flight + accepted sets. One
//! `AccountDedupState` PER account (not a global set — SPX allows numerically
//! identical spxIds across different accounts, so a shared set would collide).
//!
//! `DashSet::insert() -> bool` (true = newly inserted) is an atomic
//! check-and-set per key: the single call closes the `has()`+`add()` race a
//! literal port of the reference's JS `Set` would introduce under concurrent
//! Tokio tasks. `accepted_ids` is a `DashMap<String, Instant>` (not a plain
//! `DashSet`) so the memory bound (5000, per the reference) can evict the OLDEST
//! entry — JS `Set` iterates in insertion order natively, so this is parity.
use std::time::Instant;

use dashmap::{DashMap, DashSet};

/// Reference memory bound for `accepted_ids` (poller.ts:997-1000).
const MAX_ACCEPTED_IDS: usize = 5000;

pub struct AccountDedupState {
    accepting_now: DashSet<String>,
    accepted_ids: DashMap<String, Instant>,
}

impl Default for AccountDedupState {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountDedupState {
    pub fn new() -> Self {
        Self {
            accepting_now: DashSet::new(),
            accepted_ids: DashMap::new(),
        }
    }

    /// Claim an spxId for in-flight accept. Returns `true` iff THIS caller won
    /// the claim. Already-accepted spxIds return `false` immediately; the
    /// atomic decision is `accepting_now.insert()` (single call, no race).
    pub fn try_begin_accept(&self, spx_id: &str) -> bool {
        if self.accepted_ids.contains_key(spx_id) {
            return false;
        }
        self.accepting_now.insert(spx_id.to_string())
    }

    /// Release an in-flight claim without recording an accept (e.g. the dispatch
    /// failed and may be retried later).
    pub fn abort_accept(&self, spx_id: &str) {
        self.accepting_now.remove(spx_id);
    }

    /// Record a successful accept: drop the in-flight claim and add to
    /// `accepted_ids`, evicting the oldest entry if over the 5000 bound.
    pub fn commit_accept(&self, spx_id: &str) {
        self.accepting_now.remove(spx_id);
        self.accepted_ids.insert(spx_id.to_string(), Instant::now());
        self.evict_if_needed();
    }

    /// Seed an accepted spxId from the durable ZSET restore (Layer 3).
    pub fn insert_restored(&self, spx_id: &str) {
        self.accepted_ids.insert(spx_id.to_string(), Instant::now());
        self.evict_if_needed();
    }

    /// True if this spxId is in-flight OR already accepted.
    pub fn is_known(&self, spx_id: &str) -> bool {
        self.accepting_now.contains(spx_id) || self.accepted_ids.contains_key(spx_id)
    }

    pub fn accepted_len(&self) -> usize {
        self.accepted_ids.len()
    }

    fn evict_if_needed(&self) {
        while self.accepted_ids.len() > MAX_ACCEPTED_IDS {
            // Find the oldest key WITHOUT holding an iterator across the remove
            // (DashMap: holding a shard ref while removing can deadlock).
            let mut oldest_key: Option<String> = None;
            let mut oldest_t: Option<Instant> = None;
            for e in self.accepted_ids.iter() {
                if oldest_t.map_or(true, |t| *e.value() < t) {
                    oldest_t = Some(*e.value());
                    oldest_key = Some(e.key().clone());
                }
            }
            match oldest_key {
                Some(k) => {
                    self.accepted_ids.remove(&k);
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_then_commit_transitions_known_state() {
        let s = AccountDedupState::new();
        assert!(!s.is_known("A"));
        assert!(s.try_begin_accept("A"));
        assert!(s.is_known("A")); // in accepting_now
        s.commit_accept("A");
        assert!(s.is_known("A")); // now in accepted_ids
        // A second begin on an already-accepted id must fail.
        assert!(!s.try_begin_accept("A"));
    }

    #[test]
    fn abort_releases_inflight_claim() {
        let s = AccountDedupState::new();
        assert!(s.try_begin_accept("B"));
        s.abort_accept("B");
        assert!(!s.is_known("B"));
        assert!(s.try_begin_accept("B")); // reclaimable after abort
    }

    #[test]
    fn evicts_oldest_when_over_5000() {
        let s = AccountDedupState::new();
        // Insert 5000 with increasing timestamps (natural order), then one more.
        for i in 0..MAX_ACCEPTED_IDS {
            s.insert_restored(&format!("id-{i:05}"));
        }
        assert_eq!(s.accepted_len(), MAX_ACCEPTED_IDS);
        // The first-inserted ("id-00000") is the oldest; inserting a new one
        // must evict it, keeping the length at the bound.
        std::thread::sleep(std::time::Duration::from_millis(2));
        s.insert_restored("id-newest");
        assert_eq!(s.accepted_len(), MAX_ACCEPTED_IDS);
        assert!(s.is_known("id-newest"));
        assert!(!s.is_known("id-00000"), "oldest entry must have been evicted");
    }
}
```

- [ ] **Step 2: Concurrency test — exactly one winner (DoD #1)**

Add to `dedup.rs`'s `#[cfg(test)] mod tests` (this test needs the multi-thread runtime, so it uses `#[tokio::test(flavor = "multi_thread", worker_threads = 8)]`):

```rust
    // DoD #1: many concurrent Tokio tasks race to claim the SAME new spxId;
    // exactly one must win. This is the atomic `DashSet::insert()` (single call),
    // NOT a race-prone `has()`+`add()` two-step.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_claim_exactly_one_winner() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let state = Arc::new(AccountDedupState::new());
        let wins = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..128 {
            let s = state.clone();
            let w = wins.clone();
            handles.push(tokio::spawn(async move {
                if s.try_begin_accept("SPXID_CONTESTED") {
                    w.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(
            wins.load(Ordering::SeqCst),
            1,
            "exactly one task may win the in-flight claim"
        );
    }
```

- [ ] **Step 3: Wire into `lib.rs`**

```rust
pub mod dedup;
pub use dedup::AccountDedupState;
```

- [ ] **Step 4: Test + clippy + commit**

```bash
cd Backend
cargo test -p executor --lib
cargo clippy -p executor --all-targets -- -D warnings
cd ..
git add Backend/crates/executor
git commit -m "feat(executor): Layer 1 AccountDedupState (DashSet/DashMap, evict-oldest-5000) + concurrency test"
```

Expected: all `dedup` tests pass, including exactly-one-winner under the multi-thread runtime; clippy clean.

---

### Task 3: Layer 2 — `try_claim_auto` (fail-closed) + `try_claim_manual` (fail-open, shared key) + real-Redis tests

**Files:**
- Modify: `Backend/crates/executor/src/gate.rs` (add `ClaimOutcome`, `ManualClaimOutcome`, `try_claim_auto`, `try_claim_manual`)
- Modify: `Backend/crates/executor/src/lib.rs` (re-export the outcomes)
- Create: `Backend/crates/executor/tests/gate_redis.rs`

**Interfaces:**
- Consumes: `ExecutorHandle` (Task 1), `AccountDedupState` (Task 2).
- Produces:
  - `pub enum ClaimOutcome { Proceed, AlreadyClaimed, QuotaFull, RedisUnavailable }` (+ `pub fn should_dispatch(&self) -> bool` = `matches!(self, Proceed)`).
  - `pub enum ManualClaimOutcome { Ok, AlreadyAccepted }`.
  - `pub async fn ExecutorHandle::try_claim_auto(&self, account_id: &str, spx_id: &str, rule_id: Option<Uuid>, cap: i64, accepted_count: i64) -> ClaimOutcome` — fail-closed.
  - `pub async fn ExecutorHandle::try_claim_manual(&self, account_id: &str, spx_id: &str, dedup: &AccountDedupState) -> ManualClaimOutcome` — fail-open, shares the `spx:claim:` key, skips quota.

**Design note (read before coding):** `redis::Script::invoke_async` already does EVALSHA→(NOSCRIPT)→LOAD+retry, so `try_claim_auto` just calls `self.gate.key(...).key(...).arg(...).invoke_async(...)` and maps the `i64` result. The inflight key rule component is `rule_id.map(|u| u.to_string()).unwrap_or_else(|| "_norule".into())` — the SAME `<ruleId|_norule>` string `apply_rule_consumption` (Task 5) uses for `SREM` (both derive it from the rule's DB `Uuid`, so they always agree). `try_claim_manual` shares `KEYS[1]` (`spx:claim:<acct>:<spxId>`) exactly so a manual click and the poller can never both win the same ticket.

- [ ] **Step 1: Add the outcomes and methods to `gate.rs`**

Append to `Backend/crates/executor/src/gate.rs` (add `use redis::AsyncCommands;` and `use uuid::Uuid;` to the imports at the top, and `use crate::dedup::AccountDedupState;`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// Gate returned 1 — this caller may dispatch the accept.
    Proceed,
    /// Gate returned 0 — the claim key already existed (someone else has it).
    AlreadyClaimed,
    /// Gate returned -1 — the per-rule quota is full.
    QuotaFull,
    /// Any Redis error (connection, timeout, NOSCRIPT-after-reload) — FAIL-CLOSED:
    /// the auto path must NOT dispatch. Ports the reference's `.catch(() => 0)`.
    RedisUnavailable,
}

impl ClaimOutcome {
    /// Only `Proceed` authorizes a dispatch.
    pub fn should_dispatch(&self) -> bool {
        matches!(self, ClaimOutcome::Proceed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualClaimOutcome {
    /// Proceed with the manual accept (includes the FAIL-OPEN Redis-error case).
    Ok,
    /// Already accepted (Layer 1, Layer 3 durable, or the shared claim key).
    AlreadyAccepted,
}

impl ExecutorHandle {
    /// Auto (poller) claim path — FAIL-CLOSED. Runs `ACCEPT_GATE_LUA` (SET NX EX
    /// claim + atomic per-rule quota + inflight-set bookkeeping). Any Redis error
    /// maps to `RedisUnavailable` (do NOT dispatch). `rule_id` is the matched
    /// rule's DB id (`None` only for the uncapped/no-rule case → `_norule`);
    /// `cap` is `max_accept_count` (0 = unlimited) and `accepted_count` its
    /// current persisted value.
    pub async fn try_claim_auto(
        &self,
        account_id: &str,
        spx_id: &str,
        rule_id: Option<Uuid>,
        cap: i64,
        accepted_count: i64,
    ) -> ClaimOutcome {
        let rule_key = rule_id
            .map(|u| u.to_string())
            .unwrap_or_else(|| "_norule".to_string());
        let claim_key = format!("spx:claim:{account_id}:{spx_id}");
        let inflight_key = format!("spx:inflight:{account_id}:{rule_key}");

        let mut con = match self.redis.conn().await {
            Ok(c) => c,
            Err(_) => return ClaimOutcome::RedisUnavailable, // fail-closed
        };
        let ret: Result<i64, _> = self
            .gate
            .key(&claim_key)
            .key(&inflight_key)
            .arg(spx_id)
            .arg(cap)
            .arg(accepted_count)
            .arg("600")
            .invoke_async(&mut con)
            .await;
        match ret {
            Ok(1) => ClaimOutcome::Proceed,
            Ok(0) => ClaimOutcome::AlreadyClaimed,
            Ok(-1) => ClaimOutcome::QuotaFull,
            Ok(_) => ClaimOutcome::RedisUnavailable, // unexpected value: fail-closed
            Err(_) => ClaimOutcome::RedisUnavailable, // fail-closed
        }
    }

    /// Manual (human) claim path — FAIL-OPEN, shares the `spx:claim:` key with
    /// the auto path but SKIPS the per-rule quota (a human chose the ticket).
    /// Checks Layer 1 (`dedup`) and Layer 3 (durable ZSET) first; then `SET NX
    /// EX 600` on the SAME key `try_claim_auto` uses. On ANY Redis error returns
    /// `Ok` (proceed) — the reference's `beginManualAccept` `catch(() => 'ok')`.
    /// Rationale: a manual accept is a conscious human action, and the poller's
    /// own fail-closed gate guarantees no concurrent poller contention.
    pub async fn try_claim_manual(
        &self,
        account_id: &str,
        spx_id: &str,
        dedup: &AccountDedupState,
    ) -> ManualClaimOutcome {
        // Layer 1 (in-proc).
        if dedup.is_known(spx_id) {
            return ManualClaimOutcome::AlreadyAccepted;
        }
        let mut con = match self.redis.conn().await {
            Ok(c) => c,
            Err(_) => return ManualClaimOutcome::Ok, // fail-open
        };
        // Layer 3 (durable ZSET).
        let zkey = format!("spx:accepted:{account_id}");
        let score: Result<Option<f64>, _> = con.zscore(&zkey, spx_id).await;
        match score {
            Ok(Some(_)) => return ManualClaimOutcome::AlreadyAccepted,
            Ok(None) => {}
            Err(_) => return ManualClaimOutcome::Ok, // fail-open
        }
        // Layer 2 (shared claim key — SAME key as try_claim_auto's KEYS[1]).
        let claim_key = format!("spx:claim:{account_id}:{spx_id}");
        let opts = redis::SetOptions::default()
            .conditional_set(redis::ExistenceCheck::NX)
            .with_expiration(redis::SetExpiry::EX(600));
        let set: Result<bool, _> = con.set_options(&claim_key, "1", opts).await;
        match set {
            Ok(true) => ManualClaimOutcome::Ok,
            Ok(false) => ManualClaimOutcome::AlreadyAccepted,
            Err(_) => ManualClaimOutcome::Ok, // fail-open
        }
    }
}
```

- [ ] **Step 2: Assert the Lua is byte-identical to the design doc (DoD #2, first half)**

Add a unit test inside `gate.rs` (a `#[cfg(test)] mod tests`) that pins the literal script text. This guards against an accidental "cleanup" reformat that would change the SHA:

```rust
#[cfg(test)]
mod tests {
    use super::ACCEPT_GATE_LUA;

    // DoD #2: the script is ported byte-for-byte. This is the exact 12-line body
    // from the design doc (verified by byte-diff, 2-space nested indents), built
    // from a line array joined by `\n` — do NOT use `\n\` line-continuations
    // here: the `\` continuation strips the Lua's own leading indentation, so the
    // nested `if`/`return -1`/`SADD`/`EXPIRE` lines would silently lose their
    // 2-/4-space indents and this test would fail. Changing one character (here or
    // in the const) changes the Redis content hash.
    #[test]
    fn accept_gate_lua_is_byte_exact() {
        let expected = [
            "local ok = redis.call('SET', KEYS[1], '1', 'NX', 'EX', ARGV[4])",
            "if not ok then return 0 end",
            "local cap = tonumber(ARGV[2])",
            "if cap > 0 then",
            "  if redis.call('SISMEMBER', KEYS[2], ARGV[1]) == 0 and (tonumber(ARGV[3]) + redis.call('SCARD', KEYS[2])) >= cap then",
            "    redis.call('DEL', KEYS[1])",
            "    return -1",
            "  end",
            "  redis.call('SADD', KEYS[2], ARGV[1])",
            "  redis.call('EXPIRE', KEYS[2], 600)",
            "end",
            "return 1",
        ]
        .join("\n");
        assert_eq!(ACCEPT_GATE_LUA, expected.as_str());
    }
}
```

- [ ] **Step 3: Real-Redis tests for all three return values + fail-closed vs fail-open (DoD #2 + #3 + #8)**

```rust
// Backend/crates/executor/tests/gate_redis.rs
//! Real-Redis (127.0.0.1:16379) tests for the claim gate: the three Lua return
//! values, auto=fail-closed vs manual=fail-open under an unreachable Redis, and
//! that manual + auto share the claim keyspace. Unique account ids per test so
//! no FLUSHALL / serialization is needed for key isolation.
use executor::{AccountDedupState, ClaimOutcome, ExecutorHandle, ManualClaimOutcome};
use uuid::Uuid;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

fn acct() -> String {
    format!("t{}", Uuid::new_v4().simple())
}

#[tokio::test]
async fn gate_returns_proceed_already_and_quota_full() {
    let h = ExecutorHandle::connect(&redis_url()).await.expect("connect");
    let a = acct();
    let rule = Uuid::new_v4();

    // New claim → Proceed (uncapped).
    assert_eq!(
        h.try_claim_auto(&a, "100", None, 0, 0).await,
        ClaimOutcome::Proceed
    );
    // Same spxId again → AlreadyClaimed (SET NX fails).
    assert_eq!(
        h.try_claim_auto(&a, "100", None, 0, 0).await,
        ClaimOutcome::AlreadyClaimed
    );
    // Capped rule (cap=1, accepted=0): first NEW spxId claims and enters the
    // inflight set → Proceed.
    assert_eq!(
        h.try_claim_auto(&a, "200", Some(rule), 1, 0).await,
        ClaimOutcome::Proceed
    );
    // A second NEW spxId under the same full rule → QuotaFull.
    assert_eq!(
        h.try_claim_auto(&a, "201", Some(rule), 1, 0).await,
        ClaimOutcome::QuotaFull
    );
}

#[tokio::test]
async fn auto_fails_closed_manual_fails_open_when_redis_unreachable() {
    // Nothing listens on 16999 — the pool opens offline; commands error fast.
    let h = ExecutorHandle::connect("redis://127.0.0.1:16999")
        .await
        .expect("open offline");
    let a = acct();
    let dedup = AccountDedupState::new();

    // Auto → RedisUnavailable (fail-closed: must NOT dispatch).
    let auto = h.try_claim_auto(&a, "1", None, 0, 0).await;
    assert_eq!(auto, ClaimOutcome::RedisUnavailable);
    assert!(!auto.should_dispatch());

    // Manual → Ok (fail-open: proceed).
    assert_eq!(
        h.try_claim_manual(&a, "1", &dedup).await,
        ManualClaimOutcome::Ok
    );
}

#[tokio::test]
async fn manual_and_auto_share_the_claim_key() {
    let h = ExecutorHandle::connect(&redis_url()).await.expect("connect");
    let a = acct();
    let dedup = AccountDedupState::new();

    // Manual claims spxId X first.
    assert_eq!(
        h.try_claim_manual(&a, "555", &dedup).await,
        ManualClaimOutcome::Ok
    );
    // Auto for the SAME account+spxId must now fail — proving the keyspace is
    // genuinely shared (DoD #8).
    assert_eq!(
        h.try_claim_auto(&a, "555", None, 0, 0).await,
        ClaimOutcome::AlreadyClaimed
    );
}

#[tokio::test]
async fn manual_rejects_when_layer1_already_known() {
    let h = ExecutorHandle::connect(&redis_url()).await.expect("connect");
    let a = acct();
    let dedup = AccountDedupState::new();
    dedup.insert_restored("999"); // pretend it was already accepted
    assert_eq!(
        h.try_claim_manual(&a, "999", &dedup).await,
        ManualClaimOutcome::AlreadyAccepted
    );
}
```

- [ ] **Step 4: Wire into `lib.rs`**

```rust
pub use gate::{ClaimOutcome, ManualClaimOutcome};
```

- [ ] **Step 5: Test + clippy + commit**

```bash
cd Docker && docker compose up -d tower-redis && cd ..
cd Backend
cargo test -p executor -- --test-threads=1
cargo clippy -p executor --all-targets -- -D warnings
cd ..
git add Backend/crates/executor
git commit -m "feat(executor): Layer 2 claim gate — try_claim_auto (fail-closed) + try_claim_manual (fail-open, shared key)"
```

Expected: the byte-exact Lua unit test, the three-return-value test, the fail-closed/fail-open test, and the shared-key test all pass.

---

### Task 4: Layer 3 — `restore_accepted_ids` (ZSET trim-to-7-days + read) + `record_durable_accept` + trim test

**Files:**
- Create: `Backend/crates/executor/src/restore.rs`
- Modify: `Backend/crates/executor/src/lib.rs` (`pub mod restore;`)
- Create: `Backend/crates/executor/tests/restore_redis.rs`

**Interfaces:**
- Consumes: `ExecutorHandle`, `RedisPool`, `AccountDedupState`.
- Produces (methods on `ExecutorHandle`):
  - `pub async fn restore_accepted_ids(&self, account_id: &str, state: &AccountDedupState) -> Result<usize, ExecutorError>` — `ZREMRANGEBYSCORE key 0 (now-7d)` then `ZRANGE key 0 -1`, seeding `state`. Returns the restored count.
  - `pub async fn record_durable_accept(&self, account_id: &str, spx_id: &str) -> Result<(), ExecutorError>` — `ZADD` with the current epoch score (called by Fase 5 on a successful accept). Plus a testable variant `record_durable_accept_at(&self, account_id, spx_id, epoch_secs: i64)`.

- [ ] **Step 1: Write `restore.rs`**

```rust
// Backend/crates/executor/src/restore.rs
//! Layer 3 (durable): the `spx:accepted:<acct>` sorted set (member = spxId,
//! score = accept epoch-seconds). `restore_accepted_ids` trims the set to a
//! 7-day window and loads the survivors into Layer 1 BEFORE the first poll.
use std::time::{SystemTime, UNIX_EPOCH};

use redis::AsyncCommands;

use crate::dedup::AccountDedupState;
use crate::gate::{ExecutorError, ExecutorHandle};

/// Seven days in seconds.
const WINDOW_SECS: i64 = 7 * 24 * 60 * 60;

fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl ExecutorHandle {
    /// Restore this account's durable accepted-ids into Layer 1.
    ///
    /// CONTRACT — Fase 5 MUST `.await` this to completion BEFORE scheduling the
    /// account's FIRST poll (reference race CP-7, poller.ts:288-292): otherwise
    /// the first poll can re-accept a ticket already won in a previous process
    /// lifetime, because Layer 1 starts empty and Layer 2's claim key may have
    /// expired. Fase 4 has no poll loop and cannot enforce this ordering; the
    /// enforcement is part of Fase 5's DoD. This function only guarantees its
    /// own correctness (a filled ZSET restores the right, in-window entries).
    pub async fn restore_accepted_ids(
        &self,
        account_id: &str,
        state: &AccountDedupState,
    ) -> Result<usize, ExecutorError> {
        let key = format!("spx:accepted:{account_id}");
        let mut con = self.redis.conn().await?;

        // Trim everything older than the 7-day window (inclusive of the cutoff).
        let cutoff = now_epoch_secs() - WINDOW_SECS;
        let _removed: usize = con.zrembyscore(&key, 0i64, cutoff).await?;

        // Load the survivors into Layer 1.
        let members: Vec<String> = con.zrange(&key, 0, -1).await?;
        for m in &members {
            state.insert_restored(m);
        }
        Ok(members.len())
    }

    /// Record a durable accept at the current time (Fase 5 calls this after a
    /// confirmed accept). Pairs with `restore_accepted_ids`.
    pub async fn record_durable_accept(
        &self,
        account_id: &str,
        spx_id: &str,
    ) -> Result<(), ExecutorError> {
        self.record_durable_accept_at(account_id, spx_id, now_epoch_secs())
            .await
    }

    /// Testable variant with an explicit epoch score.
    pub async fn record_durable_accept_at(
        &self,
        account_id: &str,
        spx_id: &str,
        epoch_secs: i64,
    ) -> Result<(), ExecutorError> {
        let key = format!("spx:accepted:{account_id}");
        let mut con = self.redis.conn().await?;
        let _: () = con.zadd(&key, spx_id, epoch_secs).await?;
        Ok(())
    }
}
```

- [ ] **Step 2: Wire into `lib.rs`**

```rust
pub mod restore;
```

(`restore_accepted_ids` / `record_durable_accept*` are inherent methods on `ExecutorHandle`, already re-exported via the `ExecutorHandle` type — no extra `pub use` needed.)

- [ ] **Step 3: Trim-window test (DoD #4)**

```rust
// Backend/crates/executor/tests/restore_redis.rs
//! DoD #4: seed a durable ZSET with one in-window and one out-of-window entry,
//! restore, and assert only the in-window entry lands in Layer 1 (the stale one
//! is trimmed, not restored). Real Redis @ 127.0.0.1:16379, unique account id.
use executor::{AccountDedupState, ExecutorHandle};
use uuid::Uuid;

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

#[tokio::test]
async fn restore_keeps_in_window_and_trims_stale() {
    let h = ExecutorHandle::connect(&redis_url()).await.expect("connect");
    let account = format!("t{}", Uuid::new_v4().simple());

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let eight_days = 8 * 24 * 60 * 60;

    // One recent (in window), one 8 days old (outside the 7-day window).
    h.record_durable_accept_at(&account, "recent-spx", now)
        .await
        .expect("record recent");
    h.record_durable_accept_at(&account, "stale-spx", now - eight_days)
        .await
        .expect("record stale");

    let state = AccountDedupState::new();
    let restored = h
        .restore_accepted_ids(&account, &state)
        .await
        .expect("restore");

    assert_eq!(restored, 1, "only the in-window entry may be restored");
    assert!(state.is_known("recent-spx"));
    assert!(
        !state.is_known("stale-spx"),
        "an entry older than 7 days must be trimmed, not restored"
    );
    assert_eq!(state.accepted_len(), 1);
}
```

- [ ] **Step 4: Test + clippy + commit**

```bash
cd Docker && docker compose up -d tower-redis && cd ..
cd Backend
cargo test -p executor -- --test-threads=1
cargo clippy -p executor --all-targets -- -D warnings
cd ..
git add Backend/crates/executor
git commit -m "feat(executor): Layer 3 restore_accepted_ids (7-day ZSET trim+read) + record_durable_accept"
```

Expected: the trim-window test passes (restored == 1, stale trimmed).

---

### Task 5: `store::consume_rule_quota` + `with_account_lock` + `apply_rule_consumption` + concurrency test

**Files:**
- Create: `Backend/crates/store/src/quota.rs`
- Modify: `Backend/crates/store/src/lib.rs` (`pub mod quota;` + re-exports + `pub use sqlx::PgPool;`)
- Create: `Backend/crates/executor/src/account_lock.rs`
- Create: `Backend/crates/executor/src/quota.rs`
- Modify: `Backend/crates/executor/src/lib.rs` (`pub mod account_lock; pub mod quota;`)
- Create: `Backend/crates/executor/tests/quota_pg.rs`

**Interfaces:**
- Produces in `store`:
  - `pub enum QuotaConsumeOutcome { Consumed { accepted_count: i32 }, CapReached { accepted_count: i32, max_accept_count: i32 }, NoRule }`.
  - `pub async fn consume_rule_quota(pool: &PgPool, tenant_id: Uuid, rule_id: Uuid) -> Result<QuotaConsumeOutcome, sqlx::Error>` — atomic conditional `UPDATE` (re-reads latest, increments, persists in one statement; `max_accept_count = 0` = unlimited).
  - `pub use sqlx::PgPool;` (so `executor` names the pool type without a direct `sqlx` dep).
- Produces in `executor` (methods on `ExecutorHandle`):
  - `pub async fn with_account_lock<T, Fut, F>(&self, account_id: &str, f: F) -> T where F: FnOnce() -> Fut, Fut: Future<Output = T>` — lazily-created per-account `tokio::sync::Mutex`, FIFO.
  - `pub async fn apply_rule_consumption(&self, pool: &store::PgPool, tenant_id: Uuid, account_id: &str, rule_id: Uuid, spx_id: &str) -> Result<store::QuotaConsumeOutcome, ExecutorError>` — the re-read-in-lock quota persist; on `Consumed`, releases the Redis in-flight slot (`SREM spx:inflight:<acct>:<rule_id>`) AFTER the DB persist.

**Design note (read before coding):** The atomic conditional `UPDATE` (`SET accepted_count = accepted_count + 1 … WHERE … AND (max_accept_count = 0 OR accepted_count < max_accept_count) RETURNING accepted_count`) IS the reference `applyRuleConsumption`'s "re-read latest → increment → persist" fused into one race-free statement — no SELECT-then-UPDATE TOCTOU. The per-account `tokio::sync::Mutex` is the faithful port of `withAccountLock` and also serializes the paired Redis `SREM` so the effective count never dips. The DB `UPDATE` binds `tenant_id` explicitly (belt-and-suspenders: the `tower` login is a BYPASSRLS superuser in dev, so RLS alone would not scope it). Persist (step 3) happens BEFORE the Redis `SREM` (step 4), matching the design's ordering so the count never momentarily dips below the true value.

- [ ] **Step 1: Write `store::quota`**

```rust
// Backend/crates/store/src/quota.rs
//! Per-rule accept-quota consumption. Atomic conditional increment so cap
//! enforcement and lost-update prevention hold even under concurrency (the
//! single UPDATE is the reference applyRuleConsumption's re-read+increment+
//! persist, race-free). No schema change — writes the existing
//! `accept_rules.accepted_count` / reads `max_accept_count`.
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaConsumeOutcome {
    /// One slot consumed; `accepted_count` is the NEW persisted value.
    Consumed { accepted_count: i32 },
    /// The cap is full (no slot consumed).
    CapReached {
        accepted_count: i32,
        max_accept_count: i32,
    },
    /// No such rule for this tenant.
    NoRule,
}

/// Consume one quota slot for `rule_id` under `tenant_id`. `max_accept_count = 0`
/// means unlimited. Atomic: the conditional UPDATE increments only if under cap,
/// and `RETURNING` reports the new value; a 0-row update means either cap-full or
/// no-such-rule (disambiguated by a follow-up read in the same transaction).
pub async fn consume_rule_quota(
    pool: &PgPool,
    tenant_id: Uuid,
    rule_id: Uuid,
) -> Result<QuotaConsumeOutcome, sqlx::Error> {
    let mut tx = crate::begin_tenant_tx(pool, tenant_id).await?;

    let updated: Option<(i32,)> = sqlx::query_as(
        "UPDATE accept_rules \
         SET accepted_count = accepted_count + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 \
           AND (max_accept_count = 0 OR accepted_count < max_accept_count) \
         RETURNING accepted_count",
    )
    .bind(rule_id)
    .bind(tenant_id)
    .fetch_optional(&mut *tx)
    .await?;

    let outcome = match updated {
        Some((accepted_count,)) => QuotaConsumeOutcome::Consumed { accepted_count },
        None => {
            let row: Option<(i32, i32)> = sqlx::query_as(
                "SELECT accepted_count, max_accept_count FROM accept_rules \
                 WHERE id = $1 AND tenant_id = $2",
            )
            .bind(rule_id)
            .bind(tenant_id)
            .fetch_optional(&mut *tx)
            .await?;
            match row {
                Some((accepted_count, max_accept_count)) => QuotaConsumeOutcome::CapReached {
                    accepted_count,
                    max_accept_count,
                },
                None => QuotaConsumeOutcome::NoRule,
            }
        }
    };

    tx.commit().await?;
    Ok(outcome)
}
```

- [ ] **Step 2: Wire `store::lib.rs`**

Add to `Backend/crates/store/src/lib.rs` (keep the existing `pub mod models; pub mod pool; pub use pool::{...};`):

```rust
pub mod quota;

pub use quota::{consume_rule_quota, QuotaConsumeOutcome};
// Re-export so downstream crates (e.g. executor) can name the pool type without
// a direct `sqlx` dependency.
pub use sqlx::PgPool;
```

- [ ] **Step 3: Write `executor::account_lock`**

```rust
// Backend/crates/executor/src/account_lock.rs
//! Per-account FIFO async lock (port of the reference `withAccountLock`
//! promise-chain, as a `tokio::sync::Mutex` per account). Serializes the
//! read-modify-write of a rule's quota so no two increments for the same account
//! overlap. Locks are created lazily via `DashMap::entry`.
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::gate::ExecutorHandle;

impl ExecutorHandle {
    /// Run `f` while holding this account's lock (created on first use). FIFO per
    /// account — identical serialization property to the reference's in-proc
    /// promise chain, but async-aware.
    pub async fn with_account_lock<T, Fut, F>(&self, account_id: &str, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        // Look up (or lazily insert) the per-account lock, then DROP the DashMap
        // shard guard before awaiting the async mutex (never hold a sync shard
        // lock across an await).
        let lock: Arc<Mutex<()>> = self
            .account_locks
            .entry(account_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;
        f().await
    }
}
```

- [ ] **Step 4: Write `executor::quota`**

```rust
// Backend/crates/executor/src/quota.rs
//! Re-read-in-lock per-rule quota consumption (port of `applyRuleConsumption`).
//! Inside the per-account lock: consume one DB quota slot atomically, then (only
//! if consumed) release the Redis in-flight slot — persist BEFORE release so the
//! effective count never dips.
use redis::AsyncCommands;
use uuid::Uuid;

use crate::gate::{ExecutorError, ExecutorHandle};

impl ExecutorHandle {
    pub async fn apply_rule_consumption(
        &self,
        pool: &store::PgPool,
        tenant_id: Uuid,
        account_id: &str,
        rule_id: Uuid,
        spx_id: &str,
    ) -> Result<store::QuotaConsumeOutcome, ExecutorError> {
        // (1)-(3): re-read latest + increment + persist, atomically, serialized
        // per account. `sqlx::Error` from `store` is consumed only via Display,
        // so `executor` needs no direct `sqlx` dependency.
        let outcome = self
            .with_account_lock(account_id, || async {
                store::consume_rule_quota(pool, tenant_id, rule_id)
                    .await
                    .map_err(|e| ExecutorError::Db(e.to_string()))
            })
            .await?;

        // (4): release the Redis in-flight slot AFTER the DB persist, and only
        // when a slot was actually consumed. Best-effort — a failed SREM only
        // leaves a slot occupied until its 600s TTL, never over-accepts.
        if matches!(outcome, store::QuotaConsumeOutcome::Consumed { .. }) {
            let inflight_key = format!("spx:inflight:{account_id}:{rule_id}");
            if let Ok(mut con) = self.redis.conn().await {
                let _: Result<usize, _> = con.srem(&inflight_key, spx_id).await;
            }
        }
        Ok(outcome)
    }
}
```

- [ ] **Step 5: Wire `executor::lib.rs`**

```rust
pub mod account_lock;
pub mod quota;
```

- [ ] **Step 6: Concurrency test — no lost update, cap never exceeded (DoD #6)**

```rust
// Backend/crates/executor/tests/quota_pg.rs
//! DoD #6: fire N concurrent apply_rule_consumption for the SAME rule (cap=2)
//! and assert exactly 2 succeed (accepted_count 1 then 2), the rest hit the cap,
//! the final persisted count is 2 (no lost update), and it never exceeds the
//! cap. Postgres @ 127.0.0.1:15432 + Redis @ 127.0.0.1:16379.
use executor::ExecutorHandle;
use store::QuotaConsumeOutcome;
use uuid::Uuid;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}
fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_consume_never_exceeds_cap_no_lost_update() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let handle = std::sync::Arc::new(ExecutorHandle::connect(&redis_url()).await.expect("redis"));

    // Tenant + a capped rule (max=2, accepted=0).
    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("Quota Tenant")
        .bind(format!("quota-{tenant_id}"))
        .execute(&pool)
        .await
        .expect("insert tenant");

    let rule_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO accept_rules (id, tenant_id, name, mode, max_accept_count, accepted_count) \
         VALUES ($1, $2, 'r', 'route', 2, 0)",
    )
    .bind(rule_id)
    .bind(tenant_id)
    .execute(&pool)
    .await
    .expect("insert rule");

    let account = format!("t{}", Uuid::new_v4().simple());

    // 8 concurrent attempts for the same rule.
    let mut tasks = Vec::new();
    for i in 0..8 {
        let h = handle.clone();
        let p = pool.clone();
        let acct = account.clone();
        tasks.push(tokio::spawn(async move {
            h.apply_rule_consumption(&p, tenant_id, &acct, rule_id, &format!("spx-{i}"))
                .await
                .expect("consume")
        }));
    }
    let mut consumed = 0usize;
    let mut cap_reached = 0usize;
    let mut seen_counts = Vec::new();
    for t in tasks {
        match t.await.unwrap() {
            QuotaConsumeOutcome::Consumed { accepted_count } => {
                consumed += 1;
                seen_counts.push(accepted_count);
            }
            QuotaConsumeOutcome::CapReached { .. } => cap_reached += 1,
            QuotaConsumeOutcome::NoRule => panic!("rule must exist"),
        }
    }
    assert_eq!(consumed, 2, "exactly cap (2) consumptions may succeed");
    assert_eq!(cap_reached, 6, "the other 6 must see the cap");
    seen_counts.sort_unstable();
    assert_eq!(seen_counts, vec![1, 2], "no lost update: counts are 1 and 2");

    // Final persisted count must be exactly the cap — never exceeded.
    let (final_count,): (i32,) =
        sqlx::query_as("SELECT accepted_count FROM accept_rules WHERE id = $1")
            .bind(rule_id)
            .fetch_one(&pool)
            .await
            .expect("read final");
    assert_eq!(final_count, 2);

    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .ok();
}
```

Note: `executor`'s `tests/` target can use `sqlx`/`store` directly — but `sqlx` must be available to the TEST target. Add it as a dev-dependency:

```bash
cd Backend
cargo add --package executor --dev sqlx --features postgres,runtime-tokio-rustls,macros,uuid,chrono
cd ..
```

(This is a **dev-dependency only** — production `executor` still has no direct `sqlx` dep; Task 8 verifies that.)

- [ ] **Step 7: Bring up services, test, clippy, commit**

```bash
cd Docker && docker compose up -d tower-postgres tower-redis && cd ..
cd Backend
cargo test -p store --lib -- --test-threads=1     # store's own suite still green
cargo test -p executor --test quota_pg -- --test-threads=1
cargo clippy -p store --all-targets -- -D warnings
cargo clippy -p executor --all-targets -- -D warnings
cd ..
git add Backend/crates/store Backend/crates/executor Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(executor): with_account_lock + apply_rule_consumption (atomic re-read-in-lock quota) + store::consume_rule_quota"
```

Expected: `store`'s existing tests still pass; the concurrency test proves exactly 2 consumptions, final count 2, cap never exceeded.

---

### Task 6: `verify_agency_dup` (0/500/1500ms probe) + `fetch_self_email` + wiremock tests

**Files:**
- Create: `Backend/crates/executor/src/agency_dup.rs`
- Modify: `Backend/crates/executor/src/lib.rs` (`pub mod agency_dup;` + re-export)
- Create: `Backend/crates/executor/tests/agency_dup_mock.rs`

**Interfaces:**
- Consumes: `spx_client::{SpxClient, SpxCookies}` and `SpxClient::{fetch_bidding_log, fetch_profile}` (Fase 3), `serde_json::Value`.
- Produces:
  - `pub enum AgencyDupOutcome { Ours, LostToAgency { rival_email: String }, Inconclusive }`.
  - `pub async fn verify_agency_dup(client: &SpxClient, cookies: &SpxCookies, self_email: &str, booking_id: i64) -> AgencyDupOutcome` — retry `[0, 500, 1500]` ms; filter `booking_operation_type == 4`, operator containing `@`, earliest `create_time`; compare (lowercased/trimmed) against `self_email`.
  - `pub async fn fetch_self_email(client: &SpxClient, cookies: &SpxCookies) -> Option<String>` — extract the account email from `fetch_profile` (6-key fallback), lowercased+trimmed. Fase 5 calls this once and passes the result into `verify_agency_dup`.
  - `pub fn extract_self_email(profile: &serde_json::Value) -> Option<String>` and `pub(crate) fn earliest_accept_operator(log: &serde_json::Value) -> Option<String>` (pure helpers, unit-tested without a network).

**Design note (read before coding):** `SpxClient::fetch_bidding_log(cookies, booking_id: i64)` returns `Result<serde_json::Value, SpxError>`. The design doc wrote `booking_id: &str`, but this plan uses **`i64`** to match the Fase 3 client signature (avoiding a fragile string-parse) — a deliberate reconciliation, documented here. The reference (`spx.ts:459-464`, `poller.ts:806-810`) parses `json.data.list`, keeps `booking_operation_type === 4`, prefers `operator` containing `@`, sorts by `create_time` ascending, takes the first; `booking_operation_type`/`create_time` may be JSON numbers or numeric strings (`Number(x)` in TS), so parse flexibly. The profile email fallback keys (`spx.ts:1016-1017`) are `email, user_email, email_address, account_email, contact_email, login_email`, read from the response's `data` object (falling back to top level).

- [ ] **Step 1: Write `agency_dup.rs`**

```rust
// Backend/crates/executor/src/agency_dup.rs
//! agency_dup verification: when SPX reports "your agency already accepted",
//! probe the booking op-log to learn WHO really accepted — us (reclassify to ok)
//! or a rival agency (a real loss). Retry 0/500/1500ms because the op-log can lag
//! a beat after the race. NO "unverified" flag is stored anywhere — an
//! inconclusive probe is treated as "ours" by the caller (Fase 5).
use std::time::Duration;

use serde_json::Value;
use spx_client::{SpxClient, SpxCookies};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgencyDupOutcome {
    /// The acceptor is our own account — reclassify the accept back to "ok".
    Ours,
    /// A different agency won — Fase 5's notifier should alert on `rival_email`.
    LostToAgency { rival_email: String },
    /// 3 attempts, no `@`-bearing acceptor found. Treated as `Ours` by the caller
    /// (no state stored) — matches the reference's `return null`.
    Inconclusive,
}

/// Sleep-before-attempt delays (ms). Ported exactly from the reference; the sleep
/// is BEFORE attempts 2 and 3, not after.
const RETRY_DELAYS_MS: [u64; 3] = [0, 500, 1500];

/// Flexible numeric parse: JSON number or numeric string (reference `Number(x)`).
fn as_num(v: &Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(f) = v.as_f64() {
        return Some(f as i64);
    }
    v.as_str().and_then(|s| s.trim().parse::<i64>().ok())
}

/// From a `log/list` response, the operator (email) of the ACCEPT op
/// (`booking_operation_type == 4`) that contains `@`, choosing the EARLIEST
/// `create_time` on ties. `None` if no such `@`-bearing acceptor exists.
pub(crate) fn earliest_accept_operator(log: &Value) -> Option<String> {
    // Reference: if retcode present and != 0, treat as no data.
    if let Some(rc) = log.get("retcode").and_then(as_num) {
        if rc != 0 {
            return None;
        }
    }
    let list = log.get("data").and_then(|d| d.get("list")).and_then(Value::as_array)?;
    let mut best: Option<(i64, String)> = None; // (create_time, operator)
    for op in list {
        if op.get("booking_operation_type").and_then(as_num) != Some(4) {
            continue;
        }
        let operator = op.get("operator").and_then(Value::as_str).unwrap_or("");
        if !operator.contains('@') {
            continue;
        }
        let ct = op.get("create_time").and_then(as_num).unwrap_or(0);
        match &best {
            Some((best_ct, _)) if *best_ct <= ct => {}
            _ => best = Some((ct, operator.to_string())),
        }
    }
    best.map(|(_, op)| op)
}

/// Extract the account's own email from a profile response, lowercased+trimmed.
pub fn extract_self_email(profile: &Value) -> Option<String> {
    let data = profile.get("data").unwrap_or(profile);
    for key in [
        "email",
        "user_email",
        "email_address",
        "account_email",
        "contact_email",
        "login_email",
    ] {
        if let Some(s) = data.get(key).and_then(Value::as_str) {
            let norm = s.trim().to_lowercase();
            if norm.contains('@') {
                return Some(norm);
            }
        }
    }
    None
}

/// Fetch the account's own email via `fetch_profile` (Fase 5 calls once, then
/// passes the result into `verify_agency_dup`). Returns `None` on any error or
/// if no email field is present.
pub async fn fetch_self_email(client: &SpxClient, cookies: &SpxCookies) -> Option<String> {
    let profile = client.fetch_profile(cookies).await.ok()?;
    extract_self_email(&profile)
}

/// Verify the real acceptor of `booking_id`. `self_email` MUST already be
/// lowercased+trimmed by the caller. Stops as soon as an `@`-bearing acceptor is
/// found; otherwise retries with the 0/500/1500ms schedule and finally returns
/// `Inconclusive`.
pub async fn verify_agency_dup(
    client: &SpxClient,
    cookies: &SpxCookies,
    self_email: &str,
    booking_id: i64,
) -> AgencyDupOutcome {
    for delay in RETRY_DELAYS_MS {
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        let log = match client.fetch_bidding_log(cookies, booking_id).await {
            Ok(v) => v,
            Err(_) => continue, // transient fetch error — try again
        };
        if let Some(operator) = earliest_accept_operator(&log) {
            let rival = operator.trim().to_lowercase();
            return if rival == self_email {
                AgencyDupOutcome::Ours
            } else {
                AgencyDupOutcome::LostToAgency { rival_email: rival }
            };
        }
        // No `@`-bearing acceptor this attempt — keep retrying.
    }
    AgencyDupOutcome::Inconclusive
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn earliest_accept_operator_picks_type4_at_earliest_create_time() {
        let log = json!({
            "retcode": 0,
            "data": { "list": [
                { "booking_operation_type": 4, "operator": "late@x.com",  "create_time": 200 },
                { "booking_operation_type": 4, "operator": "early@x.com", "create_time": 100 },
                { "booking_operation_type": 5, "operator": "reject@x.com","create_time": 50  },
                { "booking_operation_type": 4, "operator": "system",      "create_time": 10  }
            ]}
        });
        assert_eq!(earliest_accept_operator(&log).as_deref(), Some("early@x.com"));
    }

    #[test]
    fn earliest_accept_operator_none_when_no_at_operator() {
        let log = json!({ "data": { "list": [
            { "booking_operation_type": 4, "operator": "system", "create_time": 10 }
        ]}});
        assert_eq!(earliest_accept_operator(&log), None);
    }

    #[test]
    fn earliest_accept_operator_handles_string_numbers() {
        let log = json!({ "data": { "list": [
            { "booking_operation_type": "4", "operator": "a@x.com", "create_time": "300" },
            { "booking_operation_type": "4", "operator": "b@x.com", "create_time": "150" }
        ]}});
        assert_eq!(earliest_accept_operator(&log).as_deref(), Some("b@x.com"));
    }

    #[test]
    fn extract_self_email_falls_back_across_keys_and_normalizes() {
        let p = json!({ "data": { "login_email": "  Me@Example.COM " } });
        assert_eq!(extract_self_email(&p).as_deref(), Some("me@example.com"));
        assert_eq!(extract_self_email(&json!({ "data": {} })), None);
    }
}
```

- [ ] **Step 2: Wire into `lib.rs`**

```rust
pub mod agency_dup;
pub use agency_dup::{fetch_self_email, verify_agency_dup, AgencyDupOutcome};
```

- [ ] **Step 3: wiremock tests — timing, early-stop, tie-break, classification (DoD #5)**

`verify_agency_dup` calls `SpxClient::fetch_bidding_log`, a GET to `/api/line_haul/agency/booking/bidding/log/list`. Point `SpxClient` at a wiremock server and assert timing + classification. `SpxCookies` derives `Default`.

```rust
// Backend/crates/executor/tests/agency_dup_mock.rs
//! DoD #5: verify_agency_dup retry timing + classification, against a wiremock
//! SPX (no real SPX). Asserts REAL elapsed time (not just call count).
use executor::{verify_agency_dup, AgencyDupOutcome};
use spx_client::client::PATH_BIDDING_LOG_LIST;
use spx_client::{SpxClient, SpxCookies};
use std::time::Instant;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cookies() -> SpxCookies {
    SpxCookies {
        csrftoken: "CSRF".into(),
        ..Default::default()
    }
}

fn accept_log(operator: &str, create_time: i64) -> serde_json::Value {
    serde_json::json!({
        "retcode": 0,
        "data": { "list": [
            { "booking_operation_type": 4, "operator": operator, "create_time": create_time }
        ]}
    })
}

#[tokio::test]
async fn early_stop_on_first_success_does_not_wait() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(PATH_BIDDING_LOG_LIST))
        .respond_with(ResponseTemplate::new(200).set_body_json(accept_log("me@x.com", 100)))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let start = Instant::now();
    let out = verify_agency_dup(&client, &cookies(), "me@x.com", 42).await;
    let elapsed = start.elapsed();

    assert_eq!(out, AgencyDupOutcome::Ours);
    assert!(
        elapsed.as_millis() < 400,
        "first-attempt success must NOT wait 500/1500ms (was {elapsed:?})"
    );
}

#[tokio::test]
async fn full_retry_timing_when_no_email_ever_found() {
    let server = MockServer::start().await;
    // Every attempt returns an accept op with NO '@' operator → never resolves.
    Mock::given(method("GET"))
        .and(path(PATH_BIDDING_LOG_LIST))
        .respond_with(ResponseTemplate::new(200).set_body_json(accept_log("system", 10)))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let start = Instant::now();
    let out = verify_agency_dup(&client, &cookies(), "me@x.com", 42).await;
    let elapsed = start.elapsed();

    assert_eq!(out, AgencyDupOutcome::Inconclusive);
    // 0 + 500 + 1500 = 2000ms of real sleeping.
    assert!(
        elapsed.as_millis() >= 1900 && elapsed.as_millis() < 3000,
        "expected ~2000ms of retry delay, was {elapsed:?}"
    );
}

#[tokio::test]
async fn rival_email_is_a_loss() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(PATH_BIDDING_LOG_LIST))
        .respond_with(ResponseTemplate::new(200).set_body_json(accept_log("rival@other.com", 100)))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let out = verify_agency_dup(&client, &cookies(), "me@x.com", 42).await;
    assert_eq!(
        out,
        AgencyDupOutcome::LostToAgency {
            rival_email: "rival@other.com".into()
        }
    );
}

#[tokio::test]
async fn tie_break_prefers_earliest_create_time() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "retcode": 0,
        "data": { "list": [
            { "booking_operation_type": 4, "operator": "late@x.com",  "create_time": 300 },
            { "booking_operation_type": 4, "operator": "early@x.com", "create_time": 100 }
        ]}
    });
    Mock::given(method("GET"))
        .and(path(PATH_BIDDING_LOG_LIST))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    // self is neither → the earliest-create_time operator is the rival.
    let out = verify_agency_dup(&client, &cookies(), "someone@else.com", 42).await;
    assert_eq!(
        out,
        AgencyDupOutcome::LostToAgency {
            rival_email: "early@x.com".into()
        }
    );
}
```

Note on `SpxClient::new` with wiremock: Fase 3's `client_requests.rs` tests already drive `SpxClient` against a wiremock `http://127.0.0.1:PORT` base, so the emulated client talks plaintext HTTP to the mock. If the installed `SpxClient::new` rejects a plaintext-localhost base under emulation, follow Fase 3's Task-9 note (construct without `.emulation()` in tests) — do not change the endpoint path or the JSON shape, which are the load-bearing parts.

- [ ] **Step 4: Test + clippy + commit**

```bash
cd Backend
cargo test -p executor --lib agency_dup
cargo test -p executor --test agency_dup_mock
cargo clippy -p executor --all-targets -- -D warnings
cd ..
git add Backend/crates/executor
git commit -m "feat(executor): verify_agency_dup (0/500/1500ms probe) + fetch_self_email + wiremock tests"
```

Expected: the 4 pure-helper unit tests + the 4 wiremock tests pass, including the ~2000ms full-retry timing assertion and the <400ms early-stop.

---

### Task 7: `find_best_matching_rule_compiled` in `core-domain` (additive) + first-wins & cross-check tests

**Files:**
- Modify: `Backend/crates/core-domain/src/matching.rs` (add ONE public function + tests; change nothing else)

**Interfaces:**
- Consumes: the existing `CompiledRule` (with `pub fn rank(&self) -> RuleRank` and `pub fn matches(&self, &Booking, &MatchState) -> bool`), `RuleRank` (`#[derive(... Ord ...)] pub struct RuleRank([i32; 6])`), `Booking`, `MatchState` — all already in `matching.rs`.
- Produces: `pub fn find_best_matching_rule_compiled(rules: &[CompiledRule], booking: &Booking, state: &MatchState) -> Option<usize>`.

**Design note (read before coding):** Purely additive — do NOT modify `CompiledRule`, `rule_rank`, `find_best_matching_rule`, or any of the 127 existing tests. Operates over ALREADY-compiled rules (the hot path compiles once, matches many). Tie-break is **first-wins** via a manual loop with strict `>` (a later same-rank rule does not replace an earlier one) — matches the existing `find_best_matching_rule`'s tie-break exactly, and must NOT use `Iterator::max_by_key` (last-wins).

- [ ] **Step 1: Add the function to `matching.rs`**

Insert after the existing `find_best_matching_rule` function (before the `matched_booking_id_for` function):

```rust
/// Hot-path variant of [`find_best_matching_rule`] over ALREADY-compiled rules:
/// returns the INDEX of the highest-ranked matching rule, or `None`. Tie-break is
/// **first-wins** — the strict `>` means a later rule that only TIES the current
/// best does not replace it, so the first rule to reach the top rank wins (never
/// `Iterator::max_by_key`, which is last-wins on ties and would diverge from the
/// reference on same-rank overlaps). Behaviorally identical to
/// `find_best_matching_rule`; it only differs in taking `&[CompiledRule]` (so the
/// caller reuses one compilation across many bookings) and returning an index.
pub fn find_best_matching_rule_compiled(
    rules: &[CompiledRule],
    booking: &Booking,
    state: &MatchState,
) -> Option<usize> {
    let mut best: Option<(usize, RuleRank)> = None;
    for (i, rule) in rules.iter().enumerate() {
        if !rule.matches(booking, state) {
            continue;
        }
        let rank = rule.rank();
        match best {
            Some((_, best_rank)) if rank > best_rank => best = Some((i, rank)),
            None => best = Some((i, rank)),
            _ => {} // equal or lower rank → keep the earlier (first-wins)
        }
    }
    best.map(|(i, _)| i)
}
```

- [ ] **Step 2: Add first-wins + cross-check tests**

Add a new module inside `matching.rs`'s existing `#[cfg(test)] mod tests` block (reuse the existing `use crate::test_support::{mk_booking, mk_rule, mk_state};` helpers):

```rust
    mod compiled_variant_tests {
        use super::*; // brings in CompiledRule, AcceptRule, Booking, BookingType,
                      // RuleMode, RuleConditions, RouteMatchMode, mk_* helpers, etc.

        // First-wins: two filter rules with IDENTICAL rank both match the same
        // booking; the FIRST index must win (not the last).
        #[test]
        fn first_wins_on_equal_rank() {
            let first = AcceptRule {
                id: "first".into(),
                ..mk_rule(
                    RuleMode::Filter,
                    RuleConditions {
                        coc_only: true,
                        ..Default::default()
                    },
                )
            };
            let second = AcceptRule {
                id: "second".into(),
                ..mk_rule(
                    RuleMode::Filter,
                    RuleConditions {
                        coc_only: true,
                        ..Default::default()
                    },
                )
            };
            let compiled = vec![
                CompiledRule::compile(&first),
                CompiledRule::compile(&second),
            ];
            let mut b = mk_booking(&[]);
            b.booking_type = BookingType::Spxid;

            // Both match with equal rank → index 0 (the first) must win.
            let idx = find_best_matching_rule_compiled(&compiled, &b, &mk_state());
            assert_eq!(idx, Some(0), "equal-rank tie must resolve to the FIRST rule");
            assert_eq!(compiled[idx.unwrap()].id, "first");
        }

        // Cross-check: on a shared corpus, the compiled-index variant agrees with
        // the existing `find_best_matching_rule` (same winning rule id, or both
        // None) — proving the hot-path variant is not a divergent reimplementation.
        #[test]
        fn agrees_with_find_best_matching_rule_on_corpus() {
            let rules = vec![
                AcceptRule {
                    id: "route-generic".into(),
                    priority: 1,
                    ..mk_rule(
                        RuleMode::Route,
                        RuleConditions {
                            destinations: vec!["Cileungsi DC".into()],
                            match_mode: RouteMatchMode::Flexible,
                            ..Default::default()
                        },
                    )
                },
                AcceptRule {
                    id: "route-specific".into(),
                    priority: 1,
                    ..mk_rule(
                        RuleMode::Route,
                        RuleConditions {
                            origin: "Padang DC".into(),
                            destinations: vec!["Cileungsi DC".into()],
                            ..Default::default()
                        },
                    )
                },
                AcceptRule {
                    id: "bkid".into(),
                    ..mk_rule(
                        RuleMode::BookingId,
                        RuleConditions {
                            booking_ids: vec!["SPXID_VM_001397649".into()],
                            ..Default::default()
                        },
                    )
                },
                AcceptRule {
                    id: "filter-coc".into(),
                    ..mk_rule(
                        RuleMode::Filter,
                        RuleConditions {
                            coc_only: true,
                            ..Default::default()
                        },
                    )
                },
            ];
            let compiled: Vec<CompiledRule> =
                rules.iter().map(CompiledRule::compile).collect();

            // A small corpus of bookings hitting different modes / no-match.
            let mut corpus: Vec<Booking> = Vec::new();
            corpus.push(mk_booking(&["Padang DC", "Cileungsi DC"])); // route
            let mut spx = mk_booking(&["Aceh DC", "Cileungsi DC"]);
            spx.spx_tx_id = "SPXID_VM_001397649".into();
            spx.booking_type = BookingType::Spxid;
            corpus.push(spx); // booking-id target should dominate
            let mut coc = mk_booking(&[]);
            coc.booking_type = BookingType::Spxid;
            corpus.push(coc); // filter-coc
            corpus.push(mk_booking(&["Nowhere DC", "Elsewhere DC"])); // likely no match

            for booking in &corpus {
                let via_owned = find_best_matching_rule(booking, &rules, &mk_state());
                let via_index = find_best_matching_rule_compiled(&compiled, booking, &mk_state());
                match (via_owned, via_index) {
                    (Some(owned), Some(i)) => assert_eq!(
                        owned.id, compiled[i].id,
                        "both variants must pick the same rule"
                    ),
                    (None, None) => {}
                    (owned_opt, idx_opt) => panic!(
                        "variants disagree: owned={:?} index={:?}",
                        owned_opt.map(|r| r.id),
                        idx_opt
                    ),
                }
            }
        }
    }
```

- [ ] **Step 3: Test (all of core-domain, proving nothing regressed) + clippy + commit**

```bash
cd Backend
cargo test -p core-domain          # all 127 existing tests + the new ones must pass
cargo clippy -p core-domain --all-targets -- -D warnings
cd ..
git add Backend/crates/core-domain
git commit -m "feat(core-domain): find_best_matching_rule_compiled (additive, first-wins) + cross-check tests"
```

Expected: every existing `core-domain` test still passes unchanged, plus the two new tests.

---

### Task 8: Final verification + Fase 4 sign-off

**Files:** None created — this task runs verification commands and checks off the plan.

**Interfaces:**
- Consumes: everything from Tasks 1-7.
- Produces: recorded evidence the Fase 4 Definition of Done (design doc) is met.

- [ ] **Step 1: Bring up services, run the full executor suite from clean containers**

```bash
cd Docker && docker compose up -d tower-postgres tower-redis && cd ..
# wait for healthy: docker compose -f Docker/docker-compose.yml ps
cd Backend && cargo test -p executor -- --test-threads=1 && cd ..
```

Expected: every `executor` test passes — the Redis round-trip, Layer-1 exactly-one-winner, the gate's three return values + fail-closed/fail-open + shared-key, the 7-day restore trim, the quota concurrency test, and the agency-dup timing/classification tests.

- [ ] **Step 2: Full workspace build/test/clippy**

```bash
cd Backend
cargo build --workspace
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets -- -D warnings
cd ..
```

Expected: all clean — `core-domain`'s existing tests + the new compiled-variant tests, `store`'s suite + the new `quota` module, `spx-client`, `executor`'s full suite, and the other crates, all green; clippy clean workspace-wide.

- [ ] **Step 3: `cargo deny check` — licenses stay clean (the `BSL-1.0` gate)**

```bash
cd Backend && cargo deny check && cd ..
```

Expected: `advisories ok, bans ok, licenses ok, sources ok`. If licenses fail, it is the `BSL-1.0` from `redis`'s `xxhash-rust` — confirm Task 1 Step 2 added `"BSL-1.0"` to the allow-list. Do NOT add any copyleft license.

- [ ] **Step 4: Confirm `executor`'s production dep footprint is exactly what's expected (DoD #9)**

```bash
cd Backend
cargo tree -p executor --edges normal
cargo tree -p executor --edges dev
cd ..
```

Expected (normal/production edges): `redis`, `dashmap`, `thiserror`, `uuid`, `serde_json`, `tokio`, `core-domain`, `spx-client`, `store`. Confirm:
- The I/O subsystems are exactly the design doc's `redis` (Redis), `store` (DB), `spx-client` (HTTP), `tokio` (runtime).
- **No direct `sqlx`** under normal edges (it appears only transitively under `store`, and under `executor`'s DEV edges from the `quota_pg` test) — production `executor` reaches the DB only through `store`.
- **No second DB driver** (no `tokio-postgres`/`diesel`) and **no duplicate HTTP client** (no `reqwest`; SPX HTTP is `spx-client`'s `wreq`, pulled transitively, not a second one).
- Exactly one `redis`, one `dashmap`.

- [ ] **Step 5: Cross-check every DoD item in the design doc**

Read `Docs/superpowers/specs/2026-07-13-fase-4-executor-design.md`'s "Definition of Done — Fase 4" (9 items) and cite the concrete evidence for each — do not just assert:
1. Layer 1 atomic (`DashSet::insert`) — `dedup.rs`'s `concurrent_claim_exactly_one_winner` (Task 2).
2. `ACCEPT_GATE_LUA` byte-for-byte + 0/-1/1 against real Redis — `gate.rs`'s `accept_gate_lua_is_byte_exact` + `gate_redis.rs`'s `gate_returns_proceed_already_and_quota_full` (Task 3).
3. Fail-closed (auto) vs fail-open (manual) under unreachable Redis — `gate_redis.rs`'s `auto_fails_closed_manual_fails_open_when_redis_unreachable` (Task 3).
4. Restore trims >7-day entries — `restore_redis.rs`'s `restore_keeps_in_window_and_trims_stale` (Task 4).
5. `verify_agency_dup` retry timing (real elapsed ≈2000ms), early-stop, tie-break — `agency_dup_mock.rs`'s four tests (Task 6).
6. `with_account_lock` + quota re-read: no lost update, cap never exceeded — `quota_pg.rs`'s `concurrent_consume_never_exceeds_cap_no_lost_update` (Task 5).
7. `find_best_matching_rule_compiled`: first-wins + cross-check vs `find_best_matching_rule` — `matching.rs`'s `compiled_variant_tests` (Task 7).
8. Manual shares the auto claim key — `gate_redis.rs`'s `manual_and_auto_share_the_claim_key` (Task 3).
9. `cargo test`/`clippy`/`deny` clean + no unexpected I/O deps — Steps 1-4 output.

- [ ] **Step 6: Mark this plan complete**

Check every remaining `- [ ]` box in this file to `- [x]` by hand or with a targeted script — then verify (grep) that no non-checkbox prose containing the literal `- [ ]` substring got corrupted. **This exact mistake — corrupting `- [ ]` sequences that appear inside prose rather than as real checkboxes — has already happened TWICE in this project's history (caught during Fase 1's and Fase 3's sign-offs). Do not repeat it a third time.** Only real leading-`- [ ]` step checkboxes should change; any `- [ ]` embedded in a sentence or code block must be left exactly as-is.

- [ ] **Step 7: Commit**

```bash
git add Backend Docs/superpowers/plans/2026-07-13-fase-4-executor.md
git commit -m "test(executor): Fase 4 sign-off — full verification + DoD cross-check"
```

Fase 4 is done once this commits clean. Fase 5 (poller — orchestration, single-flight, notif watcher, auto-login) is the next master-spec phase; it consumes this `executor` library (and MUST `.await restore_accepted_ids` before the first poll per the Layer-3 contract). Do not start it in this same task; it gets its own spec/plan cycle.
