use std::path::Path;

pub fn package_rel_dir(snapshot: &Path, package: &str) -> Option<String> {
    let go_mod = std::fs::read_to_string(snapshot.join("go.mod")).ok()?;
    let module = go_mod.lines().find_map(|line| line.trim().strip_prefix("module "))?.trim().trim_matches('"');
    let rest = package.strip_prefix(module)?;
    (rest.is_empty() || rest.starts_with('/')).then(|| rest.trim_start_matches('/').to_string())
}

pub fn locate_test(snapshot: &Path, package: &str, test: &str) -> Option<(String, u32)> {
    let name = test.split('/').next().filter(|n| !n.is_empty())?;
    let rel_dir = package_rel_dir(snapshot, package)?;
    let mut files: Vec<_> = std::fs::read_dir(snapshot.join(&rel_dir))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|file| file.ends_with("_test.go"))
        .collect();
    files.sort();
    let signature = format!("func {name}(");
    files.into_iter().find_map(|file| {
        let text = std::fs::read_to_string(snapshot.join(&rel_dir).join(&file)).ok()?;
        let line = text.lines().position(|l| l.starts_with(&signature))?;
        let path = if rel_dir.is_empty() { file } else { format!("{rel_dir}/{file}") };
        Some((path, u32::try_from(line + 1).ok()?))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/shop\n\ngo 1.22\n").unwrap();
        std::fs::create_dir_all(dir.path().join("cart")).unwrap();
        std::fs::write(
            dir.path().join("cart/cart_test.go"),
            "package cart\n\nimport \"testing\"\n\nfunc TestAdd(t *testing.T) {}\n\nfunc TestTotal(t *testing.T) {\n}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("cart/other_test.go"), "package cart\n\nfunc TestOther(t *testing.T) {}\n")
            .unwrap();
        dir
    }

    #[test]
    fn finds_the_file_and_line_of_a_top_level_test_and_of_a_subtest_parent() {
        let dir = module();
        let found = locate_test(dir.path(), "example.com/shop/cart", "TestTotal");
        assert_eq!(found, Some(("cart/cart_test.go".to_string(), 7)));
        assert_eq!(
            locate_test(dir.path(), "example.com/shop/cart", "TestOther/case_1").unwrap().0,
            "cart/other_test.go"
        );
    }

    #[test]
    fn root_package_files_have_no_directory_prefix() {
        let dir = module();
        std::fs::write(dir.path().join("root_test.go"), "package shop\nfunc TestRoot(t *testing.T) {}\n").unwrap();
        assert_eq!(locate_test(dir.path(), "example.com/shop", "TestRoot"), Some(("root_test.go".to_string(), 2)));
    }

    #[test]
    fn unknown_tests_packages_and_package_level_failures_yield_none() {
        let dir = module();
        assert_eq!(locate_test(dir.path(), "example.com/shop/cart", "TestMissing"), None);
        assert_eq!(locate_test(dir.path(), "example.com/shop/cart", ""), None);
        assert_eq!(locate_test(dir.path(), "example.com/other/cart", "TestAdd"), None);
        assert_eq!(locate_test(dir.path(), "example.com/shopx/cart", "TestAdd"), None, "module prefix must end at /");
        assert_eq!(locate_test(dir.path(), "example.com/shop/nodir", "TestAdd"), None);
        assert_eq!(package_rel_dir(dir.path(), "example.com/shop/cart"), Some("cart".into()));
        assert_eq!(package_rel_dir(tempfile::tempdir().unwrap().path(), "x"), None);
    }
}
