//! Hand-off context: after a scan, writes a review file (Markdown, optionally JSON) that developers give to
//! their coding agent (Claude Code, Cursor...) to continue fixing and developing the feature.

pub mod application;
pub mod domain;
pub mod infrastructure;

pub use application::{ExportOutcome, ExportReview, ReviewFileSink, native_default_dir};
pub use infrastructure::FsReviewFileSink;
