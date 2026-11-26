use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;

use crate::callgraph::application::{CallerReport, ChangedFile, FindCallers};
use crate::callgraph::infrastructure::TreeSitterIndexer;
use crate::review::application::ports::CallerFinder;
use crate::review::domain::callers::{CallerContext, CallerSite};
use crate::review::domain::change_set::{ChangeStatus, FileChange};

pub struct CallgraphCallers {
    root: PathBuf,
    find: Arc<FindCallers>,
}

impl CallgraphCallers {
    pub fn new(root: PathBuf) -> Self {
        Self { root, find: Arc::new(FindCallers::new(Arc::new(TreeSitterIndexer::new()))) }
    }

    #[cfg(test)]
    fn with_finder(root: PathBuf, find: FindCallers) -> Self {
        Self { root, find: Arc::new(find) }
    }
}

impl CallerFinder for CallgraphCallers {
    fn find<'a>(&'a self, files: &'a [FileChange]) -> BoxFuture<'a, CallerContext> {
        let changes: Vec<ChangedFile> = files
            .iter()
            .filter(|f| f.status != ChangeStatus::Deleted && !f.is_binary)
            .map(|f| ChangedFile { path: f.path.clone(), changed_lines: f.changed_positions() })
            .collect();
        let (root, find) = (self.root.clone(), self.find.clone());
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || find.execute(&root, &changes)).await {
                Ok(report) => to_context(report),
                Err(e) => CallerContext { sites: Vec::new(), notes: vec![format!("caller search failed: {e}")] },
            }
        })
    }
}

fn to_context(report: CallerReport) -> CallerContext {
    let sites = report
        .sites
        .into_iter()
        .map(|s| CallerSite {
            symbol: s.symbol,
            defined_in: s.defined_in,
            file: s.file,
            line: s.line,
            snippet: s.snippet,
        })
        .collect();
    let notes = report.notes.into_iter().map(|n| format!("callers: {n}")).collect();
    CallerContext { sites, notes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::domain::change_set::fixtures::{added, context, file};

    #[tokio::test]
    async fn finds_callers_of_a_changed_go_function_in_the_snapshot() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("cart.go"), "package m\n\nfunc Total(a int) int {\n\treturn a * 2\n}\n")
            .unwrap();
        std::fs::write(root.path().join("api.go"), "package m\n\nfunc h() {\n\t_ = Total(3)\n}\n").unwrap();
        let changed = file(
            "cart.go",
            3,
            vec![context(3, "func Total(a int) int {"), added(4, "\treturn a * 2")],
            Some("package m\n\nfunc Total(a int) int {\n\treturn a * 2\n}\n"),
        );
        let context = CallgraphCallers::new(root.path().to_path_buf()).find(&[changed]).await;
        assert_eq!(
            context.sites,
            vec![CallerSite {
                symbol: "Total".into(),
                defined_in: "cart.go".into(),
                file: "api.go".into(),
                line: 4,
                snippet: "_ = Total(3)".into()
            }]
        );
    }

    #[tokio::test]
    async fn deleted_files_are_ignored_and_notes_are_prefixed() {
        let mut gone = file("old.go", 1, vec![added(1, "x")], None);
        gone.status = ChangeStatus::Deleted;
        let finder = CallgraphCallers::with_finder(
            PathBuf::from("/nowhere"),
            FindCallers::new(Arc::new(TreeSitterIndexer::new())),
        );
        assert_eq!(finder.find(&[gone]).await, CallerContext::default());
        let context = to_context(CallerReport { sites: vec![], notes: vec!["too common".into()] });
        assert_eq!(context.notes, vec!["callers: too common"]);
    }
}
