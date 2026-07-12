/// SPX vehicle names carry a capacity suffix — "TRONTON (10WH)", "FUSO STD (6WH)". Maps the
/// short class label an operator types to the canonical uppercase label SPX actually returns.
fn vehicle_rule_label(normalized: &str) -> Option<&'static str> {
    match normalized {
        "tronton" => Some("TRONTON"),
        "fuso" => Some("FUSO"),
        "fuso std" => Some("FUSO"),
        "cdd long" => Some("CDD LONG"),
        "cde long" => Some("CDE LONG"),
        "blindvan" => Some("BLINDVAN"),
        "wingbox" => Some("WINGBOX"),
        "engkel" => Some("ENGKEL"),
        "40fcl" => Some("40FCL"),
        _ => None,
    }
}

/// Lowercase, strip any "(...)" span (capacity suffix), keep only `[a-z0-9 ]`, collapse
/// whitespace runs, trim. Mirrors TS
/// `(s||'').toLowerCase().replace(/\([^)]*\)/g,' ').replace(/[^a-z0-9 ]+/g,' ').replace(/\s+/g,' ').trim()`.
pub fn norm_vehicle(s: &str) -> String {
    let lower = s.to_lowercase();

    let mut depth = 0u32;
    let mut no_parens = String::with_capacity(lower.len());
    for ch in lower.chars() {
        match ch {
            '(' => {
                depth += 1;
                no_parens.push(' ');
            }
            ')' => {
                depth = depth.saturating_sub(1);
                no_parens.push(' ');
            }
            _ if depth > 0 => no_parens.push(' '),
            _ => no_parens.push(ch),
        }
    }

    let mut out = String::with_capacity(no_parens.len());
    let mut last_was_space = true;
    for ch in no_parens.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_was_space = false;
        } else if !last_was_space {
            out.push(' ');
            last_was_space = true;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// VEHICLE TYPE match — the rule stores the class an operator picked ("TRONTON", "FUSO", "CDD
/// LONG"); match as a WHOLE-WORD PREFIX so the short label resolves to the full SPX name, but
/// "CDD LONG" must never cross-match "CDE LONG (4WH)" (one letter off = different class).
pub fn vehicle_match(ticket_vehicle: &str, rule_type: &str) -> bool {
    vehicle_match_normalized(&norm_vehicle(ticket_vehicle), &norm_vehicle(rule_type))
}

/// Same as `vehicle_match`, but both sides are ALREADY normalized — the hot-matching path
/// normalizes a rule's service-type labels once at compile time via this entry point.
pub fn vehicle_match_normalized(ticket_norm: &str, rule_norm: &str) -> bool {
    if rule_norm.is_empty() || ticket_norm.is_empty() {
        return false;
    }
    if ticket_norm == rule_norm {
        return true;
    }
    let t_padded = format!("{} ", ticket_norm);
    let r_padded = format!("{} ", rule_norm);
    t_padded.starts_with(&r_padded)
}

/// Canonical label to store on a rule at save time (used by `sanitize_accept_rules`) — known
/// classes map to their canonical uppercase form; unknown classes fall back to the normalized
/// input uppercased (so an operator's custom vehicle string round-trips instead of being lost).
pub fn canonical_rule_vehicle_label(s: &str) -> String {
    let n = norm_vehicle(s);
    if n.is_empty() {
        return String::new();
    }
    vehicle_rule_label(&n)
        .map(str::to_string)
        .unwrap_or_else(|| n.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tronton_matches_tronton_10wh() {
        assert!(vehicle_match("TRONTON (10WH)", "TRONTON"));
    }

    #[test]
    fn lowercase_tronton_matches_tronton_10wh() {
        assert!(vehicle_match("TRONTON (10WH)", "tronton"));
    }

    #[test]
    fn fuso_matches_fuso_std_6wh_whole_word_prefix() {
        assert!(vehicle_match("FUSO STD (6WH)", "FUSO"));
    }

    #[test]
    fn cdd_long_matches_cdd_long_6wh() {
        assert!(vehicle_match("CDD LONG (6WH)", "CDD LONG"));
    }

    #[test]
    fn cdd_long_does_not_match_cde_long_4wh_one_letter_off() {
        assert!(!vehicle_match("CDE LONG (4WH)", "CDD LONG"));
    }

    #[test]
    fn tronton_does_not_match_fuso_std_6wh_different_class() {
        assert!(!vehicle_match("FUSO STD (6WH)", "TRONTON"));
    }

    #[test]
    fn blindvan_matches_blindvan_4wh() {
        assert!(vehicle_match("BLINDVAN (4WH)", "BLINDVAN"));
    }

    #[test]
    fn plain_40fcl_matches_40fcl() {
        assert!(vehicle_match("40FCL", "40FCL"));
    }

    #[test]
    fn empty_rule_never_matches() {
        assert!(!vehicle_match("TRONTON (10WH)", ""));
    }

    #[test]
    fn norm_vehicle_strips_capacity_suffix() {
        assert_eq!(norm_vehicle("TRONTON (10WH)"), "tronton");
    }

    #[test]
    fn canonical_rule_vehicle_label_maps_known_classes() {
        assert_eq!(canonical_rule_vehicle_label("tronton"), "TRONTON");
        assert_eq!(canonical_rule_vehicle_label("fuso std"), "FUSO");
        assert_eq!(canonical_rule_vehicle_label("cdd long"), "CDD LONG");
        assert_eq!(canonical_rule_vehicle_label(""), "");
        // unknown classes fall back to uppercased normalized input, not dropped
        assert_eq!(canonical_rule_vehicle_label("reefer"), "REEFER");
    }
}
