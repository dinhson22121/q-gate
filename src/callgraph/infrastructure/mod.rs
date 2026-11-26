use std::path::{Path, PathBuf};

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor};

use crate::callgraph::application::{Definition, Reference, SourceIndexer};

const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_WALK_ENTRIES: usize = 50_000;
const MAX_WALK_DEPTH: usize = 16;
const MAX_SNIPPET_CHARS: usize = 200;
const SKIPPED_DIRS: &[&str] =
    &[".git", "node_modules", "target", "build", "dist", "vendor", ".venv", ".qgate-venv", ".gradle", ".qgate-results"];

const GO_DEFINITIONS: &str =
    "(function_declaration name: (identifier) @name) @def (method_declaration name: (field_identifier) @name) @def";
const GO_CALLS: &str = "(call_expression function: (identifier) @call) \
     (call_expression function: (selector_expression field: (field_identifier) @call))";
const JAVA_DEFINITIONS: &str =
    "(method_declaration name: (identifier) @name) @def (constructor_declaration name: (identifier) @name) @def";
const JAVA_CALLS: &str =
    "(method_invocation name: (identifier) @call) (object_creation_expression type: (type_identifier) @call)";
const KOTLIN_DEFINITIONS: &str = "(function_declaration name: (identifier) @name) @def";
const KOTLIN_CALLS: &str =
    "(call_expression . (identifier) @call) (call_expression . (navigation_expression (identifier) @call .))";
const PYTHON_DEFINITIONS: &str = "(function_definition name: (identifier) @name) @def";
const PYTHON_CALLS: &str =
    "(call function: (identifier) @call) (call function: (attribute attribute: (identifier) @call))";
const JS_DEFINITIONS: &str = "(function_declaration name: (identifier) @name) @def \
     (method_definition name: (property_identifier) @name) @def \
     (variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @def";
const JS_CALLS: &str = "(call_expression function: (identifier) @call) \
     (call_expression function: (member_expression property: (property_identifier) @call)) \
     (new_expression constructor: (identifier) @call)";
const RUST_DEFINITIONS: &str = "(function_item name: (identifier) @name) @def";
const RUST_CALLS: &str = "(call_expression function: (identifier) @call) \
     (call_expression function: (field_expression field: (field_identifier) @call)) \
     (call_expression function: (scoped_identifier name: (identifier) @call))";

struct Grammar {
    extensions: &'static [&'static str],
    language: Language,
    definitions: Query,
    calls: Query,
}

impl Grammar {
    fn new(extensions: &'static [&'static str], language: Language, definitions: &str, calls: &str) -> Self {
        let definitions = Query::new(&language, definitions).expect("definition query is valid for its grammar");
        let calls = Query::new(&language, calls).expect("call query is valid for its grammar");
        Self { extensions, language, definitions, calls }
    }
}

pub struct TreeSitterIndexer {
    grammars: Vec<Grammar>,
}

impl Default for TreeSitterIndexer {
    fn default() -> Self {
        Self::new()
    }
}

impl TreeSitterIndexer {
    pub fn new() -> Self {
        let grammars = vec![
            Grammar::new(&["go"], tree_sitter_go::LANGUAGE.into(), GO_DEFINITIONS, GO_CALLS),
            Grammar::new(&["java"], tree_sitter_java::LANGUAGE.into(), JAVA_DEFINITIONS, JAVA_CALLS),
            Grammar::new(&["kt", "kts"], tree_sitter_kotlin_ng::LANGUAGE.into(), KOTLIN_DEFINITIONS, KOTLIN_CALLS),
            Grammar::new(&["py"], tree_sitter_python::LANGUAGE.into(), PYTHON_DEFINITIONS, PYTHON_CALLS),
            Grammar::new(
                &["js", "jsx", "mjs", "cjs"],
                tree_sitter_javascript::LANGUAGE.into(),
                JS_DEFINITIONS,
                JS_CALLS,
            ),
            Grammar::new(
                &["ts", "mts", "cts"],
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                JS_DEFINITIONS,
                JS_CALLS,
            ),
            Grammar::new(&["tsx"], tree_sitter_typescript::LANGUAGE_TSX.into(), JS_DEFINITIONS, JS_CALLS),
            Grammar::new(&["rs"], tree_sitter_rust::LANGUAGE.into(), RUST_DEFINITIONS, RUST_CALLS),
        ];
        Self { grammars }
    }

