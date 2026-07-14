//! WhatsApp/n8n message templates, ported from spx-portal-ref
//! `apps/api/src/services/webhook.ts` (`buildTicketBlock` / `buildWaMessage` /
//! `buildNewTicketsMessage` / `sendAgencyLossAlert` / `buildDriverAssignedMessage`).
//! Pure functions, unit-tested; no I/O. Layout MUST match the reference (the
//! ops team reads these WhatsApp messages verbatim).
//!
//! Cross-checked directly against `/tmp/spx-portal-ref/apps/api/src/services/webhook.ts`
//! (not just the task brief's transcription) — see doc comments below for the
//! discrepancies found and how each was resolved.
use chrono::{Datelike, FixedOffset, TimeZone, Timelike, Utc};

use crate::NotifyBooking;

fn wib() -> FixedOffset {
    FixedOffset::east_opt(7 * 3600).expect("valid +7")
}

/// Reference `idVal()`: `String(v ?? '').trim(); s && s !== '0' ? s : '-'`.
fn id_val(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() || t == "0" {
        "-".to_string()
    } else {
        t.to_string()
    }
}

/// Reference `COST_TYPE_LABEL: Record<number,string> = { 1: 'FTL', 2: 'LTL', 3: 'LTL' }`,
/// looked up as `COST_TYPE_LABEL[Number(p.costType)] || 'FTL'`.
const COST_LABELS: [(i64, &str); 3] = [(1, "FTL"), (2, "LTL"), (3, "LTL")];
fn cost_label(v: Option<i64>) -> &'static str {
    v.and_then(|n| COST_LABELS.iter().find(|(k, _)| *k == n).map(|(_, l)| *l))
        .unwrap_or("FTL")
}

/// SPX standby_time is MINUTES from midnight (e.g. 944 -> 15:44).
/// Reference `fmtStandby`: `Number.isFinite(m) && m >= 0` else `''`.
fn fmt_standby(min: Option<i64>) -> String {
    match min {
        Some(m) if m >= 0 => format!("{:02}:{:02}", m / 60, m % 60),
        _ => String::new(),
    }
}

