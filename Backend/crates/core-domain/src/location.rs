/// Normalize a location label: lowercase, collapse any run of non-alphanumeric characters to a
/// single space, trim. Mirrors TS `(s||'').toLowerCase().replace(/[^a-z0-9]+/g,' ').replace(/\s+/g,' ').trim()`.
pub fn norm_loc(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut last_was_space = true; // suppresses a leading space
    for ch in lower.chars() {
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

/// WHOLE-WORD location match — a raw substring match is dangerous here because a route rule
/// with no destination cap accepts EVERY matching ticket, so "bali" must never match
/// "Balikpapan", nor "solo" match "Solok", nor "medan" match "Medang". The needle matches only
/// when its exact phrase appears on word boundaries inside the haystack.
pub fn loc_match(hay: &str, needle: &str) -> bool {
    loc_match_normalized(hay, &norm_loc(needle))
}

/// Same as `loc_match`, but the needle is ALREADY normalized — used by `CompiledRule` so a
/// rule's fixed origin/destination strings are normalized once at compile time, not on every
/// booking evaluated against the rule.
pub fn loc_match_normalized(hay: &str, normalized_needle: &str) -> bool {
    if normalized_needle.is_empty() {
        return false;
    }
    let padded_hay = format!(" {} ", norm_loc(hay));
    let padded_needle = format!(" {} ", normalized_needle);
    padded_hay.contains(&padded_needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_loc_lowercases_and_collapses_punctuation() {
        assert_eq!(norm_loc("  Padang  DC! "), "padang dc");
    }

    #[test]
    fn bali_must_not_match_balikpapan_dc() {
        assert!(!loc_match("Balikpapan DC", "bali"));
    }

    #[test]
    fn solo_must_not_match_solok() {
        assert!(!loc_match("Solok", "solo"));
    }

    #[test]
    fn medan_must_not_match_medang() {
        assert!(!loc_match("Medang", "medan"));
    }

    #[test]
    fn exact_whole_word_matches() {
        assert!(loc_match("Padang DC", "Padang DC"));
    }

    #[test]
    fn whole_word_inside_a_phrase_matches() {
        assert!(loc_match("Transit Point Sunter DC", "Sunter DC"));
    }

    #[test]
    fn empty_needle_never_matches() {
        assert!(!loc_match("anything", ""));
    }
}
