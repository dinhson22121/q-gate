//! Parses the compact `go list -f` listing into [`GoPackage`]s.

use crate::impact::domain::GoPackage;

/// Template giving one `|`-separated line per package: import path, module path, in-package test
/// file count, external test file count, then comma-joined imports, test imports, external test imports.
pub const LIST_FORMAT: &str = "{{.ImportPath}}|{{if .Module}}{{.Module.Path}}{{end}}|{{len .TestGoFiles}}|{{len \
     .XTestGoFiles}}|{{join .Imports \",\"}}|{{join .TestImports \",\"}}|{{join .XTestImports \",\"}}";

const FIELDS: usize = 7;

/// Lines that are malformed, or belong to no module (standard library), are skipped.
pub fn parse_package_lines(output: &str) -> Vec<GoPackage> {
    output.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<GoPackage> {
    let fields: Vec<&str> = line.trim().split('|').collect();
    if fields.len() != FIELDS {
        return None;
    }
    let import_path = fields[0];
    let module = fields[1];
    let rel_dir = import_path.strip_prefix(module)?.trim_start_matches('/');
    if module.is_empty() {
        return None;
    }
    let test_files = fields[2].parse::<usize>().ok()? + fields[3].parse::<usize>().ok()?;
    let imports = fields[4..].iter().flat_map(|list| list.split(',')).filter(|i| !i.is_empty()).map(str::to_string);
    Some(GoPackage {
        import_path: import_path.to_string(),
        rel_dir: rel_dir.to_string(),
        imports: imports.collect(),
        has_tests: test_files > 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_go_list_lines() {
        let out = "example.com/m|example.com/m|0|0|example.com/m/calc,fmt||\n\
                   example.com/m/calc|example.com/m|1|1|errors|testing|example.com/m/calc,testing\n";
        let packages = parse_package_lines(out);
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].rel_dir, "");
        assert!(!packages[0].has_tests);
        assert_eq!(packages[1].rel_dir, "calc");
        assert!(packages[1].has_tests);
        assert_eq!(packages[1].imports, vec!["errors", "testing", "example.com/m/calc", "testing"]);
    }

    #[test]
    fn skips_malformed_stdlib_and_foreign_module_lines() {
        let out = "garbage\nfmt||0|0|||\nx/y|other|abc|0|||\nexample.com/m/a|example.com/m|0|0|||\n\n";
        assert_eq!(parse_package_lines(out).len(), 1);
        assert!(parse_line("a|b|0|0|||").is_none(), "import path outside its module");
    }
}
