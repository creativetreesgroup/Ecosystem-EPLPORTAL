// Backend/crates/api-gateway/tests/portal_users_routes.rs
//! Route-level tests for `GET/POST/DELETE /auth/portal-users` (Fase 6b Task
//! 6). Same convention as `tests/spx_credentials_routes.rs`/`tests/
//! auth_routes.rs`: a real `axum::serve` instance + a real HTTP client
//! (`reqwest`) — the router built here is `api_gateway::build_router`, the
//! exact one `reactor-core` mounts, not a hand-rolled test-only router. Real
//! Postgres (127.0.0.1:15432) and real Redis (127.0.0.1:16379), same as the
//! other route-level test files in this crate.
use std::sync::Arc;

use api_gateway::AppState;
use dashmap::DashMap;
use serde_json::Value;
use spx_client::crypto::password::hash_password;
use spx_client::SpxClient;
use sqlx::PgPool;
use uuid::Uuid;

const SESSION_COOKIE_NAME: &str = "spx_session";
const KNOWN_PASSWORD: &str = "correct horse battery staple 42";

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://tower:tower_dev_only@127.0.0.1:15432/tower".to_string())
}

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:16379".to_string())
}

/// Fixed 32-byte master key for `AppState.master_key` — same construction as
/// `tests/spx_credentials_routes.rs`. These tests never touch envelope
/// encryption, but `AppState` requires the field regardless.
fn test_master_key() -> Arc<spx_client::crypto::envelope::MasterKey> {
    Arc::new(spx_client::crypto::envelope::MasterKey::from_bytes(
        [7u8; 32],
    ))
}

/// Real Redis connection for `AppState.redis` (Task 1's OTP-gate field) —
/// not `Option`, so a real, live `ConnectionManager` is required to
/// construct any `AppState` at all, even though these tests never touch the
/// OTP routes.
async fn test_redis_manager() -> redis::aio::ConnectionManager {
    redis::Client::open(redis_url())
        .expect("open redis client for AppState.redis")
        .get_connection_manager()
        .await
        .expect("connect AppState.redis connection manager")
}

async fn insert_tenant(pool: &PgPool) -> Uuid {
    let tenant_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind("Portal Users Routes Test Tenant")
        .bind(format!("portal-users-routes-{tenant_id}"))
        .execute(pool)
        .await
        .expect("insert tenant");
    tenant_id
}

/// Inserts a portal user with a REAL argon2id hash of `KNOWN_PASSWORD` via
/// `store::portal_users::create` (Task 1) — `is_main_account` is a param so
/// this single helper covers both the main-account and sub-user test cases.
async fn insert_portal_user(
    pool: &PgPool,
    tenant_id: Uuid,
    username: &str,
    is_main_account: bool,
) -> Uuid {
    let hash = hash_password(KNOWN_PASSWORD).expect("hash known password");
    let user = store::portal_users::create(
        pool,
        tenant_id,
        username,
        &hash,
        &format!("Display {username}"),
        is_main_account,
    )
    .await
    .expect("insert portal_user");
    user.id
}

/// Same construction shape as `tests/spx_credentials_routes.rs`'s
/// `build_state`. Left idle (no accounts spawned, no notifier, no ws-hub
/// Redis bridge) — these tests only exercise the portal-users routes (plus
/// `POST /auth/portal-login`, used only to mint session cookies).
async fn build_state(pool: PgPool, tenant_id: Uuid) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url())
        .await
        .expect("connect executor redis");
    let client = SpxClient::new("http://127.0.0.1:1").expect("build SpxClient");
    let sidecar = poller::SidecarClient::new("http://127.0.0.1:1");

    let poller_shared = poller::PollerShared {
        executor: Arc::new(executor),
        client: Arc::new(client),
        pool,
        config: poller::PollerConfig::default(),
        accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar),
        notifier: None,
        redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    };

    AppState {
        poller: Arc::new(poller_shared),
        ws_hub: ws_hub::Hub::new(),
        tenant_id,
        cors_origins: Arc::new(Vec::new()),
        session_cookie_name: Arc::from(SESSION_COOKIE_NAME),
        cookie_secure: true,
        master_key: test_master_key(),
        redis: test_redis_manager().await,
    }
}

