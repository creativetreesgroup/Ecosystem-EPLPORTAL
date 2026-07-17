// Backend/crates/api-gateway/tests/bot_routes.rs
//! `GET/PUT /bot/settings` — `ManageBotSettings`-gated on BOTH verbs.
use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use api_gateway::AppState;
use spx_client::waha_settings::{WahaSettings, SITE_SETTINGS_KEY};

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
    redis::Client::open(redis_url()).expect("open redis client").get_connection_manager().await.expect("connect redis")
}
async fn insert_tenant(pool: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(id).bind("Bot Test Tenant").bind(format!("bot-test-{id}"))
        .execute(pool).await.expect("insert tenant");
    id
}
async fn insert_portal_user(pool: &sqlx::PgPool, tenant_id: Uuid, username: &str, is_main: bool) -> Uuid {
    let hash = spx_client::crypto::password::hash_password("pw12345678").expect("hash password");
    store::portal_users::create(pool, tenant_id, username, &hash, "Test User", is_main)
        .await.expect("create portal user").id
}
async fn build_state(pool: sqlx::PgPool, tenant_id: Uuid) -> AppState {
    let executor = executor::ExecutorHandle::connect(&redis_url()).await.expect("connect executor redis");
    let client = spx_client::SpxClient::new("http://127.0.0.1:1").expect("build SpxClient");
    let sidecar = poller::SidecarClient::new("http://127.0.0.1:1".to_string());
    let poller_shared = Arc::new(poller::PollerShared {
        executor: Arc::new(executor), client: Arc::new(client), pool: pool.clone(),
        config: poller::PollerConfig::default(), accounts: Arc::new(DashMap::new()),
        sidecar: Arc::new(sidecar), notifier: None, redis: None,
        rules_tx: tokio::sync::watch::channel(poller::RuleSet::empty()).0,
    });
    AppState {
        poller: poller_shared, ws_hub: ws_hub::Hub::new(), tenant_id,
        cors_origins: Arc::new(vec![]), session_cookie_name: Arc::from("spx_session"),
        cookie_secure: false, master_key: test_master_key(), redis: test_redis_manager().await,
    }
}
async fn spawn_server(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = api_gateway::build_router(state).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    format!("http://{addr}")
}
async fn login_cookie(http: &reqwest::Client, base: &str, username: &str) -> String {
    let resp = http.post(format!("{base}/auth/portal-login"))
        .json(&serde_json::json!({"username": username, "password": "pw12345678"}))
        .send().await.expect("login request");
    assert_eq!(resp.status(), 200);
    resp.headers().get_all("set-cookie").iter().find_map(|v| v.to_str().ok())
        .and_then(|s| s.split(';').next()).map(|s| s.to_string())
        .expect("session cookie must be set")
}
async fn cleanup(pool: &sqlx::PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tenants WHERE id = $1").bind(tenant_id).execute(pool).await;
}

#[tokio::test]
async fn sub_user_is_forbidden_on_both_get_and_put() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;
    insert_portal_user(&pool, tenant_id, "helper", false).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let helper_cookie = login_cookie(&http, &base, "helper").await;

    let get_resp = http.get(format!("{base}/bot/settings")).header("Cookie", &helper_cookie).send().await.unwrap();
    assert_eq!(get_resp.status(), 403, "GET must also be main-account-gated");

    let put_resp = http.put(format!("{base}/bot/settings")).header("Cookie", &helper_cookie)
        .json(&serde_json::json!({"waha_api_key": "k"})).send().await.unwrap();
    assert_eq!(put_resp.status(), 403);

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn put_then_get_never_echoes_the_api_key_and_blank_key_preserves_previous() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let first_put = http.put(format!("{base}/bot/settings")).header("Cookie", &cookie)
        .json(&serde_json::json!({
            "enabled": true, "wa_number": "6281234567890", "waha_url": "http://waha.example.com:3000",
            "waha_session": "default", "waha_api_key": "secret-key-1"
        }))
        .send().await.unwrap();
    assert_eq!(first_put.status(), 200);
    let first_body: serde_json::Value = first_put.json().await.unwrap();
    assert_eq!(first_body["waha_api_key_set"], true);
    assert!(
        first_body.get("waha_api_key").is_none() || first_body["waha_api_key"] == serde_json::Value::Null,
        "the API key must never be echoed back in any form"
    );
    assert!(!first_body.to_string().contains("secret-key-1"), "the plaintext key must not appear anywhere in the response");

    // Second PUT with a BLANK api key — must keep the previously configured key, not wipe it.
    let second_put = http.put(format!("{base}/bot/settings")).header("Cookie", &cookie)
        .json(&serde_json::json!({
            "enabled": false, "wa_number": "6289999999999", "waha_url": "http://waha.example.com:3000",
            "waha_session": "default", "waha_api_key": ""
        }))
        .send().await.unwrap();
    assert_eq!(second_put.status(), 200);
    let second_body: serde_json::Value = second_put.json().await.unwrap();
    assert_eq!(second_body["waha_api_key_set"], true, "a blank api_key on PUT must preserve the previously configured key");
    assert_eq!(second_body["wa_number"], "6289999999999", "non-key fields must still update");
    assert_eq!(second_body["enabled"], false);

    let get_resp = http.get(format!("{base}/bot/settings")).header("Cookie", &cookie).send().await.unwrap();
    let get_body: serde_json::Value = get_resp.json().await.unwrap();
    assert_eq!(get_body["waha_api_key_set"], true);

    cleanup(&pool, tenant_id).await;
}

