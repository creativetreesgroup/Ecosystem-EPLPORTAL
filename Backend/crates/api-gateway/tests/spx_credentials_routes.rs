// Backend/crates/api-gateway/tests/spx_credentials_routes.rs
//! Route-level tests for `GET/PUT/DELETE /auth/spx-credentials` (Fase 6b Task
//! 2). Same convention as `tests/auth_routes.rs`: a real `axum::serve`
//! instance + a real HTTP client (`reqwest`) — the router built here is
//! `api_gateway::build_router`, the exact one `reactor-core` mounts, not a
//! hand-rolled test-only router. Real Postgres (127.0.0.1:15432) and real
//! Redis (127.0.0.1:16379), same as `tests/auth_routes.rs`.
use std::sync::Arc;

use api_gateway::AppState;
use dashmap::DashMap;
use serde_json::Value;
use spx_client::crypto::envelope::decrypt_agency_password;
use spx_client::crypto::password::hash_password;
use spx_client::crypto::secret::ExposeSecret;
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

/// The SAME fixed 32-byte master key used to build `AppState.master_key`
/// below AND to independently decrypt the stored ciphertext in Step 5's
/// round-trip assertion — proving the HTTP layer's `encrypt_agency_password`
/// call and this test's own `decrypt_agency_password` call agree on the same
/// key material, not just that "some bytes" got persisted.
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
        .bind("Spx Credentials Routes Test Tenant")
        .bind(format!("spx-credentials-routes-{tenant_id}"))
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

/// Same construction shape as `tests/auth_routes.rs`'s `build_state`. Left
/// idle (no accounts spawned, no notifier, no ws-hub Redis bridge) — these
/// tests only exercise the spx-credentials routes.
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
/// `tests/auth_routes.rs`, since `POST /auth/portal-login` (used here only to
/// mint a session cookie, not under direct test) sits behind
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

/// Logs `username` in via the real `POST /auth/portal-login` route and
/// returns the `Cookie:`-header-ready session pair. Reused by every test
/// below instead of hand-rolling a session row, so each test genuinely
/// exercises `session_auth` end-to-end, same as the auth-route tests do.
async fn login(http: &reqwest::Client, base: &str, username: &str) -> String {
    let resp = http
        .post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({
            "username": username,
            "password": KNOWN_PASSWORD,
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

/// Case 1 + Case 5: `PUT /auth/spx-credentials/agency1` with a valid body ->
/// 200, response body has EXACTLY `{label, username}` keys (no
/// password/ciphertext/nonce anywhere), and (Case 5) the row actually
/// persisted in Postgres decrypts back to the plaintext password that was
/// PUT, using the SAME master key the test server used — proving the
/// encryption round-trip genuinely works end-to-end through the HTTP layer.
#[tokio::test]
async fn put_creates_credential_with_encrypted_password_and_minimal_response_shape() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-put", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-put").await;

    let plaintext_password = "sup3r-s3cret-agency-password!";
    let resp = http
        .put(format!("{base}/auth/spx-credentials/agency1"))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&serde_json::json!({
            "username": "agency1-user",
            "password": plaintext_password,
        }))
        .send()
        .await
        .expect("request PUT /auth/spx-credentials/agency1");

    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let body: Value = resp.json().await.expect("json body");
    let obj = body.as_object().expect("response body is a JSON object");
    let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["label", "username"],
        "PUT response must contain EXACTLY {{label, username}} — no ciphertext/nonce/password \
         field of any kind: {body}"
    );
    assert_eq!(body["label"], "agency1");
    assert_eq!(body["username"], "agency1-user");

    // Belt-and-suspenders: the plaintext password and the word "password"
    // must not appear anywhere in the raw response body text.
    let body_str = serde_json::to_string(&body).unwrap();
    assert!(!body_str.contains(plaintext_password));
    assert!(!body_str.to_lowercase().contains("password"));

    // Case 5: round-trip verification directly against the store + crypto
    // layers, independent of the HTTP response.
    let row = store::agency_credentials::find_by_label(&pool, tenant_id, "agency1")
        .await
        .expect("query agency_credentials")
        .expect("row exists after PUT");
    assert_eq!(row.username, "agency1-user");
    assert_ne!(
        row.ciphertext,
        plaintext_password.as_bytes(),
        "ciphertext must not equal the plaintext password"
    );
    let nonce: [u8; 12] = row
        .nonce
        .as_slice()
        .try_into()
        .expect("stored nonce is exactly 12 bytes");
    let decrypted = decrypt_agency_password(&test_master_key(), tenant_id, &row.ciphertext, &nonce)
        .expect("decrypt stored ciphertext with the same master key the test server used");
    assert_eq!(
        decrypted.expose_secret(),
        plaintext_password,
        "decrypting the stored ciphertext must yield exactly the password that was PUT"
    );

    cleanup(&pool, tenant_id).await;
}

