pub mod booking;
pub mod coc;
pub mod location;

pub use booking::{Booking, BookingType};
pub use coc::{booking_type_of, is_coc, is_coc_name};
pub use location::{loc_match, loc_match_normalized, norm_loc};