/// Minor review finding regression: `portal_label` is a `WahaSettings` field with no write path
/// through `BotSettingsRequest`/`BotSettingsResponse` today (neither exposes it — see this test's
/// own DB-level assertions below, and the report this whole-branch-review fix produced), so it can
/// only be non-empty via a row that predates/bypasses this route (a future feature, a migration, or
/// — as here — direct seeding). `put_settings`'s key-rotation (`else`) branch used to rebuild the
/// row via `WahaSettings::encrypt_new`, which always sets `portal_label: String::new()` — silently
/// wiping whatever the previous row had, unlike the blank-key branch (which reuses `existing`
/// wholesale and so preserves it for free). This test seeds a row with a non-empty `portal_label`
/// directly (bypassing the route, since there's no request field for it), then PUTs a body that
/// rotates the API key (non-empty `waha_api_key`, a different value) without mentioning
/// `portal_label` at all, then reads the stored row directly back out of Postgres (NOT the GET
/// response — `BotSettingsResponse` doesn't carry `portal_label`, so an HTTP-level assertion isn't
/// possible here) to confirm it survived the rotation instead of being reset to `""`.
#[tokio::test]
async fn key_rotation_preserves_portal_label_in_stored_row() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let mut seeded = WahaSettings::encrypt_new(
        &test_master_key(),
        tenant_id,
        "https://waha.example.com:3000",
        "default",
        "original-key",
    )
    .expect("encrypt seeded waha settings");
    seeded.portal_label = "Existing Label".to_string();
    store::site_settings::put(&pool, tenant_id, SITE_SETTINGS_KEY, &seeded.to_json_value())
        .await
        .expect("seed site_settings row with a portal_label already set");

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    // Rotates the API key (non-empty, different value) — does NOT and CANNOT mention
    // `portal_label`, since `BotSettingsRequest` has no field for it.
    let rotate_resp = http
        .put(format!("{base}/bot/settings"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({
            "enabled": true, "wa_number": "6281234567890", "waha_url": "https://waha.example.com:3000",
            "waha_session": "default", "waha_api_key": "rotated-key-2"
        }))
        .send().await.unwrap();
    assert_eq!(rotate_resp.status(), 200);

    let stored = store::site_settings::get(&pool, tenant_id, SITE_SETTINGS_KEY)
        .await
        .expect("read back site_settings row")
        .expect("row must exist after PUT");
    let waha = WahaSettings::from_json_value(&stored).expect("decode stored WahaSettings");
    assert_eq!(
        waha.portal_label, "Existing Label",
        "a key rotation must NOT wipe an already-set portal_label"
    );
    // Confirm the rotation genuinely happened (sanity check this isn't a no-op PUT).
    assert_ne!(
        waha.api_key_ciphertext_b64, seeded.api_key_ciphertext_b64,
        "the API key ciphertext must actually change on rotation"
    );

    cleanup(&pool, tenant_id).await;
}

#[tokio::test]
async fn ssrf_guard_rejects_internal_hosts() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    for bad_url in ["http://localhost:3000", "http://127.0.0.1:3000", "http://192.168.1.5:3000", "http://10.0.0.1"] {
        let resp = http.put(format!("{base}/bot/settings")).header("Cookie", &cookie)
            .json(&serde_json::json!({"waha_url": bad_url, "waha_api_key": "k"}))
            .send().await.unwrap();
        assert_eq!(resp.status(), 400, "waha_url={bad_url} must be rejected");
    }

    cleanup(&pool, tenant_id).await;
}

