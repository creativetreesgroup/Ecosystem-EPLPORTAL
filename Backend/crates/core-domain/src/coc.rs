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
