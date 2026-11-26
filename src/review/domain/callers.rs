#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerSite {
    pub symbol: String,
    pub defined_in: String,
    pub file: String,
    pub line: u32,
    pub snippet: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallerContext {
    pub sites: Vec<CallerSite>,
    pub notes: Vec<String>,
}

impl CallerContext {
    pub fn for_files<'a>(&self, paths: impl IntoIterator<Item = &'a str>) -> Vec<CallerSite> {
        let paths: Vec<&str> = paths.into_iter().collect();
        self.sites.iter().filter(|s| paths.contains(&s.defined_in.as_str())).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(defined_in: &str) -> CallerSite {
        CallerSite {
            symbol: "f".into(),
            defined_in: defined_in.into(),
            file: "x.go".into(),
            line: 1,
            snippet: "f()".into(),
        }
    }

    #[test]
    fn sites_are_filtered_by_the_file_that_defines_the_symbol() {
        let context = CallerContext { sites: vec![site("a.go"), site("b.go")], notes: vec![] };
        assert_eq!(context.for_files(["b.go"]), vec![site("b.go")]);
        assert!(context.for_files(["c.go"]).is_empty());
    }
}
