//! Dashboard secret and the session key derived from it. Pure values: no I/O, no framework.

mod secret;

pub use secret::{DashboardSecret, InvalidSecret, MIN_SECRET_LEN, SECRET_BYTES, SessionKey, constant_time_eq};
