//! Use case and ports of the impact context.

mod detect;
mod ports;

pub use detect::{DEFAULT_MAX_PACKAGES, DetectRegressions, DetectRequest};
pub use ports::{BaseRunKey, BaseRunStore, GoToolchain, ImpactError, TestSelection};
