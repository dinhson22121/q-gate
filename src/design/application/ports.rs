use std::path::Path;

use futures::future::BoxFuture;

use crate::design::domain::{DesignSpec, FigmaLink};
use crate::review::application::DesignMismatch;
use crate::review::domain::change_set::FileChange;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedDesign {
    pub spec: DesignSpec,
    pub version: String,
    pub png: Option<Vec<u8>>,
    pub notes: Vec<String>,
}

pub trait FigmaSource: Send + Sync {
    fn file_version<'a>(&'a self, file_key: &'a str) -> BoxFuture<'a, Result<String, DesignError>>;
    fn fetch<'a>(&'a self, link: &'a FigmaLink, with_png: bool) -> BoxFuture<'a, Result<FetchedDesign, DesignError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedDesign {
    pub version: String,
    pub fetched_at: String,
    pub spec: DesignSpec,
    pub png: Option<Vec<u8>>,
}

pub trait DesignCache: Send + Sync {
    fn get<'a>(&'a self, link: &'a FigmaLink) -> BoxFuture<'a, Result<Option<CachedDesign>, DesignError>>;
    fn put<'a>(&'a self, link: &'a FigmaLink, design: &'a FetchedDesign) -> BoxFuture<'a, Result<(), DesignError>>;
}

pub trait I18nSource: Send + Sync {
    fn load<'a>(&'a self, repo_root: &'a Path) -> BoxFuture<'a, Vec<String>>;
}

pub struct StatesRequest<'a> {
    pub spec: &'a DesignSpec,
    pub png: Option<&'a [u8]>,
    pub files: &'a [&'a FileChange],
    pub context: Option<&'a str>,
}

pub trait StatesJudge: Send + Sync {
    fn judge<'a>(&'a self, request: StatesRequest<'a>) -> BoxFuture<'a, Result<Vec<DesignMismatch>, DesignError>>;
}
