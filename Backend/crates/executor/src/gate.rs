//! Layer 2 — the Redis claim gate (`ACCEPT_GATE_LUA` + transport) plus the
//! shared `RedisPool`/`ExecutorHandle`/`ExecutorError` types.
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::{AsyncCommands, Script};
use tokio::sync::{Mutex, OnceCell};
use uuid::Uuid;

use crate::dedup::AccountDedupState;

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
// Task 1 only builds the skeleton: `redis`/`gate`/`account_locks` are wired
// into `try_claim_*` (Task 3) and the per-account lock helpers (Task 4), so
// nothing reads them yet. Drop this once those tasks land.
#[allow(dead_code)]
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
