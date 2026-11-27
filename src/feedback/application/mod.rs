pub mod criteria;
pub mod ports;
pub mod service;

pub use crate::feedback::domain::criterion::{CriterionVerdict, CriterionVerification, criterion_text_hash};
pub use crate::feedback::domain::fingerprint::fingerprint;
pub use crate::feedback::domain::model::{
    Assessment, DISMISSAL_TTL_DAYS, FindingFacts, Judgment, KindScore, Scope, SeverityCap, Verdict, kind_of,
};
pub use criteria::CriterionVerifications;
pub use ports::{CriterionStore, FeedbackError, FeedbackStore};
pub use service::FeedbackService;
