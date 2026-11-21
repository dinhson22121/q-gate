use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_git_root: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub base: PathBuf,
    pub path: PathBuf,
    pub parent: Option<PathBuf>,
    pub crumbs: Vec<Crumb>,
    pub is_git_root: bool,
    pub entries: Vec<DirEntry>,
    pub truncated: bool,
}
