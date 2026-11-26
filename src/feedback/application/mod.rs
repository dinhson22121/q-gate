pub mod ports;
pub mod service;

pub use crate::feedback::domain::fingerprint::fingerprint;
pub use crate::feedback::domain::model::{
    Assessment, FindingFacts, Judgment, KindScore, SeverityCap, Verdict, kind_of,
};
pub use ports::{FeedbackError, FeedbackStore};
pub use service::FeedbackService;
