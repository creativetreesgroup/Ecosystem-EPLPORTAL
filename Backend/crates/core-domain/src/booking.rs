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