/// Spawns a real `axum::serve` instance (the SAME `build_router` as
/// `reactor-core`'s `app()`) on an ephemeral loopback port and returns its
/// base URL. `.into_make_service_with_connect_info::<SocketAddr>()`, same as
/// `tests/spx_credentials_routes.rs`, since `POST /auth/portal-login` (used
/// here to mint session cookies) sits behind
/// `middleware::login_rate_limit_layer`, which needs `ConnectInfo`.
async fn spawn_server(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(
            listener,
            api_gateway::build_router(state)
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    format!("http://{addr}")
}

async fn cleanup(pool: &PgPool, tenant_id: Uuid) {
    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await
        .ok();
}

fn set_cookie_header(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(reqwest::header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().to_string())
}

fn cookie_pair(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .expect("Set-Cookie header has at least one ';'-delimited segment")
        .to_string()
}

/// Logs `username` in via the real `POST /auth/portal-login` route (with an
/// explicit password, so it can log in a freshly-created sub-user whose
/// password isn't `KNOWN_PASSWORD`) and returns the `Cookie:`-header-ready
/// session pair. Reused by every test below instead of hand-rolling a
/// session row, so each test genuinely exercises `session_auth` end-to-end.
async fn login_with(http: &reqwest::Client, base: &str, username: &str, password: &str) -> String {
    let resp = http
        .post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({
            "username": username,
            "password": password,
        }))
        .send()
        .await
        .expect("request portal-login");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "login must succeed to obtain a session cookie for this test"
    );
    cookie_pair(&set_cookie_header(&resp).expect("Set-Cookie present on successful login"))
}

async fn login(http: &reqwest::Client, base: &str, username: &str) -> String {
    login_with(http, base, username, KNOWN_PASSWORD).await
}

/// Case 1: main-account session `POST`s a new sub-user -> `200`, response
/// body has EXACTLY `{id, username, display_name, is_main_account, enabled}`
/// (no `password_hash`/`password` field anywhere), AND the created user can
/// then actually LOG IN with the submitted password via the real
/// `POST /auth/portal-login` route — proving `hash_password` at creation time
/// and `verify_password` at login time genuinely agree end-to-end through the
/// HTTP layer, not just that "some hash" got persisted.
#[tokio::test]
async fn create_never_leaks_hash_and_new_user_can_really_log_in() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-create", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-create").await;

    let new_password = "sub-user-password-1";
    let resp = http
        .post(format!("{base}/auth/portal-users"))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&serde_json::json!({
            "username": "sub-created",
            "password": new_password,
            "display_name": "Sub Created",
            "is_main_account": false,
        }))
        .send()
        .await
        .expect("request POST /auth/portal-users");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: Value = resp.json().await.expect("json body");
    let obj = body.as_object().expect("response body is a JSON object");
    let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["display_name", "enabled", "id", "is_main_account", "username"],
        "POST response must contain EXACTLY {{id, username, display_name, is_main_account, \
         enabled}} — no password/password_hash field of any kind: {body}"
    );
    assert_eq!(body["username"], "sub-created");
    assert_eq!(body["display_name"], "Sub Created");
    assert_eq!(body["is_main_account"], false);
    assert_eq!(body["enabled"], true);

    // Belt-and-suspenders: neither the plaintext password nor the word
    // "hash" appears anywhere in the raw response body text.
    let body_str = serde_json::to_string(&body).unwrap();
    assert!(!body_str.contains(new_password));
    assert!(!body_str.to_lowercase().contains("hash"));

    // Real end-to-end round trip: the freshly-created sub-user can actually
    // log in with the password that was POSTed.
    let sub_cookie = login_with(&http, &base, "sub-created", new_password).await;
    assert!(!sub_cookie.is_empty());

    cleanup(&pool, tenant_id).await;
}

