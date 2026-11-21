//! Ports of the design use case. Dyn-compatible (boxed futures) because adapters are chosen at runtime.

use std::path::Path;

use futures::future::BoxFuture;

use crate::design::domain::{DesignSpec, FigmaLink};
use crate::review::application::DesignMismatch;
use crate::review::domain::change_set::FileChange;

/// Why design data could not be read. Messages never contain the Figma token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DesignError {
    #[error("Figma configuration: {0}")]
    Config(String),
    #[error("Figma rejected the token: {0}")]
    Unauthorized(String),
    #[error("Figma node not found: {0}")]
    NotFound(String),
    #[error("Figma unavailable: {0}")]
    Unavailable(String),
    #[error("invalid Figma response: {0}")]
    Invalid(String),
    #[error("design cache: {0}")]
    Storage(String),
    #[error("design states check: {0}")]
    Judge(String),
}

/// One frame as read from Figma.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedDesign {
    pub spec: DesignSpec,
    /// Figma file version the spec was read at.
    pub version: String,
    /// Rendered PNG of the frame, when requested and available.
    pub png: Option<Vec<u8>>,
    /// Non-fatal problems (variables unavailable, image skipped...). Not cached.
    pub notes: Vec<String>,
}

/// Reads frames from Figma.
pub trait FigmaSource: Send + Sync {
    /// Current version of a file (cheap call), used to decide whether the cache is fresh.
    fn file_version<'a>(&'a self, file_key: &'a str) -> BoxFuture<'a, Result<String, DesignError>>;
    /// Reads the node tree (and, with `with_png`, a rendered image) of one frame.
    fn fetch<'a>(&'a self, link: &'a FigmaLink, with_png: bool) -> BoxFuture<'a, Result<FetchedDesign, DesignError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedDesign {
    pub version: String,
    /// RFC 3339.
    pub fetched_at: String,
    pub spec: DesignSpec,
    pub png: Option<Vec<u8>>,
}

/// Frames keyed by (file key, node id), refreshed when the file version changes.
pub trait DesignCache: Send + Sync {
    fn get<'a>(&'a self, link: &'a FigmaLink) -> BoxFuture<'a, Result<Option<CachedDesign>, DesignError>>;
    fn put<'a>(&'a self, link: &'a FigmaLink, design: &'a FetchedDesign) -> BoxFuture<'a, Result<(), DesignError>>;
}

/// Translation strings of a repository (values of json/yaml files under locales, i18n, lang, translations).
pub trait I18nSource: Send + Sync {
    fn load<'a>(&'a self, repo_root: &'a Path) -> BoxFuture<'a, Vec<String>>;
}

/// What the states judge looks at for one frame.
pub struct StatesRequest<'a> {
    pub spec: &'a DesignSpec,
    pub png: Option<&'a [u8]>,
    /// Changed UI files.
    pub files: &'a [&'a FileChange],
    /// Ticket brief context, if any.
    pub context: Option<&'a str>,
}

/// Judges which design variants (loading, empty, error, disabled...) the code does not handle.
pub trait StatesJudge: Send + Sync {
    fn judge<'a>(&'a self, request: StatesRequest<'a>) -> BoxFuture<'a, Result<Vec<DesignMismatch>, DesignError>>;
}
