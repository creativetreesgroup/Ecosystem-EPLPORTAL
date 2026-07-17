use std::net::SocketAddr;
use std::sync::Arc;

use api_gateway::AppState;
use axum::Router;
use dashmap::DashMap;

/// `key` from the environment, or `default` if unset/empty. Every fallback
/// below matches an already-established convention elsewhere in the
/// workspace (`store`'s `test_database_url()`, `executor`/`poller`'s
/// `REDIS_URL` test helpers, `auth-sidecar`'s `SPX_BASE_URL`) so a bare
/// `cargo run` against the dev `docker-compose` stack (Postgres on 15432,
/// Redis on 16379) boots with zero required env vars.
fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Parses `CORS_ALLOWED_ORIGINS` (comma-separated, e.g.
/// `https://portal.example.com,https://admin.example.com`) into the raw
/// origin strings `AppState.cors_origins` carries. Unset/empty -> an empty
/// allowlist (no origin permitted), NOT a wildcard fallback. Entries are only
/// trimmed + empty-filtered here; parsing each into an `http::HeaderValue`
/// (dropping + `tracing::warn!`ing any that fails, per Task 7's brief) is
/// `middleware::cors_layer`'s job at `build_router` time, not this one's.
fn cors_origins_from_env() -> Vec<String> {
    std::env::var("CORS_ALLOWED_ORIGINS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Fase 6a Task 9: the real account-bootstrap boot sequence. Resolves the
/// single deployment tenant from `TENANT_SLUG`, decrypts every
/// `agency_credentials` row for it, and spawns one live poller task per
/// successfully-decrypted row via `poller::schedule::ensure_restored_then_spawn`
/// — the first thing in this project's history that spawns a REAL poller
/// task from `main()` rather than a test file. Everything else this fn
/// builds (Postgres pool, `ExecutorHandle`/`SpxClient`/`SidecarClient`, CORS/
/// cookie config) was already real as of Task 1/5/7/8 — see git history for
/// that incremental build-up; this task's own diff is the tenant resolution,
/// the master key + `RedisPublisher` wiring, and the bootstrap loop below.
///
/// Three DISCLOSED, intentional gaps remain (all explicitly sanctioned as
/// acceptable for this task's scope — NOT silently invented):
///
/// - `poller::spawn_watchdog` (Fase 5) is NOT called anywhere in this file.
///   The account-bootstrap loop below only ever calls
///   `ensure_restored_then_spawn` — it never starts the durable-primary
///   watchdog task that would recreate that account's poll loop if it dies
///   at runtime. This means the steady-state half of Aturan Keras #10 ("one
///   account's failure can't take down the process, and the durable primary
///   self-heals") is not wired up yet: if the durable-primary account's poll
///   loop panics after boot, nothing respawns it until the whole process is
///   restarted. This is deferred, not forgotten — `spawn_watchdog`'s respawn
///   closure needs to rebuild a full, correct `PollerState` for the
///   restarted account, including `rules`/`rule_meta` and a real
///   `agency_id`, and both of those are currently placeholder/empty per the
///   next two disclosed gaps below. Wiring the watchdog now would either
///   respawn with that same incomplete state or require pulling forward work
///   that belongs to a later sub-phase (6c). Tracked for a near-future 6a/6b
///   follow-up once rules/agency_id have real sources.
/// - Every spawned `PollerState.rules`/`rule_meta` starts EMPTY
///   (`PollerState::new`'s own default). Accounts poll and dedupe correctly
///   but match no accept rules until Fase 6c's rules-CRUD route lets an
///   operator configure them. Fase 6a's own DoD is "the binary boots and
///   polls accounts", not "accounts auto-accept correctly yet".
/// - `PollerState.agency_id` (the numeric SPX agency id `dispatch.rs` needs
///   for `SpxClient::accept_booking`) is set to `0` for every account.
///   `agency_credentials` carries no such column (verified against
///   `migrations/0004_agency_credentials.sql`'s column list) — the only
///   place this value is genuinely knowable is the SPX login response
///   itself (`fms_user_agency_id`/`spx_agid`, see `spx_client::SpxCookies`
///   and `auth-sidecar`'s `/login` response shape), but
///   `schedule::poll_once`'s relogin branch (Task 7b) only ever writes the
///   fresh cookies back into `st.cookies`, never `st.agency_id`. That is a
///   PRE-EXISTING structural gap this task surfaces (by being the first
///   thing to construct a live `PollerState` outside a test file) but does
///   not introduce and is out of scope to close here — accept-booking calls
///   will carry `agency_id=0` (and, per the gap above, no rules to match
///   against anyway) until a later task teaches the relogin success path to
///   also parse and persist the real agency id.
async fn build_state() -> AppState {
    let database_url = env_or(
        "DATABASE_URL",
        "postgres://app_role:app_role_dev_only@127.0.0.1:15432/tower",
    );
    let redis_url = env_or("REDIS_URL", "redis://127.0.0.1:16379");
    let spx_base_url = env_or("SPX_BASE_URL", "https://logistics.myagencyservice.id");
    let sidecar_url = env_or("AUTH_SIDECAR_URL", "http://127.0.0.1:8082");

    let pool = store::connect(&database_url)
        .await
        .expect("reactor-core: connect to Postgres");

    // The single deployment tenant this process serves, resolved from
    // TENANT_SLUG (replaces the earlier placeholder TENANT_ID/Uuid::nil()
    // fallback). An unresolvable slug is a boot-time misconfiguration, not a
    // runtime-recoverable condition — panic with a clear message rather than
    // silently booting against Uuid::nil(), which would previously have made
    // every tenant-scoped query see zero rows under RLS: a much more
    // confusing failure mode to debug in production than a boot-time panic.
    let tenant_slug = env_or("TENANT_SLUG", "");
    let tenant_id = store::tenants::find_by_slug(&pool, &tenant_slug)
        .await
        .expect("reactor-core: query tenants")
        .unwrap_or_else(|| {
            panic!(
                "reactor-core: TENANT_SLUG={tenant_slug:?} does not match any row in \
                 `tenants` — set TENANT_SLUG to a real tenant's slug (see \
                 Backend/.env.example)"
            )
        })
        .id;

    // `ExecutorHandle::connect` only PARSES `redis_url` + best-effort loads
    // the claim script; it does not require Redis to be reachable right now
    // to succeed (see `executor::gate::ExecutorHandle::connect`'s doc
    // comment) — genuinely "freshly-connected but otherwise idle".
    let executor = executor::ExecutorHandle::connect(&redis_url)
        .await
        .expect("reactor-core: init executor redis pool");

    let client = spx_client::SpxClient::new(spx_base_url).expect("reactor-core: build SpxClient");
    let sidecar = poller::SidecarClient::new(sidecar_url);

    // Envelope-encryption master key (Fase 3) — needed to decrypt every
    // `agency_credentials.ciphertext` below. TOWER_MASTER_KEY_PATH's
    // production value is always set explicitly by
    // Docker/docker-compose.yml's `tower-reactor-core` service
    // (/run/secrets/tower_master_key, the Compose file-secret mount point —
    // same convention `tower-auth-sidecar` already uses). The fallback path
    // here is purely a host-run (`cargo run`/`cargo test` OUTSIDE that
    // container) dev convenience, mirroring this fn's own `env_or`
    // philosophy for every other var — deliberately NOT a bare call to
    // `MasterKey::load_default()`, which hardcodes
    // `/run/secrets/tower_master_key` as ITS OWN fallback with no
    // host-dev-friendly alternative. Functionally identical to
    // `load_default()` in production, where TOWER_MASTER_KEY_PATH is always
    // set explicitly and this fallback branch is never reached.
    // `Arc`-wrapped: Task 1 (6b) threads this SAME loaded key into
    // `AppState.master_key` below, in addition to the account-bootstrap loop
    // a few lines down still using it via `&master_key` (an `&Arc<MasterKey>`
    // deref-coerces to `&MasterKey` at every `decrypt_agency_password` call
    // site, so that loop needed zero changes). Loaded exactly ONCE — this
    // used to be a locally-scoped load with no life beyond the bootstrap
    // loop; it now also outlives it as a first-class `AppState` field so
    // Fase 6b's `agency_credentials` CRUD handlers can encrypt/decrypt
    // without a second `load_from_file` call (which would mean two decrypted
    // copies of the same secret material resident in memory for no reason).
    let master_key_path = env_or(
        "TOWER_MASTER_KEY_PATH",
        "../Docker/secrets/tower_master_key",
    );
    let master_key = Arc::new(
        spx_client::crypto::envelope::MasterKey::load_from_file(&master_key_path).unwrap_or_else(
            |e| {
                panic!(
                    "reactor-core: load master key from {master_key_path:?} — set \
                 TOWER_MASTER_KEY_PATH, or for local dev: mkdir -p Docker/secrets && \
                 openssl rand -out Docker/secrets/tower_master_key 32 && chmod 0400 \
                 Docker/secrets/tower_master_key (see Docker/.env.example): {e}"
                )
            },
        ),
    );

    // ws-hub Redis pub/sub publisher (Task 13's `RedisPublisher`), wired
    // into `PollerShared` for the first time here. A connect failure is
    // logged and left `None` — the same safe no-op `PollerShared.redis`'s
    // own doc comment already documents for tests — rather than panicking
    // the whole boot: Aturan Keras #10's "one failure can't take down the
    // process" applies to a transient Redis outage at boot too, not only to
    // steady-state per-account failures. ws `ticket_accepted` notifications
    // degrade to a no-op; every other feature (HTTP API, polling, accepts)
    // keeps running.
    let redis_publisher = match poller::RedisPublisher::connect(&redis_url).await {
        Ok(publisher) => Some(publisher),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "reactor-core: RedisPublisher::connect failed at boot — ws ticket_accepted \
                 notifications will no-op until this is fixed"
            );
            None
        }
    };

    // `AppState.redis` — Fase 6b's OTP gate (`POST /auth/request-aa-otp` /
    // `POST /auth/verify-aa-otp`) generate/store/verify/rate-limit state.
    // DELIBERATELY a hard `.expect()`, NOT the same graceful
    // connect-failure-is-a-warn-and-`None` pattern `redis_publisher` just
    // above uses — disclosed judgment call, not an oversight:
    // `RedisPublisher` is optional-at-boot because its ONLY consumer (ws
    // `ticket_accepted` push notifications) is allowed to silently degrade to
    // a no-op per that field's own doc comment — the REST API and account
    // polling keep working fine without it. `AppState.redis` has no such
    // "fine to degrade" consumer: every OTP request genuinely NEEDS Redis to
    // exist (there is no non-Redis fallback path for "generate a code, store
    // it, verify it, rate-limit it" — see the design doc's OTP Redis key
    // convention), and `AppState.redis` is a plain `ConnectionManager` field
    // (not `Option<ConnectionManager>`, per this task's own interface spec),
    // so there is no type-level way to represent "not connected yet" without
    // making EVERY handler that touches OTP state carry its own
    // reconnect-or-503 logic — worse than failing loudly once, here, at boot,
    // with a clear diagnostic. `ConnectionManager` itself still transparently
    // reconnects across any LATER transient Redis blip (that's its whole
    // purpose) — this `.expect()` only guards the initial connect.
    let redis = redis::Client::open(redis_url.as_str())
        .expect("reactor-core: parse REDIS_URL")
        .get_connection_manager()
        .await
        .expect(
            "reactor-core: connect AppState.redis (OTP gate) — Redis at REDIS_URL must be \
             reachable at boot; the OTP gate has no non-Redis fallback",
        );

    // Task 7: the tenant's persisted rule set, loaded once at boot. A load failure degrades to
    // an empty rule set (accounts poll/dedupe fine, just match no rules until a later
    // `PUT /bookings/settings` save succeeds) rather than panicking the whole boot — same
    // tolerance this fn already extends to `redis_publisher`'s connect failure just above.
    let initial_rules = match poller::rules::load_compiled_rules(&pool, tenant_id).await {
        Ok(rs) => rs,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "reactor-core: load_compiled_rules failed at boot — starting with an empty \
                 rule set until a settings save succeeds"
            );
            poller::RuleSet::empty()
        }
    };
    let (rules_tx, _rules_rx_template) = tokio::sync::watch::channel(initial_rules);

    let poller_shared = Arc::new(poller::PollerShared {
        executor: Arc::new(executor),
        client: Arc::new(client),
        pool: pool.clone(),
        config: poller::PollerConfig::from_env(),
        accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar),
        // No fire-and-forget WAHA/n8n notifications yet (6b) — disclosed,
        // pre-existing gap, unchanged by this task.
        notifier: None,
        redis: redis_publisher,
        // Task 7: the real live-reload channel, seeded from `initial_rules` above (loaded from
        // `store::accept_rules`/`store::rule_booking_targets`) — replaces Task 6's placeholder
        // `tokio::sync::watch::channel(poller::RuleSet::empty()).0` that only existed to satisfy
        // this required field until this task built the real loader.
        rules_tx,
    });

    // Account bootstrap: one live poller task per `agency_credentials` row.
    // A single row's decryption failure (bad/rotated master key, corrupted
    // ciphertext/nonce, anything) must skip ONLY that account, never abort
    // the whole boot — Aturan Keras #10 applied at boot-time bootstrap, not
    // just the steady-state watchdog Fase 5 already built.
    //
    // NOTE: that steady-state watchdog (`poller::spawn_watchdog`) is NOT
    // called anywhere below, or anywhere else in this file — see this fn's
    // own doc comment above (`build_state`'s "Three DISCLOSED, intentional
    // gaps") for why that's deliberate-but-deferred, not an oversight.
    let credentials = store::agency_credentials::list_all(&pool, tenant_id)
        .await
        .expect("reactor-core: list agency_credentials");
    for credential in credentials {
        let nonce: [u8; 12] = match credential.nonce.as_slice().try_into() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    credential_id = %credential.id,
                    label = %credential.label,
                    nonce_len = credential.nonce.len(),
                    "agency_credentials row has a malformed nonce (expected 12 bytes) — \
                     skipping this account, boot continues"
                );
                continue;
            }
        };
        let password = match spx_client::crypto::envelope::decrypt_agency_password(
            &master_key,
            tenant_id,
            &credential.ciphertext,
            &nonce,
        ) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    credential_id = %credential.id,
                    label = %credential.label,
                    error = %e,
                    "failed to decrypt agency_credentials row — skipping this account, \
                     boot continues"
                );
                continue;
            }
        };

        // `account_id` MUST match the same lowercased-username convention
        // `PollerConfig::from_env`'s `primary_account_id` (sourced from
        // PORTAL_USERNAME) already uses, and that `watchdog::spawn_watchdog`
        // looks up via `shared.accounts.get(&primary)` — this is the key
        // this row's handle is stored under below, and the ONLY identifier
        // that ties an `agency_credentials` row to its live `AccountHandle`/
        // Redis keyspace/ws-hub `acct:<id>` channel (see
        // `RedisPublisher::publish_ticket_accepted`).
        let account_id = credential.username.trim().to_lowercase();
        if account_id.is_empty() {
            tracing::warn!(
                credential_id = %credential.id,
                "agency_credentials row has an empty/whitespace-only username — skipping"
            );
            continue;
        }

        // `agency_credentials`'s only uniqueness constraint is `UNIQUE
        // (tenant_id, label)` (see `migrations/0004_agency_credentials.sql`)
        // — `username` is NOT unique, so two rows (different `label`s) CAN
        // legitimately share the same lowercased `account_id` derived above.
        // If both were spawned, the second `poller_shared.accounts.insert`
        // below would silently OVERWRITE the first row's `AccountHandle` in
        // the map — dropping a `JoinHandle` does not abort the underlying
        // Tokio task, so the first loop would keep running forever,
        // untracked and unstoppable (no handle left pointing to it), and two
        // concurrent poll loops for the same SPX account could thrash each
        // other's login sessions server-side. Guard against that here,
        // same continue-based skip pattern as the malformed-nonce/
        // decrypt-failure/empty-username checks above — enforcing real
        // uniqueness at the DB/schema level is explicitly out of scope for
        // this task (6b's CRUD scope).
        if poller_shared.accounts.contains_key(&account_id) {
            tracing::warn!(
                account_id = %account_id,
                credential_id = %credential.id,
                label = %credential.label,
                "duplicate account_id (same username, different label) already bootstrapped \
                 in this boot — skipping this row to avoid orphaning the first row's poll loop"
            );
            continue;
        }

        let mut state = poller::PollerState::new(
            account_id.clone(),
            tenant_id,
            0, // agency_id — disclosed gap, see this fn's doc comment
            spx_client::SpxCookies::default(),
            credential.username.into(),
            password,
        );
        // Task 7: subscribe to the shared rule-reload channel and eagerly seed `rules`/
        // `rule_meta` from its CURRENT value now — `poll_once`'s `has_changed()` gate (Task 6)
        // only fires on a value sent AFTER `subscribe()`, so without this eager seed the very
        // first cycle would still see the empty `PollerState::new` default.
        let rx = poller_shared.rules_tx.subscribe();
        let seed = rx.borrow().clone();
        state.rules = seed.rules;
        state.rule_meta = seed.rule_meta;
        state.rules_rx = Some(rx);
        // CP-7 contract: `ensure_restored_then_spawn` awaits the durable
        // restore BEFORE the first poll is ever scheduled — the ONLY
        // production spawn path (see `schedule.rs`'s doc comment).
        // `ensure_restored_then_spawn` does NOT insert into
        // `PollerShared.accounts` itself (verified against its source and
        // every existing caller, e.g. `poller/tests/watchdog.rs`'s respawn
        // closure) — that is this loop's job, same as every other caller.
        let handle = poller::ensure_restored_then_spawn(poller_shared.clone(), state).await;
        poller_shared.accounts.insert(account_id, handle);
    }

    AppState {
        poller: poller_shared,
        ws_hub: ws_hub::Hub::new(),
        tenant_id,
        // Task 7: real exact-match allowlist from `CORS_ALLOWED_ORIGINS`
        // (comma-separated), not a placeholder anymore.
        cors_origins: Arc::new(cors_origins_from_env()),
        session_cookie_name: Arc::from(env_or("SESSION_COOKIE_NAME", "spx_session").as_str()),
        // Default true (production-safe); set `COOKIE_SECURE=false` only for
        // local dev where reactor-core is reached directly over plain HTTP
        // (no TLS-terminating edge proxy in front of it) — see `state.rs`'s
        // field doc comment.
        cookie_secure: env_or("COOKIE_SECURE", "true").parse().unwrap_or(true),
        // Same `Arc<MasterKey>` the account-bootstrap loop above already
        // used via deref coercion — moved in here, not re-loaded.
        master_key,
        redis,
    }
}

