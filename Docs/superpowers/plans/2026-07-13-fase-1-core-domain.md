# TOWER Fase 1 — core-domain Rule Engine Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the SPX auto-accept rule engine (`matching.ts` + `coc.ts`, ~554 lines, ~90 tests) to a pure, I/O-free `core-domain` Rust crate with line-for-line semantic parity, all reference tests green.

**Architecture:** One module per responsibility inside `Backend/crates/core-domain/src/`: `coc.rs`, `booking.rs`, `location.rs`, `vehicle.rs`, `route_parse.rs`, `rule.rs`, `matching.rs`, re-exported from `lib.rs`. `matches_rule`/`find_best_matching_rule` are thin wrappers around `CompiledRule::compile()` + `CompiledRule::matches()` — the compiled form pre-normalizes a rule's origin/destinations/vehicle labels once (satisfies the master spec's "compile at save, not per ticket" requirement) instead of duplicating the matching logic in two places.

**Tech Stack:** Rust std only, except `serde_json` (already a project-wide dependency per the master spec's stack table) for the two route-parsing functions that operate on raw SPX JSON shapes.

## Global Constraints

Full context: [`Docs/tower-master-spec.md`](../../tower-master-spec.md) and [`Docs/superpowers/specs/2026-07-13-fase-1-core-domain-design.md`](../specs/2026-07-13-fase-1-core-domain-design.md).

- **Source of truth**, read-only, do not modify: `/tmp/spx-portal-ref/apps/api/src/services/matching.ts`, `.../matching.test.ts`, `.../route.test.ts`, `.../lib/coc.ts`, `.../lib/coc.test.ts`. If this path is missing, STOP and report BLOCKED — do not guess at behavior from this plan's prose alone; the plan's code is the intended translation, but the TS source is the arbiter of any ambiguity.
- **Every one of the ~90 reference test assertions must have a Rust `#[test]` equivalent.** No test is skipped, merged away, or "simplified" — this is money-critical logic and the plan's GATE (per master spec Fase 1) is 100% green before Fase 2 starts.
- `core-domain` has **zero I/O** — no `tokio`, `reqwest`, `sqlx`, `redis`. `serde_json` is the one permitted dependency (data-structure library, not I/O).
- Whole-word location matching (`loc_match`) must never let a short needle substring-match a longer place name (`bali` ⊄ `Balikpapan`, `solo` ⊄ `Solok`, `medan` ⊄ `Medang`).
- Booking-ID substring matching only applies when the normalized ID is **≥ 9 characters**; below that, only exact matches count.
- An empty/unconfigured `route` or `filter` rule must **match nothing** — never a blanket accept.
- Rule ranking: mode dominance (booking_id > route > filter) beats numeric `priority`, which beats specificity — never the reverse (CP-6).
- `matched_booking_id_for` must use **the same normalization** (`norm_id`) as `matches_rule`'s booking_id mode — this exact drift was a real production incident (see matching.ts's own comment above the function).
- Field naming: TS `camelCase` → Rust `snake_case` throughout, 1:1 renames, no restructuring beyond what's explicitly specified in this plan.

---

### Task 1: Foundational types + `coc.rs`

**Files:**
- Create: `Backend/crates/core-domain/src/booking.rs`
- Create: `Backend/crates/core-domain/src/coc.rs`
- Modify: `Backend/crates/core-domain/src/lib.rs` (currently empty — add module declarations + re-exports)

**Interfaces:**
- Consumes: nothing (first task).
- Produces: `pub struct Booking { route_stops: Vec<String>, report_station: String, spx_tx_id: String, booking_id: String, request_id: String, booking_type: BookingType, vehicle_type: String, weight: f64, cod_amount: f64, shift_type: i32, trip_type: i32 }` (derives `Debug, Clone, Default`), `pub enum BookingType { Spxid, Reguler }` (derives `Debug, Clone, Copy, PartialEq, Eq, Default` with `#[default]` on `Reguler`), `pub fn is_coc_name(name: &str) -> bool`, `pub fn is_coc(spx_id: &str, booking_name: &str) -> bool`, `pub fn booking_type_of(name: &str) -> BookingType`. All later tasks depend on `Booking`/`BookingType`.

- [ ] **Step 1: Read the reference source first**

Read `/tmp/spx-portal-ref/apps/api/src/lib/coc.ts` (39 lines) and `/tmp/spx-portal-ref/apps/api/src/lib/coc.test.ts` (45 lines) in full before writing any code — this task's code below is the intended translation, but confirm it against the actual file.

- [ ] **Step 2: Write `booking.rs`**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BookingType {
    #[default]
    Reguler,
    Spxid,
}

/// Subset of the reference `SpxBooking` interface (`spx.ts:7-37`) actually read by the rule
/// matcher. Fields the matcher never reads (originRegion/originProvince/destinationRegion/
/// destinationProvince/cod/etc.) are intentionally absent — matching.test.ts proves the real
/// engine ignores them (a route rule must NOT fall back to province/region labels). The full
/// SPX booking shape is built in Fase 3 (spx-client) and mapped down to this type.
#[derive(Debug, Clone, Default)]
pub struct Booking {
    pub route_stops: Vec<String>,
    pub report_station: String,
    pub spx_tx_id: String,
    pub booking_id: String,
    pub request_id: String,
    pub booking_type: BookingType,
    pub vehicle_type: String,
    pub weight: f64,
    pub cod_amount: f64,
    pub shift_type: i32,
    pub trip_type: i32,
}
```

- [ ] **Step 3: Write `coc.rs`**

```rust
use crate::booking::BookingType;

/// True when a transaction/booking name marks a COC (SPXID) ticket. Case/space tolerant.
/// Mirrors the TS regex `/^\s*SPXID/i` without a regex dependency: trim leading whitespace,
/// then case-insensitive compare the next 5 bytes against "SPXID".
pub fn is_coc_name(name: &str) -> bool {
    let trimmed = name.trim_start();
    trimmed.len() >= 5 && trimmed.as_bytes()[..5].eq_ignore_ascii_case(b"SPXID")
}

/// Authoritative COC test from the two identifiers persisted per booking: the SPX id and the
/// raw_data booking_name. Mirrors the SQL predicate used for DB-wide counts (Fase 2's
/// `IS_COC_SQL`) so the app layer and the DB layer always agree.
pub fn is_coc(spx_id: &str, booking_name: &str) -> bool {
    is_coc_name(spx_id) || is_coc_name(booking_name)
}

/// Canonical COC/REG label from a booking's transaction name.
pub fn booking_type_of(name: &str) -> BookingType {
    if is_coc_name(name) { BookingType::Spxid } else { BookingType::Reguler }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod is_coc_name_spxid_prefix_rule {
        use super::*;

        #[test]
        fn spxid_prefixed_names_are_coc() {
            assert!(is_coc_name("SPXID12345"));
            assert!(is_coc_name("spxid-lower"));
            assert!(is_coc_name("  SPXID-leading-space"));
        }

        #[test]
        fn non_spxid_names_are_reg() {
            assert!(!is_coc_name("BK-778899"));
            assert!(!is_coc_name("REGULER-1"));
            assert!(!is_coc_name("MY-SPXID-suffix"));
        }

        #[test]
        fn empty_or_missing_name_is_reg() {
            // TS also asserts isCocName(null)/isCocName(undefined) === false; Rust's &str has
            // no null/undefined, so the empty-string case is the exhaustive equivalent.
            assert!(!is_coc_name(""));
        }
    }

    mod is_coc_from_either_identifier {
        use super::*;

        #[test]
        fn coc_when_booking_name_is_spxid_even_if_spx_id_is_plain_booking_id() {
            assert!(is_coc("884412771", "SPXID99887766"));
        }

        #[test]
        fn coc_when_spx_id_itself_is_spxid() {
            assert!(is_coc("SPXID55", ""));
        }

        #[test]
        fn reg_when_neither_identifier_is_spxid() {
            assert!(!is_coc("884412771", "BK-1"));
            assert!(!is_coc("884412771", ""));
        }
    }

    mod booking_type_of_canonical_label {
        use super::*;

        #[test]
        fn maps_to_spxid_reguler() {
            assert_eq!(booking_type_of("SPXID1"), BookingType::Spxid);
            assert_eq!(booking_type_of("anything-else"), BookingType::Reguler);
            assert_eq!(booking_type_of(""), BookingType::Reguler);
        }
    }
}
```

- [ ] **Step 4: Wire up `lib.rs`**

```rust
pub mod booking;
pub mod coc;

pub use booking::{Booking, BookingType};
pub use coc::{booking_type_of, is_coc, is_coc_name};
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p core-domain`
Expected: 7 tests pass (3 + 3 + 1 across the three `mod` blocks), `test result: ok. 7 passed; 0 failed`.

- [ ] **Step 6: Clippy**

Run: `cargo clippy -p core-domain -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add Backend/crates/core-domain/src/booking.rs Backend/crates/core-domain/src/coc.rs Backend/crates/core-domain/src/lib.rs
git commit -m "feat(core-domain): port coc.ts + foundational Booking/BookingType types"
```

---

### Task 2: `location.rs` — whole-word location matching

**Files:**
- Create: `Backend/crates/core-domain/src/location.rs`
- Modify: `Backend/crates/core-domain/src/lib.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub fn norm_loc(s: &str) -> String`, `pub fn loc_match(hay: &str, needle: &str) -> bool`, `pub fn loc_match_normalized(hay: &str, normalized_needle: &str) -> bool` (needle already run through `norm_loc` — this is what `CompiledRule` uses in Task 8/9 to avoid re-normalizing a rule's fixed origin/destination strings on every booking). Task 7-9 (matching.rs) depend on all three.

- [ ] **Step 1: Read the reference source first**

Read `normLoc`/`locMatch` in `/tmp/spx-portal-ref/apps/api/src/services/matching.ts` (lines 90-103) and the `locMatch / normLoc` describe block in `matching.test.ts` (lines 17-27).

- [ ] **Step 2: Write the failing tests**

Create `Backend/crates/core-domain/src/location.rs` with only this test module (references `norm_loc`/`loc_match`, which don't exist yet — must fail to compile):

```rust
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
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p core-domain location`
Expected: FAIL — compile error, `cannot find function \`norm_loc\`` (and `loc_match`).

- [ ] **Step 4: Implement**

Prepend this above the test module in `location.rs`:

```rust
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
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p core-domain location`
Expected: `test result: ok. 7 passed; 0 failed`.

- [ ] **Step 6: Wire into `lib.rs`**

Add `pub mod location;` and `pub use location::{loc_match, loc_match_normalized, norm_loc};` to `Backend/crates/core-domain/src/lib.rs`.

- [ ] **Step 7: Clippy**

Run: `cargo clippy -p core-domain -- -D warnings` — expected clean.

- [ ] **Step 8: Commit**

```bash
git add Backend/crates/core-domain/src/location.rs Backend/crates/core-domain/src/lib.rs
git commit -m "feat(core-domain): port normLoc/locMatch whole-word location matching"
```

---

### Task 3: `vehicle.rs` — canonical vehicle type matching

**Files:**
- Create: `Backend/crates/core-domain/src/vehicle.rs`
- Modify: `Backend/crates/core-domain/src/lib.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub fn norm_vehicle(s: &str) -> String`, `pub fn vehicle_match(ticket_vehicle: &str, rule_type: &str) -> bool`, `pub fn vehicle_match_normalized(ticket_norm: &str, rule_norm: &str) -> bool`, `pub fn canonical_rule_vehicle_label(s: &str) -> String`. Task 5 (`sanitize_accept_rules`) uses `canonical_rule_vehicle_label` + `norm_vehicle`; Task 7-9 (`matching.rs`) use `vehicle_match_normalized`.

- [ ] **Step 1: Read the reference source first**

Read `matching.ts` lines 51-61 (`VEHICLE_RULE_LABELS`) and 105-125 (`normVehicle`/`vehicleMatch`/`canonicalRuleVehicleLabel`), plus the `vehicleMatch — canonical SPX vehicle names` describe block in `matching.test.ts` (lines 98-109).

- [ ] **Step 2: Write the failing tests**

Create `Backend/crates/core-domain/src/vehicle.rs` with only this test module:

```rust
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

    // Added during Task 3's review: not from the TS reference test file (which doesn't
    // exercise malformed input), but required to lock in the paren-stripping fix — see
    // strip_paren_spans's doc comment in Step 4 for the incident this prevents.
    #[test]
    fn norm_vehicle_unmatched_open_paren_preserves_trailing_content() {
        assert_eq!(norm_vehicle("ENGKEL BAK (STD"), "engkel bak std");
        assert_eq!(norm_vehicle("TRONTON (10WH"), "tronton 10wh");
    }

    #[test]
    fn norm_vehicle_nested_parens_are_not_depth_tracked_matches_js_regex() {
        assert_eq!(norm_vehicle("a (b (c) d) e"), "a d e");
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p core-domain vehicle`
Expected: FAIL — compile error, missing functions.

- [ ] **Step 4: Implement**

Prepend above the test module:

