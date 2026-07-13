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
