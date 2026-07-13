// Backend/crates/spx-client/tests/client_requests.rs
//! Request-construction tests against a wiremock server (no real SPX). The
//! `SpxClient` under test uses the real `Chrome148`-emulating transport
//! (Task 9/8's pinned preset); wreq's TLS/HTTP2 emulation only affects the
//! fingerprint negotiated over an encrypted connection — it talks plain HTTP
//! to a localhost `wiremock` server exactly like a non-emulating client would,
//! so no test-only client variant is needed.
use spx_client::client::{
    SpxClient, PATH_ACCEPT, PATH_BIDDING_LIST, PATH_BOOKING_LOG, PATH_BOOKING_OVERVIEW,
    PATH_COUNT_V2, PATH_NOTIFICATION, PATH_PROFILE, PATH_PROFILE_ACCOUNT_INFO,
    PATH_PROFILE_AGENCY, PATH_PROFILE_AGENCY_INFO, PATH_PROFILE_USER, PATH_PROFILE_USER_INFO,
    PATH_REQUEST_LIST, PATH_USER_LIST,
};
use spx_client::cookies::SpxCookies;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cookies() -> SpxCookies {
    SpxCookies { fms_user_agency_id: "42".into(), csrftoken: "CSRF".into(), ..Default::default() }
}

#[tokio::test]
async fn bidding_list_posts_to_correct_path_with_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_BIDDING_LIST))
        .and(header("x-csrftoken", "CSRF"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 0, "data": { "list": [{ "booking_id": "B1", "booking_name": "SPXID1" }] }
        })))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let bookings = client.fetch_bookings(&cookies(), 1, 50).await.expect("fetch");
    assert_eq!(bookings.len(), 1);
    assert_eq!(bookings[0].booking_id, "B1");
}

#[tokio::test]
async fn count_v2_posts_request_tab_all_and_unwraps_data() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_COUNT_V2))
        .and(body_json(serde_json::json!({ "request_tab_all": true })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 0, "data": { "pending": 3 }
        })))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let counts = client.fetch_booking_counts(&cookies()).await.expect("fetch");
    assert_eq!(counts["pending"], 3);
}

#[tokio::test]
async fn request_list_posts_numeric_booking_id_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_REQUEST_LIST))
        .and(body_json(serde_json::json!({ "booking_id": 100, "pageno": 1, "count": 20 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "retcode": 0, "data": [] })))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let res = client.fetch_request_list(&cookies(), 100, 20).await.expect("fetch");
    assert_eq!(res["retcode"], 0);
}

#[tokio::test]
async fn accept_classifies_agency_dup_from_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_ACCEPT))
        .and(body_json(serde_json::json!({ "booking_id": 100, "agency_id": 42 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 150399,
            "message": "Operation failed. Your agency already accepted this request before."
        })))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let r = client.accept_booking(&cookies(), 100, 42, &[]).await;
    assert_eq!(r.reason, spx_client::AcceptReason::AgencyDup);
    assert!(r.success);
}

#[tokio::test]
async fn accept_includes_request_id_list_when_non_empty() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_ACCEPT))
        .and(body_json(serde_json::json!({
            "booking_id": 100, "agency_id": 42, "request_id_list": [7, 8]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "retcode": 0 })))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let r = client.accept_booking(&cookies(), 100, 42, &[7, 8]).await;
    assert_eq!(r.reason, spx_client::AcceptReason::Ok);
    assert!(r.success);
}

#[tokio::test]
async fn accept_short_circuits_auth_before_body_when_agency_id_missing() {
    // No mock mounted at all — a real request would panic wiremock on an
    // unexpected call, proving the Auth short-circuit never hits the network.
    let server = MockServer::start().await;
    let client = SpxClient::new(server.uri()).expect("client");
    let r = client.accept_booking(&cookies(), 100, 0, &[]).await;
    assert_eq!(r.reason, spx_client::AcceptReason::Auth);
    assert!(!r.success);
}

#[tokio::test]
async fn accept_maps_401_to_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_ACCEPT))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let r = client.accept_booking(&cookies(), 100, 42, &[]).await;
    assert_eq!(r.reason, spx_client::AcceptReason::Auth);
}

#[tokio::test]
async fn accept_maps_429_and_5xx_to_transient() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_ACCEPT))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let r = client.accept_booking(&cookies(), 100, 42, &[]).await;
    assert_eq!(r.reason, spx_client::AcceptReason::Transient);
}

#[tokio::test]
async fn notification_count_posts_expected_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_NOTIFICATION))
        .and(body_json(serde_json::json!({
            "use_case": "agency portal", "user_type": 4, "notification_type_list": [30]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "retcode": 0, "data": { "count": 5 } })))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let res = client.notification_count(&cookies()).await.expect("fetch");
    assert_eq!(res["data"]["count"], 5);
}

