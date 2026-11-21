use std::collections::BTreeSet;
use std::path::Path;

use futures::future::BoxFuture;

use super::QualityRequest;
use crate::quality::domain::{ChangeIndex, Language, QualityFinding, QualityTool};

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

pub trait QualityCheck: Send + Sync {
    fn tool(&self) -> QualityTool;

    fn languages(&self) -> &'static [Language];

    fn run<'a>(&'a self, input: &'a CheckInput<'a>) -> BoxFuture<'a, CheckOutcome>;
}
