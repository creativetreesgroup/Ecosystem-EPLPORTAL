// Backend/crates/ws-hub/tests/events_serde.rs
//! The ported variants serialize to the exact reference wire shape.
use ws_hub::WsEvent;

#[test]
fn variants_serialize_to_reference_shape() {
    assert_eq!(
        WsEvent::TicketsRemoved { ids: vec!["a".into(), "b".into()] }.to_json(),
        r#"{"type":"tickets_removed","data":{"ids":["a","b"]}}"#
    );
    assert_eq!(
        WsEvent::CookiesExpired { message: "expired".into() }.to_json(),
        r#"{"type":"cookies_expired","data":{"message":"expired"}}"#
    );
    assert_eq!(
        WsEvent::Connected { session: "s1".into() }.to_json(),
        r#"{"type":"connected","data":{"session":"s1"}}"#
    );
    // camelCase inner field (reference protocol).
    assert_eq!(
        WsEvent::TicketRejected { booking_id: "B9".into() }.to_json(),
        r#"{"type":"ticket_rejected","data":{"bookingId":"B9"}}"#
    );
}