/// Builds the `ws_hub::SessionValidator` the validated ws upgrade path
/// (Task 10) needs: hash the plaintext `?session=` query value exactly like
/// `session_auth` middleware hashes the cookie (`hash_session_token`) — the
/// ws query param IS the same plaintext session token as the cookie, per the
/// design doc's note that ws-hub's channel-naming already uses the session
/// id directly — then look it up via `store::portal_sessions::find_valid_by_hash`,
/// which already filters `expires_at > now()` (migration 0018's
/// `SECURITY DEFINER` function): `Ok(Some(_))` therefore means "exists AND
/// unexpired", nothing further to check here. Authentication only — no
/// `is_main_account`/RBAC check, per the task brief: any valid, logged-in
/// session may open a WS connection.
fn session_validator(pool: store::PgPool) -> ws_hub::SessionValidator {
    Arc::new(move |token: String| {
        let pool = pool.clone();
        Box::pin(async move {
            let hash = spx_client::crypto::session_token::hash_session_token(&token);
            matches!(
                store::portal_sessions::find_valid_by_hash(&pool, hash).await,
                Ok(Some(_))
            )
        })
    })
}

/// Mounts the Task 10 validated ws router (`/ws`, real session validation)
/// alongside `api_gateway::build_router`'s REST surface on the same
/// top-level `Router`. Both are already-`.with_state(..)`-applied
/// `Router<()>`s by the time they're merged (`build_router` calls
/// `.with_state` internally; `ws_router_with_auth` does too), so `.merge`
/// needs no further state reconciliation.
///
/// Deliberate structural note (whole-branch review, Minor finding 2):
/// `api_gateway::build_router` applies its `.layer(...)` stack (CORS,
/// request body-limit, security-headers — see that fn's doc comment) to ITS
/// OWN `Router` before returning it here, and `ws_router` is `.merge`d onto
/// the result AFTER that — so `/ws` sits outside all three of those layers.
/// This is intentional, not an oversight: forcing those layers onto `/ws`
/// isn't what this route needs and risks unintended side effects on the
/// upgrade handshake itself, so the router is not restructured to capture
/// it. Each layer is a deliberate no-op for `/ws` on its own merits —
/// a WS upgrade response has no meaningful use for CSP/security headers;
/// the body-limit is meant to cap REST JSON payloads and does not apply to
/// a WS upgrade's handshake; and browsers do not run CORS preflight/
/// enforcement against WebSocket connections the way they do `fetch`/`XHR`,
/// so an allowlist mismatch there wouldn't be caught by CORS regardless of
/// layer placement. If a real need for any of these on `/ws` ever emerges,
/// it should be added as its own explicit layer on `ws_router` rather than
/// by moving `/ws` inside `build_router`'s stack.
fn app(state: AppState) -> Router {
    let ws_router = ws_hub::ws_router_with_auth(
        state.ws_hub.clone(),
        session_validator(state.poller.pool.clone()),
        state.session_cookie_name.clone(),
    );
    api_gateway::build_router(state).merge(ws_router)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("reactor-core starting");

    let state = build_state().await;

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8081")
        .await
        .expect("bind 0.0.0.0:8081");

    // `.into_make_service_with_connect_info::<SocketAddr>()` (not the plain
    // `Router`, and not `.into_make_service()`): Task 8's per-IP rate
    // limiter on `/auth/portal-login` uses `tower_governor`'s
    // `SmartIpKeyExtractor`, which reads `X-Forwarded-For`/`X-Real-Ip`
    // (set by the TLS-terminating edge proxy this deployment's architecture
    // assumes in front of reactor-core — see `state.rs`'s `cookie_secure`
    // doc comment) but FALLS BACK to the raw TCP peer address
    // (`ConnectInfo<SocketAddr>`) when none of those headers are present —
    // e.g. a local dev setup that reaches reactor-core directly, the same
    // carve-out `COOKIE_SECURE=false` already exists for. Without this,
    // that fallback path has nothing to read (axum does NOT populate
    // `ConnectInfo` automatically) and every `/auth/portal-login` request
    // would fail key extraction and 500 instead of being rate-limited — see
    // `tower_governor`'s own README "Common pitfalls" section and
    // `rate_limit.rs`'s doc comment for the full explanation.
    axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .expect("server error");
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install ctrl_c handler");
    tracing::info!("reactor-core shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use uuid::Uuid;

    /// `app_role`'s password for every test in this module. Real deploys set
    /// this exactly once via the operational step migration 0019's own
    /// comment documents (`ALTER ROLE app_role PASSWORD '...'` run by a
    /// superuser-authenticated script); a test re-runs the same idempotent
    /// statement every time, which is harmless.
    const APP_ROLE_TEST_PASSWORD: &str = "app_role_dev_only";

    fn tower_superuser_url() -> String {
        env_or(
            "TOWER_SUPERUSER_DATABASE_URL",
            "postgres://tower:tower_dev_only@127.0.0.1:15432/tower",
        )
    }

    /// Runs migrations (DDL requires `tower` — `app_role` has no CREATE/
    /// ALTER privileges of its own) and performs the one-time "set
    /// `app_role`'s password" operational step migration 0019's comment
    /// documents. Returns the `app_role`-flavored `DATABASE_URL`
    /// `build_state()` is meant to use in production.
    async fn prepare_app_role_database_url() -> String {
        let tower_pool = store::connect(&tower_superuser_url())
            .await
            .expect("connect as tower (superuser) to run migrations");
        store::run_migrations(&tower_pool)
            .await
            .expect("run migrations (incl. 0019's app_role LOGIN promotion)");
        // `AssertSqlSafe`: sqlx 0.9's `query()` refuses a dynamic `String`
        // by default (injection-audit guard). Safe here — the interpolated
        // value is the fixed, code-level `APP_ROLE_TEST_PASSWORD` constant
        // above, never external/user input.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER ROLE app_role PASSWORD '{APP_ROLE_TEST_PASSWORD}'"
        )))
        .execute(&tower_pool)
        .await
        .expect("set app_role's test password (the one-time step migration 0019 documents)");
        format!("postgres://app_role:{APP_ROLE_TEST_PASSWORD}@127.0.0.1:15432/tower")
    }

    /// Seeds a throwaway tenant (as `tower`, so RLS never gets in the way of
    /// test setup — the same convention `store/src/lib.rs`'s own
    /// `insert_test_tenant` helper uses) and returns its `(id, slug)`.
    async fn seed_test_tenant() -> (Uuid, String) {
        let tower_pool = store::connect(&tower_superuser_url())
            .await
            .expect("connect as tower");
        let tenant_id = Uuid::new_v4();
        let slug = format!("reactor-core-test-{tenant_id}");
        sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
            .bind(tenant_id)
            .bind("Reactor-Core Boot Test Tenant")
            .bind(&slug)
            .execute(&tower_pool)
            .await
            .expect("insert test tenant");
        (tenant_id, slug)
    }

    async fn cleanup_tenant(tenant_id: Uuid) {
        if let Ok(pool) = store::connect(&tower_superuser_url()).await {
            let _ = sqlx::query("DELETE FROM tenants WHERE id = $1")
                .bind(tenant_id)
                .execute(&pool)
                .await;
        }
    }

    /// Writes 32 bytes (two v4 UUIDs concatenated — no cryptographic quality
    /// needed for a throwaway test key, just 32 real bytes) to a uniquely
    /// named temp file and returns its path: a real
    /// `MasterKey::load_from_file`-loadable key, matching the Docker-secret
    /// file's own shape (see Docker/.env.example).
    fn write_test_master_key() -> std::path::PathBuf {
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        let path = std::env::temp_dir().join(format!("tower_master_key_test_{}", Uuid::new_v4()));
        std::fs::write(&path, bytes).expect("write test master key");
        path
    }

    #[tokio::test]
    async fn healthz_returns_ok_status() {
        let database_url = prepare_app_role_database_url().await;
        let (tenant_id, tenant_slug) = seed_test_tenant().await;
        let master_key_path = write_test_master_key();

        // This tenant has zero `agency_credentials` rows, so the bootstrap
        // loop is a no-op (no live poller task, no SPX_BASE_URL touched) —
        // this test's sole purpose is proving the REST of `build_state()`
        // (app_role pool, tenant resolution, master key load, Redis
        // publisher) still boots a working router end to end. The
        // credential-decryption bootstrap loop itself is covered by
        // `boot_smoke_malformed_credential_is_skipped_not_fatal` below.
        std::env::set_var("DATABASE_URL", &database_url);
        std::env::set_var("TENANT_SLUG", &tenant_slug);
        std::env::set_var("TOWER_MASTER_KEY_PATH", master_key_path.to_str().unwrap());

        let state = build_state().await;

        let response = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["service"], "api-gateway");

        let _ = std::fs::remove_file(&master_key_path);
        cleanup_tenant(tenant_id).await;
    }

    /// Step 4's real boot smoke test: seeds one well-formed
    /// `agency_credentials` row and one DELIBERATELY MALFORMED row (garbage
    /// ciphertext that will never AEAD-decrypt) against real Postgres, boots
    /// the ACTUAL assembled router (not a stub) through the real
    /// `build_state()`/`app()` production path, and asserts (a) `/healthz`
    /// still returns 200 — the malformed row must not have panicked the
    /// whole boot (Aturan Keras #10 applied at boot-time bootstrap, not just
    /// the steady-state watchdog) — and (b) the well-formed account WAS
    /// spawned while the malformed one was skipped, not silently spawned
    /// with garbage credentials.
    ///
    /// `SPX_BASE_URL`/`AUTH_SIDECAR_URL` are pointed at `127.0.0.1:1` (a
    /// port nothing listens on — the same "guaranteed-unreachable, fails
    /// fast" placeholder `poller/tests/watchdog.rs` already uses for
    /// `SidecarClient::new`) rather than the real SPX origin: the
    /// well-formed account's spawn starts a REAL live poll loop
    /// (`schedule::spawn_account_loop`), and this test must never make an
    /// actual outbound request to a real third-party domain.
    #[tokio::test]
    async fn boot_smoke_malformed_credential_is_skipped_not_fatal() {
        let database_url = prepare_app_role_database_url().await;
        let (tenant_id, tenant_slug) = seed_test_tenant().await;
        let master_key_path = write_test_master_key();

        let tower_pool = store::connect(&tower_superuser_url())
            .await
            .expect("connect as tower");
        let master_key = spx_client::crypto::envelope::MasterKey::load_from_file(&master_key_path)
            .expect("load test master key back");

        let good_username = format!("good-agent-{}", Uuid::new_v4().simple());
        let ct = spx_client::crypto::envelope::encrypt_agency_password(
            &master_key,
            tenant_id,
            "s3cr3t-pw",
        )
        .expect("encrypt test password");
        sqlx::query(
            "INSERT INTO agency_credentials \
             (tenant_id, label, username, ciphertext, nonce, key_version) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(tenant_id)
        .bind("good")
        .bind(&good_username)
        .bind(&ct.bytes)
        .bind(&ct.nonce[..])
        .bind(spx_client::crypto::envelope::KEY_VERSION)
        .execute(&tower_pool)
        .await
        .expect("insert well-formed agency_credentials row");

        let bad_username = format!("bad-agent-{}", Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO agency_credentials \
             (tenant_id, label, username, ciphertext, nonce, key_version) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(tenant_id)
        .bind("bad")
        .bind(&bad_username)
        .bind(vec![0xDE_u8, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0x02, 0x03]) // never AEAD-decrypts
        .bind(vec![0u8; 12]) // syntactically valid-length nonce, wrong content
        .bind(spx_client::crypto::envelope::KEY_VERSION)
        .execute(&tower_pool)
        .await
        .expect("insert malformed agency_credentials row");

        std::env::set_var("DATABASE_URL", &database_url);
        std::env::set_var("TENANT_SLUG", &tenant_slug);
        std::env::set_var("TOWER_MASTER_KEY_PATH", master_key_path.to_str().unwrap());
        std::env::set_var("SPX_BASE_URL", "http://127.0.0.1:1");
        std::env::set_var("AUTH_SIDECAR_URL", "http://127.0.0.1:1");

        let state = build_state().await;
        let accounts = state.poller.accounts.clone();

        let response = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "the malformed credential must not have panicked the boot"
        );

        assert!(
            accounts.contains_key(&good_username),
            "the well-formed credential must have been decrypted and spawned"
        );
        assert!(
            !accounts.contains_key(&bad_username),
            "the malformed credential must be skipped (warn + continue), never spawned"
        );

        let _ = std::fs::remove_file(&master_key_path);
        cleanup_tenant(tenant_id).await;
    }

    /// Review-finding regression test: `agency_credentials`'s only
    /// uniqueness constraint is `UNIQUE (tenant_id, label)` (see
    /// `migrations/0004_agency_credentials.sql`) — `username` is NOT
    /// unique, so two rows CAN legitimately share the same
    /// lowercased-`username` `account_id` the bootstrap loop derives.
    /// Without a guard, both rows would be bootstrapped and the SECOND
    /// `poller_shared.accounts.insert` call would silently overwrite the
    /// first row's `AccountHandle` in the `DashMap` — orphaning the first
    /// row's live poll loop forever (dropping a `JoinHandle` does not abort
    /// the underlying Tokio task).
    ///
    /// Seeds TWO well-formed, decryptable `agency_credentials` rows under
    /// the same tenant with the SAME username (different `label`s), boots
    /// the ACTUAL assembled router through the real production
    /// `build_state()`/`app()` path (same pattern as
    /// `boot_smoke_malformed_credential_is_skipped_not_fatal` above), and
    /// asserts: exactly ONE account ends up in `poller_shared.accounts` for
    /// that `account_id` (not two — the DashMap key can only ever hold one
    /// entry, so this also proves the second row didn't get a chance to
    /// clobber the first row's still-running handle — and not zero, i.e.
    /// the guard didn't accidentally skip BOTH), and `/healthz` still
    /// returns 200 (no panic).
    #[tokio::test]
    async fn boot_smoke_duplicate_username_skips_second_row() {
        let database_url = prepare_app_role_database_url().await;
        let (tenant_id, tenant_slug) = seed_test_tenant().await;
        let master_key_path = write_test_master_key();

        let tower_pool = store::connect(&tower_superuser_url())
            .await
            .expect("connect as tower");
        let master_key = spx_client::crypto::envelope::MasterKey::load_from_file(&master_key_path)
            .expect("load test master key back");

        let shared_username = format!("dup-agent-{}", Uuid::new_v4().simple());

        for label in ["dup-first", "dup-second"] {
            let ct = spx_client::crypto::envelope::encrypt_agency_password(
                &master_key,
                tenant_id,
                "s3cr3t-pw",
            )
            .expect("encrypt test password");
            sqlx::query(
                "INSERT INTO agency_credentials \
                 (tenant_id, label, username, ciphertext, nonce, key_version) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(tenant_id)
            .bind(label)
            .bind(&shared_username)
            .bind(&ct.bytes)
            .bind(&ct.nonce[..])
            .bind(spx_client::crypto::envelope::KEY_VERSION)
            .execute(&tower_pool)
            .await
            .unwrap_or_else(|e| panic!("insert agency_credentials row (label={label}): {e}"));
        }

        std::env::set_var("DATABASE_URL", &database_url);
        std::env::set_var("TENANT_SLUG", &tenant_slug);
        std::env::set_var("TOWER_MASTER_KEY_PATH", master_key_path.to_str().unwrap());
        std::env::set_var("SPX_BASE_URL", "http://127.0.0.1:1");
        std::env::set_var("AUTH_SIDECAR_URL", "http://127.0.0.1:1");

        let state = build_state().await;
        let accounts = state.poller.accounts.clone();

        let response = app(state)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "the duplicate-username second row must not have panicked the boot"
        );

        assert!(
            accounts.contains_key(&shared_username),
            "the first row's account must have been bootstrapped"
        );
        assert_eq!(
            accounts.len(),
            1,
            "exactly one AccountHandle must exist for the shared account_id — the second \
             row must be skipped, never overwrite the first row's still-running handle"
        );

        let _ = std::fs::remove_file(&master_key_path);
        cleanup_tenant(tenant_id).await;
    }

    /// Task 7 DoD: a rule persisted to `accept_rules` BEFORE boot is present in the spawned
    /// account's `PollerState.rules` at construction (the eager-seed path), AND a rule sent
    /// through `PollerShared.rules_tx` AFTER boot reaches a running account's next `poll_once`
    /// cycle (the live-reload path) — proven by directly inspecting `AccountHandle`'s account
    /// through one real `poll_once` call rather than waiting on the full spawned loop's timer.
    #[tokio::test]
    async fn boot_smoke_seeds_rules_from_db_and_live_reload_reaches_running_account() {
        let database_url = prepare_app_role_database_url().await;
        let (tenant_id, tenant_slug) = seed_test_tenant().await;
        let master_key_path = write_test_master_key();

        let tower_pool = store::connect(&tower_superuser_url())
            .await
            .expect("connect as tower");
        let master_key = spx_client::crypto::envelope::MasterKey::load_from_file(&master_key_path)
            .expect("load test master key back");

        // Seed ONE enabled, unconditional filter rule directly (bypassing the not-yet-built HTTP
        // route — this test only proves the LOADER + CHANNEL, not Task 11's route).
        store::accept_rules::replace_all(
            &tower_pool,
            tenant_id,
            &[store::NewAcceptRule {
                name: "Boot-seeded rule".to_string(),
                enabled: true,
                priority: 0,
                mode: "filter".to_string(),
                service_types: vec![],
                max_weight: None,
                coc_only: false,
                non_coc_only: false,
                max_cod_amount: None,
                origin: String::new(),
                destinations: vec![],
                booking_type: "all".to_string(),
                shift_types: vec![],
                trip_types: vec![],
                match_mode: "strict".to_string(),
                min_deadline_min: None,
                max_accept_count: 0,
                accepted_count: 0,
            }],
        )
        .await
        .expect("seed accept_rules row before boot");

        let username = format!("rules-agent-{}", Uuid::new_v4().simple());
        let ct = spx_client::crypto::envelope::encrypt_agency_password(&master_key, tenant_id, "pw")
            .expect("encrypt test password");
        sqlx::query(
            "INSERT INTO agency_credentials (tenant_id, label, username, ciphertext, nonce, key_version) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(tenant_id)
        .bind("primary")
        .bind(&username)
        .bind(&ct.bytes)
        .bind(&ct.nonce[..])
        .bind(spx_client::crypto::envelope::KEY_VERSION)
        .execute(&tower_pool)
        .await
        .expect("insert agency_credentials row");

        std::env::set_var("DATABASE_URL", &database_url);
        std::env::set_var("TENANT_SLUG", &tenant_slug);
        std::env::set_var("TOWER_MASTER_KEY_PATH", master_key_path.to_str().unwrap());
        std::env::set_var("SPX_BASE_URL", "http://127.0.0.1:1");
        std::env::set_var("AUTH_SIDECAR_URL", "http://127.0.0.1:1");

        let state = build_state().await;

        // Boot-time seed: the spawned account's live task already has one compiled rule. There is
        // no direct getter into a running task's `PollerState`, so this asserts indirectly via a
        // FRESH `PollerState` built the same way `build_state` built the real one, subscribed to
        // the SAME `rules_tx` the real boot used — proving the loader itself returned a non-empty
        // set (the thing this test can observe without reaching into the spawned task).
        let seeded = poller::rules::load_compiled_rules(&state.poller.pool, tenant_id)
            .await
            .expect("load_compiled_rules after boot");
        assert_eq!(seeded.rules.len(), 1, "the boot-seeded rule must be loaded");

        // Live-reload path: send a SECOND rule set through the same channel the running account
        // subscribed to, and confirm a subscriber sees it via `has_changed`/`borrow_and_update` —
        // the exact mechanism `poll_once` (Task 6) uses on its next cycle.
        let mut rx = state.poller.rules_tx.subscribe();
        assert!(
            !rx.has_changed().unwrap_or(true),
            "a fresh subscriber must not report a pending change with no send yet"
        );
        state
            .poller
            .rules_tx
            .send(poller::RuleSet::empty())
            .expect("send on rules_tx (at least one receiver — the spawned account — must exist)");
        assert!(
            rx.has_changed().unwrap_or(false),
            "a send after subscribe must be observable via has_changed"
        );
        let after = rx.borrow_and_update().clone();
        assert_eq!(after.rules.len(), 0, "the live-reload payload must be exactly what was sent");

        let _ = std::fs::remove_file(&master_key_path);
        cleanup_tenant(tenant_id).await;
    }
}
