// Backend/crates/api-gateway/tests/quick_accept_routes.rs
//! `GET/POST /q/:token` — the HMAC quick-accept flow, reachable with NO session cookie at all.
use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
use uuid::Uuid;

use api_gateway::AppState;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}
fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}
fn test_master_key() -> Arc<spx_client::crypto::envelope::MasterKey> {
    Arc::new(spx_client::crypto::envelope::MasterKey::from_bytes([7u8; 32]))
}
async fn test_redis_manager() -> redis::aio::ConnectionManager {
    redis::Client::open(redis_url())
        .expect("open redis client")
        .get_connection_manager()
        .await
        .expect("connect redis")
}
async fn insert_tenant(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(id)
        .bind("Quick Accept Test Tenant")
        .bind(format!("qa-test-{id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    id
}
async fn insert_booking(pool: &sqlx::PgPool, tenant_id: Uuid, spx_id: &str, status: &str) {
    sqlx::query(
        "INSERT INTO bookings (tenant_id, account_id, spx_id, status, raw_data) \
         VALUES ($1, 'acct-qa', $2, $3, '{}'::jsonb)",
    )
    .bind(tenant_id)
    .bind(spx_id)
    .bind(status)
    .execute(pool)
    .await
    .expect("insert booking");
}
async fn build_state(pool: sqlx::PgPool, tenant_id: Uuid) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect executor redis");
    let client = spx_client::SpxClient::new("http://127.0.0.1:1").expect("build SpxClient");
    let sidecar = poller::SidecarClient::new("http://127.0.0.1:1".to_string());
    let poller_shared = Arc::new(poller::PollerShared {
        executor: Arc::new(executor),
        client: Arc::new(client),
        pool: pool.clone(),
        config: poller::PollerConfig::default(),
        accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar),
        notifier: None,
        redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });
    AppState {
        poller: poller_shared,
        ws_hub: ws_hub::Hub::new(),
        tenant_id,
        cors_origins: Arc::new(vec![]),
        session_cookie_name: Arc::from("spx_session"),
        cookie_secure: false,
        master_key: test_master_key(),
        redis: test_redis_manager().await,
    }
}
async fn spawn_server(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = api_gateway::build_router(state).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}
async fn cleanup(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await;
}