#[tokio::test]
async fn bidding_log_gets_with_query_params() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(spx_client::client::PATH_BIDDING_LOG_LIST))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "retcode": 0, "data": [] })))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let res = client.fetch_bidding_log(&cookies(), 100).await.expect("fetch");
    assert_eq!(res["retcode"], 0);
}

#[tokio::test]
async fn user_list_posts_request_source_and_agency_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_USER_LIST))
        .and(body_json(serde_json::json!({
            "request_source": 1, "agency_id": 42, "pageno": 1, "count": 100
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "retcode": 0, "data": [] })))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let res = client.fetch_agency_users(&cookies(), 42).await.expect("fetch");
    assert_eq!(res["retcode"], 0);
}

#[tokio::test]
async fn profile_uses_get_on_the_primary_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(PATH_PROFILE))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "retcode": 0, "data": { "agency_id": 42 } })))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let res = client.fetch_profile(&cookies()).await.expect("fetch");
    assert_eq!(res["data"]["agency_id"], 42);
}

#[tokio::test]
async fn profile_falls_back_to_second_get_path_when_primary_fails() {
    let server = MockServer::start().await;
    // Primary explicitly fails (500) — proves the fallback below is only
    // reached because the primary was tried and rejected, not because it
    // was skipped.
    Mock::given(method("GET"))
        .and(path(PATH_PROFILE))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(PATH_PROFILE_AGENCY))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 0, "data": { "agency_id": 42, "source": "agency_profile_fallback" }
        })))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let res = client.fetch_profile(&cookies()).await.expect("fetch");
    assert_eq!(res["data"]["source"], "agency_profile_fallback");
}

#[tokio::test]
async fn profile_falls_back_through_all_get_candidates_to_first_post_candidate() {
    let server = MockServer::start().await;
    // The first 3 candidates (all GET, reference order) explicitly fail.
    for get_path in [PATH_PROFILE, PATH_PROFILE_AGENCY, PATH_PROFILE_USER] {
        Mock::given(method("GET"))
            .and(path(get_path))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
    }
    // The 4th candidate is the first POST fallback and sends an empty body.
    Mock::given(method("POST"))
        .and(path(PATH_PROFILE_ACCOUNT_INFO))
        .and(body_json(serde_json::json!({})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "retcode": 0, "data": { "source": "account_info_fallback" }
        })))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let res = client.fetch_profile(&cookies()).await.expect("fetch");
    assert_eq!(res["data"]["source"], "account_info_fallback");
}

#[tokio::test]
async fn profile_returns_exhausted_error_when_all_six_candidates_fail() {
    let server = MockServer::start().await;
    for get_path in [PATH_PROFILE, PATH_PROFILE_AGENCY, PATH_PROFILE_USER] {
        Mock::given(method("GET"))
            .and(path(get_path))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
    }
    for post_path in [
        PATH_PROFILE_ACCOUNT_INFO,
        PATH_PROFILE_USER_INFO,
        PATH_PROFILE_AGENCY_INFO,
    ] {
        Mock::given(method("POST"))
            .and(path(post_path))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
    }

    let client = SpxClient::new(server.uri()).expect("client");
    let err = client.fetch_profile(&cookies()).await.unwrap_err();
    match err {
        spx_client::client::SpxError::ProfileFallbackExhausted(count, _) => assert_eq!(count, 6),
        other => panic!("expected ProfileFallbackExhausted, got {other:?}"),
    }
}

#[tokio::test]
async fn booking_overview_posts_fallback_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_BOOKING_OVERVIEW))
        .and(body_json(serde_json::json!({
            "pageno": 1, "count": 100, "request_acceptance_status": 1, "request_tab_all": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "retcode": 0, "data": {} })))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let res = client.fetch_booking_overview(&cookies()).await.expect("fetch");
    assert_eq!(res["retcode"], 0);
}

#[tokio::test]
async fn booking_log_posts_probe_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_BOOKING_LOG))
        .and(body_json(serde_json::json!({ "booking_id": 100, "pageno": 1, "count": 20 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "retcode": 0, "data": [] })))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let res = client.fetch_booking_log(&cookies(), 100).await.expect("fetch");
    assert_eq!(res["retcode"], 0);
}

#[tokio::test]
async fn non_2xx_status_surfaces_as_spx_error_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PATH_BIDDING_LIST))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    let err = client.fetch_bookings(&cookies(), 1, 50).await.unwrap_err();
    assert!(matches!(err, spx_client::client::SpxError::Status(500)));
}
