use std::collections::BTreeSet;
use std::path::Path;

use futures::future::BoxFuture;

use super::QualityRequest;
use crate::quality::domain::{ChangeIndex, Language, QualityFinding, QualityTool};

/// What a check sees.
pub struct CheckInput<'a> {
    pub request: &'a QualityRequest,
    pub changes: &'a ChangeIndex,
    pub languages: &'a BTreeSet<Language>,
}

impl CheckInput<'_> {
    pub fn snapshot(&self) -> &Path {
        &self.request.snapshot
    }
}

/// Findings (not yet graded as new/pre-existing) and human-readable notes (skips, failures).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CheckOutcome {
    pub findings: Vec<QualityFinding>,
    pub notes: Vec<String>,
}

impl CheckOutcome {
    pub fn skipped(note: impl Into<String>) -> Self {
        Self { findings: Vec::new(), notes: vec![note.into()] }
    }

    pub fn found(findings: Vec<QualityFinding>) -> Self {
        Self { findings, notes: Vec::new() }
    }
}

/// One external tool. Implementations never fail: a missing tool or a broken run is a note.
pub trait QualityCheck: Send + Sync {
    fn tool(&self) -> QualityTool;

    /// The check applies when any of these languages is selected; empty means every repository.
    fn languages(&self) -> &'static [Language];

    fn run<'a>(&'a self, input: &'a CheckInput<'a>) -> BoxFuture<'a, CheckOutcome>;
}