#[tokio::test]
async fn get_quick_token_is_reachable_with_no_session_at_all() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "SPX-QA-1", "pending").await;

    let state = build_state(pool.clone(), tenant_id).await;
    let master_key = state.master_key.clone();
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let now = chrono::Utc::now().timestamp_millis();
    let token = spx_client::crypto::quick_token::sign_quick_token(
        &master_key,
        tenant_id,
        "SPX-QA-1",
        spx_client::crypto::quick_token::DEFAULT_TTL_MS,
        now,
    )
    .unwrap();

    let resp = http.get(format!("{base}/q/{token}")).send().await.unwrap();
    assert_eq!(resp.status(), 200, "must be reachable with zero Cookie header");
    let body = resp.text().await.unwrap();
    assert!(body.contains("SPX-QA-1"));

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn expired_token_returns_410() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let master_key = state.master_key.clone();
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let long_ago = chrono::Utc::now().timestamp_millis() - 999_999_999;
    let token = spx_client::crypto::quick_token::sign_quick_token(
        &master_key,
        tenant_id,
        "SPX-QA-2",
        1,
        long_ago,
    )
    .unwrap();

    let resp = http.get(format!("{base}/q/{token}")).send().await.unwrap();
    assert_eq!(resp.status(), 410);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn malformed_token_returns_400() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http.get(format!("{base}/q/a")).send().await.unwrap(); // too short
    assert_eq!(resp.status(), 400);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn post_quick_accept_on_a_nonexistent_booking_returns_404() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let master_key = state.master_key.clone();
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let now = chrono::Utc::now().timestamp_millis();
    let token = spx_client::crypto::quick_token::sign_quick_token(
        &master_key,
        tenant_id,
        "SPX-DOES-NOT-EXIST",
        spx_client::crypto::quick_token::DEFAULT_TTL_MS,
        now,
    )
    .unwrap();

    let resp = http
        .post(format!("{base}/q/accept"))
        .json(&serde_json::json!({"token": token}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn wrong_tenant_token_is_rejected_by_this_deployments_state_tenant_id() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let master_key = state.master_key.clone();
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    // Signed for a DIFFERENT tenant than this deployment's own state.tenant_id.
    let other_tenant = Uuid::new_v4();
    let now = chrono::Utc::now().timestamp_millis();
    let token = spx_client::crypto::quick_token::sign_quick_token(
        &master_key,
        other_tenant,
        "SPX-QA-3",
        spx_client::crypto::quick_token::DEFAULT_TTL_MS,
        now,
    )
    .unwrap();

    let resp = http.get(format!("{base}/q/{token}")).send().await.unwrap();
    assert_eq!(
        resp.status(),
        410,
        "a token signed for a different tenant must verify as invalid here"
    );

    cleanup(&pool, tenant_id).await;
}

/// Proves Task 4's CSP-relaxation fix actually took effect: the confirmation page's response
/// must carry a `Content-Security-Policy` with a `script-src` token. The OLD global strict
/// default (`default-src 'none'; frame-ancestors 'none'; base-uri 'none'`) has NO `script-src`
/// token at all — a real browser would block the page's inline `<script>` and its `fetch()`
/// under that header, which is exactly the bug this fix addresses. This test would have failed
/// before Step 1/2's change and must pass after it.
#[tokio::test]
async fn quick_token_page_csp_allows_its_own_inline_script() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "SPX-QA-CSP", "pending").await;

    let state = build_state(pool.clone(), tenant_id).await;
    let master_key = state.master_key.clone();
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let now = chrono::Utc::now().timestamp_millis();
    let token = spx_client::crypto::quick_token::sign_quick_token(
        &master_key,
        tenant_id,
        "SPX-QA-CSP",
        spx_client::crypto::quick_token::DEFAULT_TTL_MS,
        now,
    )
    .unwrap();

    let resp = http.get(format!("{base}/q/{token}")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let csp = resp
        .headers()
        .get("content-security-policy")
        .expect("confirmation page must set a Content-Security-Policy header")
        .to_str()
        .unwrap();
    assert!(
        csp.contains("script-src"),
        "confirmation page CSP must contain a script-src token allowing its own inline \
         <script> to run, got: {csp}"
    );

    cleanup(&pool, tenant_id).await;
}

