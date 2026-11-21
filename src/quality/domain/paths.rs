//! Tool output names files in several ways (relative, absolute, `./`-prefixed); the gate needs one form.

use std::path::Path;

/// `path` relative to `root` with `/` separators and no `./` prefix; unchanged shape when it is already relative
/// or outside `root`.
pub fn relative_to(path: &str, root: &Path) -> String {
    let stripped = Path::new(path).strip_prefix(root).map_or(path, |rest| rest.to_str().unwrap_or(path));
    let normalised = stripped.replace('\\', "/");
    normalised.trim_start_matches("./").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_root_and_dot_slash() {
        let root = Path::new("/snap/abc");
        assert_eq!(relative_to("/snap/abc/internal/a.go", root), "internal/a.go");
        assert_eq!(relative_to("./internal/a.go", root), "internal/a.go");
        assert_eq!(relative_to("internal/a.go", root), "internal/a.go");
        assert_eq!(relative_to("/elsewhere/a.go", root), "/elsewhere/a.go");
    }
}