```rust
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

/// Mirrors JS regex `/\([^)]*\)/g`: for each '(' scanned left-to-right, find the NEXT ')'
/// after it (no nested-depth tracking — `[^)]*` just means "any non-')' char", so
/// `(b (c)` is ONE match ending at the first ')'). Replace each matched span with a single
/// space. An unmatched trailing '(' (no ')' anywhere after it) is left as literal text — the
/// caller's subsequent `[^a-z0-9 ]` stripping pass turns the lone '(' into a space, same as
/// the TS second regex pass does, but never drops the real content after it.
///
/// (Corrected during Task 3's review: an earlier depth-tracking version blanked everything
/// from an unmatched '(' to end-of-string, e.g. turning "TRONTON (10WH" into "tronton"
/// instead of the reference's "tronton 10wh" — an over-blanked rule string matches a
/// STRICTLY LARGER set of tickets under the whole-word-prefix check, an over-accept risk on
/// malformed operator input. Fixed to match JS's actual first-match, non-nesting semantics.)
fn strip_paren_spans(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '(' {
            if let Some(rel_close) = chars[i + 1..].iter().position(|&c| c == ')') {
                let close_idx = i + 1 + rel_close;
                out.push(' ');
                i = close_idx + 1;
                continue;
            } else {
                out.extend(&chars[i..]);
                break;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Lowercase, strip any "(...)" span (capacity suffix) via `strip_paren_spans`, keep only
/// `[a-z0-9 ]`, collapse whitespace runs, trim. Mirrors TS
/// `(s||'').toLowerCase().replace(/\([^)]*\)/g,' ').replace(/[^a-z0-9 ]+/g,' ').replace(/\s+/g,' ').trim()`.
pub fn norm_vehicle(s: &str) -> String {
    let lower = s.to_lowercase();
    let no_parens = strip_paren_spans(&lower);

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
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p core-domain vehicle`
Expected: `test result: ok. 13 passed; 0 failed`. (11 from the reference port + 2 regression tests added during Task 3's review for a paren-stripping edge case not covered by the reference test file — see the `strip_paren_spans` doc comment in Step 4.)

- [ ] **Step 6: Wire into `lib.rs`**

Add `pub mod vehicle;` and `pub use vehicle::{canonical_rule_vehicle_label, norm_vehicle, vehicle_match, vehicle_match_normalized};`.

- [ ] **Step 7: Clippy**

Run: `cargo clippy -p core-domain -- -D warnings` — expected clean.

- [ ] **Step 8: Commit**

```bash
git add Backend/crates/core-domain/src/vehicle.rs Backend/crates/core-domain/src/lib.rs
git commit -m "feat(core-domain): port normVehicle/vehicleMatch canonical vehicle matching"
```

---

### Task 4: `route_parse.rs` — SPX route-shape parsing

**Files:**
- Create: `Backend/crates/core-domain/src/route_parse.rs`
- Modify: `Backend/crates/core-domain/src/lib.rs`
- Modify: `Backend/crates/core-domain/Cargo.toml` (add `serde_json` dependency)

**Interfaces:**
- Consumes: nothing new.
- Produces: `pub struct RouteNode { name: String, province: String, city: String }` (derives `Debug, Clone, PartialEq, Eq`), `pub fn parse_route_detail_list(raw: &serde_json::Value) -> Vec<RouteNode>`, `pub fn parse_route_stops(raw: &serde_json::Value) -> Vec<String>`. Not consumed by later Fase-1 tasks (matching.rs takes a pre-built `Booking`, not raw JSON) — this is ported now because it's part of `matching.ts`'s public surface and `route.test.ts` requires it, and Fase 3's `spx-client` will call it directly when building `Booking.route_stops`.

**Design note on JS `??` vs `||`/truthiness — read before writing code:** the reference source mixes three different "is this value usable" checks that behave differently on edge cases, and this task's translation must keep them distinct:
1. `x ?? fallback` (nullish coalescing) — only falls through on `null`/`undefined`, NOT on `""` or `0`. Used for the `dc_name ?? hub_name ?? name ?? location ?? ''` chain in `parseRouteStops` step 3.
2. `obj[k] !== undefined && obj[k] !== null && obj[k] !== ''` (the `pick` helper) — falls through on null/undefined AND empty string. Used for the origin/dest DC name lookup in step 6.
3. `if (raw.sgi_route_name)` (truthiness) — falls through on null/undefined/empty-string/0/false. Used in step 4.

Conflating these (e.g. implementing all three as "skip if empty string") silently changes which field wins when an earlier key resolves to an explicit empty string — get this right per the code below, which keeps three distinct helpers for exactly this reason.

- [ ] **Step 1: Read the reference source first**

Read `matching.ts` lines 462-515 (`pick`, `RouteNode`, `parseRouteDetailList`, `parseRouteStops`) and all of `route.test.ts` (53 lines).

- [ ] **Step 2: Add the `serde_json` dependency**

```bash
cd Backend && cargo add --package core-domain serde_json && cd ..
```

- [ ] **Step 3: Write the failing tests**

Create `Backend/crates/core-domain/src/route_parse.rs` with only this test module (uses `serde_json::json!` to build fixtures matching `route.test.ts`'s `rdl(...)` helper):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rdl(names: &[&str]) -> serde_json::Value {
        let node_info_list: Vec<serde_json::Value> = names
            .iter()
            .enumerate()
            .map(|(i, n)| json!({ "name": n, "address_info": { "l1": format!("PROV_{i}"), "l2": format!("CITY_{i}") } }))
            .collect();
        json!({ "route_detail_list": [{ "node_info_list": node_info_list }] })
    }

    mod parse_route_detail_list_tests {
        use super::*;

        #[test]
        fn extracts_ordered_node_names_from_route_detail_list() {
            let nodes = parse_route_detail_list(&rdl(&["Padang DC", "Cileungsi DC"]));
            let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
            assert_eq!(names, vec!["Padang DC", "Cileungsi DC"]);
        }

        #[test]
        fn captures_province_l1_from_address_info() {
            let nodes = parse_route_detail_list(&rdl(&["A DC", "B DC"]));
            assert_eq!(nodes[0].province, "PROV_0");
        }

        #[test]
        fn missing_route_detail_list_is_empty() {
            assert_eq!(parse_route_detail_list(&json!({})), vec![]);
        }

        #[test]
        fn node_info_list_not_an_array_is_skipped_no_panic() {
            let raw = json!({ "route_detail_list": [{ "node_info_list": "oops" }] });
            assert_eq!(parse_route_detail_list(&raw), vec![]);
        }

        #[test]
        fn nodes_with_empty_name_are_dropped() {
            let raw = json!({ "route_detail_list": [{ "node_info_list": [{ "name": "" }, { "name": "Real DC" }] }] });
            let nodes = parse_route_detail_list(&raw);
            let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
            assert_eq!(names, vec!["Real DC"]);
        }
    }

    mod parse_route_stops_tests {
        use super::*;

        #[test]
        fn regression_parses_route_from_route_detail_list() {
            let stops = parse_route_stops(&rdl(&["Banjarmasin 2 DC", "Pontianak DC"]));
            assert_eq!(stops, vec!["Banjarmasin 2 DC", "Pontianak DC"]);
        }

        #[test]
        fn pre_enriched_route_stops_array_wins_highest_priority() {
            let raw = json!({ "route_stops": ["Aceh DC", "Cileungsi DC"], "route_detail_list": [] });
            assert_eq!(parse_route_stops(&raw), vec!["Aceh DC", "Cileungsi DC"]);
        }

        #[test]
        fn falls_back_to_report_station_name_when_no_route_data() {
            let raw = json!({ "report_station_name": "Medan DC" });
            assert_eq!(parse_route_stops(&raw), vec!["Medan DC"]);
        }

        #[test]
        fn falls_back_to_origin_and_dest_dc_names() {
            let raw = json!({ "origin_dc_name": "X DC", "dest_dc_name": "Y DC" });
            assert_eq!(parse_route_stops(&raw), vec!["X DC", "Y DC"]);
        }

        #[test]
        fn completely_empty_raw_is_empty() {
            assert_eq!(parse_route_stops(&json!({})), Vec::<String>::new());
        }

        #[test]
        fn empty_route_detail_list_nodes_falls_through_does_not_return_empty() {
            let raw = json!({ "route_detail_list": [{ "node_info_list": [] }], "report_station_name": "Solo DC" });
            assert_eq!(parse_route_stops(&raw), vec!["Solo DC"]);
        }

        #[test]
        fn three_stop_route_preserved_in_order() {
            let stops = parse_route_stops(&rdl(&["Yogyakarta DC", "Purbalingga DC", "Banyumas DC"]));
            assert_eq!(stops, vec!["Yogyakarta DC", "Purbalingga DC", "Banyumas DC"]);
        }

        // Added during Task 4's review: not from route.test.ts (which doesn't exercise these
        // shapes), but required to lock in two fixes — see pick_nullish_value's and
        // bare_string's doc comments for the incidents these prevent.
        #[test]
        fn route_list_wrong_type_does_not_fall_through_to_routes() {
            let raw = json!({ "route_list": "invalid", "routes": [{ "dc_name": "A DC" }] });
            assert_eq!(parse_route_stops(&raw), Vec::<String>::new());
        }

        #[test]
        fn route_stops_null_entry_bare_stringifies_to_literal_null_not_empty() {
            let raw = json!({ "route_stops": [serde_json::Value::Null, "Aceh DC"] });
            assert_eq!(parse_route_stops(&raw), vec!["null".to_string(), "Aceh DC".to_string()]);
        }
    }
}
```

- [ ] **Step 4: Run to verify it fails**

Run: `cargo test -p core-domain route_parse`
Expected: FAIL — compile error, missing functions/types.

- [ ] **Step 5: Implement**

Prepend above the test module:

```rust
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteNode {
    pub name: String,
    pub province: String,
    pub city: String,
}

/// Bare JS `String(x)` coercion — unlike `field_string` (`x ?? ''` then stringify), this does
/// NOT treat null as `""`: JS `String(null)` is the literal string `"null"`, a non-empty,
/// truthy value. Array/Object inputs are approximated as their compact JSON representation
/// rather than JS's exact `Array.prototype.toString()`/`"[object Object]"` — this file's
/// fields (route/DC names) never realistically hold non-primitive values, and no reference
/// test exercises this shape, so exact fidelity here isn't worth the complexity (a documented
/// simplification, not a silent gap).
///
/// (Corrected during Task 4's review: `field_string`'s original null→"" handling was
/// mistakenly reused for step 1's `route_stops.map(String)`, which is BARE `String(x)` in the
/// reference — a `null` entry must stringify to `"null"`, not `""`. See `field_string`'s doc
/// comment for why the two must stay distinct functions, not be merged.)
fn bare_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(_) | Value::Object(_) => v.to_string(),
    }
}

/// `String(x ?? '')` — null/missing become "", everything else stringifies via `bare_string`.
/// Never treats a present empty string as "missing" (see the module doc for why that
/// distinction matters). NOT the same as bare `String(x)` — see `bare_string`.
fn field_string(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(other) => bare_string(other),
    }
}

/// `x ?? undefined` semantics as an `Option`: `None` only for null/missing — a present empty
/// string is `Some("")`, distinct from missing. Used for the `dc_name ?? hub_name ?? ...` chain.
fn field_nullish(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(other) => Some(bare_string(other)),
    }
}

/// `x ?? y ?? z` resolved as a raw `Value` reference (not yet type-checked) — the first key
/// that is present and non-null wins, REGARDLESS OF TYPE. The caller then type-checks only
/// that one resolved value; it must never fall through to the next key just because the
/// first-resolved value was the wrong type — see `parse_route_stops` step 3's comment for the
/// production-incident-class bug this exact mistake caused once already (an earlier draft of
/// this function chained `.and_then(Value::as_array).or_else(...)` per key, which silently
/// reads `routes` when `route_list` is present-but-wrong-type instead of correctly producing
/// no match at all, same as the reference).
fn pick_nullish_value<'a>(raw: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    for &k in keys {
        match raw.get(k) {
            None | Some(Value::Null) => continue,
            Some(v) => return Some(v),
        }
    }
    None
}

/// JS truthiness for `if (raw.sgi_route_name)`: false for null/missing/""/0/false, true otherwise.
fn is_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::Array(_)) | Some(Value::Object(_)) => true,
    }
}

/// Mirrors `pick()`: first key whose value is present, non-null, and non-empty-string.
fn pick_field(raw: &Value, keys: &[&str]) -> String {
    for &k in keys {
        let s = field_string(raw.get(k));
        if !s.is_empty() {
            return s;
        }
    }
    String::new()
}

pub fn parse_route_detail_list(raw: &Value) -> Vec<RouteNode> {
    let Some(rdl) = raw.get("route_detail_list").and_then(Value::as_array) else {
        return Vec::new();
    };
    if rdl.is_empty() {
        return Vec::new();
    }
    let mut nodes = Vec::new();
    for entry in rdl {
        let Some(node_list) = entry.get("node_info_list").and_then(Value::as_array) else {
            continue;
        };
        for n in node_list {
            let name = field_string(n.get("name"));
            if name.is_empty() {
                continue;
            }
            let addr = n.get("address_info");
            let province = addr.map(|a| field_string(a.get("l1"))).unwrap_or_default();
            let city = addr.map(|a| field_string(a.get("l2"))).unwrap_or_default();
            nodes.push(RouteNode { name, province, city });
        }
    }
    nodes
}

pub fn parse_route_stops(raw: &Value) -> Vec<String> {
    // 1. Pre-enriched stored array (highest priority). Returns unconditionally once the RAW
    // array is non-empty, even if every entry filters out to nothing (matches TS: the length
    // check is on the raw array, not the post-filter result).
    if let Some(arr) = raw.get("route_stops").and_then(Value::as_array) {
        if !arr.is_empty() {
            // Bare `String(x)`, per the reference (`.map(String)`, no `?? ''` first) — a
            // `null` entry stringifies to the literal `"null"`, not `""`.
            return arr
                .iter()
                .map(bare_string)
                .filter(|s| !s.is_empty())
                .collect();
        }
    }

    // 2. route_detail_list (BEST source — actual DC names).
    let rdl_nodes = parse_route_detail_list(raw);
    if !rdl_nodes.is_empty() {
        return rdl_nodes.into_iter().map(|n| n.name).collect();
    }

    // 3. route_list / routes / route array — resolve ONE value via `??` semantics first
    // (regardless of type), THEN type-check only that value. Do NOT check-and-fall-through
    // per key — a present-but-wrong-type `route_list` must produce no match here (falling
    // through to step 4), never silently read `routes` instead.
    let route_list_value = pick_nullish_value(raw, &["route_list", "routes", "route"]);
    if let Some(list) = route_list_value.and_then(Value::as_array) {
        if !list.is_empty() {
            // `dc_name ?? hub_name ?? name ?? location ?? ''` per entry (nullish-coalescing
            // chain, NOT "first non-empty").
            let stops: Vec<String> = list
                .iter()
                .map(|r| {
                    field_nullish(r.get("dc_name"))
                        .or_else(|| field_nullish(r.get("hub_name")))
                        .or_else(|| field_nullish(r.get("name")))
                        .or_else(|| field_nullish(r.get("location")))
                        .unwrap_or_default()
                })
                .filter(|s| !s.is_empty())
                .collect();
            if !stops.is_empty() {
                return stops;
            }
        }
    }

    // 4. SGI enriched route string (truthiness check, not just non-null).
    if is_truthy(raw.get("sgi_route_name")) {
        let s = field_string(raw.get("sgi_route_name"));
        return s.split(" -> ").filter(|p| !p.is_empty()).map(String::from).collect();
    }

    // 5. report_station_name (origin DC from bidding/list).
    let report_station = field_string(raw.get("report_station_name"));
    if !report_station.is_empty() {
        return vec![report_station];
    }

    // 6. Origin + destination DC names (`pick` semantics: skip null/undefined/empty-string).
    let o = pick_field(raw, &["origin_dc_name", "origin_hub", "from_dc_name", "origin_name"]);
    let d = pick_field(raw, &["dest_dc_name", "dest_hub", "to_dc_name", "dest_name"]);
    [o, d].into_iter().filter(|s| !s.is_empty()).collect()
}
```

- [ ] **Step 6: Run to verify it passes**

Run: `cargo test -p core-domain route_parse`
Expected: `test result: ok. 14 passed; 0 failed`. (12 from the reference port + 2 regression tests added during Task 4's review for the route_list-fallback and bare-stringify fixes — see the `parse_route_stops_tests` module's trailing tests in Step 5.)

- [ ] **Step 7: Wire into `lib.rs`**

Add `pub mod route_parse;` and `pub use route_parse::{parse_route_detail_list, parse_route_stops, RouteNode};`.

- [ ] **Step 8: Clippy**

Run: `cargo clippy -p core-domain -- -D warnings` — expected clean.

- [ ] **Step 9: Commit**

```bash
git add Backend/crates/core-domain/src/route_parse.rs Backend/crates/core-domain/src/lib.rs Backend/Cargo.toml Backend/Cargo.lock
git commit -m "feat(core-domain): port parseRouteStops/parseRouteDetailList SPX route parsing"
```

---

### Task 5: `rule.rs` part 1 — rule types + `sanitize_accept_rules`

**Files:**
- Create: `Backend/crates/core-domain/src/rule.rs`
- Modify: `Backend/crates/core-domain/src/lib.rs`

**Interfaces:**
- Consumes: `norm_loc` (Task 2), `norm_vehicle`/`canonical_rule_vehicle_label` (Task 3).
- Produces: `RuleMode` (`BookingId | Route | Filter`), `RouteMatchMode` (`Strict | Flexible`, default `Strict`), `RuleBookingType` (`Spxid | Reguler | All`, default `All`), `RuleConditions` (see field list below, derives `Debug, Clone, Default`), `AcceptRule { id, name, enabled, priority: i32, mode: RuleMode, conditions: RuleConditions }` (derives `Debug, Clone`), `MatchState { rule_accept_counts: HashMap<String, u32> }`, `RawAcceptRule`/`RawRuleConditions` (loosely-typed sanitizer input, see below), `RuleSanitizeResult { rules: Vec<AcceptRule>, warnings: Vec<String> }`, `pub fn sanitize_accept_rules(rules: &[RawAcceptRule]) -> RuleSanitizeResult`, `pub(crate) fn norm_id(s: &str) -> String`. Task 6 (`dedupe_rules`) and Task 7-10 (`matching.rs`) depend on all of these. A `#[cfg(test)] pub(crate) mod test_support` in `lib.rs` (added in this task) provides `mk_rule(mode, conditions) -> AcceptRule` and `mk_state() -> MatchState` helpers Task 6-10's tests reuse.