/// Case 2: `GET /` lists BOTH the pre-existing main account and the
/// newly-created sub-user, and no entry carries a `password_hash` field.
#[tokio::test]
async fn list_shows_both_main_account_and_new_sub_user() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-list", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-list").await;

    let create_resp = http
        .post(format!("{base}/auth/portal-users"))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&serde_json::json!({
            "username": "sub-list",
            "password": "sub-list-password",
            "display_name": "Sub List",
            "is_main_account": false,
        }))
        .send()
        .await
        .expect("request POST");
    assert_eq!(create_resp.status(), reqwest::StatusCode::OK);

    let get_resp = http
        .get(format!("{base}/auth/portal-users"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request GET /auth/portal-users");
    assert_eq!(get_resp.status(), reqwest::StatusCode::OK);

    let body: Value = get_resp.json().await.expect("json body");
    let list = body.as_array().expect("GET response is a JSON array");
    assert_eq!(
        list.len(),
        2,
        "both the main account and the new sub-user must be listed: {body}"
    );
    let usernames: Vec<&str> = list
        .iter()
        .map(|v| v["username"].as_str().expect("username is a string"))
        .collect();
    assert!(usernames.contains(&"main-list"));
    assert!(usernames.contains(&"sub-list"));

    let body_str = serde_json::to_string(&body).unwrap();
    assert!(!body_str.to_lowercase().contains("hash"));

    cleanup(&pool, tenant_id).await;
}

/// Case 3: a SUB-USER (non-main-account) session attempting `POST`/`DELETE`
/// -> 403 (`require_permission` rejection) — but the SAME sub-user's
/// `GET /` still succeeds (200), confirming the read/write RBAC split.
#[tokio::test]
async fn sub_user_gets_403_on_write_but_200_on_read() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-rbac", true).await;
    let sub_id = insert_portal_user(&pool, tenant_id, "sub-rbac", false).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let sub_cookie = login(&http, &base, "sub-rbac").await;

    // Sub-user POST -> 403.
    let post_resp = http
        .post(format!("{base}/auth/portal-users"))
        .header(reqwest::header::COOKIE, &sub_cookie)
        .json(&serde_json::json!({
            "username": "should-not-be-created",
            "password": "irrelevant-password",
            "display_name": "Nope",
            "is_main_account": false,
        }))
        .send()
        .await
        .expect("request POST as sub-user");
    assert_eq!(post_resp.status(), reqwest::StatusCode::FORBIDDEN);

    // Sub-user DELETE (even targeting someone else's id) -> 403.
    let delete_resp = http
        .delete(format!("{base}/auth/portal-users/{sub_id}"))
        .header(reqwest::header::COOKIE, &sub_cookie)
        .send()
        .await
        .expect("request DELETE as sub-user");
    assert_eq!(delete_resp.status(), reqwest::StatusCode::FORBIDDEN);

    // Sub-user GET -> 200, and only the original two accounts are listed
    // (proof the sub-user's POST above was actually rejected, not silently
    // applied).
    let get_resp = http
        .get(format!("{base}/auth/portal-users"))
        .header(reqwest::header::COOKIE, &sub_cookie)
        .send()
        .await
        .expect("request GET as sub-user");
    assert_eq!(get_resp.status(), reqwest::StatusCode::OK);
    let body: Value = get_resp.json().await.expect("json body");
    let list = body.as_array().expect("GET response is a JSON array");
    assert_eq!(list.len(), 2, "no new account was created: {body}");

    cleanup(&pool, tenant_id).await;
}

