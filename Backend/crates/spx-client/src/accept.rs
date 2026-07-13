// Backend/crates/spx-client/src/accept.rs
//! Pure 6-category classification of an SPX accept response (port of
//! spx.ts:922-944). Order matters: agency_dup is checked BEFORE the idempotent
//! "already accepted by you" pattern.
use std::sync::LazyLock;

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptReason {
    /// Accepted (incl. idempotent "already yours").
    Ok,
    /// SPX says OUR AGENCY already accepted — may be another account in the same
    /// agency (kasus Neva). Terminal like `Ok` (never retried), but the caller
    /// MUST verify the real acceptor and reclassify.
    AgencyDup,
    /// Another agency won / expired / closed — definitive, do not retry.
    Taken,
    /// Network/timeout/5xx/429/rate-limit — safe to retry.
    Transient,
    /// 401/403 — cookies expired — trigger relogin.
    Auth,
    /// Unexpected — logged for diagnosis.
    Error,
}

#[derive(Debug, Clone)]
pub struct AcceptResult {
    pub success: bool,
    pub reason: AcceptReason,
    pub retcode: i64,
    pub message: String,
}

static RE_AGENCY_DUP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"agency.{0,12}already.{0,12}(accept|take|took)").unwrap());
static RE_OK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"already.*(accept|own|your)|accepted by you|duplicate|telah .*terima").unwrap()
});
static RE_TAKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"taken|awarded|no longer|not available|unavailable|expired|closed|full|grabbed|assigned|sudah .*diambil|habis|tidak tersedia|kedaluwarsa",
    )
    .unwrap()
});
static RE_TRANSIENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"rate|too many|frequent|try again|busy|timeout|sibuk|coba lagi").unwrap()
});

/// Classify the accept response body. `retcode`/`json_success` come from the
/// SPX JSON; `raw_msg` is its `message`/`msg` field. HTTP-status-based `Auth`
/// and network `Transient` are decided by the caller (Task 9) BEFORE this — this
/// function only classifies a parsed JSON body.
pub fn classify_accept_response(retcode: i64, json_success: bool, raw_msg: &str) -> AcceptResult {
    let m = raw_msg.to_lowercase();
    let done = |success: bool, reason: AcceptReason| AcceptResult {
        success,
        reason,
        retcode,
        message: raw_msg.to_string(),
    };

    if retcode == 0 || json_success {
        return done(true, AcceptReason::Ok);
    }
    if RE_AGENCY_DUP.is_match(&m) {
        return done(true, AcceptReason::AgencyDup);
    }
    if RE_OK.is_match(&m) {
        return done(true, AcceptReason::Ok);
    }
    if RE_TAKEN.is_match(&m) {
        return done(false, AcceptReason::Taken);
    }
    if RE_TRANSIENT.is_match(&m) {
        return done(false, AcceptReason::Transient);
    }
    done(false, AcceptReason::Error)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The 8 REAL cases, verbatim from spx-accept.test.ts (the only recorded SPX
    // message corpus that exists — see the design doc's fixture-gap note).
    #[test]
    fn eight_real_cases() {
        let c = classify_accept_response(0, false, "success");
        assert_eq!(c.reason, AcceptReason::Ok);
        assert!(c.success);

        let c = classify_accept_response(
            150399,
            false,
            "Operation failed. Your agency already accepted this request before.",
        );
        assert_eq!(c.reason, AcceptReason::AgencyDup);
        assert!(c.success); // terminal — must not retry

        assert_eq!(
            classify_accept_response(1, false, "Request already accepted by you").reason,
            AcceptReason::Ok
        );
        assert_eq!(
            classify_accept_response(1, false, "duplicate request").reason,
            AcceptReason::Ok
        );

        let c = classify_accept_response(1, false, "This booking has been taken by another agency");
        assert_eq!(c.reason, AcceptReason::Taken);
        assert!(!c.success);

        assert_eq!(
            classify_accept_response(1, false, "The booking request has expired").reason,
            AcceptReason::Taken
        );
        assert_eq!(
            classify_accept_response(1, false, "too many requests, try again").reason,
            AcceptReason::Transient
        );
        assert_eq!(
            classify_accept_response(1, false, "sesuatu yang aneh").reason,
            AcceptReason::Error
        );
    }

    // Regression: if the idempotent-ok check ran first, "already accepted" would
    // swallow an agency-dup loss as a self-win. agency_dup MUST win.
    #[test]
    fn agency_dup_checked_before_ok() {
        let c = classify_accept_response(150399, false, "Your agency already accepted this request before");
        assert_eq!(c.reason, AcceptReason::AgencyDup, "must NOT be misclassified as Ok");
        assert!(c.success);
    }
}
