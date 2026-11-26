use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    pub file: String,
    pub name: String,
    pub start_line: u32,
    pub end_line: u32,
}

impl Definition {
    fn contains(&self, file: &str, line: u32) -> bool {
        self.file == file && (self.start_line..=self.end_line).contains(&line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub file: String,
    pub line: u32,
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub changed_lines: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    pub symbol: String,
    pub defined_in: String,
    pub file: String,
    pub line: u32,
    pub snippet: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallerReport {
    pub sites: Vec<CallSite>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallerLimits {
    pub max_symbols: usize,
    pub max_sites_per_symbol: usize,
    pub max_sites: usize,
    pub too_common: usize,
    pub min_name_chars: usize,
}

impl Default for CallerLimits {
    fn default() -> Self {
        Self { max_symbols: 15, max_sites_per_symbol: 6, max_sites: 40, too_common: 60, min_name_chars: 3 }
    }
}

pub fn changed_symbols(definitions: &[Definition], changes: &[ChangedFile], limits: &CallerLimits) -> Vec<Definition> {
    let mut symbols: Vec<Definition> = definitions
        .iter()
        .filter(|d| d.name.chars().count() >= limits.min_name_chars)
        .filter(|d| changes.iter().any(|c| c.changed_lines.iter().any(|&line| d.contains(&c.path, line))))
        .cloned()
        .collect();
    symbols.sort_by(|a, b| a.file.cmp(&b.file).then(a.start_line.cmp(&b.start_line)));
    let mut seen = BTreeSet::new();
    symbols.retain(|s| seen.insert(s.name.clone()));
    symbols
}

pub fn call_sites(symbols: &[Definition], references: &[Reference], limits: &CallerLimits) -> CallerReport {
    let mut report = CallerReport::default();
    if symbols.len() > limits.max_symbols {
        report.notes.push(format!(
            "callers traced for the first {} of {} changed functions",
            limits.max_symbols,
            symbols.len()
        ));
    }
    let mut by_name: BTreeMap<&str, Vec<&Reference>> = BTreeMap::new();
    for reference in references {
        by_name.entry(reference.name.as_str()).or_default().push(reference);
    }
    for symbol in symbols.iter().take(limits.max_symbols) {
        let callers: Vec<&Reference> = by_name
            .get(symbol.name.as_str())
            .map(|refs| refs.iter().copied().filter(|r| !symbols.iter().any(|s| s.contains(&r.file, r.line))).collect())
            .unwrap_or_default();
        if callers.len() > limits.too_common {
            report.notes.push(format!(
                "`{}` has {} call sites by name, too common to trace without type information",
                symbol.name,
                callers.len()
            ));
            continue;
        }
        let room = limits.max_sites.saturating_sub(report.sites.len());
        let shown = callers.len().min(limits.max_sites_per_symbol).min(room);
        if shown < callers.len() {
            report.notes.push(format!("`{}`: showing {shown} of {} call sites", symbol.name, callers.len()));
        }
        report.sites.extend(callers.into_iter().take(shown).map(|r| CallSite {
            symbol: symbol.name.clone(),
            defined_in: symbol.file.clone(),
            file: r.file.clone(),
            line: r.line,
            snippet: r.text.clone(),
        }));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(file: &str, name: &str, start: u32, end: u32) -> Definition {
        Definition { file: file.into(), name: name.into(), start_line: start, end_line: end }
    }

    fn reference(file: &str, line: u32, name: &str) -> Reference {
        Reference { file: file.into(), line, name: name.into(), text: format!("{name}(x)") }
    }

    fn changed(path: &str, lines: &[u32]) -> ChangedFile {
        ChangedFile { path: path.into(), changed_lines: lines.to_vec() }
    }

    #[test]
    fn symbols_whose_body_overlaps_a_changed_line_are_selected_once() {
        let defs = vec![
            def("cart.go", "Total", 10, 20),
            def("cart.go", "Tax", 22, 30),
            def("cart.go", "id", 32, 33),
            def("api.go", "Total", 1, 5),
            def("cart.go", "Total", 40, 45),
        ];
        let symbols = changed_symbols(
            &defs,
            &[changed("cart.go", &[12, 33, 41]), changed("api.go", &[3])],
            &CallerLimits::default(),
        );
        assert_eq!(symbols, vec![def("api.go", "Total", 1, 5)], "short names and repeated names are skipped");
    }

    #[test]
    fn callers_exclude_the_changed_bodies_and_respect_limits() {
        let symbols = vec![def("cart.go", "Total", 10, 20)];
        let refs = vec![
            reference("cart.go", 15, "Total"),
            reference("api.go", 4, "Total"),
            reference("api.go", 9, "Total"),
            reference("job.go", 2, "Other"),
        ];
        let limits = CallerLimits { max_sites_per_symbol: 1, ..CallerLimits::default() };
        let report = call_sites(&symbols, &refs, &limits);
        assert_eq!(report.sites.len(), 1);
        assert_eq!(
            report.sites[0],
            CallSite {
                symbol: "Total".into(),
                defined_in: "cart.go".into(),
                file: "api.go".into(),
                line: 4,
                snippet: "Total(x)".into()
            }
        );
        assert_eq!(report.notes, vec!["`Total`: showing 1 of 2 call sites"]);
    }

    #[test]
    fn too_common_names_and_too_many_symbols_are_noted() {
        let symbols = vec![def("a.go", "Save", 1, 2), def("b.go", "Load", 1, 2)];
        let refs: Vec<Reference> = (0..5).map(|i| reference("x.go", i + 10, "Save")).collect();
        let limits = CallerLimits { max_symbols: 1, too_common: 3, ..CallerLimits::default() };
        let report = call_sites(&symbols, &refs, &limits);
        assert!(report.sites.is_empty());
        assert!(report.notes[0].contains("first 1 of 2"));
        assert!(report.notes[1].contains("too common"));
    }

    #[test]
    fn total_cap_applies_across_symbols() {
        let symbols = vec![def("a.go", "Alpha", 1, 2), def("a.go", "Bravo", 4, 5)];
        let refs = vec![reference("x.go", 1, "Alpha"), reference("x.go", 2, "Alpha"), reference("y.go", 1, "Bravo")];
        let limits = CallerLimits { max_sites: 2, ..CallerLimits::default() };
        let report = call_sites(&symbols, &refs, &limits);
        assert_eq!(report.sites.len(), 2);
        assert!(report.notes.iter().any(|n| n.contains("`Bravo`: showing 0 of 1")));
    }
}