/// Security-review finding, fixed post-Task-6: the original hand-rolled string-parsing SSRF
/// guard had two real bypasses — userinfo confusion (a real HTTP client connects to whatever
/// follows `@`, ignoring the "safe-looking" text before it) and IPv6 bracket notation (the naive
/// split-on-first-`:` truncated inside `[::1]`, never matching the literal `"::1"` check). Both
/// are now rejected via a real `url::Url` parse + `Host` enum inspection — this test locks that in.
#[tokio::test]
async fn ssrf_guard_rejects_userinfo_confusion_and_ipv6_bracket_notation() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    let state = build_state(pool.clone(), tenant_id).await;
    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let bad_urls = [
        // Userinfo confusion: a real client connects to 169.254.169.254 (the cloud metadata
        // endpoint), ignoring "looks-safe.example" before the '@'.
        "http://looks-safe.example@169.254.169.254/",
        "http://looks-safe.example@127.0.0.1/",
        // IPv6 loopback/link-local/unique-local in bracket notation.
        "http://[::1]:3000",
        "http://[fe80::1]:3000",
        "http://[fc00::1]:3000",
        // IPv4-mapped IPv6 loopback.
        "http://[::ffff:127.0.0.1]:3000",
        // Cloud-metadata DNS names (whole-branch review finding): these resolve to the same
        // 169.254.169.254 IP-literal endpoint already blocked above, on GCP/AWS/Azure.
        "http://metadata.google.internal/",
        "http://metadata.goog/",
        "http://foo.internal/",
    ];
    for bad_url in bad_urls {
        let resp = http
            .put(format!("{base}/bot/settings"))
            .header("Cookie", &cookie)
            .json(&serde_json::json!({"waha_url": bad_url, "waha_api_key": "k"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "waha_url={bad_url} must be rejected");
    }

    // A genuinely external, safe URL must still be accepted (the guard isn't over-broad).
    let good_resp = http
        .put(format!("{base}/bot/settings"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({"waha_url": "https://waha.example.com:3000", "waha_api_key": "k"}))
        .send()
        .await
        .unwrap();
    assert_eq!(good_resp.status(), 200, "a genuinely external host must still be accepted");

    cleanup(&pool, tenant_id).await;
}

/// Minor review finding: this test's name previously claimed it proves an OTP send records a
/// `bot_log` entry, but it never actually called `/auth/request-aa-otp` — it only asserted an
/// empty `GET`, then a `204` on `DELETE`. `tests/otp_routes.rs` already has reusable WAHA-wiremock
/// scaffolding (a mock WAHA server + `WahaSettings::encrypt_new`-seeded `site_settings` row) that
/// adapts here with a moderate amount of setup, so this test now genuinely drives
/// `POST /auth/request-aa-otp` (which itself calls `notifier::bot_log::record` — see
/// `routes/otp.rs`) and asserts the resulting `GET /bot/logs` entry has `kind == "otp"`, making the
/// test's name accurate rather than renaming it to describe less than it could cover.
#[tokio::test]
async fn bot_logs_records_from_otp_and_can_be_cleared() {
    let pool = store::connect(&database_url()).await.expect("connect");
    store::run_migrations(&pool).await.expect("migrate");
    let tenant_id = insert_tenant(&pool).await;
    insert_portal_user(&pool, tenant_id, "owner", true).await;

    // Seed a WAHA-configured `site_settings` row directly (bypassing `PUT /bot/settings`'s SSRF
    // guard, which would otherwise reject wiremock's own loopback URI — same reason
    // `otp_routes.rs::seed_waha_settings` seeds directly instead of going through the route).
    let waha_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/sendText"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&waha_server)
        .await;
    let mut waha = WahaSettings::encrypt_new(
        &test_master_key(),
        tenant_id,
        &waha_server.uri(),
        "default",
        "waha-test-api-key",
    )
    .expect("encrypt seeded waha settings");
    waha.wa_number = "6281234567890".to_string();
    store::site_settings::put(&pool, tenant_id, SITE_SETTINGS_KEY, &waha.to_json_value())
        .await
        .expect("seed site_settings row");

    let state = build_state(pool.clone(), tenant_id).await;
    let mut redis_check = test_redis_manager().await;
    notifier::bot_log::clear(&mut redis_check, tenant_id).await;

    let base = spawn_server(state).await;
    let http = reqwest::Client::new();
    let cookie = login_cookie(&http, &base, "owner").await;

    let get_resp = http.get(format!("{base}/bot/logs")).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(get_resp.status(), 200);
    let body: Vec<serde_json::Value> = get_resp.json().await.unwrap();
    assert_eq!(body.len(), 0, "no entries recorded yet in this test's own clean Redis state");

    // Genuinely drive the OTP path — this is what should actually record the bot_log entry.
    let otp_resp = http.post(format!("{base}/auth/request-aa-otp")).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(otp_resp.status(), 200, "request-aa-otp must succeed against the seeded WAHA mock");

    let get_after_otp = http.get(format!("{base}/bot/logs")).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(get_after_otp.status(), 200);
    let body_after_otp: Vec<serde_json::Value> = get_after_otp.json().await.unwrap();
    assert_eq!(body_after_otp.len(), 1, "the OTP request must have recorded exactly one bot_log entry");
    assert_eq!(body_after_otp[0]["kind"], "otp", "the recorded entry's kind must be \"otp\"");
    assert_eq!(body_after_otp[0]["log_type"], "success");

    let delete_resp = http.delete(format!("{base}/bot/logs")).header("Cookie", &cookie).send().await.unwrap();
    assert_eq!(delete_resp.status(), 204);

    let get_after_delete = http.get(format!("{base}/bot/logs")).header("Cookie", &cookie).send().await.unwrap();
    let body_after_delete: Vec<serde_json::Value> = get_after_delete.json().await.unwrap();
    assert_eq!(body_after_delete.len(), 0, "DELETE must clear all entries");

    cleanup(&pool, tenant_id).await;
}