/// Proves Task 4's HTML-escaping fix: a `spx_id` containing an HTML metacharacter must be
/// rendered escaped, never raw, in the confirmation page's markup. `spx_id` is unconstrained
/// TEXT in the DB (this INSERT is legal even though `is_valid_token_shape` would reject this
/// exact string as a URL path segment) — the point is proving the RENDERING is safe for
/// whatever value ends up in the DB, so the token here is signed directly for this exact
/// `spx_id` value via the crypto layer, independent of `is_valid_token_shape`'s URL-shape check.
#[tokio::test]
async fn quick_token_page_html_escapes_spx_id() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let spx_id = "SPX<script>ALERT</script>";
    insert_booking(&pool, tenant_id, spx_id, "pending").await;

    let state = build_state(pool.clone(), tenant_id).await;
    let master_key = state.master_key.clone();
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let now = chrono::Utc::now().timestamp_millis();
    let token = spx_client::crypto::quick_token::sign_quick_token(
        &master_key,
        tenant_id,
        spx_id,
        spx_client::crypto::quick_token::DEFAULT_TTL_MS,
        now,
    )
    .unwrap();

    let resp = http.get(format!("{base}/q/{token}")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("&lt;script&gt;"),
        "spx_id's HTML metacharacters must be rendered escaped"
    );
    assert!(
        !body.contains("<script>ALERT</script>"),
        "the raw, unescaped spx_id must never appear in the response body"
    );

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn short_code_flow_round_trips_and_is_single_use() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    insert_booking(&pool, tenant_id, "SPX-QA-CODE-1", "pending").await;

    let state = build_state(pool.clone(), tenant_id).await;
    let mut redis_conn = test_redis_manager().await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let code = "testCode123";
    let _: () = redis::AsyncCommands::set_ex(
        &mut redis_conn,
        format!("spx:qa:{code}"),
        r#"{"b":"SPX-QA-CODE-1"}"#,
        1800,
    )
    .await
    .unwrap();

    let get_resp = http.get(format!("{base}/accept/{code}")).send().await.unwrap();
    assert_eq!(get_resp.status(), 200);
    let body = get_resp.text().await.unwrap();
    assert!(body.contains("SPX-QA-CODE-1"));

    // Account not connected in this test harness -> accept fails with a real, non-2xx status,
    // proving the route genuinely reached execute_manual_accept (not a silent no-op).
    let post_resp = http.post(format!("{base}/accept/{code}")).send().await.unwrap();
    assert_eq!(post_resp.status(), 409);
    let post_body: serde_json::Value = post_resp.json().await.unwrap();
    assert_eq!(post_body["ok"], false);
    assert_eq!(post_body["reason"], "account_offline");

    // Failure path must NOT delete the code (only a successful accept does) — confirm it's
    // still readable.
    let still_there: Option<String> =
        redis::AsyncCommands::get(&mut redis_conn, format!("spx:qa:{code}")).await.unwrap();
    assert!(still_there.is_some(), "a failed accept attempt must not consume the code");

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn expired_short_code_returns_410() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    let resp = http.get(format!("{base}/accept/never-existed-code")).send().await.unwrap();
    assert_eq!(resp.status(), 410);

    cleanup(&pool, tenant_id).await;
}

/// Proves Task 6's two rate-limit budgets are BOTH correctly scoped to their own HTTP method on
/// `short_code_router`'s shared `/{code}` path — not the same layer applied to both methods, and
/// not accidentally swapped (the stricter 12/min action budget leaking onto GET, or the lenient
/// 60/min view budget leaking onto POST). `GET` and `POST` hit the exact same URL
/// (`/accept/rl-test-code`) from the exact same client IP, so the only thing that can explain a
/// difference in outcome is the two independently-`route_layer`'d sub-routers
/// `short_code_router::view`/`::action` actually carrying different `GovernorLayer` instances, as
/// documented in that fn's own doc comment.
#[tokio::test]
async fn action_rate_limit_is_stricter_than_view_rate_limit() {
    let pool = store::connect(&database_url()).await.expect("connect");
    let tenant_id = insert_tenant(&pool).await;
    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    // 13 rapid GETs to /accept/:code (view-limited, 60/min burst) must ALL clear the rate
    // limiter — if the stricter 12/min action budget had leaked onto GET instead, this loop
    // would see a 429 well before request 13 and fail here.
    for i in 0..13 {
        let resp = http.get(format!("{base}/accept/rl-test-code")).send().await.unwrap();
        assert_ne!(
            resp.status(),
            429,
            "GET #{i} must not be rate-limited yet under the 60/min view budget"
        );
    }

    // 13 rapid POSTs to the SAME path (action-limited, 12/min burst) — an invalid code doesn't
    // matter, the rate limiter fires before the handler's own 410 logic — must trip 429 before
    // the 13th request. If the lenient 60/min view budget had leaked onto POST instead (or both
    // methods shared one layer), all 13 would clear and this assertion would fail.
    let mut saw_429 = false;
    for _ in 0..13 {
        let resp = http.post(format!("{base}/accept/rl-test-code")).send().await.unwrap();
        if resp.status() == 429 {
            saw_429 = true;
            break;
        }
    }
    assert!(saw_429, "the 12/min action limiter must eventually reject rapid POSTs");

    cleanup(&pool, tenant_id).await;
}
