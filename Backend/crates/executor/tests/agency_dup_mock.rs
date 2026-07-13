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

/// Regression for a review finding: the case above places the winner LAST in the
/// op-log list, which can't distinguish "pick minimum create_time" from a
/// hypothetical bug that just returns "the last `@`-operator in list order".
/// Here the earliest-by-time entry sits in the MIDDLE, flanked by later-by-time
/// entries before and after it, so only a genuine min-by-create_time comparison
/// (not first-wins or last-wins list-position logic) picks it.
#[tokio::test]
async fn tie_break_prefers_earliest_create_time_with_non_monotonic_list_order() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "retcode": 0,
        "data": { "list": [
            { "booking_operation_type": 4, "operator": "later-op@x.com",      "create_time": 200 },
            { "booking_operation_type": 4, "operator": "earliest-op@x.com",   "create_time": 100 },
            { "booking_operation_type": 4, "operator": "even-later-op@x.com", "create_time": 300 }
        ]}
    });
    Mock::given(method("GET"))
        .and(path(PATH_BIDDING_LOG_LIST))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let client = SpxClient::new(server.uri()).expect("client");
    // self is none of the three → the earliest-create_time operator is the rival.
    let out = verify_agency_dup(&client, &cookies(), "someone@else.com", 42).await;
    assert_eq!(
        out,
        AgencyDupOutcome::LostToAgency {
            rival_email: "earliest-op@x.com".into()
        }
    );
}