**Key type-design decision — `max_accept_count` is `u32`, not `Option<u32>`:** the reference `maxAcceptCount?: number` is read in two places (`matchesRule`'s cap check, `dedupeRules`' merge), and BOTH treat `undefined` and the literal number `0` identically as "unlimited" (`dedupeRules`' own comment: *"keep the most permissive cap (0 = unlimited wins)"*). Modeling this as `Option<u32>` would create a distinction (`None` vs `Some(0)`) the reference code never makes — use plain `u32` with `0` meaning unlimited, collapsing both TS states into one Rust state. This is **different** from `max_weight`/`max_cod_amount`, where `undefined` (no filter) and `Some(0.0)` (filter requires exactly 0) are semantically different in `matchesRule` (`c.maxWeight !== undefined && booking.weight > c.maxWeight`) — those stay `Option<f64>`.

- [ ] **Step 1: Read the reference source first**

Read `matching.ts` lines 7-44 (`AcceptRule` interface), 127-223 (`toInt`/`toOptionalNonNeg`/`uniqKeepOrder`/`sanitizeAcceptRules`), and the `sanitizeAcceptRules — professional save hygiene` describe block in `matching.test.ts` (lines 280-324).

- [ ] **Step 2: Write `rule.rs` types (no tests yet — pure data, nothing to assert beyond compiling)**

```rust
use std::collections::HashMap;

use crate::location::norm_loc;
use crate::vehicle::{canonical_rule_vehicle_label, norm_vehicle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleMode {
    BookingId,
    Route,
    Filter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RouteMatchMode {
    #[default]
    Strict,
    Flexible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuleBookingType {
    #[default]
    All,
    Spxid,
    Reguler,
}

#[derive(Debug, Clone, Default)]
pub struct RuleConditions {
    pub service_types: Vec<String>,
    pub max_weight: Option<f64>,
    pub coc_only: bool,
    pub non_coc_only: bool,
    pub max_cod_amount: Option<f64>,
    pub booking_ids: Vec<String>,
    pub origin: String,
    pub destinations: Vec<String>,
    pub booking_type: RuleBookingType,
    pub shift_types: Vec<i32>,
    pub trip_types: Vec<i32>,
    pub match_mode: RouteMatchMode,
    pub min_deadline_min: Option<u32>,
    /// 0 = unlimited. See this task's brief header for why this is `u32`, not `Option<u32>`.
    pub max_accept_count: u32,
    pub accepted_count: u32,
}

#[derive(Debug, Clone)]
pub struct AcceptRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub priority: i32,
    pub mode: RuleMode,
    pub conditions: RuleConditions,
}

#[derive(Debug, Clone, Default)]
pub struct MatchState {
    pub rule_accept_counts: HashMap<String, u32>,
}

/// Loosely-typed sanitizer input — mirrors the untrusted/partial shape `sanitizeAcceptRules`
/// accepts in TS (fields may be missing/malformed; this is the boundary that cleans them up
/// into a strict `AcceptRule`). Per-array-entry nullability (TS's defensive `v ?? ''` inside
/// `.map()`) is not modeled here: none of the reference sanitize tests exercise a null entry
/// inside an array, so `Vec<String>` (already-strings) is the faithful, simpler equivalent.
#[derive(Debug, Clone, Default)]
pub struct RawAcceptRule {
    pub id: Option<String>,
    pub name: Option<String>,
    pub enabled: bool,
    pub priority: Option<i64>,
    pub mode: Option<String>,
    pub conditions: RawRuleConditions,
}

#[derive(Debug, Clone, Default)]
pub struct RawRuleConditions {
    pub service_types: Vec<String>,
    pub max_weight: Option<f64>,
    pub coc_only: bool,
    pub non_coc_only: bool,
    pub max_cod_amount: Option<f64>,
    pub booking_ids: Vec<String>,
    pub origin: Option<String>,
    pub destinations: Vec<String>,
    pub booking_type: Option<String>,
    pub shift_types: Vec<i64>,
    pub trip_types: Vec<i64>,
    pub match_mode: Option<String>,
    pub min_deadline_min: Option<f64>,
    pub max_accept_count: Option<f64>,
    pub accepted_count: Option<i64>,
}

pub struct RuleSanitizeResult {
    pub rules: Vec<AcceptRule>,
    pub warnings: Vec<String>,
}
```

- [ ] **Step 3: Add `norm_id` + the sanitize helpers**

```rust
/// Separator-insensitive identity key: lowercase, strip everything but `[a-z0-9]`. Used to
/// dedupe booking-ids/lanes and — critically — reused verbatim by `matches_rule`'s booking_id
/// mode and `matched_booking_id_for` so the two can never disagree (see the module-level
/// warning in Task 7/10's brief about the historical production incident this prevents).
pub(crate) fn norm_id(s: &str) -> String {
    s.to_lowercase().chars().filter(char::is_ascii_alphanumeric).collect()
}

fn uniq_keep_order<F: Fn(&str) -> String>(values: &[String], norm: F) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for raw in values {
        let key = norm(raw);
        if key.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        out.push(raw.clone());
    }
    out
}

fn to_int(v: Option<f64>, fallback: i64) -> i64 {
    match v {
        Some(n) if n.is_finite() => n.trunc() as i64,
        _ => fallback,
    }
}

fn to_optional_non_neg(v: Option<f64>) -> Option<u32> {
    match v {
        Some(n) if n.is_finite() => Some(n.max(0.0).trunc() as u32),
        _ => None,
    }
}

/// Same truncate-and-clamp-to-non-negative semantics as `to_optional_non_neg`, but stays in
/// `f64` — used for `max_weight`/`max_cod_amount`, which must NOT be narrowed through `u32`
/// (a value above ~4.29 billion would silently saturate, corrupting a money-critical field).
/// (Added during Task 5's review: the original plan fed these two fields through the `u32`
/// version, which wouldn't compile against their `Option<f64>` field type — and narrowing to
/// fix the compile error would have introduced exactly the precision-loss bug this avoids.)
fn to_optional_non_neg_f64(v: Option<f64>) -> Option<f64> {
    match v {
        Some(n) if n.is_finite() => Some(n.max(0.0).trunc()),
        _ => None,
    }
}
```

- [ ] **Step 4: Write `sanitize_accept_rules`**

```rust
pub fn sanitize_accept_rules(rules: &[RawAcceptRule]) -> RuleSanitizeResult {
    let mut warnings = Vec::new();
    let mut out = Vec::with_capacity(rules.len());

    for (idx, raw) in rules.iter().enumerate() {
        let c = &raw.conditions;
        let mode = match raw.mode.as_deref() {
            Some("booking_id") => RuleMode::BookingId,
            Some("route") => RuleMode::Route,
            _ => RuleMode::Filter,
        };

        let id_trimmed = raw.id.as_deref().unwrap_or("").trim().to_string();
        let id = if id_trimmed.is_empty() { format!("rule_{}", idx + 1) } else { id_trimmed };

        let name_trimmed = raw.name.as_deref().unwrap_or("").trim().to_string();
        let name = if name_trimmed.is_empty() { format!("Rule {}", idx + 1) } else { name_trimmed };

        let raw_destinations: Vec<String> =
            c.destinations.iter().map(|v| v.trim().to_string()).filter(|s| !s.is_empty()).collect();

        let service_types_canon: Vec<String> = c
            .service_types
            .iter()
            .map(|v| canonical_rule_vehicle_label(v.trim()))
            .filter(|s| !s.is_empty())
            .collect();
        let service_types = uniq_keep_order(&service_types_canon, norm_vehicle);

        let booking_ids_trimmed: Vec<String> =
            c.booking_ids.iter().map(|v| v.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let booking_ids = uniq_keep_order(&booking_ids_trimmed, norm_id);

        let destinations_capped: Vec<String> = raw_destinations.iter().take(5).cloned().collect();
        let destinations = uniq_keep_order(&destinations_capped, norm_loc);

        let shift_types = dedup_nonneg_ints(&c.shift_types);
        let trip_types = dedup_nonneg_ints(&c.trip_types);

        let origin = c.origin.as_deref().unwrap_or("").trim().to_string();
        let coc_only = c.coc_only;
        let non_coc_only_raw = c.non_coc_only;

        let booking_type = match c.booking_type.as_deref() {
            Some("spxid") => RuleBookingType::Spxid,
            Some("reguler") => RuleBookingType::Reguler,
            _ => RuleBookingType::All,
        };
        let match_mode =
            if c.match_mode.as_deref() == Some("flexible") { RouteMatchMode::Flexible } else { RouteMatchMode::Strict };

        let max_weight = to_optional_non_neg_f64(c.max_weight);
        let max_cod_amount = to_optional_non_neg_f64(c.max_cod_amount);
        let min_deadline_min = to_optional_non_neg(c.min_deadline_min);
        let max_accept_count = to_optional_non_neg(c.max_accept_count).unwrap_or(0);
        let accepted_count = to_int(c.accepted_count.map(|x| x as f64), 0).max(0) as u32;
        let priority = to_int(raw.priority.map(|x| x as f64), 0).clamp(-999, 999) as i32;

        let mut sanitized = AcceptRule {
            id,
            name: name.clone(),
            enabled: raw.enabled,
            priority,
            mode,
            conditions: RuleConditions {
                service_types,
                max_weight,
                coc_only,
                non_coc_only: non_coc_only_raw,
                max_cod_amount,
                booking_ids: booking_ids.clone(),
                origin: origin.clone(),
                destinations: destinations.clone(),
                booking_type,
                shift_types,
                trip_types,
                match_mode,
                min_deadline_min,
                max_accept_count,
                accepted_count,
            },
        };

        if sanitized.mode == RuleMode::BookingId && booking_ids.is_empty() {
            warnings.push(format!("Rule \"{name}\" kosong: mode booking_id tanpa Booking ID"));
        }
        if sanitized.mode == RuleMode::Route && origin.is_empty() && destinations.is_empty() {
            warnings.push(format!("Rule \"{name}\" kosong: mode route tanpa origin/destinasi"));
        }
        if coc_only && non_coc_only_raw {
            sanitized.conditions.non_coc_only = false;
            warnings.push(format!("Rule \"{name}\" bentrok: COC dan Non aktif bersamaan, Non dimatikan"));
        }
        if raw_destinations.len() > 5 {
            warnings.push(format!("Rule \"{name}\" dipotong ke maksimum 5 destinasi"));
        }
        if let Some(raw_name) = &raw.name {
            if raw_name.trim() != name {
                warnings.push(format!("Rule \"{name}\" dirapikan: nama mengandung spasi berlebih"));
            }
        }

        out.push(sanitized);
    }

    RuleSanitizeResult { rules: out, warnings }
}

fn dedup_nonneg_ints(values: &[i64]) -> Vec<i32> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for &v in values {
        if v >= 0 && seen.insert(v) {
            out.push(v as i32);
        }
    }
    out
}
```

- [ ] **Step 5: Add the tests**

Append to `rule.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn raw_rule(mode: &str, conditions: RawRuleConditions) -> RawAcceptRule {
        RawAcceptRule { id: None, name: None, enabled: true, priority: None, mode: Some(mode.to_string()), conditions }
    }

    mod sanitize_accept_rules_tests {
        use super::*;

        #[test]
        fn route_rule_trimmed_canonicalized_capped_and_warns_on_overflow() {
            let rule = RawAcceptRule {
                id: Some(String::new()),
                name: Some("  Pekanbaru   ".to_string()),
                enabled: true,
                priority: None,
                mode: Some("route".to_string()),
                conditions: RawRuleConditions {
                    origin: Some("  Pekanbaru DC  ".to_string()),
                    destinations: vec![" Lampung DC ", "Cileungsi DC", "Cileungsi DC", "A", "B", "C"]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                    service_types: vec!["tronton", "TRONTON (10WH)", " fuso std "].into_iter().map(String::from).collect(),
                    ..Default::default()
                },
            };
            let result = sanitize_accept_rules(&[rule]);
            assert_eq!(result.rules[0].id, "rule_1");
            assert_eq!(result.rules[0].name, "Pekanbaru");
            assert_eq!(result.rules[0].conditions.origin, "Pekanbaru DC");
            assert_eq!(
                result.rules[0].conditions.destinations,
                vec!["Lampung DC", "Cileungsi DC", "A", "B"]
            );
            assert_eq!(result.rules[0].conditions.service_types, vec!["TRONTON", "FUSO"]);
            assert!(result.warnings.iter().any(|w| w.contains("maksimum 5 destinasi")));
        }

        #[test]
        fn conflicting_coc_flags_are_resolved_safely() {
            let rule = raw_rule(
                "filter",
                RawRuleConditions { coc_only: true, non_coc_only: true, ..Default::default() },
            );
            let result = sanitize_accept_rules(&[rule]);
            assert!(result.rules[0].conditions.coc_only);
            assert!(!result.rules[0].conditions.non_coc_only);
            assert!(result.warnings.iter().any(|w| w.contains("bentrok")));
        }

        #[test]
        fn booking_id_rule_without_ids_emits_warning() {
            let rule = raw_rule(
                "booking_id",
                RawRuleConditions { booking_ids: vec!["  ".to_string(), String::new()], ..Default::default() },
            );
            let result = sanitize_accept_rules(&[rule]);
            assert!(result.warnings.iter().any(|w| w.contains("tanpa Booking ID")));
        }
    }
}
```

- [ ] **Step 6: Add the `test_support` module to `lib.rs`**

Append to `Backend/crates/core-domain/src/lib.rs`:

```rust
#[cfg(test)]
pub(crate) mod test_support {
    use crate::rule::{AcceptRule, MatchState, RuleConditions, RuleMode};

    /// Mirrors the TS test helper `mkRule(mode, conditions, extra)`: build a minimal rule with
    /// sensible defaults (id "r1", name "test", enabled true, priority 0), then override
    /// specific fields with Rust struct-update syntax at the call site, e.g.
    /// `AcceptRule { priority: 9, id: "rt".into(), ..mk_rule(RuleMode::Route, conditions) }`.
    pub(crate) fn mk_rule(mode: RuleMode, conditions: RuleConditions) -> AcceptRule {
        AcceptRule { id: "r1".to_string(), name: "test".to_string(), enabled: true, priority: 0, mode, conditions }
    }

    pub(crate) fn mk_state() -> MatchState {
        MatchState::default()
    }
}
```

- [ ] **Step 7: Wire into `lib.rs`**

Add `pub mod rule;` and:

```rust
pub use rule::{
    sanitize_accept_rules, AcceptRule, MatchState, RawAcceptRule, RawRuleConditions, RouteMatchMode,
    RuleBookingType, RuleConditions, RuleMode, RuleSanitizeResult,
};
```

- [ ] **Step 8: Run to verify tests pass**

Run: `cargo test -p core-domain rule::`
Expected: `test result: ok. 3 passed; 0 failed`.

- [ ] **Step 9: Run the full workspace test suite once**

Run: `cargo test -p core-domain`
Expected: all prior tasks' tests still pass (7 + 7 + 11 + 12 + 3 = 40 passing so far), 0 failed.

- [ ] **Step 10: Clippy**

Run: `cargo clippy -p core-domain -- -D warnings` — expected clean.

- [ ] **Step 11: Commit**

```bash
git add Backend/crates/core-domain/src/rule.rs Backend/crates/core-domain/src/lib.rs
git commit -m "feat(core-domain): port AcceptRule types + sanitizeAcceptRules"
```

---

### Task 6: `rule.rs` part 2 — `dedupe_rules`

**Files:**
- Modify: `Backend/crates/core-domain/src/rule.rs` (add function + tests only — do not touch Task 5's types or `sanitize_accept_rules`)

**Interfaces:**
- Consumes: `AcceptRule`, `RuleMode`, `norm_id`, `norm_loc` from Task 5/2.
- Produces: `pub fn dedupe_rules(rules: &[AcceptRule]) -> Vec<AcceptRule>`. Not consumed by other Fase-1 tasks (it's a standalone save-time hygiene pass, tested in isolation) but is part of `matching.ts`'s required public surface per the master spec.

**Key translation note:** the reference `idKeep` is a `Map<AcceptRule, string[]>` keyed by **object identity** — TS can do this because objects are reference types. Rust has no equivalent for a plain struct; key by the rule's **index in the input slice** instead (`HashMap<usize, Vec<String>>`) — behaviorally identical since each input rule is visited exactly once by index either way.

- [ ] **Step 1: Read the reference source first**

Read `matching.ts` lines 388-460 (`normId`... wait, `norm_id` and `dedupKeepOrder` through `dedupeRules`) and both `dedupeRules` describe blocks in `matching.test.ts`: "anti-duplikasi" (lines 248-278) and "klaim booking_id memprioritaskan rule ENABLED (C1)" (lines 463-492).

- [ ] **Step 2: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `rule.rs` (inside the existing `mod tests { ... }`, alongside `sanitize_accept_rules_tests`) — reference `dedupe_rules`, which doesn't exist yet:

```rust
    mod dedupe_rules_tests {
        use super::*;

        fn route_rule(id: &str, origin: &str, dests: &[&str]) -> AcceptRule {
            AcceptRule {
                id: id.to_string(),
                name: id.to_string(),
                enabled: true,
                priority: 0,
                mode: RuleMode::Route,
                conditions: RuleConditions {
                    origin: origin.to_string(),
                    destinations: dests.iter().map(|s| s.to_string()).collect(),
                    ..Default::default()
                },
            }
        }

        fn route_rule_capped(id: &str, origin: &str, dests: &[&str], max_accept_count: u32, accepted_count: u32) -> AcceptRule {
            let mut r = route_rule(id, origin, dests);
            r.conditions.max_accept_count = max_accept_count;
            r.conditions.accepted_count = accepted_count;
            r
        }

        fn bkid_rule(id: &str, ids: &[&str], enabled: bool) -> AcceptRule {
            AcceptRule {
                id: id.to_string(),
                name: id.to_string(),
                enabled,
                priority: 0,
                mode: RuleMode::BookingId,
                conditions: RuleConditions {
                    booking_ids: ids.iter().map(|s| s.to_string()).collect(),
                    ..Default::default()
                },
            }
        }

        #[test]
        fn same_lane_entered_3x_collapses_to_1() {
            let out = dedupe_rules(&[
                route_rule("a", "Padang DC", &["Cileungsi DC"]),
                route_rule("b", "Padang DC", &["Cileungsi DC"]),
                route_rule("c", "Padang DC", &["Cileungsi DC"]),
            ]);
            assert_eq!(out.len(), 1);
        }

        #[test]
        fn separator_variant_of_same_lane_still_collapses() {
            let out = dedupe_rules(&[
                route_rule("a", "Padang DC", &["Cileungsi DC"]),
                route_rule("b", "Padang-DC", &["Cileungsi_DC"]),
            ]);
            assert_eq!(out.len(), 1);
        }

        #[test]
        fn different_lanes_are_kept() {
            let out = dedupe_rules(&[
                route_rule("a", "Padang DC", &["Cileungsi DC"]),
                route_rule("b", "Aceh DC", &["Cileungsi DC"]),
            ]);
            assert_eq!(out.len(), 2);
        }

        #[test]
        fn collapse_keeps_most_permissive_cap_and_higher_accepted_count() {
            let out = dedupe_rules(&[
                route_rule_capped("a", "Padang DC", &["Cileungsi DC"], 1, 1),
                route_rule_capped("b", "Padang DC", &["Cileungsi DC"], 5, 0),
            ]);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].conditions.max_accept_count, 5);
            assert_eq!(out[0].conditions.accepted_count, 1);
        }

        #[test]
        fn booking_id_repeated_within_and_across_rules_deduped() {
            let out = dedupe_rules(&[
                bkid_rule("a", &["SPXID_VM_001397509", "SPXID VM 001397509"], true),
                bkid_rule("b", &["SPXID_VM_001397509"], true),
            ]);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].conditions.booking_ids, vec!["SPXID_VM_001397509"]);
        }

        #[test]
        fn disabled_rule_entered_earlier_does_not_steal_id_from_enabled_rule_later_c1() {
            let out = dedupe_rules(&[
                bkid_rule("old", &["SPXID_VM_001402220"], false),
                bkid_rule("new", &["SPXID_VM_001402220"], true),
            ]);
            let ids: Vec<&str> = out.iter().map(|r| r.id.as_str()).collect();
            assert_eq!(ids, vec!["new"]);
            assert_eq!(out[0].conditions.booking_ids, vec!["SPXID_VM_001402220"]);
        }

        #[test]
        fn two_enabled_rules_share_id_earlier_one_wins() {
            let out = dedupe_rules(&[
                bkid_rule("a", &["SPXID_VM_001402220", "SPXID_VM_001402221"], true),
                bkid_rule("b", &["SPXID VM 001402220"], true),
            ]);
            let ids: Vec<&str> = out.iter().map(|r| r.id.as_str()).collect();
            assert_eq!(ids, vec!["a"]);
            assert_eq!(out[0].conditions.booking_ids.len(), 2);
        }

        #[test]
        fn unique_disabled_rule_is_kept_not_a_duplicate_of_anyone() {
            let out = dedupe_rules(&[bkid_rule("solo", &["SPXID_VM_001999999"], false)]);
            let ids: Vec<&str> = out.iter().map(|r| r.id.as_str()).collect();
            assert_eq!(ids, vec!["solo"]);
        }

        #[test]
        fn sanitize_then_dedupe_chain_drops_empty_booking_id_rule() {
            let raw = raw_rule("booking_id", RawRuleConditions { booking_ids: vec![], ..Default::default() });
            let sanitized = sanitize_accept_rules(&[raw]);
            assert_eq!(dedupe_rules(&sanitized.rules).len(), 0);
        }
    }
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p core-domain rule::tests::dedupe_rules_tests`
Expected: FAIL — compile error, `cannot find function \`dedupe_rules\``.

- [ ] **Step 4: Implement**

Add above the `#[cfg(test)]` block in `rule.rs` (after `sanitize_accept_rules` and its helpers):

```rust
fn dedup_keep_order<F: Fn(&str) -> String>(items: &[String], key: F) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for it in items {
        let k = key(it);
        if k.is_empty() || seen.contains(&k) {
            continue;
        }
        seen.insert(k);
        out.push(it.clone());
    }
    out
}

/// Collapse rules that target the SAME thing (operators re-enter the same lane / booking-id
/// many times). Run on every save so duplicates can never accumulate.
pub fn dedupe_rules(rules: &[AcceptRule]) -> Vec<AcceptRule> {
    // Claim booking-ids ENABLED-first, then input order within a status: a disabled rule
    // entered earlier must not steal an id from an enabled rule entered later (C1 — the id
    // would silently vanish from the active rule on save otherwise).
    let mut claimed_id: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut id_keep: HashMap<usize, Vec<String>> = HashMap::new();
    for &want_enabled in &[true, false] {
        for (idx, r) in rules.iter().enumerate() {
            if r.mode != RuleMode::BookingId || r.enabled != want_enabled {
                continue;
            }
            let mut keep = Vec::new();
            for raw in &r.conditions.booking_ids {
                let n = norm_id(raw);
                if n.is_empty() || claimed_id.contains(&n) {
                    continue;
                }
                claimed_id.insert(n);
                keep.push(raw.clone());
            }
            id_keep.insert(idx, keep);
        }
    }

    let mut out: Vec<AcceptRule> = Vec::new();
    let mut route_at: HashMap<String, usize> = HashMap::new();

    for (idx, r) in rules.iter().enumerate() {
        let c = &r.conditions;

        if r.mode == RuleMode::Route {
            let dests_sig: String =
                c.destinations.iter().map(|d| norm_loc(d)).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(">");
            let service_types_sig: String = {
                let mut v: Vec<String> =
                    c.service_types.iter().map(|s| s.to_lowercase().trim().to_string()).filter(|s| !s.is_empty()).collect();
                v.sort();
                v.join(",")
            };
            let mode_str = match c.match_mode { RouteMatchMode::Flexible => "flexible", RouteMatchMode::Strict => "strict" };
            let booking_type_str = match c.booking_type {
                RuleBookingType::Spxid => "spxid",
                RuleBookingType::Reguler => "reguler",
                RuleBookingType::All => "all",
            };
            let sig = format!("{}|{}|{}|{}|{}", norm_loc(&c.origin), dests_sig, mode_str, booking_type_str, service_types_sig);

            if let Some(&at) = route_at.get(&sig) {
                // Same lane already present → MERGE, never silently shrink capacity or lose
                // progress: keep the most permissive cap (0 = unlimited wins), the higher
                // accepted_count, enabled if either side is.
                let a = out[at].conditions.max_accept_count;
                let b = c.max_accept_count;
                out[at].conditions.max_accept_count = if a == 0 || b == 0 { 0 } else { a.max(b) };
                out[at].conditions.accepted_count = out[at].conditions.accepted_count.max(c.accepted_count);
                out[at].enabled = out[at].enabled || r.enabled;
                continue;
            }

            route_at.insert(sig, out.len());
            let mut merged = r.clone();
            merged.conditions.destinations = dedup_keep_order(&c.destinations, norm_loc);
            out.push(merged);
            continue;
        }

        if r.mode == RuleMode::BookingId {
            let ids = id_keep.get(&idx).cloned().unwrap_or_default();
            if !ids.is_empty() {
                let mut merged = r.clone();
                merged.conditions.booking_ids = ids;
                out.push(merged);
            }
            continue;
        }

        out.push(r.clone()); // filter / other modes: untouched
    }

    out
}
```

Add `use std::collections::HashMap;` at the top of `rule.rs` if not already present from Task 5.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p core-domain rule::tests::dedupe_rules_tests`
Expected: `test result: ok. 9 passed; 0 failed`.

- [ ] **Step 6: Run the full crate suite once**

Run: `cargo test -p core-domain`
Expected: 40 (prior) + 9 = 49 passing, 0 failed.

- [ ] **Step 7: Wire into `lib.rs`**

Add `dedupe_rules` to the existing `pub use rule::{...}` line in `lib.rs`.

- [ ] **Step 8: Clippy**

Run: `cargo clippy -p core-domain -- -D warnings` — expected clean.

- [ ] **Step 9: Commit**

```bash
git add Backend/crates/core-domain/src/rule.rs Backend/crates/core-domain/src/lib.rs
git commit -m "feat(core-domain): port dedupeRules anti-duplication pass"
```

---

### Task 7: `matching.rs` part 1 — `CompiledRule`, ranking, `matches()` scaffold + booking_id mode

**Files:**
- Create: `Backend/crates/core-domain/src/matching.rs`
- Modify: `Backend/crates/core-domain/src/lib.rs`

**Interfaces:**
- Consumes: `Booking`/`BookingType` (Task 1), `loc_match_normalized` (Task 2), `vehicle_match_normalized`/`norm_vehicle` (Task 3), `AcceptRule`/`RuleMode`/`RouteMatchMode`/`RuleBookingType`/`RuleConditions`/`MatchState`/`norm_id` (Task 5/6), `mk_rule`/`mk_state` test helpers (Task 5).
- Produces: `RuleRank([i32; 6])` (derives `Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord` — array-tuple lexicographic comparison IS the ranking algorithm, see note below), `pub struct CompiledRule { pub id: String, pub name: String, pub enabled: bool, pub priority: i32, pub mode: RuleMode, pub conditions: RuleConditions, /* private precomputed fields */ }`, `impl CompiledRule { pub fn compile(rule: &AcceptRule) -> Self; pub fn matches(&self, booking: &Booking, state: &MatchState) -> bool; pub fn rank(&self) -> RuleRank }`, plus the private `matches_booking_id` method this task implements (route/filter mode methods are stubbed to `unimplemented!()` here and filled in by Task 8/9 — this task's own tests only exercise booking_id mode, the cap guard, and the disabled-rule guard, so the stub is never hit by this task's own test run). `mk_booking` test helper (added to `lib.rs`'s `test_support` this task, since Task 1-6 didn't need it).

**Why ranking is a derived `Ord` on a `[i32; 6]`, not a custom comparator:** the reference `compareRuleRank` does index-by-index tuple comparison, first difference wins — this is *exactly* what `#[derive(PartialOrd, Ord)]` on a fixed-size array produces for free. `mode dominance > priority > specificity` (CP-6) falls out automatically from array index order: `[mode_score, priority, dest_count, has_origin, is_strict, service_type_count]` — a booking_id rule's `mode_score=3` always outranks a route rule's `mode_score=2` regardless of what follows, because the first array element is compared first.

- [ ] **Step 1: Read the reference source first**

Read `matching.ts` lines 63-88 (`ruleRank`/`compareRuleRank`), 225-252 (`matchesRule` header through the booking_id branch), and these `matching.test.ts` blocks: "matchesRule — maxAccept cap" (76-89), "matchesRule — booking_id mode" (140-164), "matchesRule — guards" (326-331), plus the CP-6 tests in "Phase-1: booking_id dominance + empty-filter safety" (334-345, 347-351).

- [ ] **Step 2: Write the failing tests**

Create `Backend/crates/core-domain/src/matching.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{RuleConditions, RuleMode};
    use crate::test_support::{mk_booking, mk_rule, mk_state};

    mod max_accept_cap {
        use super::*;

        #[test]
        fn cap_reached_via_persisted_accepted_count_is_false() {
            let mut conditions = RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() };
            conditions.max_accept_count = 1;
            conditions.accepted_count = 1;
            let r = mk_rule(RuleMode::Route, conditions);
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn cap_reached_via_in_flight_rule_accept_counts_is_false() {
            let mut conditions = RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() };
            conditions.max_accept_count = 2;
            conditions.accepted_count = 1;
            let r = mk_rule(RuleMode::Route, conditions);
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            let mut state = mk_state();
            state.rule_accept_counts.insert(r.id.clone(), 1);
            assert!(!CompiledRule::compile(&r).matches(&b, &state));
        }

        #[test]
        fn under_cap_still_matches() {
            let mut conditions = RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() };
            conditions.max_accept_count = 2;
            let r = mk_rule(RuleMode::Route, conditions);
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }
    }

    mod booking_id_mode {
        use super::*;

        #[test]
        fn exact_spx_tx_id_match_is_true() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["SPXID_VM_001396561".into()], ..Default::default() });
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_001396561".into();
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn short_partial_id_under_9_chars_does_not_substring_match() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["12345".into()], ..Default::default() });
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_12345_VM".into();
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn full_numeric_id_9_or_more_chars_substring_matches() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["001396561".into()], ..Default::default() });
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_001396561".into();
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn empty_booking_ids_is_false() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions::default());
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "X".into();
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn separator_tolerant_spaces_in_pasted_id_still_match_underscore_booking_name() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["SPXID VM 001397509".into()], ..Default::default() });
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_001397509".into();
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn separator_tolerant_stray_underscore_space_still_matches() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["SPXID_ VM_001397492C".into()], ..Default::default() });
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_001397492C".into();
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }
    }

    mod guards {
        use super::*;

        #[test]
        fn disabled_rule_never_matches() {
            let r = AcceptRule {
                enabled: false,
                ..mk_rule(RuleMode::Route, RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() })
            };
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }
    }

    mod cp6_ranking {
        use super::*;

        #[test]
        fn exact_booking_id_rule_beats_higher_priority_route_rule_on_same_ticket() {
            let mut b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            b.booking_id = "BKID12345678".into();
            b.spx_tx_id = "BKID12345678".into();
            let bkid = AcceptRule {
                id: "bk".into(),
                priority: 0,
                ..mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["BKID12345678".into()], ..Default::default() })
            };
            let route = AcceptRule {
                id: "rt".into(),
                priority: 9,
                ..mk_rule(RuleMode::Route, RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() })
            };
            let best = find_best_matching_rule(&b, &[route, bkid], &mk_state());
            let best = best.expect("expected a match");
            assert_eq!(best.id, "bk");
            assert_eq!(best.mode, RuleMode::BookingId);
        }

        #[test]
        fn among_two_route_rules_higher_priority_still_wins() {
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            let conditions = || RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() };
            let lo = AcceptRule { id: "lo".into(), priority: 1, ..mk_rule(RuleMode::Route, conditions()) };
            let hi = AcceptRule { id: "hi".into(), priority: 5, ..mk_rule(RuleMode::Route, conditions()) };
            let best = find_best_matching_rule(&b, &[lo, hi], &mk_state()).expect("expected a match");
            assert_eq!(best.id, "hi");
        }
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p core-domain matching`
Expected: FAIL — compile error, `CompiledRule`/`find_best_matching_rule`/`mk_booking` don't exist yet.

- [ ] **Step 4: Implement — `RuleRank` and `CompiledRule` skeleton**

Prepend above the test module in `matching.rs`:

```rust
use crate::booking::{Booking, BookingType};
use crate::location::loc_match_normalized;
use crate::rule::{norm_id, AcceptRule, MatchState, RuleBookingType, RuleConditions, RuleMode, RouteMatchMode};
use crate::vehicle::{norm_vehicle, vehicle_match_normalized};

/// `[mode_score, priority, dest_count, has_origin, is_strict, service_type_count]`. Derived
/// `Ord` gives exactly the reference `compareRuleRank`'s tuple comparison: first differing
/// element decides, higher wins — mode dominance beats priority beats specificity (CP-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuleRank([i32; 6]);

fn rule_rank(rule: &AcceptRule) -> RuleRank {
    let c = &rule.conditions;
    let mode_score = match rule.mode {
        RuleMode::BookingId => 3,
        RuleMode::Route => 2,
        RuleMode::Filter => 1,
    };
    let is_route = rule.mode == RuleMode::Route;
    let dest_count = if is_route {
        c.destinations.iter().map(|d| d.trim()).filter(|d| !d.is_empty()).count() as i32
    } else {
        0
    };
    let has_origin = i32::from(is_route && !c.origin.trim().is_empty());
    let is_strict = i32::from(is_route && c.match_mode == RouteMatchMode::Strict);
    let service_type_count = c.service_types.len() as i32;
    RuleRank([mode_score, rule.priority, dest_count, has_origin, is_strict, service_type_count])
}

pub struct CompiledRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub priority: i32,
    pub mode: RuleMode,
    pub conditions: RuleConditions,
    rank: RuleRank,
    // Precomputed once at compile() time, not re-derived per booking:
    origin_norm: String,
    destinations_norm: Vec<String>,
    service_types_norm: Vec<String>,
    booking_ids_norm: Vec<String>,
}

impl CompiledRule {
    pub fn compile(rule: &AcceptRule) -> Self {
        use crate::location::norm_loc;

        let origin_trimmed = rule.conditions.origin.trim();
        let destinations_norm: Vec<String> = rule
            .conditions
            .destinations
            .iter()
            .map(|d| d.trim())
            .filter(|d| !d.is_empty())
            .map(norm_loc)
            .collect();
        let service_types_norm: Vec<String> = rule.conditions.service_types.iter().map(|s| norm_vehicle(s)).collect();
        let booking_ids_norm: Vec<String> =
            rule.conditions.booking_ids.iter().map(|s| norm_id(s)).filter(|s| !s.is_empty()).collect();

        CompiledRule {
            id: rule.id.clone(),
            name: rule.name.clone(),
            enabled: rule.enabled,
            priority: rule.priority,
            mode: rule.mode,
            conditions: rule.conditions.clone(),
            rank: rule_rank(rule),
            origin_norm: norm_loc(origin_trimmed),
            destinations_norm,
            service_types_norm,
            booking_ids_norm,
        }
    }

    pub fn rank(&self) -> RuleRank {
        self.rank
    }

    pub fn matches(&self, booking: &Booking, state: &MatchState) -> bool {
        if !self.enabled {
            return false;
        }
        let c = &self.conditions;

        if c.max_accept_count > 0 {
            let used = c.accepted_count + state.rule_accept_counts.get(&self.id).copied().unwrap_or(0);
            if used >= c.max_accept_count {
                return false;
            }
        }

        if self.mode == RuleMode::BookingId {
            return self.matches_booking_id(booking);
        }

        if !c.shift_types.is_empty() && !c.shift_types.contains(&booking.shift_type) {
            return false;
        }
        if !c.trip_types.is_empty() && !c.trip_types.contains(&booking.trip_type) {
            return false;
        }

        match self.mode {
            RuleMode::Route => self.matches_route(booking),   // implemented in Task 8
            RuleMode::Filter => self.matches_filter(booking), // implemented in Task 9
            RuleMode::BookingId => unreachable!("handled above"),
        }
    }

    fn matches_booking_id(&self, booking: &Booking) -> bool {
        if self.booking_ids_norm.is_empty() {
            return false;
        }
        let tx = norm_id(&booking.spx_tx_id);
        let bk = norm_id(&booking.booking_id);
        let rq = norm_id(&booking.request_id);
        self.booking_ids_norm
            .iter()
            .any(|id| tx == *id || bk == *id || rq == *id || (id.len() >= 9 && tx.contains(id.as_str())))
    }

    // Task 8 fills this in (strict + flexible route matching, ordered destination walk, guards).
    fn matches_route(&self, _booking: &Booking) -> bool {
        unimplemented!("implemented in Task 8")
    }

    // Task 9 fills this in (filter-mode conditions + CP-4 empty-filter guard).
    fn matches_filter(&self, _booking: &Booking) -> bool {
        unimplemented!("implemented in Task 9")
    }
}

/// Convenience wrapper matching the reference `matchesRule(booking, rule, state)` signature
/// exactly, for callers/tests that don't need to reuse a compiled rule across many bookings.
/// The real hot path compiles once via `CompiledRule::compile` and calls `.matches()` directly.
pub fn matches_rule(booking: &Booking, rule: &AcceptRule, state: &MatchState) -> bool {
    CompiledRule::compile(rule).matches(booking, state)
}

/// Compiles every candidate and returns the highest-ranked match, or `None`. Task 10 is where
/// this gets its own dedicated ranking/overlap tests beyond this task's CP-6 smoke tests.
pub fn find_best_matching_rule(booking: &Booking, rules: &[AcceptRule], state: &MatchState) -> Option<CompiledRule> {
    let mut best: Option<CompiledRule> = None;
    for rule in rules {
        let compiled = CompiledRule::compile(rule);
        if !compiled.matches(booking, state) {
            continue;
        }
        best = match best {
            None => Some(compiled),
            Some(b) => Some(if compiled.rank() > b.rank() { compiled } else { b }),
        };
    }
    best
}
```

- [ ] **Step 5: Add the `mk_booking` test helper to `lib.rs`**

Add to the existing `#[cfg(test)] pub(crate) mod test_support` block in `lib.rs`:

```rust
    use crate::booking::Booking;

    /// Mirrors the TS test helper `mkBooking(routeStops, extra)`: build a booking with the
    /// given stops and every other field at its zero value, then override specific fields with
    /// Rust struct-update syntax at the call site, e.g. `Booking { spx_tx_id: "X".into(), ..mk_booking(&[]) }`.
    pub(crate) fn mk_booking(route_stops: &[&str]) -> Booking {
        Booking { route_stops: route_stops.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }
```

- [ ] **Step 6: Run to verify the tests this task owns pass**

Run: `cargo test -p core-domain matching::tests::max_accept_cap matching::tests::booking_id_mode matching::tests::guards matching::tests::cp6_ranking`
Expected: `3 + 6 + 1 + 2 = 12` tests pass. (The crate will not fully build-and-test as a whole yet — `matches_route`/`matches_filter` are `unimplemented!()` stubs, which is fine: they compile, and nothing in THIS task's test set calls a route/filter-mode rule, so no test panics. `cargo test -p core-domain` as a full run is deferred to Task 9's end, once both stubs are filled in.)

- [ ] **Step 7: Wire into `lib.rs`**

Add `pub mod matching;` and `pub use matching::{find_best_matching_rule, matches_rule, CompiledRule, RuleRank};`.

- [ ] **Step 8: Clippy**

Run: `cargo clippy -p core-domain -- -D warnings`
Expected: clean — the two `unimplemented!()` stubs do not trigger clippy warnings (unreachable in this task's own tests, and `unimplemented!` is an accepted marker for "next task fills this in," not dead code).

- [ ] **Step 9: Commit**

```bash
git add Backend/crates/core-domain/src/matching.rs Backend/crates/core-domain/src/lib.rs
git commit -m "feat(core-domain): CompiledRule scaffold, ranking (CP-6), booking_id mode matching"
```

---

### Task 8: `matching.rs` part 2 — route mode (strict + flexible + real production lanes)

**Files:**
- Modify: `Backend/crates/core-domain/src/matching.rs` (fill in `matches_route`, add a new `mod route_mode_tests` inside the existing `#[cfg(test)] mod tests` block — do not touch `matches_filter`, still a Task-9 stub)

**Interfaces:**
- Consumes: everything from Task 7 (`CompiledRule`, its precomputed `origin_norm`/`destinations_norm`, `loc_match_normalized`, `vehicle_match_normalized`).
- Produces: a working `CompiledRule::matches_route`. Nothing new is exposed publicly — `matches`/`matches_rule`/`find_best_matching_rule` already route to this internally once it's filled in.

This is the single largest and most failure-prone piece of the whole port — an ordered, whole-word, optionally-flexible multi-stop walk. Read the "route mode" section of this plan's design doc before starting if anything below is unclear; the algorithm has been hand-traced against 6 of the reference tests during planning (documented in the design doc) and is correct as specified — implement it as written, do not "simplify."

- [ ] **Step 1: Read the reference source first**

Read `matching.ts` lines 259-341 (the full route-mode branch of `matchesRule`) and these `matching.test.ts` blocks in full: "matchesRule — route mode" (29-74), "matchesRule — REAL lane: Aceh DC → Cileungsi DC" (111-138), "matchesRule — shift/trip targeting" (166-188), "matchesRule — flexible superset strict (F2)" (495-520), "Tipe Kendaraan kosong berarti semua jenis" (522-532), "flexible multi-destinasi" (534-549), "REAL lane: Kosambi DC → Mataram DC → Mataram 2 DC" (371-427).

- [ ] **Step 2: Write the failing tests**

Add a new `mod route_mode_tests { ... }` inside the existing `#[cfg(test)] mod tests { ... }` block in `matching.rs` (alongside `max_accept_cap`, `booking_id_mode`, `guards`, `cp6_ranking` from Task 7 — do not remove or restructure those):

```rust
    mod route_mode_tests {
        use super::*;

        fn route(origin: &str, dests: &[&str]) -> RuleConditions {
            RuleConditions { origin: origin.into(), destinations: dests.iter().map(|s| s.to_string()).collect(), ..Default::default() }
        }

        #[test]
        fn origin_and_dest_match_in_order_is_true() {
            let r = mk_rule(RuleMode::Route, route("Padang DC", &["Cileungsi DC"]));
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn bali_origin_does_not_sweep_balikpapan_dc_route() {
            let conditions = RuleConditions { origin: "bali".into(), destinations: vec![], booking_type: RuleBookingType::All, service_types: vec!["x".into()], ..Default::default() };
            let r = mk_rule(RuleMode::Route, conditions);
            let b = mk_booking(&["Balikpapan DC", "Pontianak DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn wrong_destination_is_false() {
            let r = mk_rule(RuleMode::Route, route("Padang DC", &["Cileungsi DC"]));
            let b = mk_booking(&["Padang DC", "Surabaya DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn strict_order_enforced_dest_must_come_after_origin() {
            let r = mk_rule(RuleMode::Route, route("Padang DC", &["Cileungsi DC"]));
            let b = mk_booking(&["Cileungsi DC", "Padang DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn flexible_mode_only_endpoint_must_match_intermediate_hubs_ignored() {
            let mut conditions = route("Pekanbaru DC", &["Cileungsi DC"]);
            conditions.match_mode = RouteMatchMode::Flexible;
            let r = mk_rule(RuleMode::Route, conditions);
            let b = mk_booking(&["Pekanbaru DC", "Palembang DC", "Cileungsi DC"]);
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn empty_route_rule_no_origin_dest_filter_matches_nothing() {
            let r = mk_rule(RuleMode::Route, route("", &[]));
            let b = mk_booking(&["Anywhere DC", "Else DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn origin_can_match_report_station_name_even_when_stops_are_partial() {
            let r = mk_rule(RuleMode::Route, route("Padang DC", &["Cileungsi DC"]));
            let mut b = mk_booking(&["Cileungsi DC"]);
            b.report_station = "Padang DC".into();
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn origin_must_not_fall_back_to_province_region_labels() {
            // report_station is deliberately left empty and originRegion/originProvince (not
            // modeled on Booking at all — see Task 1's design note) are absent — the matcher
            // must reject on the actual stop name alone, proving those TS fields are unread.
            let r = mk_rule(RuleMode::Route, route("Padang DC", &["Cileungsi DC"]));
            let b = mk_booking(&["Bukittinggi DC", "Cileungsi DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn final_destination_must_be_an_actual_route_stop_not_only_dest_region() {
            let r = mk_rule(RuleMode::Route, route("Padang DC", &["Cileungsi DC"]));
            let b = mk_booking(&["Padang DC", "Bekasi DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }
    }

    mod shift_trip_targeting {
        use super::*;

        #[test]
        fn no_shift_trip_condition_unaffected_matches() {
            let mut b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            b.shift_type = 1;
            let r = mk_rule(RuleMode::Route, RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() });
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn shift_types_filter_matches_when_booking_shift_in_list() {
            let mut b = mk_booking(&["Padang DC", "Cileungsi DC"]);
            b.shift_type = 1;
            let r = mk_rule(RuleMode::Route, RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], shift_types: vec![1, 2], ..Default::default() });
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn shift_types_filter_rejects_when_booking_shift_not_in_list() {
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]); // shift_type defaults to 0
            let r = mk_rule(RuleMode::Route, RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], shift_types: vec![2], ..Default::default() });
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn trip_types_filter_rejects_when_booking_trip_not_in_list() {
            let b = mk_booking(&["Padang DC", "Cileungsi DC"]); // trip_type defaults to 0
            let r = mk_rule(RuleMode::Route, RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], trip_types: vec![1], ..Default::default() });
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn shift_and_trip_both_required_both_must_match() {
            let r = mk_rule(RuleMode::Route, RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], shift_types: vec![1], trip_types: vec![2], ..Default::default() });
            let compiled = CompiledRule::compile(&r);
            let mut ok = mk_booking(&["Padang DC", "Cileungsi DC"]);
            ok.shift_type = 1;
            ok.trip_type = 2;
            assert!(compiled.matches(&ok, &mk_state()));
            let mut bad = mk_booking(&["Padang DC", "Cileungsi DC"]);
            bad.shift_type = 1;
            bad.trip_type = 0;
            assert!(!compiled.matches(&bad, &mk_state()));
        }
    }

    // REAL production lane — booking 5996405, SPXID_VM_001397649: Origin "Aceh DC" → Dest1
    // "Cileungsi DC", strict, TRONTON, COC.
    mod real_lane_aceh_to_cileungsi {
        use super::*;

        fn aceh_rule() -> AcceptRule {
            mk_rule(
                RuleMode::Route,
                RuleConditions {
                    origin: "Aceh DC".into(),
                    destinations: vec!["Cileungsi DC".into()],
                    match_mode: RouteMatchMode::Strict,
                    booking_type: RuleBookingType::Spxid,
                    service_types: vec!["TRONTON".into()],
                    coc_only: true,
                    ..Default::default()
                },
            )
        }

        fn real_booking(stops: &[&str]) -> Booking {
            let mut b = mk_booking(stops);
            b.booking_type = BookingType::Spxid;
            b.vehicle_type = "TRONTON (10WH)".into();
            b
        }

        #[test]
        fn the_real_ticket_5996405_matches_the_rule() {
            let compiled = CompiledRule::compile(&aceh_rule());
            assert!(compiled.matches(&real_booking(&["Aceh DC", "Cileungsi DC"]), &mk_state()));
        }

        #[test]
        fn tronton_10wh_vehicle_satisfies_service_type_tronton_suffix_tolerated() {
            let compiled = CompiledRule::compile(&aceh_rule());
            let mut b = real_booking(&["Aceh DC", "Cileungsi DC"]);
            b.vehicle_type = "TRONTON (10WH)".into();
            assert!(compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn different_origin_multi_hop_does_not_match_strict_aceh_rule() {
            let compiled = CompiledRule::compile(&aceh_rule());
            let b = real_booking(&["Tegal 2 DC", "Bekasi DC", "Cileungsi DC"]);
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn a_reguler_non_spxid_aceh_to_cileungsi_ticket_is_rejected_by_coc_only() {
            let compiled = CompiledRule::compile(&aceh_rule());
            let mut b = real_booking(&["Aceh DC", "Cileungsi DC"]);
            b.booking_type = BookingType::Reguler;
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn flexible_variant_catches_any_origin_to_cileungsi_endpoint() {
            let r = mk_rule(
                RuleMode::Route,
                RuleConditions {
                    destinations: vec!["Cileungsi DC".into()],
                    match_mode: RouteMatchMode::Flexible,
                    booking_type: RuleBookingType::Spxid,
                    service_types: vec!["TRONTON".into()],
                    coc_only: true,
                    ..Default::default()
                },
            );
            let compiled = CompiledRule::compile(&r);
            let b = real_booking(&["Tegal 2 DC", "Bekasi DC", "Cileungsi DC"]);
            assert!(compiled.matches(&b, &mk_state()));
        }
    }

    // REAL production lane (target): Kosambi DC → Mataram DC → Mataram 2 DC. Rule kcxv1i3omgm
    // from Redis. Real recurring ticket 6091653 / SPXID_VM_001399072, vehicle "TRONTON (10WH)".
    mod real_lane_kosambi_to_mataram {
        use super::*;

        fn kosambi_rule() -> AcceptRule {
            AcceptRule {
                id: "kcxv1i3omgm".into(),
                name: "Route Rule".into(),
                ..mk_rule(
                    RuleMode::Route,
                    RuleConditions {
                        origin: "Kosambi DC".into(),
                        destinations: vec!["Mataram DC".into(), "Mataram 2 DC".into()],
                        match_mode: RouteMatchMode::Strict,
                        booking_type: RuleBookingType::Spxid,
                        service_types: vec!["TRONTON".into()],
                        coc_only: true,
                        max_accept_count: 1,
                        accepted_count: 0,
                        ..Default::default()
                    },
                )
            }
        }

        fn real_booking(stops: &[&str]) -> Booking {
            let mut b = mk_booking(stops);
            b.booking_type = BookingType::Spxid;
            b.vehicle_type = "TRONTON (10WH)".into();
            b.report_station = "Kosambi DC".into();
            b.spx_tx_id = "SPXID_VM_001399072".into();
            b.booking_id = "6091653".into();
            b
        }

        #[test]
        fn the_real_target_ticket_matches() {
            let compiled = CompiledRule::compile(&kosambi_rule());
            assert!(compiled.matches(&real_booking(&["Kosambi DC", "Mataram DC", "Mataram 2 DC"]), &mk_state()));
        }

        #[test]
        fn find_best_matching_rule_returns_the_route_rule() {
            let b = real_booking(&["Kosambi DC", "Mataram DC", "Mataram 2 DC"]);
            let best = find_best_matching_rule(&b, &[kosambi_rule()], &mk_state()).expect("expected a match");
            assert_eq!(best.name, "Route Rule");
        }

        #[test]
        fn tronton_10wh_satisfies_service_type_tronton_capacity_suffix_tolerated() {
            assert!(vehicle_match_normalized(&norm_vehicle("TRONTON (10WH)"), &norm_vehicle("TRONTON")));
        }

        #[test]
        fn wrong_vehicle_cdd_long_is_false() {
            let compiled = CompiledRule::compile(&kosambi_rule());
            let mut b = real_booking(&["Kosambi DC", "Mataram DC", "Mataram 2 DC"]);
            b.vehicle_type = "CDD LONG (6WH)".into();
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn wrong_booking_type_reguler_not_spxid_is_false() {
            let compiled = CompiledRule::compile(&kosambi_rule());
            let mut b = real_booking(&["Kosambi DC", "Mataram DC", "Mataram 2 DC"]);
            b.booking_type = BookingType::Reguler;
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn route_out_of_order_mataram_2_before_mataram_is_false() {
            let compiled = CompiledRule::compile(&kosambi_rule());
            let b = real_booking(&["Kosambi DC", "Mataram 2 DC", "Mataram DC"]);
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn origin_not_kosambi_is_false() {
            let compiled = CompiledRule::compile(&kosambi_rule());
            let mut b = real_booking(&["Surabaya DC", "Mataram DC", "Mataram 2 DC"]);
            b.report_station = "Surabaya DC".into();
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn destination_mataram_dc_present_but_mataram_2_dc_missing_is_false() {
            let compiled = CompiledRule::compile(&kosambi_rule());
            let b = real_booking(&["Kosambi DC", "Mataram DC"]);
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn whole_word_safety_mataram_dc_must_not_satisfy_mataram_2_dc_leg() {
            let compiled = CompiledRule::compile(&kosambi_rule());
            let b = real_booking(&["Kosambi DC", "Mataram DC", "Denpasar DC"]);
            assert!(!compiled.matches(&b, &mk_state()));
        }

        #[test]
        fn reversed_dest_duplicate_rule_does_not_match_forward_route() {
            let reversed = AcceptRule {
                id: "cgkf87q3cpl".into(),
                name: "Route Rule".into(),
                ..mk_rule(
                    RuleMode::Route,
                    RuleConditions {
                        origin: "Kosambi DC".into(),
                        destinations: vec!["Mataram 2 DC".into(), "Mataram DC".into()],
                        match_mode: RouteMatchMode::Strict,
                        booking_type: RuleBookingType::Spxid,
                        service_types: vec!["TRONTON".into()],
                        coc_only: true,
                        ..Default::default()
                    },
                )
            };
            let compiled = CompiledRule::compile(&reversed);
            assert!(!compiled.matches(&real_booking(&["Kosambi DC", "Mataram DC", "Mataram 2 DC"]), &mk_state()));
        }

        #[test]
        fn cap_reached_rearm_guard_is_false_until_daily_reset() {
            let mut r = kosambi_rule();
            r.conditions.max_accept_count = 1;
            r.conditions.accepted_count = 1;
            let compiled = CompiledRule::compile(&r);
            assert!(!compiled.matches(&real_booking(&["Kosambi DC", "Mataram DC", "Mataram 2 DC"]), &mk_state()));
        }
    }

    // Regresi F2: flexible = superset strict (ekor hub setelah DC tujuan)
    mod flexible_superset_strict_f2 {
        use super::*;

        #[test]
        fn rute_berekor_hub_setelah_dc_tujuan_tetap_match_flexible() {
            let r = mk_rule(RuleMode::Route, RuleConditions { origin: "Surabaya DC".into(), destinations: vec!["Denpasar DC".into()], match_mode: RouteMatchMode::Flexible, ..Default::default() });
            let b = mk_booking(&["Surabaya DC", "Denpasar DC", "Badung Hub"]);
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn paritas_strict_juga_match_kasus_ekor_hub_yang_sama() {
            let r = mk_rule(RuleMode::Route, RuleConditions { origin: "Surabaya DC".into(), destinations: vec!["Denpasar DC".into()], match_mode: RouteMatchMode::Strict, ..Default::default() });
            let b = mk_booking(&["Surabaya DC", "Denpasar DC", "Badung Hub"]);
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn destinasi_sama_sekali_tidak_ada_di_rute_flexible_tetap_false() {
            let r = mk_rule(RuleMode::Route, RuleConditions { origin: "Surabaya DC".into(), destinations: vec!["Denpasar DC".into()], match_mode: RouteMatchMode::Flexible, ..Default::default() });
            let b = mk_booking(&["Surabaya DC", "Malang DC", "Badung Hub"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn flexible_tanpa_origin_endpoint_di_mana_pun_di_rute_match() {
            let r = mk_rule(RuleMode::Route, RuleConditions { destinations: vec!["Cileungsi DC".into()], match_mode: RouteMatchMode::Flexible, booking_type: RuleBookingType::Spxid, ..Default::default() });
            let mut b = mk_booking(&["Tegal 2 DC", "Cileungsi DC", "Bekasi Hub"]);
            b.booking_type = BookingType::Spxid;
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn flexible_destinasi_yang_hanya_muncul_di_posisi_origin_tidak_dihitung() {
            let r = mk_rule(RuleMode::Route, RuleConditions { origin: "Denpasar DC".into(), destinations: vec!["Denpasar DC".into()], match_mode: RouteMatchMode::Flexible, ..Default::default() });
            let b = mk_booking(&["Denpasar DC", "Badung Hub"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn flexible_dengan_rute_kosong_belum_enrich_false() {
            let r = mk_rule(RuleMode::Route, RuleConditions { origin: "Surabaya DC".into(), destinations: vec!["Denpasar DC".into()], match_mode: RouteMatchMode::Flexible, ..Default::default() });
            let mut b = mk_booking(&[]);
            b.report_station = "Surabaya DC".into();
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }
    }

    // Kontrak kendaraan: kosong = terima SEMUA jenis; terisi = wajib cocok
    mod vehicle_empty_means_all {
        use super::*;

        #[test]
        fn service_types_empty_accepts_any_vehicle() {
            let r = mk_rule(RuleMode::Route, RuleConditions { origin: "Surabaya DC".into(), destinations: vec!["Denpasar DC".into()], service_types: vec![], ..Default::default() });
            let mut b = mk_booking(&["Surabaya DC", "Denpasar DC"]);
            b.vehicle_type = "BLINDVAN (4WH)".into();
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn service_types_filled_rejects_unlisted_vehicle() {
            let r = mk_rule(RuleMode::Route, RuleConditions { origin: "Surabaya DC".into(), destinations: vec!["Denpasar DC".into()], service_types: vec!["TRONTON".into()], ..Default::default() });
            let mut b = mk_booking(&["Surabaya DC", "Denpasar DC"]);
            b.vehicle_type = "BLINDVAN (4WH)".into();
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }
    }

    // Regresi audit F2 lanjutan: flexible multi-destinasi tidak boleh salah-arah
    mod flexible_multi_destination {
        use super::*;

        fn rule() -> AcceptRule {
            mk_rule(RuleMode::Route, RuleConditions { origin: "Jakarta Hub".into(), destinations: vec!["Bandung DC".into(), "Surabaya DC".into()], match_mode: RouteMatchMode::Flexible, ..Default::default() })
        }

        #[test]
        fn rute_salah_urutan_endpoint_sebelum_dest_perantara_ditolak() {
            let b = mk_booking(&["Jakarta Hub", "Surabaya DC", "Bandung DC"]);
            assert!(!CompiledRule::compile(&rule()).matches(&b, &mk_state()));
        }

        #[test]
        fn dest_perantara_absen_tapi_endpoint_hadir_match() {
            let b = mk_booking(&["Jakarta Hub", "Cirebon DC", "Surabaya DC"]);
            assert!(CompiledRule::compile(&rule()).matches(&b, &mk_state()));
        }

        #[test]
        fn urutan_lengkap_plus_ekor_hub_match() {
            let b = mk_booking(&["Jakarta Hub", "Bandung DC", "Surabaya DC", "Sidoarjo Hub"]);
            assert!(CompiledRule::compile(&rule()).matches(&b, &mk_state()));
        }

        #[test]
        fn endpoint_absen_sama_sekali_ditolak_walau_dest_perantara_hadir() {
            let b = mk_booking(&["Jakarta Hub", "Bandung DC", "Semarang DC"]);
            assert!(!CompiledRule::compile(&rule()).matches(&b, &mk_state()));
        }
    }
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p core-domain matching::tests::route_mode_tests`
Expected: this specific module compiles (types already exist from Task 7) but every test in it panics with `not yet implemented: implemented in Task 8` (from `matches_route`'s `unimplemented!()`). This IS the expected RED state for this task — capture a snippet of the panic output as your RED evidence.

- [ ] **Step 4: Implement `matches_route`**

Replace the `fn matches_route(&self, _booking: &Booking) -> bool { unimplemented!("implemented in Task 8") }` stub in `matching.rs` with:

```rust
    fn matches_route(&self, booking: &Booking) -> bool {
        let c = &self.conditions;
        let stops: Vec<String> = booking.route_stops.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let flexible = c.match_mode == RouteMatchMode::Flexible;

        // SAFETY GUARD: a route rule with no origin, no destinations, AND no other active
        // filter would match EVERY ticket if uncapped — an empty route rule must match NOTHING.
        let has_other_filter = !c.service_types.is_empty()
            || c.max_weight.is_some()
            || c.max_cod_amount.is_some()
            || c.coc_only
            || c.non_coc_only
            || c.booking_type != RuleBookingType::All;
        if self.origin_norm.is_empty() && self.destinations_norm.is_empty() && !has_other_filter {
            return false;
        }

        if !self.origin_norm.is_empty() {
            // Origin must be the REAL start point: report_station_name first, then the first
            // stop. Region/province labels never satisfy a route rule (Booking doesn't even
            // carry those fields — see Task 1).
            let by_report_station = loc_match_normalized(&booking.report_station, &self.origin_norm);
            let by_first_stop = stops.first().map(|s| loc_match_normalized(s, &self.origin_norm)).unwrap_or(false);
            if !by_report_station && !by_first_stop {
                return false;
            }
        }

        if !self.destinations_norm.is_empty() {
            if stops.is_empty() {
                return false;
            }
            let origin_consumes_first_stop = if !self.origin_norm.is_empty() {
                stops.first().map(|s| loc_match_normalized(s, &self.origin_norm)).unwrap_or(false)
            } else {
                false
            };
            let start_idx = usize::from(origin_consumes_first_stop);
            if !self.destinations_match_in_order(&stops, start_idx, flexible) {
                return false;
            }
        }

        if c.booking_type != RuleBookingType::All {
            let want = match c.booking_type {
                RuleBookingType::Spxid => BookingType::Spxid,
                RuleBookingType::Reguler => BookingType::Reguler,
                RuleBookingType::All => unreachable!(),
            };
            if booking.booking_type != want {
                return false;
            }
        }
        if !self.service_types_norm.is_empty() {
            let ticket_norm = norm_vehicle(&booking.vehicle_type);
            if !self.service_types_norm.iter().any(|r| vehicle_match_normalized(&ticket_norm, r)) {
                return false;
            }
        }
        if let Some(max_weight) = c.max_weight {
            if booking.weight > max_weight {
                return false;
            }
        }
        if let Some(max_cod) = c.max_cod_amount {
            if booking.cod_amount > max_cod {
                return false;
            }
        }
        if c.coc_only && booking.booking_type != BookingType::Spxid {
            return false;
        }
        if c.non_coc_only && booking.booking_type == BookingType::Spxid {
            return false;
        }
        true
    }

    /// Ordered, whole-word walk through `stops` starting at `start_idx`, consuming
    /// `self.destinations_norm` in order. STRICT: any destination not found → false
    /// immediately. FLEXIBLE: an intermediate (non-last) destination may be absent and is
    /// skipped without advancing the cursor; the LAST destination (the endpoint) must still be
    /// found or the whole match fails.
    fn destinations_match_in_order(&self, stops: &[String], start_idx: usize, flexible: bool) -> bool {
        let mut idx = start_idx;
        let dests = &self.destinations_norm;
        for (d, want_norm) in dests.iter().enumerate() {
            let mut found: Option<usize> = None;
            for (j, stop) in stops.iter().enumerate().skip(idx) {
                if loc_match_normalized(stop, want_norm) {
                    found = Some(j);
                    break;
                }
            }
            match found {
                Some(j) => idx = j + 1,
                None => {
                    if flexible && d != dests.len() - 1 {
                        continue;
                    }
                    return false;
                }
            }
        }
        true
    }
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p core-domain matching::tests::route_mode_tests matching::tests::shift_trip_targeting matching::tests::real_lane_aceh_to_cileungsi matching::tests::real_lane_kosambi_to_mataram matching::tests::flexible_superset_strict_f2 matching::tests::vehicle_empty_means_all matching::tests::flexible_multi_destination`

Expected: `9 + 5 + 5 + 9 + 6 + 2 + 4 = 40` tests pass, 0 failed.

- [ ] **Step 6: Clippy**

Run: `cargo clippy -p core-domain -- -D warnings`
Expected: clean. (`matches_filter` is still `unimplemented!()` at this point — expected, Task 9's job — this does not trigger a clippy warning.)

- [ ] **Step 7: Commit**

```bash
git add Backend/crates/core-domain/src/matching.rs
git commit -m "feat(core-domain): route mode matching (strict/flexible, shift/trip, real production lanes)"
```

---

### Task 9: `matching.rs` part 3 — filter mode + CP-4 empty-filter guard

**Files:**
- Modify: `Backend/crates/core-domain/src/matching.rs` (fill in `matches_filter`, add `mod filter_mode_tests` inside the existing test module)

**Interfaces:**
- Consumes: everything from Task 7/8.
- Produces: a working `CompiledRule::matches_filter` — the last `unimplemented!()` stub in the crate. After this task, `cargo test -p core-domain` runs the **entire** crate suite for the first time (Task 7/8 deliberately ran scoped subsets since `matches_filter` wasn't ready yet).

- [ ] **Step 1: Read the reference source first**

Read `matching.ts` lines 343-359 (filter-mode branch) and these `matching.test.ts` blocks: "matchesRule — filter mode COC semantics" (190-203) and the two CP-4 tests inside "Phase-1: booking_id dominance + empty-filter safety" (353-362).

- [ ] **Step 2: Write the failing tests**

Add inside the existing `#[cfg(test)] mod tests { ... }` block in `matching.rs`:

```rust
    mod filter_mode_coc_semantics {
        use super::*;

        #[test]
        fn coc_only_treats_spxid_as_coc_even_when_cod_flag_is_false() {
            let r = mk_rule(RuleMode::Filter, RuleConditions { coc_only: true, ..Default::default() });
            let mut b = mk_booking(&[]);
            b.booking_type = BookingType::Spxid;
            assert!(CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn coc_only_rejects_reguler_even_when_cod_flag_is_true() {
            let r = mk_rule(RuleMode::Filter, RuleConditions { coc_only: true, ..Default::default() });
            let mut b = mk_booking(&[]);
            b.booking_type = BookingType::Reguler;
            b.cod_amount = 1.0; // stand-in for "COD flag true"; matches_filter must not read this as COC
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn non_coc_only_rejects_spxid_even_when_cod_flag_is_false() {
            let r = mk_rule(RuleMode::Filter, RuleConditions { non_coc_only: true, ..Default::default() });
            let mut b = mk_booking(&[]);
            b.booking_type = BookingType::Spxid;
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }
    }

    mod cp4_empty_filter_safety {
        use super::*;

        #[test]
        fn empty_filter_rule_matches_nothing_no_blanket_accept() {
            let r = mk_rule(RuleMode::Filter, RuleConditions::default());
            let b = mk_booking(&["Anywhere DC"]);
            assert!(!CompiledRule::compile(&r).matches(&b, &mk_state()));
        }

        #[test]
        fn filter_rule_with_active_condition_still_matches_correctly() {
            let r = mk_rule(RuleMode::Filter, RuleConditions { coc_only: true, ..Default::default() });
            let compiled = CompiledRule::compile(&r);
            let mut spxid = mk_booking(&["X"]);
            spxid.booking_type = BookingType::Spxid;
            assert!(compiled.matches(&spxid, &mk_state()));
            let mut reguler = mk_booking(&["X"]);
            reguler.booking_type = BookingType::Reguler;
            assert!(!compiled.matches(&reguler, &mk_state()));
        }
    }
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p core-domain matching::tests::filter_mode_coc_semantics matching::tests::cp4_empty_filter_safety`
Expected: every test panics with `not yet implemented: implemented in Task 9` (the `matches_filter` stub). Capture as RED evidence.

- [ ] **Step 4: Implement `matches_filter`**

Replace the `fn matches_filter(&self, _booking: &Booking) -> bool { unimplemented!("implemented in Task 9") }` stub with:

```rust
    fn matches_filter(&self, booking: &Booking) -> bool {
        let c = &self.conditions;
        // CP-4: an enabled filter rule with ZERO active conditions must match NOTHING — without
        // this guard every check below is skipped and the function falls through to `true`,
        // turning a blank/misconfigured filter rule into a blanket accept of the entire pool.
        let filter_active =
            c.max_weight.is_some() || c.max_cod_amount.is_some() || c.coc_only || c.non_coc_only || !self.service_types_norm.is_empty();
        if !filter_active {
            return false;
        }
        if let Some(max_weight) = c.max_weight {
            if booking.weight > max_weight {
                return false;
            }
        }
        if let Some(max_cod) = c.max_cod_amount {
            if booking.cod_amount > max_cod {
                return false;
            }
        }
        // Line-haul "COC" means an SPXID ticket, not the COD/cash-on-delivery flag — these are
        // deliberately separate concepts (see coc.rs's module doc).
        if c.coc_only && booking.booking_type != BookingType::Spxid {
            return false;
        }
        if c.non_coc_only && booking.booking_type == BookingType::Spxid {
            return false;
        }
        if !self.service_types_norm.is_empty() {
            let ticket_norm = norm_vehicle(&booking.vehicle_type);
            if !self.service_types_norm.iter().any(|r| vehicle_match_normalized(&ticket_norm, r)) {
                return false;
            }
        }
        true
    }
```

- [ ] **Step 5: Run the FULL crate test suite for the first time**

Run: `cargo test -p core-domain`
Expected: every test across every module now passes — `7 (coc) + 7 (location) + 13 (vehicle, incl. 2 paren-stripping regression tests added during review) + 14 (route_parse, incl. 2 regression tests added during review) + 3 (sanitize) + 9 (dedupe) + 3+6+1+2 (Task 7: cap/booking_id/guards/cp6) + 40 (Task 8: route mode) + 3+2 (this task: filter mode/CP-4) = 110` tests, `test result: ok. 110 passed; 0 failed`. If the count differs, do not adjust the count to match — investigate which test is missing or duplicated and report it in your self-review.

- [ ] **Step 6: Clippy**

Run: `cargo clippy -p core-domain -- -D warnings`
Expected: clean — no more `unimplemented!()` stubs anywhere in the crate.

- [ ] **Step 7: Commit**

```bash
git add Backend/crates/core-domain/src/matching.rs
git commit -m "feat(core-domain): filter mode matching + CP-4 empty-filter guard"
```

---

### Task 10: `matching.rs` part 4 — overlapping-rule ranking tests + `matched_booking_id_for`

**Files:**
- Modify: `Backend/crates/core-domain/src/matching.rs` (add `matched_booking_id_for` function + `mod overlapping_rules_tests` + `mod matched_booking_id_for_tests`)
- Modify: `Backend/crates/core-domain/src/lib.rs`

**Interfaces:**
- Consumes: `norm_id` (Task 5), everything from Task 7-9.
- Produces: `pub fn matched_booking_id_for(booking: &Booking, rule: &AcceptRule) -> Option<String>`. Nothing later in this plan consumes it, but it's required public surface per the master spec (Fase 4's executor will call it to know which booking-id to remove from a rule after a successful accept).

**Why this function exists as a near-duplicate of `matches_booking_id`:** the reference source's own comment (`matching.ts` right above `matchedBookingIdFor`) documents a real production incident: an earlier version of this function normalized differently from `matchesRule`, so a rule could WIN a match but this function would return `null` for it — the booking-id was never consumed, and the rule stayed "armed" forever for a ticket it had already won. The fix was making both use the exact same `normId`. Keep that invariant here: `matched_booking_id_for` must use `norm_id` — the identical function `matches_booking_id`/`dedupe_rules` use — never a different normalization.

- [ ] **Step 1: Read the reference source first**

Read `matching.ts` lines 205-246 (`findBestMatchingRule` — already ported in Task 7, re-read for context) and lines 370-386 (`matchedBookingIdFor` + its comment). Read `matching.test.ts`'s "findBestMatchingRule — overlapping rules" (205-246) and "matchedBookingIdFor — normalisasi HARUS identik dengan matchesRule (C2)" (432-461).

- [ ] **Step 2: Write the failing tests**

Add inside the existing `#[cfg(test)] mod tests { ... }` block:

```rust
    mod overlapping_rules_tests {
        use super::*;

        #[test]
        fn prefers_more_specific_multi_destination_route_over_generic_endpoint_rule() {
            let mut b = mk_booking(&["Pekanbaru DC", "Palembang DC", "Cileungsi DC"]);
            b.booking_type = BookingType::Spxid;
            b.vehicle_type = "TRONTON (10WH)".into();

            let base = || RuleConditions {
                origin: "Pekanbaru DC".into(),
                booking_type: RuleBookingType::Spxid,
                service_types: vec!["TRONTON".into()],
                coc_only: true,
                match_mode: RouteMatchMode::Strict,
                ..Default::default()
            };
            let generic = AcceptRule { id: "generic".into(), name: "generic".into(), ..mk_rule(RuleMode::Route, RuleConditions { destinations: vec!["Cileungsi DC".into()], ..base() }) };
            let specific = AcceptRule { id: "specific".into(), name: "specific".into(), ..mk_rule(RuleMode::Route, RuleConditions { destinations: vec!["Palembang DC".into(), "Cileungsi DC".into()], ..base() }) };

            let best = find_best_matching_rule(&b, &[generic, specific], &mk_state()).expect("expected a match");
            assert_eq!(best.id, "specific");
        }

        #[test]
        fn booking_id_target_beats_a_broad_route_rule_for_the_same_ticket() {
            let mut b = mk_booking(&["Aceh DC", "Cileungsi DC"]);
            b.spx_tx_id = "SPXID_VM_001397649".into();
            b.booking_id = "5996405".into();
            b.booking_type = BookingType::Spxid;
            b.vehicle_type = "TRONTON (10WH)".into();

            let route = AcceptRule {
                id: "route".into(),
                ..mk_rule(RuleMode::Route, RuleConditions {
                    origin: "Aceh DC".into(),
                    destinations: vec!["Cileungsi DC".into()],
                    booking_type: RuleBookingType::Spxid,
                    service_types: vec!["TRONTON".into()],
                    coc_only: true,
                    ..Default::default()
                })
            };
            let target = AcceptRule { id: "target".into(), ..mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["SPXID_VM_001397649".into()], ..Default::default() }) };

            let best = find_best_matching_rule(&b, &[route, target], &mk_state()).expect("expected a match");
            assert_eq!(best.id, "target");
        }
    }

    mod matched_booking_id_for_tests {
        use super::*;

        #[test]
        fn separator_beda_spasi_vs_underscore_tetap_mengembalikan_raw_id() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["SPXID VM 001402220".into()], ..Default::default() });
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_001402220".into();
            assert_eq!(matched_booking_id_for(&b, &r), Some("SPXID VM 001402220".to_string()));
        }

        #[test]
        fn match_via_booking_id_bukan_spx_tx_id() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["6254861".into()], ..Default::default() });
            let mut b = mk_booking(&[]);
            b.booking_id = "6254861".into();
            assert_eq!(matched_booking_id_for(&b, &r), Some("6254861".to_string()));
        }

        #[test]
        fn match_via_request_id() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["REQ-000123".into()], ..Default::default() });
            let mut b = mk_booking(&[]);
            b.request_id = "REQ_000123".into();
            assert_eq!(matched_booking_id_for(&b, &r), Some("REQ-000123".to_string()));
        }

        #[test]
        fn substring_hanya_untuk_id_panjang_9_plus() {
            let short = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["12345".into()], ..Default::default() });
            let mut b_short = mk_booking(&[]);
            b_short.spx_tx_id = "SPXID_12345_VM".into();
            assert_eq!(matched_booking_id_for(&b_short, &short), None);

            let long = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["001402220".into()], ..Default::default() });
            let mut b_long = mk_booking(&[]);
            b_long.spx_tx_id = "SPXID_VM_001402220".into();
            assert_eq!(matched_booking_id_for(&b_long, &long), Some("001402220".to_string()));
        }

        #[test]
        fn tidak_cocok_null_daftar_kosong_null() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["SPXID_VM_001402220".into()], ..Default::default() });
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_9".into();
            assert_eq!(matched_booking_id_for(&b, &r), None);

            let empty_rule = mk_rule(RuleMode::BookingId, RuleConditions::default());
            let mut b2 = mk_booking(&[]);
            b2.spx_tx_id = "X".into();
            assert_eq!(matched_booking_id_for(&b2, &empty_rule), None);
        }

        #[test]
        fn paritas_dengan_matches_rule_pada_kasus_separator_kontrak_anti_drift() {
            let r = mk_rule(RuleMode::BookingId, RuleConditions { booking_ids: vec!["SPXID_ VM_001397492C".into()], ..Default::default() });
            let mut b = mk_booking(&[]);
            b.spx_tx_id = "SPXID_VM_001397492C".into();
            assert!(matches_rule(&b, &r, &mk_state()));
            assert_eq!(matched_booking_id_for(&b, &r), Some("SPXID_ VM_001397492C".to_string()));
        }
    }
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p core-domain matching::tests::overlapping_rules_tests matching::tests::matched_booking_id_for_tests`
Expected: `overlapping_rules_tests` PASSES already (it only exercises `find_best_matching_rule`, fully implemented since Task 9) — that's fine, note it in your report as pre-existing coverage, not new RED/GREEN. `matched_booking_id_for_tests` FAILS to compile: `cannot find function \`matched_booking_id_for\``.

- [ ] **Step 4: Implement `matched_booking_id_for`**

Add to `matching.rs`, near `matches_rule`/`find_best_matching_rule` (outside the `impl CompiledRule` block — this is a free function operating on a raw `AcceptRule`, matching the reference signature exactly):

```rust
/// Returns the registered booking-ID string (original case/spacing) this booking matches, or
/// `None`. MUST use the same normalization (`norm_id`) as `CompiledRule::matches_booking_id` —
/// see this task's brief header for the production incident this invariant prevents.
pub fn matched_booking_id_for(booking: &Booking, rule: &AcceptRule) -> Option<String> {
    let tx = norm_id(&booking.spx_tx_id);
    let bk = norm_id(&booking.booking_id);
    let rq = norm_id(&booking.request_id);
    for raw in &rule.conditions.booking_ids {
        let id = norm_id(raw);
        if id.is_empty() {
            continue;
        }
        if tx == id || bk == id || rq == id || (id.len() >= 9 && tx.contains(id.as_str())) {
            return Some(raw.clone());
        }
    }
    None
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p core-domain matching::tests::overlapping_rules_tests matching::tests::matched_booking_id_for_tests`
Expected: `2 + 6 = 8` tests pass, 0 failed.

- [ ] **Step 6: Wire into `lib.rs`**

Add `matched_booking_id_for` to the existing `pub use matching::{...}` line in `lib.rs`.

- [ ] **Step 7: Run the full crate suite once**

Run: `cargo test -p core-domain`
Expected: 110 (from Task 9) + 8 = 118 passing, 0 failed.

- [ ] **Step 8: Clippy**

Run: `cargo clippy -p core-domain -- -D warnings` — expected clean.

- [ ] **Step 9: Commit**

```bash
git add Backend/crates/core-domain/src/matching.rs Backend/crates/core-domain/src/lib.rs
git commit -m "feat(core-domain): matchedBookingIdFor + overlapping-rule ranking coverage"
```

---

### Task 11: Precompute proof test + full-workspace verification + Fase 1 sign-off

**Files:**
- Modify: `Backend/crates/core-domain/src/matching.rs` (add one `mod precompute_at_save_tests` — no production code changes)
- Modify: `Docs/superpowers/plans/2026-07-13-fase-1-core-domain.md` (this file — check off every remaining box)

**Interfaces:**
- Consumes: everything from Task 1-10.
- Produces: nothing new consumed by later work — this is Fase 1's sign-off gate, mirroring Fase 0's Task 8.

- [ ] **Step 1: Add the precompute-at-save demonstration test**

Add inside the existing `#[cfg(test)] mod tests { ... }` block in `matching.rs`:

```rust
    mod precompute_at_save_tests {
        use super::*;

        #[test]
        fn compile_once_then_matches_many_times_against_different_bookings() {
            // Demonstrates the master spec's "compile at save, not per ticket" requirement:
            // origin/destinations are normalized exactly once here, then `matches` is called
            // against several distinct bookings without re-deriving `origin_norm`/
            // `destinations_norm` — inspect `CompiledRule::compile` (Task 7) to confirm the
            // normalization happens inside `compile`, not inside `matches`/`matches_route`.
            let rule = mk_rule(
                RuleMode::Route,
                RuleConditions { origin: "Padang DC".into(), destinations: vec!["Cileungsi DC".into()], ..Default::default() },
            );
            let compiled = CompiledRule::compile(&rule);

            assert!(compiled.matches(&mk_booking(&["Padang DC", "Cileungsi DC"]), &mk_state()));
            assert!(!compiled.matches(&mk_booking(&["Padang DC", "Surabaya DC"]), &mk_state()));
            assert!(compiled.matches(&mk_booking(&["Padang DC", "Jakarta Hub", "Cileungsi DC"]), &mk_state()));
            assert!(!compiled.matches(&mk_booking(&["Bandung DC", "Cileungsi DC"]), &mk_state()));

            // Precomputed fields are stable across all four calls above — confirm directly.
            assert_eq!(compiled.origin_norm, "padang dc");
            assert_eq!(compiled.destinations_norm, vec!["cileungsi dc".to_string()]);
        }
    }
```

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test -p core-domain matching::tests::precompute_at_save_tests`
Expected: 1 test passes. (This accesses `compiled.origin_norm`/`compiled.destinations_norm` directly — both are private fields on `CompiledRule`, which is fine since this test lives inside the same crate's `#[cfg(test)]` module and Rust privacy is module-scoped, not just crate-scoped-via-`pub`; if this doesn't compile because the test module isn't a descendant of `matching`'s private scope, move the test into `matching.rs`'s existing `mod tests` — not a new top-level integration test file — where it already has access.)

- [ ] **Step 3: Full crate test suite**

Run: `cargo test -p core-domain`
Expected: 118 (Task 10) + 1 = **119 tests pass, 0 failed**. This is the money-critical GATE from the master spec — do not proceed to Step 4 if anything fails.

- [ ] **Step 4: Full workspace build/test/clippy (confirm nothing else broke)**

```bash
cd Backend
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cd ..
```

Expected: all three clean — `reactor-core`/`auth-sidecar`'s existing 2 tests plus `core-domain`'s 115 plus the 7 remaining empty lib crates' 0-test runs, all `ok`.

- [ ] **Step 5: Confirm no I/O dependency crept in**

Run: `cd Backend && cargo tree -p core-domain && cd ..`
Expected: `core-domain` depends only on `serde_json` (plus serde_json's own transitive deps — `serde`, `itoa`, `ryu`, `memchr`, etc., which is expected and fine). Confirm `tokio`, `reqwest`, `sqlx`, `redis`, `hyper` do **not** appear anywhere in the tree — if any of them do, something pulled in an I/O dependency by accident and must be fixed before sign-off, not merely noted.

- [ ] **Step 6: Cross-check every function named in this plan's Global Constraints "Cakupan" list is actually exported**

Run: `grep -n "^pub fn\|^pub struct\|^pub enum" Backend/crates/core-domain/src/*.rs`

Manually confirm every item in the design doc's "Cakupan (in-scope)" list has a corresponding `pub` item in the grep output: `is_coc_name`, `is_coc`, `booking_type_of`, `norm_loc`, `loc_match`, `norm_vehicle`, `vehicle_match`, `sanitize_accept_rules`, `matches_rule`, `find_best_matching_rule`, `matched_booking_id_for`, `dedupe_rules`, `parse_route_stops`, `parse_route_detail_list`. If anything is missing, that's a real gap — add it and its test(s) before sign-off, don't just note it as a known gap.

- [ ] **Step 7: Mark this plan complete**

Check every remaining `- [ ]` box in this file (`Docs/superpowers/plans/2026-07-13-fase-1-core-domain.md`) to `- [x]` — the intro sentence describing the checkbox convention (if any, mirroring Fase 0's plan) is prose, not a real checkbox; do not flip it (Fase 0's Task 8 hit exactly this false-positive from a blanket find/replace — do the edit by hand per-line, or grep-verify afterward that the intro line is untouched).

- [ ] **Step 8: Commit**

```bash
git add Backend/crates/core-domain/src/matching.rs Docs/superpowers/plans/2026-07-13-fase-1-core-domain.md
git commit -m "test(core-domain): precompute-at-save proof + Fase 1 sign-off"
```

Fase 1 is done once this commits clean. Fase 2 (store + skema DB) is the next master-spec phase — do not start it in this same pass; it gets its own spec/plan cycle.