    fn grammar_for(&self, path: &Path) -> Option<&Grammar> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        self.grammars.iter().find(|g| g.extensions.contains(&extension.as_str()))
    }
}

impl SourceIndexer for TreeSitterIndexer {
    fn definitions(&self, root: &Path, files: &[String]) -> Vec<Definition> {
        let mut found = Vec::new();
        for file in files {
            let path = root.join(file);
            let (Some(grammar), Some(source)) = (self.grammar_for(&path), read_source(&path)) else { continue };
            for (name, start, end) in captures(grammar, &grammar.definitions, &source, "name", "def") {
                found.push(Definition { file: file.clone(), name, start_line: start, end_line: end });
            }
        }
        found
    }

    fn references(&self, root: &Path, names: &[String]) -> Vec<Reference> {
        let mut found = Vec::new();
        for path in source_files(root, |p| self.grammar_for(p).is_some()) {
            let (Some(grammar), Some(source)) = (self.grammar_for(&path), read_source(&path)) else { continue };
            if !names.iter().any(|name| source.contains(name.as_str())) {
                continue;
            }
            let lines: Vec<&str> = source.lines().collect();
            let file = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            for (name, line, _) in captures(grammar, &grammar.calls, &source, "call", "call") {
                if !names.contains(&name) {
                    continue;
                }
                let text = lines.get(line as usize - 1).map(|l| l.trim().chars().take(MAX_SNIPPET_CHARS).collect());
                found.push(Reference { file: file.clone(), line, name, text: text.unwrap_or_default() });
            }
        }
        found
    }
}

