use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoPackage {
    pub import_path: String,
    pub rel_dir: String,
    pub imports: Vec<String>,
    pub has_tests: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImpactedSet {
    pub changed: Vec<String>,
    pub impacted: Vec<String>,
    pub selected: Vec<String>,
    pub without_tests: Vec<String>,
    pub dropped_by_cap: Vec<String>,
}

impl ImpactedSet {
    pub fn capped(&self) -> bool {
        !self.dropped_by_cap.is_empty()
    }
}

const GLOBAL_FILES: &[&str] = &["go.mod", "go.sum", "go.work", "go.work.sum"];

pub fn changed_packages(packages: &[GoPackage], changed_files: &[String]) -> Vec<String> {
    let by_dir: BTreeMap<&str, &str> = packages.iter().map(|p| (p.rel_dir.as_str(), p.import_path.as_str())).collect();
    let mut changed = BTreeSet::new();
    for file in changed_files {
        let file = file.trim_start_matches("./");
        let (dir, name) = file.rsplit_once('/').unwrap_or(("", file));
        if GLOBAL_FILES.contains(&name) || file.starts_with("vendor/") {
            return packages.iter().map(|p| p.import_path.clone()).collect();
        }
        let owner = if name.ends_with(".go") { by_dir.get(dir).copied() } else { testdata_owner(&by_dir, dir) };
        changed.extend(owner.map(str::to_string));
    }
    changed.into_iter().collect()
}

fn testdata_owner<'a>(by_dir: &BTreeMap<&str, &'a str>, dir: &str) -> Option<&'a str> {
    let segments: Vec<&str> = dir.split('/').collect();
    let at = segments.iter().position(|s| *s == "testdata")?;
    by_dir.get(segments[..at].join("/").as_str()).copied()
}

pub fn compute_impacted(packages: &[GoPackage], changed: &[String], cap: usize) -> ImpactedSet {
    let known: BTreeSet<&str> = packages.iter().map(|p| p.import_path.as_str()).collect();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for package in packages {
        for import in &package.imports {
            if known.contains(import.as_str()) {
                dependents.entry(import.as_str()).or_default().push(package.import_path.as_str());
            }
        }
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    let mut impacted = Vec::new();
    for start in changed.iter().filter_map(|c| known.get(c.as_str())) {
        if seen.insert(start) {
            queue.push_back(start);
        }
    }
    while let Some(current) = queue.pop_front() {
        impacted.push(current.to_string());
        let mut next = dependents.get(current).cloned().unwrap_or_default();
        next.sort_unstable();
        for dependent in next {
            if seen.insert(dependent) {
                queue.push_back(dependent);
            }
        }
    }
    split_by_tests(packages, changed, impacted, cap)
}

fn split_by_tests(packages: &[GoPackage], changed: &[String], impacted: Vec<String>, cap: usize) -> ImpactedSet {
    let tested: BTreeSet<&str> = packages.iter().filter(|p| p.has_tests).map(|p| p.import_path.as_str()).collect();
    let (mut with_tests, without_tests): (Vec<String>, Vec<String>) =
        impacted.iter().cloned().partition(|p| tested.contains(p.as_str()));
    let dropped_by_cap = if with_tests.len() > cap { with_tests.split_off(cap) } else { Vec::new() };
    ImpactedSet { changed: changed.to_vec(), impacted, selected: with_tests, without_tests, dropped_by_cap }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, imports: &[&str], has_tests: bool) -> GoPackage {
        let rel = name.strip_prefix("m").unwrap_or(name).trim_start_matches('/');
        GoPackage {
            import_path: name.to_string(),
            rel_dir: rel.to_string(),
            imports: imports.iter().map(|s| s.to_string()).collect(),
            has_tests,
        }
    }

    fn graph() -> Vec<GoPackage> {
        vec![
            pkg("m", &["m/api"], true),
            pkg("m/api", &["m/svc", "fmt"], true),
            pkg("m/svc", &["m/util"], false),
            pkg("m/util", &[], true),
            pkg("m/other", &[], true),
        ]
    }

    fn files(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn go_files_map_to_their_package_and_others_are_ignored() {
        let changed = changed_packages(&graph(), &files(&["util/a.go", "README.md", "docs/x.go", "./other/b_test.go"]));
        assert_eq!(changed, vec!["m/other", "m/util"]);
    }

    #[test]
    fn root_go_file_maps_to_root_package() {
        assert_eq!(changed_packages(&graph(), &files(&["main.go"])), vec!["m"]);
    }

    #[test]
    fn testdata_belongs_to_the_nearest_package() {
        let changed =
            changed_packages(&graph(), &files(&["api/testdata/golden/x.json", "testdata/y.txt", "api/z.json"]));
        assert_eq!(changed, vec!["m", "m/api"]);
    }

    #[test]
    fn module_files_and_vendor_change_everything() {
        assert_eq!(changed_packages(&graph(), &files(&["go.sum"])).len(), 5);
        assert_eq!(changed_packages(&graph(), &files(&["x/vendor/y.go", "vendor/modules.txt"])).len(), 5);
    }

    #[test]
    fn dependents_are_found_transitively_nearest_first() {
        let set = compute_impacted(&graph(), &files(&["m/util"]), 50);
        assert_eq!(set.impacted, vec!["m/util", "m/svc", "m/api", "m"]);
        assert_eq!(set.selected, vec!["m/util", "m/api", "m"]);
        assert_eq!(set.without_tests, vec!["m/svc"]);
        assert!(!set.capped());
    }

    #[test]
    fn unrelated_and_unknown_packages_are_not_impacted() {
        let set = compute_impacted(&graph(), &files(&["m/other", "m/ghost"]), 50);
        assert_eq!(set.impacted, vec!["m/other"]);
    }

    #[test]
    fn cap_keeps_the_closest_packages_and_reports_the_rest() {
        let set = compute_impacted(&graph(), &files(&["m/util"]), 2);
        assert_eq!(set.selected, vec!["m/util", "m/api"]);
        assert_eq!(set.dropped_by_cap, vec!["m"]);
        assert!(set.capped());
    }

    #[test]
    fn import_cycles_in_the_graph_terminate() {
        let cyclic = vec![pkg("m/a", &["m/b"], true), pkg("m/b", &["m/a"], true)];
        let set = compute_impacted(&cyclic, &files(&["m/a"]), 10);
        assert_eq!(set.impacted, vec!["m/a", "m/b"]);
    }
}
