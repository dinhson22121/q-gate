pub mod application;
pub mod domain;
pub mod infrastructure;

use std::path::Path;
use std::sync::Arc;

use crate::sandbox::application::ExecBackend;
use application::{BaseRunStore, DetectRegressions, DetectSuiteRegressions};
use domain::Regression;
use infrastructure::{GoCli, all_suites, locate_test, package_rel_dir};

pub fn build(backend: Arc<dyn ExecBackend>, base_store: Option<Arc<dyn BaseRunStore>>) -> DetectRegressions {
    let detect = DetectRegressions::new(Arc::new(GoCli::new(backend)));
    match base_store {
        Some(store) => detect.with_base_store(store),
        None => detect,
    }
}

pub fn build_suites(
    backend: Arc<dyn ExecBackend>,
    base_store: Option<Arc<dyn BaseRunStore>>,
) -> DetectSuiteRegressions {
    let detect = DetectSuiteRegressions::new(all_suites(&backend));
    match base_store {
        Some(store) => detect.with_base_store(store),
        None => detect,
    }
}

pub fn locate_failure(snapshot: &Path, regression: &Regression) -> Option<(String, u32)> {
    if regression.location.is_some() {
        return regression.location.clone();
    }
    locate_test(snapshot, &regression.package, &regression.test).or_else(|| {
        let dir = package_rel_dir(snapshot, &regression.package)?;
        Some((if dir.is_empty() { ".".to_string() } else { dir }, 1))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_location_prefers_the_test_file_then_the_package_directory() {
        let head = tempfile::tempdir().unwrap();
        std::fs::write(head.path().join("go.mod"), "module example.com/shop\n").unwrap();
        std::fs::create_dir_all(head.path().join("cart")).unwrap();
        std::fs::write(head.path().join("cart/cart_test.go"), "package cart\n\nfunc TestTotal(t *testing.T) {}\n")
            .unwrap();
        let locate =
            |package: &str, test: &str| locate_failure(head.path(), &Regression::new(package, test, String::new()));
        assert_eq!(locate("example.com/shop/cart", "TestTotal/sub"), Some(("cart/cart_test.go".into(), 3)));
        assert_eq!(locate("example.com/shop/cart", ""), Some(("cart".into(), 1)));
        assert_eq!(locate("example.com/shop", ""), Some((".".into(), 1)));
        assert_eq!(locate("other/pkg", "TestX"), None);
    }
}