/// Case 4: a SUB-USER (non-main-account) session `POST`s a body that
/// explicitly sets `"is_main_account": true` — the single most
/// security-critical escalation path (a sub-user attempting to mint a
/// BRAND-NEW main-account user for themselves) — is rejected `403`, AND a
/// subsequent `GET /` proves no row was actually inserted (the user list
/// is unchanged from immediately before the rejected POST). This is
/// distinct from Case 3's generic RBAC check: Case 3's rejected POST body
/// sets `is_main_account: false`, so it never actually exercises the
/// escalation path itself — only this case does. Guards against a future
/// regression that reordered body-handling ahead of the
/// `require_permission` check in the `create` handler.
#[tokio::test]
async fn sub_user_cannot_escalate_by_creating_a_main_account_user() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-escalation", true).await;
    insert_portal_user(&pool, tenant_id, "sub-escalation", false).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let sub_cookie = login(&http, &base, "sub-escalation").await;

    // Baseline: capture the user list BEFORE the escalation attempt.
    let before_resp = http
        .get(format!("{base}/auth/portal-users"))
        .header(reqwest::header::COOKIE, &sub_cookie)
        .send()
        .await
        .expect("request GET before escalation attempt");
    assert_eq!(before_resp.status(), reqwest::StatusCode::OK);
    let before_body: Value = before_resp.json().await.expect("json body");
    let before_list = before_body.as_array().expect("GET response is a JSON array");
    assert_eq!(
        before_list.len(),
        2,
        "sanity check: only the seeded main+sub accounts exist yet: {before_body}"
    );
    let mut before_usernames: Vec<&str> = before_list
        .iter()
        .map(|v| v["username"].as_str().expect("username is a string"))
        .collect();
    before_usernames.sort_unstable();

    // The escalation attempt itself: sub-user POSTs a body that explicitly
    // sets `is_main_account: true`.
    let post_resp = http
        .post(format!("{base}/auth/portal-users"))
        .header(reqwest::header::COOKIE, &sub_cookie)
        .json(&serde_json::json!({
            "username": "escalated-main",
            "password": "escalation-attempt-password",
            "display_name": "Escalated Main",
            "is_main_account": true,
        }))
        .send()
        .await
        .expect("request POST with is_main_account: true as sub-user");
    assert_eq!(
        post_resp.status(),
        reqwest::StatusCode::FORBIDDEN,
        "a sub-user must never be able to create ANY user, least of all a main-account one"
    );

    // Prove the rejection was real, not cosmetic: the list is unchanged
    // from immediately before the POST — no `escalated-main` row exists.
    let after_resp = http
        .get(format!("{base}/auth/portal-users"))
        .header(reqwest::header::COOKIE, &sub_cookie)
        .send()
        .await
        .expect("request GET after rejected escalation attempt");
    assert_eq!(after_resp.status(), reqwest::StatusCode::OK);
    let after_body: Value = after_resp.json().await.expect("json body");
    let after_list = after_body.as_array().expect("GET response is a JSON array");
    let mut after_usernames: Vec<&str> = after_list
        .iter()
        .map(|v| v["username"].as_str().expect("username is a string"))
        .collect();
    after_usernames.sort_unstable();

    assert_eq!(
        after_usernames, before_usernames,
        "user list must be unchanged after the rejected escalation POST — no row was inserted: \
         {after_body}"
    );
    assert!(
        !after_usernames.contains(&"escalated-main"),
        "the sub-user's attempted main-account user must NOT have been created: {after_body}"
    );

    cleanup(&pool, tenant_id).await;
}

/// Case 5: `DELETE /:id` for the sub-user -> 204, then `GET /` no longer
/// lists them; `DELETE` on an already-deleted (nonexistent) id -> 404.
#[tokio::test]
async fn delete_removes_sub_user_and_they_are_gone_from_list() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-delete", true).await;
    let sub_id = insert_portal_user(&pool, tenant_id, "sub-delete", false).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-delete").await;

    let delete_resp = http
        .delete(format!("{base}/auth/portal-users/{sub_id}"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request DELETE /auth/portal-users/:id");
    assert_eq!(delete_resp.status(), reqwest::StatusCode::NO_CONTENT);

    let get_resp = http
        .get(format!("{base}/auth/portal-users"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request GET after delete");
    assert_eq!(get_resp.status(), reqwest::StatusCode::OK);
    let body: Value = get_resp.json().await.expect("json body");
    let list = body.as_array().expect("GET response is a JSON array");
    assert_eq!(list.len(), 1, "only the main account remains: {body}");
    assert_eq!(list[0]["username"], "main-delete");

    // DELETE again on the now-gone id -> 404.
    let delete_missing_resp = http
        .delete(format!("{base}/auth/portal-users/{sub_id}"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request DELETE on already-deleted id");
    assert_eq!(delete_missing_resp.status(), reqwest::StatusCode::NOT_FOUND);

    cleanup(&pool, tenant_id).await;
}

/// Case 6: a main-account user attempting to `DELETE` THEIR OWN `id` -> 400
/// (self-lockout guard), and the account is still present afterward (login
/// still works).
#[tokio::test]
async fn main_account_cannot_delete_their_own_id() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    let main_id = insert_portal_user(&pool, tenant_id, "main-self-lockout", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-self-lockout").await;

    let delete_resp = http
        .delete(format!("{base}/auth/portal-users/{main_id}"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request DELETE on own id");
    assert_eq!(delete_resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // Still there — the account can still log in.
    let still_cookie = login(&http, &base, "main-self-lockout").await;
    assert!(!still_cookie.is_empty());

    let get_resp = http
        .get(format!("{base}/auth/portal-users"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request GET after rejected self-delete");
    let body: Value = get_resp.json().await.expect("json body");
    let list = body.as_array().expect("GET response is a JSON array");
    assert_eq!(list.len(), 1, "self-lockout guard must have prevented deletion: {body}");

    cleanup(&pool, tenant_id).await;
}