/// unix seconds -> DD/MM/YYYY in WIB (Asia/Jakarta has no DST, fixed +7 is exact).
fn fmt_dmy(sec: Option<i64>) -> String {
    match sec {
        Some(s) if s > 0 => wib()
            .timestamp_opt(s, 0)
            .single()
            .map(|d| d.format("%d/%m/%Y").to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Reference `fmtPeriodDMY(p)`: `Number(p.periodStart) || Number(p.bookingDate) || 0`,
/// then DD/MM/YYYY with '/' replaced by ' - '.
///
/// NOTE (reference cross-check): the reference falls back to `p.bookingDate`
/// when `periodStart` is unset. `NotifyBooking` (this crate's declared
/// interface — see brief step 2/lib.rs) carries no `booking_date` field, so
/// this falls back to `period_end` instead — the closest available signal.
/// Whatever layer constructs `NotifyBooking` (poller/executor) should prefer
/// setting `period_start` to the real booking-date-or-period-start value so
/// this fallback is rarely exercised.
fn fmt_period_dmy(b: &NotifyBooking) -> String {
    let start = b.period_start.filter(|&s| s > 0).or(b.period_end);
    let s = fmt_dmy(start);
    if s.is_empty() {
        "-".to_string()
    } else {
        s.replace('/', " - ")
    }
}

/// Indonesian ("id-ID") locale month abbreviations, verified against real
/// Node `Intl` output (`toLocaleString('id-ID', { dateStyle: 'medium', ... })`)
/// on 2026-07-14: Jan Feb Mar Apr **Mei** Jun Jul **Agu** Sep **Okt** Nov **Des**
/// — differs from chrono's English `%b` in 4 months (Mei/Agu/Okt/Des), so a
/// literal `%b` port would silently mis-render those months to ops.
const ID_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "Mei", "Jun", "Jul", "Agu", "Sep", "Okt", "Nov", "Des",
];

/// Reference `buildDriverAssignedMessage`'s `when`:
/// `new Date().toLocaleString('id-ID', { dateStyle: 'medium', timeStyle: 'short', timeZone: 'Asia/Jakarta' })`.
/// Verified via real Node output: `"14 Jul 2026, 07.56"` — `DD Mon YYYY, HH.MM`
/// (Indonesian month abbreviation, **period** as the time separator, comma
/// between date and time — NOT a colon/no-comma `%H:%M` as a naive port would
/// produce).
fn fmt_id_datetime_now_wib() -> String {
    let now = Utc::now().with_timezone(&wib());
    let month = ID_MONTHS[(now.month() as usize).saturating_sub(1).min(11)];
    format!(
        "{:02} {} {}, {:02}.{:02}",
        now.day(),
        month,
        now.year(),
        now.hour(),
        now.minute()
    )
}

/// The ONE canonical ticket block (`buildTicketBlock`). `accepted` prepends
/// the TIKET-DITERIMA header; `link` (Some) appends the bell/accept-link
/// footer.
pub fn build_ticket_block(
    b: &NotifyBooking,
    accepted: bool,
    link: Option<&str>,
    portal_label: &str,
) -> String {
    let cost = cost_label(b.cost_type);
    // Reference: `String(p.bookingName || p.spxTxId || p.txId || '').toUpperCase().startsWith('SPXID')`
    // — JS `||` picks the FIRST truthy candidate, not an OR of independent
    // starts-with checks. NotifyBooking has no separate txId field, so the
    // chain is booking_name -> spx_tx_id -> "" (matching the reference's
    // fallback order exactly for the fields this crate's interface carries).
    let coc_source = if !b.booking_name.is_empty() {
        b.booking_name.as_str()
    } else if !b.spx_tx_id.is_empty() {
        b.spx_tx_id.as_str()
    } else {
        ""
    };
    let is_coc = coc_source.to_uppercase().starts_with("SPXID");
    let type_line = format!(
        "{cost} {}",
        if is_coc {
            "CENTRAL ON CALL ( COC )"
        } else {
            "REGULER ( REG )"
        }
    );
    let d: String = "—".repeat(25);
    // Reference: stops are mapped+trimmed+filtered FIRST, THEN checked for
    // emptiness (`stops.length ? stops.join(' → ') : '-'`) — not "is the raw
    // array empty" (a raw array of blank strings must still render '-').
    let stops: Vec<&str> = b
        .route_stops
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let rute = if stops.is_empty() {
        "-".to_string()
    } else {
        stops.join(" → ")
    };
    let station = {
        let t = b.report_station.trim();
        if t.is_empty() {
            "-".to_string()
        } else {
            t.to_string()
        }
    };
    let standby = {
        let s = fmt_standby(b.standby_time);
        if s.is_empty() {
            "-".to_string()
        } else {
            s
        }
    };
    const PAD: usize = 7;
    let row = |label: &str, val: &str| {
        let pad = PAD.saturating_sub(label.len());
        format!("{label}{} : {val}", " ".repeat(pad))
    };
    let name = if !b.booking_name.is_empty() {
        &b.booking_name
    } else if !b.spx_tx_id.is_empty() {
        &b.spx_tx_id
    } else {
        "-"
    };
    let req = id_val(&b.request_id);
    let on = id_val(&b.onsite_id);

    let mut lines: Vec<String> = Vec::new();
    if accepted {
        let suffix = if portal_label.is_empty() {
            String::new()
        } else {
            format!(" - {portal_label}")
        };
        lines.push(format!("*TIKET DI TERIMA OLEH SYSTEM{suffix}*"));
        lines.push(format!(" {type_line}"));
    } else {
        lines.push(type_line);
    }
    lines.push(d.clone());
    lines.push(row(
        "Booking",
        &format!("[ {} ] {}", id_val(&b.booking_id), name),
    ));
    lines.push(row(
        "Request",
        &(if req != "-" {
            format!("[ {req} ]")
        } else {
            "-".to_string()
        }),
    ));
    lines.push(row(
        "Onsite",
        &(if on != "-" {
            format!("[ {on} ]")
        } else {
            "-".to_string()
        }),
    ));
    lines.push(row("Station", &station));
    lines.push(row("Rute", &rute));
    lines.push(row(
        "Armada",
        if b.vehicle_type.is_empty() {
            "-"
        } else {
            &b.vehicle_type
        },
    ));
    lines.push(row("Periode", &fmt_period_dmy(b)));
    lines.push(row("Standby", &standby));
    lines.push(d.clone());
    if let Some(l) = link {
        lines.push(format!("🔔 : {l}"));
        lines.push(d);
    }
    lines.join("\n")
}

/// Accept notification (TIKET DITERIMA header, no link). Reference `buildWaMessage`.
pub fn build_wa_message(b: &NotifyBooking, portal_label: &str) -> String {
    build_ticket_block(b, true, None, portal_label)
}

/// New-ticket broadcast: up to 10 blocks (each with a bell/accept link) + "+N more".
/// Reference `buildNewTicketsMessage`.
///
/// `accept_base` is a Fase 5 addition: the reference mints a real one-tap
/// `/accept/<code>` link via a stateful Redis-backed random code (see
/// `genAcceptCode`/`acceptLink` in webhook.ts) — that's a server-side REST
/// concern (Fase 6), out of scope for this pure/no-I/O crate. Passing
/// `accept_base` (e.g. `https://portal/accept`) lets the caller mint a
/// placeholder `{accept_base}/{booking_id}` link instead; pass `""` to omit
/// the link entirely.
pub fn build_new_tickets_message(bs: &[NotifyBooking], portal_label: &str, accept_base: &str) -> String {
    const CAP: usize = 10;
    let shown = bs.iter().take(CAP);
    let blocks: Vec<String> = shown
        .map(|b| {
            let link = if accept_base.is_empty() {
                None
            } else {
                Some(format!("{accept_base}/{}", b.booking_id))
            };
            build_ticket_block(b, false, link.as_deref(), portal_label)
        })
        .collect();
    let mut out = blocks.join("\n\n");
    if bs.len() > CAP {
        out.push_str(&format!("\n{}\nTiket lain: +{}", "—".repeat(25), bs.len() - CAP));
    }
    out
}

/// Same-agency loss alert. Reference `sendAgencyLossAlert`'s text (ported verbatim).
pub fn build_agency_loss_text(spx_id: &str, rival: &str, latency_ms: i64, rule: Option<&str>) -> String {
    let rule_line = rule.map(|r| format!("\nRule: {r}")).unwrap_or_default();
    format!(
        "⚠️ KALAH RACE (rekan se-agency)\nTiket: {spx_id}\nDiambil oleh: {rival}\nTembakan kita: {latency_ms}ms{rule_line}\n— rival mengalahkan kita di race ini (bukti race diperebutkan)"
    )
}

/// Driver-assigned follow-up. Reference `buildDriverAssignedMessage`.
pub fn build_driver_assigned_message(
    tx_id: &str,
    booking_id: &str,
    onsite_id: &str,
    driver_name: &str,
    plate: &str,
    portal_label: &str,
) -> String {
    // Reference `LABEL_SUFFIX = PORTAL_LABEL ? \` · ${PORTAL_LABEL}\` : ''` (middle dot U+00B7).
    let suffix = if portal_label.is_empty() {
        String::new()
    } else {
        format!(" · {portal_label}")
    };
    // Reference `DIV = '——————————————————'` — verified 18 em-dashes (U+2014)
    // by counting the literal in webhook.ts, NOT 20 (the brief's transcription
    // used 20; this is the one real transcription gap found by cross-checking
    // the reference source directly instead of trusting the brief).
    let div = "—".repeat(18);
    let when = fmt_id_datetime_now_wib();
    // Reference: `Nomor Booking: *${idVal(p.txId || p.bookingId)}*` — txId
    // falls back to bookingId (JS `||`) BEFORE idVal is applied; a literal
    // `id_val(tx_id)` (no fallback) would show "-" whenever tx_id is empty
    // even if booking_id is present.
    let nomor_booking = if !tx_id.is_empty() { tx_id } else { booking_id };
    [
        format!("*SPX AGENCY PORTAL{suffix}*"),
        "*Driver & Armada Ditugaskan*".to_string(),
        div.clone(),
        format!("Nomor Booking: *{}*", id_val(nomor_booking)),
        format!("Booking ID: {}", id_val(booking_id)),
        format!("Onsite ID: {}", id_val(onsite_id)),
        div,
        format!(
            "Driver: *{}*",
            if driver_name.is_empty() { "-" } else { driver_name }
        ),
        format!("Nomor Polisi: *{}*", if plate.is_empty() { "-" } else { plate }),
        format!("Waktu: {when} WIB"),
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> NotifyBooking {
        NotifyBooking {
            booking_id: "5931641".into(),
            request_id: "0".into(),
            onsite_id: "6141306".into(),
            booking_name: "SPXID_VM_001396562".into(),
            spx_tx_id: "SPXID_VM_001396562".into(),
            vehicle_type: "TRONTON (10WH)".into(),
            route_stops: vec!["Cileungsi DC".into(), "Medan Amplas DC".into()],
            report_station: "Cileungsi DC".into(),
            cost_type: Some(1),
            adhoc_tag: Some(1),
            standby_time: Some(944),
            period_start: Some(1_750_000_000),
            period_end: None,
            bidding_ddl: None,
            is_coc: true,
        }
    }

    #[test]
    fn accepted_block_has_header_and_coc_type_and_aligned_rows() {
        let s = build_wa_message(&sample(), "12 LOG");
        assert!(s.contains("*TIKET DI TERIMA OLEH SYSTEM - 12 LOG*"));
        assert!(s.contains("FTL CENTRAL ON CALL ( COC )"));
        assert!(s.contains("Booking : [ 5931641 ] SPXID_VM_001396562"));
        assert!(s.contains("Standby : 15:44")); // 944 min from midnight
        assert!(!s.contains("🔔"), "accept notif has NO link");
    }

    #[test]
    fn reg_type_when_booking_name_does_not_start_with_spxid() {
        // is_coc=true on NotifyBooking must NOT override the reference's
        // string-based detection (booking_name/spx_tx_id starts-with SPXID) —
        // the reference has no such field at all.
        let mut b = sample();
        b.booking_name = "REG-00123".into();
        b.spx_tx_id = "REG-00123".into();
        b.is_coc = true;
        let s = build_ticket_block(&b, false, None, "");
        assert!(s.contains("FTL REGULER ( REG )"));
        assert!(!s.contains("CENTRAL ON CALL"));
    }

    #[test]
    fn blank_route_stops_render_dash_not_empty_string() {
        let mut b = sample();
        b.route_stops = vec!["   ".into(), "".into()];
        let s = build_ticket_block(&b, false, None, "");
        assert!(s.contains("Rute    : -"));
    }

    #[test]
    fn new_tickets_caps_at_ten_with_more_line() {
        let many: Vec<NotifyBooking> = (0..13)
            .map(|i| {
                let mut b = sample();
                b.booking_id = i.to_string();
                b
            })
            .collect();
        let s = build_new_tickets_message(&many, "EPL", "https://p/accept");
        assert!(s.contains("Tiket lain: +3"));
        assert!(s.contains("🔔 : https://p/accept/0"));
    }

    #[test]
    fn agency_loss_text_matches_reference_shape() {
        let s = build_agency_loss_text("SPXID1", "rival@x.com", 42, Some("R"));
        assert!(s.starts_with("⚠️ KALAH RACE"));
        assert!(s.contains("Diambil oleh: rival@x.com"));
        assert!(s.contains("Tembakan kita: 42ms"));
        assert!(s.contains("Rule: R"));
        assert!(s.ends_with("— rival mengalahkan kita di race ini (bukti race diperebutkan)"));
    }

    #[test]
    fn agency_loss_text_omits_rule_line_when_none() {
        let s = build_agency_loss_text("SPXID1", "rival@x.com", 42, None);
        assert!(!s.contains("Rule:"));
        assert!(s.contains("Tembakan kita: 42ms\n— rival"));
    }

    #[test]
    fn driver_assigned_message_has_correct_fields_and_divider_length() {
        let s = build_driver_assigned_message("SPXID_VM_1", "5931641", "6141306", "Budi", "B 1234 XY", "EPL");
        assert!(s.contains("*SPX AGENCY PORTAL · EPL*"));
        assert!(s.contains("*Driver & Armada Ditugaskan*"));
        assert!(s.contains("Nomor Booking: *SPXID_VM_1*"));
        assert!(s.contains("Booking ID: 5931641"));
        assert!(s.contains("Onsite ID: 6141306"));
        assert!(s.contains("Driver: *Budi*"));
        assert!(s.contains("Nomor Polisi: *B 1234 XY*"));
        // 18 em-dashes, verified against the reference's DIV constant.
        assert!(s.contains(&"—".repeat(18)));
        assert!(!s.contains(&"—".repeat(19)));
    }

    #[test]
    fn driver_assigned_message_falls_back_to_booking_id_when_tx_id_empty() {
        let s = build_driver_assigned_message("", "5931641", "6141306", "Budi", "B 1234 XY", "");
        assert!(s.contains("Nomor Booking: *5931641*"));
        assert!(s.contains("*SPX AGENCY PORTAL*")); // no label suffix when portal_label empty
    }
}
