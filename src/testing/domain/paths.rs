//! Lexical policy for paths the model sends: relative, inside the snapshot, no tricks.
//! Symlinks are checked by the workspace adapter after canonicalization.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathError(pub String);

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Normalizes a snapshot-relative path to `a/b/c` ("" is the snapshot root).
///
/// Rejects absolute paths, drive prefixes, backslashes, NUL bytes and any `..` component;
/// `.` and empty components are dropped.
pub fn normalize_relative(path: &str) -> Result<String, PathError> {
    let reject = |why: &str| Err(PathError(format!("invalid path '{}': {why}", path.escape_debug())));
    if path.contains('\0') {
        return reject("contains a NUL byte");
    }
    if path.contains('\\') {
        return reject("use '/' as separator");
    }
    if path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') {
        return reject("must be relative to the repository root");
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return reject("'..' is not allowed"),
            other => parts.push(other),
        }
    }
    Ok(parts.join("/"))
}

/// Directory part of a normalized path ("" at the root).
pub fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// Joins normalized paths, treating "" as the root.
pub fn join(dir: &str, rest: &str) -> String {
    match (dir.is_empty(), rest.is_empty()) {
        (true, _) => rest.to_string(),
        (false, true) => dir.to_string(),
        (false, false) => format!("{dir}/{rest}"),
    }
}

/// `path` relative to `base` when `path` is `base` or below it.
pub fn strip_dir<'a>(path: &'a str, base: &str) -> Option<&'a str> {
    if base.is_empty() {
        return Some(path);
    }
    if path == base {
        return Some("");
    }
    path.strip_prefix(base).and_then(|rest| rest.strip_prefix('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_harmless_paths() {
        assert_eq!(normalize_relative("a/./b//c.go").unwrap(), "a/b/c.go");
        assert_eq!(normalize_relative("./").unwrap(), "");
        assert_eq!(normalize_relative(".").unwrap(), "");
        assert_eq!(normalize_relative("").unwrap(), "");
    }

    #[test]
    fn rejects_escape_attempts() {
        for bad in ["../x", "a/../../etc/passwd", "a/..", "/etc/passwd", "C:/x", "c:x", "a\\..\\b", "a\0b"] {
            assert!(normalize_relative(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(normalize_relative("../x").unwrap_err().to_string().contains("'..'"));
    }

    #[test]
    fn dir_helpers() {
        assert_eq!(parent_dir("a/b/c.go"), "a/b");
        assert_eq!(parent_dir("c.go"), "");
        assert_eq!(join("", "x"), "x");
        assert_eq!(join("a", ""), "a");
        assert_eq!(join("a", "x"), "a/x");
        assert_eq!(strip_dir("a/b/c", "a"), Some("b/c"));
        assert_eq!(strip_dir("a/b", "a/b"), Some(""));
        assert_eq!(strip_dir("ab/c", "a"), None);
        assert_eq!(strip_dir("x", ""), Some("x"));
    }
}
