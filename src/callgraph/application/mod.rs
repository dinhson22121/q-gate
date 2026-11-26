use std::path::Path;
use std::sync::Arc;

pub use crate::callgraph::domain::{CallSite, CallerLimits, CallerReport, ChangedFile, Definition, Reference};
use crate::callgraph::domain::{call_sites, changed_symbols};

pub trait SourceIndexer: Send + Sync {
    fn definitions(&self, root: &Path, files: &[String]) -> Vec<Definition>;

    fn references(&self, root: &Path, names: &[String]) -> Vec<Reference>;
}

pub struct FindCallers {
    indexer: Arc<dyn SourceIndexer>,
    limits: CallerLimits,
}

impl FindCallers {
    pub fn new(indexer: Arc<dyn SourceIndexer>) -> Self {
        Self { indexer, limits: CallerLimits::default() }
    }

    pub fn execute(&self, root: &Path, changes: &[ChangedFile]) -> CallerReport {
        let files: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
        let definitions = self.indexer.definitions(root, &files);
        let symbols = changed_symbols(&definitions, changes, &self.limits);
        if symbols.is_empty() {
            return CallerReport::default();
        }
        let names: Vec<String> = symbols.iter().take(self.limits.max_symbols).map(|s| s.name.clone()).collect();
        let references = self.indexer.references(root, &names);
        call_sites(&symbols, &references, &self.limits)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeIndexer {
        asked: Mutex<Vec<Vec<String>>>,
    }

    impl SourceIndexer for FakeIndexer {
        fn definitions(&self, _root: &Path, files: &[String]) -> Vec<Definition> {
            assert_eq!(files, ["cart.go"]);
            vec![Definition { file: "cart.go".into(), name: "Total".into(), start_line: 3, end_line: 9 }]
        }

        fn references(&self, _root: &Path, names: &[String]) -> Vec<Reference> {
            self.asked.lock().unwrap().push(names.to_vec());
            vec![Reference { file: "api.go".into(), line: 7, name: "Total".into(), text: "t := cart.Total()".into() }]
        }
    }

    #[test]
    fn callers_of_changed_functions_are_found_by_name() {
        let indexer = Arc::new(FakeIndexer::default());
        let find = FindCallers::new(indexer.clone());
        let report = find.execute(Path::new("/r"), &[ChangedFile { path: "cart.go".into(), changed_lines: vec![5] }]);
        assert_eq!(report.sites.len(), 1);
        assert_eq!((report.sites[0].file.as_str(), report.sites[0].line), ("api.go", 7));
        assert_eq!(indexer.asked.lock().unwrap().as_slice(), [vec!["Total".to_string()]]);
    }

    #[test]
    fn no_changed_function_means_no_reference_scan() {
        let indexer = Arc::new(FakeIndexer::default());
        let report = FindCallers::new(indexer.clone())
            .execute(Path::new("/r"), &[ChangedFile { path: "cart.go".into(), changed_lines: vec![20] }]);
        assert_eq!(report, CallerReport::default());
        assert!(indexer.asked.lock().unwrap().is_empty());
    }
}