/// Case 2: `GET /` -> the created credential appears, `username` correct,
/// still no password/ciphertext/nonce field anywhere.
#[tokio::test]
async fn get_lists_credentials_without_secrets() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-get", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-get").await;

    let put_resp = http
        .put(format!("{base}/auth/spx-credentials/agency1"))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&serde_json::json!({
            "username": "agency1-user",
            "password": "whatever-password-1",
        }))
        .send()
        .await
        .expect("request PUT");
    assert_eq!(put_resp.status(), reqwest::StatusCode::OK);

    let get_resp = http
        .get(format!("{base}/auth/spx-credentials"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request GET /auth/spx-credentials");
    assert_eq!(get_resp.status(), reqwest::StatusCode::OK);

    let body: Value = get_resp.json().await.expect("json body");
    let list = body.as_array().expect("GET response is a JSON array");
    assert_eq!(list.len(), 1, "exactly one credential was created: {body}");
    let entry = list[0].as_object().expect("entry is a JSON object");
    let mut keys: Vec<&str> = entry.keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["label", "username"],
        "GET list entries must contain EXACTLY {{label, username}}: {body}"
    );
    assert_eq!(entry["label"], "agency1");
    assert_eq!(entry["username"], "agency1-user");

    let body_str = serde_json::to_string(&body).unwrap();
    assert!(!body_str.to_lowercase().contains("password"));

    cleanup(&pool, tenant_id).await;
}

/// Case 3: a SUB-USER (non-main-account) session attempting `PUT`/`DELETE`
/// -> 403 (`require_permission` rejection) — but the SAME sub-user's
/// `GET /` still succeeds (200), confirming the read/write RBAC split.
#[tokio::test]
async fn sub_user_can_read_but_not_write_or_delete() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-rbac", true).await;
    insert_portal_user(&pool, tenant_id, "sub-rbac", false).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();

    // Main account seeds a credential so the sub-user's GET has something to
    // read and so DELETE has a real target to (unsuccessfully) attempt.
    let main_cookie = login(&http, &base, "main-rbac").await;
    let seed_resp = http
        .put(format!("{base}/auth/spx-credentials/agency1"))
        .header(reqwest::header::COOKIE, &main_cookie)
        .json(&serde_json::json!({
            "username": "agency1-user",
            "password": "seed-password",
        }))
        .send()
        .await
        .expect("request PUT (main account seeding)");
    assert_eq!(seed_resp.status(), reqwest::StatusCode::OK);

    let sub_cookie = login(&http, &base, "sub-rbac").await;

    // Sub-user PUT -> 403.
    let put_resp = http
        .put(format!("{base}/auth/spx-credentials/agency2"))
        .header(reqwest::header::COOKIE, &sub_cookie)
        .json(&serde_json::json!({
            "username": "agency2-user",
            "password": "another-password",
        }))
        .send()
        .await
        .expect("request PUT as sub-user");
    assert_eq!(put_resp.status(), reqwest::StatusCode::FORBIDDEN);

    // Sub-user DELETE -> 403.
    let delete_resp = http
        .delete(format!("{base}/auth/spx-credentials/agency1"))
        .header(reqwest::header::COOKIE, &sub_cookie)
        .send()
        .await
        .expect("request DELETE as sub-user");
    assert_eq!(delete_resp.status(), reqwest::StatusCode::FORBIDDEN);

    // Sub-user GET -> 200, and the seeded credential is still there (proof
    // the sub-user's DELETE attempt above was actually rejected, not silently
    // ignored).
    let get_resp = http
        .get(format!("{base}/auth/spx-credentials"))
        .header(reqwest::header::COOKIE, &sub_cookie)
        .send()
        .await
        .expect("request GET as sub-user");
    assert_eq!(get_resp.status(), reqwest::StatusCode::OK);
    let body: Value = get_resp.json().await.expect("json body");
    let list = body.as_array().expect("GET response is a JSON array");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["label"], "agency1");

    cleanup(&pool, tenant_id).await;
}

/// Case 4: `DELETE /agency1` then `GET /` -> the credential is gone; `DELETE`
/// on a nonexistent label -> 404.
#[tokio::test]
async fn delete_removes_credential_and_404s_on_nonexistent_label() {
    let pool = store::connect(&database_url()).await.expect("connect pg");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "main-delete", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login(&http, &base, "main-delete").await;

    let put_resp = http
        .put(format!("{base}/auth/spx-credentials/agency1"))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&serde_json::json!({
            "username": "agency1-user",
            "password": "to-be-deleted-password",
        }))
        .send()
        .await
        .expect("request PUT");
    assert_eq!(put_resp.status(), reqwest::StatusCode::OK);

    let delete_resp = http
        .delete(format!("{base}/auth/spx-credentials/agency1"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request DELETE /auth/spx-credentials/agency1");
    assert_eq!(delete_resp.status(), reqwest::StatusCode::NO_CONTENT);

    let get_resp = http
        .get(format!("{base}/auth/spx-credentials"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request GET after delete");
    assert_eq!(get_resp.status(), reqwest::StatusCode::OK);
    let body: Value = get_resp.json().await.expect("json body");
    let list = body.as_array().expect("GET response is a JSON array");
    assert!(
        list.is_empty(),
        "credential must be gone after DELETE: {body}"
    );

    // DELETE on a label that never existed -> 404.
    let delete_missing_resp = http
        .delete(format!("{base}/auth/spx-credentials/does-not-exist"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("request DELETE on nonexistent label");
    assert_eq!(delete_missing_resp.status(), reqwest::StatusCode::NOT_FOUND);

    cleanup(&pool, tenant_id).await;
}
