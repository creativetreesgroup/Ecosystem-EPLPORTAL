pub mod accept;
pub mod booking;
pub mod client;
pub mod cookies;
pub mod crypto;
pub mod waha_settings;

pub use accept::{classify_accept_response, AcceptReason, AcceptResult};
pub use booking::{normalize_booking, to_core_booking, SpxBooking};
pub use client::SpxClient;
pub use cookies::{build_cookie_string, build_headers, SpxCookies};
