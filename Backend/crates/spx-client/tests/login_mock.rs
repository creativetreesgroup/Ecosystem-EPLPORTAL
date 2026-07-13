// Backend/crates/spx-client/tests/login_mock.rs
//! Tier 2/3 login against a wiremock SPX server (DoD #5). Proves the
//! Set-Cookie-capture plumbing (Task 7's fill-in of the `login.rs`
//! `TODO(impl)`s) actually works: success == a captured `fms_user_skey`.
use spx_client::SpxClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn api_login_captures_fms_user_skey_from_set_cookie() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/basicserver/agency/account/login"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("set-cookie", "fms_user_skey=OKSKEY; Path=/"),
        )
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let jar = client
        .api_login("user@example.com", "hunter2")
        .await
        .expect("api_login must succeed off the first attempt's Set-Cookie");
    assert_eq!(jar.fms_user_skey, "OKSKEY");
}

#[tokio::test]
async fn api_login_falls_through_its_5_attempts_in_order() {
    let server = MockServer::start().await;
    // First 4 endpoint variants explicitly fail (no skey) — proves the 5th
    // (last-resort `/api/user/login`) is reached only after the earlier ones
    // were tried and rejected, not skipped.
    for failing_path in [
        "/api/basicserver/agency/account/login",
        "/api/basicserver/account/login",
        "/api/basicserver/agency/auth/login",
    ] {
        Mock::given(method("POST"))
            .and(path(failing_path))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/api/user/login"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("set-cookie", "fms_user_skey=LASTRESORT; Path=/"),
        )
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let jar = client
        .api_login("user@example.com", "hunter2")
        .await
        .expect("must fall through to the 5th attempt");
    assert_eq!(jar.fms_user_skey, "LASTRESORT");
}

#[tokio::test]
async fn api_login_returns_none_when_no_attempt_captures_a_skey() {
    let server = MockServer::start().await;
    // No mocks mounted at all: wiremock 404s every attempt — none captures a
    // Set-Cookie, so api_login must return None (not panic, not Some(empty)).
    let client = SpxClient::new(server.uri()).expect("client");
    let jar = client.api_login("user@example.com", "hunter2").await;
    assert!(jar.is_none());
}

#[tokio::test]
async fn form_login_follows_csrf_then_redirect_to_capture_session_cookies() {
    let server = MockServer::start().await;
    // Step 1: GET /login sets a csrftoken cookie.
    Mock::given(method("GET"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(200).insert_header("set-cookie", "csrftoken=CSRF-ABC; Path=/"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // Step 2: POST /login redirects (302) to /dashboard — NOT auto-followed by
    // the client (default redirect::Policy::none()), so login.rs must chase it
    // manually.
    Mock::given(method("POST"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/dashboard"))
        .mount(&server)
        .await;
    // Step 3: the redirect target is what actually sets the session cookie.
    Mock::given(method("GET"))
        .and(path("/dashboard"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("set-cookie", "fms_user_skey=FORMWIN; Path=/"),
        )
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let jar = client
        .form_login("user@example.com", "hunter2")
        .await
        .expect("form_login must follow GET -> POST(redirect) -> GET to capture the session cookie");
    assert_eq!(jar.fms_user_skey, "FORMWIN");
    assert_eq!(jar.csrftoken, "CSRF-ABC");
}

#[tokio::test]
async fn form_login_returns_none_when_no_redirect_and_no_skey() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    // POST /login just 200s with no Set-Cookie and no Location — a failed
    // login attempt (wrong credentials).
    Mock::given(method("POST"))
        .and(path("/login"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let jar = client.form_login("user@example.com", "wrongpass").await;
    assert!(jar.is_none());
}

#[tokio::test]
async fn fetch_spx_cid_is_noop_when_already_present() {
    let server = MockServer::start().await;
    // No mocks mounted — a real request would panic wiremock on an unexpected
    // call, proving the already-populated fast path never hits the network.
    let client = SpxClient::new(server.uri()).expect("client");
    let mut cookies = spx_client::SpxCookies {
        spx_cid: "ALREADY-SET".into(),
        ..Default::default()
    };
    client.fetch_spx_cid(&mut cookies).await;
    assert_eq!(cookies.spx_cid, "ALREADY-SET");
}

#[tokio::test]
async fn fetch_spx_cid_fills_from_first_page_that_sets_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/line-haul/booking"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("set-cookie", "spx_cid=CID-123; Path=/"),
        )
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let mut cookies = spx_client::SpxCookies::default();
    client.fetch_spx_cid(&mut cookies).await;
    assert_eq!(cookies.spx_cid, "CID-123");
}