fn captures(
    grammar: &Grammar,
    query: &Query,
    source: &str,
    name_capture: &str,
    span_capture: &str,
) -> Vec<(String, u32, u32)> {
    let mut parser = Parser::new();
    if parser.set_language(&grammar.language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else { return Vec::new() };
    let (Some(name_index), Some(span_index)) =
        (query.capture_index_for_name(name_capture), query.capture_index_for_name(span_capture))
    else {
        return Vec::new();
    };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut out = Vec::new();
    while let Some(found) = matches.next() {
        let name = found.nodes_for_capture_index(name_index).next();
        let span = found.nodes_for_capture_index(span_index).next();
        let (Some(name), Some(span)) = (name, span) else { continue };
        let Ok(text) = name.utf8_text(source.as_bytes()) else { continue };
        out.push((text.to_string(), row(span.start_position().row), row(span.end_position().row)));
    }
    out
}

fn row(zero_based: usize) -> u32 {
    u32::try_from(zero_based + 1).unwrap_or(u32::MAX)
}

fn read_source(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn source_files(root: &Path, keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_WALK_ENTRIES {
                return found;
            }
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            let skipped = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| SKIPPED_DIRS.contains(&n));
            if kind.is_dir() && depth < MAX_WALK_DEPTH && !skipped {
                stack.push((path, depth + 1));
            } else if kind.is_file() && keep(&path) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn names(defs: &[Definition]) -> Vec<(String, u32, u32)> {
        defs.iter().map(|d| (d.name.clone(), d.start_line, d.end_line)).collect()
    }

    fn calls(root: &Path, wanted: &[&str]) -> Vec<(String, u32, String)> {
        let wanted: Vec<String> = wanted.iter().map(|s| (*s).to_string()).collect();
        TreeSitterIndexer::new().references(root, &wanted).into_iter().map(|r| (r.file, r.line, r.name)).collect()
    }

    const CASES: &[(&str, &str, &str)] = &[
        (
            "a.go",
            "package m\n\nfunc Total(a int) int {\n\treturn a\n}\n\nfunc (s *S) Apply() {}\n",
            "package m\n\nfunc use() {\n\tx := Total(1)\n\ts.Apply()\n}\n",
        ),
        (
            "A.java",
            "class A {\n  int total(int a) {\n    return a;\n  }\n  A() {}\n}\n",
            "class B {\n  void go() {\n    o.total(1);\n    new A();\n  }\n}\n",
        ),
        (
            "a.kt",
            "class A {\n  fun total(a: Int): Int {\n    return a\n  }\n}\n",
            "fun go() {\n  val x = total(1)\n  A().total(2)\n}\n",
        ),
        ("a.py", "class A:\n    def total(self, a):\n        return a\n", "def go():\n    total(1)\n    o.total(2)\n"),
        (
            "a.ts",
            "export function total(a: number) {\n  return a;\n}\nconst apply = (x: number) => x;\n",
            "import { total } from './a';\ntotal(1);\no.apply(2);\n",
        ),
        (
            "a.rs",
            "pub fn total(a: i32) -> i32 {\n    a\n}\n",
            "fn go() {\n    total(1);\n    m::total(2);\n    o.total(3);\n}\n",
        ),
    ];

    #[test]
    fn every_grammar_finds_definitions_with_their_spans() {
        let root = tempfile::tempdir().unwrap();
        let indexer = TreeSitterIndexer::new();
        let mut found = Vec::new();
        for (file, definitions, _) in CASES {
            write(root.path(), file, definitions);
            found.push((*file, names(&indexer.definitions(root.path(), &[(*file).to_string()]))));
        }
        assert_eq!(found[0].1, vec![("Total".to_string(), 3, 5), ("Apply".to_string(), 7, 7)]);
        assert_eq!(found[1].1, vec![("total".to_string(), 2, 4), ("A".to_string(), 5, 5)]);
        assert_eq!(found[2].1, vec![("total".to_string(), 2, 4)]);
        assert_eq!(found[3].1, vec![("total".to_string(), 2, 3)]);
        assert_eq!(found[4].1, vec![("total".to_string(), 1, 3), ("apply".to_string(), 4, 4)]);
        assert_eq!(found[5].1, vec![("total".to_string(), 1, 3)]);
    }

    #[test]
    fn every_grammar_finds_call_sites_by_name() {
        let expected: [Vec<(&str, u32)>; 6] = [
            vec![("Total", 4), ("Apply", 5)],
            vec![("total", 3), ("A", 4)],
            vec![("total", 2), ("A", 3), ("total", 3)],
            vec![("total", 2), ("total", 3)],
            vec![("total", 2), ("apply", 3)],
            vec![("total", 2), ("total", 3), ("total", 4)],
        ];
        for ((file, _, callers), expected) in CASES.iter().zip(expected) {
            let root = tempfile::tempdir().unwrap();
            let caller_file = format!("caller.{}", file.rsplit('.').next().unwrap());
            write(root.path(), &caller_file, callers);
            let mut got = calls(root.path(), &["Total", "Apply", "total", "apply", "A"]);
            got.sort_by_key(|(_, line, _)| *line);
            let got: Vec<(&str, u32)> = got.iter().map(|(_, line, name)| (name.as_str(), *line)).collect();
            assert_eq!(got, expected, "{file}");
        }
    }

    #[test]
    fn references_skip_dependency_dirs_files_without_the_name_and_capture_the_line() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "src/api.go", "package m\n\nfunc h() {\n\tcart.Total()  \n}\n");
        write(root.path(), "vendor/x/y.go", "package y\n\nfunc h() { Total() }\n");
        write(root.path(), "src/other.go", "package m\n\nfunc h() { Sum() }\n");
        write(root.path(), "README.md", "Total()");
        let refs = TreeSitterIndexer::new().references(root.path(), &["Total".to_string()]);
        assert_eq!(
            refs,
            vec![Reference { file: "src/api.go".into(), line: 4, name: "Total".into(), text: "cart.Total()".into() }]
        );
    }

    #[test]
    fn unsupported_missing_or_oversized_files_are_ignored() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "big.go", &format!("package m\nfunc Total() {{}}\n{}", "// x\n".repeat(200_000)));
        let indexer = TreeSitterIndexer::new();
        let files = ["notes.txt", "missing.go", "big.go"].map(String::from);
        let definitions = indexer.definitions(root.path(), &files);
        assert!(definitions.is_empty());
    }
}
